// check = "devswarm-child-gate" (Stop; devswarm-parent-reply-tracker.js and devswarm-child-drain.js build on this script). The Stop gate
// of a DevSwarm child workspace. A Stop guard is never weaker than Node (D74). The silent exits of the Node hook come first, in Node's
// order: the judge-child exit, the switch, the user skip, a supervisor that is not active, a session that is not a child workspace
// (`exitIfInert`), then the stable launchers the hook installs when it is loaded (they must already be current) and the on-disk
// corroboration of the child role (its registered descriptor). Past them the gate answers what Node answers: a continuing stop
// (`stop_hook_active`) records the check time; the mailbox-cron-missing block (Claude only) with its metric line; the report already
// sent this stop episode (the summary projection's `recent` rows, or an authenticated drop-attempt record and its mismatch notice); the
// fresh zero `inbox tick` marker; the child's own unread mail (its descriptor's durable inbox counted with its own reader in the mesh
// store, ah.mesh.countFor) and the native `hivecontrol workspace message-count` probe; the shared stop-policy budgets per stable
// kind (`heartbeat-report`, `inbox`, `inbox-unknown`, `cron-missing`), the cap notice, and the framed block with the heartbeat, drop
// reason, inbox and mailbox-wake texts, with the gate's state file written as Node writes it. What it cannot settle exactly defers to
// Node before anything is written: an archived workspace (the handover block and its metrics), a session not corroborated by a
// descriptor while the DevSwarm repos directory exists (Node checks its own working directory), no payload working directory where one
// is needed, a store Node reads as UNKNOWN, a native probe that does not finish, a PATH the engine cannot search as Node does, a
// drop-attempt record that only the identity-family fallback could authenticate, a session id that is not a plain string, and a gate
// setting (strict mode, the cron warning age) set anywhere. Mirrors hooks/devswarm-child-gate.js (`main` and its helpers),
// hooks/lib/stop-policy.js, hooks/lib/devswarm-role.js `isChildWorkspaceCorroborated`, hooks/lib/devswarm-wake.js `wakeReassert`,
// hooks/lib/block-message.js `frame` and hooks/lib/skip-cmd.js. Builds on devswarm-child-role.js (script.includes). Keys and texts:
// devswarm_gates.toml (devswarm_gates.*), devswarm_role.toml.
'use strict';

function dgNonEmpty(name) { var v = ah.env.get(name); return v !== null && v.trim() !== ''; }
function dgChild() { return dgNonEmpty(ah.cfg('devswarm_gates.source_branch_env')); }

function dgActive() {
  if (ah.env.get(ah.cfg('devswarm_gates.kill_env')) === ah.cfg('devswarm_gates.kill_env_value')) return false;
  var mode = ah.settings.enum('devswarm_gates.supervisor_mode');
  if (mode === ah.cfg('devswarm_gates.mode_off')) return false;
  if (mode === ah.cfg('devswarm_gates.mode_on')) return true;
  return dgNonEmpty(ah.cfg('devswarm_gates.repo_id_env'));
}

// `devswarm-primary-gate.js` `inert`: the hook would exit silently before doing anything (child role).
function dgInertForChild(switchKey, guard) {
  return !ah.settings.bool(switchKey) || (guard !== null && ah.settings.skipped(guard)) || !dgActive() || !dgChild();
}

// Only id characters (letters, digits and devswarm_gates.readside_id_extra) and no `..`.
function dgIdChars(id) {
  var extra = ah.cfg('devswarm_gates.readside_id_extra');
  return id !== '' && id.indexOf('..') < 0 && id.split('').every(function (c) { return /[A-Za-z0-9]/.test(c) || extra.indexOf(c) >= 0; });
}
// `isSafeId` of companion/lib/liveness.js.
function dgSafeId(id) { return typeof id === 'string' && id !== '.' && dgIdChars(id); }

// ---- the gate ------------------------------------------------------------------------------------------------------------

function cgDirRel() { return ah.cfg('devswarm_role.pg_devswarm_dir'); }
function cgFinite(v) { return typeof v === 'number' && isFinite(v); }
function cgBuilder() { return ah.env.get(ah.cfg('devswarm_gates.readside_builder_env')); }

