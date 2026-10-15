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
    // each runner has its own home and state: the first one's loop-state and records must not decide the second's answer
    let (eh, nh) = (dir.join("e"), dir.join("n"));
    for h in [&eh, &nh] {
        std::fs::create_dir_all(h.join("home")).unwrap();
    }
    (feed(e, &eh, &payload.to_string()), feed(n, &nh, &payload.to_string()))
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
    for h in ["e", "n"] {
        assert!(!dir.join(h).join("home/.anti-hall/codex-availability.json").exists(), "nothing recorded by {h}");
    }
    // a JSON-shaped result escapes the newlines and quotes of the echoed brief
    let obj = json!({"result": echo});
    let (eng, node) = both("codex-quota-detect", "codex-quota-detect.js", &dir, &rescue(prompt, "x").tap_resp(obj));
    assert!(eng.1.trim().is_empty() && node.1.trim().is_empty(), "{} / {}", eng.1, node.1);
    // the harness result may carry the brief itself beside the answer
    let wrapped = json!({"status": "completed", "prompt": prompt, "content": [{"type": "text", "text": "Done: no findings."}]});
    let (eng, node) = both("codex-quota-detect", "codex-quota-detect.js", &dir, &rescue(prompt, "x").tap_resp(wrapped));
    // a result with several keys that mention a quota is the Node hook's to read: the engine defers to it
    assert!(matches!(eng.1.trim(), "" | "AHFALLBACK") && node.1.trim().is_empty(), "{} / {}", eng.1, node.1);
    for h in ["e", "n"] {
        assert!(!dir.join(h).join("home/.anti-hall/codex-availability.json").exists(), "nothing recorded by {h}");
    }
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

// ---- #177: the task guard reads a task's current subject and honours an owner that is a running agent ----

