//! `ah-engine devswarm wake-watch` never starts a Node process, not even to hand over to a newer installed build: the handoff
//! replaces the engine process with the engine running the newer build's plugin root. Scratch HOME, a PATH that holds no `node`,
//! and plugin roots that only link the real engine defaults, so the user's home and the live engine are never touched.
#![allow(clippy::unwrap_used, clippy::expect_used)] // a test crate: a panic is the failure report
#![allow(clippy::panic, clippy::unreachable)] // integration tests: a panic is the failure report; production crates deny these in Cargo.toml

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const ID: &str = "w1";

struct Scratch {
    root: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
    own_root: PathBuf,
    nopath: PathBuf,
}

fn real_plugin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugins/anti-hall")
}

fn build(root: &Path, version: &str) {
    std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
    std::fs::write(root.join(".claude-plugin/plugin.json"), format!("{{\"name\":\"anti-hall\",\"version\":\"{version}\"}}")).unwrap();
    std::os::unix::fs::symlink(real_plugin().join("engine"), root.join("engine")).unwrap();
}

/// A scratch HOME with a marketplace clone at `own` and, when `newer` is given, a cache directory of that version (no Node
/// watcher script in it: the handoff must not need one).
fn scratch(tag: &str, own: &str, newer: Option<&str>) -> Scratch {
    let root = std::env::temp_dir().join(format!("ah-wwnative-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    let cwd = root.join("work");
    let nopath = root.join("emptybin");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&nopath).unwrap();
    let own_root = home.join(".claude/plugins/marketplaces/anti-hall/plugins/anti-hall");
    build(&own_root, own);
    if let Some(v) = newer {
        build(&home.join(".claude/plugins/cache/anti-hall/anti-hall").join(v), v);
    }
    let inbox = home.join(".anti-hall/devswarm/inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(inbox.join(format!("{ID}.ndjson")), "").unwrap();
    Scratch { root, home, cwd, own_root, nopath }
}

fn spawn(s: &Scratch, auto: bool) -> Child {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ah-engine"));
    c.args(["devswarm", "wake-watch"]);
    if auto {
        c.arg("--auto");
    }
    c.env_clear()
        .env("PATH", &s.nopath)
        .env("HOME", &s.home)
        .env("AH_ENGINE_PLUGIN_ROOT", &s.own_root)
        .env("ANTIHALL_DEVSWARM_WAKE_WATCH_POLL_MS", "250")
        .current_dir(&s.cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    c.env("DEVSWARM_SOURCE_BRANCH", "main").env("DEVSWARM_BUILDER_ID", ID);
    c.spawn().unwrap()
}

/// The stdout lines of a child, delivered as they print.
fn lines(child: &mut Child) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    let out = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for l in BufReader::new(out).lines().map_while(Result::ok) {
            if tx.send(l).is_err() {
                return;
            }
        }
    });
    rx
}

fn wait_for(rx: &mpsc::Receiver<String>, needle: &str, secs: u64, seen: &mut Vec<String>) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(l) => {
                let hit = l.contains(needle);
                seen.push(l);
                if hit {
                    return true;
                }
            }
            Err(_) => break,
        }
    }
    false
}

fn children_of(pid: u32) -> String {
    let o = Command::new("/usr/bin/pgrep").args(["-P", &pid.to_string()]).output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[test]
fn a_newer_build_is_taken_over_in_place_with_no_node_and_mail_still_wakes() {
    let s = scratch("handoff", "0.0.1", Some("9.9.9"));
    // the Node watcher of the newer build is present, as in a real install: it must be ignored
    let js = s.home.join(".claude/plugins/cache/anti-hall/anti-hall/9.9.9/companion/lib/devswarm-wake-watch.js");
    std::fs::create_dir_all(js.parent().unwrap()).unwrap();
    std::fs::write(&js, "// stub\n").unwrap();
    let mut c = spawn(&s, true);
    let rx = lines(&mut c);
    let mut seen = Vec::new();
    assert!(wait_for(&rx, "handed off to 9.9.9", 20, &mut seen), "no handoff line: {seen:?}");
    assert!(wait_for(&rx, "armed: watching child w1", 20, &mut seen), "the successor did not arm: {seen:?}");
    // the same process, no child of any kind (a Node child was the defect)
    assert_eq!(children_of(c.id()), "", "the watcher must have no child process");
    // new direct mail wakes the session with a PATH that holds no node
    std::fs::write(s.home.join(format!(".anti-hall/devswarm/inbox/{ID}.ndjson")), "{\"m\":1}\n").unwrap();
    assert!(wait_for(&rx, "new mesh mail for child w1", 20, &mut seen), "no wake line: {seen:?}");
    c.kill().unwrap();
    c.wait().unwrap();
    std::fs::remove_dir_all(&s.root).ok();
}

#[test]
fn without_a_newer_build_it_arms_wakes_and_has_no_child() {
    let s = scratch("plain", "9.9.9", None);
    let mut c = spawn(&s, true);
    let rx = lines(&mut c);
    let mut seen = Vec::new();
    assert!(wait_for(&rx, "armed: watching child w1", 20, &mut seen), "{seen:?}");
    assert!(!seen.iter().any(|l| l.contains("handed off")), "{seen:?}");
    assert_eq!(children_of(c.id()), "");
    // no mail: nothing but the arm line
    assert!(!wait_for(&rx, "new mesh mail", 1, &mut seen), "{seen:?}");
    std::fs::write(s.home.join(format!(".anti-hall/devswarm/inbox/{ID}.ndjson")), "{\"m\":1}\n{\"m\":2}\n").unwrap();
    assert!(wait_for(&rx, "(+2)", 20, &mut seen), "{seen:?}");
    c.kill().unwrap();
    c.wait().unwrap();
    std::fs::remove_dir_all(&s.root).ok();
}

#[test]
fn a_missing_build_manifest_is_no_handoff_target() {
    let s = scratch("nomanifest", "0.0.1", Some("9.9.9"));
    std::fs::remove_file(s.home.join(".claude/plugins/cache/anti-hall/anti-hall/9.9.9/.claude-plugin/plugin.json")).unwrap();
    let mut c = spawn(&s, true);
    let rx = lines(&mut c);
    let mut seen = Vec::new();
    assert!(wait_for(&rx, "armed: watching child w1", 20, &mut seen), "{seen:?}");
    assert!(!seen.iter().any(|l| l.contains("handed off")), "{seen:?}");
    assert_eq!(children_of(c.id()), "");
    c.kill().unwrap();
    c.wait().unwrap();
    std::fs::remove_dir_all(&s.root).ok();
}
