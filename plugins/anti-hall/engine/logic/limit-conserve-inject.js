// check = "limit-conserve-inject" (UserPromptSubmit). Conservation is active when the mode is `on`, or when `auto` and a usage bucket
// of the OMC cache is at or over the threshold and the account-switch guard does not hold the reading stale. Then the directive is
// built and emit-dedupe (lib/74-emit-dedupe.js) decides whether to send it this turn; everything else prints the empty context line.
// The account-switch state file is read and written as Node does. The home is Node's os.homedir() (hookProc.adoptHome: HOME unset
// adopts the passwd home); a relative transcript path is opened for emit-dedupe from the absolute payload cwd; a number or boolean reset
// time is read by Date exactly as V8 reads it. Only what the engine cannot reproduce exactly defers, before anything is written, logged
// under ctxbudget.ah_unsure_log_kind (cb.defer): a relative or empty HOME, no home, the real home under a test marker, a relative path
// with no absolute cwd, a usage cache, ~/.claude.json or account state over the read cap, an object reset time, an emit-dedupe
// timestamp only V8 reads. Mirrors hooks/limit-conserve-inject.js and hooks/limit-conserve.js. Keys and texts: ctxbudget.toml (ctxbudget.*).
'use strict';

function lcEmpty() { return { exact: { code: 0, out: ah.cfg('ctxbudget.ups_empty'), err: '' } }; }

// A JSON file under the home directory: {v} (parsed), {bad: true} (absent or not JSON), or {big: true} (too large to read here).
function lcReadJson(path) {
  var size = ah.fs.size(path);
  if (size === null) return { bad: true };
  if (size > ah.cfgNum('script.read_max_bytes')) return { big: true };
  var raw = ah.fs.readText(path);
  if (raw === null) return { bad: true };
  try { return { v: JSON.parse(raw) }; } catch (e) { return { bad: true }; }
}

// The account-switch guard: true when the logged-in account changed and the usage cache has not been refreshed since. Writes the
// account state where Node does.
function lcAccountStale(home) {
  if (!ah.settings.bool('ctxbudget.set_limit_account_check')) return false;
  var cj = lcReadJson(home + '/' + ah.cfg('ctxbudget.claude_json'));
  if (cj.big) cb.defer('claude_json_big');
  var user = cj.v && typeof cj.v.userID === 'string' ? cj.v.userID : null;
  if (user === null) return false;
  var accountRel = ah.cfg('ctxbudget.account_state'), acc = lcReadJson(home + '/' + accountRel);
  if (acc.big) cb.defer('account_big');
  var stored = acc.v && typeof acc.v === 'object' && typeof acc.v.userID === 'string' && typeof acc.v.usageCacheMtime === 'number' ? acc.v : null;
  var mtime = ah.fs.mtimeMs(home + '/' + ah.cfg('ctxbudget.usage_cache'));
  var write = function (m) { try { ah.state.writeAtomic(accountRel, text.render(ah.cfg('ctxbudget.lc_account_json'), { user: JSON.stringify(user), mtime: m === null ? ah.cfg('ctxbudget.json_null') : String(m) })); } catch (e) { /* best effort */ } };
  if (!stored) { write(mtime); return false; }
  if (stored.userID !== user) {
    if (mtime !== null && mtime <= stored.usageCacheMtime) return true;
    write(mtime);
    return false;
  }
  if (mtime !== null && mtime !== stored.usageCacheMtime) write(mtime);
  return false;
}

