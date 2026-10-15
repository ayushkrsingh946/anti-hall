//! Task 66, the interpreter guardrail: a Stop check script never pulls a megabyte-sized transcript window into the interpreter
//! as one string. Reading, decoding, splitting and picking the lines are the host's work (`ah.fs.tailLines`, `ah.fs.tailHasAny`,
//! `ah.transcript.*`); a script receives the few lines it parses. This test reads the scripts and fails when a new `readTail`
//! call or an unfiltered `rp.lines` appears outside the list below, each entry of which says why it may stay.
use std::path::{Path, PathBuf};

fn logic_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugins/anti-hall/engine/logic")
}

fn scripts() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![logic_dir()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "js") {
                let rel = p.strip_prefix(logic_dir()).unwrap().to_string_lossy().into_owned();
                out.push((rel, std::fs::read_to_string(&p).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// Scripts that may still call `ah.fs.readTail`, and why: none of them runs on Stop.
const READ_TAIL_ALLOWED: &[(&str, &str)] = &[
    ("compact-declaration-guard.js", "PreCompact: one read of the declaration window per compaction"),
    ("git.js", "the git guard reads the end of a log file it names, at most git_guard.* bytes, on a Bash call"),
    ("merge-gate.js", "PreToolUse on a merge command"),
    ("precompact-snapshot.js", "PreCompact: one read of the handover tail per compaction"),
];

#[test]
fn only_the_listed_scripts_read_a_transcript_window_into_the_interpreter() {
    let mut bad = Vec::new();
    for (rel, src) in scripts() {
        if rel == "lib/60-b3.js" {
            continue; // the definition of the primitive
        }
        if src.contains("ah.fs.readTail(") && !READ_TAIL_ALLOWED.iter().any(|(f, _)| *f == rel) {
            bad.push(format!("{rel}: ah.fs.readTail(...) pulls a whole window in as one string; use ah.fs.tailLines / ah.fs.tailHasAny"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn every_allowance_is_still_needed() {
    let all = scripts();
    for (f, why) in READ_TAIL_ALLOWED {
        let src = all.iter().find(|(r, _)| r == f).map(|(_, s)| s.as_str());
        assert!(src.is_some_and(|s| s.contains("ah.fs.readTail(")), "{f} no longer calls readTail: drop its entry ({why})");
    }
}

#[test]
fn the_stop_checks_ask_the_host_for_the_lines_they_parse() {
    let all = scripts();
    let get = |name: &str| all.iter().find(|(r, _)| r == name).map(|(_, s)| s.as_str()).unwrap();
    // tasklist-guard's work scan, the judge and the speculation guard's reply read name the lines they parse
    assert!(get("tasklist-guard.js").contains("rp.lines(path, tlN('window_bytes'), ['tool_use'"));
    assert!(get("speculation-judge.js").contains("'assistant'"));
    assert!(get("speculation-guard.js").contains("['assistant'"));
    // the prompt key, the code-edit scan and the context reading fetch only the lines they read
    assert!(get("task-guard.js").contains("ah.fs.tailLines("));
    assert!(get("codex-nudge.js").contains("ah.fs.tailLines("));
    assert!(get("lib/75-ctxbudget.js").contains("ah.fs.tailLines("));
    assert!(get("compact-advice-guard.js").contains("ah.fs.tailHasAny("));
    // and none of the Stop checks splits a transcript string itself
    for f in ["tasklist-guard.js", "task-guard.js", "codex-nudge.js", "compact-advice-guard.js", "auto-handover-pause-nag.js", "speculation-judge.js", "speculation-guard.js", "claim-ledger.js"] {
        assert!(!get(f).contains("tail.split("), "{f} splits a transcript tail in the interpreter");
    }
}
