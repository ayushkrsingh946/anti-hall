//! Host primitives of the `devswarm-child-turn` check for a child that runs inside a git checkout (D88). The child's prompt hook
//! registers the child on every turn: it merges the child's descriptor (`workspaces/<id>.json`), pre-creates its cursor and
//! inbox, upserts the registry row of the project's store and refreshes the summary projection, all under the workspace's lock.
//! The script (`engine/logic/devswarm-child-turn.js`) holds every decision and text; these functions hold no rule, threshold
//! or message, they only extract what it asks for and perform the one registration it has decided on.
//!
//! | raw function | what it does |
//! |---|---|
//! | `ctPrepare(cwd, id, session)` | reads everything the registration depends on and answers `{ok: true}`, or `{unsure: true}` when Node would do something the engine does not (a second registry row of the worktree, a phantom descriptor to retire, a row at another worktree, a missing or foreign store, a summary it cannot reproduce); writes nothing |
//! | `ctRegister(cwd, id, session)` | performs the registration `ctPrepare` planned: lock, descriptor, cursor and inbox, registry row, summary |
//! | `ctArchived(id, top, session)` | `{archived}`: the archive marker or the DevSwarm app's own verdict names this workspace |
//! | `ctHealthy(key)` | `{healthy}`: the project's ingest daemon is alive and its monitor is answering |
//! | `ctReaders(key, id)` | `{declared}`: some reader other than the floor holds a cursor of the partition (whose view of the unread count a hook can only tell from its own process ancestry) |
//! | `ctFindPlan(id, root)` | `{plan}`: the step plan of the workspace (by the worktree's key, then by id), `null` for none |
use super::host::with_settings;
use crate::checks::git::util::Settings;
use crate::checks::guardkit::ojson::OVal;
use crate::defaults;
use crate::meshw::common::{self, Inv};
use crate::meshw::ident::{self, R, defer};
use crate::meshw::idlock;
use crate::meshw::store::{MeshStore, RegistryRow};
use crate::meshw::{pull, summary};
use rquickjs::{Ctx, Function, Object};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::path::{Path, PathBuf};

/// A registration read and planned, waiting for [`register`] to perform it.
struct Plan {
    id: String,
    desc: OVal,
    row: RegistryRow,
    /// The project key of the worktree and the store it names (none: the hook registers no row).
    store: Option<(String, MeshStore)>,
}

thread_local! {
    /// The plan of this call (a script call runs on one thread).
    static PLANNED: RefCell<Option<Plan>> = const { RefCell::new(None) };
}

/// A new script call starts: no plan is held.
pub(super) fn reset_call() {
    PLANNED.with(|p| *p.borrow_mut() = None);
}

fn unsure() -> String {
    json!({"unsure": true}).to_string()
}

fn answer(r: R<Value>) -> String {
    r.map_or_else(|_| unsure(), |v| v.to_string())
}

fn inv_of(st: &Settings, cwd: &str) -> Inv {
    let home = PathBuf::from(&st.home);
    Inv { home: home.clone(), env: st.env.clone(), cwd: cwd.to_string(), now: common::now_ms(), stdin: None, write_home: home, store_override: None }
}

fn json_suffix() -> &'static str {
    defaults::text("mesh_write.json_suffix")
}

/// The descriptor the hook merges over: `{}` for none, unreadable or a scalar; a shape JavaScript would read differently defers.
fn read_base(target: &Path) -> R<OVal> {
    let empty = || OVal::Obj(Vec::new());
    match std::fs::symlink_metadata(target) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(empty()),
        Err(_) => return defer("descriptor-io"),
        Ok(m) if !m.is_file() => return defer("descriptor-kind"),
        Ok(_) => {}
    }
    let Ok(bytes) = std::fs::read(target) else { return Ok(empty()) };
    let text = String::from_utf8_lossy(&bytes);
    match OVal::parse(&text) {
        Some(v @ OVal::Obj(_)) => {
            if let OVal::Obj(fields) = &v
                && fields.iter().any(|(k, _)| k == defaults::text("devswarm_prompt.proto_key"))
            {
                return defer("descriptor-keys");
            }
            Ok(v)
        }
        Some(OVal::Arr(_)) => defer("descriptor-shape"),
        Some(_) => Ok(empty()),
        None if text.contains(defaults::text("devswarm_prompt.escape_marker")) => defer("descriptor-escapes"),
        None => Ok(empty()),
    }
}

fn path_field(desc: &OVal, key: &str, dflt: String) -> R<OVal> {
    match desc.get(key) {
        None | Some(OVal::Null) => Ok(OVal::Str(dflt)),
        Some(v @ OVal::Str(_)) => Ok(v.clone()),
        Some(_) => defer("descriptor-path-type"),
    }
}

