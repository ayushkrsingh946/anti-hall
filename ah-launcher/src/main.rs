//! ah-hook: the thin hook launcher in front of `ah-engine hook --event <Event>` (prototype, task #47).
//!
//! It keeps the outcome table of `hooks/ah-hook.sh` (see that file's header) without the shell's ~38 process spawns:
//! - engine exit 0 or 2: pass stdout/stderr through, same exit;
//! - engine exit `dispatch.defer_exit`: run this event's fallback rows (delegated to `ah-hook.sh --fallback-only`, the rare
//!   path that goes away with the Node removal);
//! - any other exit after the engine marked the dispatch done (`env.done_file`): a hook's own exit code, passed through;
//! - timeout, signal death, an unmarked exit, a group left alive, a missing or unspawnable engine: fail open, silently
//!   (and count the failure for the integrity script);
//! - a guard event against a cold daemon: start it with the kick request and wait for its socket up to
//!   `client.cold_guard_wait_ms`, then ask; not up by then: exit 0 with one notice per session.
//!
//! Every number, name and text is read from the plugin's `engine/defaults/*.toml` (no hardcoding). The only fixed strings are
//! the paths of those files inside the plugin and the test-knob names, which are the wrapper's interface.
//! The launcher itself never blocks a session: a panic exits 0, and the host treats a missing binary or a crash as a
//! non-blocking hook error.

use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TEST_KNOBS: [&str; 7] = [
    "AH_ENGINE_BIN",
    "AH_FALLBACK_LIST",
    "AH_FALLBACK_MAP",
    "AH_HOOK_TIMEOUT_S",
    "AH_HOOK_TIMEOUT_MS",
    "AH_KILL_GRACE_S",
    "AH_HOOK_SWEEP_AGE_S",
];

// ---------------------------------------------------------------------------------------------------------------------------
// Config: the plugin's TOML defaults, parsed by the `toml` crate. A/B (owner rule: an established crate when it is at least as
// good): the crate parsed engine.toml + dispatch.toml in 0.55-0.69 ms against 1.27-1.54 ms for a hand-written line reader
// that covered the same TOML subset, and end to end the two launchers were within noise (THIN-LAUNCHER.md).
// ---------------------------------------------------------------------------------------------------------------------------

#[derive(Default)]
struct Cfg {
    doc: toml::Table,
}

impl Cfg {
    fn parse(text: &str) -> Cfg {
        Cfg {
            doc: text.parse().unwrap_or_default(),
        }
    }

    /// `[a.b]` field `f`: walks the dotted key (the launcher reads no quoted, dotted segment).
    fn raw(&self, key: &str, field: &str) -> Option<&toml::Value> {
        let mut t = &self.doc;
        for seg in key.split('.') {
            t = t.get(seg)?.as_table()?;
        }
        t.get(field)
    }

    fn num_field(&self, key: &str, field: &str) -> Option<u64> {
        self.raw(key, field)?
            .as_integer()
            .and_then(|i| u64::try_from(i).ok())
    }

