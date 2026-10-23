//! `devswarm.js inbox count|read|peek-primary|ack|pull <id>`, ported from `scripts/devswarm-lib/inbox-cmd.js` `cmdInbox`.
//!
//! These verbs read the same union the tick and `read-primary` count through ([`union`], [`tick`]); only the output and the
//! effects differ. Each verb answers the single-partition, registered case and defers every other shape BEFORE anything is
//! written (a descriptor that does not exist, a flag the engine does not read, a mesh group, a retired redirect, a store that
//! needs a re-home). A deferral is a visible error (`exit_unable`): there is no Node to hand the verb to.
use crate::checks::guardkit::ojson::OVal;
use crate::defaults;
use crate::meshw::args::Args;
use crate::meshw::common::{self, Inv, Obj, n, s};
use crate::meshw::ident::{R, defer};
use crate::meshw::idlock::is_safe_id;
use crate::meshw::send::{Answer, Effect, resolve_mesh_target};
use crate::meshw::{cursors, ident};
use crate::meshw::{tick, union};

/// `inbox <verb> <id>`'s id, when the call has exactly that shape and no flag but the ones named (a flag the engine does not
/// read is Node's).
fn plain_id<'a>(a: &'a Args, allowed: &[&str]) -> R<&'a str> {
    if a.is_help() {
        return defer("help");
    }
    if a.positionals.len() != 3 {
        return defer("argv-shape");
    }
    let Some(id) = a.positionals.get(2).map(String::as_str).filter(|i| is_safe_id(i)) else { return defer("bad-id") };
    if a.flags.keys().any(|k| !allowed.contains(&k.as_str())) {
        return defer("flags");
    }
    Ok(id)
}

/// `inboxWindowRejection` for `inbox <sub> <id>`: `--since` / `--tail` on a verb that acks or reports on a contiguous prefix is
/// refused (printed, exit 2) before anything is read.
fn window(inv: &Inv, a: &Args, sub: &str) -> R<Option<Answer>> {
    if a.is_help() || a.positionals.len() != 3 {
        return Ok(None);
    }
    let Some(id) = a.positionals.get(2).map(String::as_str).filter(|i| is_safe_id(i)) else { return Ok(None) };
    crate::meshw::readprimary::window_refusal(inv, a, id, sub)
}

/// Node's `emitKnownWarning` for an `inbox` verb whose count is not known.
fn warn_unknown(verb: &str, id: &str) {
    eprintln!(
        "{}",
        defaults::render(
            "mesh_write.known_warning",
            &[
                ("label", &format!("{} {verb} {}", defaults::text("mesh_write.verb_inbox"), OVal::Str(id.to_string()).stringify())),
                ("reasons", &defaults::text("mesh_write.known_unknown"))
            ]
        )
    );
}

/// `inbox count <id>`: the unread count of a single registered partition, read only.
pub fn run_count(inv: &Inv, a: &Args) -> R<Answer> {
    if let Some(refused) = window(inv, a, defaults::text("mesh_write.verb_count"))? {
        return Ok(refused);
    }
    let id = plain_id(a, &[])?;
    let Some(desc) = ident::read_descriptor(&inv.home, id) else { return defer("no-descriptor") };
    let counted = tick::count_mail(inv, id, &desc, true, true)?;
    let stdout = tick::count_json(inv, id, &counted, defaults::text("mesh_write.verb_count"), false)?;
    if !counted.known {
        warn_unknown(defaults::text("mesh_write.verb_count"), id);
    }
    Ok(Answer { code: 0, stdout: format!("{stdout}\n"), effect: Effect::None })
}

