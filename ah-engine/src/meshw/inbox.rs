//! `devswarm.js inbox ack-primary <id> --receipt <rid> [--ack-as-owner]`, ported from `scripts/devswarm-lib/inbox-cmd.js`
//! `cmdInboxAckPrimary` and `cursors.js` `applyReadAckOps`.
//!
//! The ack applies the cursor moves a read receipt (written by `inbox read-primary`, which stays in Node) recorded, after
//! the receipt, the reader and the ownership checks pass. It writes: the caller's `reader_cursors` rows and the floor
//! (one transaction per op), the legacy shared cursor file and the `cursors` row, the cursor-write journal, the summary
//! projection, the receipt's `ackedAt`, and a transient drain marker the parent gate honours while the ack runs.
//!
//! The engine answers only the successful path it can reproduce exactly. Every refusal Node prints (unknown, expired,
//! foreign or mismatched receipt, a foreign project, a caller that does not own the id), every receipt op the engine does
//! not apply (`sibling`, `nd`), a re-home, a partition whose legacy cursors still need Node's one-time import, and a
//! store the engine will not open are [`Defer`]s decided BEFORE the first write; Node then runs the verb. After the first
//! cursor commit nothing defers: a later failure is reported in the result (`cursorWriteFailures`) exactly as Node
//! reports it, never by running the verb again.
use crate::checks::guardkit::ojson::OVal;
use crate::defaults;
use crate::meshw::args::Args;
use crate::meshw::common::{self, Inv, Obj, n, s};
use crate::meshw::cursors::{self, LogRec, Procs};
use crate::meshw::ident::{self, Defer, R, defer};
use crate::meshw::idlock::{devswarm_root, is_safe_id};
use crate::meshw::send::{Answer, Effect, mesh_candidates, rehome_is_noop, resolve_mesh_target};
use crate::meshw::union;
use std::path::{Path, PathBuf};

/// A receipt op the engine applies: a move of the caller's own partition cursor, or of a sibling partition of its mesh group.
enum AckOp {
    Own { partition: String, target: f64, delivered: Option<f64> },
    /// `not_ackable` is decided again at ack time (`revalidate`): a sibling that is live must not have its cursor moved by this caller.
    Sibling { partition: String, ack_target: f64, seen_target: f64, delivered: Option<f64>, not_ackable: bool },
}

impl AckOp {
    fn partition(&self) -> &str {
        match self {
            AckOp::Own { partition, .. } | AckOp::Sibling { partition, .. } => partition,
        }
    }
}

/// `readReceiptDir(home, id)`.
fn receipt_dir(home: &Path, id: &str) -> PathBuf {
    devswarm_root(home).join(defaults::text("mesh_write.dir_read_receipts")).join(id)
}

fn receipt_id_ok(rid: &str) -> bool {
    let p = defaults::text("mesh_write.receipt_id_prefix");
    rid.strip_prefix(p).is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
}

/// `readReadReceipt(home, id, rid, { dirId })`: the receipt and the directory it came from; the family's canonical
/// directory is tried first, the literal id's second.
fn read_receipt(home: &Path, id: &str, rid: &str, dir_id: &str) -> Option<(OVal, String)> {
    if !is_safe_id(id) || !receipt_id_ok(rid) {
        return None;
    }
    let mut dirs = Vec::new();
    if dir_id != id && is_safe_id(dir_id) {
        dirs.push(dir_id);
    }
    dirs.push(id);
    for d in dirs {
        let file = receipt_dir(home, d).join(format!("{rid}{}", defaults::text("mesh_write.json_suffix")));
        let Some(bytes) = ident::read_or_skip(&file, defaults::text("mesh_write.site_receipt")) else { continue };
        if let Some(rec @ OVal::Obj(_)) = OVal::parse(&String::from_utf8_lossy(&bytes))
            && matches!(rec.get("ops"), Some(OVal::Arr(_)))
        {
            return Some((rec, d.to_string()));
        }
    }
    None
}

