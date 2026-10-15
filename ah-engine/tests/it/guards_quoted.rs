//! R1 GUARDS (#176): guards read quoted or echoed text as text. Each case runs the engine's check and the Node twin on the same
//! payload in a scratch HOME and compares the verdicts (the twin is the parity half).
#![allow(clippy::unwrap_used, clippy::expect_used)] // a test crate: a panic is the failure report, and E2 exempts tests

use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn plugin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../plugins/anti-hall")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("gq-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("home")).unwrap();
    dir
}

fn feed(mut c: Command, dir: &std::path::Path, stdin: &str) -> (i32, String) {
    c.env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", dir.join("home"))
        .env("AH_ENGINE_DIR", dir.join("state"))
        .env("AH_ENGINE_NOSPAWN", "1")
        .env("AH_ENGINE_PLUGIN_ROOT", plugin())
        .env("CLAUDE_PLUGIN_ROOT", plugin())
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut ch = c.spawn().unwrap();
    ch.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let o = ch.wait_with_output().unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).to_string())
}

/// (engine verdict, Node twin verdict) for the same payload.
fn both(check: &str, hook: &str, dir: &std::path::Path, payload: &Value) -> ((i32, String), (i32, String)) {
    let mut e = Command::new(env!("CARGO_BIN_EXE_ah-engine"));
    e.args(["check", check]).env_clear();
    let mut n = Command::new("node");
    n.arg(plugin().join("hooks").join(hook)).env_clear();
    (feed(e, dir, &payload.to_string()), feed(n, dir, &payload.to_string()))
}

fn spawn(prompt: &str) -> Value {
    json!({"hook_event_name": "PreToolUse", "tool_name": "Agent", "session_id": "s1", "cwd": "/tmp",
        "tool_input": {"subagent_type": "general-purpose", "model": "haiku", "description": "list files", "prompt": prompt}})
}

/// A word that is only quoted ('deploy' in a question about the word) does not make the spawn deploy-shaped; the same word
/// unquoted still does.
#[test]
fn model_routing_does_not_read_a_quoted_word_as_a_deploy_task() {
    let dir = scratch("mr");
    let floor = "deploy/migration/secret-shaped spawn";
    for prompt in [
        "List every file in README that mentions the word 'deploy'. Do not run anything.",
        "List every file in README that mentions the word \"deploy\". Do not run anything.",
        "It's a list task, don't touch anything: find files that say 'migrate' or 'rollback'.",
        "Set the status label on issue 12 and post this comment:\n\"Shipped the cache fix.\nWe will deploy it after review,\nthen migrate the index.\"\nNothing else.",
    ] {
        let (eng, node) = both("model-routing", "model-routing-guard.js", &dir, &spawn(prompt));
        assert!(!eng.1.contains(floor), "engine: {prompt}: {}", eng.1);
        assert!(!node.1.contains(floor), "node: {prompt}: {}", node.1);
    }
    for prompt in ["Deploy the functions to the staging project.", "List files, then deploy 'the build' now."] {
        let (eng, node) = both("model-routing", "model-routing-guard.js", &dir, &spawn(prompt));
        assert!(eng.1.contains(floor), "engine must still see the deploy: {prompt}: {}", eng.1);
        assert!(node.1.contains(floor), "node must still see the deploy: {prompt}: {}", node.1);
    }
}

fn rescue(prompt: &str, result: &str) -> Value {
    json!({"hook_event_name": "PostToolUse", "tool_name": "Agent", "session_id": "s1", "cwd": "/tmp",
        "tool_input": {"subagent_type": "codex:codex-rescue", "prompt": prompt}, "tool_response": result})
}

/// A Codex result that only echoes the agent's own brief (which mentions a quota limit) is not an outage; a real exhaustion
/// message beside the echo still is.
#[test]
fn codex_quota_detect_ignores_the_agents_own_prompt_text() {
    let prompt = "Review the patch.\nIf you have hit your usage limit, say so and stop.\nReport findings only.";
    let echo = format!("Task received:\n{prompt}\nDone: no findings.");
    let dir = scratch("cq");
    let (eng, node) = both("codex-quota-detect", "codex-quota-detect.js", &dir, &rescue(prompt, &echo));
    assert!(eng.1.trim().is_empty(), "engine marked Codex unavailable on its own prompt: {}", eng.1);
    assert!(node.1.trim().is_empty(), "node marked Codex unavailable on its own prompt: {}", node.1);
    assert!(!dir.join("home/.anti-hall/codex-availability.json").exists(), "nothing recorded");
    // a JSON-shaped result escapes the newlines and quotes of the echoed brief
    let obj = json!({"result": echo});
    let (eng, node) = both("codex-quota-detect", "codex-quota-detect.js", &dir, &rescue(prompt, "x").tap_resp(obj));
    assert!(eng.1.trim().is_empty() && node.1.trim().is_empty(), "{} / {}", eng.1, node.1);
    // the harness result may carry the brief itself beside the answer
    let wrapped = json!({"status": "completed", "prompt": prompt, "content": [{"type": "text", "text": "Done: no findings."}]});
    let (eng, node) = both("codex-quota-detect", "codex-quota-detect.js", &dir, &rescue(prompt, "x").tap_resp(wrapped));
    // a result with several keys that mention a quota is the Node hook's to read: the engine defers to it
    assert!(matches!(eng.1.trim(), "" | "AHFALLBACK") && node.1.trim().is_empty(), "{} / {}", eng.1, node.1);
    assert!(!dir.join("home/.anti-hall/codex-availability.json").exists(), "nothing recorded");
    // the real message still counts
    let real = format!("{echo}\nError: you have hit your usage limit, try again at 2026-10-30T12:00:00Z.");
    let (eng, node) = both("codex-quota-detect", "codex-quota-detect.js", &dir, &rescue(prompt, &real));
    assert!(eng.1.contains("quota exhaustion"), "{}", eng.1);
    assert!(node.1.contains("quota exhaustion"), "{}", node.1);
}

trait TapResp {
    fn tap_resp(self, v: Value) -> Value;
}
impl TapResp for Value {
    fn tap_resp(mut self, v: Value) -> Value {
        self["tool_response"] = v;
        self
    }
}