/// `inbox read <id>`: the unread rows of both channels, read only (nothing is acked, and the result says so).
pub fn run_read(inv: &Inv, a: &Args) -> R<Answer> {
    if let Some(refused) = window(inv, a, defaults::text("mesh_write.verb_read"))? {
        return Ok(refused);
    }
    let id = plain_id(a, &[])?;
    let Some(desc) = ident::read_descriptor(&inv.home, id) else { return defer("no-descriptor") };
    let c = tick::count_mail(inv, id, &desc, true, true)?;
    let Some(total) = c.total else { return defer("unknown-count-json") };
    if c.store_unread > defaults::num("mesh_write.inbox_read_limit") as usize {
        return defer("read-cap");
    }
    let mut o = Obj::default();
    o.put("ok", OVal::Bool(true))
        .put("action", s(defaults::text("mesh_write.action_read")))
        .put("id", s(id))
        .put("repoKey", s(&c.repo_key))
        .put("storePath", OVal::Str(union::store_dir(&inv.home, &c.repo_key).to_string_lossy().into_owned()))
        .put("cwd", s(&inv.cwd))
        .put("meshPartitionIds", OVal::Arr(vec![s(id)]))
        .put("lines", OVal::Arr(c.nd_lines.iter().map(|l| s(l)).collect()))
        .put("meshMessages", OVal::Arr(c.store_rows.iter().map(crate::meshw::readprimary::store_row).collect()))
        .put("unreadTotal", n(c.unread as f64))
        .put("unreadNdjson", n(c.nd_unread as f64))
        .put("unreadStore", n(c.store_unread as f64))
        .put("cursorNdjson", n(c.nd_cursor))
        .put("cursorStore", n(c.store_cursor))
        .put("total", n(total as f64))
        .put("known", OVal::Bool(c.known))
        .put("storeUnavailable", OVal::Bool(false))
        .put("storeUnavailableReason", OVal::Null)
        .put("meshGroupUnresolved", OVal::Bool(false))
        .put("meshGroupError", OVal::Null)
        .put("totalsPartial", OVal::Bool(false));
    if c.unread > 0 {
        o.put("cursorAdvanced", OVal::Bool(false)).put(
            "ackHint",
            OVal::Str(defaults::render("mesh_write.read_ack_hint", &[("total", &c.unread), ("nd", &c.nd_unread), ("store", &c.store_unread), ("id", &id)])),
        );
    }
    o.put("count", n(c.unread as f64)).put("cursor", n(c.nd_cursor)).put("storeCursor", n(c.store_cursor));
    if !c.known {
        warn_unknown(defaults::text("mesh_write.action_read"), id);
    }
    Ok(Answer { code: 0, stdout: format!("{}\n", o.done().stringify()), effect: Effect::None })
}