/// `descriptorRegisteredRepoKey(desc, id)` (`devswarm-repokey.js` `registeredRepoKey`): the project the id is registered
/// under, from its worktree when that still resolves, else the key the descriptor persisted (never the legacy hash bucket).
pub(crate) fn registered_repo_key(desc: &OVal, id: &str) -> R<Option<String>> {
    let text = |k: &str| match desc.get(k) {
        Some(OVal::Str(x)) if !x.is_empty() => Some(x.clone()),
        _ => None,
    };
    if let Some(wt) = text(defaults::text("mesh_write.field_worktree_path"))
        && let Some(fresh) = ident::repo_key_for_worktree(&wt)?
    {
        return Ok(Some(fresh));
    }
    let persisted = text(defaults::text("mesh_write.field_repo_key")).or_else(|| text(defaults::text("mesh_write.field_owner_key")));
    Ok(persisted.filter(|p| *p != crate::meshw::send::hash_from_workspace_id(id)))
}

/// The ops of a receipt: the `own` and `sibling` moves to apply. An `nd` op (the NDJSON inbox) is Node's; so is an op whose fields the engine cannot read exactly like JavaScript does. An unknown kind is
/// skipped, as Node skips it.
fn own_ops(rec: &OVal, id: &str) -> R<Vec<AckOp>> {
    let Some(OVal::Arr(ops)) = rec.get("ops") else { return defer("receipt-ops") };
    let mut out = Vec::new();
    for op in ops {
        if !matches!(op, OVal::Obj(_) | OVal::Arr(_)) {
            continue;
        }
        match op.get("k") {
            Some(OVal::Str(k)) if k == defaults::text("mesh_write.op_own") => {
                let partition = match op.get("partition") {
                    None | Some(OVal::Null) => id.to_string(),
                    Some(OVal::Str(p)) => p.clone(),
                    Some(_) => return defer("receipt-partition"),
                };
                let target = match op.get("target") {
                    Some(OVal::Num(t)) if t.is_finite() => *t,
                    _ => return defer("receipt-target"),
                };
                let delivered = match op.get("delivered") {
                    Some(OVal::Num(d)) if d.is_finite() => Some(*d),
                    _ => None,
                };
                out.push(AckOp::Own { partition, target, delivered });
            }
            Some(OVal::Str(k)) if k == defaults::text("mesh_write.op_sibling") => {
                let partition = match op.get("partition") {
                    Some(OVal::Str(p)) if is_safe_id(p) => p.clone(),
                    _ => return defer("receipt-partition"),
                };
                let ack_target = match op.get("ackTarget") {
                    Some(OVal::Num(t)) if t.is_finite() => *t,
                    _ => return defer("receipt-target"),
                };
                // `op.seenTarget > 0` is false for anything that is not a positive number
                let seen_target = match op.get("seenTarget") {
                    Some(OVal::Num(t)) if t.is_finite() => *t,
                    _ => 0.0,
                };
                let delivered = match op.get("delivered") {
                    Some(OVal::Num(d)) if d.is_finite() => Some(*d),
                    _ => None,
                };
                out.push(AckOp::Sibling { partition, ack_target, seen_target, delivered, not_ackable: false });
            }
            Some(OVal::Str(k)) if k == defaults::text("mesh_write.op_nd") => return defer("receipt-op-kind"),
            _ => {}
        }
    }
    Ok(out)
}

/// `markDrainStart(home, id, { sessionId, count: 0, now })` (best effort: a failed write only means the gate is not
/// silenced this turn).
fn mark_drain(inv: &Inv, id: &str) {
    let dir = devswarm_root(&inv.home).join(defaults::text("mesh_write.dir_drain"));
    let p = dir.join(format!("{id}{}", defaults::text("mesh_write.json_suffix")));
    let mut m = Obj::default();
    m.put("startedAt", n(inv.now as f64))
        .put("sessionId", inv.env.get(defaults::text("mesh_write.env_session_id")).filter(|x| !x.is_empty()).map_or(OVal::Null, |x| OVal::Str(x.clone())))
        .put("pid", n(f64::from(std::process::id())))
        .put("count", n(0.0));
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        let mut tmp = p.as_os_str().to_os_string();
        tmp.push(format!(".{}.{}{}", std::process::id(), common::now_ms(), defaults::text("mesh_write.tmp_suffix")));
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, m.done().stringify())?;
        std::fs::rename(&tmp, &p)
    };
    crate::discard::harmless(write()); // keep: the marker is advisory (Node returns false)
}

