'use strict';
// GH #203: `send --to <8-char short id>` (the id the roster and the broadcast feed show)
// failed closed as `unregistered-recipient`; only the full uuid worked. resolveSendTarget
// matched `--to` against a row's meshId and its exact `id` only. A unique id prefix must
// resolve; an ambiguous one must refuse and list every candidate (never pick one).

require('../helpers/isolate-home.js'); // HOME -> empty temp dir
const { test } = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const cp = require('node:child_process');

process.env.ANTI_HALL_LOG_DIR = fs.mkdtempSync(path.join(os.tmpdir(), 'anti-hall-prefix-log-'));

const cli = require('../../plugins/anti-hall/scripts/devswarm.js');
const storeLib = require('../../plugins/anti-hall/companion/lib/devswarm-store.js');
const repokey = require('../../plugins/anti-hall/companion/lib/devswarm-repokey.js');

function tmpHome() {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'anti-hall-prefix-'));
  fs.mkdirSync(path.join(home, '.anti-hall'), { recursive: true });
  return home;
}
function rm(p) { try { fs.rmSync(p, { recursive: true, force: true }); } catch (_) {} }
const ctx = (home, over) => Object.assign({ home, backend: 'journal', env: {} }, over || {});
function makeGitRepo(tag) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'anti-hall-prefix-repo-' + tag + '-'));
  cp.spawnSync('git', ['init', '-q', dir]);
  cp.spawnSync('git', ['-C', dir, 'config', 'user.email', 'a@b.c']);
  cp.spawnSync('git', ['-C', dir, 'config', 'user.name', 'Test']);
  fs.writeFileSync(path.join(dir, 'README.md'), tag);
  cp.spawnSync('git', ['-C', dir, 'add', '.']);
  cp.spawnSync('git', ['-C', dir, 'commit', '-q', '-m', 'init']);
  return dir;
}
function seed(home, repoKey, descs) {
  const s = storeLib.openStore({ home, hash: repoKey, backend: 'journal' });
  try { for (const d of descs) s.upsertRegistry(d); } finally { s.close(); }
}

const UUID_A = '48f38c9e-8dc2-426a-adb2-f9401ab84e99';
const UUID_B = '48f38c9e-1111-426a-adb2-f9401ab84e00';
const UUID_C = '9b1d0000-8dc2-426a-adb2-f9401ab84e01';

test('#203: send --to <8-char short id> resolves to the full-id partition', () => {
  const home = tmpHome();
  const repo = makeGitRepo('uniq');
  try {
    const repoKey = repokey.repoKeyForWorktree(repo);
    seed(home, repoKey, [{ id: UUID_A, worktreePath: repo, sessionId: 's' }, { id: UUID_C, worktreePath: repo, sessionId: 's2' }]);
    const r = cli.run(['send', '--to', UUID_A.slice(0, 8), '--message', 'hi'], ctx(home, { cwd: repo }));
    assert.equal(r.result.ok, true, JSON.stringify(r.result));
    const s = storeLib.openStore({ home, hash: repoKey, backend: 'journal' });
    try {
      assert.equal(s.listMessages(UUID_A).length, 1, 'the message lands in the full-id partition');
      assert.equal(s.listMessages(UUID_C).length, 0);
    } finally { s.close(); }
  } finally { rm(home); rm(repo); }
});

test('#203: an ambiguous prefix refuses, lists every candidate, and writes nothing', () => {
  const home = tmpHome();
  const repo = makeGitRepo('amb');
  try {
    const repoKey = repokey.repoKeyForWorktree(repo);
    seed(home, repoKey, [{ id: UUID_A, worktreePath: repo, sessionId: 's' }, { id: UUID_B, worktreePath: repo, sessionId: 's2' }]);
    const r = cli.run(['send', '--to', '48f38c9e', '--message', 'hi'], ctx(home, { cwd: repo }));
    assert.equal(r.result.ok, false);
    assert.equal(r.result.reason, 'ambiguous-recipient');
    assert.ok(r.result.error.includes(UUID_A) && r.result.error.includes(UUID_B), r.result.error);
    const s = storeLib.openStore({ home, hash: repoKey, backend: 'journal' });
    try {
      assert.equal(s.listMessages(UUID_A).length + s.listMessages(UUID_B).length, 0);
    } finally { s.close(); }
  } finally { rm(home); rm(repo); }
});

test('#203: a prefix matching nothing still fails closed as unregistered-recipient', () => {
  const home = tmpHome();
  const repo = makeGitRepo('none');
  try {
    const repoKey = repokey.repoKeyForWorktree(repo);
    seed(home, repoKey, [{ id: UUID_A, worktreePath: repo, sessionId: 's' }]);
    const r = cli.run(['send', '--to', 'deadbeef', '--message', 'hi'], ctx(home, { cwd: repo }));
    assert.equal(r.result.ok, false);
    assert.equal(r.result.reason, 'unregistered-recipient');
  } finally { rm(home); rm(repo); }
});

// #203 follow-up: a Primary's registry id is a mesh id (`primary-<hash>`); the first 8 chars
// were `primary-`, useless as a label and ambiguous. The short form keeps the hash.
const names = require('../../plugins/anti-hall/companion/lib/devswarm-names.js');
const PRIMARY = 'primary-d7a18d81';

test('#203: a primary-<hash> id shortens to a form that keeps its hash; uuids keep 8 chars', () => {
  assert.equal(names.shortId(PRIMARY), PRIMARY);
  assert.equal(names.shortId('primary-d7a18d81ffee0011'), PRIMARY);
  assert.equal(names.displayName(PRIMARY, 'Primary Workspace'), 'Primary Workspace (primary-d7a18d81)');
  assert.equal(names.shortId(UUID_A), UUID_A.slice(0, 8));
});

test('#203: send --to <primary short form> resolves to the Primary', () => {
  const home = tmpHome();
  const repo = makeGitRepo('primary');
  try {
    const repoKey = repokey.repoKeyForWorktree(repo);
    seed(home, repoKey, [{ id: PRIMARY, worktreePath: repo, sessionId: 's' }, { id: UUID_C, worktreePath: repo, sessionId: 's2' }]);
    const r = cli.run(['send', '--to', names.shortId(PRIMARY), '--message', 'hi'], ctx(home, { cwd: repo }));
    assert.equal(r.result.ok, true, JSON.stringify(r.result));
    const s = storeLib.openStore({ home, hash: repoKey, backend: 'journal' });
    try { assert.equal(s.listMessages(PRIMARY).length, 1); } finally { s.close(); }
  } finally { rm(home); rm(repo); }
});