/// `inbox ack <id>`: consume everything this read saw, on both channels, and say where the cursors stand.
///
/// The NDJSON side moves to the position the read snapshot ended at (never the live file's tail), the store side to the highest
/// store position that was delivered, and only a caller that owns the id moves the store side (a caller whose identity resolves to
/// a DIFFERENT registered row is refused, which is Node's). Every refusal and every move the engine does not apply is decided
/// before the first write.
pub fn run_ack(inv: &Inv, a: &Args) -> R<Answer> {
    if let Some(refused) = window(inv, a, defaults::text("mesh_write.verb_ack"))? {
        return Ok(refused);
    }
    let id = plain_id(a, &[defaults::text("mesh_write.flag_ack_as_owner")])?;
    if a.flags.get(defaults::text("mesh_write.flag_ack_as_owner")).is_some_and(|v| v.iter().any(|x| *x != crate::meshw::args::FlagVal::True)) {
        return defer("ack-as-owner-value");
    }
    let ack_as_owner = a.has(defaults::text("mesh_write.flag_ack_as_owner"));
    common::seat_check(inv)?;
    let Some(desc) = ident::read_descriptor(&inv.home, id) else { return defer("no-descriptor") };
    let Some(inbox) = union::path_field(&desc, defaults::text("mesh_write.field_inbox_path"))? else { return defer("no-inbox-path") };
    let Some(cursor_file) = union::path_field(&desc, defaults::text("mesh_write.field_cursor_path"))? else { return defer("no-cursor-path") };
    let c = tick::count_mail(inv, id, &desc, true, true)?;
    let st = common::open_store(inv, &c.repo_key)?;
    let rows = ident::rows_of(&st.reader().roster().map_err(|e| ident::Defer(format!("registry:{e}")))?);
    // the ownership gate: a caller that is a DIFFERENT registered row is refused (Node's); one that resolves to nothing acks the
    // NDJSON side and leaves the store side alone
    let mut owns_store = true;
    if !ack_as_owner {
        let caller = ident::caller_identity_detailed(&inv.env, &inv.cwd)?;
        let own_entry = resolve_mesh_target(inv, None, &rows, Some(caller.identity.as_str()))?;
        let owns = caller.identity == id
            || own_entry.as_ref().is_some_and(|e| e.id == id)
            || ident::declared_self_id(&inv.env, &inv.cwd, &rows)?.as_deref() == Some(id);
        if !owns && own_entry.is_some() {
            return defer("not-owner");
        }
        owns_store = owns;
    }
    if c.store_unread > defaults::num("mesh_write.inbox_read_limit") as usize {
        return defer("read-cap");
    }
    if cursors::needs_import(&cursors::rows_of(&st, id).map_err(|e| ident::Defer(format!("cursor-rows:{e}")))?) {
        return defer("cursor-import");
    }
    if owns_store {
        crate::meshw::summary::check(&st, inv, None)?;
    }
    // ---- writes ----
    let reader = cursors::reader_key(tick::reader_nonce_cached(&inv.home).as_deref());
    let nonce = reader.as_deref().map(cursors::short_nonce);
    let mut procs = cursors::Procs::default();
    // the NDJSON side: the position the read ended at, never past the inbox as it is now
    let lines_now = union::non_empty_lines(&inbox)?.map_or(0, |l| l.len()) as f64;
    let nd_to = c.nd_target.max(0.0).floor().min(lines_now);
    let nd = cursors::ack_for(&st, &inv.home, id, defaults::text("mesh_write.cursor_ns_nd"), reader.as_deref(), nd_to, &mut procs);
    if nd.ok {
        crate::meshw::mark_committed();
    }
    cursors::log_cursor_write(
        &inv.home,
        &cursors::LogRec {
            id,
            caller_id: Some(id),
            ns: defaults::text("mesh_write.log_ns_nd"),
            from: nd.from,
            to: if nd.ok { nd.own.map(|x| x as f64) } else { Some(nd_to) },
            delivered: None,
            nonce: nonce.clone(),
            gate: defaults::text("mesh_write.gate_min_floor"),
            verb: defaults::text("mesh_write.verb_log_ack_all"),
            cwd: None, // absent for the CLI: Node's `ctx.cwd` is unset there
            ok: nd.ok,
            err: if nd.ok { None } else { nd.error.as_deref() },
            repo_key: Some(&c.repo_key),
        },
    );
    if !nd.ok {
        return defer("nd-ack-failed");
    }
    // the legacy projection of the NDJSON floor: the descriptor's cursor file, upward only, best effort
    if let Some(fl) = nd.floor.filter(|f| *f > 0)
        && cursors::read_cursor(std::path::Path::new(&cursor_file)) < fl as f64
    {
        crate::discard::harmless(cursors::ack_to(std::path::Path::new(&cursor_file), fl as f64)); // keep: the projection is best-effort in Node
    }
    let mut failures: Vec<OVal> = Vec::new();
    if owns_store {
        let max_index = c.store_rows.iter().filter_map(|r| r["index"].as_f64()).fold(None, |m: Option<f64>, i| Some(m.map_or(i, |x| x.max(i))));
        let to = max_index.map_or(c.store_cursor, |m| c.store_cursor.max(m));
        let r = cursors::ack_for(&st, &inv.home, id, defaults::text("mesh_write.ns_store"), reader.as_deref(), to, &mut procs);
        cursors::log_cursor_write(
            &inv.home,
            &cursors::LogRec {
                id,
                caller_id: Some(id),
                ns: defaults::text("mesh_write.log_ns_store"),
                from: r.from,
                to: if r.ok { r.own.map(|x| x as f64) } else { Some(to) },
                delivered: Some(c.store_rows.len() as f64),
                nonce,
                gate: defaults::text("mesh_write.gate_owner"),
                verb: defaults::text("mesh_write.verb_log_ack"),
                cwd: None, // absent for the CLI: Node's `ctx.cwd` is unset there
                ok: r.ok,
                err: if r.ok { None } else { r.error.as_deref() },
                repo_key: Some(&c.repo_key),
            },
        );
        if let Some(e) = &r.error {
            let mut f = Obj::default();
            f.put("partitionId", s(id)).put("channel", s(defaults::text("mesh_write.channel_store_cursor"))).put("error", s(e));
            failures.push(f.done());
        }
        if let Some(why) = crate::meshw::summary::derive_after_write(&st, inv, &c.repo_key) {
            crate::meshw::log_summary_failure(defaults::text("mesh_write.verb_ack"), &why);
        }
    }
    let mut out = Obj::default();
    out.put("ok", OVal::Bool(true))
        .put("action", s(defaults::text("mesh_write.verb_ack")))
        .put("id", s(id))
        .put("cursor", n(nd.own.map_or(0.0, |x| x as f64)))
        .put("total", n(union::non_empty_lines(&inbox)?.map_or(0, |l| l.len()) as f64))
        .put("known", OVal::Bool(c.known))
        .put("storeUnavailable", OVal::Bool(false))
        .put("storeUnavailableReason", OVal::Null);
    if !failures.is_empty() {
        out.put("cursorWriteFailures", OVal::Arr(failures)).put("cursorPersisted", OVal::Bool(false));
    }
    if !c.known {
        warn_unknown(defaults::text("mesh_write.verb_ack"), id);
    }
    Ok(Answer { code: 0, stdout: format!("{}\n", out.done().stringify()), effect: Effect::None })
}

