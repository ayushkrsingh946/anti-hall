//! Roadmap #61 chunk C1: the post-update store stages the engine answers itself (`reader-cursors-import`, `reader-floor-repair`,
//! `dual-partition-acks`, `promote-unclaimed`), pinned against the Node functions of `update.js`.
//!
//! Each scenario seeds a scratch home with the BAD state an earlier version left (a partition never imported, floors pinned by
//! readers whose process is gone, a twin partition that never saw the other's ack, a row stuck on `unclaimed:`), then compares
//! the engine's answer, the store afterwards and the files under `.anti-hall` with what the Node function produced on the same
//! seed. The Node answer is FROZEN in `tests/fixtures/update_post/<stage>/<scenario>.json` (record it with
//! `AH_RECORD_UPDATE_FIXTURES=1`, which runs the Node function on a seeded world; the replay never runs Node: its `node` is a
//! shim that records the call and fails). Every stage is also run a second time (a no-op) and switched off with `enabled = false`.
use super::update_parity::{Opt, World, envs, finish, node_calls, norm, run_engine, tree, world};
use ah_engine::meshw::store::{CursorPut, MeshRow, MeshStore, RegistryRow};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);
const VERSION: &str = "0.1.0";
const STORE: &str = "proj-a1b2c3";
const FAR_FUTURE: i64 = 4_000_000_000_000;

fn recording() -> bool {
    std::env::var_os("AH_RECORD_UPDATE_FIXTURES").is_some()
}

fn fixture_path(stage: &str, name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/update_post").join(stage).join(format!("{name}.json"))
}

// ---- seeding -------------------------------------------------------------------------------------------------------------

fn ds(home: &Path) -> PathBuf {
    home.join(".anti-hall/devswarm")
}

fn open_store(home: &Path, hash: &str) -> MeshStore {
    let dir = ds(home).join("store").join(hash);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("BACKEND"), "sqlite\n").unwrap();
    MeshStore::open(&dir.join("devswarm.db")).unwrap()
}

fn msg(st: &MeshStore, ws: &str, n: i64, hash: &str, orig: Option<&str>) {
    st.append_mesh_row(&MeshRow {
        workspace_id: ws.into(),
        ts: 1_790_000_000_000 + n,
        hash: Some(hash.into()),
        body: format!("m{n}"),
        sender: Some("sender".into()),
        recipient: Some(ws.into()),
        mtype: Some("direct".into()),
        urgency: Some("normal".into()),
        orig_hash: orig.map(str::to_string),
        ..MeshRow::default()
    })
    .unwrap();
}

fn cur(st: &MeshStore, p: &str, ns: &str, reader: &str, value: i64) {
    st.reader_cursor_txn(&[CursorPut { partition: p.into(), ns: ns.into(), reader: reader.into(), value, retired_line: None, updated_at: 1000 }]).unwrap();
}

fn registry(st: &MeshStore, id: &str, wt: Option<&str>, session: Option<&str>) {
    st.upsert_registry(
        &RegistryRow { id: id.into(), worktree_path: wt.map(str::to_string), session_id: session.map(str::to_string), ..RegistryRow::default() },
        1000,
        |a, b| a == b,
    )
    .unwrap();
}

