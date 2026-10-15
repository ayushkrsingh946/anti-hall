//! The DevSwarm thread: the file watcher over every DevSwarm source, the start-up reconcile and the loop that turns batches of
//! changes into reconciles. The watcher is generic (it knows directories, not DevSwarm); this module chooses the directories and
//! says what a change in each of them means.
use super::{Wire, consume, facts};
use crate::defaults;
use crate::devswarm_rt::reconcile::Cause;
use crate::devswarm_rt::state::Lifecycle;
use crate::meshw::idlock::devswarm_root;
use crate::meshw::union;
use crate::watch::{Config, Watcher, poll::Filter};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// What a watched directory is.
#[derive(Debug, Clone, PartialEq)]
pub enum Role {
    /// A source of the workspace state: a change is a hint to reconcile.
    State,
    /// A workspace's transcript directory: a change dirties that workspace for Jev.
    Transcript(String),
    /// A workspace's git directory: a commit or checkout dirties that workspace for Jev.
    Git(String),
}

/// The directories to watch now, with the filter and role of each.
pub fn wanted(w: &Wire, cfg: &Config) -> Vec<(PathBuf, Filter, Role)> {
    let mut out: Vec<(PathBuf, Filter, Role)> = Vec::new();
    if let Some(db) = &w.rt.detection().app_db
        && let (Some(dir), Some(name)) = (db.parent(), db.file_name().and_then(|n| n.to_str()))
    {
        out.push((dir.to_path_buf(), cfg.sqlite_filter(name), Role::State));
    }
    let root = devswarm_root(&w.home);
    for k in defaults::list("devswarm_wire.watch_state_dirs") {
        out.push((root.join(defaults::text(k)), cfg.all_filter(), Role::State));
    }
    let snap = w.rt.current();
    for ws in snap.workspaces.values().filter(|x| x.lifecycle.value == Lifecycle::Active) {
        let Some(wt) = ws.worktree.as_deref() else { continue };
        if let Some(key) = facts::repo_key(wt) {
            // the store's SQLite shared-memory index is ignored (all_filter): the reconcile itself opens the store, and the open
            // touches that file, so watching it made every reconcile trigger the next one (live P0, 2026-10-15)
            out.push((union::store_dir(&w.home, &key), cfg.all_filter(), Role::State));
        }
        let descs = facts::descriptors(&w.home);
        for d in descs.iter().filter(|d| d.worktree == wt || d.id == ws.id) {
            if let Some(dir) = facts::transcript_path(&w.home, &d.worktree, &d.session).parent() {
                out.push((dir.to_path_buf(), cfg.all_filter(), Role::Transcript(ws.id.clone())));
            }
        }
        if let Some(g) = facts::git_dir(wt) {
            out.push((g, Filter::names(defaults::list("devswarm_wire.watch_git_names")), Role::Git(ws.id.clone())));
        }
    }
    out
}

/// Bring the watcher in line with [`wanted`]: add directories that appeared, drop ones no longer wanted.
pub fn sync(w: &Wire, watcher: &Watcher, have: &mut HashMap<PathBuf, Role>) {
    let want = wanted(w, watcher.config());
    for (dir, filter, role) in &want {
        if have.get(dir) != Some(role) {
            if have.contains_key(dir) {
                watcher.remove(dir);
            }
            watcher.add(dir, filter.clone());
            have.insert(dir.clone(), role.clone());
        }
    }
    let gone: Vec<PathBuf> = have.keys().filter(|d| !want.iter().any(|(x, _, _)| x == *d)).cloned().collect();
    for d in gone {
        watcher.remove(&d);
        have.remove(&d);
    }
}

/// What a batch of changed paths asks for.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// A state source changed, or the watcher lost track.
    pub reconcile: Option<Cause>,
    /// Workspaces whose transcript or git directory changed.
    pub dirty: Vec<String>,
}

