'use strict';
// GH #203 (live): a store row id c5dbd3c3-... with meshId primary-46debb6e and NO descriptor. `send` resolved the short id
// the roster prints, but every verb that goes through resolveArchiveId (archive, unarchive, nudge, gate, plan, scope, respawn,
// correct, archive-ignore/unignore, wake-directive) only searched descriptor ids and answered 'invalid or missing workspace id'.
// One canonical resolver: exact id -> meshId -> unique prefix over registry AND descriptor ids -> else ambiguous + candidates.

require('../helpers/isolate-home.js'); // HOME -> empty temp dir
const { test } = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const cp = require('node:child_process');

process.env.ANTI_HALL_LOG_DIR = fs.mkdtempSync(path.join(os.tmpdir(), 'anti-hall-wsref-log-'));

const cli = require('../../plugins/anti-hall-node/scripts/devswarm.js');
const storeLib = require('../../plugins/anti-hall-node/companion/lib/devswarm-store.js');
const repokey = require('../../plugins/anti-hall-node/companion/lib/devswarm-repokey.js');

function tmpHome() {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'anti-hall-wsref-'));
  fs.mkdirSync(path.join(home, '.anti-hall'), { recursive: true });
  return home;
}
function rm(p) { try { fs.rmSync(p, { recursive: true, force: true }); } catch (_) {} }
const ctx = (home, over) => Object.assign({ home, backend: 'journal', env: {} }, over || {});
function makeGitRepo(tag) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'anti-hall-wsref-repo-' + tag + '-'));
  cp.spawnSync('git', ['init', '-q', dir]);
  cp.spawnSync('git', ['-C', dir, 'config', 'user.email', 'a@b.c']);
  cp.spawnSync('git', ['-C', dir, 'config', 'user.name', 'Test']);
  fs.writeFileSync(path.join(dir, 'README.md'), tag);
  cp.spawnSync('git', ['-C', dir, 'add', '.']);
  cp.spawnSync('git', ['-C', dir, 'commit', '-q', '-m', 'init']);
  return dir;
}
function seed(home, repoKey, rows) {
  const s = storeLib.openStore({ home, hash: repoKey, backend: 'journal' });
  try { for (const d of rows) s.upsertRegistry(d); } finally { s.close(); }
}

const UUID_A = 'c5dbd3c3-1e53-4a0b-9d11-0123456789ab';
const UUID_B = '9b1d0000-8dc2-426a-adb2-f9401ab84e01';
const UUID_C = 'c5dbd3c3-ffff-426a-adb2-f9401ab84e02';

test('#203: a registry-only row (no descriptor) resolves by short id, full id and meshId in every resolveArchiveId verb', () => {
  const home = tmpHome();
  const repo = makeGitRepo('only');
  try {
    const repoKey = repokey.repoKeyForWorktree(repo);
    seed(home, repoKey, [{ id: UUID_A, worktreePath: repo, sessionId: 's' }, { id: UUID_B, worktreePath: path.join(repo, 'other'), sessionId: 's2' }]);
    const c = ctx(home, { cwd: repo });
    assert.deepStrictEqual(cli.resolveArchiveId(UUID_A.slice(0, 8), c), { ok: true, id: UUID_A }, 'short id');
    assert.deepStrictEqual(cli.resolveArchiveId(UUID_A, c), { ok: true, id: UUID_A }, 'full id');
    assert.deepStrictEqual(cli.resolveArchiveId(UUID_B.slice(0, 8), c), { ok: true, id: UUID_B }, 'every short id the roster prints');
    // a verb that goes through resolveTargetId
    const r = cli.run(['archive-ignore', UUID_A.slice(0, 8)], c);
    assert.strictEqual(r.result.ok, true, JSON.stringify(r.result));
    assert.strictEqual(r.result.id, UUID_A, 'the verb acts on the full id');
    const u = cli.run(['archive-unignore', UUID_A.slice(0, 8)], c);
    assert.strictEqual(u.result.id, UUID_A);
  } finally { rm(home); rm(repo); }
});

test('#203: an ambiguous prefix over registry rows refuses with every candidate; nothing matching stays invalid', () => {
  const home = tmpHome();
  const repo = makeGitRepo('amb');
  try {
    const repoKey = repokey.repoKeyForWorktree(repo);
    seed(home, repoKey, [{ id: UUID_A, worktreePath: repo, sessionId: 's' }, { id: UUID_C, worktreePath: path.join(repo, 'o'), sessionId: 's2' }]);
    const c = ctx(home, { cwd: repo });
    const r = cli.resolveArchiveId('c5dbd3c3', c);
    assert.strictEqual(r.ok, false);
    assert.deepStrictEqual(r.candidates, [UUID_A, UUID_C].sort());
    assert.match(r.error, /ambiguous/);
    const v = cli.run(['archive-ignore', 'c5dbd3c3'], c);
    assert.strictEqual(v.code, 2);
    assert.deepStrictEqual(v.result.candidates, [UUID_A, UUID_C].sort());
    assert.strictEqual(cli.resolveArchiveId('deadbeef', c).ok, false);
  } finally { rm(home); rm(repo); }
});

// A DESCRIPTOR-only workspace (the app uuid: no registry row of its own) whose worktree derives to a registered meshId row.
// `send --to <full descriptor id>` already routes to that row (descriptorMeshId alias); its short id must too.
test('#203: send --to <descriptor short id> resolves through the descriptor id like the full id does', () => {
  const home = tmpHome();
  const repo = makeGitRepo('desc');
  try {
    const repoKey = repokey.repoKeyForWorktree(repo);
    seed(home, repoKey, [{ id: UUID_B, worktreePath: repo, sessionId: 's2' }]);
    const wsDir = path.join(home, '.anti-hall', 'devswarm', 'workspaces');
    fs.mkdirSync(wsDir, { recursive: true });
    fs.writeFileSync(path.join(wsDir, UUID_A + '.json'), JSON.stringify({ id: UUID_A, worktreePath: repo }));
    const c = ctx(home, { cwd: repo });
    const full = cli.run(['send', '--to', UUID_A, '--message', 'hi'], c);
    assert.strictEqual(full.result.ok, true, 'control: the full descriptor id routes: ' + JSON.stringify(full.result));
    const short = cli.run(['send', '--to', UUID_A.slice(0, 8), '--message', 'hi'], c);
    assert.strictEqual(short.result.ok, true, 'the short descriptor id routes too: ' + JSON.stringify(short.result));
    const s = storeLib.openStore({ home, hash: repoKey, backend: 'journal' });
    try { assert.strictEqual(s.listMessages(UUID_B).length, 2); } finally { s.close(); }
  } finally { rm(home); rm(repo); }
});

test('#203: an exact id wins over a longer id that merely starts with it', () => {
  const home = tmpHome();
  const repo = makeGitRepo('exact');
  try {
    const repoKey = repokey.repoKeyForWorktree(repo);
    seed(home, repoKey, [{ id: 'ws-1', worktreePath: repo, sessionId: 's' }, { id: 'ws-10', worktreePath: path.join(repo, 'o'), sessionId: 's2' }]);
    assert.deepStrictEqual(cli.resolveArchiveId('ws-1', ctx(home, { cwd: repo })), { ok: true, id: 'ws-1' });
  } finally { rm(home); rm(repo); }
});
