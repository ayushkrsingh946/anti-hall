// check = "devswarm-child-role" (SessionStart; devswarm-parent-gate.js and devswarm-child-drain.js build on this script). Injects the
// DevSwarm mesh-only messaging directive for a child workspace. The Node hook installed the stable launchers when it was loaded; where they are not current the
// script names the launcher targets (dwLaunchersNative, counted). A Primary in a checkout (it adopts or registers its seat in the mesh store)
// gets the directive without a seat (counted). Mirrors `main` of
// hooks/devswarm-child-role.js and the module-level launcher install above it. Keys and texts: devswarm_role.toml (devswarm_role.*).
'use strict';

function dwRealHomeUnderTest(home) {
  var marked = ah.cfg('devswarm_role.test_markers').some(function (m) { var v = ah.env.get(m); return v !== null && v !== ''; });
  var escape = ah.env.get(ah.cfg('devswarm_role.real_home_escape'));
  if (!marked || (escape !== null && escape !== '')) return false;
  var real = ah.env.passwdHome(), strip = function (p) { return posix.normalize(p).replace(/\/+$/, ''); };
  return real === null || strip(real) === strip(home);
}

// The home directory of the settings reads and the file paths, when it is one this script can use exactly as Node does: HOME set,
// absolute, already normalized (Node joins paths lexically), and not the real home under a test marker (where the Node settings reader
// refuses). null otherwise.
function dwUsableHome() {
  var home = ah.env.get(ah.cfg('env.home'));
  if (home === null || home.charAt(0) !== '/' || posix.normalize(home) !== home) return null;
  return dwRealHomeUnderTest(home) ? null : home;
}

function dwJudgeChild() { return ah.env.get(ah.cfg('devswarm_role.judge_env')) === '1'; }
function dwNonBlank(name) { var v = ah.env.get(name); return v !== null && v.trim() !== ''; }

// `isDevswarmActive(env)`: the kill switch, then the supervisor mode (`on`, `off` or detect from the environment).
function dwActive() {
  if (ah.env.get(ah.cfg('devswarm_role.kill_env')) === '1') return false;
  var mode = ah.settings.enum('devswarm_role.sw_supervisor_mode');
  if (mode === 'off') return false;
  if (mode === 'on') return true;
  return dwNonBlank(ah.cfg('devswarm_role.repo_env'));
}

// The real path of the plugin root's parent of `hooks`, which is where a Node hook's `path.join(__dirname, '..')` points.
function dwNodeRoot(root) {
  var hooks = ah.fs.realpath(root + '/' + ah.cfg('devswarm_role.hooks_dir'));
  return hooks === null ? null : ah.path.resolveAbs(hooks + '/..');
}

// The source `buildLauncherSource(segments, fallback)` generates for the script at `target` whose absolute path at generation time is `fallback`.
function dwLauncherSource(target, fallback) {
  return text.render(ah.cfg('devswarm_role.launcher_src'), { segments: JSON.stringify(target.split('/')), fallback: JSON.stringify(fallback) });
}

// The path of a launcher the Node hook would install, when it is already there with exactly the content Node would write; null otherwise.
function dwCurrentLauncher(home, root, key) {
  var l = ah.cfg(key), dest = posix.normalize(home + '/' + ah.cfg('devswarm_role.bin_dir') + '/' + l.name);
  var have = ah.fs.readText(dest);
  return have !== null && have === dwLauncherSource(l.target, posix.normalize(root + '/' + l.target)) ? dest : null;
}

// {home, cli, watcher} when the launchers Node would install are current (or its stable-launcher switch is off, when they are the
// plugin's own scripts); null when Node would have to install them or the answer is not exact.
function dwLaunchers(root, wantWatcher) {
  var home = dwUsableHome();
  if (home === null || !root) return null;
  var nr = dwNodeRoot(root);
  if (nr === null) return null;
  if (!ah.settings.bool('devswarm_role.sw_stable_launcher')) {
    return { home: home, cli: posix.normalize(nr + '/' + ah.cfg('devswarm_role.launcher_cli').target), watcher: posix.normalize(nr + '/' + ah.cfg('devswarm_role.launcher_watcher').target) };
  }
  var cli = dwCurrentLauncher(home, nr, 'devswarm_role.launcher_cli');
  if (cli === null) return null;
  var watcher = dwCurrentLauncher(home, nr, 'devswarm_role.launcher_watcher');
  if (watcher === null && wantWatcher) return null;
  return { home: home, cli: cli, watcher: watcher };
}

// `dwLaunchers`, or where the stable launchers are not current (Node installed them when its hook loaded; the engine's update stages
// do that now) the launcher targets themselves, named from the plugin root (counted). null only without a usable home or plugin root.
function dwLaunchersNative(root, wantWatcher) {
  var l = dwLaunchers(root, wantWatcher);
  if (l !== null) return l;
  var home = dwUsableHome(), nr = root ? dwNodeRoot(root) : null;
  if (home === null || nr === null) return null;
  ah.unable('devswarm-launchers:not_current');
  return { home: home, cli: posix.normalize(nr + '/' + ah.cfg('devswarm_role.launcher_cli').target), watcher: posix.normalize(nr + '/' + ah.cfg('devswarm_role.launcher_watcher').target) };
}

