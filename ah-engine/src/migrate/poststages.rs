//! Four post-update store stages of `update.js`, run by the engine (roadmap #61 chunk C1): `reader-cursors-import`,
//! `reader-floor-repair` and `dual-partition-acks` (the reader-cursor repairs of `devswarm-lib/repair.js`) and
//! `promote-unclaimed` (`devswarm-lib/identity.js`). Each returns the counts Node's `devswarm.js` function returned; the shaping of the
//! answer (keys, detail text, the one-time marker) is the stage table's, in `operator/postpull.rs`.
//!
//! Every stage is idempotent (a second run finds nothing to do), fail-open (an error is counted and the stage retries next run),
//! and moves or marks, never deletes: a reader row is retired by marking it, a cursor only rises, a descriptor is rewritten in
//! place. A store the engine cannot open the way Node would (no sqlite `BACKEND` marker: a journal store, a store Node would
//! first mark), or a case only Node decides (a session working directory only git can classify), returns `None`: the stage
//! table's `fallback` Node function then runs the whole stage.
use super::readercur::{self as rc, Cx, Handle};
use super::{Ctx, list_store_hashes};
use crate::checks::jsport::num::to_js_string;
use crate::defaults;
use crate::meshw::cursors::{self, LogRec};
use crate::meshw::ident;
use crate::meshw::idlock::is_safe_id;
use crate::meshw::store::mesh_message_hash;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

/// What a stage counted (Node's result object), plus the pending rows that keep its one-time marker from being stamped.
#[derive(Default)]
pub(crate) struct Tally {
    pub n: BTreeMap<&'static str, f64>,
    pub pending_rows: f64,
    /// `ok` of a result that carries one (`promoteUnclaimedRegistrySessions`).
    pub ok: Option<bool>,
}

impl Tally {
    fn add(&mut self, k: &'static str, by: f64) {
        *self.n.entry(k).or_insert(0.0) += by;
    }

    fn get(&self, k: &str) -> f64 {
        self.n.get(k).copied().unwrap_or(0.0)
    }
}

/// The stages this module answers, by their `native` word in the stage table.
pub(crate) fn is_store_stage(which: &str) -> bool {
    defaults::list("update_post.store_stages").contains(&which)
}

/// Run one: `None` when Node must answer. `dry` is the ingest dry-run switch (a preview: nothing is written).
pub(crate) fn run(ctx: &Ctx, which: &str, dry: bool) -> Option<Tally> {
    let home = Path::new(&ctx.home);
    let t = match which {
        "promote_unclaimed" => super::promote::promote_unclaimed(ctx),
        "cursor_hygiene" => Some(super::cursorhyg::run(ctx)),
        "merge_split_backend_stores" => merge_split(ctx),
        _ => {
            let hashes = stores(ctx)?;
            let mut cx = Cx::new(&ctx.home);
            let t = match which {
                "reader_cursors_import" => reader_cursors_import(ctx, &mut cx, &hashes, dry),
                "reader_floor_repair" => reader_floor_repair(ctx, &mut cx, &hashes, dry),
                "dual_partition_acks" => dual_partition_acks(ctx, &mut cx, &hashes, dry),
                "child_sender_labels" => super::childlabels::run(ctx, &hashes, dry),
                _ => return None,
            };
            if cx.deferred {
                return None;
            }
            t
        }
    };
    let _ = home;
    t
}

/// `store.listStoreHashes(home)`, when the engine can open every one of them as Node would.
fn stores(ctx: &Ctx) -> Option<Vec<String>> {
    let forced = ctx.env.get(defaults::text("mesh_write.env_store_backend")).map(|s| s.trim().to_lowercase()).unwrap_or_default();
    if !forced.is_empty() && forced != defaults::text("mesh.backend_sqlite") {
        return None;
    }
    let hashes = list_store_hashes(ctx);
    hashes.iter().all(|h| rc::sqlite_store(Path::new(&ctx.home), h)).then_some(hashes)
}

