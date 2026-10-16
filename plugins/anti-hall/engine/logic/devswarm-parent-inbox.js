// check = "devswarm-parent-inbox" (UserPromptSubmit). Mirrors hooks/devswarm-parent-inbox.js. It answers every case where Node prints and
// writes nothing (a Jev judge child, a child workspace, the integration killed or switched off, DevSwarm inactive) and, for an active
// Primary, the turns whose answer it can reproduce exactly: the communication-override reminder through the emit-dedupe store, the
// ingest-daemon banners, the live workspace table of the project's summary with the rows' labels (archive, liveness, app database), the
// nudges for unread backlogs (urgent and normal) and the archive recommendation with their cooldown, trend and acceptance-log state, and
// the supervisor escalations parked for this Primary. What stays with Node is decided before anything is written and logged with its
// cause (devswarm_parent.defer_log_kind): see piDeferrals. Keys and texts: small_guards.toml (devswarm_prompt.*), devswarm_parent.toml.
'use strict';

// ---- shared small readers ----------------------------------------------------------------------------------------------

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
function piRel(rest) { return ah.cfg('devswarm_role.pg_devswarm_dir') + '/' + rest; }

// `isSafeId` of companion/lib/liveness.js.
function piSafeId(id) {
  return typeof id === 'string' && id !== '' && id !== '.' && id !== '..' && id.indexOf('..') < 0 && /^[A-Za-z0-9._-]+$/.test(id);
}

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

// A summary or descriptor field read with JavaScript truthiness as a string: a non-empty string, null for a falsy value; another type defers.
function piStrField(v) {
  if (v === undefined || v === null || v === false || v === '' || v === 0 || (typeof v === 'number' && isNaN(v))) return null;
  if (typeof v === 'string') return v;
  throw meshDefer;
}

function piFinite(v) { return typeof v === 'number' && isFinite(v); }

// ---- the CLI path and the parked escalations --------------------------------------------------------------------------

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

// ---- time and the numbers of the text -----------------------------------------------------------------------------------

// `formatRelative(ts, now)` of the hook: a compact age, the none-word for an unknown time; a future time reads as 0.
function piRelative(ts, now) {
  if (!piFinite(ts) || ts <= 0) return ah.cfg('devswarm_parent.relative_none');
  var delta = now - ts;
  if (delta < 0) delta = 0;
  var s = Math.floor(delta / 1000);
  if (s < 60) return s + 's';
  var m = Math.floor(s / 60);
  if (m < 60) return m + 'm';
  var h = Math.floor(m / 60);
  if (h < 24) return h + 'h';
  return Math.floor(h / 24) + 'd';
}

// `names.shortId(id)` and `names.displayName(id, name)`.
function piShortId(id) {
  var s = typeof id === 'string' ? id : String(id == null ? '' : id);
  return s.length > ah.cfgNum('devswarm_role.pg_short_id_len') ? s.slice(0, ah.cfgNum('devswarm_role.pg_short_id_len')) : s;
}
function piDisplayName(id, name) {
  if (typeof name === 'string' && name) return name + ' (' + piShortId(id) + ')';
  return String(id == null ? '' : id);
}
// `names.readName(home, id)`: the cached title of a workspace, or null.
function piReadName(home, id) {
  if (!piSafeId(id)) return null;
  var v = piJson(piDevswarmDir(home) + '/' + ah.cfg('devswarm_role.pg_dir_names') + '/' + id + '.json');
  return v !== null && typeof v === 'object' && typeof v.name === 'string' && v.name ? v.name : null;
}

function piTruncate(s, max) {
  var str = String(s == null ? '' : s);
  var limit = max || ah.cfgNum('devswarm_parent.title_max');
  return str.length > limit ? str.slice(0, limit) + '…' : str;
}

// A setting the hook reads at its shipped default; defers when it is set anywhere (its value is then Node's to resolve).
function piSetDefault(key) {
  if (ah.settings.touched(key)) throw meshDefer;
  return ah.cfg(key).default;
}

// ---- labels, the finish cell and the markers ---------------------------------------------------------------------------

function piLabel(name) { return ah.cfg('devswarm_parent.labels')[name]; }

// `displayStatus(archiveReady, status, activityTs, now, dormant, notDraining, idleAlive)`.
function piDisplayStatus(archiveReady, status, activityTs, now, dormant, notDraining, idleAlive, idleMs) {
  if (status === 'escalated' && !idleAlive) return piLabel('escalated');
  if ((status === 'stale' || status === 'nudged') && !idleAlive) return piLabel('stale');
  if (notDraining) return piLabel('not_draining');
  if (archiveReady) return piLabel('archive_ready');
  if (idleAlive) return piLabel('idle_alive');
  if (dormant) return piLabel('dormant');
  if (piFinite(activityTs) && piFinite(now) && (now - activityTs) >= idleMs) return piLabel('idle');
  return piLabel('active');
}

// `doneStateLabel(summary, id, heartbeat)`.
function piDoneState(entry, heartbeat) {
  var gates = entry && entry.gates && typeof entry.gates === 'object' ? entry.gates : {};
  var doneReported = !!(entry && (gates.done === true || entry.archive_ready === true));
  if (doneReported) {
    if (entry && entry.mergedVerified === true) return ah.cfg('devswarm_parent.done_merged');
    if (entry && entry.mergedVerified === false) return ah.cfg('devswarm_parent.done_not_merged');
    return ah.cfg('devswarm_parent.done_unverified');
  }
  var pct = heartbeat && piFinite(heartbeat.progress_pct) ? heartbeat.progress_pct : null;
  return pct !== null ? text.render(ah.cfg('devswarm_parent.working_pct'), { pct: pct }) : ah.cfg('devswarm_parent.working');
}

// `riskMarker(r)`.
function piRisk(r) {
  var parts = [];
  if (r.noUpstream) parts.push(ah.cfg('devswarm_parent.risk_local_only'));
  else if (piFinite(r.unpushed) && r.unpushed > 0) parts.push(text.render(ah.cfg('devswarm_parent.risk_unpushed'), { n: r.unpushed }));
  if (r.label === piLabel('archive_ready').label && r.mergedVerified !== true) parts.push(ah.cfg('devswarm_parent.risk_merged_unverified'));
  return parts.length ? ' ' + parts.join(', ') : '';
}

// `appRowMarkers(ws, brief, focused)`.
function piAppMarks(ws, brief, focused) {
  var parts = [];
  if (ws && ws.isPinned) parts.push(ah.cfg('devswarm_parent.mark_pinned'));
  if (focused) parts.push(ah.cfg('devswarm_parent.mark_on_screen'));
  if (brief === ah.cfg('devswarm_parent.brief_not_delivered')) parts.push(ah.cfg('devswarm_parent.mark_brief_not_delivered'));
  if (brief === ah.cfg('devswarm_parent.brief_withheld')) parts.push(ah.cfg('devswarm_parent.mark_brief_withheld'));
  return parts.length ? ' [' + parts.join(', ') + ']' : '';
}

