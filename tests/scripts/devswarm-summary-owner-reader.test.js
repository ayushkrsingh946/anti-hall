'use strict';
// GH #181 follow-up 1: the roster/summary counts a workspace's unread from that workspace's OWN reader (the latest live reader row of
// its partition), so its row says what `inbox tick` says for it; the shared #floor is only the fallback for a partition with no reader
// row. Engine twin: ah-engine/src/meshw/union.rs `the_owner_is_the_latest_live_reader_row_...` + summary.rs.

require('../helpers/isolate-home.js'); // HOME -> empty temp dir: this file reads home-dir state
const { test } = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const ROOT = path.join(__dirname, '..', '..', 'plugins', 'anti-hall');
const storeLib = require(path.join(ROOT, 'companion', 'lib', 'devswarm-store.js'));
const rc = require(path.join(ROOT, 'companion', 'lib', 'reader-cursors.js'));

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

test('a workspace is counted from its own reader, not from a lagging reader that holds the floor', () => {
  const { home, s } = seed('lag', [{ reader: 'h:1:1', value: 3, updatedAt: 10 }, { reader: 'h:2:2', value: 1, updatedAt: 5 }]);
  try {
    const sum = storeLib.computeSummary(s, { home, workspaceId: 'X' });
    assert.strictEqual(row(sum).unread, 0, 'X read all 3 (its latest reader is at 3); the floor (1) belongs to the lagging reader');
    assert.strictEqual(row(sum).cursor, 3);
    const own = rc.countFor(s, { reader: 'h:1:1', partition: 'X', home, excludeFamily: true });
    assert.strictEqual(row(sum).unread, own.unread, 'roster unread == what `inbox tick` reports for the same reader');
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

test('a retired reader row is not the owner', () => {
  const { home, s } = seed('retired', [{ reader: 'h:1:1', value: 3, retiredLine: 3, updatedAt: 10 }, { reader: 'h:2:2', value: 2, updatedAt: 5 }]);
  try {
    assert.strictEqual(row(storeLib.computeSummary(s, { home, workspaceId: 'X' })).unread, 1, 'counted from the live reader at 2');
  } finally { s.close(); fs.rmSync(home, { recursive: true, force: true }); }
});
