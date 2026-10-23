//! Node-vs-engine parity of `devswarm-child-turn` for a child that runs INSIDE a git checkout (roadmap #68 part 2).
//!
//! Each scenario builds a scratch HOME with a real git repository, lets the real Node hook run once (it creates the project's store), seeds the
//! state under test, then runs the Node hook on one copy of that tree and `ah-engine check devswarm-child-turn` on another copy at the SAME
//! path (the project key and every stored path derive from it). The contract:
//! - a scenario the engine answers natively gives the same stdout and stderr as Node and leaves the same files behind (the store compared
//!   table by table, clock readings masked), so answering natively loses nothing;
//! - a scenario the engine hands to Node (`AHFALLBACK`) writes nothing the Node run would then repeat: its heartbeat is the warm run's.
//!
//! Needs `git` and `node` on the PATH. The dispatcher-level run on both hosts (no Node process at all) is in `native_session_checks.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)] // a test crate: a panic is the failure report, and E2 exempts tests
use rusqlite::Connection;
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
    /// The engine hands the turn to Node after the registration, which Node repeats without changing a byte of the outcome: Node run on
    /// the engine's files gives Node's own stdout and files.
    DeferLate,
}

struct Scene {
    name: &'static str,
    want: Want,
    /// Seeds the state after the warm run (and the healthy ingest daemon) and before the compared runs.
    seed: Box<dyn Fn(&Path)>,
    env: Vec<(&'static str, &'static str)>,
    /// Environment of the warm run, when it differs (a builder id that registers a store).
    warm_env: Option<Vec<(&'static str, &'static str)>>,
    session: &'static str,
    repeat: usize,
    transcript: bool,
    sub: Option<&'static str>,
    healthy: bool,
    warm: bool,
    /// The Codex host's payload: a turn id and a model beside the common fields.
    codex: bool,
}

