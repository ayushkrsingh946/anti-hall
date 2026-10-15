// check = "devswarm-child-turn" (UserPromptSubmit). Mirrors hooks/devswarm-child-turn.js. It answers every case where Node prints and writes
// nothing (a Jev judge child, not a child workspace, the integration killed or switched off, DevSwarm inactive) and, for an active child, the
// cases where there is no git checkout around its working directory: then there is no descriptor to register, no project identity for a mailbox
// summary or a banner, and the turn is: the heartbeat file, the fixed reminder block (through the emit-dedupe store), and the step plan when the
// child has one. A child with unread mail, or in a checkout, defers to Node, which owns the descriptor, the store and the mailbox rendering.
// Builds on devswarm-child-role.js (script.includes) for the plugin root and the stable launcher. Keys and texts: small_guards.toml (devswarm_prompt.*).
'use strict';

// `isSafeId` of companion/lib/liveness.js.
function ctSafeId(id) {
  return typeof id === 'string' && id !== '' && id !== '.' && id !== '..' && id.indexOf('..') < 0 && /^[A-Za-z0-9._-]+$/.test(id);
}

// `heartbeatKey(builderId, branch)`: the heartbeat's file name.
function ctHeartbeatKey(builderId, branch) {
  if (ctSafeId(builderId)) return builderId;
  if (ctSafeId(branch)) return branch;
  var safe = String(branch).replace(new RegExp(ah.cfg('devswarm_prompt.heartbeat_branch_unsafe'), 'g'), '-').replace(/^\.+/, '_').slice(0, ah.cfgNum('devswarm_prompt.heartbeat_name_max')) || ah.cfg('devswarm_prompt.heartbeat_name_fallback');
  return safe + '-' + ah.sha1(String(branch)).slice(0, ah.cfgNum('devswarm_prompt.heartbeat_hash_len'));
}

// The CLI path the reminders name: the stable launcher when that is switched on and installed, else the plugin's own script (`preferStableLauncher`).
// null when the plugin root cannot be resolved the way Node's `__dirname` would be.
function ctCli(opts, home) {
  var root = jx.isObj(opts) && typeof opts.plugin_root === 'string' ? opts.plugin_root : ah.env.get(ah.cfg('env.plugin_root'));
  if (root === null || root === undefined || root === '') return null;
  var nr = dwNodeRoot(root);
  if (nr === null) return null;
  var l = ah.cfg('devswarm_role.launcher_cli');
  if (ah.settings.bool('devswarm_role.sw_stable_launcher')) {
    var stable = posix.normalize(home + '/' + ah.cfg('devswarm_role.bin_dir') + '/' + l.name);
    if (ah.fs.isFile(stable)) return stable;
  }
  return posix.normalize(nr + '/' + l.target);
}

// Text of a small state file, or null when absent; undefined when it is too large to read whole (Node reads all of it).
function ctRead(path) {
  var size = ah.fs.size(path);
  if (size === null) return null;
  if (size > ah.cfgNum('script.read_max_bytes')) return undefined;
  return ah.fs.readText(path);
}

