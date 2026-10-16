// check = "compact-advice-guard" (Stop, loop-safe). Blocks, once per declaration, a final reply that recommends compacting while the
// context is low or a compact just happened: LOW CONTEXT (context % below autoHandover.pct minus guards.compactAdviceMarginPct) or a
// RECENT COMPACT (a boundary within guards.compactAdviceRecentTurns turns), unless the threshold-fired auto-handover latch allows it.
// Almost no reply recommends compacting, so the cheap tests come first: the final text (last_assistant_message, else the current turn's
// assistant text read by ah.transcript.turnText) must hold the words of a recommendation before the transcript is parsed. The phrase
// rules are lib/76-compact-advice.js (the Node regular expressions); the context and latch are lib/75-ctxbudget.js. Every Stop is
// decided here (no Node twin): a relative transcript path is opened from the hook process's directory (hookProc); a compact
// boundary's timestamp is read by jx.isoMs, else jx.dateParse; a request with no absolute HOME (nowhere to record the block), a
// transcript line the interpreter cannot parse as V8 does, and a latch comparison that needs a timestamp only V8 reads allow, each
// logged (ctxbudget.ca_unsure_log_kind). Mirrors hooks/compact-advice-guard.js. Keys and texts: ctxbudget.toml (ctxbudget.ca_*).
'use strict';

function caAllow(w) {
  if (w.inferred) cb.writeInferred(w.inferred);
  return 'allow';
}

// A compact boundary whose timestamp neither jx.isoMs nor jx.dateParse reads (V8's legacy parser and the local zone decide it).
var CA_UNKNOWN_TS = { unknownTs: true };

function caUnsure(why) { ah.log(ah.cfg('ctxbudget.ca_unsure_log_kind'), why); return 'allow'; }

// cadv.readTurn with the timestamp forms jx.dateParse reads too (the shared reader stays as the other guards use it).
function caReadTurn(lines) {
  var tsOf = cadv.tsOf;
  cadv.tsOf = function (e) {
    if (!e || typeof e.timestamp !== 'string') return null;
    var ms = jx.isoMs(e.timestamp);
    if (ms === undefined) ms = jx.dateParse(e.timestamp);
    if (ms === undefined) return CA_UNKNOWN_TS;
    return isFinite(ms) ? ms : null;
  };
  try { return cadv.readTurn(lines); } finally { cadv.tsOf = tsOf; }
}

function caBlock(reason) { return { exact: { code: 0, out: text.render(cb.c('ctxbudget.stop_block_line'), { reason: JSON.stringify(reason) }), err: '' } }; }