/// `clearDrainMarker(home, id)`.
fn clear_drain(inv: &Inv, id: &str) {
    let p = devswarm_root(&inv.home).join(defaults::text("mesh_write.dir_drain")).join(format!("{id}{}", defaults::text("mesh_write.json_suffix")));
    crate::discard::harmless(std::fs::remove_file(p)); // keep: an absent marker is already cleared
}

/// What an ack has settled before it writes anything: the receipt (read from disk, or the one a read-primary is about to
/// file), the moves it records and the store they go to.
pub(crate) struct AckPlan {
    st: crate::meshw::store::MeshStore,
    id: String,
    rid: String,
    rec: OVal,
    found_dir: String,
    ops: Vec<AckOp>,
    /// Partitions with no floor rows yet: `ackFor` imports their legacy cursor files first, inside the same move.
    imports: Vec<String>,
    reader: Option<String>,
    repo_key: String,
}

/// Run `inbox ack-primary` (argv already parsed; `positionals` start with `inbox`, `ack-primary`).
pub fn run(inv: &Inv, a: &Args) -> R<Answer> {
    if a.is_help() {
        return defer("help");
    }
    let Some(id) = a.positionals.get(2).map(String::as_str).filter(|i| is_safe_id(i)) else { return defer("bad-id") };
    // inboxWindowRejection: a window flag on an acking verb is Node's refusal
    if a.has(defaults::text("mesh_write.flag_tail")) || a.has(defaults::text("mesh_write.flag_since")) {
        return defer("window-flags");
    }
    common::seat_check(inv)?;
    let Some(rid) = a.one(defaults::text("mesh_write.flag_receipt")).filter(|r| receipt_id_ok(r)) else { return defer("receipt-argument") };
    let plan = plan_ack(inv, id, rid, a.has(defaults::text("mesh_write.flag_ack_as_owner")), None)?;
    // ---- commit: everything below writes, nothing below defers ----
    let out = apply_ack(inv, plan);
    Ok(Answer { code: 0, stdout: format!("{}\n", out.done().stringify()), effect: Effect::None })
}

