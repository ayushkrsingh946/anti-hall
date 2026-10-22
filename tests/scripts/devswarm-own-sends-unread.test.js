'use strict';
// GH #181 (own sends): mail the recipient's own identity family sent is not inbound. `countFor` with `excludeFamily` leaves it out of
// `unread` (the row lists stay whole so an ack still walks them). Engine twin: ah-engine/src/meshw/union.rs
// `mail_the_recipients_own_family_sent_is_not_unread`.

require('../helpers/isolate-home.js'); // HOME -> empty temp dir: this file reads home-dir state
const { test } = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const ROOT = path.join(__dirname, '..', '..', 'plugins', 'anti-hall');
const NODE_ROOT = path.join(__dirname, '..', '..', 'plugins', 'anti-hall-node');
const storeLib = require(path.join(NODE_ROOT, 'companion', 'lib', 'devswarm-store.js'));
const rc = require(path.join(NODE_ROOT, 'companion', 'lib', process.env.AH_TEST_RC_FILE || 'reader-cursors.js'));

function tmpHome() {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'ah-own-sends-'));
  fs.mkdirSync(path.join(home, '.anti-hall'), { recursive: true });
  return home;
}

test('mail the recipient sent itself is left out of unread; others and NULL senders count', () => {
  const home = tmpHome();
  const s = storeLib.openStore({ home, hash: 'ownsends', backend: 'journal' });
  try {
    ['P', 'other', 'P', 'third'].forEach((from, i) => {
      const f = { from, to: 'P', type: 'direct', message: 'm' + i, timestamp: 1700000000000 + i, urgency: 'normal' };
      storeLib.appendMeshMessage(s, Object.assign({}, f, { hash: storeLib.meshMessageHash(f) }));
    });
    const plain = rc.countFor(s, { reader: null, partition: 'P', home });
    assert.strictEqual(plain.unread, 4, 'without the option every row counts (pre-fix shape)');
    const u = rc.countFor(s, { reader: null, partition: 'P', home, excludeFamily: true });
    assert.strictEqual(u.unread, 2, 'P sent two of the four');
    assert.strictEqual(u.ownExcluded, 2);
    assert.strictEqual(u.unreadAll, 4, 'the whole union stays available');
    assert.strictEqual(u.storeOnlyUnreadRows.length, 4, 'the row list stays whole: an ack walks it');
  } finally { s.close(); fs.rmSync(home, { recursive: true, force: true }); }
});
