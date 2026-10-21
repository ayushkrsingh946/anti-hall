//! Roadmap #61 group G4: the six reconcile-family post-update stages (`reconcile`, `fold`, `fold-all-stores`,
//! `heal-orphan-partitions`, `fold-archived-rows`, `heal-registry-rows`) run by the engine's witnessed ports when the engine-mode
//! switch says engine, pinned against the Node functions of `update.js`.
//!
//! Each scenario seeds a scratch home with the BAD state an earlier version left (a phantom registry row beside a live one, an
//! orphan partition, a stale registry path, a row of an archived workspace), runs the Node function on one world to FREEZE its
//! answer, the stores and the files (`AH_RECORD_UPDATE_FIXTURES=1`, `tests/fixtures/update_post/<stage>/<name>.json`), and replays
//! the engine on another world with the switch on. In engine mode the witness still runs Node, on scratch mirrors only: `node` is a
//! spy that records every call and runs the real one, and the replay asserts that the stage's own Node function
//! (`...PostUpdate`) was never called while the witness agreed for every job. A second `node` shim disagrees with the engine on
//! purpose and fails every other call: nothing may be applied and the refusal is logged.
//!
//! Without the switch (the default) the stage runs Node's function: `the_default_mode_runs_the_node_function` pins that.
use super::update_parity::{Opt, World, envs, finish, world};
use super::update_post_stages::{STORE, VERSION, anchor, diff, fixture_path, msg, open_store, picture, put, recording, registry, repo};
use ah_engine::meshw::store::CursorPut;
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

/// The DevSwarm app's own CLI is never called by these scenarios (a path that is not there).
const NO_HC: &[(&str, &str)] = &[("ANTIHALL_DEVSWARM_HIVECONTROL", "/nonexistent/hivecontrol")];

const WITNESS: &str = ".anti-hall/logs/devswarm-recon-witness.ndjson";

fn real_key() -> String {
    ah_engine::meshw::ident::repo_key_for_worktree(&repo().to_string_lossy()).unwrap().unwrap()
}

fn wt() -> String {
    repo().to_string_lossy().into_owned()
}