/// Everything `cmdInboxAckPrimary` checks before its first write: every refusal and every move the engine does not apply
/// is a [`Defer`] here. `fresh` is the receipt a `read-primary --ack-after-print` is about to file (not on disk yet).
pub(crate) fn plan_ack(inv: &Inv, id: &str, rid: &str, ack_as_owner: bool, fresh: Option<OVal>) -> R<AckPlan> {
    let home = &inv.home;
    // resolveWorkspaceStoreForRead: the caller's project, the id's registered project, the re-home trigger
    let Some(repo_key) = ident::resolve_context(&inv.cwd, true)?.repo_key else { return defer("no-project") };
    let desc = ident::read_descriptor(home, id);
    if let Some(d) = &desc
        && let Some(reg) = registered_repo_key(d, id)?
        && reg != repo_key
    {
        return defer("project-context-mismatch");
    }
    rehome_is_noop(inv, id)?;
    let st = common::open_store(inv, &repo_key)?;
    let rows = ident::rows_of(&st.reader().roster().map_err(|e| Defer(format!("registry:{e}")))?);
    // canonicalReceiptId: the lowest id of the worktree's identity family, else the literal id
    let mut dir_id = id.to_string();
    if let Some(d) = &desc {
        match d.get(defaults::text("mesh_write.field_worktree_path")) {
            Some(OVal::Str(wt)) if !wt.is_empty() => {
                if let Some(mesh) = ident::canonical_mesh_id(wt)? {
                    let mut ids: Vec<String> = mesh_candidates(&rows, Some(mesh.as_str()))?.into_iter().map(|r| r.id).collect();
                    ids.sort();
                    ids.dedup();
                    if ids.len() > 1 && ids.iter().any(|x| x == id) {
                        dir_id = ids[0].clone();
                    }
                }
            }
            None | Some(OVal::Null | OVal::Str(_)) => {}
            Some(OVal::Bool(false)) => {}
            Some(_) => return defer("descriptor-worktree"),
        }
    }
    let Some((rec, found_dir)) = (match fresh {
        Some(rec) => Some((rec, id.to_string())),
        None => read_receipt(home, id, rid, &dir_id),
    }) else {
        return defer("unknown-receipt");
    };
    if !matches!(rec.get("id"), Some(OVal::Str(r)) if r == id) {
        return defer("receipt-owner");
    }
    let reader = cursors::reader_key(ident::reader_nonce(home).as_deref());
    let rec_reader = match rec.get("reader") {
        None | Some(OVal::Null | OVal::Bool(false)) => None,
        Some(OVal::Str(r)) if r.is_empty() => None,
        Some(OVal::Str(r)) => Some(r.as_str()),
        Some(_) => return defer("receipt-reader"),
    };
    if rec_reader != reader.as_deref() {
        return defer("receipt-reader");
    }
    match rec.get("createdAt") {
        Some(OVal::Num(c)) if c.is_finite() && (inv.now as f64) - c <= defaults::num("mesh_write.receipt_ttl_ms") as f64 => {}
        _ => return defer("receipt-expired"),
    }
    if !ack_as_owner {
        let caller = ident::caller_identity_detailed(&inv.env, &inv.cwd)?;
        let own_entry = resolve_mesh_target(inv, None, &rows, Some(caller.identity.as_str()))?;
        let owns = caller.identity == id
            || own_entry.as_ref().is_some_and(|e| e.id == id)
            || ident::declared_self_id(&inv.env, &inv.cwd, &rows)?.as_deref() == Some(id);
        if !owns {
            return defer("not-owner");
        }
    }
    let mut ops = own_ops(&rec, id)?;
    let mut imports: Vec<String> = Vec::new();
    for op in &mut ops {
        // a sibling is re-judged now (`revalidate`): the receipt's verdict was the one at read time
        if let AckOp::Sibling { partition, not_ackable, .. } = op {
            *not_ackable = crate::meshw::meshgroup::ack_gate(inv, &rows, id, partition)?;
            if *not_ackable {
                continue;
            }
        }
        let cur = cursors::rows_of(&st, op.partition()).map_err(|e| Defer(format!("cursor-rows:{e}")))?;
        if cursors::needs_import(&cur) {
            // the import itself must be one the engine can do exactly (a dry run says so); else the whole verb is Node's
            let h = crate::migrate::readercur::Handle::Ro(
                crate::mesh::MeshReader::open(&union::store_dir(home, &repo_key).join(defaults::text("mesh_write.store_file"))).map_err(|e| Defer(format!("store-open:{e}")))?,
            );
            let mut cx = crate::migrate::readercur::Cx::new(&home.to_string_lossy());
            crate::migrate::readercur::import_legacy(&mut cx, &h, op.partition(), None, true).map_err(|e| Defer(format!("cursor-import:{e}")))?;
            if cx.deferred {
                return defer("cursor-import");
            }
            if !imports.iter().any(|p| p == op.partition()) {
                imports.push(op.partition().to_string());
            }
        }
    }
    // the summary refresh after the write must be one the engine can do (else Node runs the whole verb)
    crate::meshw::summary::check(&st, inv, None)?;
    Ok(AckPlan { st, id: id.to_string(), rid: rid.to_string(), rec, found_dir, ops, imports, reader, repo_key })
}