    fn text(&self, key: &str) -> Option<String> {
        self.raw(key, "value")?
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    fn env_name(&self, key: &str) -> Option<String> {
        self.raw(key, "env")?
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    fn list(&self, key: &str) -> Vec<String> {
        let Some(a) = self.raw(key, "value").and_then(toml::Value::as_array) else {
            return Vec::new();
        };
        a.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    }
}

impl Cfg {
    fn load(path: &Path) -> Cfg {
        std::fs::read_to_string(path)
            .map(|t| Cfg::parse(&t))
            .unwrap_or_default()
    }

    fn num(&self, key: &str) -> Option<u64> {
        self.num_field(key, "value")
    }

    /// The value with its env override (taken only when it is all digits, as the engine's `defaults::num`), clamped to min/max.
    fn effective_num(&self, key: &str) -> Option<u64> {
        let from_env = self
            .env_name(key)
            .and_then(|n| std::env::var(n).ok())
            .and_then(|v| {
                let v: String = v.chars().filter(|c| !c.is_whitespace()).collect();
                (!v.is_empty() && v.chars().all(|c| c.is_ascii_digit()))
                    .then(|| v.parse().ok())
                    .flatten()
            });
        let mut v = from_env.or_else(|| self.num(key))?;
        if let Some(min) = self.num_field(key, "min") {
            v = v.max(min);
        }
        if let Some(max) = self.num_field(key, "max") {
            v = v.min(max);
        }
        Some(v)
    }
}

// ---------------------------------------------------------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------------------------------------------------------

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn is_exec(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.mode() & 0o111 != 0)
}

fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

fn positive(v: &str) -> Option<u64> {
    (!v.is_empty() && v.chars().all(|c| c.is_ascii_digit()))
        .then(|| v.parse().ok())
        .flatten()
        .filter(|n| *n > 0)
}

/// A top-level string field of a JSON object, found by a cheap scan (session_id for the once-per-session notice).
fn json_str_field(payload: &[u8], field: &str) -> Option<String> {
    let text = std::str::from_utf8(payload).ok()?;
    let pat = format!("\"{field}\"");
    let at = text.find(&pat)? + pat.len();
    let rest = text[at..]
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    Some(rest.split('"').next()?.to_string())
}

fn stop_hook_active(event: &str, payload: &[u8]) -> bool {
    if event != "Stop" && event != "SubagentStop" {
        return false;
    }
    let Ok(text) = std::str::from_utf8(payload) else {
        return false;
    };
    text.find("\"stop_hook_active\"").is_some_and(|i| {
        text[i + "\"stop_hook_active\"".len()..]
            .trim_start()
            .strip_prefix(':')
            .is_some_and(|r| r.trim_start().starts_with("true"))
    })
}

// ---------------------------------------------------------------------------------------------------------------------------
// Child process with a process-group watchdog
// ---------------------------------------------------------------------------------------------------------------------------

/// The process group of the child being waited for (0 = none) and the run dir, for the signal thread.
static CHILD_PGID: AtomicI32 = AtomicI32::new(0);
static RUN_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// HUP/INT/TERM to the launcher (the host gave up on the hook): take the child's whole group down, remove the run dir, and
/// exit 128+signal as the shell wrapper's traps do. The signals are blocked in every thread and taken by one `sigwait` thread;
/// std's spawn resets the mask in the child, so the engine still gets them.
fn install_signal_thread() {
    // SAFETY: sigset manipulation on a local set; pthread_sigmask on the calling (main) thread before any other thread exists.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for sig in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM] {
            libc::sigaddset(&mut set, sig);
        }
        if libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
            return;
        }
        std::thread::spawn(move || {
            let mut sig: libc::c_int = 0;
            if libc::sigwait(&set, &mut sig) != 0 {
                return;
            }
            let pgid = CHILD_PGID.load(Ordering::SeqCst);
            if pgid > 0 {
                libc::kill(-pgid, libc::SIGTERM);
                libc::kill(-pgid, libc::SIGKILL);
            }
            if let Ok(g) = RUN_DIR.lock()
                && let Some(d) = g.as_ref()
            {
                let _ = std::fs::remove_dir_all(d);
            }
            libc::_exit(128 + sig);
        });
    }
}

enum Run {
    Exited {
        code: i32,
        out: Vec<u8>,
        err: Vec<u8>,
    },
    TimedOut,
    GroupAlive,
    SpawnFailed,
}

fn kill_group(pgid: i32, sig: i32) {
    // SAFETY: plain syscall on a process group we created (process_group(0) makes pgid == child pid).
    unsafe {
        libc::kill(-pgid, sig);
    }
}

fn group_alive(pgid: i32) -> bool {
    // SAFETY: signal 0 only probes for existence.
    unsafe { libc::kill(-pgid, 0) == 0 }
}

