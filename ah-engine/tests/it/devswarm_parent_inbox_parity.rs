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

fn escalation(name: &'static str, body: Value) -> impl Fn(&Path) {
    move |h| write(&ds(h).join(format!("escalation-pending/{name}.json")), &body.to_string())
}

fn primary_id(h: &Path) -> String {
    let script = format!("process.stdout.write(require({:?}).primaryWorkspaceId({:?}))", plugin().join("companion/install-devswarm-ingest.js"), h.join("proj"));
    let o = cmd("node", h, &[]).arg("-e").arg(script).output().unwrap();
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn seat_marker(body: &'static str) -> impl Fn(&Path) {
    move |h| write(&ds(h).join("primary-seat/s1.json"), body)
}

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
        // the cases that stay with Node, each decided before anything is written
        scene("summary-present", Defer).seed(|h| write(&ds(h).join(format!("summaries/{}.json", project_key(h))), r#"{"workspaces":{}}"#)),
        scene("summary-empty-file", Defer).seed(|h| write(&ds(h).join(format!("summaries/{}.json", project_key(h))), "")),
        scene("descriptor-present", Defer).seed(|h| write(&ds(h).join("workspaces/child-1.json"), r#"{"id":"child-1"}"#)),
        scene("primary-descriptor-present", Defer).seed(|h| write(&ds(h).join(format!("workspaces/{}.json", primary_id(h))), r#"{"sessionId":"s0"}"#)),
        scene("maintainer-notices-present", Defer).seed(|h| write(&ds(h).join("maintainer-notices.jsonl"), "")),
        scene("app-db-present", Defer).seed(|h| write(&h.join("app.db"), "x")).env(vec![("ANTIHALL_DEVSWARM_APP_DB", "$HOME/app.db")]),
        scene("unhealthy-daemon-with-store", Defer).seed(store_dir),
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
}