// `dwLaunchers` for the read side of a hook that needs only that the launchers are current.
function dwLaunchersCurrent(root, wantWatcher) {
  var l = dwLaunchers(root, wantWatcher);
  return l === null || (wantWatcher && l.watcher === null) ? null : l;
}

// `wakeCron(env)` of hooks/lib/devswarm-wake.js: the configured schedule when it is exactly the right number of whitespace-separated
// fields made only of cron characters (re-joined with single spaces), else the default.
function dwWakeCron() {
  var dflt = ah.cfg('devswarm_role.sw_wake_cron').default, expr = ah.settings.str('devswarm_role.sw_wake_cron').trim();
  if (expr === '') return dflt;
  var fields = expr.split(/\s+/).filter(function (f) { return f !== ''; }), charset = ah.cfg('devswarm_role.cron_charset');
  if (fields.length !== ah.cfgNum('devswarm_role.cron_fields') || !fields.every(function (f) { return f.split('').every(function (c) { return charset.indexOf(c) >= 0; }); })) return dflt;
  return fields.join(' ');
}

function dwChildContext(cli, watcher, agent, id, cron, tickOnly) {
  return dwContext(true, cli, watcher, agent, id, cron, tickOnly);
}

// The directive of a child workspace (`child`) or of a Primary: the same wake text but for the drain command it names.
function dwContext(child, cli, watcher, agent, id, cron, tickOnly) {
  var out = text.render(ah.cfg(child ? 'devswarm_role.msg_base' : 'devswarm_role.msg_base_parent'), { cli: cli });
  if (agent === '') return out;
  if (agent === ah.cfg('devswarm_role.claude_agent')) {
    var expiry = ah.cfg(tickOnly ? 'devswarm_role.msg_expiry_tick' : 'devswarm_role.msg_expiry_inline');
    return out + text.render(ah.cfg(child ? 'devswarm_role.msg_wake_claude' : 'devswarm_role.msg_wake_claude_parent'), { cli: cli, watcher: watcher, id: id, cron: cron, expiry: expiry });
  }
  return out + text.render(ah.cfg(child ? 'devswarm_role.msg_wake_other' : 'devswarm_role.msg_wake_other_parent'), { cli: cli, id: id, agent: agent });
}

// `primarySeatResult(payload)` of hooks/devswarm-child-role.js for the Primaries that have no seat to settle: true (no notices, no seat id),
// or false when the seat needs the mesh store, the app database or a session liveness check (a Primary in a git checkout): the seat is
// then not adopted or registered by this script (counted), the directive is still said. Without a session id the hook looks at nothing;
// with one, a working directory that is no checkout has the verdict `n/a`.
function dwNoSeat(p) {
  var sid = jx.isObj(p) && typeof p.session_id === 'string' ? p.session_id : '';
  if (sid === '') return true;
  var cwd = p.cwd;
  if (typeof cwd !== 'string' || cwd === '') cwd = hookProc.cwd(p); // Node uses its own working directory: the stand-in
  else if (cwd.charAt(0) !== '/') cwd = hookProc.cwd(p);
  var c = ah.repo.context(cwd);
  return !c.unsure && c.root === null;
}

function decide(p, opts, event) {
  if (event !== 'SessionStart') return 'allow'; // this script answers SessionStart only
  if (dwJudgeChild()) return 'allow';
  var root = jx.isObj(opts) && typeof opts.plugin_root === 'string' ? opts.plugin_root : ah.env.get(ah.cfg('env.plugin_root'));
  var l = dwLaunchersNative(root, true);
  if (l === null) { ah.unable('devswarm-child-role:no_home_or_root'); return 'allow'; } // no usable home or plugin root: no launcher can be named
  if (!ah.settings.bool('devswarm_role.sw_child_role') || !dwActive()) return 'allow';
  var child = dwNonBlank(ah.cfg('devswarm_role.branch_env'));
  if (!child && !dwNoSeat(p)) ah.unable('devswarm-child-role:seat_not_registered'); // a Primary in a checkout: its seat is adopted or registered elsewhere
  var agentRaw = ah.env.get(ah.cfg('devswarm_role.agent_env')) || '';
  if (/[^\x00-\x7f]/.test(agentRaw)) ah.unable('devswarm-child-role:agent_non_ascii'); // only the ASCII letters are lower-cased: the agent name is matched as given
  var agent = agentRaw.trim().replace(/[A-Z]/g, function (c) { return c.toLowerCase(); });
  var rawId = ah.env.get(ah.cfg('devswarm_role.builder_env')), extra = ah.cfg('devswarm_role.id_extra_chars');
  var id = rawId !== null && rawId !== '' && rawId.split('').every(function (c) { return /[A-Za-z0-9]/.test(c) || extra.indexOf(c) >= 0; }) ? rawId : ah.cfg('devswarm_role.id_placeholder');
  var ctx = dwContext(child, l.cli, l.watcher, agent, id, dwWakeCron(), ah.settings.bool('devswarm_role.sw_rearm'));
  return { exact: { code: 0, out: ah.cfg('devswarm_role.out_prefix') + JSON.stringify(ctx) + ah.cfg('devswarm_role.out_suffix') + '\n', err: '' } };
}