fn stamp(minutes_ago: i64) -> String {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64 - minutes_ago * 60_000;
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // civil-from-days (Howard Hinnant): the UTC date of a day count since 1970-01-01
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn asst_tool(ago: i64, id: &str, name: &str, input: Value) -> Value {
    json!({"type": "assistant", "timestamp": stamp(ago), "message": {"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}})
}

fn tool_res(ago: i64, id: &str, text: &str) -> Value {
    json!({"type": "user", "timestamp": stamp(ago), "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": [{"type": "text", "text": text}]}]}})
}

/// A Stop whose transcript holds the given lines; returns (engine, node) answers.
fn stop(tag: &str, lines: &[Value]) -> ((i32, String), (i32, String)) {
    let dir = scratch(tag);
    let tp = dir.join("t.jsonl");
    std::fs::write(&tp, lines.iter().map(|l| l.to_string() + "\n").collect::<String>()).unwrap();
    let p = json!({"hook_event_name": "Stop", "session_id": "s1", "cwd": dir.to_string_lossy(), "transcript_path": tp.to_string_lossy(), "stop_hook_active": false});
    both("task-guard", "task-guard.js", &dir, &p)
}

fn create(ago: i64, tu: &str, n: u32, subject: &str) -> [Value; 2] {
    [
        asst_tool(ago, tu, "TaskCreate", json!({"subject": subject, "description": subject})),
        json!({"type": "user", "timestamp": stamp(ago), "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tu, "content": format!("Task #{n} created successfully: {subject}")}]}}),
    ]
}

/// A task renamed after it was created is listed under its CURRENT subject, not the one it was created with.
#[test]
fn the_task_guard_lists_a_renamed_task_by_its_current_subject() {
    let mut l: Vec<Value> = create(90, "toolu_c1", 1, "R0 old working title").into();
    l.push(asst_tool(60, "toolu_u1", "TaskUpdate", json!({"taskId": "1", "subject": "#12 renamed board subject"})));
    let (eng, node) = stop("tg-rename", &l);
    for (who, out) in [("engine", &eng.1), ("node", &node.1)] {
        assert_ne!(out.trim(), "AHFALLBACK", "{who} answers this Stop itself");
        assert!(out.contains("#12 renamed board subject"), "{who} must list the current subject: {out}");
        assert!(!out.contains("R0 old working title"), "{who} listed the old subject: {out}");
    }
    assert!(node.1.contains("#12 renamed board subject"), "node: {}", node.1);
}

/// A task owned by a running agent named by the `name` it was launched with (not its id or description) is attended.
#[test]
fn the_task_guard_treats_an_owner_that_is_a_running_named_agent_as_attended() {
    let launch_text = "Async agent launched successfully.\nagentId: abc123def456 (internal ID - do not mention to user. Use SendMessage with to: 'abc123def456')\nThe agent is working in the background.\noutput_file: /tmp/none/abc123def456.output\n";
    let mut l: Vec<Value> = create(90, "toolu_c1", 1, "Lane 9 hardening").into();
    l.push(asst_tool(80, "toolu_u1", "TaskUpdate", json!({"taskId": "1", "status": "in_progress", "owner": "live9"})));
    l.push(asst_tool(70, "toolu_a1", "Agent", json!({"description": "train lane", "name": "live9", "prompt": "go", "run_in_background": true})));
    l.push(tool_res(70, "toolu_a1", launch_text));
    let (eng, node) = stop("tg-owner-name", &l);
    for (who, out) in [("engine", &eng.1), ("node", &node.1)] {
        assert!(!out.contains("\"decision\":\"block\""), "{who} blocked on a task its running agent owns: {out}");
    }
    // the same task with an owner that is no running agent still blocks
    let mut l2: Vec<Value> = create(90, "toolu_c1", 1, "Lane 9 hardening").into();
    l2.push(asst_tool(80, "toolu_u1", "TaskUpdate", json!({"taskId": "1", "status": "in_progress", "owner": "ghost"})));
    let (eng, node) = stop("tg-owner-ghost", &l2);
    assert!(node.1.contains("\"decision\":\"block\""), "node: {}", node.1);
    assert!(eng.1.contains("\"decision\":\"block\"") || eng.1.trim() == "AHFALLBACK", "engine: {}", eng.1);
}

// ---- #178: silent-agent-nudge expires a days-old record and leaves a quiet daemon alone ----

/// A Stop whose transcript carries one adopted background task (a compaction's `task_status` attachment) whose output file was last
/// written `age_min` minutes ago. Returns (engine, node) answers, each in its own home.
fn silent_stop(tag: &str, task_type: &str, description: &str, age_min: i64) -> ((i32, String), (i32, String)) {
    let dir = scratch(tag);
    let out = dir.join("task.output");
    std::fs::write(&out, "x").unwrap();
    let when = std::time::SystemTime::now() - std::time::Duration::from_secs(age_min as u64 * 60);
    std::fs::OpenOptions::new().write(true).open(&out).unwrap().set_modified(when).unwrap();
    let att = json!({"type": "attachment", "timestamp": stamp(age_min + 5), "attachment": {"type": "task_status", "taskId": "bg1", "taskType": task_type,
        "description": description, "status": "running", "outputFilePath": out.to_string_lossy()}});
    let tp = dir.join("t.jsonl");
    std::fs::write(&tp, att.to_string() + "\n").unwrap();
    let p = json!({"hook_event_name": "Stop", "session_id": "s1", "cwd": dir.to_string_lossy(), "transcript_path": tp.to_string_lossy(), "stop_hook_active": false});
    both("silent-agent-nudge", "silent-agent-nudge.js", &dir, &p)
}

fn nudged(out: &str) -> bool {
    out.contains("\"decision\":\"block\"")
}

/// A record about 63 hours old is gone, not silent; one of a few hours still is.
#[test]
fn the_silent_agent_nudge_expires_a_record_older_than_the_configured_age() {
    let (eng, node) = silent_stop("sn-stale", "local_agent", "Spike scripted checks go/no-go", 3785);
    assert!(!nudged(&eng.1), "engine nudged a 63 h old record: {}", eng.1);
    assert!(!nudged(&node.1), "node nudged a 63 h old record: {}", node.1);
    let (eng, node) = silent_stop("sn-fresh", "local_agent", "Spike scripted checks go/no-go", 29);
    assert!(nudged(&eng.1) && nudged(&node.1), "a 29 min silent agent is still nudged: {} / {}", eng.1, node.1);
}

/// A background shell whose description says it is a daemon is quiet by design; the same silence of an ordinary shell or agent is not.
#[test]
fn the_silent_agent_nudge_exempts_a_quiet_daemon_by_the_configured_rule() {
    let (eng, node) = silent_stop("sn-daemon", "local_bash", "Restart the build-load throttle daemon", 29);
    assert!(!nudged(&eng.1), "engine nudged a daemon: {}", eng.1);
    assert!(!nudged(&node.1), "node nudged a daemon: {}", node.1);
    let (eng, node) = silent_stop("sn-shell", "local_bash", "Run the full test suite", 29);
    assert!(nudged(&eng.1) && nudged(&node.1), "an ordinary silent shell is still nudged: {} / {}", eng.1, node.1);
}

/// R1 GUARDS (#179). A spawn payload with a running writer in the transcript, so the shared-tree note has something to compare.
fn shared_tree_pair(tag: &str, prompt: &str) -> ((i32, String), (i32, String)) {
    let dir = scratch(tag);
    let t = dir.join("t.jsonl");
    let launch = json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Agent",
        "input": {"subagent_type": "general-purpose", "prompt": "fix it"}}]}});
    let result = json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1",
        "content": "Async agent launched successfully.\nagentId: a1b2c3d4e5f60718 (internal ID - do not mention to user)\noutput_file: /tmp/x/a1b2c3d4e5f60718.output"}]}});
    std::fs::write(&t, format!("{launch}\n{result}\n")).unwrap();
    let p = json!({"hook_event_name": "PreToolUse", "tool_name": "Agent", "session_id": "s1", "cwd": dir.to_string_lossy(),
        "transcript_path": t.to_string_lossy(),
        "tool_input": {"subagent_type": "general-purpose", "model": "sonnet", "description": "sync labels", "prompt": prompt}});
    both("swarm-guard", "swarm-guard.js", &dir, &p)
}