/// `inbox pull <id>`: drain the native queue of a workspace into its durable inbox (the pull a `--child` tick runs first), and
/// say what was imported. A pull that did not succeed prints its result and exits 2, and is logged like Node's `logVerbOutcome`.
pub fn run_pull(inv: &Inv, a: &Args) -> R<Answer> {
    if let Some(mut refused) = window(inv, a, defaults::text("mesh_write.verb_pull"))? {
        // `pull` is the one inbox verb wrapped in `withSelfHeal`: the refusal carries the heal's fields too
        if let Some(mut o) = OVal::parse(refused.stdout.trim_end()) {
            for (k, v) in common::self_heal(inv)? {
                o.set(&k, v);
            }
            refused.stdout = format!("{}\n", o.stringify());
        }
        return Ok(refused);
    }
    let id = plain_id(a, &[])?;
    crate::meshw::clog::ready(inv)?;
    let found = ident::read_descriptor(&inv.home, id);
    let plan = crate::meshw::pull::prepare(inv, id, found.as_ref())?;
    // the count is not printed, so nothing here depends on it: the plan only settles the lock, the delivery log and the native count
    let plan = plan.finish(inv, 0, true)?;
    // `withSelfHeal` adds its fields to what `pull` returns (the plan already refused a heal that would spawn anything)
    let heal = common::self_heal(inv)?;
    let repo_key = ident::resolve_context(&inv.cwd, true)?.repo_key;
    let out = crate::meshw::pull::execute(inv, plan)?;
    let r = out.res;
    let mut o = Obj::default();
    o.put("ok", OVal::Bool(r.ok))
        .put("action", s(defaults::text("mesh_write.verb_pull")))
        .put("id", s(id))
        .put("imported", n(r.imported as f64))
        .put("duplicate", n(r.duplicate as f64))
        .put("nativeCount", n(r.native_count))
        .put("locked", OVal::Bool(r.locked))
        .put("lost", n(r.lost));
    if r.native_timeout {
        o.put("nativeTimeout", OVal::Bool(true));
    }
    if let Some(e) = &r.error {
        o.put("error", s(e));
    }
    for (k, v) in heal {
        o.put(&k, v);
    }
    if !r.ok {
        let msg = r.error.clone().unwrap_or_else(|| defaults::text("mesh_write.msg_verb_not_ok").to_string());
        crate::meshw::clog::refusal(inv, defaults::text("mesh_write.log_op_inbox_pull"), repo_key.as_deref(), Some(id), &msg, None);
    }
    Ok(Answer { code: if r.ok { 0 } else { 2 }, stdout: format!("{}\n", o.done().stringify()), effect: Effect::None })
}
