//! `ah-engine devswarm recover --id <ws> --request <id>`: the on-demand kill-and-resume of ONE workspace's session. It is never
//! scheduled and never started by a sweep (`devswarm_act.automatic_kinds` and `devswarm_sup.duties` do not list it).
//!
//! The engine decides whether it may start and refuses before anything is touched: a caller that is not the owner's own session
//! (the role matrix, then the automated-caller rule), an id that is not a plain workspace id, a request id that already ran, a
//! descriptor without a worktree and a session, a workspace already recovered `maxRecoveries` times. What then runs is the
//! engine's own [`super::kill`]: it resolves the process (exactly one match or it abstains), confirms its identity and working
//! directory right before SIGTERM and again before SIGKILL, and resumes the session headless. There is no Node rollback of the kill
//! (owner decision F): `devswarm_sup.duties_off` naming `recover` switches the recovery off, and a request is then `refused`.
// Discard triage (E3): every `.ok()` / `unwrap_or_default()` in this module is a deliberate keep, for these reasons:
// - an unreadable descriptor or verdict is an absent one: the first is a refusal, the second counts as no recoveries
use super::setting;
use crate::checks::git::util::Settings;
use crate::defaults;
use crate::dsact::runner::Runner;
use serde_json::{Value, json};
use std::path::Path;

/// A refused request: `code` names the way (a key of `devswarm_sup.act_recover_reasons`), `why` is the text the caller gets.
struct Refusal {
    code: &'static str,
    why: String,
}

fn refuse(code: &'static str, why: String) -> Refusal {
    Refusal { code, why }
}

fn ledger(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join(defaults::text("devswarm_sup.recover_ledger"))
}

fn already_ran(state_dir: &Path, request: &str) -> bool {
    std::fs::read_to_string(ledger(state_dir))
        .ok()
        .is_some_and(|t| t.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).any(|v| v.get("request").and_then(Value::as_str) == Some(request)))
}

/// Where a recovery runs: the home directory, the plugin root and the engine state directory.
pub struct Place<'a> {
    /// The home directory.
    pub home: &'a Path,
    /// The plugin root.
    pub root: &'a Path,
    /// The engine state directory (the request ledger lives there).
    pub state_dir: &'a Path,
}

/// Run the recovery request. Returns the report and the exit code (0 when the run was handled, whatever the outcome Node reports).
pub fn run(at: &Place, st: &Settings, runner: &dyn Runner, id: &str, request: &str, now: i64) -> (Value, i32) {
    let t0 = std::time::Instant::now();
    let (report, code) = match decide(at, st, runner, id, request, now) {
        Ok(done) => done,
        Err(r) => {
            let reason = defaults::raw("devswarm_sup.act_recover_reasons").str_field(r.code);
            super::tick::emit_act(at.home, defaults::text("devswarm_sup.recover_duty"), id, "refused", reason, t0.elapsed().as_millis() as u64, request);
            crate::obs::counters::account("duty", defaults::text("devswarm_sup.recover_duty"), crate::obs::counters::Outcome::Refused, 0, t0.elapsed().as_micros() as u64);
            (json!({"outcome": "refused", "why": r.why}), defaults::num("devswarm_wire.usage_exit") as i32)
        }
    };
    if report["outcome"] == "handled" {
        super::tick::emit_act(at.home, defaults::text("devswarm_sup.recover_duty"), id, "ran", "", t0.elapsed().as_millis() as u64, request);
        crate::obs::counters::account("duty", defaults::text("devswarm_sup.recover_duty"), crate::obs::counters::Outcome::Ok, 0, t0.elapsed().as_micros() as u64);
    }
    (report, code)
}

fn decide(at: &Place, st: &Settings, runner: &dyn Runner, id: &str, request: &str, now: i64) -> Result<(Value, i32), Refusal> {
    let Place { home, state_dir, .. } = *at;
    let caller = st.env.get(defaults::text("devswarm_act.caller_env")).cloned().unwrap_or_default();
    if !caller.is_empty() && caller != defaults::text("devswarm_act.interactive_caller") {
        return Err(refuse("caller", defaults::render("devswarm_sup.msg_recover_caller", &[("caller", &caller)])));
    }
    if id.is_empty() || !crate::meshw::idlock::is_safe_id(id) {
        return Err(refuse("id", defaults::text("devswarm_sup.msg_recover_id").into()));
    }
    if request.is_empty() {
        return Err(refuse("request", defaults::text("devswarm_sup.msg_recover_request").into()));
    }
    if already_ran(state_dir, request) {
        return Err(refuse("duplicate", defaults::render("devswarm_sup.msg_recover_duplicate", &[("request", &request)])));
    }
    let keys = defaults::raw("devswarm_sup.recover_descriptor_keys");
    let desc: Option<Value> = std::fs::read_to_string(super::root(home).join(defaults::text("mesh_write.dir_workspaces")).join(format!("{id}.json")))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let has = |k: &str| desc.as_ref().and_then(|d| d.get(keys.str_field(k))).and_then(Value::as_str).is_some_and(|s| !s.is_empty());
    if !has("worktree") || !has("session") {
        return Err(refuse("descriptor", defaults::render("devswarm_sup.msg_recover_descriptor", &[("id", &id)])));
    }
    let done = super::verdict::path(home, id)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("recoveries").and_then(Value::as_u64))
        .unwrap_or(0);
    let max = setting(st, "devswarm_sup.set_max_recoveries").as_u64().unwrap_or_default(); // keep: the shipped default always resolves; zero would refuse every recovery
    if done >= max {
        return Err(refuse("max", defaults::render("devswarm_sup.msg_recover_max", &[("id", &id), ("n", &done), ("max", &max)])));
    }
    // `devswarm_sup.duties_off` naming `recover` switches the on-demand recovery off: it is refused before anything is recorded or
    // touched (owner decision F: there is no Node rollback of the kill, and the engine's kill does not run while the switch is off)
    if defaults::list("devswarm_sup.duties_off").contains(&defaults::text("devswarm_sup.recover_duty")) {
        return Err(refuse("switched_off", defaults::text("devswarm_sup.msg_recover_off").into()));
    }
    // the request is recorded BEFORE the run: a run that dies half way is never repeated by the same request
    crate::dsact::exec::append_line(&ledger(state_dir), &json!({"ts": now, "request": request, "id": id}));
    let desc = desc.unwrap_or(Value::Null);
    let wt = desc.get(keys.str_field("worktree")).and_then(Value::as_str).unwrap_or_default();
    let session = desc.get(keys.str_field("session")).and_then(Value::as_str).unwrap_or_default();
    let sys = super::kill::Real {
        runner,
        claude: super::kill::find_claude(st.env.get(defaults::text("devswarm_ingest.env_path")).map(String::as_str), home),
        readiness_ms: defaults::num("devswarm_sup.kill_readiness_ms"),
    };
    let target = super::kill::find_target(&sys, home, wt, session, true);
    let grace = setting(st, "devswarm_sup.set_grace_sec").as_u64().unwrap_or_default().max(1) * 1000; // keep: the shipped default always resolves
    let job = super::kill::Job { home, id, descriptor: &desc, now, max_recoveries: max, grace_ms: grace, allow_interactive: true };
    let result = super::kill::recover(&sys, &job, &target);
    Ok((json!({"outcome": "handled", "id": id, "request": request, "target": target.to_json(wt), "result": result}), 0))
}
