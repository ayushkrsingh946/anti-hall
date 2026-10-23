//! Two registry rows on one worktree are ONE recipient: `inbox read-primary` reads every partition of the group, each message once,
//! and the ack moves every partition's cursor; with NO Node and no exit 69. (Node: `resolveMeshPartitionIds` + the sibling fold.)
#![allow(clippy::unwrap_used, clippy::expect_used)] // a test crate: a panic is the failure report
#![allow(clippy::panic, clippy::unreachable)] // integration tests: a panic is the failure report
use ah_engine::meshw::store::{MeshRow, MeshStore};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_ah-engine");

struct Fx {
    home: PathBuf,
    repo: PathBuf,
}

impl Fx {
    fn new(tag: &str) -> Fx {
        let root = std::env::temp_dir().join(format!("ah-group-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (home, repo) = (root.join("home"), root.join("repo"));
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let o = Command::new("git").current_dir(&repo).args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false"]).args(args).output().unwrap();
            assert!(o.status.success());
        };
        git(&["init", "-q", "-b", "main"]);
        fs::write(repo.join("f"), "x").unwrap();
        git(&["add", "f"]);
        git(&["commit", "-q", "-m", "i"]);
        Fx { home, repo: repo.canonicalize().unwrap() }
    }

    fn raw(&self, argv: &[&str]) -> (i32, String) {
        let o = Command::new(BIN)
            .arg("mesh")
            .args(argv)
            .current_dir(&self.repo)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .env("AH_ENGINE_PLUGIN_ROOT", Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("plugins").join("anti-hall"))
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        (o.status.code().unwrap(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    }

    fn run(&self, argv: &[&str]) -> (i32, Value) {
        let (code, out) = self.raw(argv);
        (code, serde_json::from_str(out.trim()).unwrap_or_else(|_| panic!("not JSON ({code}): {out}")))
    }

    /// The caller's own label, its project key, and the twin: the label row (`register-primary`) and a child row on the same worktree.
    fn twins(&self) -> (String, String) {
        let (code, v) = self.run(&["register-primary", "--worktree", self.repo.to_str().unwrap(), "--session", "s-label"]);
        assert_eq!(code, 0, "{v}");
        let label = v["id"].as_str().unwrap().to_string();
        let (code, v) = self.run(&["register", "child-x", "--worktree", self.repo.to_str().unwrap(), "--session", "s-child"]);
        assert_eq!(code, 0, "{v}");
        let key = v["descriptor"]["repoKey"].as_str().unwrap().to_string();
        (label, key)
    }

    fn store(&self, key: &str) -> MeshStore {
        MeshStore::open(&self.home.join(".anti-hall/devswarm/store").join(key).join("devswarm.db")).unwrap()
    }
}

fn row(partition: &str, ts: i64, hash: &str, body: &str, sender: &str) -> MeshRow {
    MeshRow {
        workspace_id: partition.into(),
        ts,
        hash: Some(hash.into()),
        body: body.into(),
        sender: Some(sender.into()),
        recipient: Some(partition.into()),
        mtype: Some("direct".into()),
        urgency: Some("normal".into()),
        is_heartbeat: false,
        needs_reply: false,
        orig_hash: None,
        instance_nonce: None,
    }
}

#[test]
fn a_read_of_twin_rows_unions_both_partitions_once_and_the_ack_moves_both() {
    let fx = Fx::new("union");
    let (label, key) = fx.twins();
    {
        let st = fx.store(&key);
        st.append_mesh_row(&row(&label, 1_000, "h-a1", "to label 1", "peer-a")).unwrap();
        st.append_mesh_row(&row(&label, 3_000, "h-a2", "to label 2", "peer-a")).unwrap();
        st.append_mesh_row(&row("child-x", 2_000, "h-b1", "to child 1", "peer-b")).unwrap();
        st.append_mesh_row(&row("child-x", 4_000, "h-b2", "to child 2", "peer-b")).unwrap();
    }
    let (code, v) = fx.run(&["inbox", "read-primary", &label]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], Value::Bool(true), "{v}");
    let bodies: Vec<&str> = v["messages"].as_array().unwrap().iter().map(|m| m["body"].as_str().unwrap()).collect();
    assert_eq!(bodies, ["to label 1", "to child 1", "to label 2", "to child 2"], "merged by time: {v}");
    assert_eq!((v["count"].clone(), v["unreadCount"].clone(), v["total"].clone()), (Value::from(4), Value::from(4), Value::from(4)), "{v}");
    let ids: Vec<&str> = v["meshPartitionIds"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
    assert!(ids.contains(&label.as_str()) && ids.contains(&"child-x") && ids.len() == 2, "{v}");
    assert_eq!(v["messages"][1]["partitionId"].as_str(), Some("child-x"), "a sibling row names its partition: {v}");
    // the read acked nothing; the receipt carries the moves
    let rid = v["readReceiptId"].as_str().unwrap().to_string();
    let (code, a) = fx.run(&["inbox", "ack-primary", &label, "--receipt", &rid]);
    assert_eq!(code, 0, "{a}");
    assert_eq!(a["ok"], Value::Bool(true), "{a}");
    // everything is read now, in both partitions
    let (code, again) = fx.run(&["inbox", "read-primary", &label]);
    assert_eq!(code, 0, "{again}");
    assert_eq!((again["count"].clone(), again["unreadCount"].clone()), (Value::from(0), Value::from(0)), "{again}");
}

#[test]
fn a_message_present_in_both_partitions_is_delivered_once() {
    let fx = Fx::new("dup");
    let (label, key) = fx.twins();
    {
        let st = fx.store(&key);
        st.append_mesh_row(&row(&label, 1_000, "h-same", "same body", "peer-a")).unwrap();
        // a forwarded copy: same content under another hash, marked with the original's
        let mut copy = row("child-x", 1_000, "h-copy", "same body", "peer-a");
        copy.orig_hash = Some("h-same".into());
        st.append_mesh_row(&copy).unwrap();
        st.append_mesh_row(&row("child-x", 2_000, "h-only", "only here", "peer-b")).unwrap();
    }
    let (code, v) = fx.run(&["inbox", "read-primary", &label]);
    assert_eq!(code, 0, "{v}");
    let bodies: Vec<&str> = v["messages"].as_array().unwrap().iter().map(|m| m["body"].as_str().unwrap()).collect();
    assert_eq!(bodies, ["same body", "only here"], "{v}");
}

#[test]
fn a_live_sibling_is_read_but_never_acked_by_the_caller() {
    let fx = Fx::new("live");
    let (label, key) = fx.twins();
    {
        let st = fx.store(&key);
        st.append_mesh_row(&row(&label, 1_000, "h-a1", "to label", "peer-a")).unwrap();
        st.append_mesh_row(&row("child-x", 2_000, "h-b1", "to live child", "peer-b")).unwrap();
    }
    // the child is alive: a fresh heartbeat of its own session
    let beats = fx.home.join(".anti-hall/devswarm/heartbeats");
    fs::create_dir_all(&beats).unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    fs::write(beats.join("child-x.json"), format!(r#"{{"ts":{now},"sessionId":"s-child"}}"#)).unwrap();
    let (code, v) = fx.run(&["inbox", "read-primary", &label]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["count"], Value::from(2), "the live child's mail is shown: {v}");
    assert_eq!(v["liveSiblingsSkipped"], serde_json::json!(["child-x"]), "{v}");
    let rid = v["readReceiptId"].as_str().unwrap().to_string();
    let (code, a) = fx.run(&["inbox", "ack-primary", &label, "--receipt", &rid]);
    assert_eq!(code, 0, "{a}");
    // the caller-scoped watermark hides what it has seen from its next read ...
    let (code, again) = fx.run(&["inbox", "read-primary", &label]);
    assert_eq!(code, 0, "{again}");
    assert_eq!(again["count"], Value::from(0), "{again}");
    // ... but the child's own cursor never moved: its mail is still unread for the child
    let st = fx.store(&key);
    let rows = st.reader().reader_cursors("child-x").unwrap();
    let floor = rows.iter().find(|r| r["ns"] == "store" && r["reader"] == "#floor").map(|r| r["value"].as_f64().unwrap_or(0.0)).unwrap_or(0.0);
    assert_eq!(floor, 0.0, "{rows:?}");
    assert!(fx.home.join(".anti-hall/devswarm/cursors").read_dir().unwrap().flatten().any(|e| e.file_name().to_string_lossy().contains(".seen-")), "the watermark file");
}
