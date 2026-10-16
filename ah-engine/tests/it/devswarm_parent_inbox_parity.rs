//! Node-vs-engine parity of `devswarm-parent-inbox` for a Primary that runs INSIDE a git checkout (roadmap #68 part 2B).
//!
//! Each scene builds a scratch HOME with a real git repository, seeds the state under test, then runs the real Node hook on one copy of that
//! tree and `ah-engine check devswarm-parent-inbox` on another copy at the SAME path (the project key and every stored path derive from it).
//! The contract:
//! - a scene the engine answers natively gives the same stdout, stderr and exit code as Node and leaves the same files behind (clock readings
//!   masked), so answering natively loses nothing;
//! - a scene the engine hands to Node (`AHFALLBACK`) has written nothing: its files are the seeded ones.
//!
//! Needs `git` and `node` on the PATH.
#![allow(clippy::unwrap_used, clippy::expect_used)] // a test crate: a panic is the failure report, and E2 exempts tests
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);
/// The Node answers of the scenes the engine answered natively: the corpus must exercise every text it claims to reproduce.
static ANSWERS: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Want {
    /// The engine answers and matches Node.
    Same,
    /// The engine hands the turn to Node before writing anything.
    Defer,
}

struct Scene {
    name: &'static str,
    want: Want,
    /// Seeds the state after the priming run and before the compared runs.
    seed: Box<dyn Fn(&Path)>,
    env: Vec<(&'static str, &'static str)>,
    session: &'static str,
    /// A first Node turn of the same session runs before the seed (so the dedupe store holds the reminder).
    prime: bool,
    repeat: usize,
    transcript: bool,
    sub: Option<&'static str>,
    /// The Codex host's payload: a turn id and a model beside the common fields.
    codex: bool,
    /// A relative transcript path in the payload.
    relative_transcript: bool,
}

fn scene(name: &'static str, want: Want) -> Scene {
    Scene { name, want, seed: Box::new(|_| {}), env: vec![], session: "s1", prime: false, repeat: 1, transcript: false, sub: None, codex: false, relative_transcript: false }
}