// `buildWorkspaceTable(rows, now, capped, hidden, hiddenRows, archivedHidden)`.
function piTable(rows, now, capped, hidden, hiddenRows, archivedHidden, maxRows) {
  var lines = [ah.cfg('devswarm_parent.table_head')];
  rows.forEach(function (r) {
    var workspaceCol = piDisplayName(r.id, r.wsName).replace(/\|/g, '\\|') + piRisk(r) + (r.appMarks || '');
    lines.push(text.render(ah.cfg('devswarm_parent.table_row'), { workspace: workspaceCol, label: r.label, finish: r.finish, unread: r.unread, last: piRelative(r.lastActivityTs, now) }));
  });
  if (rows.some(function (r) { return r.label === piLabel('dormant').label; })) lines.push(ah.cfg('devswarm_parent.legend_dormant'));
  if (rows.some(function (r) { return r.label === piLabel('not_draining').label; })) lines.push(ah.cfg('devswarm_parent.legend_not_draining'));
  if (capped) {
    var overflow = text.render(ah.cfg('devswarm_parent.overflow'), { hidden: hidden, max: maxRows });
    if (Array.isArray(hiddenRows) && hiddenRows.length) {
      var named = ah.cfgNum('devswarm_parent.overflow_named_max');
      var ids = hiddenRows.slice(0, named).map(function (r) { return piShortId(r.id); });
      overflow += text.render(ah.cfg('devswarm_parent.overflow_ids'), { ids: ids.join(', ') + (hiddenRows.length > named ? ah.cfg('devswarm_parent.overflow_ids_more') : '') });
    }
    lines.push(overflow);
  }
  if (piFinite(archivedHidden) && archivedHidden > 0) lines.push(text.render(ah.cfg('devswarm_parent.archived_hidden'), { n: archivedHidden }));
  return lines.join('\n');
}

// `normalizeTableAges`: the volatile cells of a table row are dropped before the text is hashed.
function piNormTable(t) {
  return String(t).split('\n').map(function (l) {
    return /^\|.*\|\s*$/.test(l)
      ? l.replace(/\|[^|]*\|\s*$/, '| |').replace(/\|\s*\d+\s*\|\s*\|\s*$/, '| | |').replace(/ · \d+[mhd](?= · (?:progress|no progress))/g, '').replace(/ · progress \d+[mhd] ago/g, '')
        .replace(/ · [\d.]+[kM]? tok\b/g, '')
      : l;
  }).join('\n');
}

// `normalizeInboxVolatile`: the oldest age and the trend of the nudge are dropped before the text is hashed.
function piNormInbox(t) {
  return String(t).replace(/, oldest (?:—|\d+[smhd])/g, '').replace(/, (?:rising|flat|falling)\)/g, ')');
}

// ---- the acceptance log and the unread trend ----------------------------------------------------------------------------

// `lastInjectedUnread(home, id)`: the unread count of `id` in the newest 'inject' line of the log, or null.
function piLastInjected(home, id) {
  var p = piDevswarmDir(home) + '/' + ah.cfg('devswarm_parent.file_log');
  var size = ah.fs.size(p);
  if (size === null || size === 0 || size > ah.cfgNum('devswarm_parent.trend_log_max_bytes')) return null;
  var raw = ah.fs.readText(p, size);
  if (raw === null) throw meshDefer;
  var lines = raw.split('\n');
  for (var i = lines.length - 1; i >= 0; i--) {
    var line = lines[i];
    if (!line) continue;
    var rec;
    try { rec = JSON.parse(line); } catch (e) { continue; }
    if (!rec || rec.event !== 'inject' || !Array.isArray(rec.workspaces)) continue;
    var w = null;
    for (var j = 0; j < rec.workspaces.length; j++) if (rec.workspaces[j] && rec.workspaces[j].id === id) { w = rec.workspaces[j]; break; }
    if (w && piFinite(w.unread)) return w.unread;
  }
  return null;
}

function piTrend(home, id, curr) {
  var prev = piLastInjected(home, id);
  if (!piFinite(prev)) return null;
  if (curr > prev) return ah.cfg('devswarm_parent.rising');
  if (curr < prev) return ah.cfg('devswarm_parent.falling');
  return ah.cfg('devswarm_parent.flat');
}

// ---- the nudge segments -------------------------------------------------------------------------------------------------

function piStuck(status) { return !!status && ah.cfg('devswarm_parent.stuck_statuses').indexOf(status) >= 0; }
function piHigh(u) { return ah.cfg('devswarm_parent.high_urgencies').indexOf(u) >= 0; }

// `tierOf(w)`.
function piTier(w) {
  if (!(w.unread > 0)) return 'normal';
  if (piHigh(w.urgencyMax)) return 'urgent';
  if (piStuck(w.status)) return 'normal';
  if (w.urgencyMax === ah.cfg('devswarm_parent.low_urgency')) return 'low';
  return 'normal';
}

// `buildUnreadSegment(list, home)`.
function piUnreadSegment(list, home, cli, now) {
  var max = ah.cfgNum('devswarm_parent.max_listed');
  var shown = list.slice(0, max).map(function (w) {
    var title = piTruncate(piDisplayName(w.id, w.wsName));
    var parts = [];
    if (w.unread > 0) parts.push(text.render(ah.cfg('devswarm_parent.part_unread'), { n: w.unread }));
    if (piStuck(w.status)) parts.push(w.status);
    if (w.staleAntiHallMessage) parts.push(w.staleAntiHallMessage);
    else if (w.notDraining) parts.push(ah.cfg('devswarm_parent.part_not_draining'));
    if (piFinite(w.oldestUnreadTs)) parts.push(text.render(ah.cfg('devswarm_parent.part_oldest'), { age: piRelative(w.oldestUnreadTs, now) }));
    var trend = piTrend(home, w.id, w.unread);
    if (trend) parts.push(trend);
    return parts.length ? text.render(ah.cfg('devswarm_parent.unread_title_parts'), { title: title, parts: parts.join(ah.cfg('devswarm_parent.part_joiner')) }) : title;
  });
  var extra = list.length > max ? text.render(ah.cfg('devswarm_parent.unread_extra'), { n: list.length - max }) : '';
  var anyUnread = list.some(function (w) { return w.unread > 0; });
  var body = text.render(ah.cfg('devswarm_parent.unread_head'), { count: list.length, shown: shown.join(ah.cfg('devswarm_parent.shown_joiner')), extra: extra });
  body += anyUnread ? text.render(ah.cfg('devswarm_parent.unread_body_drain'), { cli: cli }) : ah.cfg('devswarm_parent.unread_body_read');
  body += ah.cfg('devswarm_parent.unread_tail');
  return body;
}

