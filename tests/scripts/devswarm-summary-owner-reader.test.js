'use strict';
// GH #181 follow-up 1: the roster/summary base of a partition is the MINIMUM position over its LIVE (non-retired) reader rows, computed at
// read time; the stored #floor (which a dead reader can hold back) is only the fallback for a partition with no live reader row.
// Engine twin: ah-engine/src/meshw/union.rs `the_live_base_is_...` + summary.rs.

require('../helpers/isolate-home.js'); // HOME -> empty temp dir: this file reads home-dir state
const { test } = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const ROOT = path.join(__dirname, '..', '..', 'plugins', 'anti-hall');
const NODE_ROOT = path.join(__dirname, '..', '..', 'plugins', 'anti-hall-node');
const storeLib = require(path.join(NODE_ROOT, 'companion', 'lib', 'devswarm-store.js'));
const rc = require(path.join(NODE_ROOT, 'companion', 'lib', 'reader-cursors.js'));

function seed(tag, readers) {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'ah-owner-' + tag + '-'));
  fs.mkdirSync(path.join(home, '.anti-hall'), { recursive: true });
  const s = storeLib.openStore({ home, hash: 'ownerrdr', backend: 'journal' });
  s.upsertRegistry({ id: 'X', worktreePath: path.join(home, 'wt'), sessionId: 'sx', inboxPath: null, cursorPath: null, nudgeCommand: null });
  ['a', 'b', 'c'].forEach((m, i) => {
    const f = { from: 'other', to: 'X', type: 'direct', message: m, timestamp: 1700000000000 + i, urgency: 'normal' };
    storeLib.appendMeshMessage(s, Object.assign({}, f, { hash: storeLib.meshMessageHash(f) }));
  });
  s.readerCursorTxn((tx) => {
    tx.put({ partition: 'X', ns: 'store', reader: '#floor', value: 1, updatedAt: 1 });
    tx.put({ partition: 'X', ns: 'nd', reader: '#floor', value: 0, updatedAt: 1 });
    for (const r of readers) tx.put(Object.assign({ partition: 'X', ns: 'store' }, r));
  });
  return { home, s };
}
const row = (sum) => sum.workspaces.X;

test('the roster does not count from a floor a dead (retired) reader holds back', () => {
  const { home, s } = seed('dead', [{ reader: 'h:1:1', value: 3, updatedAt: 10 }, { reader: 'h:2:2', value: 1, retiredLine: 1, updatedAt: 5 }]);
  try {
    const sum = storeLib.computeSummary(s, { home, workspaceId: 'X' });
    assert.strictEqual(row(sum).unread, 0, 'the only live reader read all 3; the stored floor (1) belongs to the dead one');
    assert.strictEqual(row(sum).cursor, 3);
  } finally { s.close(); fs.rmSync(home, { recursive: true, force: true }); }
});

test('two live readers: the roster shows the least-advanced one (nothing hidden)', () => {
  const { home, s } = seed('live', [{ reader: 'h:1:1', value: 3, updatedAt: 10 }, { reader: 'h:2:2', value: 1, updatedAt: 5 }]);
  try {
    const sum = storeLib.computeSummary(s, { home, workspaceId: 'X' });
    assert.strictEqual(row(sum).unread, 2);
    assert.strictEqual(row(sum).cursor, 1);
  } finally { s.close(); fs.rmSync(home, { recursive: true, force: true }); }
});

test('a partition with no reader row falls back to the floor', () => {
  const { home, s } = seed('floor', []);
  try {
    const sum = storeLib.computeSummary(s, { home, workspaceId: 'X' });
    assert.strictEqual(row(sum).unread, 2);
    assert.strictEqual(row(sum).cursor, 1);
  } finally { s.close(); fs.rmSync(home, { recursive: true, force: true }); }
});

test('a retired reader row is not live', () => {
  const { home, s } = seed('retired', [{ reader: 'h:1:1', value: 3, retiredLine: 3, updatedAt: 10 }, { reader: 'h:2:2', value: 2, updatedAt: 5 }]);
  try {
    assert.strictEqual(row(storeLib.computeSummary(s, { home, workspaceId: 'X' })).unread, 1, 'counted from the live reader at 2 (h:1:1 retired)');
  } finally { s.close(); fs.rmSync(home, { recursive: true, force: true }); }
});
