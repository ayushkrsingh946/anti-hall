// check = "devswarm-child-turn" (UserPromptSubmit). Mirrors hooks/devswarm-child-turn.js. It answers every case where Node prints and writes
// nothing (a Jev judge child, not a child workspace, the integration killed or switched off, DevSwarm inactive) and every active child's
// turn: the heartbeat (with the git push state when the child runs in a checkout), the registration of the child's descriptor and registry
// row under its lock (host_childturn.rs), the ingest-health banners, the fixed reminder block (through the emit-dedupe store), the step
// plan, the unread parent mail of the durable inbox and the archive request. What stays with Node, each decided before anything is written
// or (for the mesh-direct count) before the first emit: an archived workspace, a truncated builder id, an ingest daemon that is not
// healthy (the banners), a mesh-direct unread count (the reader-identity correction), a project whose store Node would first create or
// that holds a second row of the worktree or a phantom descriptor, and a workspace lock that stays busy.
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

// `unreadInfo(env, home)`: the unread lines of the child's own durable inbox (`workspaces/<id>.json` -> inboxPath / cursorPath), or null
// when there are none, the inbox or its cursor cannot be read, or the descriptor names no inbox. A file the engine cannot read whole, and
// a relative path (Node resolves it against its own working directory), are Node's.
function ctUnread(id, home) {
  if (!ctSafeId(id)) return null;
  var raw = ctRead(home + '/' + ah.cfg('paths.base_dir') + '/' + ah.cfg('devswarm_prompt.descriptor_dir') + '/' + id + '.json');
  if (raw === undefined) throw gates.UNDECIDABLE;
  if (raw === null) return null;
  var desc;
  try { desc = JSON.parse(raw); } catch (e) { return null; }
  if (!desc || typeof desc !== 'object' || !desc.inboxPath) return null;
  if (typeof desc.inboxPath !== 'string' || (desc.cursorPath !== undefined && desc.cursorPath !== null && typeof desc.cursorPath !== 'string')) throw gates.UNDECIDABLE;
  if (desc.inboxPath.charAt(0) !== '/' || (desc.cursorPath && desc.cursorPath.charAt(0) !== '/')) throw gates.UNDECIDABLE;
  var inbox = ctRead(desc.inboxPath);
  if (inbox === undefined) throw gates.UNDECIDABLE;
  if (inbox === null) return null; // missing or unreadable: the count is not known, so nothing is said
  if (!desc.cursorPath) return null;
  var cur = ctRead(desc.cursorPath);
  if (cur === undefined) throw gates.UNDECIDABLE;
  if (cur === null) return null;
  var trimmed = cur.trim(), cursor;
  if (/^\d+$/.test(trimmed)) cursor = parseInt(trimmed, 10);
  else { try { cursor = Number(JSON.parse(trimmed).line); } catch (e) { return null; } }
  if (!isFinite(cursor) || cursor < 0) return null;
  var lines = inbox.split('\n').filter(function (l) { return l.trim() !== ''; }).slice(cursor);
  return lines.length > 0 ? { id: id, count: lines.length, lines: lines } : null;
}