/// The writes of `cmdInboxAckPrimary` and its result: nothing defers from here, a failure is reported in the result.
pub(crate) fn apply_ack(inv: &Inv, plan: AckPlan) -> Obj {
    let AckPlan { st, id, rid, rec, found_dir, ops, imports, reader, repo_key } = plan;
    let (id, rid, home) = (id.as_str(), rid.as_str(), &inv.home);
    mark_drain(inv, id);
    let mut procs = Procs::default();
    let mut failures: Vec<OVal> = Vec::new();
    let mut acked: Option<f64> = None;
    let ns = defaults::text("mesh_write.ns_store");
    let nonce = reader.as_deref().map(cursors::short_nonce);
    // `ackFor` imports a partition's legacy cursor files into floor rows the first time it moves it
    let mut import_failed: Vec<String> = Vec::new();
    for partition in &imports {
        let done = (|| -> Result<(), String> {
            let h = crate::migrate::readercur::Handle::Rw(
                crate::meshw::store::MeshStore::open(&union::store_dir(home, &repo_key).join(defaults::text("mesh_write.store_file"))).map_err(|e| e.to_string())?,
            );
            let mut cx = crate::migrate::readercur::Cx::new(&home.to_string_lossy());
            crate::migrate::readercur::import_legacy(&mut cx, &h, partition, None, false).map(|_| ())
        })();
        if let Err(e) = done {
            import_failed.push(partition.clone());
            failures.push(failure(partition, defaults::text("mesh_write.channel_store_cursor"), &e));
        }
    }
    for op in &ops {
        if import_failed.iter().any(|p| p == op.partition()) {
            continue;
        }
        match op {
            AckOp::Own { partition, target, delivered } => {
                let r = cursors::ack_for(&st, home, partition, ns, reader.as_deref(), *target, &mut procs);
                if r.ok {
                    crate::meshw::mark_committed();
                }
                log_move(home, id, partition, &r, *target, *delivered, nonce.clone(), defaults::text("mesh_write.gate_owner"), &repo_key);
                if let Some(e) = &r.error {
                    failures.push(failure(partition, defaults::text("mesh_write.channel_store_cursor"), e));
                }
                // `Number.isFinite(committed.instance) ? committed.instance : Math.max(committed.floor || 0, op.target)`
                acked = Some(match (reader.is_some(), r.own) {
                    (true, Some(own)) if r.ok => own as f64,
                    _ => (r.floor.unwrap_or(0) as f64).max(*target),
                });
            }
            AckOp::Sibling { partition, ack_target, seen_target, delivered, not_ackable } => {
                if *not_ackable {
                    // a live sibling is not this caller's to ack: only the caller-scoped watermark moves, so the next read starts after it
                    if *seen_target > 0.0 && !with_watermark_lock(inv, id, partition, || crate::meshw::meshgroup::write_seen_cursor(inv, id, partition, *seen_target)) {
                        failures.push(failure(partition, defaults::text("mesh_write.channel_sibling_seen"), defaults::text("mesh_write.msg_seen_watermark_failed")));
                    }
                    continue;
                }
                let r = cursors::ack_for(&st, home, partition, ns, reader.as_deref(), *ack_target, &mut procs);
                if r.ok {
                    crate::meshw::mark_committed();
                }
                log_move(home, id, partition, &r, *ack_target, *delivered, nonce.clone(), defaults::text("mesh_write.gate_sibling"), &repo_key);
                if let Some(e) = &r.error {
                    failures.push(failure(partition, defaults::text("mesh_write.channel_sibling_store"), e));
                    continue;
                }
                // the watermark this ack covers is no longer needed
                with_watermark_lock(inv, id, partition, || {
                    if *ack_target >= crate::meshw::meshgroup::seen_cursor(inv, id, partition) {
                        crate::meshw::meshgroup::remove_seen_cursor(inv, id, partition);
                    }
                    true
                });
            }
        }
    }
    if let Some(why) = crate::meshw::summary::derive_after_write(&st, inv, &repo_key) {
        crate::meshw::log_summary_failure(defaults::text("mesh_write.verb_ack_primary"), &why);
    }
    clear_drain(inv, id);
    let already = !matches!(rec.get("ackedAt"), None | Some(OVal::Null));
    if failures.is_empty() && !already {
        let mut stamped = rec.clone();
        stamped.set("ackedAt", n(inv.now as f64));
        let file = receipt_dir(home, &found_dir).join(format!("{rid}{}", defaults::text("mesh_write.json_suffix")));
        let mut tmp = file.as_os_str().to_os_string();
        tmp.push(defaults::text("mesh_write.tmp_suffix"));
        let tmp = PathBuf::from(tmp);
        let write = || -> std::io::Result<()> {
            std::fs::write(&tmp, stamped.stringify())?;
            std::fs::rename(&tmp, &file)
        };
        crate::discard::harmless(write()); // keep: the cursors moved; the ackedAt stamp is informational (Node's catch)
    }
    let mut out = Obj::default();
    out.put("ok", OVal::Bool(true)).put("action", s(defaults::text("mesh_write.action_ack_primary"))).put("id", s(id)).put("readReceiptId", s(rid));
    if let Some(x) = acked {
        out.put("acked", n(x));
    }
    out.put("alreadyAcked", OVal::Bool(already)).put(
        "messages",
        match rec.get("hashes") {
            Some(OVal::Arr(h)) => n(h.len() as f64),
            _ => OVal::Null,
        },
    );
    if !failures.is_empty() {
        out.put("cursorWriteFailures", OVal::Arr(failures)).put("cursorPersisted", OVal::Bool(false));
    }
    out
}

