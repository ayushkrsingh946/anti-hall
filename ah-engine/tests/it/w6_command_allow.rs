//! W6: command-guard allows a short `node -e` JSON filter that indexes a key like `j["git-guard"]`, and still blocks what the Node guard
//! blocks (a block is never weaker than Node's, #235 / #236): an inline-body `gh issue create` (a remote write that needs delegation), a push of
//! any branch but the current one (also an existing non-protected branch of a sibling worktree), a push of a protected or missing
//! branch, a force push, a `node -e` that writes through a computed or bracketed API name, and the other gh mutations.
#![allow(clippy::unwrap_used, clippy::expect_used)] // a test crate: a panic is the failure report, and E2 exempts tests

use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn plugin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../plugins/anti-hall")
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"]).args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

/// (exit code, stderr) of `ah-engine check command` for one Bash command run as the main thread from `cwd`.
fn verdict(dir: &Path, home: &Path, cwd: &Path, command: &str) -> (i32, String) {
    let payload = json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "session_id": "w6", "cwd": cwd, "tool_input": {"command": command}});
    let mut c = Command::new(env!("CARGO_BIN_EXE_ah-engine"));
    c.args(["check", "command"])
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("AH_ENGINE_DIR", dir.join("state"))
        .env("AH_ENGINE_NOSPAWN", "1")
        .env("ANTIHALL_TEST_ISOLATION", "1")
        .env("AH_ENGINE_PLUGIN_ROOT", plugin())
        .env("CLAUDE_PLUGIN_ROOT", plugin())
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut ch = c.spawn().unwrap();
    ch.stdin.take().unwrap().write_all(payload.to_string().as_bytes()).unwrap();
    let o = ch.wait_with_output().unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stderr).into_owned())
}

#[test]
fn single_small_state_changing_commands_are_allowed_and_the_real_cases_still_block() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("w6-cmd-{}", std::process::id()));
    ah_engine::discard::harmless(std::fs::remove_dir_all(&dir));
    std::fs::create_dir_all(dir.join("home")).unwrap();
    let home = dir.join("home").canonicalize().unwrap();
    let (bare, main, wt2) = (home.join("remote.git"), home.join("main"), home.join("wt2"));
    assert!(Command::new("git").args(["init", "-q", "--bare"]).arg(&bare).status().unwrap().success());
    assert!(Command::new("git").args(["init", "-q", "-b", "base"]).arg(&main).status().unwrap().success());
    git(&main, &["commit", "-q", "--allow-empty", "-m", "x"]);
    git(&main, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&main, &["branch", "main"]); // a local main exists: it still must not be carved out
    git(&main, &["worktree", "add", "-q", "-b", "feat2", wt2.to_str().unwrap()]);
    let skip = home.join("skip.json");
    std::fs::write(&skip, "{}").unwrap();

    let allowed = |c: &str| {
        let (code, err) = verdict(&dir, &home, &main, c);
        assert_eq!((code, err.as_str()), (0, ""), "should be allowed: {c}");
    };
    let blocked = |c: &str| {
        let (code, err) = verdict(&dir, &home, &main, c);
        assert!(code == 2 && err.contains("command-guard"), "should be blocked: {c} -> {code} {err}");
    };
    allowed(&format!(r#"node -e 'const j=require("{}");console.log(j["git-guard"], j.plugins["anti-hall@anti-hall"])'"#, skip.display()));

    // #256 (owner decision): filing an issue from a prepared body file and `cargo clean` are bookkeeping, not floods; the config rows are
    // command.light_exceptions. An inline body, a pull request, a release and the builds stay heavy.
    allowed(r#"gh issue create --repo o/r --title "x (y)" --body-file /tmp/f.md"#);
    allowed(&format!("cd {} && gh issue create --repo o/r --title x --body-file /tmp/f.md 2>&1 | tail -1", main.display()));
    allowed("gh issue comment 5 --repo o/r --body-file /tmp/f.md");
    allowed("cargo clean");
    blocked(r#"gh issue create --repo o/r --title "x" --body "inline""#);
    blocked("cargo build");
    blocked("cargo clean && cargo build");
    // #235: a plain push exempts the CURRENT branch only; another existing local branch (a sibling worktree's) does not
    blocked("git push -u origin feat2");
    blocked("git push origin feat2");
    blocked("git push -u origin main");
    blocked("git push -u origin no-such-branch");
    blocked("git push --force origin feat2");
    blocked("git push origin +feat2");
    blocked(r#"node -e 'require("fs")["writeFileSync"]("/tmp/x","y")'"#);
    blocked(r#"node -e 'require("fs")["write"+"FileSync"]("/tmp/x","y")'"#);
    blocked("gh issue delete 5");
    blocked(r#"gh pr create --repo o/r --title "x" --body-file /tmp/f.md"#);
    blocked("gh release create v1 --notes x");
    ah_engine::discard::harmless(std::fs::remove_dir_all(&dir));
}