// JSON.parse(readFileSync(p)) inside Node's try/catch: undefined when it throws (absent, unreadable, not JSON), else the value. A file
// over the read cap or a parse the interpreter cannot finish defers.
function cgJson(p) {
  var r = jx.read(p);
  if (r.big) throw meshDefer;
  if (r.text === undefined) return undefined;
  var j = jx.parse(r.text);
  if (j.unsure) throw meshDefer;
  return j.invalid ? undefined : j.v;
}

// A numeric or boolean gate setting at its shipped default; defers when it is set anywhere.
function cgSetDefault(key) {
  if (ah.settings.touched(key)) throw meshDefer;
  return ah.cfg(key).default;
}

function cgClaude() {
  var raw = ah.env.get(ah.cfg('devswarm_role.agent_env'));
  return (raw === null ? '' : raw.trim().toLowerCase()) === ah.cfg('devswarm_role.claude_agent');
}

// `resolvedId(env)` of devswarm-wake.js: the builder id when it is made only of id characters, else the placeholder.
function cgResolvedId() {
  var b = cgBuilder(), extra = ah.cfg('devswarm_gates.readside_id_extra');
  var ok = b !== null && b !== '' && b.split('').every(function (c) { return /[A-Za-z0-9]/.test(c) || extra.indexOf(c) >= 0; });
  return ok ? b : ah.cfg('devswarm_role.id_placeholder');
}

