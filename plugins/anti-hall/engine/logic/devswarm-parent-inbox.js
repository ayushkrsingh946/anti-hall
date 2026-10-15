// check = "devswarm-parent-inbox" (UserPromptSubmit). Mirrors hooks/devswarm-parent-inbox.js. It answers every case where Node prints and
// writes nothing (a Jev judge child, a child workspace, the integration killed or switched off, DevSwarm inactive) and, for an active
// Primary, the cases where there is no git checkout around its working directory: then the hook has no project identity, so no roster,
// summary, mailbox, wake coverage, maintainer notice or drift to show. All it says is its communication-override reminder, through the
// emit-dedupe store, and, once, a seat conflict the SessionStart hook recorded. A Primary in a checkout defers to Node, which owns the
// roster, the mailbox and their dedupe state. Keys and texts: small_guards.toml (devswarm_prompt.*).
'use strict';

// The text of the session id, or null (`typeof payload.session_id === 'string' && payload.session_id`).
function dpSession(p) {
  return jx.isObj(p) && typeof p.session_id === 'string' && p.session_id !== '' ? p.session_id : null;
}

// The seat-conflict marker of the session: 'none' (nothing to repeat), or 'repeat' (Node must check the seat again and rewrite it).
function dpSeatMarker(home, sid) {
  var file = home + '/' + ah.cfg('paths.base_dir') + '/' + ah.cfg('devswarm_prompt.seat_dir') + '/' + sid.replace(/[^A-Za-z0-9_-]/g, '') + '.json';
  var raw = ah.fs.readText(file);
  if (raw === null) return 'none';
  var mark;
  try { mark = JSON.parse(raw); } catch (e) { return 'none'; }
  if (!mark) return 'none';
  return Number(mark.shown) < ah.cfgNum('devswarm_prompt.seat_shown_max') ? 'repeat' : 'none';
}

function decide(p) {
  if (devswarmGate.silent(false, 'devswarm_prompt.parent_setting')) return 'allow';
  // An active Primary. With a working directory the hook resolves the checkout around it; with none it has no project identity either.
  var cwd = jx.isObj(p) && typeof p.cwd === 'string' && p.cwd !== '' ? p.cwd : null;
  if (cwd !== null) {
    if (cwd.charAt(0) !== '/') return 'defer'; // Node resolves it against its own working directory
    var c = ah.repo.context(cwd);
    if (c.unsure || c.root !== null) return 'defer';
  }
  var home = spawn.osHome();
  if (home === null) return 'defer';
  var sid = dpSession(p);
  var tp = jx.isObj(p) && typeof p.transcript_path === 'string' ? p.transcript_path : null;
  if (tp !== null && tp !== '' && tp.charAt(0) !== '/') return 'defer'; // a relative transcript path is Node's to resolve
  // the seat marker is read before anything is written: a marker still owed a repeat is Node's
  if (sid !== null && dpSeatMarker(home, sid) === 'repeat') return 'defer';
  var every = ah.settings.num('verify_first.num_repeat_every');
  var text0 = ah.cfg('devswarm_prompt.override_reassert');
  var opts = { sessionId: sid, key: ah.cfg('devswarm_prompt.override_key'), content: text0, transcriptPath: tp, keepaliveTurns: isFinite(every) && every > 0 ? every : 0 };
  var emit = true;
  if (sid !== null && !dedupe.disabled()) {
    if (!dedupe.canDecide(opts)) return 'defer';
    emit = dedupe.shouldEmit(opts);
  }
  if (!emit) return 'allow';
  return { advisory: text.advisoryJson(ah.cfg('devswarm_prompt.event'), text0) };
}