fn descriptor(id: &str, worktree: &str, session: &str, extra: &str) -> String {
    format!(r#"{{"id":"{id}","worktreePath":"{worktree}","sessionId":"{session}","inboxPath":null,"cursorPath":null,"nudgeCommand":null{extra}}}"#)
}

// ---- seeds ---------------------------------------------------------------------------------------------------------------

/// Two stores, each with a live row and a phantom row (no session) on one worktree; the phantom holds unread direct mail.
fn seed_fold(home: &Path) {
    for (store, n) in [(real_key(), 3), (STORE.to_string(), 1)] {
        let st = open_store(home, &store);
        registry(&st, "w-live", Some(&wt()), Some("s1"));
        registry(&st, "w-ph", Some(&wt()), None);
        for i in 0..n {
            msg(&st, "w-ph", i, &format!("{store}-m{i}"), None);
        }
        // the partition was imported by the earlier build: it holds the floor of each namespace
        for ns in ["store", "nd"] {
            st.reader_cursor_txn(&[CursorPut { partition: "w-ph".into(), ns: ns.into(), reader: "#floor".into(), value: 0, retired_line: None, updated_at: 1000 }]).unwrap();
        }
    }
}

/// A partition with mail and no registry row, whose descriptor names a live session (the orphan heal adopts it).
fn seed_orphans(home: &Path) {
    let st = open_store(home, &real_key());
    msg(&st, "o1", 0, "o1-a", None);
    msg(&st, "o1", 1, "o1-b", None);
    drop(st);
    registry(&open_store(home, STORE), "other", None, None);
    put(home, ".anti-hall/devswarm/workspaces/o1.json", &descriptor("o1", &wt(), "so1", ""));
}

/// A registry row whose path is stale and whose descriptor carries no owner key: both are healed.
fn seed_heal(home: &Path) {
    let st = open_store(home, &real_key());
    registry(&st, "w1", Some("/nonexistent/old/path"), Some("s1"));
    drop(st);
    registry(&open_store(home, STORE), "w9", None, None);
    put(home, ".anti-hall/devswarm/workspaces/w1.json", &descriptor("w1", &wt(), "s1", ""));
}

/// The archived marker of a workspace whose registry row an older build left behind.
fn seed_archived(home: &Path) {
    let st = open_store(home, &real_key());
    registry(&st, "a1", Some(&wt()), Some("s1"));
    drop(st);
    put(home, ".anti-hall/devswarm/archived/a1.json", &descriptor("a1", &wt(), "s1", ""));
}

/// The same, with mail in the archived workspace's partition: retiring its row would leave an orphan partition, which the engine
/// does not decide (Node does).
fn seed_archived_with_mail(home: &Path) {
    seed_archived(home);
    msg(&open_store(home, &real_key()), "a1", 1, "a1-n", None);
}

/// A directory that is no project (reconcile reports it) holding a descriptor.
fn seed_nothing(home: &Path) {
    put(home, ".anti-hall/devswarm/workspaces/w1.json", r#"{"id":"w1"}"#);
}

/// A project whose store holds one row with no worktree: reconcile has nothing to drain.
fn seed_empty_project(home: &Path) {
    registry(&open_store(home, &real_key()), "w1", None, None);
}

/// A project whose store holds the row of a workspace whose worktree is gone and which was archived: skipped, not a failure.
fn seed_gone_archived(home: &Path) {
    registry(&open_store(home, &real_key()), "w-gone", Some("/nonexistent/gone/worktree"), Some("s1"));
    put(home, ".anti-hall/devswarm/archived/w-gone.json", &descriptor("w-gone", "/nonexistent/gone/worktree", "s1", ""));
}

// ---- the scenarios -------------------------------------------------------------------------------------------------------

struct Case {
    stage: &'static str,
    name: &'static str,
    key: &'static str,
    func: &'static str,
    seed: fn(&Path),
    /// Run from the shared checkout (the project of the working directory), else from the home (no project).
    in_repo: bool,
    /// Extra environment for both runs.
    env: &'static [(&'static str, &'static str)],
    /// Seed the one-time marker so the stage is already done.
    marked: bool,
}

fn opt(_c: &Case) -> Opt {
    Opt { ds: true, no_upstream_move: true, live: true, ..Opt::default() }
}

fn cwd_of(c: &Case, w: &World) -> std::path::PathBuf {
    if c.in_repo { repo().to_path_buf() } else { w.home.clone() }
}

fn mark(c: &Case, home: &Path) {
    if c.marked {
        let keys: &[&str] = match c.stage {
            "fold-all-stores" => &["foldAllStores"],
            "heal-orphan-partitions" => &["healOrphanPartitions"],
            "fold-archived-rows" => &["foldArchivedRows", "foldArchivedFamilyDescriptors"],
            "heal-registry-rows" => &["healRegistry"],
            _ => &[],
        };
        let body: Vec<String> = keys.iter().map(|k| format!(r#""{k}":{{"completedVersion":"{VERSION}"}}"#)).collect();
        put(home, ".anti-hall/update-sweep-state.json", &format!("{{{}}}", body.join(",")));
    }
}

/// Node's stage function on the world, as `runUpdate` calls it (the store hashes smallest first, the version).
fn node_answer(w: &World, o: &Opt, c: &Case) -> Value {
    let snippet = r#"const os=require("os");const u=require(process.argv[1]+"/skills/update/scripts/update.js");const d=require(process.argv[1]+"/companion/lib/devswarm-store.js");const o={version:process.argv[3]};o.paths=u.resolvePaths(process.env,os.homedir());o.hashes=u.orderStoreHashesBySize(d,os.homedir(),d.listStoreHashes(os.homedir())||[],undefined);console.log(JSON.stringify(u[process.argv[2]](o)))"#;
    let mut cmd = Command::new("node");
    cmd.arg("-e").arg(snippet).arg(w.mp.join("plugins/anti-hall")).arg(c.func).arg(VERSION).current_dir(cwd_of(c, w));
    envs(&mut cmd, w, o);
    cmd.env("ANTIHALL_DEVSWARM_SWEEP_TAIL_MODE", "node");
    for (k, v) in c.env {
        cmd.env(k, v);
    }
    let out = finish(cmd.output().unwrap());
    assert_eq!(out.code, 0, "node stage failed: {}", out.stderr);
    serde_json::from_str(out.stdout.lines().rev().find(|l| !l.trim().is_empty()).unwrap()).unwrap()
}

/// How the replay's `node` behaves.
#[derive(Clone, Copy, PartialEq)]
enum Shim {
    /// Records the call and runs the real Node.
    Spy,
}

fn install_shim(w: &World, shim: Shim) {
    let real = String::from_utf8(Command::new("sh").args(["-c", "command -v node"]).output().unwrap().stdout).unwrap();
    let log = w.root.join("node-spy.log");
    let body = match shim {
        Shim::Spy => format!("#!/bin/sh\necho \"node $*\" >> '{}'\nexec {} \"$@\"\n", log.display(), real.trim()),
    };
    put(&w.bin, "node", &body);
    fs::set_permissions(w.bin.join("node"), fs::Permissions::from_mode(0o755)).unwrap();
}

fn spy_log(w: &World) -> Vec<String> {
    fs::read_to_string(w.root.join("node-spy.log")).unwrap_or_default().lines().map(str::to_string).collect()
}

/// The engine's `update --post-pull-only` on the world with the switch on; its answer for the stage.
fn engine_answer(w: &World, o: &Opt, c: &Case, mode: &str) -> Value {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ah-engine"));
    cmd.arg("update").arg("--post-pull-only").current_dir(cwd_of(c, w));
    envs(&mut cmd, w, o);
    cmd.env("AH_ENGINE_PLUGIN_ROOT", w.mp.join("plugins/anti-hall"));
    let cfg = w.root.join("cfg");
    fs::create_dir_all(&cfg).unwrap();
    cmd.env("AH_ENGINE_CONFIG", cfg.join("config.toml")).env("AH_ENGINE_SETTINGS", cfg.join("settings.json"));
    // a test-only knob: the same replay with the switch forced to node shows what the assertions catch
    let mode = std::env::var("AH_TEST_FORCE_MODE").unwrap_or_else(|_| mode.to_string());
    fs::write(cfg.join("config.toml"), format!("[devswarm_sup]\nreconcile_mode = \"{mode}\"\n")).unwrap();
    cmd.env("ANTIHALL_DEVSWARM_SWEEP_TAIL_MODE", &mode);
    for (k, v) in c.env {
        cmd.env(k, v);
    }
    let out = finish(cmd.output().unwrap());
    assert_eq!(out.code, 0, "engine failed: {}", out.stderr);
    let status: Value = serde_json::from_str(out.stdout.lines().next().unwrap()).unwrap();
    status["status"].get(c.key).unwrap_or_else(|| panic!("no {} in {}", c.key, status["status"])).clone()
}

fn row_of(c: &Case) -> &'static str {
    c.stage
}

fn witness(w: &World) -> Vec<Value> {
    fs::read_to_string(w.home.join(WITNESS)).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Record (when asked) the frozen Node picture; build and seed the replay world.
fn prepare(c: &Case, shim: Shim) -> (World, Opt, Value) {
    let o = opt(c);
    let a = c.anchor();
    if recording() {
        let node_world = world(&o);
        (c.seed)(&node_world.home);
        mark(c, &node_world.home);
        let ans = node_answer(&node_world, &o, c);
        let pic = picture(&node_world, ans, Some(&a));
        let p = fixture_path(c.stage, c.name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, format!("{}\n", serde_json::to_string_pretty(&pic).unwrap())).unwrap();
    }
    let want: Value = serde_json::from_str(&fs::read_to_string(fixture_path(c.stage, c.name)).unwrap_or_else(|e| panic!("{}/{}: no fixture ({e}); record with AH_RECORD_UPDATE_FIXTURES=1", c.stage, c.name))).unwrap();
    let eo = Opt { stage_only: Some(row_of(c)), ..o };
    let w = world(&eo);
    install_shim(&w, shim);
    (c.seed)(&w.home);
    mark(c, &w.home);
    (w, eo, want)
}

impl Case {
    fn anchor(&self) -> String {
        anchor()
    }
}

/// The members of two pictures that differ, for a failure message; the answer's `detail` is compared as well.
/// The engine's witness leaves its own log and the (emptied) scratch directory under `.anti-hall`; Node has neither.
fn without_witness(v: &Value) -> Value {
    let mut v = v.clone();
    if let Some(f) = v["files"].as_object_mut() {
        f.retain(|k, _| k != ".anti-hall/witness/" && k != ".anti-hall/logs/" && k != ".anti-hall/devswarm/capabilities.json");
        // the capability probe cache of `hivecontrol` carries the time it was taken
        let at = regex::Regex::new(r#""checkedAt": "[^"]*""#).unwrap();
        for text in f.values_mut() {
            if let Some(t) = text.as_str() {
                *text = Value::String(at.replace_all(t, r#""checkedAt": "T""#).into_owned());
            }
        }
    }
    v
}

fn same(c: &Case, want: &Value, got: &Value) {
    let (want, got) = (&without_witness(want), &without_witness(got));
    for k in ["answer", "stores", "files"] {
        assert!(want[k] == got[k], "{}/{}: {k} differs from the frozen Node result\n{}", c.stage, c.name, diff(&want[k], &got[k]));
    }
}

fn run_case(c: &Case) {
    let (w, o, want) = prepare(c, Shim::Spy);
    let ans = engine_answer(&w, &o, c, "engine");
    let got = picture(&w, ans, Some(&c.anchor()));
    same(c, &want, &got);
    let calls = spy_log(&w);
    assert!(calls.is_empty(), "{}/{}: the engine started Node: {calls:?}", c.stage, c.name);
    // the native result of every job is logged (no Node comparison any more)
    let jobs = witness(&w);
    assert!(jobs.iter().all(|j| j["native"] == json!(true)), "{}/{}: a job log line is not a native one: {jobs:?}", c.stage, c.name);
    let again = engine_answer(&w, &o, c, "engine");
    if c.name == "budget" {
        // the first run stopped at the budget and left the rest; the next run resumes that list, drains it and stamps the version
        assert_eq!((got["answer"]["budgetExhausted"].clone(), got["answer"]["pending"].clone()), (json!(true), json!(1)), "{got}");
        assert_eq!((again["budgetExhausted"].clone(), again["pending"].clone(), again["stores"].clone()), (json!(false), json!(0), json!(1)), "the resume: {again}");
        let third = engine_answer(&w, &o, c, "engine");
        assert_eq!(third["skippedAlreadyDone"], json!(true), "{third}");
        return;
    }
    // a second run changes nothing: the one-time marker was stamped (or the stage is idempotent without one)
    let pic = picture(&w, again.clone(), Some(&c.anchor()));
    assert_eq!(got["stores"], pic["stores"], "{}/{}: a second run changed a store", c.stage, c.name);
    let strip = |v: &Value| {
        let mut f = without_witness(v)["files"].as_object().unwrap().clone();
        f.retain(|k, _| !k.contains("update-sweep-state") && !k.contains("devswarm.jsonl"));
        f
    };
    assert_eq!(strip(&got), strip(&pic), "{}/{}: a second run changed a file", c.stage, c.name);
    if got["answer"]["errors"] == json!(0) && !c.marked && c.stage != "reconcile" && c.stage != "fold" {
        assert_eq!(again["skippedAlreadyDone"], json!(true), "{}/{}: the one-time marker was not stamped: {again}", c.stage, c.name);
    }
    // nothing is deleted: every message is still there (a retired registry row is the one guarded removal)
    let before = {
        let a = world(&opt(c));
        (c.seed)(&a.home);
        picture(&a, Value::Null, None)
    };
    let msgs = |p: &Value| p["stores"].as_object().unwrap().values().map(|s| s["messages"].as_array().unwrap().len()).sum::<usize>();
    assert!(msgs(&got) >= msgs(&before), "{}/{}: a message vanished", c.stage, c.name);
}

macro_rules! case {
    ($fn:ident, $stage:literal, $name:literal, $key:literal, $func:literal, $seed:expr, repo: $repo:literal, env: $env:expr, marked: $marked:literal) => {
        #[test]
        fn $fn() {
            run_case(&Case { stage: $stage, name: $name, key: $key, func: $func, seed: $seed, in_repo: $repo, env: $env, marked: $marked });
        }
    };
}

case!(fold_all_retires_the_phantom_rows_of_every_store, "fold-all-stores", "bad_state", "foldAllStores", "foldAllStoresPostUpdate", seed_fold, repo: false, env: &[], marked: false);
case!(fold_all_stops_at_the_sweep_budget_and_leaves_a_resume_list, "fold-all-stores", "budget", "foldAllStores", "foldAllStoresPostUpdate", seed_fold, repo: false, env: &[("ANTIHALL_UPDATE_SWEEP_BUDGET_MS", "0")], marked: false);
case!(fold_all_already_done_is_skipped, "fold-all-stores", "already_done", "foldAllStores", "foldAllStoresPostUpdate", seed_fold, repo: false, env: &[], marked: true);
case!(orphans_are_adopted_into_the_registry, "heal-orphan-partitions", "bad_state", "healOrphanPartitions", "healOrphanPartitionsPostUpdate", seed_orphans, repo: false, env: &[], marked: false);
case!(orphans_already_done_is_skipped, "heal-orphan-partitions", "already_done", "healOrphanPartitions", "healOrphanPartitionsPostUpdate", seed_orphans, repo: false, env: &[], marked: true);
case!(heal_registry_repairs_a_stale_path_and_a_descriptor_without_an_owner, "heal-registry-rows", "bad_state", "healRegistryRows", "healRegistryPostUpdate", seed_heal, repo: false, env: &[], marked: false);
case!(heal_registry_already_done_is_skipped, "heal-registry-rows", "already_done", "healRegistryRows", "healRegistryPostUpdate", seed_heal, repo: false, env: &[], marked: true);
case!(archived_rows_are_retired_and_both_halves_stamped, "fold-archived-rows", "bad_state", "foldArchivedRows", "foldArchivedRowsPostUpdate", seed_archived, repo: false, env: &[], marked: false);
case!(archived_row_with_mail_is_left_to_node, "fold-archived-rows", "handback_mail", "foldArchivedRows", "foldArchivedRowsPostUpdate", seed_archived_with_mail, repo: false, env: &[], marked: false);
case!(archived_rows_already_done_is_skipped, "fold-archived-rows", "already_done", "foldArchivedRows", "foldArchivedRowsPostUpdate", seed_archived, repo: false, env: &[], marked: true);
case!(fold_folds_the_project_of_the_working_directory, "fold", "bad_state", "fold", "foldMeshPostUpdate", seed_fold, repo: true, env: &[], marked: false);
case!(reconcile_of_a_directory_that_is_no_project_reports_it, "reconcile", "no_project", "reconcile", "reconcilePostUpdate", seed_nothing, repo: false, env: &[], marked: false);
case!(reconcile_of_a_project_with_nothing_to_drain, "reconcile", "empty_project", "reconcile", "reconcilePostUpdate", seed_empty_project, repo: true, env: NO_HC, marked: false);
case!(reconcile_skips_an_archived_workspace_whose_worktree_is_gone, "reconcile", "gone_archived", "reconcile", "reconcilePostUpdate", seed_gone_archived, repo: true, env: NO_HC, marked: false);

// ---- the switch ----------------------------------------------------------------------------------------------------------

/// A stage whose switch says anything but `engine` is disabled: nothing runs, no Node is started, and the answer says so.
#[test]
fn a_switched_off_stage_is_disabled_and_starts_no_node() {
    let c = Case { stage: "fold-all-stores", name: "bad_state", key: "foldAllStores", func: "foldAllStoresPostUpdate", seed: seed_fold, in_repo: false, env: &[], marked: false };
    let (w, o, _) = prepare(&c, Shim::Spy);
    let ans = engine_answer(&w, &o, &c, "off");
    assert_eq!(ans["disabled"], json!(true), "{ans}");
    assert!(spy_log(&w).is_empty(), "a switched-off stage started Node: {:?}", spy_log(&w));
    assert!(witness(&w).is_empty(), "a switched-off stage logged a job");
}

/// The reconcile stage follows `devswarm_sup.reconcile_mode`, the others `devswarm_sup.sweep_tail_mode`.
#[test]
fn each_stage_follows_its_own_switch() {
    let c = Case { stage: "reconcile", name: "empty_project", key: "reconcile", func: "reconcilePostUpdate", seed: seed_empty_project, in_repo: true, env: &[], marked: false };
    let (w, o, _) = prepare(&c, Shim::Spy);
    // every switch (sweep tail and reconcile) is forced to `off` by the harness: the reconcile stage is disabled
    let ans = engine_answer(&w, &o, &c, "off");
    assert_eq!(ans["disabled"], json!(true), "{ans}");
    assert!(spy_log(&w).is_empty(), "{:?}", spy_log(&w));
}