// The active conservation {reason, resetsAt}, or null when none.
function lcConserving(home) {
  var mode = ah.settings.enum('ctxbudget.set_limit_mode');
  if (mode === 'on') return { reason: ah.cfg('ctxbudget.lc_reason_manual'), resetsAt: null };
  if (mode === 'off') return null;
  var threshold = ah.settings.num('ctxbudget.set_limit_threshold');
  var cache = lcReadJson(home + '/' + ah.cfg('ctxbudget.usage_cache'));
  if (cache.big) cb.defer('usage_big');
  if (cache.bad) return null;
  var parsed = cache.v, now = ah.clock.now();
  if (!parsed || typeof parsed !== 'object' || !parsed.data || typeof parsed.data !== 'object') return null;
  var d = parsed.data, ts = typeof parsed.timestamp === 'number' ? parsed.timestamp : 0;
  var effective = function (pct, resetsAt) {
    if (resetsAt && typeof resetsAt === 'string') {
      var rt = new Date(resetsAt).getTime();
      if (isFinite(rt) && rt < now) return 0;
    } else if (ts > 0 && (now - ts) > ah.cfgNum('ctxbudget.usage_max_stale_ms')) return 0;
    return typeof pct === 'number' ? pct : 0;
  };
  var pcts = ah.cfg('ctxbudget.lc_bucket_pct'), resets = ah.cfg('ctxbudget.lc_bucket_resets'), names = ah.cfg('ctxbudget.lc_bucket_trip');
  var trips = [], candidates = [];
  for (var i = 0; i < pcts.length; i++) {
    if (effective(d[pcts[i]], d[resets[i]]) < threshold) continue;
    trips.push(names[i]);
    if (d[resets[i]]) {
      // a number or boolean is read by Date exactly as V8 reads it; an object or array goes through its text, which V8's legacy parser decides
      if (typeof d[resets[i]] === 'object') cb.defer('resets_object');
      candidates.push(d[resets[i]]);
    }
  }
  if (!trips.length) return null;
  if (lcAccountStale(home)) return null;
  var finite = candidates.filter(function (s) { return isFinite(new Date(s).getTime()); });
  finite.sort(function (a, b) { return new Date(a).getTime() - new Date(b).getTime(); });
  return { reason: trips.join('+'), resetsAt: finite.length ? finite[0] : null };
}

// The reset time at minute precision (Node's minuteRounded): the cache's millisecond jitter would change the text every turn.
function lcMinuteRounded(iso) {
  var r = ah.cfg('ctxbudget.lc_reset_round'), t = new Date(iso).getTime();
  if (!isFinite(t)) return String(iso);
  return new Date(Math.round(t / r.ms) * r.ms).toISOString().replace(r.cut, r.to);
}

function decide(p) { return cb.run(function () { return lcDecide(p); }); }

function lcDecide(p) {
  if (ah.env.get(ah.cfg('ctxbudget.judge_child_env')) === ah.cfg('ctxbudget.judge_child_on')) return 'allow';
  var noHome = hookProc.adoptHome();
  if (noHome !== null) cb.defer(noHome);
  var home = ah.home();
  if (ah.settings.skipped(ah.cfg('ctxbudget.skip_limit_conserve'))) return lcEmpty();
  var tp = p && typeof p.transcript_path === 'string' && p.transcript_path !== '' ? p.transcript_path : null;
  var active = lcConserving(home);
  if (active === null) return lcEmpty();
  var resets = active.resetsAt !== null ? text.render(ah.cfg('ctxbudget.lc_resets_at'), { at: lcMinuteRounded(active.resetsAt) }) : ah.cfg('ctxbudget.lc_resets_next');
  var built = text.message('warn', ah.cfg('ctxbudget.lc_guard'), {
    what: text.render(ah.cfg('ctxbudget.lc_what'), { reason: active.reason }), why: ah.cfg('ctxbudget.lc_why'),
    instead: ah.cfg('ctxbudget.lc_instead') + resets + ' ' + ah.cfg('ctxbudget.lc_downshift'),
  });
  var sid = p && typeof p.session_id === 'string' ? p.session_id : null;
  var emit = true;
  if (sid !== null) {
    var o = { sessionId: sid, transcriptPath: tp, readPath: tp !== null ? cb.open(p, tp) : null, key: ah.cfg('ctxbudget.lc_dedupe_key'), content: built, keepaliveTurns: ah.cfgNum('ctxbudget.lc_keepalive_turns') };
    if (!dedupe.canDecide(o)) cb.defer('dedupe_unsure'); // a transcript timestamp only V8 reads
    emit = dedupe.shouldEmit(o);
  }
  return emit ? { exact: { code: 0, out: text.render(ah.cfg('ctxbudget.ups_line'), { text: JSON.stringify(built) }), err: '' } } : lcEmpty();
}