/// A spawn that only calls the GitHub API shares no checkout, so the shared-tree note stays silent; the same spawn without that
/// statement is still a write-capable agent beside a running one.
#[test]
fn the_shared_tree_note_is_silent_for_a_spawn_that_only_calls_the_github_api() {
    for prompt in [
        "Sync the status labels on issues 12 and 13. gh-API-only: no checkout, no file edits.",
        "Set the board status via the GitHub API only. Do not touch the working tree.",
    ] {
        let (eng, node) = shared_tree_pair("st-api", prompt);
        assert!(!eng.1.contains("shared-tree"), "engine: {prompt}: {}", eng.1);
        assert!(!node.1.contains("shared-tree"), "node: {prompt}: {}", node.1);
    }
    let (eng, node) = shared_tree_pair("st-write", "Fix the cache bug and update the tests.");
    assert!(eng.1.contains("shared-tree") && node.1.contains("shared-tree"), "a writer still gets the note: {} / {}", eng.1, node.1);
    let (eng, node) = shared_tree_pair("st-inplace", "Use the GitHub API only for labels, then edit the repo files in place.");
    assert!(eng.1.contains("shared-tree") && node.1.contains("shared-tree"), "an in-place statement still cancels: {} / {}", eng.1, node.1);
}

/// A brief that moves and removes files, commits and opens a PR is a write task, however many research words it carries.
#[test]
fn model_routing_does_not_read_a_git_mv_commit_and_pr_brief_as_read_only() {
    let dir = scratch("mr-write");
    let readonly = "research/read-only-shaped spawn";
    let write = |prompt: &str| json!({"hook_event_name": "PreToolUse", "tool_name": "Agent", "session_id": "s1", "cwd": "/tmp",
        "tool_input": {"subagent_type": "general-purpose", "model": "sonnet", "description": "tidy the docs", "prompt": prompt}});
    for prompt in [
        "Find the stale docs, git mv the kept ones into docs/, git rm the rest, commit and open a PR.",
        "Locate the duplicate scripts, git rm them and open a pull request with the result.",
    ] {
        let (eng, node) = both("model-routing", "model-routing-guard.js", &dir, &write(prompt));
        assert!(!eng.1.contains(readonly), "engine: {prompt}: {}", eng.1);
        assert!(!node.1.contains(readonly), "node: {prompt}: {}", node.1);
    }
    let (eng, node) = both("model-routing", "model-routing-guard.js", &dir, &write("Research how the guards are wired and report what you find."));
    assert!(eng.1.contains(readonly) && node.1.contains(readonly), "a read-only brief is still nudged: {} / {}", eng.1, node.1);
}

/// R1 GUARDS (#180). merge-side-pick records a side-pick with the checkout and branch it happened on and warns only for a push of
/// that branch. A fixture repository per case; each case's engine and Node runs keep their own session state between steps.
fn side_pick_repo(dir: &std::path::Path, name: &str, branch: &str) -> PathBuf {
    let repo = dir.join(name);
    std::fs::create_dir_all(&repo).unwrap();
    let ok = Command::new("git").args(["init", "-q", "-b", branch]).arg(&repo).status().unwrap().success();
    assert!(ok, "git init");
    repo
}

fn bash_step(event: &str, cwd: &std::path::Path, cmd: &str) -> Value {
    json!({"hook_event_name": event, "tool_name": "Bash", "session_id": "sp1", "cwd": cwd.to_string_lossy(), "tool_input": {"command": cmd}})
}