function decide(p) {
  if (ah.env.get(ah.cfg('ctxbudget.judge_child_env')) === ah.cfg('ctxbudget.judge_child_on')) return 'allow';
  var home = ah.env.get(ah.cfg('ctxbudget.home_env'));
  // the block is recorded under the request's home; with none there is nowhere to record it, and an unrecorded block never blocks
  if (home === null || !ah.path.isAbsolute(home)) return caUnsure('home');
  if (!ah.settings.bool('ctxbudget.set_compact_advice_guard')) return 'allow';
  if (p === null || typeof p !== 'object' || ah.cfg('coordinator_work.agent_markers').some(function (k) { return p[k] !== undefined && p[k] !== null; }) || p.stop_hook_active === true) return 'allow';
  if (ah.settings.skipped(ah.cfg('ctxbudget.skip_compact_advice'))) return 'allow';
  var tp = typeof p.transcript_path === 'string' ? p.transcript_path : null;
  if (!tp) return 'allow';
  tp = hookProc.open(p, tp); // Node reads a relative path from its own directory, the payload's cwd
  var tailBytes = cb.n('ctxbudget.tail_bytes');
  // an unreadable transcript allows. A zero-byte window opens and measures the file exactly as the full read does, without
  // pulling it in: most Stops end at `mayAdvise` below and never need the text.
  if (ah.fs.tailLines(tp, 0, false, ['\u0000']) === null) return 'allow';
  var lam = typeof p.last_assistant_message === 'string' ? p.last_assistant_message : '';
  if (lam.trim()) {
    if (!cadv.mayAdvise(lam)) return 'allow';
  } else {
    // the final text is the turn's last part: a cheap exact test on the host first, then the whole turn's text (a superset of it)
    var hint = ah.cfg('ctxbudget.advice_prefilter');
    var seen = ah.fs.tailHasAny(tp, tailBytes, ['\\u'], hint.map(function (w) { return w.toLowerCase(); }));
    if (seen === null || !seen) return 'allow';
    var tt = ah.transcript.turnText(tp, cb.n('ctxbudget.tail_bytes'), '');
    if (tt !== null && !tt.unsure && tt.parts && !cadv.mayAdvise(tt.parts.join('\n'))) return 'allow';
  }
  var lines = ah.fs.tailLines(tp, tailBytes, false); // a trailing \r stays: JSON.parse reads past it
  if (lines === null) return 'allow';
  var turn = caReadTurn(lines);
  if (turn === null) return caUnsure('transcript_line');
  var finalText = lam.trim() ? lam : turn.finalText;
  if (!cadv.mayAdvise(finalText)) return 'allow';
  var found = cadv.findAdvice(finalText);
  if (!found.length) return 'allow';
  var last = found[found.length - 1];
  if (cadv.lastRetraction(finalText) > last.index) return 'allow';
  var w = { inferred: null };
  var result = cb.getContextPct(tp, p.session_id, w); // tp is absolute here
  var pct = result && isFinite(result.pct) ? result.pct : null;
  var threshold = ah.settings.num('ctxbudget.set_ah_pct'), margin = ah.settings.num('ctxbudget.set_ca_margin'), window = ah.settings.num('ctxbudget.set_ca_recent');
  var low = pct !== null && pct < threshold - margin;
  var recent = window > 0 && turn.turnsSinceCompact !== null && turn.turnsSinceCompact <= window;
  if (!low && !recent) return caAllow(w);
  var tag = cb.sessionTag(p);
  if (!tag) return caAllow(w);
  var latch = cb.readLatch(tag);
  var firedAt = typeof latch.firedAt === 'number' && isFinite(latch.firedAt) ? latch.firedAt : null;
  // only this comparison needs the boundary's time: one only V8 reads leaves it unknown
  if (turn.compactAt === CA_UNKNOWN_TS && turn.turnsSinceCompact !== null && latch.fired === true && firedAt !== null) return caUnsure('compact_timestamp');
  var compactAfterFire = turn.turnsSinceCompact !== null && (turn.compactAt === null || firedAt === null || turn.compactAt >= firedAt);
  var tokensFired = latch.fired === true && cb.c('ctxbudget.ca_tokens_vias').indexOf(latch.firedVia) !== -1;
  if (latch.fired === true && (!low || tokensFired) && !compactAfterFire) return caAllow(w);
  var hash = ah.sha1(finalText), rel = cb.rel(cb.c('ctxbudget.ca_state_dir'), tag + '.json');
  var prevText = ah.state.readText(rel);
  if (prevText !== null) {
    try { var prev = JSON.parse(prevText); if (prev && prev.hash === hash) return caAllow(w); } catch (e) { /* no state */ }
  }
  if (w.inferred) cb.writeInferred(w.inferred);
  var wrote = false;
  try { wrote = ah.state.writeAtomic(rel, JSON.stringify({ hash: hash, at: ah.clock.now() })); } catch (e) { wrote = false; }
  if (!wrote) return 'allow';
  var why = [pct !== null ? cb.r('ctxbudget.ca_why_pct', { pct: Math.round(pct), threshold: String(threshold) }) : cb.c('ctxbudget.ca_why_unknown')];
  if (recent) {
    var n = turn.turnsSinceCompact;
    why.push(cb.r(n === 0 ? 'ctxbudget.ca_recent_now' : n === 1 ? 'ctxbudget.ca_recent_one' : 'ctxbudget.ca_recent_n', { n: n }));
  }
  var ctx = pct !== null ? cb.r('ctxbudget.ca_ctx_pct', { pct: Math.round(pct) }) : cb.c('ctxbudget.ca_ctx_low');
  return caBlock(text.message('block', cb.c('ctxbudget.ca_guard'), {
    what: cb.r('ctxbudget.ca_what', { phrase: last.phrase, why: why.join(cb.c('ctxbudget.ca_why_join')) }),
    why: cb.c('ctxbudget.ca_why'), instead: cb.r('ctxbudget.ca_instead', { ctx: ctx }),
  }));
}