/// Run `argv` in its own process group with `input` on stdin. Killed (TERM, grace, KILL to the whole group) when it is still
/// running at `budget`; once, when `wait_file` exists at that point (the client is waiting on a live daemon), the deadline moves
/// to `slow_max` from the start instead.
fn run_child(
    argv: &[String],
    envs: &[(String, String)],
    input: &[u8],
    budget: Duration,
    wait_file: Option<&Path>,
    slow_max: Duration,
    grace: Duration,
) -> Run {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let start = Instant::now();
    let Ok(mut child) = cmd.spawn() else {
        return Run::SpawnFailed;
    };
    let pgid = child.id() as i32;
    CHILD_PGID.store(pgid, Ordering::SeqCst);
    let (Some(mut stdin), Some(mut so), Some(mut se)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        kill_group(pgid, libc::SIGKILL);
        let _ = child.wait();
        return Run::SpawnFailed;
    };
    let input = input.to_vec();
    std::thread::spawn(move || {
        let _ = stdin.write_all(&input); // EPIPE: the child stopped reading; its answer still decides
    });
    let (otx, orx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = so.read_to_end(&mut b);
        let _ = otx.send(b);
    });
    let (etx, erx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = se.read_to_end(&mut b);
        let _ = etx.send(b);
    });
    let (wtx, wrx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = wtx.send(child.wait());
    });
    let mut status = wrx.recv_timeout(budget).ok();
    if status.is_none() && slow_max > budget && wait_file.is_some_and(Path::exists) {
        status = wrx
            .recv_timeout(slow_max.saturating_sub(start.elapsed()))
            .ok();
    }
    let Some(status) = status else {
        kill_group(pgid, libc::SIGTERM);
        if wrx.recv_timeout(grace).is_err() || group_alive(pgid) {
            kill_group(pgid, libc::SIGKILL);
        }
        return Run::TimedOut;
    };
    if group_alive(pgid) {
        // a helper of the engine outlived it in its group: it may still hold the pipes, and its answer is not the engine's
        kill_group(pgid, libc::SIGTERM);
        std::thread::sleep(grace);
        kill_group(pgid, libc::SIGKILL);
        return Run::GroupAlive;
    }
    let code = match status {
        Ok(st) => st.code().unwrap_or_else(|| 128 + st.signal().unwrap_or(0)),
        Err(_) => return Run::SpawnFailed,
    };
    let (Ok(out), Ok(err)) = (
        orx.recv_timeout(grace.max(Duration::from_millis(1))),
        erx.recv_timeout(grace.max(Duration::from_millis(1))),
    ) else {
        return Run::GroupAlive;
    };
    Run::Exited { code, out, err }
}

// ---------------------------------------------------------------------------------------------------------------------------
// The launcher
// ---------------------------------------------------------------------------------------------------------------------------

struct L {
    event: String,
    cfg_dir: PathBuf,
    host: String,
    hooks_dir: PathBuf,
    cfg: Cfg,
    dcfg: Cfg,
    guard: bool,
    test: bool,
    state_dir: PathBuf,
    payload: Vec<u8>,
    list: PathBuf,
    map: PathBuf,
    timeout_s: u64,
    timeout_ms: u64,
    grace: Duration,
}

fn write_out(out: &[u8], err: &[u8]) {
    let _ = std::io::stdout().write_all(out);
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().write_all(err);
}

impl L {
    fn fail_closed(&self, why: &str) -> i32 {
        if stop_hook_active(&self.event, &self.payload) {
            eprintln!(
                "anti-hall: fail open for {} because stop_hook_active is true: {why}",
                self.event
            );
            return 0;
        }
        eprintln!("anti-hall: fail closed for {}: {why}", self.event);
        2
    }

    /// The event's row timeout in the fallback list (`@Event<TAB>secs`), capped by the seconds budget.
    fn event_timeout_s(&self) -> Option<u64> {
        let text = std::fs::read_to_string(&self.list).ok()?;
        let want = format!("@{}\t", self.event);
        let line = text.lines().find(|l| l.starts_with(&want))?;
        let t = positive(line[want.len()..].split('\t').next()?)?;
        Some(t.min(self.timeout_s))
    }

    fn integrity(&self, verb: &str) {
        if env_nonempty("AH_ENGINE_BIN").is_some() {
            return; // never for a test double
        }
        self.detached(&self.hooks_dir.join("ah-engine-integrity.sh"), &[verb]);
    }

    fn detached(&self, script: &Path, args: &[&str]) {
        if script.is_file() {
            let _ = Command::new("sh")
                .arg(script)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn();
        }
    }

    fn diag(&self, line: &str) {
        if !self.test {
            return;
        }
        if let Some(p) = env_nonempty("AH_ENGINE_WRAPPER_DIAG").filter(|p| p.starts_with('/'))
            && let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
        {
            let _ = writeln!(f, "{line}");
        }
    }

