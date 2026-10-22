//! End-to-end parity of the task Stop guards (`task-guard`, `tasklist-guard`) against the real Node hooks on every non-string or
//! falsy task status a transcript can carry (#251): a number, boolean, object, array, null, 0 and false, at each place a status
//! is read (TodoWrite, TaskCreate, TaskUpdate), kept to the end, replaced by a later update, and (TaskUpdate) on a task never
//! seen. The engine decides every scenario (a check with no verdict follows its failure mode, an allow on Stop), so nothing may
//! defer: the answer must equal Node's, block where Node blocks.

use super::fx::*;
use super::fx_tasklines::*;
use super::jsjson::J;
use super::support::*;
use std::cell::{Cell, RefCell};
use std::path::Path;
use std::sync::Arc;

fn stop(tp: &str) -> J {
    jo! {"hook_event_name": "Stop", "session_id": "sess-1", "cwd": "$PROJ", "transcript_path": tp, "stop_hook_active": false}
}

fn shapes() -> Vec<(&'static str, J)> {
    vec![
        ("num", J::from(7i64)),
        ("true", J::Bool(true)),
        ("obj", jo! {"a": 1}),
        ("arr", ja!["completed"]),
        ("null", J::Null),
        ("zero", J::from(0i64)),
        ("false", J::Bool(false)),
    ]
}

/// `(id, transcript lines)` for every site x shape x ending.
fn histories(tl: &Tl, works: usize) -> Vec<(String, Vec<String>)> {
    let mut v = Vec::new();
    let in_prog = || tl.update(1, vec![("status", J::from("in_progress"))]);
    let done = || tl.update(1, vec![("status", J::from("completed"))]);
    let work = |tl: &Tl| -> Vec<String> {
        (0..works)
            .map(|i| {
                let tu = tl.use_("Edit", jo! {"file_path": format!("/p/src/f{i}.js"), "old_string": "a", "new_string": format!("b{i}")});
                tl.asst(vec![tu])
            })
            .collect()
    };
    for (name, shape) in shapes() {
        let sites: Vec<(&str, Box<dyn Fn() -> Vec<String> + '_>)> = vec![
            ("todo", Box::new(|| tl.todo(ja![jo! {"id": "1", "content": "x", "status": shape.clone()}]))),
            ("create", Box::new(|| tl.create(1, jo! {"subject": "x", "status": shape.clone()}))),
            ("update", Box::new(|| tl.update(1, vec![("status", shape.clone())]))),
            ("create-then-update", Box::new(|| cat(vec![tl.create(1, jo! {"subject": "x"}), tl.update(1, vec![("status", shape.clone())])]))),
        ];
        for (site, mk) in sites {
            let ends: Vec<(&str, Box<dyn Fn() -> Vec<String> + '_>)> = vec![
                ("kept", Box::new(Vec::new)),
                ("inprogress", Box::new(|| in_prog())),
                ("completed", Box::new(|| done())),
            ];
            for (end, tail) in ends {
                let lines = cat(vec![vec![tl.prompt("go")], mk(), tail(), work(tl)]);
                v.push((format!("badstatus-{site}-{name}-{end}"), lines));
            }
        }
    }
    v
}

pub(crate) fn task_guard() -> (Vec<Scenario>, Scratch) {
    let tl = Tl::new();
    let shared = Scratch::new("bs-tg");
    let w = World::new().git("proj");
    let mut out = Vec::new();
    for (id, lines) in histories(&tl, 0) {
        let f = shared.path().join(format!("{id}.jsonl"));
        write_file(&f, format!("{}\n", lines.join("\n")).as_bytes());
        out.push(Scenario::one(&id, stop(&f.to_string_lossy()), &w));
    }
    (out, shared)
}

pub(crate) fn tasklist_guard() -> (Vec<Scenario>, Scratch) {
    let tl = Tl::new();
    let shared = Scratch::new("bs-tl");
    let today = iso_from_ms(now_ms() as i64)[..10].to_string();
    let prog = format!("proj/.anti-hall/progress/{today}/sess-1.md");
    let old_secs = now_ms() as f64 / 1000.0 - 40.0 * 86400.0;
    let stale = World::new().git("proj").file(&prog, "# progress\n");
    let n = Cell::new(0usize);
    let out: RefCell<Vec<Scenario>> = RefCell::new(Vec::new());
    for (id, lines) in histories(&tl, 3) {
        n.set(n.get() + 1);
        let f = shared.path().join(format!("{id}.jsonl"));
        write_file(&f, format!("{}\n", lines.join("\n")).as_bytes());
        let mut step = Step::payload(stop(&f.to_string_lossy()));
        let rel = prog.clone();
        step.before = Some(Arc::new(move |w: &Path| set_mtime(&w.join(&rel), old_secs)));
        out.borrow_mut().push(Scenario::new(&id, stale.clone(), vec![step]));
    }
    (out.into_inner(), shared)
}

pub(crate) fn opts_task_guard() -> Opts {
    Opts::new("task-guard", "task-guard.js", "task-guard")
}
pub(crate) fn opts_tasklist_guard() -> Opts {
    let mut o = Opts::new("tasklist-guard", "tasklist-guard.js", "tasklist-guard");
    o.mask_out = Some(super::fx_tasklist_guard::mask_ack_time);
    o
}