// `unreadInfo(env, home)` for a child whose mail the engine can count: true = nothing unread (or nothing readable), false = unread mail, or
// something only Node reads the same way.
function ctNoUnread(id, home) {
  if (!ctSafeId(id)) return true;
  var raw = ctRead(home + '/' + ah.cfg('paths.base_dir') + '/' + ah.cfg('devswarm_prompt.descriptor_dir') + '/' + id + '.json');
  if (raw === undefined) return false;
  if (raw === null) return true;
  var desc;
  try { desc = JSON.parse(raw); } catch (e) { return true; }
  if (!desc || typeof desc !== 'object' || !desc.inboxPath) return true;
  if (typeof desc.inboxPath !== 'string' || (desc.cursorPath !== undefined && desc.cursorPath !== null && typeof desc.cursorPath !== 'string')) return false;
  if (desc.inboxPath.charAt(0) !== '/' || (desc.cursorPath && desc.cursorPath.charAt(0) !== '/')) return false;
  var inbox = ctRead(desc.inboxPath);
  if (inbox === undefined) return false;
  if (inbox === null) return true; // missing or unreadable: the count is not known, so nothing is said
  if (!desc.cursorPath) return true;
  var cur = ctRead(desc.cursorPath);
  if (cur === undefined) return false;
  if (cur === null) return true;
  var trimmed = cur.trim(), cursor;
  if (/^\d+$/.test(trimmed)) cursor = parseInt(trimmed, 10);
  else { try { cursor = Number(JSON.parse(trimmed).line); } catch (e) { return true; } }
  if (!isFinite(cursor) || cursor < 0) return true;
  var lines = inbox.split('\n').filter(function (l) { return l.trim() !== ''; });
  return lines.slice(cursor).length === 0;
}

// `buildPlanSegment(id, home, null, env)` of the hook: the child's step plan as a reminder, null for none. null for anything that throws.
function ctPlanSegment(id, home, root, cli) {
  try {
    if (!ctSafeId(id)) return null;
    if (gates.setting('devswarm_prompt.plan_tracking_setting', true, root) === false) return null;
    var raw = ctRead(home + '/' + ah.cfg('paths.base_dir') + '/' + ah.cfg('devswarm_prompt.plan_dir') + '/' + id + '.json');
    if (raw === undefined) throw gates.UNDECIDABLE;
    var plan = null;
    if (raw !== null) { try { plan = JSON.parse(raw); } catch (e) { plan = null; } }
    var found = plan && typeof plan === 'object' && !Array.isArray(plan) && Array.isArray(plan.steps) ? plan : null;
    if (!found || !found.steps.length) {
      if (gates.setting('devswarm_prompt.plan_required_setting', false, root) !== true) return null;
      return text.render(ah.cfg('devswarm_prompt.plan_none'), { cli: cli, id: id });
    }
    var open = found.steps.filter(function (s) { return s.status !== 'done'; }), cur = null;
    for (var i = 0; i < open.length; i++) {
      var s = open[i];
      if (s.status !== 'doing' && s.status !== 'blocked') continue;
      if (!cur || (s.ts || 0) >= (cur.ts || 0)) cur = s;
    }
    cur = cur || open[0] || null;
    var done = found.steps.filter(function (s) { return s.status === 'done'; }).length;
    var head = cur
      ? text.render(ah.cfg('devswarm_prompt.plan_head'), { done: done, total: found.steps.length, n: cur.n, blocked: cur.status === 'blocked' ? ah.cfg('devswarm_prompt.plan_blocked') : '', text: cur.text })
      : text.render(ah.cfg('devswarm_prompt.plan_all_done'), { total: found.steps.length, cli: cli });
    return head + text.render(ah.cfg('devswarm_prompt.plan_tail'), { cli: cli, id: id });
  } catch (e) {
    if (e === gates.UNDECIDABLE) throw e;
    return null;
  }
}