fn put(home: &Path, rel: &str, text: &str) {
    let p = home.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// This process as a live harness session (it started long before `FAR_FUTURE`, so its recorded start is not a pid reuse).
fn live_session(home: &Path) -> (String, String) {
    let pid = std::process::id();
    put(home, &format!(".claude/sessions/{pid}.json"), &format!(r#"{{"pid":{pid},"startedAt":{FAR_FUTURE},"cwd":"/nonexistent"}}"#));
    (format!("h:{pid}:{FAR_FUTURE}"), ah_engine::meshw::cursors::short_nonce(&format!("anc:{pid}:{FAR_FUTURE}")))
}

const GONE: &str = "h:999999:1000000000001";

/// A partition never imported by the earlier build, a legacy position per file, a live mapped harness, an ended reader on a
/// partition that was imported; a second, empty store.
fn seed_import(home: &Path) {
    let st = open_store(home, STORE);
    for (i, h) in ["a1", "a2", "a3"].iter().enumerate() {
        msg(&st, "w1", i as i64, h, None);
    }
    for (i, h) in ["b1", "b2"].iter().enumerate() {
        msg(&st, "w2", i as i64, h, None);
    }
    cur(&st, "w2", "store", "#floor", 1);
    cur(&st, "w2", "nd", "#floor", 0);
    cur(&st, "w2", "store", GONE, 0);
    cur(&st, "w2", "nd", GONE, 0);
    drop(st);
    registry(&open_store(home, "proj-d4e5f6"), "w9", None, None);
    let (_, short) = live_session(home);
    put(home, ".anti-hall/devswarm/cursors/w1.json", "2");
    put(home, &format!(".anti-hall/devswarm/cursors/w1#inst-{short}.json"), "1");
    put(home, &format!(".anti-hall/devswarm/cursors/w1#nd-{short}.json"), "0");
    put(home, ".anti-hall/devswarm/workspaces/w1.json", &format!(r#"{{"id":"w1","cursorPath":"{}/nd-w1.cursor"}}"#, home.display()));
    put(home, "nd-w1.cursor", "3");
}

/// A partition never imported and a live harness session whose `cwd` is empty and which has no legacy file: Node's
/// `cwdInPartition` is `false` for an empty cwd, so no reader row is declared (only the floors).
fn seed_import_no_cwd(home: &Path) {
    let st = open_store(home, STORE);
    for (i, h) in ["a1", "a2", "a3"].iter().enumerate() {
        msg(&st, "w1", i as i64, h, None);
    }
    drop(st);
    let pid = std::process::id();
    put(home, &format!(".claude/sessions/{pid}.json"), &format!(r#"{{"pid":{pid},"startedAt":{FAR_FUTURE},"cwd":""}}"#));
    put(home, ".anti-hall/devswarm/cursors/w1.json", "2");
}

/// Two partitions never imported; the second (`primary-...`, so it names a mesh id) meets a live session whose relative `cwd`
/// only Node can classify. The first alone would import cleanly.
fn seed_import_deferred(home: &Path) {
    let st = open_store(home, STORE);
    for p in ["a1", "primary-0123abcd"] {
        for (i, h) in ["a1", "a2"].iter().enumerate() {
            msg(&st, p, i as i64, &format!("{p}{h}"), None);
        }
    }
    drop(st);
    let pid = std::process::id();
    put(home, &format!(".claude/sessions/{pid}.json"), &format!(r#"{{"pid":{pid},"startedAt":{FAR_FUTURE},"cwd":"relative/dir"}}"#));
}

/// Two imported partitions; `a1` has a reader whose process is gone (it would be retired), `primary-0123abcd` a live reader whose
/// session names a relative `cwd` that only Node can classify.
fn seed_floors_deferred(home: &Path) {
    let st = open_store(home, STORE);
    let pid = std::process::id();
    for p in ["a1", "primary-0123abcd"] {
        for i in 0..3 {
            msg(&st, p, i, &format!("{p}{i}"), None);
        }
        for ns in ["store", "nd"] {
            cur(&st, p, ns, "#floor", 0);
        }
    }
    for ns in ["store", "nd"] {
        cur(&st, "a1", ns, GONE, 0);
        cur(&st, "primary-0123abcd", ns, &format!("h:{pid}:{FAR_FUTURE}"), 0);
    }
    put(home, &format!(".claude/sessions/{pid}.json"), &format!(r#"{{"pid":{pid},"startedAt":{FAR_FUTURE},"cwd":"relative/dir"}}"#));
}

/// `seed_dual`, plus a registry row whose worktree path is relative (only git can place it).
fn seed_dual_deferred(home: &Path) {
    seed_dual(home);
    registry(&open_store(home, STORE), "rel-1", Some("relative/wt"), None);
}

/// The anchor was never imported and the twin read all three of its rows (so the anchor would be raised through the import),
/// while a live session names a relative `cwd` that only Node can classify.
fn seed_dual_import_deferred(home: &Path) {
    let st = open_store(home, STORE);
    let (a, wt) = (anchor(), repo().to_string_lossy().into_owned());
    registry(&st, &a, Some(&wt), None);
    registry(&st, "kid-1", Some(&wt), None);
    for (i, h) in ["x1", "x2", "x3"].iter().enumerate() {
        msg(&st, &a, i as i64, h, None);
    }
    for (i, (h, o)) in [("y1", Some("x1")), ("y2", Some("x2")), ("y3", Some("x3"))].iter().enumerate() {
        msg(&st, "kid-1", 10 + i as i64, h, *o);
    }
    for ns in ["store", "nd"] {
        cur(&st, "kid-1", ns, "#floor", if ns == "store" { 3 } else { 0 });
    }
    drop(st);
    let pid = std::process::id();
    put(home, &format!(".claude/sessions/{pid}.json"), &format!(r#"{{"pid":{pid},"startedAt":{FAR_FUTURE},"cwd":"relative/dir"}}"#));
}

/// Floors the v0.106.0 import pinned: readers whose process is gone, one whose session file names another session, one live
/// but not local, one local that really read; an nd floor imported as 0 on a partition with a legacy nd cursor.
fn seed_floors(home: &Path) {
    let st = open_store(home, STORE);
    for i in 0..6 {
        msg(&st, "p1", i, &format!("c{i}"), None);
    }
    msg(&st, "p2", 0, "d0", None);
    for ns in ["store", "nd"] {
        for p in ["p1", "p2"] {
            cur(&st, p, ns, "#floor", 0);
        }
    }
    let (live, _) = live_session(home);
    let pid = std::process::id();
    for ns in ["store", "nd"] {
        cur(&st, "p1", ns, GONE, 0);
        cur(&st, "p1", ns, &format!("h:{pid}:5000000000000"), 2);
    }
    cur(&st, "p1", "store", &live, 5);
    cur(&st, "p1", "nd", &live, 0);
    put(home, ".anti-hall/devswarm/workspaces/p2.json", &format!(r#"{{"id":"p2","cursorPath":"{}/nd-p2.cursor"}}"#, home.display()));
    put(home, "nd-p2.cursor", "4");
}


/// Rows an older build stamped `unclaimed:`: one with a real session in its heartbeat (descriptor and registry both promoted),
/// one without a heartbeat, one whose heartbeat is itself a marker, one whose registry already holds a real value, one with no
/// registry row at all.
fn seed_promote(home: &Path) {
    let rows: [(&str, Option<&str>, Option<&str>); 5] = [
        ("u1", Some("sess-real-1"), Some("unclaimed:u1")),
        ("u2", None, Some("unclaimed:u2")),
        ("u3", Some("unclaimed:u3"), Some("unclaimed:u3")),
        ("u4", Some("sess-4"), Some("sess-reg-4")),
        ("u5", Some("sess-5"), None),
    ];
    for (id, beat, registered) in rows {
        let wt = format!("/nonexistent/{id}");
        put(
            home,
            &format!(".anti-hall/devswarm/workspaces/{id}.json"),
            &format!(r#"{{"id":"{id}","worktreePath":"{wt}","sessionId":"unclaimed:{id}","inboxPath":null,"cursorPath":null,"nudgeCommand":null}}"#),
        );
        if let Some(b) = beat {
            put(home, &format!(".anti-hall/devswarm/heartbeats/{id}.json"), &format!(r#"{{"id":"{id}","sessionId":" {b} "}}"#));
        }
        let st = open_store(home, &ah_engine::meshw::send::hash_from_workspace_id(id));
        if let Some(r) = registered {
            registry(&st, id, Some(&wt), Some(r));
        }
    }
}

static REPO: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// One git checkout shared by every world of this process: the mesh id of a worktree is a hash of its real path.
fn repo() -> &'static Path {
    REPO.get_or_init(|| {
        let p = fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("ah-c1-repo-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&p).unwrap();
        let ok = Command::new("git").args(["init", "-q"]).current_dir(&p).env("GIT_CONFIG_GLOBAL", "/dev/null").status().unwrap().success();
        assert!(ok, "git init");
        p
    })
}

fn anchor() -> String {
    ah_engine::meshw::ident::canonical_mesh_id(&repo().to_string_lossy()).unwrap().unwrap()
}

/// An anchor and a partner row on one worktree; the partner holds forwarded copies of what the anchor's reader already read.
fn seed_dual(home: &Path) {
    let st = open_store(home, STORE);
    let (a, wt) = (anchor(), repo().to_string_lossy().into_owned());
    registry(&st, &a, Some(&wt), None);
    registry(&st, "kid-1", Some(&wt), None);
    for (i, h) in ["x1", "x2", "x3"].iter().enumerate() {
        msg(&st, &a, i as i64, h, None);
    }
    for (i, (h, o)) in [("y1", Some("x1")), ("y2", Some("x2")), ("y3", Some("x3")), ("y4", None)].iter().enumerate() {
        msg(&st, "kid-1", 10 + i as i64, h, *o);
    }
    for ns in ["store", "nd"] {
        cur(&st, &a, ns, "#floor", if ns == "store" { 2 } else { 0 });
        cur(&st, "kid-1", ns, "#floor", 0);
    }
}

// ---- the picture of a world ----------------------------------------------------------------------------------------------

fn near_now(v: i64) -> bool {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    (now - v).abs() < 600_000
}

fn rows(db: &Path, table: &str, cols: &str, order: &str) -> Value {
    let c = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let Ok(mut st) = c.prepare(&format!("SELECT {cols} FROM {table} ORDER BY {order}")) else { return Value::Null };
    let n = st.column_count();
    let names: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
    let mut out = Vec::new();
    let mut q = st.query([]).unwrap();
    while let Some(r) = q.next().unwrap() {
        let mut o = serde_json::Map::new();
        for i in 0..n {
            let v = match r.get_ref(i).unwrap() {
                rusqlite::types::ValueRef::Null => Value::Null,
                rusqlite::types::ValueRef::Integer(x) if names[i].ends_with("updated_at") && near_now(x) => json!("<now>"),
                rusqlite::types::ValueRef::Integer(x) => json!(x),
                rusqlite::types::ValueRef::Real(x) => json!(x),
                rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                rusqlite::types::ValueRef::Blob(_) => json!("<blob>"),
            };
            o.insert(names[i].clone(), v);
        }
        out.push(Value::Object(o));
    }
    Value::Array(out)
}

/// The stores (reader cursors, the legacy cursor rows, the registry) and the files under `.anti-hall` of a world.
fn picture(w: &World, answer: Value, anchor: Option<&str>) -> Value {
    let mut stores = BTreeMap::new();
    let dir = ds(&w.home).join("store");
    for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
        let db = e.path().join("devswarm.db");
        if !db.is_file() {
            continue;
        }
        stores.insert(
            e.file_name().to_string_lossy().into_owned(),
            json!({
                "reader_cursors": rows(&db, "reader_cursors", "partition, ns, reader, value, retired_line, updated_at", "partition, ns, reader"),
                "cursors": rows(&db, "cursors", "workspace_id, value", "workspace_id"),
                "registry": rows(&db, "registry", "id, worktree_path, session_id", "id"),
                "messages": rows(&db, "messages", "workspace_id, hash", "id"),
            }),
        );
    }
    let pid = regex::Regex::new(r#""pid":\d+"#).unwrap();
    let ts = regex::Regex::new(r#"^\{"ts":"[^"]*""#).unwrap();
    let generated = regex::Regex::new(r#""generatedAt":\d+"#).unwrap();
    let files: BTreeMap<String, String> = tree(&w.home)
        .into_iter()
        .filter(|(k, _)| {
            k.starts_with(".anti-hall/") && !k.contains("/store/") && !k.contains("/locks/") && !k.starts_with(".anti-hall/state") && (!k.starts_with(".anti-hall/logs") || k == ".anti-hall/logs/" || k == ".anti-hall/logs/devswarm.jsonl")
        })
        .map(|(k, v)| {
            let v = pid.replace_all(&norm(&v, w), r#""pid":0"#).into_owned();
            let v = generated.replace_all(&v, r#""generatedAt":0"#).into_owned();
            (k, v.lines().map(|l| ts.replace(l, r#"{"ts":"T""#).into_owned()).collect::<Vec<_>>().join("\n"))
        })
        .collect();
    let mut text = json!({"answer": answer, "stores": stores, "files": files}).to_string();
    if let Some(a) = anchor {
        text = text.replace(a, "{ANCHOR}");
    }
    // the harness this test pretends to be is the test process itself: its pid and the tag derived from it are not part of the picture
    let pid = std::process::id();
    let far = ah_engine::meshw::cursors::short_nonce(&format!("anc:{pid}:{FAR_FUTURE}"));
    text = text.replace(&format!("h:{pid}:"), "h:{PID}:").replace(&far, "{SHORT}");
    if let Some(r) = REPO.get() {
        text = text.replace(r.to_string_lossy().as_ref(), "{REPO}");
    }
    let mut v: Value = serde_json::from_str(&norm(&text, w)).unwrap();
    for st in v["stores"].as_object_mut().unwrap().values_mut() {
        if let Some(rows) = st["reader_cursors"].as_array_mut() {
            rows.sort_by_key(|r| (r["partition"].to_string(), r["ns"].to_string(), r["reader"].to_string()));
        }
    }
    v
}

// ---- running a stage -----------------------------------------------------------------------------------------------------

/// Node's stage function on the world, as `update.js` calls it.
fn node_answer(w: &World, o: &Opt, func: &str) -> Value {
    let snippet = r#"const u=require(process.argv[1]+"/skills/update/scripts/update.js");const o=JSON.parse(process.argv[3]);o.paths=u.resolvePaths(process.env,require("os").homedir());console.log(JSON.stringify(u[process.argv[2]](o)))"#;
    let mut cmd = Command::new("node");
    cmd.arg("-e").arg(snippet).arg(w.mp.join("plugins/anti-hall")).arg(func).arg(format!(r#"{{"version":"{VERSION}"}}"#)).current_dir(&w.home);
    envs(&mut cmd, w, o);
    let out = finish(cmd.output().unwrap());
    assert_eq!(out.code, 0, "node stage failed: {}", out.stderr);
    serde_json::from_str(out.stdout.lines().rev().find(|l| !l.trim().is_empty()).unwrap()).unwrap()
}

fn engine_answer(w: &World, o: &Opt, key: &str) -> Value {
    let out = run_engine(w, o, &["--post-pull-only"]);
    assert_eq!(out.code, 0, "engine failed: {}", out.stderr);
    let status: Value = serde_json::from_str(out.stdout.lines().next().unwrap()).unwrap();
    let status = &status["status"];
    status.get(key).unwrap_or_else(|| panic!("no {key} in {status}")).clone()
}

struct Case {
    stage: &'static str,
    name: &'static str,
    /// The stage row's name in the post-pull table.
    row: &'static str,
    /// Its status key.
    key: &'static str,
    /// The Node stage function.
    func: &'static str,
    seed: fn(&Path),
    live: bool,
    markers: bool,
}

impl Case {
    fn opt(&self) -> Opt {
        Opt { ds: true, no_upstream_move: true, live: self.live, ..Opt::default() }
    }

    fn mark(&self, home: &Path) {
        if self.markers {
            put(home, ".anti-hall/update-sweep-state.json", &format!(r#"{{"{}":{{"completedVersion":"{VERSION}"}}}}"#, self.state_key()));
        }
    }

    fn state_key(&self) -> &'static str {
        match self.stage {
            "reader-cursors-import" => "readerCursorsImport",
            "reader-floor-repair" => "repairReaderFloors",
            "dual-partition-acks" => "reconcileDualPartitionAcks",
            _ => "none",
        }
    }
}

/// What the replay's `node` shim logged, besides the ingest probe every DevSwarm run makes (it is not a stage).
fn stage_node_calls(w: &World) -> Vec<String> {
    node_calls(w).lines().filter(|l| !l.contains("ingestUnitNeedsHeal")).map(str::to_string).collect()
}

/// Record (when asked) and replay one scenario; returns the replay world's picture and the world, for follow-up checks.
fn replay(c: &Case) -> (Value, World, Opt) {
    let o = c.opt();
    let anchor = anchor();
    if recording() {
        let a = world(&o);
        (c.seed)(&a.home);
        c.mark(&a.home);
        let ans = node_answer(&a, &o, c.func);
        let pic = picture(&a, ans, Some(&anchor));
        let p = fixture_path(c.stage, c.name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, format!("{}\n", serde_json::to_string_pretty(&pic).unwrap())).unwrap();
    }
    let want: Value = serde_json::from_str(&fs::read_to_string(fixture_path(c.stage, c.name)).unwrap_or_else(|e| panic!("{}/{}: no fixture ({e}); record with AH_RECORD_UPDATE_FIXTURES=1", c.stage, c.name))).unwrap();
    let eo = Opt { no_node: true, stage_only: Some(c.row), ..o };
    let w = world(&eo);
    (c.seed)(&w.home);
    c.mark(&w.home);
    let ans = engine_answer(&w, &eo, c.key);
    assert_eq!(stage_node_calls(&w), Vec::<String>::new(), "{}/{}: the engine ran Node for the stage", c.stage, c.name);
    let got = picture(&w, ans, Some(&anchor));
    for k in ["answer", "stores", "files"] {
        assert!(want[k] == got[k], "{}/{}: {k} differs from the frozen Node result\n{}", c.stage, c.name, diff(&want[k], &got[k]));
    }
    (got, w, eo)
}

/// The members that differ between two JSON objects (or the two values), for a failure message.
fn diff(want: &Value, got: &Value) -> String {
    match (want.as_object(), got.as_object()) {
        (Some(w), Some(g)) => w
            .keys()
            .chain(g.keys())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter(|k| w.get(*k) != g.get(*k))
            .map(|k| format!("  {k}\n    frozen: {}\n    engine: {}", w.get(k).map_or("<absent>".into(), Value::to_string), g.get(k).map_or("<absent>".into(), Value::to_string)))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => format!("  frozen: {want}\n  engine: {got}"),
    }
}

/// A second run changes nothing: the one-time marker is stamped by the first run (or the stage is idempotent without one).
fn second_run_is_a_no_op(c: &Case, first: &Value, w: &World, o: &Opt) {
    let again = engine_answer(w, o, c.key);
    let pic = picture(w, again.clone(), Some(&anchor()));
    assert_eq!(first["stores"], pic["stores"], "{}/{}: a second run changed a store", c.stage, c.name);
    let strip = |v: &Value| {
        let mut f = v["files"].as_object().unwrap().clone();
        f.retain(|k, _| !k.contains("cursor-log") && !k.contains("update-sweep-state"));
        f
    };
    assert_eq!(strip(first), strip(&pic), "{}/{}: a second run changed a file", c.stage, c.name);
    if c.live && first["answer"]["errors"] == json!(0) && c.state_key() != "none" {
        assert_eq!(again["skippedAlreadyDone"], json!(true), "{}/{}: the one-time marker was not stamped: {again}", c.stage, c.name);
    }
}

fn run_case(c: &Case) {
    let (got, w, o) = replay(c);
    second_run_is_a_no_op(c, &got, &w, &o);
}

macro_rules! case {
    ($fn:ident, $stage:literal, $name:literal, $row:literal, $key:literal, $func:literal, $seed:expr, live: $live:literal, markers: $markers:literal) => {
        #[test]
        fn $fn() {
            run_case(&Case { stage: $stage, name: $name, row: $row, key: $key, func: $func, seed: $seed, live: $live, markers: $markers });
        }
    };
}

case!(import_repairs_a_partition_the_earlier_build_never_imported, "reader-cursors-import", "bad_state", "reader-cursors-import", "readerCursorsImport", "readerCursorsImportPostUpdate", seed_import, live: true, markers: false);
case!(import_skips_a_session_with_an_empty_cwd_like_node, "reader-cursors-import", "empty_cwd", "reader-cursors-import", "readerCursorsImport", "readerCursorsImportPostUpdate", seed_import_no_cwd, live: true, markers: false);
case!(import_preview_writes_nothing, "reader-cursors-import", "dry_run", "reader-cursors-import", "readerCursorsImport", "readerCursorsImportPostUpdate", seed_import, live: false, markers: false);
case!(import_already_done_is_skipped, "reader-cursors-import", "already_done", "reader-cursors-import", "readerCursorsImport", "readerCursorsImportPostUpdate", seed_import, live: true, markers: true);
case!(floor_repair_retires_pinning_readers_and_raises_the_floors, "reader-floor-repair", "bad_state", "reader-floor-repair", "readerFloorRepair", "readerFloorRepairPostUpdate", seed_floors, live: true, markers: false);
case!(floor_repair_preview_writes_nothing, "reader-floor-repair", "dry_run", "reader-floor-repair", "readerFloorRepair", "readerFloorRepairPostUpdate", seed_floors, live: false, markers: false);
case!(floor_repair_already_done_is_skipped, "reader-floor-repair", "already_done", "reader-floor-repair", "readerFloorRepair", "readerFloorRepairPostUpdate", seed_floors, live: true, markers: true);
case!(dual_acks_raise_the_twin_partition_through_what_was_already_read, "dual-partition-acks", "bad_state", "dual-partition-acks", "dualPartitionAcks", "dualPartitionAcksPostUpdate", seed_dual, live: true, markers: false);
case!(dual_acks_preview_writes_nothing, "dual-partition-acks", "dry_run", "dual-partition-acks", "dualPartitionAcks", "dualPartitionAcksPostUpdate", seed_dual, live: false, markers: false);
case!(dual_acks_already_done_is_skipped, "dual-partition-acks", "already_done", "dual-partition-acks", "dualPartitionAcks", "dualPartitionAcksPostUpdate", seed_dual, live: true, markers: true);

case!(promote_turns_a_marker_into_the_heartbeat_session, "promote-unclaimed", "bad_state", "promote-unclaimed", "promoteUnclaimed", "promoteUnclaimedPostUpdate", seed_promote, live: true, markers: false);
case!(promote_ignores_the_ingest_dry_run_switch_like_node, "promote-unclaimed", "dry_run_switch", "promote-unclaimed", "promoteUnclaimed", "promoteUnclaimedPostUpdate", seed_promote, live: false, markers: false);

// ---- fallback, the switch, nothing deleted -------------------------------------------------------------------------------------

/// A journal store is not one the engine opens: the stage hands the whole run to Node (here a shim that fails, so the answer is the
/// engine's own "could not run" answer and the shim saw exactly the fallback function).
#[test]
fn a_journal_store_is_left_to_the_node_function() {
    let o = Opt { ds: true, no_upstream_move: true, live: true, no_node: true, stage_only: Some("reader-floor-repair"), ..Opt::default() };
    let w = world(&o);
    seed_floors(&w.home);
    fs::write(ds(&w.home).join("store").join(STORE).join("BACKEND"), "journal\n").unwrap();
    let ans = engine_answer(&w, &o, "readerFloorRepair");
    assert_eq!(ans["attempted"], json!(false), "{ans}");
    assert!(ans["detail"].as_str().unwrap().starts_with("reader-floor-repair skipped: the Node stage could not run"), "{ans}");
    let calls = stage_node_calls(&w);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(calls[0].contains("readerFloorRepairPostUpdate"), "{calls:?}");
    let db = fs::read(ds(&w.home).join("store").join(STORE).join("devswarm.db")).unwrap();
    assert!(!db.is_empty());
}

/// `enabled = false` in the stage table switches a stage off on the next run, with the table's text, and nothing runs for it.
#[test]
fn a_stage_switched_off_reports_itself_skipped_and_touches_nothing() {
    let o = Opt { ds: true, no_upstream_move: true, live: true, no_node: true, stage_only: Some("none"), ..Opt::default() };
    let w = world(&o);
    seed_floors(&w.home);
    seed_dual(&w.home);
    let before = picture(&w, Value::Null, None);
    let out = run_engine(&w, &o, &["--post-pull-only"]);
    let status: Value = serde_json::from_str(out.stdout.lines().next().unwrap()).unwrap();
    let status = &status["status"];
    for (key, name) in [("readerCursorsImport", "reader-cursors-import"), ("readerFloorRepair", "reader-floor-repair"), ("dualPartitionAcks", "dual-partition-acks"), ("promoteUnclaimed", "promote-unclaimed")] {
        let a = &status[key];
        assert_eq!(a["attempted"], json!(false), "{key}: {a}");
        assert_eq!(a["disabled"], json!(true), "{key}: {a}");
        assert_eq!(a["detail"], json!(format!("{name}: disabled (enabled = false in update_post.stages) - skipped")), "{key}");
    }
    assert_eq!(stage_node_calls(&w), Vec::<String>::new());
    let after = picture(&w, Value::Null, None);
    assert_eq!(before["stores"], after["stores"], "a switched-off stage changed a store");
}

/// Reading `enabled` on every run: the same world, the stage switched back on, does its work.
#[test]
fn a_stage_switched_back_on_runs_on_the_next_run() {
    let mut o = Opt { ds: true, no_upstream_move: true, live: true, no_node: true, stage_only: Some("none"), ..Opt::default() };
    let w = world(&o);
    seed_floors(&w.home);
    let off = engine_answer(&w, &o, "readerFloorRepair");
    assert_eq!(off["disabled"], json!(true), "{off}");
    let toml = w.mp.join("plugins/anti-hall/engine/defaults/update_post.toml");
    let text = fs::read_to_string(&toml).unwrap();
    let on = text.replacen("{ name = \"reader-floor-repair\", enabled = false,", "{ name = \"reader-floor-repair\", enabled = true,", 1);
    assert_ne!(text, on);
    fs::write(&toml, on).unwrap();
    o.stage_only = Some("none");
    let ans = engine_answer(&w, &o, "readerFloorRepair");
    assert_eq!(ans["attempted"], json!(true), "{ans}");
    assert_eq!(ans["repaired"], json!(2), "{ans}");
}

/// The repairs move or mark: every message, every registry row and every legacy cursor file is still there afterwards.
#[test]
fn the_store_stages_delete_nothing() {
    for (seed, row, key) in [(seed_import as fn(&Path), "reader-cursors-import", "readerCursorsImport"), (seed_floors, "reader-floor-repair", "readerFloorRepair"), (seed_dual, "dual-partition-acks", "dualPartitionAcks")] {
        let o = Opt { ds: true, no_upstream_move: true, live: true, no_node: true, stage_only: Some(row), ..Opt::default() };
        let w = world(&o);
        seed(&w.home);
        let before = picture(&w, Value::Null, None);
        engine_answer(&w, &o, key);
        let after = picture(&w, Value::Null, None);
        for store in before["stores"].as_object().unwrap().keys() {
            for table in ["messages", "registry"] {
                assert_eq!(before["stores"][store][table], after["stores"][store][table], "{row}: {store}.{table} changed");
            }
            let was = before["stores"][store]["reader_cursors"].as_array().unwrap();
            let now = after["stores"][store]["reader_cursors"].as_array().unwrap();
            for r in was {
                let same = now.iter().find(|n| n["partition"] == r["partition"] && n["ns"] == r["ns"] && n["reader"] == r["reader"]);
                let same = same.unwrap_or_else(|| panic!("{row}: reader row {r} vanished"));
                assert!(same["value"].as_i64() >= r["value"].as_i64(), "{row}: a cursor was lowered: {r} -> {same}");
            }
        }
        for f in before["files"].as_object().unwrap().keys() {
            assert!(after["files"].get(f).is_some(), "{row}: file {f} vanished");
        }
    }
}

/// A store with one session the engine cannot classify (relative cwd) is left byte-identical: the engine writes nothing for it,
/// even for the partition that alone would import cleanly, and the Node function answers from the untouched state.
#[test]
fn a_store_needing_the_node_fallback_is_left_untouched_by_the_engine() {
    let o = Opt { ds: true, no_upstream_move: true, live: true, no_node: true, stage_only: Some("reader-cursors-import"), ..Opt::default() };
    let w = world(&o);
    seed_import_deferred(&w.home);
    let before = picture(&w, Value::Null, None);
    let ans = engine_answer(&w, &o, "readerCursorsImport");
    assert_eq!(ans["attempted"], json!(false), "{ans}");
    let calls = stage_node_calls(&w);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(calls[0].contains("readerCursorsImportPostUpdate"), "{calls:?}");
    let after = picture(&w, Value::Null, None);
    assert_eq!(before["stores"], after["stores"], "the engine wrote rows for a store it handed to Node");
    assert!(before["stores"][STORE]["reader_cursors"].as_array().unwrap().is_empty(), "seed must start without reader rows");
}

/// The same for the two repairs that write per partition: the engine plans the whole store first and, when Node must decide any
/// part of it, writes nothing (not even the partition that alone would be repaired).
#[test]
fn the_repairs_leave_a_store_needing_the_node_fallback_untouched() {
    type Seed = fn(&Path);
    let cases: [(Seed, &str, &str, &str); 3] = [
        (seed_floors_deferred, "reader-floor-repair", "readerFloorRepair", "readerFloorRepairPostUpdate"),
        (seed_dual_deferred, "dual-partition-acks", "dualPartitionAcks", "dualPartitionAcksPostUpdate"),
        (seed_dual_import_deferred, "dual-partition-acks", "dualPartitionAcks", "dualPartitionAcksPostUpdate"),
    ];
    for (seed, row, key, func) in cases {
        let o = Opt { ds: true, no_upstream_move: true, live: true, no_node: true, stage_only: Some(row), ..Opt::default() };
        let w = world(&o);
        seed(&w.home);
        let before = picture(&w, Value::Null, None);
        let ans = engine_answer(&w, &o, key);
        assert_eq!(ans["attempted"], json!(false), "{row}: {ans}");
        let calls = stage_node_calls(&w);
        assert_eq!(calls.len(), 1, "{row}: {calls:?}");
        assert!(calls[0].contains(func), "{row}: {calls:?}");
        let after = picture(&w, Value::Null, None);
        assert_eq!(before["stores"], after["stores"], "{row}: the engine wrote to a store it handed to Node");
    }
}
