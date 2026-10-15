//! The session-start checks that used to hand almost every call to Node (verify-first-orch, jev-review-reminder and devswarm-child-role),
//! through the real dispatcher and a real daemon, on both hosts (Claude and Codex).
//!
//! * no Node on the normal path: every entry of the event is mapped (`--fallback-map`) to a command that records its id, and none of
//!   the three ports may appear in that record, while the joined output carries what each of them says;
//! * latency: the p50 of each check's own time in the daemon (`check_latency_us` of `ah-engine metrics`) over a run of calls stays
//!   under the budget in `tests/budgets.toml` (`[native_session_checks]`), scaled for an unoptimised build.
//!
//! The byte-for-byte comparison with the Node hooks is in `spawn_ctx_parity.rs` (verify-first-orch, both entries),
//! `port_guards_parity.rs` (jev-review-reminder) and `devswarm_role_parity.rs` (devswarm-child-role).
#![allow(clippy::unwrap_used, clippy::expect_used)] // a test crate: a panic is the failure report, and E2 exempts tests

use crate::common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_ah-engine");

/// Calls per host; the first one starts the daemon and is not counted.
const CALLS: usize = 25;

struct Scene {
    dir: PathBuf,
}

impl Scene {
    fn new(tag: &str) -> Scene {
        let root = std::env::var_os("AH_ENGINE_IT_ROOT").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
        let dir = root.join(format!("ah-nsc-{tag}-{}", std::process::id()));
        ah_engine::discard::harmless(std::fs::remove_dir_all(&dir));
        let home = dir.join("home");
        for d in [".claude/projects/-p", ".anti-hall/logs", "nogit"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        // Jev on, with a decision log that makes one review due: the review line is part of the output
        std::fs::write(home.join(".anti-hall/settings.json"), r#"{"jev":{"enabled":true}}"#).unwrap();
        let rows: String =
            (0..31).map(|i| format!("{{\"ts\":\"2026-01-0{}T00:00:00.000Z\",\"id\":\"newRequest\",\"mode\":\"shadow\"}}\n", 1 + i % 9)).collect();
        std::fs::write(home.join(".anti-hall/logs/jev-assist.ndjson"), rows).unwrap();
        Scene { dir }
    }

    fn home(&self) -> PathBuf {
        self.dir.join("home")
    }

    fn plugin() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("plugins/anti-hall").canonicalize().unwrap()
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(BIN);
        c.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", self.home())
            .env("USERPROFILE", self.home())
            .env("AH_ENGINE_DIR", self.dir.join("eng"))
            .env("AH_ENGINE_PLUGIN_ROOT", Scene::plugin())
            .env("AH_ENGINE_VERSION", "native-session-checks")
            .env("AH_ENGINE_DISPATCH_IN_PROCESS", "0")
            .env("AH_ENGINE_SNAPSHOT_MS", "100")
            .env("ANTIHALL_TEST_ISOLATION", "1")
            .env("ANTIHALL_PROCWATCH", "0")
            .env("AH_ENGINE_PROCWATCH_MS", "0")
            .env("ANTIHALL_INGEST_DRY_RUN", "1")
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            // a DevSwarm Primary, the shape that used to defer on every call
            .env("DEVSWARM_REPO_ID", "repo-1")
            .env("DEVSWARM_BUILDER_ID", "abc-123")
            .env("DEVSWARM_AI_AGENT", "claude")
            .current_dir(&self.dir);
        c
    }

    /// A map that replaces every Node hook of the event with a command that records its id.
    fn map(&self, host: &str, event: &str) -> PathBuf {
        let m: serde_json::Map<String, serde_json::Value> = ah_engine::dispatch::table::entries(host, event)
            .into_iter()
            .map(|e| {
                let cmd = format!("echo '{}' >> \"$AH_TEST_RAN\"", e.id);
                (e.id, cmd.into())
            })
            .collect();
        let p = self.dir.join(format!("map-{host}.json"));
        std::fs::write(&p, serde_json::json!({ event: m }).to_string()).unwrap();
        p
    }

    fn payload(&self, host: &str) -> String {
        let cwd = self.dir.join("nogit");
        let mut p = serde_json::json!({"hook_event_name": "SessionStart", "source": "startup", "session_id": "S1", "cwd": cwd,
            "transcript_path": self.home().join(".claude/projects/-p/S1.jsonl"), "model": "m"});
        if host == "codex" {
            p["turn_id"] = serde_json::json!("t1");
        }
        p.to_string()
    }

    /// One dispatcher call; returns (exit code, stdout, stderr).
    fn hook(&self, host: &str, ran: &Path) -> (i32, String, String) {
        let map = self.map(host, "SessionStart");
        let mut ch = self
            .cmd()
            .env("AH_TEST_RAN", ran)
            .env("CLAUDE_PLUGIN_ROOT", Scene::plugin())
            .args(["hook", "--event", "SessionStart", "--host", host, "--fallback-map"])
            .arg(map)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        ch.stdin.take().unwrap().write_all(self.payload(host).as_bytes()).unwrap();
        let o = ch.wait_with_output().unwrap();
        (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let o = self.cmd().args(args).output().unwrap();
        serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{args:?} did not print JSON ({e}): {}", String::from_utf8_lossy(&o.stdout)))
    }

    /// The p50 (microseconds) of a check's own time in the daemon, and its call count.
    fn p50_us(&self, check: &str) -> (u64, u64) {
        // the daemon snapshots its counters every `AH_ENGINE_SNAPSHOT_MS`: wait (bounded) for one that holds every call
        let t = std::time::Instant::now();
        loop {
            let m = self.json(&["metrics", "--json", "--check", check]);
            let calls: u64 =
                m["metrics"]["counters"].as_array().unwrap().iter().filter(|c| c["name"] == "check_calls").map(|c| c["value"].as_u64().unwrap()).sum();
            let h = m["metrics"]["histograms"].as_array().unwrap().iter().find(|h| h["name"] == "check_latency_us").cloned();
            if let (Some(h), true) = (h, calls >= CALLS as u64) {
                return (h["p50_us"].as_u64().unwrap(), calls);
            }
            assert!(t.elapsed() < std::time::Duration::from_secs(30), "no latency snapshot for {check}: {m}");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

impl Drop for Scene {
    fn drop(&mut self) {
        common::reap(&self.dir.join("eng"), || {
            ah_engine::discard::harmless(self.cmd().arg("stop").output());
        });
        ah_engine::discard::harmless(std::fs::remove_dir_all(&self.dir));
    }
}

/// The budgets of `tests/budgets.toml`, `[native_session_checks]`.
fn budget_ms(key: &str) -> f64 {
    let t = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/budgets.toml")).unwrap();
    let v: toml::Value = toml::from_str(&t).unwrap();
    let b = &v["native_session_checks"];
    let num = |v: &toml::Value| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)).unwrap();
    let factor = if cfg!(debug_assertions) { num(&b["debug_factor"]) } else { 1.0 };
    num(&b[key]) * factor
}

/// The answer's context with the part the dispatcher spilled to a file (a SessionStart join is over the host's inline cap) put back.
fn with_spill(out: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(out.trim()) else { return out.to_string() };
    let ctx = v["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string();
    match ctx.rsplit_once("Read it with the Read tool: ") {
        Some((_, tail)) => {
            let path = tail.trim().trim_end_matches(']');
            format!("{ctx}\n{}", std::fs::read_to_string(path).unwrap_or_default())
        }
        None => ctx,
    }
}

/// Start the daemon with dispatcher calls and wait until it answers (a client that finds no daemon starts one and lets that call pass).
fn warm_daemon(s: &Scene, host: &str, ran: &Path) {
    let t = std::time::Instant::now();
    while t.elapsed() < std::time::Duration::from_secs(90) {
        s.hook(host, ran);
        if s.cmd().args(["ctl", "ping"]).output().is_ok_and(|o| o.status.success()) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("the daemon never came up");
}

/// Warm the stable launchers the child-role check compares with (Node writes them when it loads), so the check can answer.
fn warm_launchers(s: &Scene) {
    let hook = Scene::plugin().join("hooks/devswarm-child-role.js");
    let mut c = Command::new("node");
    c.arg(hook)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", s.home())
        .env("ANTIHALL_TEST_ISOLATION", "1")
        .env("DEVSWARM_REPO_ID", "repo-1")
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    assert!(c.status().unwrap().success());
    assert!(s.home().join(".anti-hall/bin/devswarm.js").exists(), "the launchers exist");
}

fn run_host(host: &str, orch_check: &str) {
    let s = Scene::new(host);
    warm_launchers(&s);
    let ran = s.dir.join("ran.txt");
    warm_daemon(&s, host, &ran);
    std::fs::write(&ran, "").unwrap();
    let mut last = String::new();
    for i in 0..CALLS {
        let (code, out, err) = s.hook(host, &ran);
        assert_eq!(code, 0, "call {i}: {out} {err}");
        last = format!("{}\nSTDERR: {err}\nRAN: {}", with_spill(&out), std::fs::read_to_string(&ran).unwrap_or_default());
    }
    // what the three ports say, in one joined answer
    assert!(last.contains("ORCHESTRATION DISCIPLINE"), "verify-first-orch spoke: {last}");
    assert!(last.contains("W. DEVSWARM PRIMARY"), "the Primary rule is part of it: {last}");
    assert!(last.contains("devswarm-comms"), "devswarm-child-role spoke: {last}");
    assert!(last.contains("jev-review: review due for newRequest"), "jev-review-reminder spoke: {last}");
    // and Node ran for none of them
    let ran_ids = std::fs::read_to_string(&ran).unwrap();
    for id in ["verify-first-orch", "jev-review-reminder", "devswarm-child-role"] {
        assert!(!ran_ids.lines().any(|l| l.starts_with(id)), "{host}: Node was started for {id}; it ran: {ran_ids}");
    }
    // latency, from the daemon's own histogram
    for (check, key) in
        [(orch_check, "verify_first_orch_p50_ms"), ("jev-review-reminder", "jev_review_reminder_p50_ms"), ("devswarm-child-role", "devswarm_child_role_p50_ms")]
    {
        let (p50, calls) = s.p50_us(check);
        let budget = budget_ms(key);
        eprintln!("{host}/{check}: p50 {:.2} ms over {calls} calls (budget {budget:.0} ms)", p50 as f64 / 1000.0);
        assert!(calls >= (CALLS - 1) as u64, "{check}: the daemon answered the calls ({calls})");
        assert!(p50 as f64 / 1000.0 <= budget, "{host}/{check}: p50 {:.2} ms is over the budget of {budget} ms", p50 as f64 / 1000.0);
    }
}

#[test]
fn the_claude_session_start_ports_start_no_node_and_stay_inside_their_budget() {
    run_host("claude", "verify-first-orch");
}

#[test]
fn the_codex_session_start_ports_start_no_node_and_stay_inside_their_budget() {
    run_host("codex", "verify-first-orch-codex");
}
