//! `devswarm.js inbox count|read|peek-primary|ack|pull <id>`, ported from `scripts/devswarm-lib/inbox-cmd.js` `cmdInbox`.
//!
//! These verbs read the same union the tick and `read-primary` count through ([`union`], [`tick`]); only the output and the
//! effects differ. Each verb answers the single-partition, registered case and defers every other shape BEFORE anything is
//! written (a descriptor that does not exist, a flag the engine does not read, a mesh group, a retired redirect, a store that
//! needs a re-home). A deferral is a visible error (`exit_unable`): there is no Node to hand the verb to.
use crate::checks::guardkit::ojson::OVal;
use crate::defaults;
use crate::meshw::args::Args;
use crate::meshw::common::{Inv, Obj, n, s};
use crate::meshw::ident::{R, defer};
use crate::meshw::idlock::is_safe_id;
use crate::meshw::send::{Answer, Effect};
use crate::meshw::{tick, union};

/// `inbox <verb> <id>`'s id, when the call has exactly that shape and no flag (a flag the engine does not read is Node's).
fn plain_id<'a>(a: &'a Args) -> R<&'a str> {
    if a.is_help() {
        return defer("help");
    }
    if a.positionals.len() != 3 {
        return defer("argv-shape");
    }
    let Some(id) = a.positionals.get(2).map(String::as_str).filter(|i| is_safe_id(i)) else { return defer("bad-id") };
    if !a.flags.is_empty() {
        return defer("flags");
    }
    Ok(id)
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
    let id = plain_id(a)?;
    let Some(desc) = crate::meshw::ident::read_descriptor(&inv.home, id) else { return defer("no-descriptor") };
    let counted = tick::count_mail(inv, id, &desc, true, true)?;
    let stdout = tick::count_json(inv, id, &counted, defaults::text("mesh_write.verb_count"), false)?;
    if !counted.known {
        warn_unknown(defaults::text("mesh_write.verb_count"), id);
    }
    Ok(Answer { code: 0, stdout: format!("{stdout}\n"), effect: Effect::None })
}

/// `inbox read <id>`: the unread rows of both channels, read only (nothing is acked, and the result says so).
pub fn run_read(inv: &Inv, a: &Args) -> R<Answer> {
    let id = plain_id(a)?;
    let Some(desc) = crate::meshw::ident::read_descriptor(&inv.home, id) else { return defer("no-descriptor") };
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