/// A truncated-id phantom descriptor that shares this worktree: Node archives it (a fold and an archive the engine leaves to Node).
fn phantom_present(ws_dir: &Path, keep_id: &str, top: &str) -> R<bool> {
    let Ok(rd) = std::fs::read_dir(ws_dir) else { return Ok(false) };
    let truncated = regex::RegexBuilder::new(defaults::text("devswarm_prompt.uuid_truncated_re")).case_insensitive(true).build().map_err(|_| ident::Defer("regex".into()))?;
    let full = regex::RegexBuilder::new(defaults::text("devswarm_prompt.uuid_full_re")).case_insensitive(true).build().map_err(|_| ident::Defer("regex".into()))?;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(cand) = name.strip_suffix(json_suffix()) else { continue };
        if cand == keep_id || !idlock::is_safe_id(cand) || !truncated.is_match(cand) || full.is_match(cand) {
            continue;
        }
        let Some(OVal::Obj(fields)) = std::fs::read(e.path()).ok().and_then(|b| OVal::parse(&String::from_utf8_lossy(&b))) else { continue };
        let d = OVal::Obj(fields);
        if matches!(d.get(defaults::text("mesh_write.field_worktree_path")), Some(OVal::Str(w)) if w == top) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn plan(inv: &Inv, id: &str, session: &str) -> R<Plan> {
    let ctx = ident::resolve_context(&inv.cwd, true)?;
    let Some(top) = ctx.toplevel else { return defer("no-checkout") };
    let ws_dir = idlock::devswarm_root(&inv.home).join(defaults::text("mesh_write.dir_workspaces"));
    let mut desc = read_base(&ws_dir.join(format!("{id}{}", json_suffix())))?;
    desc.set("id", common::s(id));
    desc.set(defaults::text("mesh_write.field_worktree_path"), common::s(&top));
    desc.set(defaults::text("mesh_write.field_session_id"), common::s(session));
    let repo_id = inv.env.get(defaults::text("devswarm_prompt.repo_env")).filter(|r| !r.is_empty());
    desc.set("repoId", repo_id.map_or(OVal::Null, |r| common::s(r)));
    let inbox = path_field(&desc, defaults::text("mesh_write.field_inbox_path"), pull::inbox_default(&inv.home, id))?;
    desc.set(defaults::text("mesh_write.field_inbox_path"), inbox);
    let cursor = path_field(&desc, defaults::text("mesh_write.field_cursor_path"), pull::cursor_default(&inv.home, id))?;
    desc.set(defaults::text("mesh_write.field_cursor_path"), cursor);
    if desc.get("nudgeCommand").is_none() {
        desc.set("nudgeCommand", OVal::Null);
    }
    let row = RegistryRow {
        id: id.to_string(),
        worktree_path: pull::text_or_null(&desc, defaults::text("mesh_write.field_worktree_path"))?,
        session_id: pull::text_or_null(&desc, defaults::text("mesh_write.field_session_id"))?,
        inbox_path: pull::text_or_null(&desc, defaults::text("mesh_write.field_inbox_path"))?,
        cursor_path: pull::text_or_null(&desc, defaults::text("mesh_write.field_cursor_path"))?,
        nudge_command: match desc.get("nudgeCommand") {
            None | Some(OVal::Null) => None,
            Some(v) => Some(v.stringify()),
        },
    };
    let mut store = None;
    if let Some(key) = ident::repo_key_for_worktree(&top)? {
        let s = common::open_store(inv, &key)?;
        summary::check(&s, inv, None)?;
        let reader = crate::meshw::tick::open_reader(inv, &key)?.ok_or_else(|| ident::Defer("no-store".into()))?;
        let rows = ident::rows_of(&reader.roster().map_err(|e| ident::Defer(format!("registry:{e}")))?);
        if rows.iter().find(|r| r.id == id).and_then(|r| r.worktree_path.as_deref()).is_some_and(|have| have != top) {
            return defer("registry-collision");
        }
        if crate::meshw::lifeverbs::has_duplicate_rows(inv, &key, id, Some(&top))? {
            return defer("duplicate-registry-rows");
        }
        store = Some((key, s));
    }
    if phantom_present(&ws_dir, id, &top)? {
        return defer("phantom-descriptor");
    }
    Ok(Plan { id: id.to_string(), desc, row, store })
}

/// Perform a planned registration under the workspace's lock. `Err` only before the first write.
fn perform(inv: &Inv, p: Plan) -> R<()> {
    let Some(lock) = idlock::acquire(&inv.home, &p.id) else { return defer("lock-busy") };
    // the cursor and the inbox first, the descriptor last (it is what makes the child discoverable)
    pull::precreate(&p.desc);
    // a descriptor that cannot be written ends the registration, as in Node (the turn goes on without a row)
    let written = pull::write_descriptor(inv, &p.id, &p.desc).is_ok();
    if written && let Some((key, store)) = &p.store {
        match store.upsert_registry(&p.row, inv.now, |a, b| a == b) {
            Ok(_) => {
                if let Some(why) = summary::derive_after_write(store, inv, key) {
                    crate::meshw::log_summary_failure(defaults::text("devswarm_prompt.log_verb"), &why);
                }
            }
            // Node's registerStoreDescriptor swallows it and the next turn retries
            Err(e) => crate::discard::note("childturn.registry_upsert", &e.to_string()),
        }
    }
    lock.release();
    Ok(())
}

fn archived(inv: &Inv, id: &str, top: &str, session: &str) -> R<bool> {
    let t = crate::dssup::recon::sweep::Target { id: id.to_string(), worktree: top.to_string(), session: Some(session.to_string()) };
    if crate::dssup::recon::sweep::marker_archived(&inv.home, &t)? {
        return Ok(true);
    }
    let (verdict, _) = crate::meshw::appdb::archived_verdict(&inv.home, &inv.env, inv.now, id, Some(top), false)?;
    Ok(verdict == Some(true))
}

/// Whether a reader of this partition other than the floor has a cursor row: only then does the unread count depend on which reader asks.
fn readers_declared(inv: &Inv, key: &str, id: &str) -> R<bool> {
    let Some(reader) = crate::meshw::tick::open_reader(inv, key)? else { return Ok(false) };
    let rows = reader.reader_cursors(id).map_err(|e| ident::Defer(format!("reader-cursors:{e}")))?;
    let floor = defaults::text("mesh_write.floor_reader");
    Ok(rows.iter().any(|r| r["reader"].as_str() != Some(floor)))
}

fn find_plan(inv: &Inv, id: &str, root: &str) -> R<Value> {
    Ok(match crate::meshw::plan::find_for(inv, id, Some(root).filter(|r| !r.is_empty()))? {
        Some(f) => json!({"plan": f.plan.stringify()}),
        None => json!({"plan": null}),
    })
}

pub(super) fn install<'js>(c: &Ctx<'js>, h: &Object<'js>) -> rquickjs::Result<()> {
    h.set(
        "ctPrepare",
        Function::new(c.clone(), |cwd: String, id: String, session: String| -> rquickjs::Result<String> {
            with_settings(|st| {
                let inv = inv_of(st, &cwd);
                answer(plan(&inv, &id, &session).map(|p| {
                    PLANNED.with(|slot| *slot.borrow_mut() = Some(p));
                    json!({"ok": true})
                }))
            })
        })?,
    )?;
    h.set(
        "ctRegister",
        Function::new(c.clone(), |cwd: String, id: String, session: String| -> rquickjs::Result<String> {
            with_settings(|st| {
                let inv = inv_of(st, &cwd);
                let planned = PLANNED.with(|slot| slot.borrow_mut().take()).filter(|p| p.id == id);
                answer(planned.map_or_else(|| plan(&inv, &id, &session), Ok).and_then(|p| perform(&inv, p)).map(|()| json!({"ok": true})))
            })
        })?,
    )?;
    h.set(
        "ctArchived",
        Function::new(c.clone(), |id: String, top: String, session: String| -> rquickjs::Result<String> {
            with_settings(|st| answer(archived(&inv_of(st, &top), &id, &top, &session).map(|a| json!({"archived": a}))))
        })?,
    )?;
    h.set(
        "ctHealthy",
        Function::new(c.clone(), |key: String| -> rquickjs::Result<String> {
            with_settings(|st| answer(common::daemon_healthy(&inv_of(st, ""), Some(&key)).map(|h| json!({"healthy": h}))))
        })?,
    )?;
    h.set(
        "ctReaders",
        Function::new(c.clone(), |key: String, id: String| -> rquickjs::Result<String> {
            with_settings(|st| answer(readers_declared(&inv_of(st, ""), &key, &id).map(|d| json!({"declared": d}))))
        })?,
    )?;
    h.set(
        "ctFindPlan",
        Function::new(c.clone(), |id: String, root: String| -> rquickjs::Result<String> {
            with_settings(|st| answer(find_plan(&inv_of(st, &root), &id, &root)))
        })?,
    )?;
    Ok(())
}