// `buildPlanSegment(id, home, null, env)` of the hook: the child's step plan as a reminder, null for none. null for anything that throws.
function ctPlanSegment(id, home, root, cli, wroot) {
  try {
    if (!ctSafeId(id)) return null;
    if (gates.setting('devswarm_prompt.plan_tracking_setting', true, root) === false) return null;
    var plan = null;
    if (wroot) {
      // in a checkout the plan is found by the worktree's key first, then by id (`findPlan`)
      var fp = JSON.parse(ahHost.ctFindPlan(id, wroot));
      if (fp.unsure) throw gates.UNDECIDABLE;
      plan = fp.plan === null ? null : JSON.parse(fp.plan);
    } else {
      var raw = ctRead(home + '/' + ah.cfg('paths.base_dir') + '/' + ah.cfg('devswarm_prompt.plan_dir') + '/' + id + '.json');
      if (raw === undefined) throw gates.UNDECIDABLE;
      if (raw !== null) { try { plan = JSON.parse(raw); } catch (e) { plan = null; } }
    }
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

// `formatRelative(ts, now)` of companion/lib/ingest-health.js.
function ctRelative(ts, now) {
  if (typeof ts !== 'number' || !isFinite(ts) || ts <= 0) return ah.cfg('devswarm_prompt.relative_none');
  var secs = Math.floor(Math.max(0, now - ts) / 1000);
  if (secs < 60) return secs + 's';
  var mins = Math.floor(secs / 60);
  if (mins < 60) return mins + 'm';
  var hours = Math.floor(mins / 60);
  return hours < 24 ? hours + 'h' : Math.floor(hours / 24) + 'd';
}

// `launchdDisabledLabel(repoKey)`: the label of this project's ingest job when launchd lists it disabled (macOS only; never under a test).
function ctDisabledLabel(key) {
  if (ahHost.platform() !== ah.cfg('devswarm_prompt.launchd_platform') || !key) return null;
  var marks = ah.cfg('devswarm_prompt.test_markers');
  for (var i = 0; i < marks.length; i++) if (ah.env.get(marks[i])) return null;
  var r = ah.exec('launchctl', ah.cfg('devswarm_prompt.launchctl_args').map(function (a) { return a.replace('{uid}', String(ah.uid())); }), { timeoutMs: ah.cfgNum('devswarm_prompt.launchctl_timeout_ms') });
  if (r === null || r.status !== 0 || r.truncated) return null;
  var label = ah.cfg('devswarm_prompt.launchd_label_prefix') + key;
  var esc = label.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return new RegExp('"' + esc + '"\\s*=>\\s*(?:disabled|true)\\b').test(r.stdout) ? label : null;
}

// `normalizeStaleBanner` / `normalizeMonitorFaultBanner`: the age is bucketed (or masked) before the text is hashed, so a banner that only grew older is a repeat.
function ctNormStale(t) {
  return String(t).replace(/last alive (—|\d+)([smhd]?) ago/, function (m, n, u) {
    var tier = 'unknown';
    if (u) {
      var v = Number(n);
      if (u === 's' || (u === 'm' && v < 15)) tier = 't0';
      else if (u === 'm') tier = 't1';
      else if (u === 'h') tier = v < 6 ? 't2' : 't3';
      else tier = 't4';
    }
    return 'last alive ' + tier + ' ago';
  });
}
function ctNormFault(t) {
  return String(t).replace(/(—|\d+[smhd]?) ago/g, 'N ago').replace(/\d+x in a row/g, 'Nx in a row');
}

// The ingest-daemon banner of this turn: {content, opts, forget}. content is the text (null for none) and opts the emit-dedupe options of
// its record; forget lists the dedupe keys of episodes that are over. A hand-over in progress (a fresh heartbeat of a live lock) says nothing.
function ctIngestBanner(h, key, now, sid, tp) {
  var o = { sessionId: sid, transcriptPath: tp, keepaliveTurns: ah.cfgNum('devswarm_prompt.banner_keepalive') };
  if (h.status === 'failed') {
    var f = h.fault, what;
    var rel = function (t) { return ctRelative(t, now); };
    if (f.noOkSinceStart) what = text.render(ah.cfg('devswarm_prompt.fault_no_ok'), { rel: rel(f.startedAtMs) });
    else if (f.okStale) what = text.render(ah.cfg('devswarm_prompt.fault_ok_stale'), { rel: rel(f.lastOkMs) });
    else what = text.render(ah.cfg('devswarm_prompt.fault_failing'), { n: typeof f.consecutive === 'number' && isFinite(f.consecutive) ? f.consecutive : '?' });
    var err = f.error ? text.render(ah.cfg('devswarm_prompt.fault_error'), { error: String(f.error).slice(0, ah.cfgNum('devswarm_prompt.fault_error_max')) }) : '';
    o.key = ah.cfg('devswarm_prompt.fault_key'); o.normalize = ctNormFault;
    o.content = text.render(ah.cfg(f.code ? 'devswarm_prompt.fault_config' : 'devswarm_prompt.fault_banner'), { beat: rel(f.heartbeatTs), what: what, err: err, code: f.code || '' });
    return { content: o.content, opts: o, forget: [] };
  }
  if (h.status !== 'stale') return { content: null, opts: null, forget: [ah.cfg('devswarm_prompt.fault_key'), ah.cfg('devswarm_prompt.stale_key')] };
  var beatTs = typeof h.beatTs === 'number' && isFinite(h.beatTs) ? h.beatTs : null;
  var gone = [ah.cfg('devswarm_prompt.fault_key')];
  if (beatTs !== null && now - beatTs < ah.cfgNum('devswarm_prompt.banner_grace_ms') && h.liveLock) return { content: null, opts: null, forget: gone };
  var label = ctDisabledLabel(key);
  var tail = label ? text.render(ah.cfg('devswarm_prompt.stale_disabled'), { label: label, uid: String(ah.uid()) }) : ah.cfg('devswarm_prompt.stale_doctor');
  o.key = ah.cfg('devswarm_prompt.stale_key'); o.normalize = ctNormStale;
  o.content = text.render(ah.cfg('devswarm_prompt.stale_banner'), { age: ctRelative(beatTs, now), tail: tail });
  return { content: o.content, opts: o, forget: gone };
}

// `buildMeshDirectSegment(count, id, urgencyMax, oldestTs)`: the nudge for mesh mail addressed to this child, the loudest wording for an urgent one.
function ctMeshDirect(count, id, entry, cli) {
  var oldest = typeof entry.oldestDirectUnreadTs === 'number' && isFinite(entry.oldestDirectUnreadTs) ? entry.oldestDirectUnreadTs : null;
  var age = '';
  if (oldest !== null && oldest > 0) {
    var delta = ah.clock.now() - oldest;
    if (delta >= 0) age = text.render(ah.cfg('devswarm_prompt.mesh_direct_age'), { minutes: Math.floor(delta / ah.cfgNum('devswarm_prompt.ms_per_minute')) });
  }
  var urgent = ah.cfg('devswarm_prompt.urgent_levels').indexOf(entry.urgencyMax || null) >= 0;
  return text.render(ah.cfg(urgent ? 'devswarm_prompt.mesh_direct_urgent' : 'devswarm_prompt.mesh_direct'), { count: count, age: age, cli: cli, id: id });
}

// `gitPushState(worktree)` of companion/lib/devswarm-git-truth.js: {noUpstream, unpushed}, or null when the probe did not complete or the
// directory is no repository (an unknown, not a fact).
function ctPushState(top) {
  var r = ah.exec('git', ['-C', top].concat(ah.cfg('devswarm_prompt.git_push_args')), { env: ah.cfg('devswarm_prompt.git_env'), timeoutMs: ah.cfgNum('devswarm_prompt.git_timeout_ms') });
  if (r === null || r.status === null || r.truncated) return null;
  if (r.status !== 0) {
    if (new RegExp(ah.cfg('devswarm_prompt.not_repo_re'), 'i').test(String(r.stderr || ''))) return null;
    return { noUpstream: true, unpushed: null };
  }
  var n = parseInt(String(r.stdout || '').trim(), 10);
  if (!isFinite(n) || n < 0) return null;
  return { noUpstream: false, unpushed: n };
}

// `emit-dedupe.forget`: drop the record of a block that is no longer announced, so its next occurrence is a new episode.
function ctForget(sid, key) {
  if (sid === null || dedupe.disabled()) return;
  var rel = dedupe.stateRel(sid), state = dedupe.readState(rel);
  if (!Object.prototype.hasOwnProperty.call(state, key)) return;
  delete state[key];
  ah.state.writeAtomic(rel, JSON.stringify(state));
}

// Whether the archive marker or the DevSwarm app names this child's workspace archived (`isArchivedChildTurn`): true / false, null when only Node can say.
function ctArchivedNow(builder, top, sid) {
  if (!gates.setting('devswarm_prompt.archived_stop_setting', true, undefined)) return false;
  var r = JSON.parse(ahHost.ctArchived(builder, top, sid));
  return r.unsure ? null : r.archived;
}

// Hand the turn to the Node hook, saying why in the engine's event log (a deferral costs a Node start, so every cause is countable).
function ctDefer(why) {
  ah.log(ah.cfg('devswarm_prompt.defer_log_kind'), why);
  return 'defer';
}

function ctDecide(p, opts) {
  if (devswarmGate.silent(true, 'devswarm_prompt.child_setting')) return 'allow';
  return gates.run(function () {
    // An active child. Its working directory must be known.
    var cwd = jx.isObj(p) ? p.cwd : undefined;
    if (typeof cwd !== 'string' || cwd === '' || cwd.charAt(0) !== '/') return ctDefer('cwd');
    var c = ah.repo.context(cwd);
    if (c.unsure) return ctDefer('repo-context');
    var top = c.toplevel, wroot = c.root; // both null: no git checkout around the working directory
    var guard = ah.homeGuard();
    if (guard.status !== 'ok') return ctDefer('home');
    var home = guard.home;
    var root = gates.pluginRoot(opts);
    var cli = ctCli(opts, home);
    if (cli === null) return ctDefer('cli');
    var sidRaw = jx.isObj(p) ? p.session_id : undefined;
    if (sidRaw && typeof sidRaw !== 'string') return ctDefer('session-id'); // a number or an object as a session id: Node keys the store on its text
    var sid = typeof sidRaw === 'string' && sidRaw !== '' ? sidRaw : null;
    var tpRaw = jx.isObj(p) ? p.transcript_path : undefined;
    if (tpRaw && typeof tpRaw !== 'string') return ctDefer('transcript-path');
    var tp = typeof tpRaw === 'string' && tpRaw !== '' ? tpRaw : null;
    if (tp !== null && tp.charAt(0) !== '/') return ctDefer('transcript-path');
    var builder = ah.env.get(ah.cfg('devswarm_prompt.builder_env'));
    var branch = ah.env.get(ah.cfg('devswarm_prompt.branch_env'));
    // an id shaped like a cut-short UUID makes Node say so on stderr while it registers the descriptor
    if (ctSafeId(builder) && sid !== null && new RegExp(ah.cfg('devswarm_prompt.uuid_truncated_re'), 'i').test(builder) && !new RegExp(ah.cfg('devswarm_prompt.uuid_full_re'), 'i').test(builder)) return ctDefer('truncated-builder-id');
    var inCheckout = top !== null;
    var reg = inCheckout && ctSafeId(builder) && sid !== null; // the hook registers the child's descriptor and registry row
    var wkey = null, banner = null;
    if (inCheckout) {
      if (reg) {
        var arch = ctArchivedNow(builder, top, sid);
        if (arch !== false) return ctDefer(arch === null ? 'archived-unsure' : 'archived'); // an archived workspace gets the banner, the handover marker and the metrics from Node
        var prep = JSON.parse(ahHost.ctPrepare(cwd, builder, sid));
        if (prep.unsure) return ctDefer('prepare');
      }
      // the ingest daemon of the project: its banner (once per episode) when it is stopped or its monitor fails
      wkey = wroot === null ? null : ah.mesh.repoKey(wroot);
      if (wkey !== null) {
        var health = JSON.parse(ahHost.ctIngest(wkey));
        if (health.unsure) return ctDefer('ingest-health');
        banner = ctIngestBanner(health, wkey, ah.clock.now(), sid, tp);
      }
    }
    var plan = ctPlanSegment(builder, home, root, cli, wroot);
    // the blocks of the turn
    var idTok = ah.cfg('devswarm_prompt.id_token');
    function sub(t) { return ctSafeId(builder) ? t.split(idTok).join(builder) : t; }
    var block = [ah.cfg('devswarm_prompt.child_override'), ah.cfg('devswarm_prompt.child_autonomy'),
      sub(text.render(ah.cfg('devswarm_prompt.child_reminder'), { cli: cli })), sub(text.render(ah.cfg('devswarm_prompt.child_receive'), { cli: cli }))];
    var every = ah.settings.num('verify_first.num_repeat_every');
    var staticOpts = { sessionId: sid, key: ah.cfg('devswarm_prompt.child_static_key'), content: block.join(ah.cfg('devswarm_prompt.child_block_joiner')), transcriptPath: tp, keepaliveTurns: isFinite(every) && every > 0 ? every : 0 };
    var planOpts = plan === null ? null : { sessionId: sid, key: ah.cfg('devswarm_prompt.child_plan_key'), content: plan, transcriptPath: tp, keepaliveTurns: ah.cfgNum('devswarm_prompt.child_plan_keepalive') };
    var dedupeOn = sid !== null && !dedupe.disabled();
    if (dedupeOn && ((banner !== null && banner.opts !== null && banner.content !== null && !dedupe.canDecide(banner.opts)) || !dedupe.canDecide(staticOpts) || (planOpts !== null && !dedupe.canDecide(planOpts)))) return ctDefer('dedupe-undecidable');
    // the durable inbox as it is now: a path or a file the engine cannot read as Node does hands the turn over before anything is written
    var preInfo;
    try { preInfo = ctUnread(builder, home); } catch (e) { if (e === gates.UNDECIDABLE) return ctDefer('inbox'); throw e; }
    // the push state of the checkout (a read), then the writes: the heartbeat first (its failure never stops the turn), the registration, the dedupe records
    var push = inCheckout ? ctPushState(top) : null;
    ah.commit();
    var key = ctHeartbeatKey(builder, branch);
    var beatObj = { ts: Math.floor(ah.clock.now()), source: ah.cfg('devswarm_prompt.heartbeat_source'), branch: branch, repoId: ah.env.get(ah.cfg('devswarm_prompt.repo_env')) || null,
      builderId: builder || null, builderName: ah.env.get(ah.cfg('devswarm_prompt.builder_name_env')) || null, sessionId: sid };
    if (push !== null) { beatObj.noUpstream = push.noUpstream; beatObj.unpushed = push.unpushed; }
    try { ah.state.writeAtomic(ah.cfg('script.write_root') + '/' + ah.cfg('devswarm_prompt.heartbeat_dir') + '/' + key + '.json', JSON.stringify(beatObj)); } catch (e) { /* never stops a turn */ }
    if (reg) {
      var done = JSON.parse(ahHost.ctRegister(cwd, builder, sid));
      if (done.unsure) return ctDefer('register'); // before the first registration write (the lock stayed busy, or the state changed since the plan)
    }
    var bannerText = null;
    if (banner !== null) {
      banner.forget.forEach(function (k) { ctForget(sid, k); });
      if (banner.content !== null && (!dedupeOn || dedupe.shouldEmit(banner.opts))) bannerText = banner.content;
    }
    // the mailbox summary the registration just refreshed: mail addressed to this child by mesh, and a parent's request to archive
    var archiveRequested = false, meshDirect = null;
    if (wkey !== null && ctSafeId(builder)) {
      var spath = home + '/' + ah.cfg('paths.base_dir') + '/' + ah.cfg('devswarm_prompt.summary_dir') + '/' + wkey + '.json';
      var ssize = ah.fs.size(spath);
      if (ssize !== null && ssize > ah.cfgNum('script.read_max_bytes')) throw gates.UNDECIDABLE;
      var sraw = ssize === null ? null : ah.fs.readText(spath);
      if (sraw !== null) {
        var entry = null;
        try { var summary = sraw.trim() ? JSON.parse(sraw.trim()) : null; entry = summary && summary.workspaces && summary.workspaces[builder]; } catch (e) { entry = null; }
        if (entry) {
          var rawDirect = typeof entry.directUnread === 'number' && isFinite(entry.directUnread) ? entry.directUnread : (typeof entry.unread === 'number' && isFinite(entry.unread) ? entry.unread : 0);
          if (rawDirect > 0) {
            // Node corrects the count by this reader's own cursor, which it finds from its process ancestry; the daemon sees another tree. The
            // correction can only change the count when some reader of the partition has a cursor of its own, so only then is the turn Node's.
            var rd = JSON.parse(ahHost.ctReaders(wkey, builder));
            if (rd.unsure || rd.declared) return ctDefer('own-reader');
            meshDirect = ctMeshDirect(rawDirect, builder, entry, cli);
          }
          if (entry.archive_requested) archiveRequested = true;
        }
      }
    }
    var info = reg ? ctUnread(builder, home) : preInfo; // a registration may have created the cursor and the inbox
    var segments = [];
    if (bannerText !== null) segments.push(bannerText);
    if (!dedupeOn || dedupe.shouldEmit(staticOpts)) segments = segments.concat(block);
    if (meshDirect !== null) segments.push(meshDirect);
    if (plan !== null && (!dedupeOn || dedupe.shouldEmit(planOpts))) segments.push(plan);
    var archiveSegment = false;
    if (info !== null) {
      segments.push(text.render(ah.cfg('devswarm_prompt.unread_segment'), { count: info.count, cli: cli, id: info.id }));
      if (info.lines.some(function (l) { return typeof l === 'string' && l.indexOf(ah.cfg('devswarm_prompt.archive_marker')) >= 0; })) {
        segments.push(text.render(ah.cfg('devswarm_prompt.archive_request'), { cli: cli, id: info.id }));
        archiveSegment = true;
      }
    }
    if (archiveRequested && !archiveSegment) segments.push(text.render(ah.cfg('devswarm_prompt.archive_request'), { cli: cli, id: builder }));
    if (!segments.length) return 'allow';
    return { advisory: text.advisoryJson(ah.cfg('devswarm_prompt.event'), segments.join(ah.cfg('devswarm_prompt.child_block_joiner'))) };
  });
}

function decide(p, opts) {
  try { return ctDecide(p, opts); } catch (e) { if (e === meshDefer) return 'defer'; throw e; }
}