/// Classify a batch against the watched directories.
pub fn plan(have: &HashMap<PathBuf, Role>, rescan: bool, paths: &[PathBuf]) -> Plan {
    let mut p = Plan::default();
    if rescan {
        p.reconcile = Some(Cause::Overflow);
    }
    for path in paths {
        match path.parent().and_then(|d| have.get(d)) {
            Some(Role::State) if p.reconcile.is_none() => p.reconcile = Some(Cause::Event),
            Some(Role::Transcript(id) | Role::Git(id)) if !p.dirty.contains(id) => p.dirty.push(id.clone()),
            _ => {}
        }
    }
    p
}

/// The work the thread has done since the process started: batches received, paths in them, and reconciles run per cause (the
/// `status --threads` work counters; reading them costs nothing).
pub mod work {
    use std::sync::atomic::{AtomicU64, Ordering};
    /// Batches the watcher handed the thread.
    pub static BATCHES: AtomicU64 = AtomicU64::new(0);
    /// Changed paths in those batches.
    pub static PATHS: AtomicU64 = AtomicU64::new(0);
    /// Reconcile requests folded into one already waiting for `devswarm_wire.reconcile_min_gap_ms` to pass.
    pub static DEFERRED: AtomicU64 = AtomicU64::new(0);
    pub(crate) fn add(c: &AtomicU64, n: u64) {
        c.fetch_add(n, Ordering::Relaxed);
    }
    /// `(name, value)` of every counter.
    pub fn snapshot() -> Vec<(&'static str, u64)> {
        vec![("batches", BATCHES.load(Ordering::Relaxed)), ("paths", PATHS.load(Ordering::Relaxed)), ("reconciles_deferred", DEFERRED.load(Ordering::Relaxed))]
    }
}

/// The cause that covers both: an overflow (a full reconcile that counts repairs) wins over a plain event.
fn merge(a: Option<Cause>, b: Cause) -> Cause {
    if a == Some(Cause::Overflow) { Cause::Overflow } else { b }
}

/// The thread body: reconcile at start, then one reconcile per batch (at most one per `devswarm_wire.reconcile_min_gap_ms`; a
/// batch inside the gap is folded into the reconcile at its end, never dropped), until `stop` says the daemon is draining.
pub fn run(w: &Wire, stop: &dyn Fn() -> bool) {
    w.reconcile(Cause::Startup);
    let watcher = Watcher::start(Config::load());
    let mut have: HashMap<PathBuf, Role> = HashMap::new();
    sync(w, &watcher, &mut have);
    let wait = Duration::from_millis(defaults::num("devswarm_wire.wait_ms"));
    let gap = Duration::from_millis(defaults::num("devswarm_wire.reconcile_min_gap_ms"));
    let mut last = Instant::now();
    let mut pending: Option<Cause> = None;
    while !stop() {
        w.act_if_pending();
        w.events_if_due();
        w.stall_tick();
        let left = gap.saturating_sub(last.elapsed());
        if let Some(batch) = watcher.next(if pending.is_some() { wait.min(left) } else { wait }) {
            work::add(&work::BATCHES, 1);
            work::add(&work::PATHS, batch.paths.len() as u64);
            let p = plan(&have, batch.rescan, &batch.paths);
            if !p.dirty.is_empty() {
                let n = consume::queue_ids(&w.state_dir, &p.dirty);
                if n > 0 {
                    w.count_dirty(n);
                }
                w.run_jev();
            }
            // a transcript or git event on a workspace the state calls stuck: the silence clock reset, so read the state again now
            if let Some(cause) = p.reconcile.or_else(|| w.stuck_among(&p.dirty).then_some(Cause::Event)) {
                if pending.is_some() {
                    work::add(&work::DEFERRED, 1);
                }
                pending = Some(merge(pending, cause));
            }
        }
        if let Some(cause) = pending
            && last.elapsed() >= gap
        {
            pending = None;
            w.reconcile(cause);
            last = Instant::now();
            sync(w, &watcher, &mut have);
        }
    }
}
