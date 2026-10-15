// check = "devswarm-parent-inbox" (UserPromptSubmit). Mirrors hooks/devswarm-parent-inbox.js. It answers every case where Node prints and
// writes nothing (a Jev judge child, a child workspace, the integration killed or switched off, DevSwarm inactive) and, for an active
// Primary, the cases where the hook has nothing to show but its communication-override reminder (through the emit-dedupe store) and, once,
// a seat conflict the SessionStart hook recorded: a working directory with no git checkout around it, and a checkout whose project has no
// summary, no workspace descriptor, no maintainer notice and no app database (no roster, mailbox, wake coverage or drift to show). A
// parked supervisor escalation is a segment of its own, answered here. What stays with Node is decided before anything is written and
// logged with its cause (devswarm_parent.defer_log_kind): see piProject. Keys and texts: small_guards.toml (devswarm_prompt.*),
// devswarm_parent.toml.
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

// Hand the turn to the Node hook, saying why in the engine's event log (a deferral costs a Node start, so every cause is countable).
function piDefer(why) {
  ah.log(ah.cfg('devswarm_parent.defer_log_kind'), why);
  return 'defer';
}

function piDevswarmDir(home) { return home + '/' + ah.cfg('devswarm_role.pg_devswarm_dir'); }

// `JSON.parse(readFileSync(p))` inside Node's try/catch: null for a file that cannot be read or does not parse; a file over the read cap
// or a parse the interpreter cannot finish defers.
function piJson(p) {
  var r = jx.read(p);
  if (r.big) throw meshDefer;
  if (r.text === undefined) return null;
  var j = jx.parse(r.text);
  if (j.unsure) throw meshDefer;
  return j.invalid ? null : j.v;
}

// The sorted names in a directory; null when it is not one. A directory too big to list defers.
function piNames(dir) {
  var n = ah.fs.readdir(dir);
  if (n === null && ah.fs.isDir(dir)) throw meshDefer;
  return n;
}

// Whether a path exists the way `existsSync`-style probes see it: a link that dangles does not.
function piPresent(p) {
  var st = ah.fs.lstat(p);
  return !(st === null || (st.kind === 'link' && ah.fs.realpath(p) === null));
}

// `listEscalationIntents` + `parkedEscalationSegment(home, parentId, cli)`: the supervisor's escalations parked for this Primary, or null.
function piParked(home, primaryId, cli) {
  var dir = piDevswarmDir(home) + '/' + ah.cfg('devswarm_role.pg_dir_escalation');
  var names = piNames(dir);
  if (names === null) return null;
  var mine = [];
  names.forEach(function (n) {
    if (!/\.json$/.test(n)) return;
    var it = piJson(dir + '/' + n);
    if (!it || !it.parentId || !it.row || it.delivered) return;
    if (typeof it.parentId !== 'string') throw meshDefer; // String() of another type
    if (it.parentId === primaryId) mine.push(it);
  });
  if (!mine.length) return null;
  var kids = mine.map(function (i) { if (typeof i.childId !== 'string') throw meshDefer; return i.childId; }).sort();
  return text.render(ah.cfg('devswarm_parent.escalation_banner'), { count: mine.length, kids: kids.map(function (k) { return JSON.stringify(k); }).join(', '), parent: primaryId, cli: cli });
}