    fn fresh_marker(&self, files_key: &str) -> bool {
        let Some(name) = self.cfg.text(&format!("files.{files_key}")) else {
            return false;
        };
        let Ok(t) = std::fs::read_to_string(self.state_dir.join(name)) else {
            return false;
        };
        let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
        digits.parse::<u64>().is_ok_and(|until| until > now_ms())
    }

    fn halted(&self) -> bool {
        self.fresh_marker("daemon_down_marker")
            || self.fresh_marker("breaker_until")
            || self.fresh_marker("crashloop_until")
    }

    /// A private run directory for the done and wait markers: `$HOME/.anti-hall/tmp` (ours, mode 700), else TMPDIR.
    fn run_dir(&self) -> Option<PathBuf> {
        let mut bases = Vec::new();
        if let Some(h) = env_nonempty("HOME").filter(|h| h.starts_with('/')) {
            bases.push(PathBuf::from(h).join(".anti-hall").join("tmp"));
        }
        if let Some(t) = env_nonempty("TMPDIR") {
            bases.push(PathBuf::from(t).join(format!("ah-hook-{}", uid())));
        }
        for b in bases {
            let _ = std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&b);
            let Ok(m) = std::fs::symlink_metadata(&b) else {
                continue;
            };
            if !m.is_dir() || m.uid() != uid() || m.mode() & 0o777 != 0o700 {
                continue;
            }
            let d = b.join(format!("ah-launch.{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d); // a dead launcher with our pid left it behind
            if std::fs::DirBuilder::new().mode(0o700).create(&d).is_ok() {
                return Some(d);
            }
        }
        None
    }

    fn cold_notice(&self) {
        let msgs = Cfg::load(&self.cfg_dir.join("messages.toml"));
        let Some(text) = msgs.text("msg.cold_guard_notice") else {
            return;
        };
        let sid = json_str_field(&self.payload, "session_id").unwrap_or_else(|| "none".into());
        if let Some(name) = self.cfg.text("files.cold_guard_notice") {
            let p = self.state_dir.join(name);
            if std::fs::read_to_string(&p).is_ok_and(|s| s.trim_end() == sid) {
                return;
            }
            let _ = std::fs::create_dir_all(&self.state_dir);
            let _ = std::fs::write(&p, format!("{sid}\n"));
        }
        eprintln!("{}", text.replace("{event}", &self.event));
    }

    /// Cold start for a guard event: kick the daemon up and wait for its socket. `Some(code)` ends the call here.
    fn cold_guard_wait(&self, engine: &str, eng_ms: u64) -> Option<i32> {
        let c = &self.cfg;
        let cap = c.num("client.cold_guard_wait_ms").filter(|v| *v > 0)?;
        let poll = c.num("client.cold_guard_poll_ms").filter(|v| *v > 0)?;
        let sock_max = c.num("paths.socket_max_len").filter(|v| *v > 0)?;
        let margin = c.num("client.wrapper_extra_ms").filter(|v| *v > 0)?;
        let kick = c.text("client.cold_guard_kick_event")?;
        let sock_name = c.text("paths.socket_file")?;
        if c.text("env.nospawn")
            .is_some_and(|n| std::env::var_os(n).is_some())
        {
            return None;
        }
        let sock = self.state_dir.join(sock_name);
        if sock.as_os_str().len() as u64 > sock_max {
            return None;
        }
        if std::fs::symlink_metadata(&sock).is_ok_and(|m| m.file_type().is_socket()) {
            return None;
        }
        let total_ms = self.event_timeout_s().unwrap_or(self.timeout_s) * 1000;
        let room = total_ms.checked_sub(eng_ms + margin).filter(|r| *r > 0)?;
        let cap = cap.min(room);
        let start = Instant::now();
        if self.halted() {
            return None;
        }
        let grace_ms = self.grace.as_millis() as u64;
        let (kick_ms, grace) = if cap > grace_ms {
            (cap - grace_ms, self.grace)
        } else {
            (cap, Duration::ZERO)
        };
        let argv = [
            engine.to_string(),
            "hook".into(),
            "--event".into(),
            kick.clone(),
            "--host".into(),
            self.host.clone(),
        ];
        let body = format!("{{\"hook_event_name\":\"{kick}\"}}");
        match run_child(
            &argv,
            &[],
            body.as_bytes(),
            Duration::from_millis(kick_ms),
            None,
            Duration::ZERO,
            grace,
        ) {
            Run::TimedOut => {
                self.diag(&format!(
                    "cold_guard_gave_up event={} cap_ms={cap} kick_cut=1",
                    self.event
                ));
                self.cold_notice();
                return Some(0);
            }
            Run::Exited { code: 0, .. } => {}
            _ => return None, // the kick failed: no daemon is coming, the real attempt fails open the usual way
        }
        loop {
            if std::fs::symlink_metadata(&sock).is_ok_and(|m| m.file_type().is_socket())
                || self.halted()
            {
                return None;
            }
            if start.elapsed() >= Duration::from_millis(cap) {
                break;
            }
            std::thread::sleep(Duration::from_millis(poll));
        }
        self.diag(&format!(
            "cold_guard_gave_up event={} cap_ms={cap}",
            self.event
        ));
        self.cold_notice();
        Some(0)
    }

    /// dispatch.defer_exit: the fallback rows run in the shell wrapper (rare path; removed with the Node hooks).
    fn fallback(&self) -> i32 {
        let script = self.hooks_dir.join("ah-hook.sh");
        let child = Command::new("sh")
            .arg(&script)
            .arg(&self.event)
            .args(["--host", &self.host, "--fallback-only"])
            .stdin(Stdio::piped())
            .spawn();
        let Ok(mut child) = child else {
            return if self.guard {
                self.fail_closed("fallback runner unavailable")
            } else {
                0
            };
        };
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(&self.payload);
        }
        match child.wait() {
            Ok(st) => st.code().unwrap_or_else(|| {
                if self.guard {
                    self.fail_closed("fallback runner died")
                } else {
                    0
                }
            }),
            Err(_) => 0,
        }
    }