/// `mergeSplitBackendStoresAllStores(home)`: a sqlite-backed store is SPLIT when its `journal/` directory also holds a `.ndjson` segment.
/// Merging the two sides (the journal backend's reader, the dedupe, the registry union, the rewrite of the summary) is Node's, so a split
/// store hands the whole run to Node before anything is written; with no split store Node writes nothing and counts nothing.
fn merge_split(ctx: &Ctx) -> Option<Tally> {
    let hashes = stores(ctx)?;
    for hash in &hashes {
        let dir = rc::store_dir(Path::new(&ctx.home), hash).join(defaults::text("update_post.journal_dir"));
        let segments = std::fs::read_dir(dir).is_ok_and(|rd| rd.flatten().any(|e| e.file_name().to_string_lossy().ends_with(defaults::text("mesh_write.ndjson_suffix"))));
        if segments {
            return None;
        }
    }
    let mut t = Tally::default();
    for k in ["splitStores", "messagesMerged", "registryMerged", "errors", "reDeliveredUnread"] {
        t.n.insert(k, 0.0);
    }
    t.add("stores", hashes.len() as f64);
    Some(t)
}

pub(super) fn swallowed(ctx: &Ctx, stage: &str, what: &str, e: &str) {
    ctx.note(defaults::render("update_post.note_store", &[("stage", &stage), ("what", &what), ("error", &e)]));
}