// `buildUrgentUnreadSegment(list, home)`.
function piUrgentSegment(list, home, cli) {
  var max = ah.cfgNum('devswarm_parent.max_listed');
  var shown = list.slice(0, max).map(function (w) {
    var parts = [text.render(ah.cfg('devswarm_parent.part_unread'), { n: w.unread })];
    if (piStuck(w.status)) parts.push(w.status);
    if (w.staleAntiHallMessage) parts.push(w.staleAntiHallMessage);
    else if (w.notDraining) parts.push(ah.cfg('devswarm_parent.part_not_draining'));
    var trend = piTrend(home, w.id, w.unread);
    if (trend) parts.push(trend);
    return text.render(ah.cfg('devswarm_parent.urgent_part'), { id: w.id, parts: parts.join(ah.cfg('devswarm_parent.part_joiner')) });
  });
  var extra = list.length > max ? text.render(ah.cfg('devswarm_parent.unread_extra'), { n: list.length - max }) : '';
  return text.render(ah.cfg('devswarm_parent.urgent_head'), { count: list.length, shown: shown.join(ah.cfg('devswarm_parent.shown_joiner')), extra: extra, cli: cli });
}

// `buildArchiveSegment(ids)`.
function piArchiveSegment(ids, cli) {
  var max = ah.cfgNum('devswarm_parent.max_listed');
  var extra = ids.length > max ? text.render(ah.cfg('devswarm_parent.archive_extra'), { n: ids.length - max }) : '';
  return text.render(ah.cfg('devswarm_parent.archive_head'), { shown: ids.slice(0, max).join(ah.cfg('devswarm_parent.archive_ids_joiner')), extra: extra, cli: cli });
}

// ---- the ingest-daemon banner -------------------------------------------------------------------------------------------