    fn run_engine(&self, engine: &str, run_dir: Option<&Path>, eng_ms: u64) -> i32 {
        let mut envs = Vec::new();
        let done = run_dir.map(|d| d.join("done"));
        let wait = run_dir.map(|d| d.join("wait"));
        if let (Some(name), Some(p)) = (self.cfg.text("env.done_file"), &done) {
            envs.push((name, p.display().to_string()));
        }
        let mut slow_max = self
            .cfg
            .effective_num("client.slow_fallback_max_ms")
            .unwrap_or(0);
        slow_max = slow_max.min(self.event_timeout_s().unwrap_or(self.timeout_s) * 1000);
        let mut wait_file = None;
        match (self.cfg.text("env.wait_file"), &wait) {
            (Some(name), Some(p)) if slow_max > eng_ms => {
                envs.push((name, p.display().to_string()));
                wait_file = Some(p.as_path());
            }
            _ => slow_max = 0,
        }
        let argv = [
            engine.to_string(),
            "hook".into(),
            "--event".into(),
            self.event.clone(),
            "--host".into(),
            self.host.clone(),
            "--fallback-map".into(),
            self.map.display().to_string(),
        ];
        let r = run_child(
            &argv,
            &envs,
            &self.payload,
            Duration::from_millis(eng_ms),
            wait_file,
            Duration::from_millis(slow_max),
            self.grace,
        );
        let done_marked = done.as_deref().is_some_and(Path::exists);
        let defer = self.dcfg.num("dispatch.defer_exit").map(|v| v as i32);
        match r {
            Run::Exited {
                code: c @ (0 | 2),
                out,
                err,
            } => {
                write_out(&out, &err);
                c
            }
            Run::Exited { code, .. } if Some(code) == defer => self.fallback(),
            Run::Exited { code, out, err } if done_marked => {
                write_out(&out, &err);
                code
            }
            Run::Exited { code, .. } => {
                self.diag(&format!(
                    "engine_silent_exit event={} timeout_ms={eng_ms} rc={code} done=0",
                    self.event
                ));
                self.integrity("fail");
                0
            }
            Run::TimedOut => {
                self.diag(&format!(
                    "engine_timed event={} timeout_ms={eng_ms} done={}",
                    self.event,
                    u8::from(done_marked)
                ));
                self.integrity("fail");
                0
            }
            Run::GroupAlive => {
                self.diag(&format!(
                    "engine_group_alive event={} timeout_ms={eng_ms}",
                    self.event
                ));
                self.integrity("fail");
                0
            }
            Run::SpawnFailed => 0,
        }
    }
}