impl Scene {
    fn seed(mut self, f: impl Fn(&Path) + 'static) -> Scene {
        self.seed = Box::new(f);
        self
    }
    fn env(mut self, e: Vec<(&'static str, &'static str)>) -> Scene {
        self.env = e;
        self
    }
    fn prime(mut self) -> Scene {
        self.prime = true;
        self
    }
    fn codex(mut self) -> Scene {
        self.codex = true;
        self
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn plugin() -> PathBuf {
    repo_root().join("plugins/anti-hall").canonicalize().unwrap()
}

fn cmd(prog: impl AsRef<std::ffi::OsStr>, home: &Path, extra: &[(&str, &str)]) -> Command {
    let mut c = Command::new(prog);
    c.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AH_ENGINE_DIR", home.join("engine"))
        .env("AH_ENGINE_PLUGIN_ROOT", plugin())
        .env("CLAUDE_PLUGIN_ROOT", plugin())
        .env("ANTIHALL_TEST_ISOLATION", "1")
        .env("ANTIHALL_INGEST_DRY_RUN", "1")
        .env("DEVSWARM_REPO_ID", "repo-1");
    for (k, v) in extra {
        c.env(k, v);
    }
    c
}

fn run(mut c: Command, input: &str) -> (i32, String, String) {
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut ch = c.spawn().unwrap();
    ch.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let o = ch.wait_with_output().unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

fn node(home: &Path, payload: &str, env: &[(&str, &str)]) -> (i32, String, String) {
    let mut c = cmd("node", home, env);
    c.arg(plugin().join("hooks/devswarm-parent-inbox.js"));
    run(c, payload)
}

fn engine(home: &Path, payload: &str, env: &[(&str, &str)]) -> (i32, String, String) {
    let mut c = cmd(env!("CARGO_BIN_EXE_ah-engine"), home, env);
    c.args(["check", "devswarm-parent-inbox"]);
    run(c, payload)
}

fn git(repo: &Path, args: &[&str]) {
    let o = Command::new("git").arg("-C").arg(repo).args(["-c", "user.email=a@b", "-c", "user.name=n"]).args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let t = e.file_type().unwrap();
        if t.is_dir() {
            copy_dir(&src, &dst);
        } else if t.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&src).unwrap(), &dst).unwrap();
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

fn ds(home: &Path) -> PathBuf {
    home.join(".anti-hall/devswarm")
}

fn mask(t: &str) -> String {
    // a clock reading: 12 or more digits in a row
    let mut out = String::new();
    let mut run = String::new();
    for c in t.chars() {
        if c.is_ascii_digit() {
            run.push(c);
        } else {
            out.push_str(&if run.len() >= 12 { "{TS}".to_string() } else { std::mem::take(&mut run) });
            run.clear();
            out.push(c);
        }
    }
    out.push_str(&if run.len() >= 12 { "{TS}".to_string() } else { run });
    out
}

fn snapshot(home: &Path) -> BTreeMap<String, String> {
    fn walk(base: &Path, d: &Path, out: &mut BTreeMap<String, String>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            if rel == "engine" || rel.starts_with("proj") || rel.ends_with("-wal") || rel.ends_with("-shm") {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                walk(base, &p, out);
            } else {
                out.insert(rel, mask(&String::from_utf8_lossy(&std::fs::read(&p).unwrap_or_default())));
            }
        }
    }
    let mut m = BTreeMap::new();
    walk(home, home, &mut m);
    m
}

fn payload(cwd: &Path, session: &str, transcript: Option<&str>, codex: bool) -> String {
    let mut p = json!({"hook_event_name": "UserPromptSubmit", "session_id": session, "prompt": "hi", "cwd": cwd});
    if codex {
        p["turn_id"] = json!("t1");
        p["model"] = json!("gpt-5");
    }
    if let Some(t) = transcript {
        p["transcript_path"] = json!(t);
    }
    p.to_string()
}

fn write(p: &Path, t: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, t).unwrap();
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

/// The project key of the scratch checkout, asked of Node's own resolver.
fn project_key(home: &Path) -> String {
    let script = format!("process.stdout.write(require({:?}).repoKeyForWorktree({:?}))", plugin().join("companion/lib/devswarm-repokey.js"), home.join("proj"));
    let o = cmd("node", home, &[]).arg("-e").arg(script).output().unwrap();
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// The ingest daemon's heartbeat and lock, as a healthy daemon leaves them, and the project's store directory.
fn healthy_daemon(h: &Path) {
    let key = project_key(h);
    write(&ds(h).join(format!("heartbeats/ingest-{key}.json")), &json!({"ts": now_ms(), "pid": std::process::id()}).to_string());
    write(&ds(h).join(format!("locks/ingest-project-{key}.lock")), &json!({"pid": std::process::id()}).to_string());
}

fn store_dir(h: &Path) {
    let key = project_key(h);
    std::fs::create_dir_all(ds(h).join("store").join(key)).unwrap();
}

fn primary_id(h: &Path) -> String {
    let script = format!("process.stdout.write(require({:?}).primaryWorkspaceId({:?}))", plugin().join("companion/install-devswarm-ingest.js"), h.join("proj"));
    let o = cmd("node", h, &[]).arg("-e").arg(script).output().unwrap();
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn seat_marker(body: &'static str) -> impl Fn(&Path) {
    move |h| write(&ds(h).join("primary-seat/s1.json"), body)
}


/// Writes the project's summary: the workspaces object under `workspaces`, plus the extra top-level fields.
fn summary_with(rows: Value, extra: Value) -> impl Fn(&Path) {
    move |h| {
        let mut body = json!({"workspaces": rows});
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        write(&ds(h).join(format!("summaries/{}.json", project_key(h))), &body.to_string());
    }
}

fn summary(rows: Value) -> impl Fn(&Path) {
    summary_with(rows, json!({}))
}

/// A summary row for a child whose worktree is `$HOME/<id>-wt` (no checkout there, so no project key to compare).
fn row(h: &Path, id: &str, extra: Value) -> Value {
    let mut r = json!({"worktreePath": h.join(format!("{id}-wt")), "sessionId": format!("sess-{id}"), "directUnread": 0, "total": 0, "cursor": 0});
    for (k, v) in extra.as_object().unwrap() {
        r[k] = v.clone();
    }
    r
}

/// A summary of the given rows, built from the home so the worktrees sit under it.
fn rows_of(spec: Vec<(&'static str, Value)>) -> impl Fn(&Path) {
    move |h| {
        let mut o = serde_json::Map::new();
        for (id, extra) in &spec {
            o.insert((*id).to_string(), row(h, id, extra.clone()));
        }
        summary(Value::Object(o))(h);
    }
}

fn rows_with(spec: Vec<(&'static str, Value)>, extra: Value) -> impl Fn(&Path) {
    move |h| {
        let mut o = serde_json::Map::new();
        for (id, e) in &spec {
            o.insert((*id).to_string(), row(h, id, e.clone()));
        }
        summary_with(Value::Object(o), extra.clone())(h);
    }
}

/// A heartbeat of `id` written `age_ms` ago.
fn heartbeat(id: &'static str, age_ms: i64, extra: Value) -> impl Fn(&Path) {
    move |h| {
        let mut b = json!({"ts": now_ms() - age_ms});
        for (k, v) in extra.as_object().unwrap() {
            b[k] = v.clone();
        }
        write(&ds(h).join(format!("heartbeats/{id}.json")), &b.to_string());
    }
}

fn verdict(id: &'static str, body: Value) -> impl Fn(&Path) {
    move |h| write(&ds(h).join(format!("liveness/{id}.json")), &body.to_string())
}

fn name_of(id: &'static str, name: &'static str) -> impl Fn(&Path) {
    move |h| write(&ds(h).join(format!("names/{id}.json")), &json!({"name": name}).to_string())
}

/// A transcript of the child's session, last written `age_ms` ago.
fn transcript_aged(id: &'static str, age_ms: u64) -> impl Fn(&Path) {
    move |h| {
        let wt = h.join(format!("{id}-wt"));
        let enc: String = wt.to_string_lossy().chars().map(|c| if "/\\:.".contains(c) { '-' } else { c }).collect();
        let f = h.join(".claude/projects").join(enc).join(format!("sess-{id}.jsonl"));
        write(&f, "{}\n");
        let t = std::time::SystemTime::now() - std::time::Duration::from_millis(age_ms);
        std::fs::File::options().write(true).open(&f).unwrap().set_modified(t).unwrap();
    }
}

fn both(a: impl Fn(&Path) + 'static, b: impl Fn(&Path) + 'static) -> impl Fn(&Path) {
    move |h| {
        a(h);
        b(h);
    }
}

fn all3(a: impl Fn(&Path) + 'static, b: impl Fn(&Path) + 'static, c: impl Fn(&Path) + 'static) -> impl Fn(&Path) {
    move |h| {
        a(h);
        b(h);
        c(h);
    }
}

/// A summary with one daemon that is not healthy beside the rows.
fn stale_daemon(age_ms: i64) -> impl Fn(&Path) {
    move |h| {
        let key = project_key(h);
        write(&ds(h).join(format!("heartbeats/ingest-{key}.json")), &json!({"ts": now_ms() - age_ms, "pid": 999_998}).to_string());
        write(&ds(h).join(format!("locks/ingest-project-{key}.lock")), &json!({"pid": 999_999}).to_string());
    }
}

fn faulty_daemon(extra: Value) -> impl Fn(&Path) {
    move |h| {
        let key = project_key(h);
        let mut b = json!({"ts": now_ms() - 1_000, "pid": std::process::id()});
        for (k, v) in extra.as_object().unwrap() {
            b[k] = v.clone();
        }
        write(&ds(h).join(format!("heartbeats/ingest-{key}.json")), &b.to_string());
        write(&ds(h).join(format!("locks/ingest-project-{key}.lock")), &json!({"pid": std::process::id()}).to_string());
    }
}

const MIN: i64 = 60_000;

/// A turn of a child workspace on the scratch checkout, run by the real Node hook: it creates the project's store, a registry row, the
/// child's descriptor and the summary.
fn warm_child(h: &Path) {
    let mut c = cmd("node", h, &[("DEVSWARM_SOURCE_BRANCH", "feat/x"), ("DEVSWARM_BUILDER_ID", "child-1")]);
    c.arg(plugin().join("hooks/devswarm-child-turn.js"));
    let (code, _, err) = run(c, &payload(&h.join("proj"), "warm", None, false));
    assert_eq!(code, 0, "warm child turn failed: {err}");
}

/// Makes the summary on disk differ from what the store derives, so a refresh is visible.
fn stale_summary(h: &Path) {
    write(&ds(h).join(format!("summaries/{}.json", project_key(h))), r#"{"workspaces":{"stale-row":{"worktreePath":"/nowhere","sessionId":"x","directUnread":9}}}"#);
}

/// A session transcript of the checkout whose first record names `cwd`, last written `age_ms` ago.
fn session_file(sid: &'static str, cwd_sub: &'static str, age_ms: u64) -> impl Fn(&Path) {
    move |h| {
        let wt = h.join("proj");
        let enc: String = wt.to_string_lossy().chars().map(|c| if "/\\:.".contains(c) { '-' } else { c }).collect();
        let f = h.join(".claude/projects").join(enc).join(format!("{sid}.jsonl"));
        let cwd = if cwd_sub.is_empty() { wt.to_string_lossy().into_owned() } else if cwd_sub.starts_with('/') { cwd_sub.to_string() } else { wt.join(cwd_sub).to_string_lossy().into_owned() };
        write(&f, &format!("{}\n", json!({"cwd": cwd, "timestamp": "2026-10-01T00:00:00.000Z"})));
        let t = std::time::SystemTime::now() - std::time::Duration::from_millis(age_ms);
        std::fs::File::options().write(true).open(&f).unwrap().set_modified(t).unwrap();
    }
}

fn own_descriptor(anchor: &'static str) -> impl Fn(&Path) {
    move |h| write(&ds(h).join(format!("workspaces/{}.json", primary_id(h))), &json!({"id": primary_id(h), "worktreePath": h.join("proj"), "sessionId": anchor}).to_string())
}

fn notices(rows: Vec<Value>) -> impl Fn(&Path) {
    move |h| write(&ds(h).join("maintainer-notices.jsonl"), &rows.iter().map(|r| format!("{r}\n")).collect::<String>())
}

/// Adds a linked git worktree of the scratch checkout at `$HOME/<name>` (it shares the project key).
fn linked_worktree(h: &Path, name: &str) -> PathBuf {
    let wt = h.join(name);
    git(&h.join("proj"), &["worktree", "add", "-q", "-b", &format!("b-{name}"), wt.to_str().unwrap()]);
    wt
}

/// A workspace descriptor of a child on a linked worktree.
fn child_descriptor(h: &Path, id: &str, session: &str) {
    let wt = linked_worktree(h, &format!("{id}-wt"));
    write(&ds(h).join(format!("workspaces/{id}.json")), &json!({"id": id, "worktreePath": wt, "sessionId": session}).to_string());
}

const CLAUDE: [(&str, &str); 1] = [("DEVSWARM_AI_AGENT", "claude")];

/// The Primary's wake-tick marker, `age_min` minutes old.
fn tick_marker(age_min: i64) -> impl Fn(&Path) {
    move |h| write(&ds(h).join(format!("wake-tick/{}.json", primary_id(h))), &json!({"ts": now_ms() - age_min * MIN - 20_000}).to_string())
}

fn watcher_lock(pid: u32, age_ms: i64) -> impl Fn(&Path) {
    move |h| write(&ds(h).join(format!("locks/wake-watch-{}.lock", primary_id(h))), &json!({"ts": now_ms() - age_ms, "pid": pid}).to_string())
}

/// A fixture of the DevSwarm app's database: `(id, worktree, label, rank, pinned, active, hidden, builderType, lastSelectedAt)`.
type AppRow = (&'static str, Option<PathBuf>, &'static str, i64, i64, i64, i64, &'static str, Option<i64>);
/// A terminal: `(builder, created ms, delivered ms, withheld ms)`.

fn app_db(rows: Vec<AppRow>, terminals: Vec<(&'static str, i64, Option<i64>, Option<i64>)>) -> impl Fn(&Path) {
    move |h| {
        let f = h.join("app.db");
        std::fs::remove_file(&f).ok();
        let c = rusqlite::Connection::open(&f).unwrap();
        c.execute_batch(
            "CREATE TABLE builders (id TEXT, repositoryId TEXT, sourceBranch TEXT, branchName TEXT, worktreePath TEXT, terminalId TEXT, label TEXT, createdAt TEXT, lastAccessed TEXT, rank INTEGER, isHidden INTEGER, pullRequestId TEXT, builderType TEXT, isPinned INTEGER, isActive INTEGER, lastSelectedAt TEXT);
             CREATE TABLE builder_terminals (id TEXT, builderId TEXT, terminalId TEXT, terminalType TEXT, aiAgent TEXT, ai_session_config TEXT, isActive INTEGER, panelStatus TEXT, createdAt TEXT, lastViewedAt TEXT, initialPrompt TEXT, initialPromptDeliveredAt TEXT, initialPromptWithheldAt TEXT);
             CREATE TABLE pull_requests (id TEXT, repositoryId TEXT, branchName TEXT, number INTEGER, state TEXT, isDraft INTEGER, url TEXT, checkStatus TEXT, reviewStatus TEXT, lastSyncedAt TEXT);
             CREATE TABLE repositories (id TEXT, path TEXT, name TEXT, defaultBaseBranch TEXT);",
        )
        .unwrap();
        c.execute("INSERT INTO repositories VALUES ('r1', ?, 'proj', 'main')", rusqlite::params![h.join("proj").to_string_lossy()]).unwrap();
        for (id, wt, label, rank, pinned, active, hidden, bt, sel) in &rows {
            c.execute(
                "INSERT INTO builders (id, repositoryId, branchName, worktreePath, label, rank, isHidden, builderType, isPinned, isActive, lastSelectedAt) VALUES (?1,'r1','b',?2,?3,?4,?5,?6,?7,?8,?9)",
                rusqlite::params![id, wt.as_ref().map(|w| w.to_string_lossy().into_owned()), label, rank, hidden, bt, pinned, active, sel.map(iso)],
            )
            .unwrap();
        }
        for (builder, created, delivered, withheld) in &terminals {
            c.execute(
                "INSERT INTO builder_terminals VALUES (?1,?2,?3,'ai','claude',?4,1,'idle',?5,?5,'do the thing',?6,?7)",
                rusqlite::params![format!("t-{builder}-{created}"), builder, format!("term.{builder}"), format!("{{\"sessionId\":\"sess-{builder}\"}}"), iso(*created), delivered.map(iso), withheld.map(iso)],
            )
            .unwrap();
        }
    }
}

/// An epoch time as the ISO text the app stores (UTC, millisecond precision).
fn iso(ms: i64) -> String {
    let (secs, milli) = (ms.div_euclid(1000), ms.rem_euclid(1000));
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{milli:03}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

const APP_ENV: [(&str, &str); 1] = [("ANTIHALL_DEVSWARM_APP_DB", "$HOME/app.db")];

fn scenes() -> Vec<Scene> {
    use Want::{Defer, Same};
    let parked = |h: &Path, child: &str, extra: Value| {
        let mut body = json!({"parentId": primary_id(h), "childId": child, "row": {"x": 1}});
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        write(&ds(h).join(format!("escalation-pending/{child}.json")), &body.to_string());
    };
    let mut v = vec![
        scene("first-turn", Same),
        scene("first-turn-codex-payload", Same).codex(),
        scene("second-turn-suppressed", Same).prime(),
        {
            let mut s = scene("second-turn-other-session", Same).prime();
            s.session = "s2";
            s
        },
        {
            let mut s = scene("subdir-cwd", Same);
            s.sub = Some("sub/deep");
            s
        },
        {
            let mut s = scene("repeat-with-transcript", Same);
            s.repeat = 3;
            s.transcript = true;
            s
        },
        scene("switch-off", Same).seed(|h| write(&h.join(".anti-hall/settings.json"), r#"{"devswarm":{"parentInbox":false}}"#)),
        scene("dedupe-off", Same).prime().seed(|h| write(&h.join(".anti-hall/settings.json"), r#"{"guards":{"emitDedupe":false}}"#)),
        scene("healthy-daemon-with-store-dir", Same).seed(|h| {
            healthy_daemon(h);
            store_dir(h);
        }),
        scene("unhealthy-daemon-without-store", Same),
        scene("seat-marker-shown-twice", Same).seed(seat_marker("{\"shown\":2}")),
        scene("summary-without-rows", Same).seed(summary(json!({}))),
        scene("summary-scalar-reads-as-none", Same).seed(|h| write(&ds(h).join(format!("summaries/{}.json", project_key(h))), "5")),
        scene("summary-invalid-json", Defer).seed(|h| write(&ds(h).join(format!("summaries/{}.json", project_key(h))), "{torn")),
        scene("summary-workspaces-list", Defer).seed(|h| write(&ds(h).join(format!("summaries/{}.json", project_key(h))), r#"{"workspaces":[1]}"#)),
        scene("ignore-file-present", Same).seed(|h| write(&ds(h).join("ignore.json"), r#"{"ids":["x"]}"#)),
        scene("names-dir-present", Same).seed(|h| write(&ds(h).join("names/a.json"), r#"{"name":"A"}"#)),
        // supervisor escalations parked for this Primary
        scene("parked-one", Same).seed(move |h| parked(h, "child-a", json!({}))),
        scene("parked-two-sorted", Same).seed(move |h| {
            parked(h, "child-b", json!({}));
            parked(h, "child-a", json!({}));
        }),
        scene("parked-delivered-ignored", Same).seed(move |h| parked(h, "child-a", json!({"delivered": true}))),
        scene("parked-other-parent-ignored", Same).seed(move |h| parked(h, "child-a", json!({"parentId": "primary-other"}))),
        scene("parked-without-row-ignored", Same).seed(|h| write(&ds(h).join("escalation-pending/x.json"), r#"{"parentId":"p","childId":"c"}"#)),
        scene("parked-garbage-ignored", Same).seed(|h| write(&ds(h).join("escalation-pending/x.json"), "{torn")),
        scene("parked-not-json-name-ignored", Same).seed(|h| write(&ds(h).join("escalation-pending/x.txt"), "{}")),
        scene("parked-and-suppressed-override", Same).prime().seed(move |h| parked(h, "child-a", json!({}))),
        scene("parked-child-id-quoting", Same).seed(move |h| parked(h, "ch\"ild", json!({}))),
        scene("parked-child-id-not-a-string", Defer).seed(move |h| parked(h, "child-a", json!({"childId": 5}))),
        scene("parked-parent-id-not-a-string", Defer).seed(|h| write(&ds(h).join("escalation-pending/x.json"), r#"{"parentId":5,"childId":"c","row":{}}"#)),
        // the project's roster
        scene("rows-one-quiet", Same).seed(rows_of(vec![("w1", json!({}))])),
        scene("rows-two-heartbeats", Same).seed(all3(rows_of(vec![("w1", json!({})), ("w2", json!({}))]), heartbeat("w1", 7 * MIN + 30_000, json!({})), heartbeat("w2", 3 * 3_600_000 + 20 * MIN, json!({"progress_pct": 40})))),
        scene("rows-idle-after-hours", Same).seed(both(rows_of(vec![("w1", json!({}))]), heartbeat("w1", 7 * 3_600_000 + 20 * MIN, json!({})))),
        scene("rows-dormant-by-transcript", Same).seed(both(rows_of(vec![("w1", json!({}))]), transcript_aged("w1", 45 * 60_000))),
        scene("rows-active-by-transcript", Same).seed(both(rows_of(vec![("w1", json!({}))]), transcript_aged("w1", 5 * 60_000))),
        scene("rows-idle-alive", Same).seed(all3(rows_of(vec![("w1", json!({}))]), transcript_aged("w1", 45 * 60_000), |h| {
            write(&h.join(".claude/sessions/1.json"), &json!({"sessionId": "sess-w1", "pid": std::process::id()}).to_string());
        })),
        scene("rows-activity-from-verdict", Same).seed(both(rows_of(vec![("w1", json!({}))]), verdict("w1", json!({"lastOutboundTs": now_ms() - 12 * MIN - 20_000})))),
        scene("rows-archive-ready", Same).seed(rows_of(vec![("w1", json!({"archive_ready": true, "gates": {"done": true}, "mergedVerified": true}))])),
        scene("rows-done-not-merged", Same).seed(rows_of(vec![("w1", json!({"gates": {"done": true}, "mergedVerified": false}))])),
        scene("rows-done-merge-unverified", Same).seed(rows_of(vec![("w1", json!({"gates": {"done": true}}))])),
        scene("rows-risk-markers", Same).seed(rows_of(vec![("w1", json!({"noUpstream": true})), ("w2", json!({"unpushed": 3})), ("w3", json!({"archive_ready": true, "mergedVerified": false, "unpushed": 2}))])),
        scene("rows-name-with-pipe", Same).seed(both(rows_of(vec![("0123456789abcdef", json!({}))]), name_of("0123456789abcdef", "Fix: the | pipe"))),
        scene("rows-nudge-normal-unread", Same).seed(rows_of(vec![("w1", json!({"directUnread": 3, "total": 5, "cursor": 2, "oldestDirectUnreadTs": now_ms() - 18 * MIN - 30_000}))])),
        scene("rows-nudge-urgent", Same).seed(rows_of(vec![("w1", json!({"directUnread": 2, "urgencyMax": "urgent", "oldestDirectUnreadTs": now_ms() - 9 * MIN - 30_000}))])),
        scene("rows-nudge-high-and-normal", Same).seed(rows_of(vec![("w1", json!({"directUnread": 2, "urgencyMax": "high"})), ("w2", json!({"directUnread": 1}))])),
        scene("rows-nudge-low-priority-is-no-nudge", Same).seed(rows_of(vec![("w1", json!({"directUnread": 4, "urgencyMax": "low"}))])),
        scene("rows-nudge-stuck-status", Same).seed(rows_of(vec![("w1", json!({"status": "escalated"})), ("w2", json!({"status": "stale", "directUnread": 1})), ("w3", json!({"status": "nudged"}))])),
        scene("rows-nudge-seven-listed", Same).seed(rows_of((1..=8).map(|i| (["a1", "a2", "a3", "a4", "a5", "a6", "a7", "a8"][i - 1], json!({"directUnread": i}))).collect())),
        scene("rows-unread-without-timestamp", Same).seed(rows_of(vec![("w1", json!({"unread": 2}))])),
        scene("rows-unread-graced-from-own-send", Same).seed(|h| {
            let id = primary_id(h);
            rows_of(vec![("w1", json!({"directUnread": 1, "oldestDirectUnreadTs": now_ms() - 5_000, "oldestDirectUnreadSender": id}))])(h);
        }),
        scene("rows-unread-graced-but-other-sender", Same).seed(rows_of(vec![("w1", json!({"directUnread": 1, "oldestDirectUnreadTs": now_ms() - 70 * 1000, "oldestDirectUnreadSender": "someone"}))])),
        scene("rows-unread-past-the-grace", Same).seed(rows_of(vec![("w1", json!({"directUnread": 1, "oldestDirectUnreadTs": now_ms() - 7 * MIN - 30_000}))])),
        scene("rows-unread-graced-but-stuck", Same).seed(|h| {
            let id = primary_id(h);
            rows_of(vec![("w1", json!({"status": "stale", "directUnread": 1, "oldestDirectUnreadTs": now_ms() - 5_000, "oldestDirectUnreadSender": id}))])(h);
        }),
        scene("rows-trend-rising-flat-falling", Same).seed(both(
            rows_of(vec![("w1", json!({"directUnread": 3})), ("w2", json!({"directUnread": 3})), ("w3", json!({"directUnread": 3}))]),
            |h| {
                let line = json!({"ts": now_ms() - 1000, "event": "inject", "workspaces": [{"id": "w1", "unread": 1}, {"id": "w2", "unread": 3}, {"id": "w3", "unread": 9}]});
                write(&ds(h).join("parent-inbox.log"), &format!("{{torn\n{line}\n"));
            },
        )),
        scene("rows-second-turn-suppressed", Same).prime().seed(rows_of(vec![("w1", json!({"directUnread": 1}))])),
        {
            let mut s = scene("rows-repeat-turns", Same).seed(rows_of(vec![("w1", json!({"directUnread": 1})), ("w2", json!({}))]));
            s.repeat = 4;
            s.transcript = true;
            s
        },
        scene("rows-table-changes-between-turns", Same).prime().seed(rows_of(vec![("w1", json!({"directUnread": 1})), ("w2", json!({}))])).env(vec![("ANTIHALL_INJECTION_REPEAT_EVERY", "2")]),
        scene("rows-archive-ready-cooldown-elapsed", Same).seed(both(
            rows_of(vec![("w1", json!({"archive_ready": true}))]),
            |h| write(&ds(h).join("archive-nudges/w1.json"), &json!({"lastNudgedAt": now_ms() - 3_600_000}).to_string()),
        )),
        scene("rows-archive-ready-cooling-down", Same).seed(both(
            rows_of(vec![("w1", json!({"archive_ready": true}))]),
            |h| write(&ds(h).join("archive-nudges/w1.json"), &json!({"lastNudgedAt": now_ms() - 1000}).to_string()),
        )),
        scene("rows-archive-ready-seven", Same).seed(rows_of((1..=7).map(|i| (["b1", "b2", "b3", "b4", "b5", "b6", "b7"][i - 1], json!({"archive_ready": true}))).collect())),
        scene("rows-archive-request-pending", Same).seed(rows_of(vec![("w1", json!({"archive_ready": true, "directUnread": 1, "archive_request_only_unread": true, "oldestDirectUnreadTs": now_ms() - 20 * MIN}))])),
        scene("rows-archive-request-gone-stale", Same).seed(rows_of(vec![("w1", json!({"archive_ready": true, "directUnread": 1, "archive_request_only_unread": true, "oldestDirectUnreadTs": now_ms() - 30 * 3_600_000}))])),
        scene("rows-archive-ready-quiet-dead-session", Same).seed(rows_of(vec![("w1", json!({"archive_ready": true, "directUnread": 1, "archive_request_only_unread": true}))])),
        scene("rows-nag-ignored", Same).seed(both(rows_of(vec![("w1", json!({"directUnread": 2, "status": "escalated"}))]), |h| write(&ds(h).join("ignore.json"), r#"{"ids":["w1"]}"#))),
        scene("rows-archived-marker-is-collapsed", Same).seed(both(rows_of(vec![("w1", json!({})), ("w2", json!({}))]), |h| {
            write(&ds(h).join("archived/w2.json"), &json!({"worktreePath": h.join("w2-wt"), "sessionId": "sess-w2"}).to_string());
        })),
        scene("rows-archived-marker-shown-on-request", Same).env(vec![("ANTIHALL_ROSTER_HIDE_ARCHIVED", "0")]).seed(both(rows_of(vec![("w1", json!({})), ("w2", json!({"directUnread": 1}))]), |h| {
            write(&ds(h).join("archived/w2.json"), &json!({"worktreePath": h.join("w2-wt"), "sessionId": "sess-w2"}).to_string());
        })),
        scene("rows-archived-marker-of-another-worktree-ignored", Same).seed(both(rows_of(vec![("w1", json!({}))]), |h| {
            write(&ds(h).join("archived/w1.json"), &json!({"worktreePath": h.join("elsewhere"), "sessionId": "sess-w1"}).to_string());
        })),
        scene("rows-archived-marker-without-session", Same).seed(both(rows_of(vec![("w1", json!({}))]), |h| write(&ds(h).join("archived/w1.json"), "{}"))),
        scene("rows-archived-marker-garbage", Same).seed(both(rows_of(vec![("w1", json!({}))]), |h| write(&ds(h).join("archived/w1.json"), "{torn"))),
        scene("rows-archived-sibling-group", Same).seed(both(rows_of(vec![("w1", json!({}))]), |h| {
            write(&ds(h).join("archived/old-1.json"), &json!({"worktreePath": h.join("w1-wt"), "sessionId": "sess-w1"}).to_string());
        })),
        scene("rows-archived-sibling-other-session", Same).seed(both(rows_of(vec![("w1", json!({}))]), |h| {
            write(&ds(h).join("archived/old-1.json"), &json!({"worktreePath": h.join("w1-wt"), "sessionId": "other"}).to_string());
        })),
        scene("rows-archived-sibling-other-session-live", Same).seed(all3(rows_of(vec![("w1", json!({}))]), heartbeat("w1", 20_000, json!({})), |h| {
            write(&ds(h).join("archived/old-1.json"), &json!({"worktreePath": h.join("w1-wt"), "sessionId": "other"}).to_string());
        })),
        scene("rows-capped", Same).seed(rows_of((0..15).map(|i| (["c00", "c01", "c02", "c03", "c04", "c05", "c06", "c07", "c08", "c09", "c10", "c11", "c12", "c13", "c14"][i], json!({"directUnread": i % 3}))).collect())),
        scene("rows-capped-by-env", Same).env(vec![("ANTIHALL_ROSTER_MAX_ROWS", "2")]).seed(rows_of(vec![("w1", json!({})), ("w2", json!({})), ("w3", json!({}))])),
        scene("rows-other-project-skipped", Same).seed(|h| {
            let other = h.join("other");
            assert!(Command::new("git").args(["init", "-q"]).arg(&other).status().unwrap().success());
            git(&other, &["commit", "-q", "--allow-empty", "-m", "y"]);
            summary(json!({"x1": {"worktreePath": other, "sessionId": "s", "directUnread": 4}}))(h);
        }),
        scene("rows-same-project-worktree", Same).seed(|h| {
            summary(json!({"x1": {"worktreePath": h.join("proj"), "sessionId": "s", "directUnread": 1}}))(h);
        }),
        scene("rows-entry-not-an-object-skipped", Same).seed(summary(json!({"x1": 5, "x2": null}))),
        scene("rows-unsafe-id-skipped", Same).seed(summary(json!({"a b": {"directUnread": 1}}))),
        scene("rows-flat-unread-alias", Same).seed(summary(json!({"w1": {"unread": 2}}))),
        scene("rows-primary-own-entry-quiet", Same).seed(|h| {
            let id = primary_id(h);
            let mut o = serde_json::Map::new();
            o.insert(id, json!({"unread": 0, "pendingQuestions": []}));
            summary(Value::Object(o))(h);
        }),
        // the ingest daemon's banners over a roster
        scene("rows-ingest-never-ran", Same).seed(rows_of(vec![("w1", json!({}))])),
        scene("rows-ingest-stale", Same).seed(both(rows_of(vec![("w1", json!({}))]), stale_daemon(40 * MIN))),
        scene("rows-ingest-stale-fresh-heartbeat-live-lock", Same).seed(both(rows_of(vec![("w1", json!({}))]), |h| {
            let key = project_key(h);
            write(&ds(h).join(format!("heartbeats/ingest-{key}.json")), &json!({"ts": now_ms() - 240_000, "pid": 999_998}).to_string());
            write(&ds(h).join(format!("locks/ingest-project-{key}.lock")), &json!({"pid": std::process::id()}).to_string());
        })),
        scene("rows-ingest-healthy", Same).seed(both(rows_of(vec![("w1", json!({}))]), |h| healthy_daemon(h))),
        scene("rows-ingest-fault", Same).seed(both(rows_of(vec![("w1", json!({}))]), faulty_daemon(json!({"consecutiveMonitorFailures": 5, "lastMonitorErrorCode": "ENOENT", "lastMonitorError": "spawn hivecontrol ENOENT"})))),
        scene("rows-ingest-fault-suppressed-second-turn", Same).prime().seed(both(rows_of(vec![("w1", json!({}))]), faulty_daemon(json!({"consecutiveMonitorFailures": 3})))),
        scene("rows-ingest-stale-second-turn", Same).prime().seed(both(rows_of(vec![("w1", json!({}))]), stale_daemon(2 * 3_600_000))),
        scene("rows-ingest-no-rows-no-banner", Same).seed(both(summary(json!({})), stale_daemon(40 * MIN))),
        // everything the engine leaves to Node, decided before any write
        scene("rows-setting-grace-set", Defer).seed(both(rows_of(vec![("w1", json!({}))]), |h| write(&h.join(".anti-hall/settings.json"), r#"{"devswarm":{"inboxGraceSec":30}}"#))),
        scene("rows-setting-renag-set", Defer).seed(rows_of(vec![("w1", json!({}))])).env(vec![("ANTIHALL_DEVSWARM_ARCHIVE_REQUEST_RENAG_HOURS", "2")]),
        scene("rows-not-draining-verdict", Defer).seed(both(rows_of(vec![("w1", json!({}))]), verdict("w1", json!({"notDraining": true})))),
        scene("rows-step-plan", Defer).seed(both(rows_of(vec![("w1", json!({}))]), |h| write(&ds(h).join("plans/w1.json"), r#"{"steps":[{"n":1,"text":"a","status":"doing"}]}"#))),
        scene("rows-own-unread", Defer).seed(|h| {
            let id = primary_id(h);
            let mut o = serde_json::Map::new();
            o.insert(id, json!({"unread": 2}));
            summary(Value::Object(o))(h);
        }),
        scene("rows-own-pending-questions", Defer).seed(|h| {
            let id = primary_id(h);
            let mut o = serde_json::Map::new();
            o.insert(id, json!({"pendingQuestions": [{"seq": 1}]}));
            summary(Value::Object(o))(h);
        }),
        scene("rows-orphans", Defer).seed(rows_with(vec![("w1", json!({}))], json!({"orphans": [{"id": "o1", "unread": 2}]}))),
        scene("rows-stale-registry", Defer).seed(rows_with(vec![("w1", json!({}))], json!({"staleRegistryPartitions": [{"id": "o1"}]}))),
        scene("rows-broadcast-feed", Defer).seed(rows_with(vec![("w1", json!({}))], json!({"recent": [{"from": "x", "ts": 1, "summary": "s"}]}))),
        scene("rows-empty-orphans-list-is-nothing", Same).seed(rows_with(vec![("w1", json!({}))], json!({"orphans": [], "staleRegistryPartitions": [], "recent": []}))),
        scene("rows-app-state-file", Defer).seed(both(rows_of(vec![("w1", json!({}))]), |h| write(&ds(h).join("app-state.json"), "{}"))),
        scene("rows-app-db-unreadable", Defer).seed(both(rows_of(vec![("w1", json!({}))]), |h| write(&h.join("app.db"), "x"))).env(vec![("ANTIHALL_DEVSWARM_APP_DB", "$HOME/app.db")]),
        scene("rows-descriptor-present-not-claude", Same).seed(both(rows_of(vec![("w1", json!({}))]), |h| write(&ds(h).join("workspaces/w1.json"), r#"{"id":"w1"}"#))),
        scene("rows-auto-archive-record-names-the-row", Defer).seed(both(rows_of(vec![("w1", json!({"archive_ready": true}))]), |h| {
            write(&ds(h).join("auto-archive-state.json"), &json!({"owned": ["w1"], "at": now_ms()}).to_string());
        })),
        scene("rows-auto-archive-record-other-row", Same).seed(both(rows_of(vec![("w1", json!({"archive_ready": true}))]), |h| {
            write(&ds(h).join("auto-archive-state.json"), &json!({"owned": ["zz"], "at": now_ms()}).to_string());
        })),
        scene("rows-auto-archive-record-expired", Same).seed(both(rows_of(vec![("w1", json!({"archive_ready": true}))]), |h| {
            write(&ds(h).join("auto-archive-state.json"), &json!({"owned": ["w1"], "at": now_ms() - 7_200_000}).to_string());
        })),
        scene("rows-codex-payload", Same).codex().seed(rows_of(vec![("w1", json!({"directUnread": 1}))])),
        // the mailbox-wake line over the descriptors
        scene("wake-claude-no-descriptors", Same).env(CLAUDE.to_vec()),
        scene("wake-claude-live-child-recent-tick", Same).env(CLAUDE.to_vec()).seed(both(|h| child_descriptor(h, "w1", "sess-w1"), tick_marker(10))),
        scene("wake-claude-live-child-no-tick-no-watcher", Same).env(CLAUDE.to_vec()).seed(|h| child_descriptor(h, "w1", "sess-w1")),
        scene("wake-claude-live-child-stale-tick", Same).env(CLAUDE.to_vec()).seed(both(|h| child_descriptor(h, "w1", "sess-w1"), tick_marker(90))),
        scene("wake-claude-live-child-no-tick-live-watcher", Same).env(CLAUDE.to_vec()).seed(both(|h| child_descriptor(h, "w1", "sess-w1"), watcher_lock(std::process::id(), 10_000))),
        scene("wake-claude-live-child-watcher-lock-stale", Same).env(CLAUDE.to_vec()).seed(both(|h| child_descriptor(h, "w1", "sess-w1"), watcher_lock(std::process::id(), 600_000))),
        scene("wake-claude-live-child-watcher-dead", Same).env(CLAUDE.to_vec()).seed(both(|h| child_descriptor(h, "w1", "sess-w1"), watcher_lock(999_999, 10_000))),
        scene("wake-claude-watcher-of-a-twin-row", Same).env(CLAUDE.to_vec()).seed(all3(|h| child_descriptor(h, "w1", "sess-w1"), |h| write(&ds(h).join(format!("workspaces/twin.json")), &json!({"id": "twin", "worktreePath": h.join("proj"), "sessionId": "t"}).to_string()), |h| write(&ds(h).join("locks/wake-watch-twin.lock"), &json!({"ts": now_ms() - 1000, "pid": std::process::id()}).to_string()))),
        scene("wake-claude-child-archived-by-marker", Same).env(CLAUDE.to_vec()).seed(both(|h| child_descriptor(h, "w1", "sess-w1"), |h| write(&ds(h).join("archived/w1.json"), &json!({"worktreePath": h.join("w1-wt"), "sessionId": "sess-w1"}).to_string()))),
        scene("wake-claude-child-archive-ignored", Same).env(CLAUDE.to_vec()).seed(both(|h| child_descriptor(h, "w1", "sess-w1"), |h| write(&ds(h).join("archive-ignore/w1.json"), "{}"))),
        scene("wake-claude-child-of-another-project", Same).env(CLAUDE.to_vec()).seed(|h| {
            let other = h.join("other");
            assert!(Command::new("git").args(["init", "-q"]).arg(&other).status().unwrap().success());
            git(&other, &["commit", "-q", "--allow-empty", "-m", "y"]);
            write(&ds(h).join("workspaces/w1.json"), &json!({"id": "w1", "worktreePath": other, "sessionId": "s"}).to_string());
        }),
        scene("wake-claude-descriptor-without-session", Same).env(CLAUDE.to_vec()).seed(|h| write(&ds(h).join("workspaces/w1.json"), &json!({"id": "w1", "worktreePath": h.join("proj")}).to_string())),
        scene("wake-claude-descriptor-unsafe-id", Same).env(CLAUDE.to_vec()).seed(|h| write(&ds(h).join("workspaces/w1.json"), &json!({"id": "a b", "worktreePath": h.join("proj-x"), "sessionId": "s"}).to_string())),
        scene("wake-claude-descriptor-garbage", Same).env(CLAUDE.to_vec()).seed(|h| write(&ds(h).join("workspaces/w1.json"), "{torn")),
        scene("wake-claude-second-turn-suppressed", Same).env(CLAUDE.to_vec()).prime().seed(|h| child_descriptor(h, "w1", "sess-w1")),
        scene("wake-claude-with-rows", Same).env(CLAUDE.to_vec()).seed(both(rows_of(vec![("w1", json!({"directUnread": 1}))]), |h| child_descriptor(h, "w1", "sess-w1"))),
        scene("wake-codex-style-agent", Same).env(vec![("DEVSWARM_AI_AGENT", "codex")]).seed(|h| child_descriptor(h, "w1", "sess-w1")),
        scene("wake-cron-warn-setting-set", Defer).env(CLAUDE.to_vec()).seed(both(|h| child_descriptor(h, "w1", "sess-w1"), |h| write(&h.join(".anti-hall/settings.json"), r#"{"devswarm":{"cronMissingWarnMin":30}}"#))),
        scene("wake-held-setting-set", Defer).env(CLAUDE.to_vec()).seed(both(|h| child_descriptor(h, "w1", "sess-w1"), |h| write(&h.join(".anti-hall/settings.json"), r#"{"devswarm":{"heldPartitions":"x"}}"#))),
        // the DevSwarm app's database
        scene("app-rows-label-rank-pinned", Same).env(APP_ENV.to_vec()).seed(both(rows_of(vec![("w1", json!({})), ("w2", json!({}))]), |h| {
            app_db(vec![("w1", Some(h.join("w1-wt")), "Alpha title", 5, 1, 1, 0, "standard", None), ("w2", Some(h.join("w2-wt")), "Beta", 1, 0, 1, 0, "standard", None)], vec![])(h);
        })),
        scene("app-rows-on-screen-suppresses-the-nudge", Same).env(APP_ENV.to_vec()).seed(both(rows_of(vec![("w1", json!({"directUnread": 3, "status": "escalated"}))]), |h| {
            app_db(vec![("w1", Some(h.join("w1-wt")), "Alpha", 1, 1, 1, 0, "standard", Some(now_ms() - 30_000))], vec![])(h);
        })),
        scene("app-rows-focus-window-off", Same).env(vec![("ANTIHALL_DEVSWARM_APP_DB", "$HOME/app.db"), ("ANTIHALL_DEVSWARM_FOCUS_MS", "0")]).seed(both(rows_of(vec![("w1", json!({"directUnread": 3}))]), |h| {
            app_db(vec![("w1", Some(h.join("w1-wt")), "Alpha", 1, 0, 1, 0, "standard", Some(now_ms() - 30_000))], vec![])(h);
        })),
        scene("app-rows-archived-in-the-app", Same).env(APP_ENV.to_vec()).seed(both(rows_of(vec![("w1", json!({})), ("w2", json!({"status": "escalated", "directUnread": 2}))]), |h| {
            app_db(vec![("w1", Some(h.join("w1-wt")), "Alpha", 1, 0, 1, 0, "standard", None), ("w2", Some(h.join("w2-wt")), "Beta", 2, 0, 0, 1, "standard", None)], vec![])(h);
        })),
        scene("app-rows-brief-not-delivered", Same).env(APP_ENV.to_vec()).seed(both(rows_of(vec![("w1", json!({}))]), |h| {
            app_db(vec![("w1", Some(h.join("w1-wt")), "Alpha", 1, 0, 1, 0, "standard", None)], vec![("w0", 1_700_000_000_000, Some(1_700_000_005_000), None), ("w1", now_ms() - 3_600_000, None, None)])(h);
        })),
        scene("app-rows-brief-withheld", Same).env(APP_ENV.to_vec()).seed(both(rows_of(vec![("w1", json!({}))]), |h| {
            app_db(vec![("w1", Some(h.join("w1-wt")), "Alpha", 1, 0, 1, 0, "standard", None)], vec![("w1", now_ms() - 3_600_000, None, Some(now_ms() - 3_000_000))])(h);
        })),
        scene("app-rows-primary-builder-of-this-checkout", Defer).env(APP_ENV.to_vec()).seed(both(summary_with(json!({"p-app": {"worktreePath": "x", "sessionId": "s", "directUnread": 1}}), json!({})), |h| {
            // the summary row sits on the Primary's own checkout
            let body = json!({"workspaces": {"p-app": {"worktreePath": h.join("proj"), "sessionId": "s", "directUnread": 1}}});
            write(&ds(h).join(format!("summaries/{}.json", project_key(h))), &body.to_string());
            app_db(vec![("p-app", Some(h.join("proj")), "proj", 1, 0, 1, 0, "primary", None)], vec![])(h);
        })),
        scene("app-rows-ghost-folds-into-its-canonical-row", Same).env(APP_ENV.to_vec()).seed(both(|h| {
            let mut o = serde_json::Map::new();
            o.insert("w1".to_string(), row(h, "w1", json!({"directUnread": 1})));
            o.insert("primary-abcdef12".to_string(), json!({"worktreePath": h.join("w1-wt"), "sessionId": "g", "directUnread": 2}));
            summary(Value::Object(o))(h);
        }, |h| {
            app_db(vec![("w1", Some(h.join("w1-wt")), "Alpha", 1, 0, 1, 0, "standard", None)], vec![])(h);
        })),
        scene("rows-ghost-folds-by-retired-record", Same).seed(both(|h| {
            let mut o = serde_json::Map::new();
            o.insert("w1".to_string(), row(h, "w1", json!({"directUnread": 1})));
            o.insert("primary-abcdef12".to_string(), json!({"worktreePath": h.join("g-wt"), "sessionId": "g", "directUnread": 2}));
            summary(Value::Object(o))(h);
        }, |h| write(&ds(h).join("retired/primary-abcdef12.json"), &json!({"retiredTo": "w1"}).to_string()))),
        scene("rows-ghost-without-a-canonical-row-stays", Same).seed(|h| {
            let mut o = serde_json::Map::new();
            o.insert("primary-abcdef12".to_string(), json!({"worktreePath": h.join("g-wt"), "sessionId": "g", "directUnread": 2}));
            summary(Value::Object(o))(h);
        }),
        scene("app-live-child-no-descriptor", Same).env([&APP_ENV[..], &CLAUDE[..]].concat()).seed(|h| {
            app_db(vec![("c1", Some(h.join("c1-wt")), "Child", 1, 0, 1, 0, "standard", None)], vec![])(h);
        }),
        scene("app-live-child-no-descriptor-with-tick", Same).env([&APP_ENV[..], &CLAUDE[..]].concat()).seed(both(tick_marker(5), |h| {
            app_db(vec![("c1", Some(h.join("c1-wt")), "Child", 1, 0, 1, 0, "standard", None)], vec![])(h);
        })),
        scene("app-archived-child-is-not-live", Same).env([&APP_ENV[..], &CLAUDE[..]].concat()).seed(|h| {
            app_db(vec![("c1", Some(h.join("c1-wt")), "Child", 1, 0, 0, 1, "standard", None)], vec![])(h);
        }),
        scene("app-only-primary-builder-is-not-a-child", Same).env([&APP_ENV[..], &CLAUDE[..]].concat()).seed(|h| {
            app_db(vec![("p1", Some(h.join("proj")), "proj", 1, 0, 1, 0, "primary", None)], vec![])(h);
        }),
        // the Primary's own anchor and the session drift
        scene("drift-own-descriptor-same-session", Same).seed(own_descriptor("s1")),
        scene("drift-newer-session-exists", Same).seed(all3(own_descriptor("s0"), session_file("s1", "", 600_000), session_file("s2", "", 60_000))),
        scene("drift-newer-session-exists-codex", Same).codex().seed(all3(own_descriptor("s0"), session_file("s1", "", 600_000), session_file("s2", "", 60_000))),
        scene("drift-newer-session-second-turn", Same).prime().seed(all3(own_descriptor("s0"), session_file("s1", "", 600_000), session_file("s2", "", 60_000))),
        scene("drift-current-is-the-newest", Same).seed(all3(own_descriptor("s0"), session_file("s1", "", 60_000), session_file("s2", "", 600_000))),
        scene("drift-anchor-is-the-newest", Same).seed(all3(own_descriptor("s2"), session_file("s1", "", 600_000), session_file("s2", "", 60_000))),
        scene("drift-unclaimed-anchor", Same).seed(all3(own_descriptor("unclaimed:x"), session_file("s1", "", 600_000), session_file("s2", "", 60_000))),
        scene("drift-no-transcript-for-the-current-session", Same).seed(both(own_descriptor("s0"), session_file("s2", "", 60_000))),
        scene("drift-transcript-of-another-directory-ignored", Same).seed(all3(own_descriptor("s0"), session_file("s1", "", 600_000), session_file("s2", "/elsewhere", 60_000))),
        scene("drift-transcript-of-a-subdirectory-counts", Same).seed(all3(own_descriptor("s0"), session_file("s1", "", 600_000), session_file("s2", "sub/deep", 60_000))),
        scene("drift-transcript-without-a-cwd", Same).seed(all3(own_descriptor("s0"), session_file("s1", "", 600_000), |h| {
            let wt = h.join("proj");
            let enc: String = wt.to_string_lossy().chars().map(|c| if "/\\:.".contains(c) { '-' } else { c }).collect();
            write(&h.join(".claude/projects").join(enc).join("s2.jsonl"), "{}\n");
        })),
        scene("drift-descriptor-without-a-session", Same).seed(all3(|h| write(&ds(h).join(format!("workspaces/{}.json", primary_id(h))), "{}"), session_file("s1", "", 600_000), session_file("s2", "", 60_000))),
        scene("drift-descriptor-garbage", Same).seed(all3(|h| write(&ds(h).join(format!("workspaces/{}.json", primary_id(h))), "{torn"), session_file("s1", "", 600_000), session_file("s2", "", 60_000))),
        scene("drift-descriptor-is-a-bare-zero", Same).seed(all3(|h| write(&ds(h).join(format!("workspaces/{}.json", primary_id(h))), "0"), session_file("s1", "", 600_000), session_file("s2", "", 60_000))),
        scene("drift-with-the-current-transcript-path", Same).seed(all3(own_descriptor("s0"), session_file("s2", "", 60_000), |h| {
            let t = h.join(".claude/projects/-p/t.jsonl");
            write(&t, "");
            let old = std::time::SystemTime::now() - std::time::Duration::from_millis(900_000);
            std::fs::File::options().write(true).open(&t).unwrap().set_modified(old).unwrap();
        })),
        scene("drift-twenty-seven-sessions", Same).seed(all3(own_descriptor("s0"), |h| {
            for i in 0..27u64 {
                let name: &'static str = Box::leak(format!("x{i:02}").into_boxed_str());
                session_file(name, "", 60_000 + i * 1000)(h);
            }
        }, session_file("s1", "", 7_200_000))),
        // the maintainer-notice feed
        scene("notices-feed-empty", Same).seed(notices(vec![])),
        scene("notices-all-expired", Same).seed(notices(vec![json!({"id": "n1", "ts": 1, "expiresAt": 2, "text": "old"})])),
        scene("notices-garbage-lines", Same).seed(|h| write(&ds(h).join("maintainer-notices.jsonl"), "{torn\n\n[1]\n{\"x\":1}\n")),
        scene("notices-all-seen", Same).seed(both(notices(vec![json!({"id": "n1", "ts": 1, "text": "a"}), json!({"id": "n2", "ts": 2, "text": "b"})]), |h| {
            write(&ds(h).join(format!("maintainer-notice-cursor/{}.json", project_key(h))), r#"{"lastSeenId":"n2"}"#);
        })),
        scene("notices-unseen-defers", Defer).seed(notices(vec![json!({"id": "n1", "ts": 1, "text": "a"})])),
        scene("notices-partly-seen-defers", Defer).seed(both(notices(vec![json!({"id": "n1", "ts": 1, "text": "a"}), json!({"id": "n2", "ts": 2, "text": "b"})]), |h| {
            write(&ds(h).join(format!("maintainer-notice-cursor/{}.json", project_key(h))), r#"{"lastSeenId":"n1"}"#);
        })),
        scene("notices-unseen-but-the-cursor-is-unknown", Defer).seed(both(notices(vec![json!({"id": "n1", "ts": 1, "text": "a"})]), |h| {
            write(&ds(h).join(format!("maintainer-notice-cursor/{}.json", project_key(h))), r#"{"lastSeenId":"gone"}"#);
        })),
        scene("notices-show-setting-set", Defer).seed(both(notices(vec![json!({"id": "n1", "ts": 1, "text": "a"})]), |h| write(&h.join(".anti-hall/settings.json"), r#"{"devswarm":{"maintainerNotice":{"show":false}}}"#))),
        // the summary refresh the hook makes while the ingest daemon is not running
        scene("refresh-store-with-a-row", Same).seed(warm_child),
        scene("refresh-replaces-a-stale-summary", Same).seed(both(warm_child, stale_summary)),
        scene("refresh-skipped-while-the-daemon-is-healthy", Same).seed(all3(warm_child, stale_summary, healthy_daemon)),
        scene("refresh-second-turn", Same).prime().seed(both(warm_child, stale_summary)),
        scene("refresh-with-descriptor-of-the-child-claude", Same).env(CLAUDE.to_vec()).seed(both(warm_child, stale_summary)),
        scene("refresh-journal-backend", Defer).seed(both(warm_child, |h| write(&ds(h).join("store").join(project_key(h)).join("BACKEND"), "journal\n"))),
        scene("refresh-store-dir-without-a-database", Defer).seed(|h| {
            std::fs::create_dir_all(ds(h).join("store").join(project_key(h))).unwrap();
            write(&ds(h).join("store").join(project_key(h)).join("BACKEND"), "sqlite\n");
        }),
        // the cases that stay with Node, each decided before anything is written
        scene("summary-empty-file", Defer).seed(|h| write(&ds(h).join(format!("summaries/{}.json", project_key(h))), "")),
        scene("descriptor-present-not-claude", Same).seed(|h| write(&ds(h).join("workspaces/child-1.json"), r#"{"id":"child-1"}"#)),
        scene("app-db-unreadable-without-rows", Same).seed(|h| write(&h.join("app.db"), "x")).env(vec![("ANTIHALL_DEVSWARM_APP_DB", "$HOME/app.db")]),
        scene("unhealthy-daemon-with-an-empty-store-dir", Same).seed(store_dir),
        scene("seat-marker-shown-once", Defer).seed(seat_marker("{\"shown\":1}")),
        {
            let mut s = scene("relative-transcript-path", Defer);
            s.relative_transcript = true;
            s
        },
    ];
    v.shrink_to_fit();
    v
}

fn run_scene(s: &Scene) -> Result<bool, String> {
    let root = std::env::var_os("AH_ENGINE_IT_ROOT").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let base = root.join(format!("ah-pip-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    ah_engine::discard::harmless(std::fs::remove_dir_all(&base));
    let home = base.join("h");
    for d in [".anti-hall", "engine"] {
        std::fs::create_dir_all(home.join(d)).unwrap();
    }
    let repo = home.join("proj");
    assert!(Command::new("git").args(["init", "-q"]).arg(&repo).status().unwrap().success());
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "x"]);
    let cwd = match s.sub {
        Some(sub) => repo.join(sub),
        None => repo.clone(),
    };
    std::fs::create_dir_all(&cwd).unwrap();
    let home = home.canonicalize().unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let home_str = home.to_string_lossy().into_owned();
    // an environment value may name the scratch home
    let env: Vec<(String, String)> = s.env.iter().map(|(k, v)| ((*k).to_string(), v.replace("$HOME", &home_str))).collect();
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let tpath = home.join(".claude/projects/-p/t.jsonl");
    let transcript = if s.relative_transcript {
        Some("x/t.jsonl".to_string())
    } else {
        s.transcript.then(|| {
            write(&tpath, "");
            tpath.to_string_lossy().into_owned()
        })
    };
    let pl = payload(&cwd, s.session, transcript.as_deref(), s.codex);
    if s.prime {
        node(&home, &payload(&cwd, "s1", transcript.as_deref(), s.codex), &env);
    }
    (s.seed)(&home);
    let before = snapshot(&home);
    let saved = base.join("saved");
    copy_dir(&home, &saved);
    let mut n = (0, String::new(), String::new());
    for _ in 0..s.repeat {
        n = node(&home, &pl, &env);
    }
    let node_tree = snapshot(&home);
    std::fs::remove_dir_all(&home).unwrap();
    copy_dir(&saved, &home);
    let mut e = (0, String::new(), String::new());
    for _ in 0..s.repeat {
        e = engine(&home, &pl, &env);
    }
    let eng_tree = snapshot(&home);
    ah_engine::discard::harmless(std::fs::remove_dir_all(&base));
    let deferred = e.1.trim() == "AHFALLBACK";
    let diff = |a: &BTreeMap<String, String>, b: &BTreeMap<String, String>, la: &str, lb: &str| -> String {
        a.keys()
            .chain(b.keys())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter(|k| a.get(*k) != b.get(*k))
            .map(|k| format!("  {k}\n   {la}: {:?}\n   {lb}: {:?}", a.get(k), b.get(k)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    match (s.want, deferred) {
        (Want::Defer, true) => {
            if eng_tree != before {
                return Err(format!("{}: deferred after writing\n{}", s.name, diff(&before, &eng_tree, "seeded", "engine")));
            }
            Ok(false)
        }
        (Want::Defer, false) => Err(format!("{}: expected a deferral, the engine answered {:?}", s.name, e.1)),
        (Want::Same, true) => Err(format!("{}: expected a native answer, the engine deferred", s.name)),
        (Want::Same, false) => {
            if (n.0, &n.1, &n.2) != (e.0, &e.1, &e.2) {
                return Err(format!("{}: output differs\n node: {n:?}\n eng : {e:?}", s.name));
            }
            if node_tree != eng_tree {
                return Err(format!("{}: files differ\n{}", s.name, diff(&node_tree, &eng_tree, "node", "eng ")));
            }
            ANSWERS.lock().unwrap().push_str(&n.1);
            Ok(true)
        }
    }
}

#[test]
fn engine_and_node_agree_for_a_primary_in_a_checkout() {
    let all = scenes();
    let (mut native, mut deferred) = (0, 0);
    let mut problems = Vec::new();
    for s in &all {
        match run_scene(s) {
            Ok(true) => native += 1,
            Ok(false) => deferred += 1,
            Err(e) => problems.push(e),
        }
    }
    eprintln!("scenes {}: native {native}, deferred {deferred}", all.len());
    assert!(problems.is_empty(), "{} problems:\n{}", problems.len(), problems.join("\n"));
    let answers = ANSWERS.lock().unwrap().clone();
    for text in [
        "devswarm-comms",
        "devswarm-escalations",
        "devswarm-stale-data",
        "devswarm-ingest-failing",
        "devswarm-workspaces",
        "[pinned, on screen]",
        "brief not delivered",
        "brief withheld",
        "devswarm-parent-inbox:",
        "devswarm-urgent-inbox",
        "devswarm-archive-ready",
        "NO MAILBOX WAKE PATH",
        "NO MAILBOX TICK",
        "devswarm-primary-session",
        "(capped at 12)",
        "archived (done",
        "dormant = no recent",
        "rising",
        "falling",
        "local only (not pushed)",
        "idle (alive)",
        "archived-superseded",
    ] {
        assert!(answers.contains(text), "no native scene exercises {text:?}");
    }
}