// `normalizeStaleBanner` / `normalizeMonitorFaultBanner`: the age is bucketed (or masked) before the text is hashed, so a banner that only grew older is a repeat.
function piNormStale(t) {
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
function piNormFault(t) {
  return String(t).replace(/(—|\d+[smhd]?) ago/g, 'N ago').replace(/\d+x in a row/g, 'Nx in a row');
}

// `formatRelative(ts, now)` of companion/lib/ingest-health.js.
function piIngestRelative(ts, now) {
  if (typeof ts !== 'number' || !isFinite(ts) || ts <= 0) return ah.cfg('devswarm_prompt.relative_none');
  var secs = Math.floor(Math.max(0, now - ts) / 1000);
  if (secs < 60) return secs + 's';
  var mins = Math.floor(secs / 60);
  if (mins < 60) return mins + 'm';
  var hours = Math.floor(mins / 60);
  return hours < 24 ? hours + 'h' : Math.floor(hours / 24) + 'd';
}

// `launchdDisabledLabel(repoKey)`: the label of this project's ingest job when launchd lists it disabled (macOS only; never under a test).
function piDisabledLabel(key) {
  if (ahHost.platform() !== ah.cfg('devswarm_prompt.launchd_platform') || !key) return null;
  var marks = ah.cfg('devswarm_prompt.test_markers');
  for (var i = 0; i < marks.length; i++) if (ah.env.get(marks[i])) return null;
  var r = ah.exec('launchctl', ah.cfg('devswarm_prompt.launchctl_args').map(function (a) { return a.replace('{uid}', String(ah.uid())); }), { timeoutMs: ah.cfgNum('devswarm_prompt.launchctl_timeout_ms') });
  if (r === null || r.status !== 0 || r.truncated) return null;
  var label = ah.cfg('devswarm_prompt.launchd_label_prefix') + key;
  var esc = label.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return new RegExp('"' + esc + '"\\s*=>\\s*(?:disabled|true)\\b').test(r.stdout) ? label : null;
}

// The ingest-daemon banner state of this turn, as the hook's two `*BannerOnce` calls leave it: the dedupe records to consult (`fault`, `stale`)
// and the keys to forget. A hand-over in progress (a fresh heartbeat of a live lock) says nothing.
function piIngest(h, key, now, sid, tp) {
  var res = { fault: null, stale: null, forgetFault: false, forgetStale: false };
  var base = { sessionId: sid, transcriptPath: tp, keepaliveTurns: ah.cfgNum('devswarm_prompt.banner_keepalive') };
  if (h.status === 'failed') {
    var f = h.fault, what;
    var rel = function (t) { return piIngestRelative(t, now); };
    if (f.noOkSinceStart) what = text.render(ah.cfg('devswarm_prompt.fault_no_ok'), { rel: rel(f.startedAtMs) });
    else if (f.okStale) what = text.render(ah.cfg('devswarm_prompt.fault_ok_stale'), { rel: rel(f.lastOkMs) });
    else what = text.render(ah.cfg('devswarm_prompt.fault_failing'), { n: typeof f.consecutive === 'number' && isFinite(f.consecutive) ? f.consecutive : '?' });
    var err = f.error ? text.render(ah.cfg('devswarm_prompt.fault_error'), { error: String(f.error).slice(0, ah.cfgNum('devswarm_prompt.fault_error_max')) }) : '';
    var o = Object.assign({}, base, { key: ah.cfg('devswarm_prompt.fault_key'), normalize: piNormFault });
    o.content = text.render(ah.cfg(f.code ? 'devswarm_prompt.fault_config' : 'devswarm_prompt.fault_banner'), { beat: rel(f.heartbeatTs), what: what, err: err, code: f.code || '' });
    res.fault = o;
    return res;
  }
  res.forgetFault = true;
  if (h.status !== 'stale') { res.forgetStale = true; return res; }
  var beatTs = typeof h.beatTs === 'number' && isFinite(h.beatTs) ? h.beatTs : null;
  if (beatTs !== null && now - beatTs < ah.cfgNum('devswarm_prompt.banner_grace_ms') && h.liveLock) return res;
  var label = piDisabledLabel(key);
  var tail = label ? text.render(ah.cfg('devswarm_prompt.stale_disabled'), { label: label, uid: String(ah.uid()) }) : ah.cfg('devswarm_prompt.stale_doctor');
  var s = Object.assign({}, base, { key: ah.cfg('devswarm_prompt.stale_key'), normalize: piNormStale });
  s.content = text.render(ah.cfg('devswarm_prompt.stale_banner'), { age: piIngestRelative(beatTs, now), tail: tail });
  res.stale = s;
  return res;
}

// `emit-dedupe.forget`: drop the record of a block that is no longer announced, so its next occurrence is a new episode.
function piForget(sid, key) {
  if (sid === null || dedupe.disabled()) return;
  var rel = dedupe.stateRel(sid), state = dedupe.readState(rel);
  if (!Object.prototype.hasOwnProperty.call(state, key)) return;
  delete state[key];
  ah.state.writeAtomic(rel, JSON.stringify(state));
}

// ---- the roster of the project's summary ----------------------------------------------------------------------------------

// A summary's text as the hook reads it: {absent: true}, {summary} or the cause of a deferral (anything the hook would log, or a shape it
// would read otherwise).
function piParseSummary(raw) {
  var t = raw.trim();
  if (t === '') return { cause: 'summary-empty' };
  var j = jx.parse(t);
  if (j.unsure) throw meshDefer;
  if (j.invalid) return { cause: 'summary-invalid' };
  if (j.v === null || typeof j.v !== 'object') return { absent: true }; // a scalar reads as no summary
  if (Array.isArray(j.v)) return { cause: 'summary-shape' }; // a list is an object of its own: Node's
  return { summary: j.v };
}

// The project's summary file.
function piReadSummary(home, key) {
  var p = piDevswarmDir(home) + '/' + ah.cfg('devswarm_role.pg_dir_summaries') + '/' + key + '.json';
  if (!piPresent(p)) return { absent: true };
  var r = jx.read(p);
  if (r.big) throw meshDefer;
  if (r.text === undefined) return { cause: 'summary-unreadable' };
  return piParseSummary(r.text);
}

// What the Node hook would do with the summary's rows, decided without a write: the rows, the nudges and the archive recommendations,
// or the cause of a deferral. `ctx`: {home, key, top, primaryId, now, sid}.
function piRoster(ctx, summary) {
  var home = ctx.home, key = ctx.key, now = ctx.now;
  var ws = summary.workspaces;
  if (Array.isArray(ws)) throw meshDefer;
  var summaryWorkspaces = ws !== null && typeof ws === 'object' ? ws : {};
  var ids = Object.keys(summaryWorkspaces);
  var out = { rows: [], attention: [], archiveList: [], ownUnread: false, deferral: null, snapUnreadable: false };
  // the DevSwarm app's database, read once
  var focusEnv = ah.env.get(ah.cfg('devswarm_parent.env_focus_ms'));
  var focusRaw = Number(focusEnv === null ? undefined : focusEnv); // Number(undefined) is NaN, Number(null) would be 0
  var windowMs = isFinite(focusRaw) && focusRaw >= 0 ? focusRaw : ah.cfgNum('devswarm_parent.focus_default_ms');
  var snapInfo = JSON.parse(ahHost.piSnap(windowMs, now));
  if (snapInfo.unsure) throw meshDefer;
  var focusedId = windowMs > 0 && snapInfo.focused ? snapInfo.focused : null;
  out.snapUnreadable = snapInfo.file && !snapInfo.readable;
  function appWs(id, wt) {
    if (!snapInfo.readable) return null;
    var r = JSON.parse(ahHost.piWs(id, wt, now));
    if (r !== null && r.unsure) throw meshDefer;
    return r;
  }
  var graceMs = piSetDefault('devswarm_parent.set_grace') * 1000;
  var renagMs = piSetDefault('devswarm_parent.set_renag') * ah.cfgNum('devswarm_parent.ms_per_hour');
  var ignoreV = piJson(piDevswarmDir(home) + '/' + ah.cfg('devswarm_role.pg_ignore_file'));
  var ignoreIds = {};
  if (jx.isObj(ignoreV) && Array.isArray(ignoreV.ids)) ignoreV.ids.forEach(function (x) { if (typeof x === 'string' && x !== '') ignoreIds[x] = true; });
  var present = ids.filter(function (i) { return true; });
  // the ghost rows: a `primary-<hash>` label whose canonical row is also in the summary is that workspace, shown once under it
  var ghostFoldTo = {}, foldedUnread = {};
  ids.forEach(function (id) {
    if (!piSafeId(id) || id === ctx.primaryId || !/^primary-[0-9a-f]{8}$/.test(id)) return;
    var entry = summaryWorkspaces[id];
    if (!entry || typeof entry !== 'object') return;
    var wt = entry.worktreePath ? piStrField(entry.worktreePath) : null;
    var w = wt ? appWs(null, wt) : null;
    var appBuilderId = w && w.builderType !== 'primary' ? w.id : null;
    var f = JSON.parse(ahHost.piFold(id, JSON.stringify(present), appBuilderId, key, now));
    if (f.unsure) throw meshDefer;
    if (!f.to) return;
    ghostFoldTo[id] = f.to;
    var gu = piFinite(entry.directUnread) ? entry.directUnread : (piFinite(entry.unread) ? entry.unread : 0);
    if (gu > 0) foldedUnread[f.to] = (foldedUnread[f.to] || 0) + gu;
  });
  for (var n = 0; n < ids.length; n++) {
    var id = ids[n];
    if (!piSafeId(id)) continue;
    if (id === ctx.primaryId) continue; // the Primary's own entry is surfaced through the own-unread path
    var entry = summaryWorkspaces[id];
    if (!entry || typeof entry !== 'object') continue;
    if (Object.prototype.hasOwnProperty.call(ghostFoldTo, id)) continue;
    var wtPath = piStrField(entry.worktreePath);
    var sessionId = piStrField(entry.sessionId);
    // a row on this very checkout that the app calls its primary builder is the Primary's own inbox, not a child
    if (wtPath) {
      var bw = appWs(id, null);
      if (bw && bw.builderType === 'primary') {
        var wc = ah.repo.context(wtPath);
        if (wc.unsure) throw meshDefer;
        if (wc.root === ctx.top) { out.deferral = 'own-checkout-row'; return out; }
      }
    }
    var dKey = wtPath ? ah.mesh.repoKey(wtPath) : null;
    if (key && dKey && dKey !== key) continue; // another project's workspace
    var unread = (piFinite(entry.directUnread) ? entry.directUnread : (piFinite(entry.unread) ? entry.unread : 0)) + (foldedUnread[id] || 0);
    var total = piFinite(entry.total) ? entry.total : 0;
    var cursor = piFinite(entry.cursor) ? entry.cursor : 0;
    var urgencyMax = entry.urgencyMax || null;
    var vv = piJson(piDevswarmDir(home) + '/' + ah.cfg('mesh_write.dir_liveness') + '/' + id + '.json');
    var verdict = vv && typeof vv === 'object' ? vv : null;
    var status = entry && typeof entry.status === 'string' ? entry.status : (verdict && typeof verdict.status === 'string' ? verdict.status : null);
    var stuck = status !== null && piStuck(status);
    var notDraining = !!(verdict && verdict.notDraining);
    if (notDraining) { out.deferral = 'not-draining'; return out; } // the stale-build check of a backlog that does not drain is Node's
    var archiveReady = entry.archive_ready === true;
    var archiveReadyQuiet = false;
    if (archiveReady && unread > 0 && entry.archive_request_only_unread === true) {
      var liveSession = false;
      if (entry.sessionId) liveSession = ah.mesh.sessionAlive(piStrField(entry.sessionId));
      archiveReadyQuiet = !liveSession;
    }
    var archiveRequestPending = entry.archive_request_only_unread === true;
    var archiveRequestAgeMs = archiveRequestPending && piFinite(entry.oldestDirectUnreadTs) ? now - entry.oldestDirectUnreadTs : null;
    var archiveRequestStale = archiveRequestAgeMs !== null && archiveRequestAgeMs >= renagMs;
    var archiveRequestQuiet = archiveRequestPending && !archiveRequestStale;
    var nagIgnored = ignoreIds[id] === true;
    var oldestForGate = piFinite(entry.oldestDirectUnreadTs) ? entry.oldestDirectUnreadTs : null;
    var senderRaw = entry.oldestDirectUnreadSender;
    var graced = false;
    if (unread > 0 && oldestForGate !== null) {
      var ageMs = now - oldestForGate;
      if (ageMs >= graceMs) graced = false;
      else if (senderRaw != null && ctx.primaryId != null && String(senderRaw) !== String(ctx.primaryId)) graced = false;
      else graced = true;
    }
    var unreadForNag = unread > 0 && !graced;
    var st = JSON.parse(ahHost.piRow(id, wtPath, sessionId, key, now));
    if (st.unsure) throw meshDefer;
    var archivedRow = st.marker;
    var appArchivedRow = st.app;
    var rowWs = appWs(id, wtPath);
    var ownerFocused = !!(focusedId && rowWs && rowWs.id === focusedId);
    if (((unreadForNag && !archiveRequestQuiet) || stuck || (notDraining && !archiveRequestQuiet)) && !archiveReadyQuiet && !nagIgnored && !appArchivedRow && !ownerFocused) {
      var wsName = (rowWs && rowWs.label) || piReadName(home, id);
      var reportUnread = graced ? 0 : unread;
      out.attention.push({
        id: id, unread: reportUnread, cursor: cursor, total: total, status: status, urgencyMax: graced ? null : urgencyMax, wsName: wsName,
        oldestUnreadTs: graced ? null : oldestForGate, notDraining: false, staleAntiHallMessage: null,
      });
    }
    if (archiveReady && !appArchivedRow && !ownerFocused && !st.ignored && piArchiveCooldown(home, id, now) && !piAutoArchiveOwns(home, id, now)
        && !(archiveRequestQuiet && !archiveReadyQuiet)) out.archiveList.push(id);
    // the live-table row
    var archivedSuperseded = !archivedRow && st.superseded;
    if (!archivedRow && appArchivedRow) archivedRow = true;
    var hb = piJson(piDevswarmDir(home) + '/' + ah.cfg('mesh_write.dir_heartbeats') + '/' + id + '.json');
    var heartbeat = hb && typeof hb === 'object' ? hb : null;
    var lastOut = verdict && piFinite(verdict.lastOutboundTs) ? verdict.lastOutboundTs : null;
    var a = (lastOut !== null ? lastOut : 0), b = heartbeat && piFinite(heartbeat.ts) ? heartbeat.ts : 0, best = Math.max(a, b);
    var activityTs = best > 0 ? best : null;
    var dormant = false, idleAlive = false, idleMs = 0;
    if (!archivedRow) {
      var act = JSON.parse(ahHost.piActivity(id, wtPath, sessionId, lastOut, key, now));
      if (act.unsure) throw meshDefer;
      idleMs = act.idleMs;
      if (act.ts !== null && (!piFinite(activityTs) || act.ts > activityTs)) activityTs = act.ts;
      dormant = act.dormant;
      idleAlive = act.idleAlive;
    }
    var ds = archivedRow
      ? (appArchivedRow ? piLabel('archived') : piLabel('archived'))
      : archivedSuperseded ? piLabel('superseded') : piDisplayStatus(archiveReady, status, activityTs, now, dormant, false, idleAlive, idleMs);
    var finishCell = piDoneState(entry, heartbeat);
    if (/^working\b/.test(finishCell) && gates.setting('devswarm_prompt.plan_tracking_setting', true, ctx.pluginRoot) !== false) {
      var fp = JSON.parse(ahHost.ctFindPlan(id, wtPath || ''));
      if (fp.unsure) throw meshDefer;
      if (fp.plan !== null) { out.deferral = 'plan-row'; return out; }
    }
    var appMarks = '';
    if (rowWs) {
      if (rowWs.finish) finishCell = finishCell === ah.cfg('devswarm_parent.relative_none') ? rowWs.finish : finishCell + ' · ' + rowWs.finish;
      appMarks = piAppMarks(rowWs, rowWs.brief, !!(focusedId && rowWs.id === focusedId));
    }
    out.rows.push({
      id: id, label: ds.label, rank: ds.rank, finish: finishCell, appRank: rowWs && piFinite(rowWs.rank) ? rowWs.rank : null, appMarks: appMarks,
      unread: unread, lastActivityTs: activityTs, wsName: (rowWs && rowWs.label) || piReadName(home, id),
      unpushed: piFinite(entry.unpushed) ? entry.unpushed : null, noUpstream: entry.noUpstream === true, mergedVerified: entry.mergedVerified,
    });
  }
  return out;
}

// `archiveCooldownElapsed(home, id, now)`: no earlier recommendation, or its cooldown is over.
function piArchiveCooldown(home, id, now) {
  var st = piJson(piDevswarmDir(home) + '/' + ah.cfg('devswarm_parent.dir_archive_nudges') + '/' + id + '.json');
  var last = st && piFinite(st.lastNudgedAt) ? st.lastNudgedAt : null;
  if (last === null) return true;
  return (now - last) >= ah.cfgNum('devswarm_parent.archive_cooldown_ms');
}

// `autoArchiveOwnsRow(home, id, now)`: the supervisor's last sweep owns the row. A record that could say yes needs the capability check: Node's.
function piAutoArchiveOwns(home, id, now) {
  if (ah.settings.touched('devswarm_parent.set_auto_archive')) throw meshDefer;
  var st = piJson(piDevswarmDir(home) + '/' + ah.cfg('devswarm_parent.file_auto_archive'));
  if (!st || !Array.isArray(st.owned) || !(now - Number(st.at) < 3600000)) return false;
  if (st.owned.indexOf(String(id)) < 0) return false;
  throw meshDefer;
}

// ---- wake coverage --------------------------------------------------------------------------------------------------------

// `readDescriptors(home)` of the supervisor: the workspace descriptors that name a worktree and a session and a safe id.
function piDescriptors(home) {
  var dir = piDevswarmDir(home) + '/' + ah.cfg('devswarm_role.pg_dir_workspaces'), out = [];
  (piNames(dir) || []).forEach(function (n) {
    if (!/\.json$/.test(n)) return;
    var d = piJson(dir + '/' + n);
    if (!d || !d.worktreePath || !d.sessionId || !piSafeId(d.id)) return;
    if (typeof d.worktreePath !== 'string' || typeof d.sessionId !== 'string') throw meshDefer;
    out.push(d);
  });
  return out;
}

// The part of `rowEligibility(row, { liveness: true })` the live-child test reads: is the row put away, held, ignored or waiting on a human?
// `marker` and `app` are the archive verdicts the caller asks for; the liveness is read only when nothing else already skips the row.
function piSkipped(ctx, id, wt, sid, marker, appKey) {
  if (marker && JSON.parse(ahHost.piMarker(id, wt, sid)).archived === true) return true;
  if (appKey !== undefined) {
    var a = JSON.parse(ahHost.piApp(id, wt, appKey, ctx.now));
    if (a.unsure) throw meshDefer;
    if (a.archived) return true;
  }
  if (ah.settings.touched('devswarm_parent.set_held')) throw meshDefer; // the owner's held partitions
  if (ahHost.piIgnored(id)) return true;
  var live = sid ? ah.mesh.sessionAlive(sid) : false;
  if (id !== '' && sid && wt) {
    var b = ah.mesh.childBusy(id, wt, sid, ctx.now, ah.cfg('devswarm_role.pg_set_busy_fresh').default * ah.cfgNum('devswarm_role.pg_ms_per_min'));
    if (b.none !== true && b.waiting && live) return true;
  }
  return false;
}

// `liveChildState(home, cwd, { excludeHeldIgnored: true, excludeWaitingOnUser: true })`: {known, live}.
function piLiveChildren(ctx, cwd) {
  var d = { live: false, known: true };
  var descriptors = piDescriptors(ctx.home);
  if (descriptors.length) {
    var selfKey = ah.mesh.repoKey(cwd);
    if (selfKey === null) d = { live: false, known: false };
    else {
      var selfReal = ah.fs.realpath(cwd);
      if (selfReal === null) selfReal = cwd;
      for (var i = 0; i < descriptors.length; i++) {
        var x = descriptors[i];
        if (!x.worktreePath || !x.id) continue;
        var real = ah.fs.realpath(x.worktreePath);
        if ((real === null ? x.worktreePath : real) === selfReal) continue; // this Primary's own row
        var dKey = ah.mesh.repoKey(x.worktreePath);
        if (dKey !== selfKey) continue;
        if (!piSkipped(ctx, x.id, x.worktreePath, x.sessionId, true, dKey)) { d = { live: true, known: true }; break; }
      }
    }
  }
  if (d.known && d.live) return d;
  // a live child no descriptor names, found in the app database
  var ac = JSON.parse(ahHost.piAppChildren(cwd, ctx.now));
  if (ac.unsure) throw meshDefer;
  for (var j = 0; j < ac.children.length; j++) {
    var w = ac.children[j];
    if (!piSkipped(ctx, w.id, w.worktreePath, w.sessionId, true, undefined)) return { live: true, known: true };
  }
  return d;
}

// `watcherLockLive(home, id, now)`: a live Monitor wake-watch holds the lock of `id`.
function piWatcherLock(home, id, now) {
  var lock = piJson(piDevswarmDir(home) + '/' + ah.cfg('devswarm_parent.dir_locks') + '/' + ah.cfg('devswarm_parent.watch_lock_prefix') + id + ah.cfg('devswarm_parent.watch_lock_suffix'));
  if (lock === null) return false;
  var ts = lock && Number(lock.ts);
  var fresh = piFinite(ts) && (now - ts) <= ah.cfgNum('wake_watch.lock_stale_ms');
  var pid = lock && typeof lock.pid === 'number' && isFinite(lock.pid) && Math.floor(lock.pid) === lock.pid ? lock.pid : null;
  var alive = pid !== null ? ahHost.piPid(pid) : null;
  return !!(fresh && alive !== false);
}

// `watcherLiveForWorkspace(home, id, now)`: the id's own lock, or the lock of another row on the same checkout.
function piWatcherLive(home, id, now) {
  if (piWatcherLock(home, id, now)) return true;
  var descriptors = piDescriptors(home), own = null;
  descriptors.forEach(function (d) { if (own === null && String(d.id) === String(id)) own = d; });
  if (!own || !own.worktreePath) return false;
  var forms = function (p) {
    var f = {}, resolved = ah.path.resolveAbs(p);
    f[resolved] = true;
    var r = ah.fs.realpath(resolved);
    if (r !== null) f[r] = true;
    return Object.keys(f);
  };
  var ownForms = forms(own.worktreePath);
  for (var i = 0; i < descriptors.length; i++) {
    var d = descriptors[i];
    if (!d.id || !d.worktreePath || String(d.id) === String(id)) continue;
    if (forms(d.worktreePath).some(function (f) { return ownForms.indexOf(f) >= 0; }) && piWatcherLock(home, d.id, now)) return true;
  }
  return false;
}

// The mailbox-wake line of this turn (`noWakePathLine(wakeCoverage(...))`), or null: only a Claude Primary with a live child and no recent tick is told.
function piWakeLine(ctx, cwd, cli, opts) {
  var raw = ah.env.get(ah.cfg('devswarm_role.agent_env'));
  var agent = raw === null ? '' : raw.trim().toLowerCase();
  if (agent !== ah.cfg('devswarm_role.claude_agent')) return null;
  var lc = piLiveChildren(ctx, cwd);
  if (!lc.known || !lc.live) return null;
  var tick = piJson(piDevswarmDir(ctx.home) + '/' + ah.cfg('devswarm_parent.dir_wake_tick') + '/' + ctx.primaryId + '.json');
  var ageMin = tick && piFinite(tick.ts) ? Math.max(0, Math.floor((ctx.now - tick.ts) / ah.cfgNum('devswarm_prompt.ms_per_minute'))) : null;
  var warn = piSetDefault('devswarm_parent.set_cron_warn');
  if (!(ageMin === null || ageMin > warn)) return null;
  var wanted = ah.settings.bool('wake_watch.set_enabled') !== false;
  var noWatcher = wanted && !piWatcherLive(ctx.home, ctx.primaryId, ctx.now);
  var cron = text.render(ah.cfg('devswarm_parent.wake_cron_part'), { cron: dwWakeCron(), cli: cli, id: ctx.primaryId });
  if (noWatcher) {
    var watcher = piWatcherPath(opts, ctx.home);
    if (watcher === null) throw meshDefer;
    return text.render(ah.cfg('devswarm_parent.wake_both'), { mon: text.render(ah.cfg('devswarm_parent.wake_monitor'), { watcher: watcher }), cron: cron });
  }
  return text.render(ah.cfg('devswarm_parent.wake_tick'), { cron: cron });
}

// The mailbox watcher path the line names (`preferStableLauncher('wakeWatch', RAW_WATCHER)`).
function piWatcherPath(opts, home) {
  var root = jx.isObj(opts) && typeof opts.plugin_root === 'string' ? opts.plugin_root : ah.env.get(ah.cfg('env.plugin_root'));
  if (root === null || root === undefined || root === '') return null;
  var nr = dwNodeRoot(root);
  if (nr === null) return null;
  var l = ah.cfg('devswarm_role.launcher_watcher');
  if (ah.settings.bool('devswarm_role.sw_stable_launcher')) {
    var stable = posix.normalize(home + '/' + ah.cfg('devswarm_role.bin_dir') + '/' + l.name);
    if (ah.fs.isFile(stable)) return stable;
  }
  return posix.normalize(nr + '/' + l.target);
}

// ---- the maintainer notices and the session drift -----------------------------------------------------------------------

// `buildMaintainerNoticeSegment(home, repoKey, now)` for a feed with nothing unseen: no segment and nothing written. A notice this project has
// not seen is shown once and marked seen (a write and a metrics line): Node's.
function piNotices(home, key, now) {
  var file = piDevswarmDir(home) + '/' + ah.cfg('devswarm_parent.notices_file');
  if (!piPresent(file)) return;
  if (ah.settings.touched('devswarm_parent.set_notice_show')) throw meshDefer;
  var size = ah.fs.size(file);
  if (size !== null && size > ah.cfgNum('script.read_max_bytes')) throw meshDefer;
  var raw = size === null ? null : ah.fs.readText(file);
  var rows = [];
  if (raw !== null) raw.split('\n').forEach(function (line) {
    var t = line.trim();
    if (!t) return;
    var row;
    try { row = JSON.parse(t); } catch (e) { return; }
    if (row && typeof row === 'object' && row.id) rows.push(row);
  });
  var live = rows.filter(function (r) { return !(piFinite(r.expiresAt) && r.expiresAt <= now); });
  live.sort(function (a, b) { return (Number(a.ts) || 0) - (Number(b.ts) || 0); });
  var shown = live.slice(Math.max(0, live.length - ah.cfgNum('devswarm_parent.notice_max_shown')));
  if (!shown.length) return;
  var cursor = null;
  if (/^[A-Za-z0-9._-]+$/.test(key)) {
    var cv = piJson(piDevswarmDir(home) + '/' + ah.cfg('devswarm_parent.dir_notice_cursor') + '/' + key + '.json');
    cursor = cv && typeof cv.lastSeenId === 'string' ? cv.lastSeenId : null;
  }
  var unseen = shown;
  if (cursor) {
    var idx = -1;
    for (var i = 0; i < shown.length; i++) if (shown[i].id === cursor) { idx = i; break; }
    unseen = idx === -1 ? shown : shown.slice(idx + 1);
  }
  if (unseen.length) throw meshDefer;
}

// The session-drift notice (`driftNotice(anchorSessionDrift(...))`), or null: a newer session of this worktree is active than the Primary's anchor.
function piDriftNotice(home, top, primaryId, sid, tp) {
  var desc = piJson(piDevswarmDir(home) + '/' + ah.cfg('devswarm_role.pg_dir_workspaces') + '/' + primaryId + '.json');
  var anchorRaw = desc && desc.sessionId;
  var anchor = anchorRaw != null ? String(anchorRaw) : '';
  var d = JSON.parse(ahHost.piDrift(top, anchor, sid != null ? String(sid) : '', tp));
  if (d !== null && d.unsure) throw meshDefer;
  if (d === null) return null;
  if (sid && String(sid) === d.newest) return null;
  return text.render(ah.cfg('devswarm_parent.drift_notice'), { newest: d.newest, anchor: d.anchor });
}

// ---- the project of a Primary in a checkout ---------------------------------------------------------------------------------

// The cause of a deferral for everything the hook would show or do that the engine does not reproduce yet, or null. Everything it reads is
// read before the first write of the turn.
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
  var now = ah.clock.now();
  var every = ah.settings.num('verify_first.num_repeat_every');
  var repeat = isFinite(every) && every > 0 ? every : 0;
  var parked = null, roster = null, key = null, health = null, banner = null, wakeLine = null, driftNotice = null, derived = null;
  if (top !== null) {
    var cli = piCli(opts, home);
    if (cli === null) return piDefer('cli');
    key = ah.mesh.repoKey(top);
    if (key === null) return piDefer('repo-key');
    var dev = piDevswarmDir(home);
    // the ingest daemon: when it is not healthy the hook refreshes the summary from the store (a write), which the engine leaves to Node
    health = JSON.parse(ahHost.ctIngest(key));
    if (health.unsure) return piDefer('ingest-health');
    // when it is not healthy the hook refreshes the summary from the store first; the refresh is written once the turn is settled
    if (health.status !== 'healthy') {
      var dr = JSON.parse(ahHost.piDerive(key, now));
      if (dr.unsure) return piDefer('summary-refresh');
      derived = dr.text;
    }
    var sm = derived !== null ? piParseSummary(derived) : piReadSummary(home, key);
    if (sm.cause) return piDefer(sm.cause);
    var summary = sm.summary || null;
    if (summary !== null) {
      if (Array.isArray(summary)) return piDefer('summary-shape');
      var ctx = { home: home, key: key, top: top, primaryId: primaryId, now: now, sid: sid, pluginRoot: gates.pluginRoot(opts) };
      roster = piRoster(ctx, summary);
      if (roster.deferral !== null) return piDefer(roster.deferral);
      // the Primary's own inbound mail, the questions it was asked, and what the summary lists beside the rows
      var own = summary.workspaces && summary.workspaces[primaryId] ? summary.workspaces[primaryId] : summary[primaryId];
      if (own && typeof own === 'object') {
        if (piFinite(own.unread) && own.unread > 0) return piDefer('own-unread');
        if (Array.isArray(own.pendingQuestions) && own.pendingQuestions.length) return piDefer('own-questions');
      }
      if (Array.isArray(summary.orphans) && summary.orphans.length) return piDefer('orphans');
      if (Array.isArray(summary.staleRegistryPartitions) && summary.staleRegistryPartitions.length) return piDefer('stale-registry');
      if (Array.isArray(summary.recent) && summary.recent.length) return piDefer('broadcast');
      if (roster.rows.length) {
        if (roster.snapUnreadable) return piDefer('app-db-unreadable');
        if (piPresent(dev + '/app-state.json')) return piDefer('app-state');
      }
    }
    piNotices(home, key, now);
    if (roster !== null && roster.rows.length) banner = piIngest(health, key, now, sid, tp);
    wakeLine = piWakeLine({ home: home, primaryId: primaryId, now: now }, cwd, cli, opts);
    driftNotice = piDriftNotice(home, top, primaryId, sid, tp);
    parked = piParked(home, primaryId, cli);
  }
  // The dedupe records are consulted in the order the hook does (the ingest banners come before the override reminder), and the answer
  // is assembled in the order of its segments (`rank`).
  var text0 = ah.cfg('devswarm_prompt.override_reassert');
  var overrideOpts = { sessionId: sid, key: ah.cfg('devswarm_prompt.override_key'), content: text0, transcriptPath: tp, keepaliveTurns: repeat };
  var pending = []; // {kind, rank, o?, text}
  if (banner !== null) {
    if (banner.fault) {
      pending.push({ kind: 'dedupe', rank: 10, o: banner.fault, text: banner.fault.content, onSuppress: function () { piForget(sid, ah.cfg('devswarm_prompt.stale_key')); } });
    } else {
      pending.push({ kind: 'forget', key: ah.cfg('devswarm_prompt.fault_key') });
      if (banner.stale) pending.push({ kind: 'dedupe', rank: 10, o: banner.stale, text: banner.stale.content });
      else pending.push({ kind: 'forget', key: ah.cfg('devswarm_prompt.stale_key') });
    }
  }
  pending.push({ kind: 'dedupe', rank: 0, o: overrideOpts, text: text0 });
  if (wakeLine !== null) pending.push({ kind: 'dedupe', rank: 20, o: { sessionId: sid, key: ah.cfg('devswarm_parent.key_wake'), content: wakeLine, transcriptPath: tp, keepaliveTurns: repeat }, text: wakeLine });
  if (driftNotice !== null) pending.push({ kind: 'dedupe', rank: 50, o: { sessionId: sid, key: ah.cfg('devswarm_parent.key_drift'), content: driftNotice, transcriptPath: tp, keepaliveTurns: ah.cfgNum('devswarm_parent.drift_keepalive') }, text: driftNotice });
  var tableText = null, attention = [], archiveList = [], rows = [];
  if (roster !== null) {
    rows = roster.rows;
    attention = roster.attention;
    archiveList = roster.archiveList;
  }
  var maxRows = ah.cfgNum('devswarm_parent.max_table_rows');
  var tableWrites = [];
  if (rows.length) {
    var appRankOf = function (r) { return piFinite(r.appRank) ? r.appRank : Infinity; };
    rows.sort(function (a, b) {
      return (a.rank - b.rank) || (b.unread - a.unread) || ((appRankOf(a) - appRankOf(b)) || 0) || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
    });
    var archivedHidden = 0;
    var hide = String(ah.env.get(ah.cfg('devswarm_parent.env_roster_hide')) || '') !== ah.cfg('devswarm_parent.roster_hide_off');
    var archivedWord = piLabel('archived').label;
    var activeRows = hide ? rows.filter(function (r) { if (r.label === archivedWord) { archivedHidden += 1; return false; } return true; }) : rows;
    var capEnv = ah.env.get(ah.cfg('devswarm_parent.env_roster_max'));
    var capRaw = Number(capEnv === null ? undefined : capEnv);
    var cap = isFinite(capRaw) && capRaw > 0 ? Math.floor(capRaw) : maxRows;
    var capped = activeRows.length > cap;
    var shownRows = capped ? activeRows.slice(0, cap) : activeRows;
    var evicted = capped ? activeRows.slice(cap) : [];
    if (capped) tableWrites.push({ event: 'table-cap', total: activeRows.length, shown: shownRows.length, hidden: activeRows.length - shownRows.length });
    if (shownRows.length || archivedHidden) {
      tableText = piTable(shownRows, now, capped, activeRows.length - shownRows.length, evicted, archivedHidden, maxRows); // the text names the default cap, as Node's does
      pending.push({ kind: 'dedupe', rank: 60, o: { sessionId: sid, key: 'parent-inbox-table', content: tableText, transcriptPath: tp, keepaliveTurns: repeat, normalize: piNormTable }, text: tableText, after: ah.cfg('devswarm_parent.title_instruction') });
    }
  }
  var urgentList = attention.filter(function (w) { return piTier(w) === 'urgent'; });
  var normalList = attention.filter(function (w) { return piTier(w) === 'normal'; });
  if (parked !== null) pending.push({ kind: 'plain', rank: 120, text: parked });
  var cliPath = top !== null ? piCli(opts, home) : null;
  if (urgentList.length) {
    var us = piUrgentSegment(urgentList, home, cliPath);
    pending.push({ kind: 'dedupe', rank: 130, o: { sessionId: sid, key: 'parent-inbox-urgent', content: us, transcriptPath: tp, keepaliveTurns: 10 }, text: us });
  }
  if (normalList.length) {
    var ns = piUnreadSegment(normalList, home, cliPath, now);
    pending.push({ kind: 'dedupe', rank: 140, o: { sessionId: sid, key: 'parent-inbox-nudge', content: ns, transcriptPath: tp, normalize: piNormInbox, keepaliveTurns: 2 }, text: ns, suppressedLog: normalList.some(function (w) { return w.notDraining; }) });
  }
  if (archiveList.length) pending.push({ kind: 'plain', rank: 150, text: piArchiveSegment(archiveList, cliPath) });
  // every dedupe record must be decidable before the first write
  var dedupeOn = sid !== null && !dedupe.disabled();
  for (var i = 0; i < pending.length; i++) {
    if (pending[i].kind === 'dedupe' && dedupeOn && !dedupe.canDecide(pending[i].o)) return piDefer('dedupe-undecidable');
  }
  ah.commit();
  if (derived !== null) ah.state.writeAtomic(piRel(ah.cfg('devswarm_role.pg_dir_summaries') + '/' + key + '.json'), derived);
  var emitted = [];
  pending.forEach(function (s) {
    if (s.kind === 'forget') { piForget(sid, s.key); return; }
    if (s.kind === 'plain') { emitted.push({ rank: s.rank, text: s.text }); return; }
    var emit = !dedupeOn || dedupe.shouldEmit(s.o);
    if (emit) { emitted.push({ rank: s.rank, text: s.text }); if (s.after) emitted.push({ rank: s.rank, text: s.after }); } else if (s.onSuppress) s.onSuppress();
  });
  var segments = emitted.map(function (e, i) { return { r: e.rank, i: i, t: e.text }; }).sort(function (a, b) { return (a.r - b.r) || (a.i - b.i); }).map(function (e) { return e.t; });
  // the acceptance telemetry and the archive-recommendation records
  tableWrites.forEach(function (w) { ah.state.appendFile(piRel(ah.cfg('devswarm_parent.file_log')), JSON.stringify({ ts: Math.floor(ah.clock.now()), event: w.event, total: w.total, shown: w.shown, hidden: w.hidden }) + '\n'); });
  var totalUnread = attention.reduce(function (s, w) { return s + w.unread; }, 0);
  if (attention.length && totalUnread > 0) {
    var inj = attention.filter(function (w) { return w.unread > 0; }).map(function (w) { return { id: w.id, unread: w.unread, cursor: w.cursor, total: w.total, status: w.status || null }; });
    ah.state.appendFile(piRel(ah.cfg('devswarm_parent.file_log')), JSON.stringify({ ts: Math.floor(ah.clock.now()), event: 'inject', workspaces: inj }) + '\n');
  }
  archiveList.forEach(function (id) { ah.state.writeAtomic(piRel(ah.cfg('devswarm_parent.dir_archive_nudges') + '/' + id + '.json'), JSON.stringify({ lastNudgedAt: now })); });
  if (!segments.length) return 'allow';
  return { advisory: text.advisoryJson(ah.cfg('devswarm_prompt.event'), segments.join(ah.cfg('devswarm_parent.segment_joiner'))) };
}

function decide(p, opts) {
  try { return piDecide(p, opts); } catch (e) { if (e === meshDefer || e === gates.UNDECIDABLE) return piDefer('unsure'); throw e; }
}