fn find_engine() -> Option<String> {
    if let Some(b) = env_nonempty("AH_ENGINE_BIN") {
        return is_exec(Path::new(&b)).then_some(b);
    }
    let home =
        env_nonempty("HOME").map(|h| PathBuf::from(h).join(".anti-hall/ah-engine/bin/ah-engine"));
    if let Some(h) = home.filter(|p| is_exec(p)) {
        return Some(h.display().to_string());
    }
    std::env::var_os("PATH")
        .and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("ah-engine"))
                .find(|p| is_exec(p))
        })
        .map(|p| p.display().to_string())
}

/// The plugin this launcher serves (its hooks/ dir: lists, fallback runner, integrity script): `--plugin-root`, else the host's
/// plugin-root variable, else the directory above the binary.
fn plugin_root(args: &[String]) -> Option<PathBuf> {
    let flag = args
        .iter()
        .position(|a| a == "--plugin-root")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let candidates = [
        flag,
        env_nonempty("CLAUDE_PLUGIN_ROOT").map(PathBuf::from),
        env_nonempty("PLUGIN_ROOT").map(PathBuf::from),
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent()?.parent().map(Path::to_path_buf)),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|r| r.join("hooks").is_dir())
}

fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = plugin_root(&args);
    let hooks_dir = root.as_ref().map(|r| r.join("hooks")).unwrap_or_default();
    if args.first().is_some_and(|a| a == "--doctor") {
        let doctor = hooks_dir.join("ah-doctor.sh");
        if doctor.is_file() {
            let err = Command::new("sh").arg(doctor).args(&args[1..]).exec();
            eprintln!("anti-hall: cannot run ah-doctor.sh: {err}");
        } else {
            eprintln!(
                "anti-hall: ah-doctor.sh is missing next to ah-hook.sh; reinstall the plugin"
            );
        }
        return 1;
    }
    let event = args.first().cloned().unwrap_or_default();
    let test = std::env::var("AH_WRAPPER_TEST").is_ok_and(|v| v == "1");
    if !test {
        let ignored: Vec<&str> = TEST_KNOBS
            .iter()
            .copied()
            .filter(|k| env_nonempty(k).is_some())
            .collect();
        for k in TEST_KNOBS {
            // SAFETY: single-threaded here; no other thread reads the environment yet.
            unsafe { std::env::remove_var(k) };
        }
        if !ignored.is_empty() {
            eprintln!(
                "anti-hall: ignoring test-only variables (need AH_WRAPPER_TEST=1): {}",
                ignored.join(" ")
            );
        }
    }
    let mut host = "claude".to_string();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--host" {
            match args.get(i + 1).map(String::as_str) {
                Some(h @ ("claude" | "codex")) => {
                    host = h.to_string();
                    i += 1;
                }
                _ => eprintln!("anti-hall: ignoring unknown --host value"),
            }
        } else if args[i] == "--plugin-root" {
            i += 1;
        }
        i += 1;
    }
    // the engine reads its settings from THIS plugin unless an operator override (AH_ENGINE_PLUGIN_ROOT) names another one
    let cfg_root = match env_nonempty("AH_ENGINE_PLUGIN_ROOT") {
        Some(r) => Some(PathBuf::from(r)),
        None => {
            let r = root
                .clone()
                .filter(|r| r.join("engine/defaults/index.toml").is_file());
            if let Some(r) = &r {
                // SAFETY: single-threaded here.
                unsafe { std::env::set_var("AH_ENGINE_PLUGIN_ROOT", r) };
            }
            r
        }
    };
    let cfg_dir = cfg_root
        .map(|r| r.join("engine/defaults"))
        .unwrap_or_default();
    let cfg = Cfg::load(&cfg_dir.join("engine.toml"));
    let dcfg = Cfg::load(&cfg_dir.join("dispatch.toml"));
    let guard = dcfg.list("dispatch.guard_events").contains(&event);
    let mut payload = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut payload);
    let (list_default, map_default) = if host == "codex" {
        ("ah-fallback.codex.list", "ah-fallback.codex.map.json")
    } else {
        ("ah-fallback.list", "ah-fallback.map.json")
    };
    let state_dir = cfg
        .text("env.dir")
        .and_then(|n| env_nonempty(&n))
        .map(PathBuf::from)
        .or_else(|| {
            let (a, b) = (cfg.text("paths.base_dir")?, cfg.text("paths.state_dir")?);
            env_nonempty("HOME").map(|h| PathBuf::from(h).join(a).join(b))
        })
        .unwrap_or_default();
    let mut l = L {
        event,
        cfg_dir: cfg_dir.clone(),
        host,
        list: env_nonempty("AH_FALLBACK_LIST")
            .map(PathBuf::from)
            .unwrap_or_else(|| hooks_dir.join(list_default)),
        map: env_nonempty("AH_FALLBACK_MAP")
            .map(PathBuf::from)
            .unwrap_or_else(|| hooks_dir.join(map_default)),
        hooks_dir,
        cfg,
        dcfg,
        guard,
        test,
        state_dir,
        payload,
        timeout_s: 0,
        timeout_ms: 0,
        grace: Duration::ZERO,
    };
    // budgets: the test knobs, else the shipped client deadline + slack + wrapper margin
    let default_s = l.cfg.effective_num("client.wrapper_default_timeout_s");
    l.timeout_s = match env_nonempty("AH_HOOK_TIMEOUT_S") {
        Some(v) => match positive(&v) {
            Some(n) => n,
            None => {
                return if l.guard {
                    l.fail_closed("AH_HOOK_TIMEOUT_S must be a positive integer")
                } else {
                    0
                };
            }
        },
        None => match default_s {
            Some(n) => n,
            None => return 0, // no readable defaults: an incomplete install fails open
        },
    };
    l.timeout_ms = match env_nonempty("AH_HOOK_TIMEOUT_MS") {
        Some(v) => match positive(&v) {
            Some(n) => n,
            None => {
                return if l.guard {
                    l.fail_closed("AH_HOOK_TIMEOUT_MS must be a positive integer")
                } else {
                    0
                };
            }
        },
        None if env_nonempty("AH_HOOK_TIMEOUT_S").is_some() => l.timeout_s * 1000,
        None => {
            let parts = (
                l.cfg.effective_num("client.deadline_ms"),
                l.cfg.num("client.deadline_slack_ms"),
                l.cfg.num("client.wrapper_extra_ms"),
            );
            match parts {
                (Some(a), Some(b), Some(c)) => a + b + c,
                _ => l.timeout_s * 1000,
            }
        }
    };
    l.grace = match env_nonempty("AH_KILL_GRACE_S")
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v <= 5)
    {
        Some(s) => Duration::from_secs(s),
        None => Duration::from_millis(
            l.cfg
                .effective_num("client.wrapper_kill_grace_ms")
                .unwrap_or(0),
        ),
    };
    if l.event == "SessionStart" {
        l.integrity("check");
        if env_nonempty("AH_ENGINE_BIN").is_none()
            && l.hooks_dir.join("../ah-engine.lock").is_file()
        {
            l.detached(&l.hooks_dir.join("ah-engine-bootstrap.sh"), &[]);
        }
    }
    let Some(engine) = find_engine() else {
        return 0;
    };
    if std::env::var("AH_ENGINE_DISPATCH_IN_PROCESS").map_or(true, |v| v != "1")
        && l.fresh_marker("daemon_down_marker")
    {
        return 0;
    }
    let eng_ms = match l.event_timeout_s() {
        Some(t) => (t * 1000).min(l.timeout_ms),
        None => l.timeout_ms,
    };
    let run_dir = l.run_dir();
    if let Ok(mut g) = RUN_DIR.lock() {
        g.clone_from(&run_dir);
    }
    let code = (|| {
        if l.guard
            && run_dir.is_some()
            && let Some(c) = l.cold_guard_wait(&engine, eng_ms)
        {
            return c;
        }
        l.run_engine(&engine, run_dir.as_deref(), eng_ms)
    })();
    if let Some(d) = run_dir {
        let _ = std::fs::remove_dir_all(d);
    }
    code
}

fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    install_signal_thread();
    let code = std::panic::catch_unwind(run).unwrap_or(0);
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}