/// After a `git checkout --theirs` recorded on `engine-proto`, does `push` (run in the repo, or in `other`) draw the advisory? (engine, node)
fn pick_then_push(tag: &str, pushed_from: &str, switch_to: Option<&str>, push: &str) -> (bool, bool) {
    let dir = scratch(tag);
    let repo = side_pick_repo(&dir, "repo", "engine-proto");
    let other = side_pick_repo(&dir, "other", "main");
    let (_, _) = both("merge-side-pick", "merge-side-pick.js", &dir, &bash_step("PostToolUse", &repo, "git checkout --theirs .claude/skills/x.md"));
    if let Some(b) = switch_to {
        let ok = Command::new("git").arg("-C").arg(&repo).args(["symbolic-ref", "HEAD", &format!("refs/heads/{b}")]).status().unwrap().success();
        assert!(ok, "switch branch");
    }
    let at = if pushed_from == "other" { &other } else { &repo };
    let (e, n) = both("merge-side-pick", "merge-side-pick.js", &dir, &bash_step("PreToolUse", at, push));
    (e.1.contains("merge-side-pick"), n.1.contains("merge-side-pick"))
}

#[test]
fn merge_side_pick_stays_silent_for_a_push_that_has_no_side_pick_in_it() {
    for (tag, from, switch, push) in [
        ("sp-other-branch", "repo", None, "git push origin fix/21-bounded-caches"),
        ("sp-switched", "repo", Some("fix/21"), "git push"),
        ("sp-other-repo", "other", None, "git push origin main"),
        ("sp-other-head", "repo", None, "git push -u origin fix/21 fix/22"),
        ("sp-dash-c-other", "other", None, "git -C ../repo push origin other"),
    ] {
        let (e, n) = pick_then_push(tag, from, switch, push);
        assert!(!e && !n, "{tag}: no advisory expected (engine {e}, node {n}) for {push}");
    }
}

#[test]
fn merge_side_pick_still_warns_for_every_push_that_may_carry_the_side_pick() {
    for (tag, from, switch, push) in [
        ("sp-bare", "repo", None, "git push"),
        ("sp-remote-only", "repo", None, "git push origin"),
        ("sp-named", "repo", None, "git push origin engine-proto"),
        ("sp-forced-named", "repo", None, "git push origin +engine-proto:engine-proto"),
        ("sp-head", "repo", None, "git push origin HEAD"),
        ("sp-all", "repo", None, "git push --all origin"),
        ("sp-mirror", "repo", None, "git push --mirror"),
        ("sp-quoted", "repo", None, "git push origin \"fix/21\""),
        ("sp-cd", "other", None, "cd ../repo && git push origin other"),
        ("sp-list", "repo", None, "git push origin fix/21 engine-proto"),
        ("sp-dash-c", "other", None, "git -C ../repo push origin engine-proto"),
        ("sp-head-other", "repo", Some("main"), "git push origin engine-proto"),
    ] {
        let (e, n) = pick_then_push(tag, from, switch, push);
        assert!(e && n, "{tag}: advisory expected (engine {e}, node {n}) for {push}");
    }
}

/// A linked worktree has its own HEAD: a push from it is not a push of the branch the pick was recorded on, a push from the picked
/// worktree itself still is.
#[test]
fn merge_side_pick_tells_a_linked_worktree_from_the_picked_checkout() {
    let dir = scratch("sp-worktree");
    let repo = side_pick_repo(&dir, "repo", "engine-proto");
    let git = |args: &[&str]| {
        let ok = Command::new("git").arg("-C").arg(&repo).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).status().unwrap().success();
        assert!(ok, "git {args:?}");
    };
    git(&["commit", "-q", "--allow-empty", "-m", "x"]);
    let wt = dir.join("wt");
    git(&["worktree", "add", "-q", "-b", "wtb", &wt.to_string_lossy()]);
    both("merge-side-pick", "merge-side-pick.js", &dir, &bash_step("PostToolUse", &repo, "git checkout --theirs f"));
    let (e, n) = both("merge-side-pick", "merge-side-pick.js", &dir, &bash_step("PreToolUse", &wt, "git push"));
    assert!(!e.1.contains("merge-side-pick") && !n.1.contains("merge-side-pick"), "a push from another worktree: {} / {}", e.1, n.1);
    let (e, n) = both("merge-side-pick", "merge-side-pick.js", &dir, &bash_step("PreToolUse", &repo, "git push"));
    assert!(e.1.contains("merge-side-pick") && n.1.contains("merge-side-pick"), "a push from the picked worktree: {} / {}", e.1, n.1);
    // the pick made in the linked worktree is recorded there
    both("merge-side-pick", "merge-side-pick.js", &dir, &bash_step("PostToolUse", &wt, "git checkout --ours g"));
    let (e, n) = both("merge-side-pick", "merge-side-pick.js", &dir, &bash_step("PreToolUse", &wt, "git push origin wtb"));
    assert!(e.1.contains("merge-side-pick") && n.1.contains("merge-side-pick"), "a push of the worktree's branch: {} / {}", e.1, n.1);
}