/// Plan (read-only) every partition of a store that still needs its import; true when any plan met a case only Node decides.
fn import_needs_node(cx: &mut Cx, h: &rc::Handle, home: &Path) -> bool {
    for id in h.rd().workspace_ids() {
        if !is_safe_id(&id) {
            continue;
        }
        if crate::meshw::cursors::rows_from_reader(h.rd(), &id).is_ok_and(|rows| !crate::meshw::cursors::needs_import(&rows)) {
            continue;
        }
        let cursor_path = match ident::read_descriptor(home, &id).as_ref().and_then(|d| d.get("cursorPath")) {
            Some(crate::checks::guardkit::ojson::OVal::Str(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        };
        rc::import_plan_dry(cx, h, &id, cursor_path.as_deref());
        if cx.deferred {
            return true;
        }
    }
    false
}

/// Plan (read-only) every partition of a store for the floor repair; true when any plan met a case only Node decides.
fn repair_needs_node(cx: &mut Cx, h: &rc::Handle) -> bool {
    for id in h.rd().workspace_ids() {
        if is_safe_id(&id) {
            let _planned = rc::repair_pinned_floors(cx, h, &id, true); // keep: only the deferral flag matters, errors surface in the real pass
            if cx.deferred {
                return true;
            }
        }
    }
    false
}

/// `importReaderCursorsAllStores(home)`.
fn reader_cursors_import(ctx: &Ctx, cx: &mut Cx, hashes: &[String], dry: bool) -> Option<Tally> {
    let home = Path::new(&ctx.home);
    let mut t = Tally::default();
    for hash in hashes {
        let h = match rc::open(home, hash, dry) {
            Ok(h) => h,
            Err(e) => {
                t.add("errors", 1.0);
                swallowed(ctx, "reader-cursors-import", hash, &e);
                continue;
            }
        };
        t.add("stores", 1.0);
        if !dry && import_needs_node(cx, &h, home) {
            // one session only Node can classify: write nothing for this store, the Node fallback starts from the untouched state
            cx.deferred = true;
            return Some(t);
        }
        for id in h.rd().workspace_ids() {
            if !is_safe_id(&id) {
                continue;
            }
            t.add("partitions", 1.0);
            let cursor_path = match ident::read_descriptor(home, &id).as_ref().and_then(|d| d.get("cursorPath")) {
                Some(crate::checks::guardkit::ojson::OVal::Str(s)) if !s.is_empty() => Some(s.clone()),
                _ => None,
            };
            let done = rc::import_legacy(cx, &h, &id, cursor_path.as_deref(), dry).and_then(|r| {
                if r.imported {
                    t.add("imported", 1.0);
                }
                if r.would {
                    t.add("wouldImport", 1.0);
                }
                // provably ended readers are marked (process proof only); this never moves the floor by itself
                if dry { Ok(()) } else { rc::retire_ended(cx, &h, &id).map(|_| ()) }
            });
            if let Err(e) = done {
                t.add("errors", 1.0);
                swallowed(ctx, "reader-cursors-import", &id, &e);
            }
        }
    }
    // a preview imported nothing: the import itself is still pending
    t.pending_rows = if dry { 1.0 } else { 0.0 };
    Some(t)
}

/// `repairReaderFloorsAllStores(home)`.
fn reader_floor_repair(ctx: &Ctx, cx: &mut Cx, hashes: &[String], dry: bool) -> Option<Tally> {
    let home = Path::new(&ctx.home);
    let mut t = Tally::default();
    for hash in hashes {
        let h = match rc::open(home, hash, dry) {
            Ok(h) => h,
            Err(e) => {
                t.add("errors", 1.0);
                swallowed(ctx, "reader-floor-repair", hash, &e);
                continue;
            }
        };
        t.add("stores", 1.0);
        if !dry && repair_needs_node(cx, &h) {
            // a case only Node decides: write nothing for this store, the Node fallback starts from the untouched state
            return Some(t);
        }
        for id in h.rd().workspace_ids() {
            if !is_safe_id(&id) {
                continue;
            }
            t.add("partitions", 1.0);
            match rc::repair_pinned_floors(cx, &h, &id, dry) {
                Ok(r) if !r.changed => {}
                Ok(r) => {
                    t.add("pending", 1.0);
                    if !dry {
                        t.add("repaired", 1.0);
                        t.add("retired", r.retired as f64);
                        t.add("floorsRaised", r.floors_raised() as f64);
                    }
                }
                Err(e) => {
                    t.add("errors", 1.0);
                    swallowed(ctx, "reader-floor-repair", &id, &e);
                }
            }
        }
    }
    t.pending_rows = if dry { t.get("pending") } else { 0.0 };
    Some(t)
}

/// `reconcileDualPartitionAcksAllStores(home)`.
fn dual_partition_acks(ctx: &Ctx, cx: &mut Cx, hashes: &[String], dry: bool) -> Option<Tally> {
    let home = Path::new(&ctx.home);
    let mut t = Tally::default();
    for hash in hashes {
        let h = match rc::open(home, hash, dry) {
            Ok(h) => h,
            Err(e) => {
                t.add("errors", 1.0);
                swallowed(ctx, "dual-partition-acks", hash, &e);
                continue;
            }
        };
        t.add("stores", 1.0);
        if !dry {
            // plan the whole store first (read-only): a case only Node decides leaves the store untouched for the fallback
            let _planned = reconcile(cx, &h, true, true, &mut Tally::default()); // keep: only the deferral flag matters
            if cx.deferred {
                return Some(t);
            }
        }
        if let Err(e) = reconcile(cx, &h, dry, false, &mut t) {
            t.add("errors", 1.0);
            swallowed(ctx, "dual-partition-acks", hash, &e);
        }
    }
    t.pending_rows = if dry { t.get("wouldRaise") } else { 0.0 };
    Some(t)
}

/// One store's message as the reconcile reads it.
struct Msg {
    hash: Option<String>,
    orig: Option<String>,
}

/// `forwardedOrigHashOf(row)`: the original's hash for a forwarded copy (stamped, else reconstructed from the provenance prefix).
fn forwarded_orig_hash(r: &Value) -> Option<String> {
    if let Some(h) = jv(r, "origHash").as_str().filter(|h| !h.is_empty()) {
        return Some(h.to_string());
    }
    let rest = jv(r, "body").as_str()?.strip_prefix(defaults::text("mesh_write.forward_prefix"))?;
    let close = rest.find(']').filter(|i| *i > 0)?;
    let archived = rest.get(..close)?;
    let body = rest.get(close + 1..)?.strip_prefix(' ')?;
    let direct = defaults::text("mesh_write.mtype_direct");
    Some(mesh_message_hash(
        jv(r, "sender").as_str(),
        Some(archived),
        direct,
        jv(r, "urgency").as_str().unwrap_or(""),
        body,
        &to_js_string(jv(r, "ts").as_f64().unwrap_or(0.0)),
        jv(r, "needsReply").as_bool().unwrap_or(false),
    ))
}

struct View {
    floor: i64,
    rows: Vec<Msg>,
    consumed: HashSet<String>,
}

fn keys(m: &Msg) -> Vec<&String> {
    [m.hash.as_ref(), m.orig.as_ref()].into_iter().flatten().collect()
}

/// `reconcileDualPartitionAcks(s, home, dryRun, out)` (`probe`: plan only, and run what a raise would plan for an unimported partition): for every anchor/partner pair on one worktree, raise a partition's floor
/// through the contiguous prefix of rows already consumed in the other (MAX-only, loss-free; stops at the first row not).
fn reconcile(cx: &mut Cx, h: &Handle, dry: bool, probe: bool, t: &mut Tally) -> Result<(), String> {
    let registry = h.rd().roster().map_err(|e| e.to_string())?;
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    for d in &registry {
        let Some(wt) = jv(d, "worktreePath").as_str().filter(|w| !w.is_empty()) else { continue };
        let Some(id) = row_id(d).filter(|i| is_safe_id(i)) else { continue };
        let key = match ident::canonical_mesh_id(wt) {
            Ok(Some(k)) => k,
            Ok(None) => continue,
            Err(_) => {
                cx.deferred = true;
                continue;
            }
        };
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        groups.entry(key).or_default().push(id);
    }
    if cx.deferred {
        return Ok(()); // a worktree only git can place: nothing is raised for this store
    }
    let sid_of: HashMap<String, String> = registry.iter().filter_map(|d| row_id(d).map(|i| (i, jv(d, "sessionId").as_str().unwrap_or("").to_string()))).collect();
    for mesh_id in order {
        let Some(ids) = groups.get(&mesh_id) else { continue };
        if ids.len() < 2 || !ids.contains(&mesh_id) {
            continue;
        }
        let mut view: HashMap<&String, View> = HashMap::new();
        for id in ids {
            let mut rows = Vec::new();
            h.rd()
                .for_each_message(id, 0, |m| {
                    rows.push(Msg { hash: jv(&m, "hash").as_str().map(str::to_string), orig: forwarded_orig_hash(&m) });
                    true
                })
                .map_err(|e| e.to_string())?;
            let floor = rc::floor_store(cx, h, id);
            let mut consumed = HashSet::new();
            for r in rows.iter().take((floor.max(0) as usize).min(rows.len())) {
                consumed.extend(keys(r).into_iter().cloned());
            }
            view.insert(id, View { floor, rows, consumed });
        }
        let anchor_sid = sid_of.get(&mesh_id).cloned().unwrap_or_default();
        let same_identity = |id: &String| sid_of.get(id).is_none_or(|s| s.is_empty() || *s == anchor_sid);
        let pair_of = |a: &String, b: &String| (*a == mesh_id && same_identity(b)) || (*b == mesh_id && same_identity(a));
        for id in ids {
            let Some(me) = view.get(id) else { continue };
            let others: Vec<&View> = ids.iter().filter(|o| *o != id && pair_of(id, o)).filter_map(|o| view.get(o)).collect();
            if others.is_empty() {
                continue;
            }
            let acked_elsewhere = |m: &Msg| others.iter().any(|o| keys(m).iter().any(|k| o.consumed.contains(*k)));
            let mut to = me.floor.max(0) as usize;
            while me.rows.get(to).is_some_and(&acked_elsewhere) {
                to += 1;
            }
            if (to as i64) <= me.floor {
                continue;
            }
            t.add("partitions", 1.0);
            t.add("rows", (to as i64 - me.floor) as f64);
            if probe {
                rc::raise_probe(cx, h, id);
                continue;
            }
            if dry {
                t.add("wouldRaise", 1.0);
                continue;
            }
            rc::raise_all_loss_free(cx, h, id, defaults::text("mesh_write.ns_store"), to as i64)?;
            cursors::log_cursor_write(
                &cx.home,
                &LogRec {
                    id,
                    caller_id: None,
                    ns: defaults::text("mesh_write.log_ns_store"),
                    from: Some(me.floor),
                    to: Some(to as f64),
                    delivered: None,
                    nonce: None,
                    gate: defaults::text("update_post.cursor_gate"),
                    verb: defaults::text("update_post.cursor_verb"),
                    cwd: None,
                    ok: true,
                    err: None,
                    repo_key: None,
                },
            );
            t.add("raised", 1.0);
        }
    }
    Ok(())
}

/// `v[k]` of a JSON value: `null` when absent.
pub(super) fn jv<'a>(v: &'a Value, k: &str) -> &'a Value {
    v.get(k).unwrap_or(&Value::Null)
}

/// `String(d.id)` of a registry row.
pub(super) fn row_id(d: &Value) -> Option<String> {
    match jv(d, "id") {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(to_js_string(n.as_f64()?)),
        _ => None,
    }
}