fn scene(name: &'static str, want: Want) -> Scene {
    Scene {
        name,
        want,
        seed: Box::new(|_| {}),
        env: vec![],
        warm_env: None,
        session: "s1",
        repeat: 1,
        transcript: false,
        sub: None,
        healthy: true,
        warm: true,
        codex: false,
    }
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
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn plugin() -> PathBuf {
    repo_root().join("plugins/anti-hall").canonicalize().unwrap()
}

fn cmd(prog: impl AsRef<std::ffi::OsStr>, home: &Path, extra: &[(&str, &str)]) -> Command {
    let mut c = Command::new(prog);
    c.current_dir(home) // a relative path in a scene (inboxPath "in.ndjson") resolves inside the scratch home, never the repo
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AH_ENGINE_DIR", home.join("engine"))
        .env("AH_ENGINE_PLUGIN_ROOT", plugin())
        .env("CLAUDE_PLUGIN_ROOT", plugin())
        .env("ANTIHALL_TEST_ISOLATION", "1")
        .env("ANTIHALL_INGEST_DRY_RUN", "1")
        .env("DEVSWARM_REPO_ID", "repo-1")
        .env("DEVSWARM_SOURCE_BRANCH", "feat/x")
        .env("DEVSWARM_BUILDER_ID", "child-1");
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
    c.arg(plugin().join("hooks/devswarm-child-turn.js"));
    run(c, payload)
}

fn engine(home: &Path, payload: &str, env: &[(&str, &str)]) -> (i32, String, String) {
    let mut c = cmd(env!("CARGO_BIN_EXE_ah-engine"), home, env);
    c.args(["check", "devswarm-child-turn"]);
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

fn dump_db(p: &Path) -> String {
    let tmp = std::env::temp_dir().join(format!("ct-dump-{}-{}.db", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    std::fs::copy(p, &tmp).unwrap();
    for ext in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{ext}", p.display()));
        if side.exists() {
            std::fs::copy(&side, PathBuf::from(format!("{}{ext}", tmp.display()))).unwrap();
        }
    }
    let c = Connection::open(&tmp).unwrap();
    let mut out = String::new();
    // the schema as Node created it: tables and indexes, without SQLite's own bookkeeping
    let ddl: Vec<String> = c
        .prepare("SELECT sql FROM sqlite_master WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    out.push_str(&ddl.join("\n"));
    out.push('\n');
    let tables: Vec<String> = c
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for t in tables {
        let mut st = c.prepare(&format!("SELECT * FROM \"{t}\"")).unwrap();
        let n = st.column_count();
        let mut rows: Vec<String> = st
            .query_map([], |r| {
                Ok((0..n)
                    .map(|i| match r.get_ref(i).unwrap() {
                        rusqlite::types::ValueRef::Null => "null".to_string(),
                        rusqlite::types::ValueRef::Integer(v) => v.to_string(),
                        rusqlite::types::ValueRef::Real(v) => v.to_string(),
                        rusqlite::types::ValueRef::Text(b) | rusqlite::types::ValueRef::Blob(b) => String::from_utf8_lossy(b).into_owned(),
                    })
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        rows.sort();
        out.push_str(&format!("{t}\n{}\n", rows.join("\n")));
    }
    drop(c);
    ah_engine::discard::harmless(std::fs::remove_dir_all(&tmp));
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
            } else if rel.ends_with(".db") {
                out.insert(rel, mask(&dump_db(&p)));
            } else {
                out.insert(rel, mask(&String::from_utf8_lossy(&std::fs::read(&p).unwrap_or_default())));
            }
        }
    }
    let mut m = BTreeMap::new();
    walk(home, home, &mut m);
    m
}

/// The registry's `write_seq` counts upserts; one more registration by the engine before Node's own is one more step of that counter and
/// nothing else, so the late-deferral comparison leaves it out.
fn without_write_seq(tree: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    tree.iter()
        .map(|(k, v)| {
            let mut in_registry = false;
            let lines: Vec<String> = v
                .lines()
                .map(|l| {
                    if !k.ends_with(".db") {
                        return l.to_string();
                    }
                    match l {
                        "registry" => in_registry = true,
                        "sqlite_sequence" => in_registry = false,
                        _ => {}
                    }
                    if in_registry && l.matches('|').count() >= 6 { l.rsplit_once('|').map_or(l.to_string(), |(a, _)| a.to_string()) } else { l.to_string() }
                })
                .collect();
            (k.clone(), lines.join("\n"))
        })
        .collect()
}

fn payload(cwd: &Path, session: &str, transcript: Option<&Path>, codex: bool) -> String {
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

fn sql(home: &Path, stmts: &[(&str, Vec<rusqlite::types::Value>)]) {
    let key = std::fs::read_dir(ds(home).join("store")).unwrap().next().unwrap().unwrap().file_name();
    let c = Connection::open(ds(home).join("store").join(key).join("devswarm.db")).unwrap();
    for (q, args) in stmts {
        c.execute(q, rusqlite::params_from_iter(args.iter())).unwrap();
    }
}

fn t(x: impl ToString) -> rusqlite::types::Value {
    rusqlite::types::Value::Text(x.to_string())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

fn write(p: &Path, t: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, t).unwrap();
}

fn ingest_key(home: &Path) -> String {
    std::fs::read_dir(ds(home).join("store")).unwrap().next().unwrap().unwrap().file_name().to_string_lossy().into_owned()
}

/// The ingest daemon's heartbeat and lock, as the daemon would leave them (`beat` is merged over a live, fresh one).
fn daemon(beat_age_ms: i64, beat_pid: u32, lock_pid: u32, extra: Value) -> impl Fn(&Path) {
    move |h| {
        let key = ingest_key(h);
        let mut b = json!({"ts": now_ms() - beat_age_ms, "pid": beat_pid});
        for (k, v) in extra.as_object().unwrap() {
            b[k] = v.clone();
        }
        write(&ds(h).join(format!("heartbeats/ingest-{key}.json")), &b.to_string());
        write(&ds(h).join(format!("locks/ingest-project-{key}.lock")), &json!({"pid": lock_pid}).to_string());
    }
}

fn inbox(lines: &[&str], cursor: &'static str) -> impl Fn(&Path) {
    let body = format!("{}\n", lines.join("\n"));
    move |h| {
        write(&ds(h).join("inbox/child-1.ndjson"), &body);
        write(&ds(h).join("cursors/child-1.cursor"), cursor);
    }
}

fn plan(steps: Value) -> impl Fn(&Path) {
    move |h| write(&ds(h).join("plans/child-1.json"), &json!({"steps": steps}).to_string())
}

fn descriptor(text: &'static str) -> impl Fn(&Path) {
    move |h| write(&ds(h).join("workspaces/child-1.json"), text)
}

fn message(urgency: &'static str, age_ms: i64) -> impl Fn(&Path) {
    move |h| {
        sql(
            h,
            &[(
                "INSERT INTO messages (workspace_id, ts, hash, body, sender, recipient, mtype, urgency, is_heartbeat, needs_reply, orig_hash, instance_nonce, seq) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,(SELECT COALESCE(MAX(seq),0)+1 FROM messages))",
                vec![
                    t("child-1"),
                    (now_ms() - age_ms).into(),
                    t(format!("h-{urgency}")),
                    t("hello"),
                    t("primary-x"),
                    t("child-1"),
                    t("direct"),
                    t(urgency),
                    0i64.into(),
                    0i64.into(),
                    rusqlite::types::Value::Null,
                    rusqlite::types::Value::Null,
                ],
            )],
        );
    }
}

fn both(a: impl Fn(&Path) + 'static, b: impl Fn(&Path) + 'static) -> impl Fn(&Path) {
    move |h| {
        a(h);
        b(h);
    }
}

/// `count` unread messages in the partition of `id`, which has no registry row: an orphan.
fn orphan(id: &'static str, count: usize) -> impl Fn(&Path) {
    move |h| {
        for i in 0..count {
            sql(
                h,
                &[(
                    "INSERT INTO messages (workspace_id, ts, hash, body, sender, recipient, mtype, urgency, is_heartbeat, needs_reply, orig_hash, instance_nonce, seq) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,(SELECT COALESCE(MAX(seq),0)+1 FROM messages))",
                    vec![
                        t(id),
                        (now_ms() - 5_000 - i as i64).into(),
                        t(format!("h-{id}-{i}")),
                        t("lost"),
                        t("primary-x"),
                        t(id),
                        t("direct"),
                        t("normal"),
                        0i64.into(),
                        0i64.into(),
                        rusqlite::types::Value::Null,
                        rusqlite::types::Value::Null,
                    ],
                )],
            );
        }
    }
}

/// A descriptor file of the orphan `id`, live (`workspaces/`) or the archive marker (`archived/`).
fn orphan_file(dir: &'static str, id: &'static str, body: Value) -> impl Fn(&Path) {
    move |h| write(&ds(h).join(format!("{dir}/{id}.json")), &body.to_string())
}

fn scenes() -> Vec<Scene> {
    use Want::{Defer, DeferLate, Same};
    let archived = |sid: &'static str| {
        move |h: &Path| write(&ds(h).join("archived/child-1.json"), &json!({"id": "child-1", "worktreePath": h.join("proj"), "sessionId": sid}).to_string())
    };
    let with_upstream = |h: &Path| {
        let remote = h.join("remote.git");
        assert!(Command::new("git").args(["init", "-q", "--bare"]).arg(&remote).status().unwrap().success());
        git(&h.join("proj"), &["remote", "add", "origin", remote.to_str().unwrap()]);
        git(&h.join("proj"), &["push", "-q", "-u", "origin", "HEAD"]);
    };
    let mut v = vec![
        scene("steady", Same),
        {
            let mut s = scene("new-session", Same);
            s.session = "other";
            s
        },
        scene("upstream-in-sync", Same).seed(with_upstream),
        scene("upstream-ahead", Same).seed(move |h| {
            with_upstream(h);
            git(&h.join("proj"), &["commit", "-q", "--allow-empty", "-m", "y"]);
            git(&h.join("proj"), &["commit", "-q", "--allow-empty", "-m", "z"]);
        }),
        scene("unread-lines", Same).seed(inbox(&["{\"a\":1}", "{\"b\":2}", "hello"], "0")),
        scene("unread-archive-marker", Same).seed(inbox(&["x", "please [[ANTIHALL_ARCHIVE_REQUEST]] now"], "0")),
        scene("cursor-partial", Same).seed(inbox(&["a", "b", "c"], "2")),
        scene("cursor-json", Same).seed(inbox(&["a", "b", "c"], "{\"line\": 1}")),
        scene("cursor-garbage", Same).seed(inbox(&["a", "b", "c"], "zzz")),
        scene("plan-by-id", Same).seed(plan(json!([{"n": 1, "text": "one", "status": "doing", "ts": 5}, {"n": 2, "text": "two", "status": "todo"}]))),
        scene("plan-all-done", Same).seed(plan(json!([{"n": 1, "text": "one", "status": "done"}]))),
        scene("plan-required", Same).seed(|h| write(&h.join(".anti-hall/settings.json"), r#"{"devswarm":{"planRequired":true}}"#)),
        {
            let mut s = scene("no-builder-id", Same).env(vec![("DEVSWARM_BUILDER_ID", "")]);
            s.warm_env = Some(vec![]);
            s
        },
        {
            let mut s = scene("unsafe-builder-id", Same).env(vec![("DEVSWARM_BUILDER_ID", "a b/c")]);
            s.warm_env = Some(vec![]);
            s
        },
        {
            let mut s = scene("codex-host-payload", Same);
            s.codex = true;
            s
        },
        {
            let mut s = scene("codex-host-unread-and-plan", Same)
                .seed(both(inbox(&["a", "b"], "0"), plan(json!([{"n": 1, "text": "one", "status": "blocked", "ts": 5}]))));
            s.codex = true;
            s
        },
        scene("builder-name", Same).env(vec![("DEVSWARM_BUILDER_NAME", "Fix the thing")]),
        {
            let mut s = scene("repeat-same-session", Same);
            s.repeat = 2;
            s
        },
        {
            let mut s = scene("repeat-with-transcript", Same);
            s.repeat = 3;
            s.transcript = true;
            s
        },
        {
            let mut s = scene("subdir-cwd", Same);
            s.sub = Some("sub/deep");
            s
        },
        scene("mesh-direct", Same).seed(message("normal", 120_000)),
        scene("mesh-direct-urgent", Same).seed(message("urgent", 300_000)),
        scene("descriptor-corrupt", Same).seed(descriptor("{not json")),
        scene("descriptor-extra-keys", Same)
            .seed(descriptor(r#"{"id":"child-1","extra":{"a":[1,2,3]},"inboxPath":null,"nudgeCommand":["x"],"sessionId":"old"}"#)),
        scene("descriptor-custom-paths", Same).seed(|h| {
            let dir = h.join("custom");
            std::fs::create_dir_all(&dir).unwrap();
            let text = json!({"id": "child-1", "worktreePath": "/old", "sessionId": "old", "inboxPath": dir.join("in.ndjson"), "cursorPath": dir.join("cur")});
            write(&ds(h).join("workspaces/child-1.json"), &text.to_string());
        }),
        scene("archived-marker-superseded", Same).seed(archived("older-session")),
        scene("archived-marker-setting-off", Same)
            .seed(both(archived("s1"), |h| write(&h.join(".anti-hall/settings.json"), r#"{"devswarm":{"archivedChildStop":false}}"#))),
        // orphan partitions in the summary the registration refreshes: sorted natively, or handed to Node whole
        scene("orphan-no-descriptor", Same).seed(orphan("orph-1", 2)),
        scene("orphan-live-descriptor-not-archived", Same).seed(both(
            orphan("orph-1", 1),
            orphan_file("workspaces", "orph-1", json!({"id": "orph-1", "worktreePath": "/nonexistent/wt-a", "sessionId": "x"})),
        )),
        scene("orphan-archived-no-family", Same)
            .seed(both(orphan("orph-1", 3), orphan_file("archived", "orph-1", json!({"id": "orph-1", "worktreePath": "/nonexistent/wt-b", "sessionId": "x"})))),
        scene("orphan-archived-no-worktree-field", Same)
            .seed(both(orphan("orph-1", 1), orphan_file("archived", "orph-1", json!({"id": "orph-1", "sessionId": "x"})))),
        scene("orphan-archived-no-session", Same)
            .seed(both(orphan("orph-1", 1), orphan_file("archived", "orph-1", json!({"id": "orph-1", "worktreePath": "/nonexistent/wt-c"})))),
        scene("orphan-archived-existing-dir-no-family", Same).seed(both(orphan("orph-1", 2), |h| {
            let dir = h.join("elsewhere");
            std::fs::create_dir_all(&dir).unwrap();
            write(&ds(h).join("archived/orph-1.json"), &json!({"id": "orph-1", "worktreePath": dir, "sessionId": "x"}).to_string());
        })),
        scene("orphan-marker-unreadable-live-descriptor", Same).seed(both(
            both(orphan("orph-1", 1), orphan_file("workspaces", "orph-1", json!({"id": "orph-1", "worktreePath": "/nonexistent/wt-d", "sessionId": "x"}))),
            |h| write(&ds(h).join("archived/orph-1.json"), "{not json"),
        )),
        scene("orphan-marker-of-another-worktree", Same).seed(both(
            both(orphan("orph-1", 1), orphan_file("workspaces", "orph-1", json!({"id": "orph-1", "worktreePath": "/nonexistent/wt-e", "sessionId": "x"}))),
            orphan_file("archived", "orph-1", json!({"id": "orph-1", "worktreePath": "/nonexistent/wt-other", "sessionId": "x"})),
        )),
        scene("orphan-worktree-group-sibling", Same).seed(both(
            both(orphan("orph-1", 2), orphan_file("workspaces", "orph-1", json!({"id": "orph-1", "worktreePath": "/nonexistent/wt-f", "sessionId": "x"}))),
            orphan_file("archived", "other-1", json!({"id": "other-1", "worktreePath": "/nonexistent/wt-f", "sessionId": "x"})),
        )),
        scene("orphan-held", Same)
            .seed(both(orphan("orph-1", 1), |h| write(&h.join(".anti-hall/settings.json"), r#"{"devswarm":{"heldPartitions":"orph-1, other"}}"#))),
        scene("orphan-held-and-stranded", Same).seed(both(
            both(orphan("orph-1", 1), orphan_file("archived", "orph-1", json!({"id": "orph-1", "worktreePath": "/nonexistent/wt-g", "sessionId": "x"}))),
            |h| write(&h.join(".anti-hall/settings.json"), r#"{"devswarm":{"heldPartitions":"orph-1"}}"#),
        )),
        scene("orphan-drained-is-not-listed", Same).seed(both(orphan("orph-1", 2), |h| {
            sql(
                h,
                &[(
                    "INSERT INTO reader_cursors (partition, ns, reader, value, retired_line, updated_at) VALUES (?,?,?,?,NULL,?)",
                    vec![t("orph-1"), t("store"), t("floor"), 2i64.into(), 1i64.into()],
                )],
            );
        })),
        scene("orphans-mixed", Same).seed(both(
            both(orphan("orph-1", 1), orphan("orph-2", 4)),
            both(
                orphan_file("archived", "orph-2", json!({"id": "orph-2", "worktreePath": "/nonexistent/wt-h", "sessionId": "x"})),
                orphan_file("workspaces", "orph-3", json!({"id": "orph-3", "worktreePath": "/nonexistent/wt-i"})),
            ),
        )),
        scene("orphan-archived-family-has-a-survivor", Defer).seed(both(orphan("orph-1", 1), |h| {
            write(&ds(h).join("archived/orph-1.json"), &json!({"id": "orph-1", "worktreePath": h.join("proj"), "sessionId": "x"}).to_string())
        })),
        scene("orphan-sessions-differ-needs-liveness", Defer).seed(both(
            both(orphan("orph-1", 1), orphan_file("workspaces", "orph-1", json!({"id": "orph-1", "worktreePath": "/nonexistent/wt-j", "sessionId": "live-1"}))),
            orphan_file("archived", "orph-1", json!({"id": "orph-1", "worktreePath": "/nonexistent/wt-j", "sessionId": "old-1"})),
        )),
        scene("orphan-descriptor-session-not-text", Defer)
            .seed(both(orphan("orph-1", 1), orphan_file("archived", "orph-1", json!({"id": "orph-1", "worktreePath": "/nonexistent/wt-k", "sessionId": 7})))),
        // the cases that stay with Node, each decided before anything is written
        scene("archived-marker", Defer).seed(archived("s1")),
        scene("descriptor-array", Defer).seed(descriptor("[1,2]")),
        scene("inbox-relative-path", Defer).seed(descriptor(r#"{"id":"child-1","inboxPath":"in.ndjson","cursorPath":"in.cursor"}"#)),
        scene("inbox-path-not-a-string", Defer).seed(descriptor(r#"{"id":"child-1","inboxPath":5}"#)),
        scene("duplicate-row", Defer).seed(|h| {
            sql(
                h,
                &[(
                    "INSERT INTO registry (id, worktree_path, session_id, inbox_path, cursor_path, nudge_command, updated_at) VALUES (?,?,?,?,?,?,?)",
                    vec![t("sib-1"), t(h.join("proj").display()), t("x"), t("/nonexistent/i"), t("/nonexistent/c"), rusqlite::types::Value::Null, 1i64.into()],
                )],
            );
        }),
        scene("declared-reader", DeferLate).seed(both(message("normal", 120_000), |h| {
            sql(
                h,
                &[(
                    "INSERT INTO reader_cursors (partition, ns, reader, value, retired_line, updated_at) VALUES (?,?,?,?,NULL,?)",
                    vec![t("child-1"), t("store"), t("h:1:2"), 0i64.into(), 1i64.into()],
                )],
            );
        })),
        scene("phantom-descriptor", Defer).seed(|h| {
            let id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeee";
            write(&ds(h).join(format!("workspaces/{id}.json")), &json!({"id": id, "worktreePath": h.join("proj"), "sessionId": "x"}).to_string());
        }),
        {
            let mut s = scene("ingest-never-ran", Same);
            s.healthy = false;
            s
        },
        {
            let mut s = scene("ingest-never-ran-repeat", Same);
            s.healthy = false;
            s.repeat = 2;
            s.transcript = true;
            s
        },
        {
            let mut s = scene("first-turn-creates-the-store", Same);
            s.warm = false;
            s.healthy = false;
            s
        },
        scene("ingest-stale-hour", Same).seed(daemon(3_600_000, 999_998, 999_999, json!({}))),
        scene("ingest-stale-minutes", Same).seed(daemon(600_000, 999_998, 999_999, json!({}))),
        scene("ingest-stale-days", Same).seed(daemon(3 * 86_400_000, 999_998, 999_999, json!({}))),
        scene("ingest-handover-in-progress", Same).seed(daemon(240_000, 999_998, std::process::id(), json!({}))),
        scene("ingest-fault-config", Same).seed(daemon(
            1_000,
            std::process::id(),
            std::process::id(),
            json!({"consecutiveMonitorFailures": 5, "lastMonitorErrorCode": "ENOENT", "lastMonitorError": "spawn hivecontrol ENOENT"}),
        )),
        scene("ingest-fault-slow", Same).seed(daemon(1_000, std::process::id(), std::process::id(), json!({"consecutiveMonitorFailures": 3}))),
        scene("ingest-fault-ok-stale", Same).seed(daemon(
            1_000,
            std::process::id(),
            std::process::id(),
            json!({"consecutiveMonitorFailures": 0, "lastMonitorOkMs": now_ms() - 1_200_000}),
        )),
        scene("ingest-fault-no-ok-since-start", Same).seed(daemon(
            1_000,
            std::process::id(),
            std::process::id(),
            json!({"consecutiveMonitorFailures": 1, "startedAtMs": now_ms() - 1_800_000}),
        )),
        scene("ingest-monitor-starting-up", Same).seed(daemon(
            1_000,
            std::process::id(),
            std::process::id(),
            json!({"consecutiveMonitorFailures": 1, "startedAtMs": now_ms() - 60_000}),
        )),
    ];
    v.shrink_to_fit();
    v
}

fn run_scene(s: &Scene) -> Result<bool, String> {
    let root = std::env::var_os("AH_ENGINE_IT_ROOT").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let base = root.join(format!("ah-ctp-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
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
    if s.warm {
        let warm_env = s.warm_env.clone().unwrap_or_else(|| s.env.clone());
        node(&home, &payload(&cwd, "warm", None, s.codex), &warm_env);
    }
    if s.healthy && s.warm {
        let key = std::fs::read_dir(ds(&home).join("store")).unwrap().next().unwrap().unwrap().file_name().to_string_lossy().into_owned();
        write(&ds(&home).join(format!("heartbeats/ingest-{key}.json")), &json!({"ts": now_ms(), "pid": std::process::id()}).to_string());
        write(&ds(&home).join(format!("locks/ingest-project-{key}.lock")), &json!({"pid": std::process::id()}).to_string());
    }
    (s.seed)(&home);
    let transcript = s.transcript.then(|| {
        let t = home.join(".claude/projects/-p/t.jsonl");
        write(&t, "");
        t
    });
    let pl = payload(&cwd, s.session, transcript.as_deref(), s.codex);
    let saved = base.join("saved");
    copy_dir(&home, &saved);
    let mut n = (0, String::new(), String::new());
    for _ in 0..s.repeat {
        n = node(&home, &pl, &s.env);
    }
    let node_tree = snapshot(&home);
    std::fs::remove_dir_all(&home).unwrap();
    copy_dir(&saved, &home);
    let mut e = (0, String::new(), String::new());
    for _ in 0..s.repeat {
        e = engine(&home, &pl, &s.env);
    }
    let eng_tree = snapshot(&home);
    let deferred = e.1.trim() == "AHFALLBACK";
    let mut again = None;
    if s.want == Want::DeferLate && deferred {
        // Node takes over on top of what the engine already did
        let mut n2 = (0, String::new(), String::new());
        for _ in 0..1 {
            n2 = node(&home, &pl, &s.env);
        }
        again = Some((n2, snapshot(&home)));
    }
    ah_engine::discard::harmless(std::fs::remove_dir_all(&base));
    match (s.want, deferred) {
        (Want::DeferLate, true) => {
            let (n2, tree2) = again.unwrap();
            let (tree2, node_tree) = (without_write_seq(&tree2), without_write_seq(&node_tree));
            if n2.1 != n.1 || tree2 != node_tree {
                let diff: Vec<String> = node_tree
                    .keys()
                    .chain(tree2.keys())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .filter(|k| node_tree.get(*k) != tree2.get(*k))
                    .map(|k| format!("  {k}\n   node alone: {:?}\n   node after: {:?}", node_tree.get(k), tree2.get(k)))
                    .collect();
                return Err(format!("{}: Node on the engine's files differs from Node alone (stdout equal: {})\n{}", s.name, n2.1 == n.1, diff.join("\n")));
            }
            Ok(false)
        }
        // the old engine handed these scenes to the Node hook; it now answers them itself: the answer may say less than Node's
        // (advisory text is dropped on purpose) or more, but where Node blocks the engine must block
        (Want::DeferLate | Want::Defer, false) => {
            crate::common::not_weaker_than_node((n.0, &n.1), (e.0, &e.1)).map_err(|w| format!("{}: {w}", s.name))?;
            Ok(false)
        }
        (Want::Defer, true) => {
            // nothing written that Node would repeat: the registration, heartbeat and dedupe state are the seeded ones
            let beat = eng_tree.get(".anti-hall/devswarm/heartbeats/child-1.json").cloned().unwrap_or_default();
            if s.warm && !beat.contains("\"sessionId\":\"warm\"") {
                return Err(format!("{}: deferred after writing the heartbeat: {beat}", s.name));
            }
            Ok(false)
        }
        (Want::Same, true) => Err(format!("{}: expected a native answer, the engine deferred", s.name)),
        (Want::Same, false) => {
            if (n.0, &n.1, &n.2) != (e.0, &e.1, &e.2) {
                return Err(format!("{}: output differs\n node: {n:?}\n eng : {e:?}", s.name));
            }
            if node_tree != eng_tree {
                let diff: Vec<String> = node_tree
                    .keys()
                    .chain(eng_tree.keys())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .filter(|k| node_tree.get(*k) != eng_tree.get(*k))
                    .map(|k| format!("  {k}\n   node: {:?}\n   eng : {:?}", node_tree.get(k), eng_tree.get(k)))
                    .collect();
                return Err(format!("{}: files differ\n{}", s.name, diff.join("\n")));
            }
            Ok(true)
        }
    }
}

#[test]
fn engine_and_node_agree_for_a_child_in_a_checkout() {
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
    assert!(native >= 30 && deferred >= 5, "corpus too small: native {native}, deferred {deferred}");
}