/// One `cursorWriteFailures` entry.
fn failure(partition: &str, channel: &str, error: &str) -> OVal {
    let mut f = Obj::default();
    f.put("partitionId", s(partition)).put("channel", s(channel)).put("error", s(error));
    f.done()
}

/// The cursor-write journal line of one move.
#[allow(clippy::too_many_arguments)]
fn log_move(home: &Path, caller: &str, partition: &str, r: &cursors::Acked, target: f64, delivered: Option<f64>, nonce: Option<String>, gate: &str, repo_key: &str) {
    cursors::log_cursor_write(
        home,
        &LogRec {
            id: partition,
            caller_id: Some(caller),
            ns: defaults::text("mesh_write.log_ns_store"),
            from: r.from,
            to: if r.ok { r.own.map(|x| x as f64) } else { Some(target) },
            delivered,
            nonce,
            gate,
            verb: defaults::text("mesh_write.verb_ack_primary"),
            cwd: None,
            ok: r.ok,
            err: r.error.as_deref(),
            repo_key: Some(repo_key),
        },
    );
}

/// `withWatermarkLock(caller, sibling, home, fn)`: `fn` under the lock of the watermark file; contended or unlockable, it runs unlocked
/// (a watermark is never dropped for want of a lock).
fn with_watermark_lock(inv: &Inv, caller: &str, sibling: &str, f: impl FnOnce() -> bool) -> bool {
    let sep = defaults::text("devswarm_recon.seen_sep");
    let safe = |x: &str| is_safe_id(x) && !x.contains(sep);
    if !(safe(caller) && safe(sibling)) {
        return f();
    }
    match crate::meshw::idlock::acquire(&inv.home, &format!("{caller}{sep}{sibling}")) {
        Some(lock) => {
            let out = f();
            lock.release();
            out
        }
        None => f(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meshw::ident::fifo_kit::*;

    #[test]
    fn a_fifo_receipt_is_no_receipt_and_does_not_block() {
        let home = scratch("receipt");
        let dir = receipt_dir(&home, "w1");
        std::fs::create_dir_all(&dir).unwrap();
        let rid = format!("{}abc", defaults::text("mesh_write.receipt_id_prefix"));
        fifo(&dir.join(format!("{rid}{}", defaults::text("mesh_write.json_suffix"))));
        let h = home.clone();
        assert!(within("read_receipt", move || read_receipt(&h, "w1", &rid, "w1").is_none()));
        crate::discard::harmless(std::fs::remove_dir_all(home)); // keep: cleanup of a scratch directory
    }
}