// The CLI path the reminders name: the stable launcher when that is switched on and installed, else the plugin's own script (`preferStableLauncher`).
// null when the plugin root cannot be resolved the way Node's `__dirname` would be.
function piCli(opts, home) {
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

// The project of a Primary in a checkout. Returns {parked} (the escalation segment or null) when the hook has nothing else to say, or a
// string: the cause of the deferral. Everything it reads is read before the first write of the turn.
function piProject(home, top, primaryId, cli) {
  var key = ah.mesh.repoKey(top);
  if (key === null) return 'repo-key';
  var dev = piDevswarmDir(home);
  // the ingest daemon: when it is not healthy the hook refreshes the summary from the store (a write), which the engine leaves to Node
  var health = JSON.parse(ahHost.ctIngest(key));
  if (health.unsure) return 'ingest-health';
  if (health.status !== 'healthy' && ah.fs.lstat(dev + '/' + ah.cfg('mesh_write.dir_store') + '/' + key) !== null) return 'summary-refresh';
  // the roster and every segment that stands on the summary
  if (piPresent(dev + '/' + ah.cfg('devswarm_role.pg_dir_summaries') + '/' + key + '.json')) return 'summary';
  // workspace descriptors: the roster, wake coverage and the session drift of the Primary's own anchor
  var names = piNames(dev + '/' + ah.cfg('devswarm_role.pg_dir_workspaces'));
  if (names !== null && names.some(function (n) { return /\.json$/.test(n); })) return 'descriptors';
  // a live child no descriptor names, found through the DevSwarm app's database
  if (ahHost.piAppDb()) return 'app-db';
  if (piPresent(dev + '/' + ah.cfg('devswarm_parent.notices_file'))) return 'maintainer-notice';
  return { parked: piParked(home, primaryId, cli) };
}

function piDecide(p, opts) {
  if (devswarmGate.silent(false, 'devswarm_prompt.parent_setting')) return 'allow';
  // An active Primary. With a working directory the hook resolves the checkout around it; with none it has no project identity either.
  var cwd = jx.isObj(p) && typeof p.cwd === 'string' && p.cwd !== '' ? p.cwd : null;
  var top = null, primaryId = null;
  if (cwd !== null) {
    if (cwd.charAt(0) !== '/') return piDefer('relative-cwd'); // Node resolves it against its own working directory
    var c = ah.repo.context(cwd);
    if (c.unsure) return piDefer('repo-context');
    if (c.root !== null) {
      var idn = ah.mesh.ident(cwd);
      if (idn.top === null || idn.own === null) return piDefer('identity');
      top = idn.top;
      primaryId = idn.own;
    }
  }
  var home = spawn.osHome();
  if (home === null) return piDefer('home');
  var sid = dpSession(p);
  var tp = jx.isObj(p) && typeof p.transcript_path === 'string' ? p.transcript_path : null;
  if (tp !== null && tp !== '' && tp.charAt(0) !== '/') return piDefer('transcript-path'); // a relative transcript path is Node's to resolve
  // the seat marker is read before anything is written: a marker still owed a repeat is Node's
  if (sid !== null && dpSeatMarker(home, sid) === 'repeat') return piDefer('seat-marker');
  var parked = null;
  if (top !== null) {
    var cli = piCli(opts, home);
    if (cli === null) return piDefer('cli');
    var project = piProject(home, top, primaryId, cli);
    if (typeof project === 'string') return piDefer(project);
    parked = project.parked;
  }
  var every = ah.settings.num('verify_first.num_repeat_every');
  var text0 = ah.cfg('devswarm_prompt.override_reassert');
  var o = { sessionId: sid, key: ah.cfg('devswarm_prompt.override_key'), content: text0, transcriptPath: tp, keepaliveTurns: isFinite(every) && every > 0 ? every : 0 };
  var emit = true;
  if (sid !== null && !dedupe.disabled()) {
    if (!dedupe.canDecide(o)) return piDefer('dedupe-undecidable');
    emit = dedupe.shouldEmit(o);
  }
  var segments = [];
  if (emit) segments.push(text0);
  if (parked !== null) segments.push(parked);
  if (!segments.length) return 'allow';
  return { advisory: text.advisoryJson(ah.cfg('devswarm_prompt.event'), segments.join(ah.cfg('devswarm_parent.segment_joiner'))) };
}

function decide(p, opts) {
  try { return piDecide(p, opts); } catch (e) { if (e === meshDefer) return piDefer('unsure'); throw e; }
}