function decide(p, opts) {
  if (devswarmGate.silent(true, 'devswarm_prompt.child_setting')) return 'allow';
  return gates.run(function () {
    // An active child. Its working directory must be known and no git checkout (the hook resolves the checkout around it).
    var cwd = jx.isObj(p) ? p.cwd : undefined;
    if (typeof cwd !== 'string' || cwd === '' || cwd.charAt(0) !== '/') return 'defer';
    var c = ah.repo.context(cwd);
    if (c.unsure || c.toplevel !== null || c.root !== null) return 'defer';
    var guard = ah.homeGuard();
    if (guard.status !== 'ok') return 'defer';
    var home = guard.home;
    var root = gates.pluginRoot(opts);
    var cli = ctCli(opts, home);
    if (cli === null) return 'defer';
    var sidRaw = jx.isObj(p) ? p.session_id : undefined;
    if (sidRaw && typeof sidRaw !== 'string') return 'defer'; // a number or an object as a session id: Node keys the store on its text
    var sid = typeof sidRaw === 'string' && sidRaw !== '' ? sidRaw : null;
    var tpRaw = jx.isObj(p) ? p.transcript_path : undefined;
    if (tpRaw && typeof tpRaw !== 'string') return 'defer';
    var tp = typeof tpRaw === 'string' && tpRaw !== '' ? tpRaw : null;
    if (tp !== null && tp.charAt(0) !== '/') return 'defer';
    var builder = ah.env.get(ah.cfg('devswarm_prompt.builder_env'));
    var branch = ah.env.get(ah.cfg('devswarm_prompt.branch_env'));
    // an id shaped like a cut-short UUID makes Node say so on stderr while it registers the descriptor
    if (ctSafeId(builder) && sid !== null && new RegExp(ah.cfg('devswarm_prompt.uuid_truncated_re'), 'i').test(builder) && !new RegExp(ah.cfg('devswarm_prompt.uuid_full_re'), 'i').test(builder)) return 'defer';
    if (!ctNoUnread(builder, home)) return 'defer';
    var plan = ctPlanSegment(builder, home, root, cli);
    // the blocks of the turn
    var idTok = ah.cfg('devswarm_prompt.id_token');
    function sub(t) { return ctSafeId(builder) ? t.split(idTok).join(builder) : t; }
    var block = [ah.cfg('devswarm_prompt.child_override'), ah.cfg('devswarm_prompt.child_autonomy'),
      sub(text.render(ah.cfg('devswarm_prompt.child_reminder'), { cli: cli })), sub(text.render(ah.cfg('devswarm_prompt.child_receive'), { cli: cli }))];
    var every = ah.settings.num('verify_first.num_repeat_every');
    var staticOpts = { sessionId: sid, key: ah.cfg('devswarm_prompt.child_static_key'), content: block.join(ah.cfg('devswarm_prompt.child_block_joiner')), transcriptPath: tp, keepaliveTurns: isFinite(every) && every > 0 ? every : 0 };
    var planOpts = plan === null ? null : { sessionId: sid, key: ah.cfg('devswarm_prompt.child_plan_key'), content: plan, transcriptPath: tp, keepaliveTurns: ah.cfgNum('devswarm_prompt.child_plan_keepalive') };
    var dedupeOn = sid !== null && !dedupe.disabled();
    if (dedupeOn && (!dedupe.canDecide(staticOpts) || (planOpts !== null && !dedupe.canDecide(planOpts)))) return 'defer';
    // writes: the heartbeat first (its failure never stops the turn), then the dedupe records
    ah.commit();
    var key = ctHeartbeatKey(builder, branch);
    var beat = JSON.stringify({ ts: Math.floor(ah.clock.now()), source: ah.cfg('devswarm_prompt.heartbeat_source'), branch: branch, repoId: ah.env.get(ah.cfg('devswarm_prompt.repo_env')) || null,
      builderId: builder || null, builderName: ah.env.get(ah.cfg('devswarm_prompt.builder_name_env')) || null, sessionId: sid });
    try { ah.state.writeAtomic(ah.cfg('script.write_root') + '/' + ah.cfg('devswarm_prompt.heartbeat_dir') + '/' + key + '.json', beat); } catch (e) { /* never stops a turn */ }
    var segments = [];
    if (!dedupeOn || dedupe.shouldEmit(staticOpts)) segments = segments.concat(block);
    if (plan !== null && (!dedupeOn || dedupe.shouldEmit(planOpts))) segments.push(plan);
    if (!segments.length) return 'allow';
    return { advisory: text.advisoryJson(ah.cfg('devswarm_prompt.event'), segments.join(ah.cfg('devswarm_prompt.child_block_joiner'))) };
  });
}