function cgDq(s) { return '"' + s.replace(/["\\$`]/g, function (c) { return '\\' + c; }) + '"'; }
function cgShQuote(s) { return "'" + s.replace(/'/g, "'\\''") + "'"; }

// `wakeReassertLine(env, true)`: the child's mailbox-wake re-verify line (Claude only).
function cgWakeLine(l) {
  if (!cgClaude()) return '';
  var id = cgResolvedId();
  var tick = text.render(ah.cfg('devswarm_gates.cg_wake_tick'), { id: id });
  var watch = l.watcher ? text.render(ah.cfg('devswarm_role.pg_wake_watch'), { watcher: cgDq(l.watcher) }) : '';
  return text.render(ah.cfg('devswarm_role.pg_wake_line'), { cli: cgDq(l.cli), cron: dwWakeCron(), tick: tick, watch: watch, id: id });
}

// `frame({ guard, headline, why, override, body })` of hooks/lib/block-message.js with the gate's own texts and skip command.
function cgFrame(body, ctx) {
  var clean = text.clean, guard = ah.cfg('devswarm_gates.child_gate_guard_name');
  var cli = posix.normalize(ctx.nodeRoot + '/' + ah.cfg('devswarm_role.launcher_cli').target);
  var skip = text.render(ah.cfg('devswarm_role.pg_skip_cmd'), { cli: cgShQuote(cli), guard: guard });
  var lines = [text.render(ah.cfg('devswarm_role.pg_frame_head'), { guard: clean(guard), headline: clean(ah.cfg('devswarm_gates.cg_headline')) })];
  lines.push(ah.cfg('devswarm_role.pg_frame_why') + clean(ah.cfg('devswarm_gates.cg_why')));
  var paras = body.split(/\n{2,}/).map(clean).filter(function (x) { return x !== ''; });
  if (paras.length > 0) {
    lines.push(ah.cfg('devswarm_role.pg_frame_instead') + paras[0]);
    paras.slice(1).forEach(function (x) { lines.push(ah.cfg('devswarm_role.pg_frame_indent') + x); });
  }
  lines.push(ah.cfg('devswarm_role.pg_frame_override') + clean(skip + ah.cfg('devswarm_role.pg_skip_ttl')));
  return JSON.stringify({ decision: ah.cfg('devswarm_role.pg_decision_block'), reason: lines.join('\n') }) + '\n';
}

// ---- the planned writes (applied only once nothing can defer) --------------------------------------------------------------

function cgPut(ctx, rel, body) {
  if (!Object.prototype.hasOwnProperty.call(ctx.files, rel)) ctx.order.push(rel);
  ctx.files[rel] = body;
}
// The text of a home file as this run left it: {text} (planned or on disk), {absent: true} or {big: true}.
function cgRead(ctx, rel) {
  if (Object.prototype.hasOwnProperty.call(ctx.files, rel)) return { text: ctx.files[rel] };
  return jx.read(ctx.home + '/' + rel);
}

// ---- the shared stop policy (hooks/lib/stop-policy.js) ----------------------------------------------------------------------

function cgPolicyRel(ctx) {
  return cgDirRel() + '/' + ah.cfg('devswarm_gates.cg_policy_dir') + '/' + ctx.sid.replace(/[^A-Za-z0-9_.-]/g, '_') + '.json';
}
function cgBuckets(ctx) {
  var r = cgRead(ctx, cgPolicyRel(ctx));
  if (r.big) throw meshDefer;
  if (r.text === undefined) return {};
  var j = jx.parse(r.text);
  if (j.unsure) throw meshDefer;
  return !j.invalid && j.v && typeof j.v === 'object' && !Array.isArray(j.v) ? j.v : {};
}
function cgKey(ctx, kind) { return ctx.sid + '|' + ah.cfg('devswarm_gates.cg_hook') + '|' + kind; }

// `consume(home, sessionId, hook, kinds, cap, now)`: {block, persisted}. The write is planned; its failure is handled at apply time.
function cgConsume(ctx, kinds, cap) {
  var buckets = cgBuckets(ctx), counts = {}, open = [];
  kinds.forEach(function (k) {
    if (Object.prototype.hasOwnProperty.call(counts, k)) return;
    var b = buckets[cgKey(ctx, k)];
    counts[k] = b && cgFinite(b.count) ? b.count : 0;
    if (counts[k] < cap) open.push(k);
  });
  if (!open.length) return { block: false, persisted: true };
  open.forEach(function (k) {
    counts[k] += 1;
    buckets[cgKey(ctx, k)] = { count: counts[k], lastAt: ctx.now };
  });
  cgPut(ctx, cgPolicyRel(ctx), JSON.stringify(buckets));
  ctx.consumed = cgPolicyRel(ctx);
  return { block: true, persisted: true };
}

// `clear(home, sessionId, hook, kinds?)`.
function cgClear(ctx, kinds) {
  var buckets = cgBuckets(ctx), prefix = ctx.sid + '|' + ah.cfg('devswarm_gates.cg_hook') + '|', changed = false;
  var only = kinds ? kinds.map(function (k) { return prefix + k; }) : null;
  Object.keys(buckets).forEach(function (k) {
    if (k.indexOf(prefix) !== 0) return;
    if (only && only.indexOf(k) < 0) return;
    delete buckets[k];
    changed = true;
  });
  if (changed) cgPut(ctx, cgPolicyRel(ctx), JSON.stringify(buckets));
}

// ---- the gate's own state file ----------------------------------------------------------------------------------------------

function cgReadState(ctx) {
  var parsed = cgJson(ctx.home + '/' + ctx.stateRel);
  if (parsed && typeof parsed === 'object') {
    return {
      blocks: cgFinite(parsed.blocks) ? parsed.blocks : 0,
      lastBlockAt: cgFinite(parsed.lastBlockAt) ? parsed.lastBlockAt : 0,
      totalBlocks: cgFinite(parsed.totalBlocks) ? parsed.totalBlocks : 0,
      lifetimeCapLogged: parsed.lifetimeCapLogged === true,
      nonceFailClosedLogged: parsed.nonceFailClosedLogged === true,
      mismatchLogged: parsed.mismatchLogged === true,
      lastCheckAt: cgFinite(parsed.lastCheckAt) ? parsed.lastCheckAt : 0,
    };
  }
  return { blocks: 0, lastBlockAt: 0, totalBlocks: 0, lifetimeCapLogged: false, nonceFailClosedLogged: false, mismatchLogged: false, lastCheckAt: 0 };
}
function cgWriteState(ctx, state) { cgPut(ctx, ctx.stateRel, JSON.stringify(state)); }

// ---- reads ------------------------------------------------------------------------------------------------------------------

// The payload's working directory (Node falls back to its own, which the engine does not know).
function cgCwd(ctx) {
  var c = ctx.p.cwd;
  if (typeof c !== 'string' || c === '' || c.charAt(0) !== '/') throw meshDefer;
  return c;
}

function cgDescPath(ctx, id) { return ctx.home + '/' + cgDirRel() + '/' + ah.cfg('devswarm_role.pg_dir_workspaces') + '/' + id + '.json'; }

// `isChildWorkspaceCorroborated(env, home, process.cwd())`: a registered descriptor, else the DevSwarm worktree layout around the Node
// hook's own working directory, which can hold only when the repos directory exists (then Node decides).
function cgCorroborated(ctx) {
  var id = cgBuilder();
  if (id !== null && id.trim() !== '' && dgIdChars(id)) {
    var p = cgDescPath(ctx, id);
    if (ah.fs.isFile(p)) return true;
    if (ah.fs.lstat(p) !== null) throw meshDefer; // a descriptor path that is there but no readable file: Node's stat decides
  }
  if (ah.fs.lstat(ctx.home + '/' + ah.cfg('devswarm_gates.cg_repos_dir')) === null) return false;
  throw meshDefer;
}

// The worktree root and project key of the payload's working directory (alreadyReportedThisEpisode / findRecentDropAttempt), or null.
function cgRepoKey(ctx) {
  if (ctx.key !== undefined) return ctx.key;
  var c = ah.repo.context(cgCwd(ctx));
  if (c.unsure) throw meshDefer;
  ctx.key = c.root ? ah.mesh.repoKey(c.root) : null;
  return ctx.key;
}

// `alreadyReportedThisEpisode`: a `recent` row of the project's summary sent by this child since `since`.
function cgReported(ctx, since) {
  var id = cgBuilder();
  if (!dgSafeId(id)) return false;
  var key = cgRepoKey(ctx);
  if (!key) return false;
  var r = jx.read(ctx.home + '/' + cgDirRel() + '/' + ah.cfg('devswarm_role.pg_dir_summaries') + '/' + key + '.json');
  if (r.big) throw meshDefer;
  if (r.text === undefined) return false;
  var raw = r.text.trim();
  if (!raw) return false;
  var j = jx.parse(raw);
  if (j.unsure) throw meshDefer;
  if (j.invalid) return false;
  var recent = j.v && Array.isArray(j.v.recent) ? j.v.recent : [];
  return recent.some(function (x) { return x && x.from === id && cgFinite(x.ts) && x.ts >= since; });
}

// `findRecentDropAttempt`: the freshest drop-attempt record of this project since `since` that this hook authenticates (its reader
// nonce, or a session id of its own descriptor), and the mismatch diagnosis of an own-id record that authenticates against neither.
function cgDrop(ctx, since, diag) {
  var id = cgBuilder();
  if (!dgSafeId(id)) return null;
  var key = cgRepoKey(ctx);
  if (!key) return null;
  var nonce = ah.mesh.readerNonce();
  var own = [];
  var desc = cgJson(cgDescPath(ctx, id));
  if (desc && typeof desc === 'object' && desc.sessionId !== null && desc.sessionId !== undefined && String(desc.sessionId) !== '') own.push(String(desc.sessionId));
  var base = ctx.home + '/' + cgDirRel() + '/' + ah.cfg('devswarm_gates.cg_attempts_dir'), ext = ah.cfg('devswarm_gates.cg_attempts_ext');
  var dir = base + '/' + key, sources = [];
  var names = ah.fs.readdir(dir);
  if (names === null && ah.fs.isDir(dir)) throw meshDefer;
  (names || []).forEach(function (n) { if (n.slice(-ext.length) === ext) sources.push(dir + '/' + n); });
  if (ah.fs.isFile(base + '/' + key + ext)) sources.push(base + '/' + key + ext);
  var latest = null, mismatch = null;
  sources.forEach(function (f) {
    var r = jx.read(f);
    if (r.big) throw meshDefer;
    if (r.text === undefined) return;
    r.text.split('\n').filter(Boolean).forEach(function (line) {
      var j = jx.parse(line);
      if (j.unsure) throw meshDefer;
      if (j.invalid) return;
      var row = j.v;
      if (!row || !cgFinite(row.ts) || row.ts < since) return;
      var nonceMatch = typeof nonce === 'string' && typeof row.instanceNonce === 'string' && row.instanceNonce === nonce;
      var sessionMatch = typeof row.sessionId === 'string' && row.sessionId !== '' && own.indexOf(row.sessionId) >= 0;
      // without an own session id Node scans every descriptor of the identity family for one: Node's
      if (!nonceMatch && !sessionMatch && own.length === 0 && typeof row.sessionId === 'string' && row.sessionId !== '') throw meshDefer;
      if (!nonceMatch && !sessionMatch) {
        if (row.id === id && (!mismatch || row.ts > mismatch.ts)) mismatch = row;
        return;
      }
      if (!latest || row.ts > latest.ts) latest = row;
    });
  });
  var plen = ah.cfgNum('devswarm_gates.cg_nonce_prefix_len');
  if (!latest && mismatch) {
    diag.mismatch = {
      row: typeof mismatch.instanceNonce === 'string' ? mismatch.instanceNonce.slice(0, plen) : null,
      own: typeof nonce === 'string' ? nonce.slice(0, plen) : null,
    };
  }
  return latest;
}

// `describeDropAttempt`: the whitelisted label and remedy of a known drop reason, else the generic text.
function cgDescribeDrop(drop) {
  var reason = typeof drop.reason === 'string' ? drop.reason : null;
  var labels = ah.cfg('devswarm_gates.cg_drop_labels'), remedies = ah.cfg('devswarm_gates.cg_drop_remedies');
  var label = reason !== null && Object.prototype.hasOwnProperty.call(labels, reason) ? labels[reason] : null;
  var remedy = reason !== null && Object.prototype.hasOwnProperty.call(remedies, reason) ? remedies[reason] : null;
  if (remedy) remedy = remedy.split(ah.cfg('devswarm_role.id_placeholder')).join(cgResolvedId());
  if (label && remedy) return text.render(ah.cfg('devswarm_gates.cg_drop_known'), { label: label, remedy: remedy });
  return ah.cfg('devswarm_gates.cg_drop_unknown');
}

// `readDurableUnread`: {known, count} of the child's own mailbox (its descriptor's inbox, with the project's store when its worktree
// names one). A store Node reads as UNKNOWN defers (in the host).
function cgDurable(ctx) {
  if (ctx.durable !== undefined) return ctx.durable;
  var none = { known: false, count: 0 }, id = cgBuilder();
  if (!dgSafeId(id)) { ctx.durable = none; return none; }
  var desc = cgJson(cgDescPath(ctx, id));
  if (!desc || typeof desc !== 'object' || !desc.inboxPath) { ctx.durable = none; return none; }
  var inbox = desc.inboxPath, cursor = desc.cursorPath, wt = desc.worktreePath;
  if (typeof inbox !== 'string' || inbox.charAt(0) !== '/') throw meshDefer; // Node resolves it against its own working directory
  if (cursor === undefined || cursor === null || cursor === '' || cursor === false || cursor === 0) cursor = null;
  else if (typeof cursor !== 'string' || cursor.charAt(0) !== '/') throw meshDefer;
  var key = null;
  if (wt) {
    if (typeof wt !== 'string') throw meshDefer;
    key = ah.mesh.repoKey(wt);
  }
  var c = ah.mesh.countFor(key, id, inbox, cursor, ctx.now);
  ctx.durable = { known: !!c.known, count: c.known ? c.unread : 0 };
  return ctx.durable;
}

// `probeNativeMessageCount`: a count, 'absent' (no hivecontrol on the PATH) or null (it failed: unknown). A PATH the engine cannot
// search exactly as Node's spawn does, and a probe that does not finish, defer.
function cgNative(ctx) {
  if (ctx.native !== undefined) return ctx.native;
  var path = ah.env.get(ah.cfg('devswarm_gates.cg_path_env')), prog = ah.cfg('devswarm_gates.cg_probe_program');
  if (path === null || path === '') throw meshDefer;
  var dirs = path.split(':');
  if (dirs.some(function (d) { return d === '' || d.charAt(0) !== '/'; })) throw meshDefer;
  if (!dirs.some(function (d) { return ah.fs.isFile(d + '/' + prog); })) { ctx.native = 'absent'; return ctx.native; }
  var r = ah.exec(prog, ah.cfg('devswarm_gates.cg_probe_args'), { timeoutMs: ah.cfgNum('devswarm_gates.cg_probe_timeout_ms') });
  if (r === null || r.truncated) throw meshDefer;
  var n = null;
  if (r.status === 0) {
    var m = String(r.stdout || '').trim().match(new RegExp(ah.cfg('devswarm_gates.cg_probe_count_re')));
    if (m) {
      var v = parseInt(m[0], 10);
      n = isFinite(v) && v >= 0 ? v : null;
    }
  }
  ctx.native = n;
  return n;
}

// `hasUnreadParentMessages`: true, false or 'unknown'.
function cgHasUnread(ctx) {
  var d = cgDurable(ctx);
  if (d.known && d.count > 0) return true;
  if (cgSetDefault('devswarm_gates.cg_strict_setting') === false) return false;
  var n = cgNative(ctx);
  if (n === 'absent') return false;
  if (n === null) return 'unknown';
  return n > 0;
}

// `tickMarkerFreshZero`: a fresh `inbox tick` marker that found nothing, with a known count.
function cgTickFresh(ctx) {
  var id = cgBuilder();
  if (!dgSafeId(id)) return false;
  var m = cgJson(ctx.home + '/' + cgDirRel() + '/' + ah.cfg('devswarm_parent.dir_wake_tick') + '/' + id + '.json');
  if (!m || typeof m !== 'object') return false;
  if (!cgFinite(m.ts)) return false;
  if ((ctx.now - m.ts) > ah.cfgNum('devswarm_gates.cg_tick_fresh_ms')) return false;
  if (m.unreadTotal !== 0) return false;
  if (m.meshGapWithheld) return false;
  return m.known === true;
}

// `cronMissingWarning`: the warning text, or null (not Claude, no usable id, a recent tick, or too new to judge).
function cgCronWarn(ctx) {
  if (!cgClaude()) return null;
  var id = cgBuilder();
  if (!dgSafeId(id)) return null;
  var warnMin = cgSetDefault('devswarm_parent.set_cron_warn'), perMin = ah.cfgNum('devswarm_role.pg_ms_per_min');
  var warnMs = warnMin * perMin, dir = ctx.home + '/' + cgDirRel() + '/';
  var marker = cgJson(dir + ah.cfg('devswarm_parent.dir_wake_tick') + '/' + id + '.json');
  var ageMs = marker && cgFinite(marker.ts) ? ctx.now - marker.ts : null;
  if (ageMs !== null) {
    if (ageMs <= warnMs) return null;
    return text.render(ah.cfg('devswarm_gates.cg_cron_missing_tick'), { m: String(Math.round(ageMs / perMin)) }) + cgWakeLine(ctx.l);
  }
  var hb = cgJson(dir + ah.cfg('devswarm_gates.cg_heartbeats_dir') + '/' + id + '.json');
  var hbAge = hb && cgFinite(hb.ts) ? ctx.now - hb.ts : null;
  if (hbAge === null || hbAge <= warnMs) return null;
  return text.render(ah.cfg('devswarm_gates.cg_cron_missing_hb'), { m: String(Math.round(hbAge / perMin)) }) + cgWakeLine(ctx.l);
}

// `recordCronMissingWarn`: one `{ts}` line appended, the file capped to its newest lines.
function cgCronMetric(ctx) {
  var rel = cgDirRel() + '/' + ah.cfg('devswarm_gates.cg_cron_metric_file');
  var r = cgRead(ctx, rel);
  if (r.big) throw meshDefer;
  var lines = r.text === undefined ? [] : r.text.split('\n').filter(Boolean);
  lines.push(JSON.stringify({ ts: ctx.now }));
  var cap = ah.cfgNum('devswarm_gates.cg_cron_metric_cap');
  if (lines.length > cap) lines = lines.slice(lines.length - cap);
  cgPut(ctx, rel, lines.join('\n') + '\n');
}

// The session key: session_id, else a hash of the transcript path, else `unknown`.
function cgSession(p) {
  var s = p.session_id;
  if (s) {
    if (typeof s !== 'string' || jx.loneSurrogate(s)) throw meshDefer; // String() of another value, a key JavaScript would escape
    return s;
  }
  var tp = p.transcript_path;
  if (tp) {
    if (typeof tp !== 'string') throw meshDefer;
    return ah.sha1(tp).slice(0, ah.cfgNum('devswarm_gates.cg_session_hash_len'));
  }
  return ah.cfg('devswarm_gates.cg_unknown_session');
}

function cgGate(p, root, l) {
  var ctx = { p: p, l: l, home: l.home, now: ah.clock.now(), files: Object.create(null), order: [], err: '', out: '', consumed: null,
    nodeRoot: dwNodeRoot(root), key: undefined, durable: undefined, native: undefined };
  if (ctx.nodeRoot === null) throw meshDefer;
  if (!cgCorroborated(ctx)) return ctx;
  if (!jx.isObj(p)) throw meshDefer; // a payload that is not an object takes Node paths this script does not follow
  ctx.sid = cgSession(p);
  ctx.stateRel = cgDirRel() + '/' + ah.cfg('devswarm_gates.cg_state_dir') + '/' + ctx.sid.replace(/[^A-Za-z0-9_.-]/g, '_') + '.json';
  var id = cgBuilder(), MAX = ah.cfgNum('devswarm_gates.cg_max_blocks');

  // an archived workspace: the handover block, its loop budget and metrics are Node's
  if (ah.settings.bool('devswarm_prompt.archived_stop_setting') && dgSafeId(id)) {
    var desc = cgJson(cgDescPath(ctx, id)), wt = null;
    if (desc && typeof desc.worktreePath === 'string' && desc.worktreePath) wt = desc.worktreePath;
    if (!wt) {
      var c = ah.repo.context(cgCwd(ctx));
      if (c.unsure) throw meshDefer;
      wt = c.toplevel || null;
    }
    if (wt) {
      var a = JSON.parse(ahHost.ctArchived(id, wt, ctx.sid));
      if (a.unsure || a.archived) throw meshDefer;
    }
  }

  var state = cgReadState(ctx);
  if (p.stop_hook_active === true) {
    state.lastCheckAt = ctx.now;
    cgWriteState(ctx, state);
    return ctx;
  }

  var cronWarn = cgCronWarn(ctx), kCron = ah.cfg('devswarm_gates.cg_kind_cron');
  if (cronWarn !== null) {
    if (cgConsume(ctx, [kCron], MAX).block) {
      cgCronMetric(ctx);
      state.lastCheckAt = ctx.now;
      cgWriteState(ctx, state);
      ctx.out = cgFrame(cronWarn, ctx);
      return ctx;
    }
  } else cgClear(ctx, [kCron]);

  var prevCheckAt = state.lastCheckAt;
  state.lastCheckAt = ctx.now;
  var episodeSince = Math.max(state.lastBlockAt, prevCheckAt || (ctx.now - ah.cfgNum('devswarm_gates.cg_reset_ms')));
  var reported = cgReported(ctx, episodeSince);
  var diag = {};
  var drop = reported ? null : cgDrop(ctx, episodeSince, diag);
  if (diag.mismatch && !state.mismatchLogged) {
    state.mismatchLogged = true;
    ctx.err += text.render(ah.cfg('devswarm_gates.cg_err_mismatch'), { sid: JSON.stringify(ctx.sid), row: JSON.stringify(diag.mismatch.row), own: JSON.stringify(diag.mismatch.own) });
    cgWriteState(ctx, state);
  }
  if (reported || drop) {
    if (!cgDurable(ctx).unknown && cgHasUnread(ctx) === false) {
      cgClear(ctx, null);
      cgWriteState(ctx, state);
      return ctx;
    }
  }
  if (cgTickFresh(ctx)) {
    if (!cgDurable(ctx).unknown && cgHasUnread(ctx) === false) {
      cgClear(ctx, null);
      cgWriteState(ctx, state);
      return ctx;
    }
  }

  var pending = cgHasUnread(ctx);
  var tickFresh = cgTickFresh(ctx);
  var heartbeatSatisfied = !!(reported || tickFresh);
  var outboundSatisfied = !!(reported || drop || tickFresh);
  var nativeOnlyUnknown = outboundSatisfied && pending === 'unknown';
  var kinds = (nativeOnlyUnknown || heartbeatSatisfied) ? [] : [ah.cfg('devswarm_gates.cg_kind_report')];
  if (pending === true) kinds.push(ah.cfg('devswarm_gates.cg_kind_inbox'));
  else if (pending === 'unknown') kinds.push(ah.cfg('devswarm_gates.cg_kind_unknown'));
  if (kinds.length === 0) {
    cgClear(ctx, [ah.cfg('devswarm_gates.cg_kind_report'), ah.cfg('devswarm_gates.cg_kind_inbox'), ah.cfg('devswarm_gates.cg_kind_unknown')]);
    cgWriteState(ctx, state);
    return ctx;
  }
  var decision = cgConsume(ctx, kinds, nativeOnlyUnknown ? 1 : MAX);
  if (!decision.block) {
    if (!state.lifetimeCapLogged) {
      cgWriteState(ctx, Object.assign({}, state, { lifetimeCapLogged: true }));
      ctx.err += text.render(ah.cfg('devswarm_gates.cg_err_cap'), { max: String(MAX), sid: JSON.stringify(ctx.sid) });
    } else cgWriteState(ctx, state);
    return ctx;
  }
  cgWriteState(ctx, Object.assign({}, state, { lastBlockAt: ctx.now, lifetimeCapLogged: false }));

  var cli = l.cli, rid = cgResolvedId(), args = { cli: cli, id: rid };
  var prefix = pending === true ? text.render(ah.cfg('devswarm_gates.cg_inbox_pending'), args)
    : pending === 'unknown' ? text.render(ah.cfg(nativeOnlyUnknown ? 'devswarm_gates.cg_inbox_native_only' : 'devswarm_gates.cg_inbox_native_unknown'), args)
    : '';
  var wakeLine = cgWakeLine(l);
  var outbound = drop ? text.render(ah.cfg('devswarm_gates.cg_heartbeat_drop'), { drop: cgDescribeDrop(drop) })
    : text.render(ah.cfg('devswarm_gates.cg_heartbeat'), args);
  var reason = nativeOnlyUnknown ? prefix.trim() : heartbeatSatisfied ? prefix + wakeLine : prefix + outbound + wakeLine;
  ctx.out = cgFrame(reason, ctx);
  return ctx;
}

// Apply what the gate decided, only once nothing can defer any more. A stop-policy budget the disk refuses to record is Node's
// fail-open: no block (and the gate's state file as the allow path writes it).
function cgApply(ctx) {
  if (ctx.order.length) ah.commit();
  var out = ctx.out;
  for (var i = 0; i < ctx.order.length; i++) {
    var rel = ctx.order[i];
    if (!ah.state.writeAtomic(rel, ctx.files[rel]) && rel === ctx.consumed) out = '';
  }
  return out === '' && ctx.err === '' ? 'allow' : { exact: { code: 0, out: out, err: ctx.err } };
}

function decide(p, opts, event) {
  if (dwJudgeChild()) return 'allow';
  if (dgInertForChild('devswarm_gates.child_gate_setting', ah.cfg('devswarm_gates.child_gate_guard_name'))) return 'allow';
  if (event !== 'Stop') return 'defer';
  var root = jx.isObj(opts) && typeof opts.plugin_root === 'string' ? opts.plugin_root : ah.env.get(ah.cfg('env.plugin_root'));
  var l = dwLaunchersCurrent(root, true);
  if (l === null) return 'defer'; // Node installs the launchers when it is loaded
  var ctx;
  try { ctx = cgGate(p, root, l); } catch (e) { if (e === meshDefer) return 'defer'; throw e; }
  return cgApply(ctx);
}
