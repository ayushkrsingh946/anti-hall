'use strict';
// Live 2026-10-16: two Node ingest launchd units were INSTALLED while the
// anti-hall engine already drained ingest — 19:33 for a scratch clone an agent
// ran `devswarm.js send` in (selfHeal), 19:36 for a registered project the
// engine had not discovered (selfHeal via the reconcile sweep's `inbox pull`).
// Every Node path that installs or heals an ingest unit must stand down while
// the engine's ownership marker (devswarm/ingest-owner.json) says engine, and
// an automatic install never qualifies a project no workspace descriptor names.
// Each path below fails on the pre-fix code.
require('../helpers/isolate-home.js'); // HOME -> empty temp dir: this file reads home-dir state
const test = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const os = require('os');
const path = require('path');
const cp = require('child_process');

const ROOT = path.join(__dirname, '..', '..');
const PLUGIN = path.join(ROOT, 'plugins', 'anti-hall');
const health = require(path.join(PLUGIN, 'companion', 'lib', 'ingest-health.js'));
const cli = require(path.join(PLUGIN, 'scripts', 'devswarm.js'));
const repair = require(path.join(PLUGIN, 'hooks', 'lib', 'doctor-repair.js'));
const update = require(path.join(PLUGIN, 'skills', 'update', 'scripts', 'update.js'));

function tmp(tag) { return fs.mkdtempSync(path.join(os.tmpdir(), 'ah-ingest-owned-' + tag + '-')); }
function rm(d) { fs.rmSync(d, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); }
function makeGitRepo(tag) {
  const dir = tmp('repo-' + tag);
  cp.spawnSync('git', ['init', '-q', dir]);
  cp.spawnSync('git', ['-C', dir, '-c', 'user.email=a@b.c', '-c', 'user.name=T', 'commit', '-q', '--allow-empty', '-m', 'init']);
  return fs.realpathSync(dir);
}
function writeMarker(home, owner, ts, validForMs) {
  const p = health.ingestOwnerPath(home);
  fs.mkdirSync(path.dirname(p), { recursive: true });
  fs.writeFileSync(p, JSON.stringify({ owner, pid: 1, ts, validForMs }));
}
function registerProject(home, repoKey) {
  const dir = path.join(home, '.anti-hall', 'devswarm', 'workspaces');
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, 'primary-x.json'), JSON.stringify({ id: 'primary-x', repoKey, worktreePath: '/x', sessionId: 's' }));
}
const ACTIVE_ENV = { DEVSWARM_REPO_ID: 'repo-1' };
function healCtx(home, repo, key, onSpawn, now) {
  return {
    home, env: ACTIVE_ENV, cwd: repo, now: now || Date.now(),
    io: { platform: 'linux', resolveWorktree: () => repo, repoKeyForWorktree: () => key, spawnInstaller: onSpawn },
  };
}

test('engineOwnsIngest: engine + fresh -> true; witness, expired, missing or torn -> false', () => {
  const home = tmp('marker');
  try {
    const now = Date.now();
    assert.strictEqual(health.engineOwnsIngest(home, { now }), false, 'no marker');
    writeMarker(home, 'engine', now - 1000, 60000);
    assert.strictEqual(health.engineOwnsIngest(home, { now }), true);
    writeMarker(home, 'engine', now - 120000, 60000);
    assert.strictEqual(health.engineOwnsIngest(home, { now }), false, 'expired: an engine gone that long hands installs back');
    writeMarker(home, 'witness', now, 60000);
    assert.strictEqual(health.engineOwnsIngest(home, { now }), false);
    fs.writeFileSync(health.ingestOwnerPath(home), '{torn');
    assert.strictEqual(health.engineOwnsIngest(home, { now }), false);
  } finally { rm(home); }
});

test('selfHeal (19:36 path): engine owns ingest -> no installer spawn even for a registered stale project', () => {
  const home = tmp('selfheal-owned');
  const repo = makeGitRepo('owned');
  try {
    registerProject(home, 'proj-owned');
    writeMarker(home, 'engine', Date.now(), 3600000);
    let spawned = 0;
    const r = cli.selfHeal(healCtx(home, repo, 'proj-owned', () => { spawned++; }));
    assert.strictEqual(spawned, 0, 'a Node unit would be a second consumer in front of the engine');
    assert.strictEqual(r.daemonEngineOwned, true);
    assert.strictEqual(r.daemonHealAttempted, undefined);
  } finally { rm(home); rm(repo); }
});

test('selfHeal (19:33 path): a scratch clone no workspace descriptor names never gets an installer spawn', () => {
  const home = tmp('selfheal-scratch');
  const repo = makeGitRepo('scratch');
  try {
    registerProject(home, 'some-other-project');
    let spawned = 0;
    const r = cli.selfHeal(healCtx(home, repo, 'wt-scratch-c66e90', () => { spawned++; }));
    assert.strictEqual(spawned, 0);
    assert.strictEqual(r.daemonUnregistered, true);
  } finally { rm(home); rm(repo); }
});

test('selfHeal: a registered project with no (or an expired) engine marker still heals as before', () => {
  const home = tmp('selfheal-control');
  const repo = makeGitRepo('control');
  try {
    registerProject(home, 'proj-reg');
    let spawned = 0;
    const now = Date.now();
    assert.strictEqual(cli.selfHeal(healCtx(home, repo, 'proj-reg', () => { spawned++; }, now)).daemonHealAttempted, true);
    writeMarker(home, 'engine', now - 7200000, 3600000);
    const later = now + cli.SELF_HEAL_COOLDOWN_MS + 1000;
    assert.strictEqual(cli.selfHeal(healCtx(home, repo, 'proj-reg', () => { spawned++; }, later)).daemonHealAttempted, true);
    assert.strictEqual(spawned, 2);
  } finally { rm(home); rm(repo); }
});

test('installer main (every caller\'s choke point): engine owns ingest -> installs nothing, exit 0', () => {
  if (process.platform !== 'darwin' && process.platform !== 'linux') return;
  const home = tmp('installer');
  const repo = makeGitRepo('installer');
  try {
    writeMarker(home, 'engine', Date.now(), 3600000);
    const r = cp.spawnSync(process.execPath, [path.join(PLUGIN, 'companion', 'install-devswarm-ingest.js')], {
      cwd: repo, encoding: 'utf8', timeout: 30000,
      env: Object.assign({}, process.env, { HOME: home, USERPROFILE: home, ANTIHALL_INGEST_DRY_RUN: '1' }),
    });
    assert.strictEqual(r.status, 0, r.stderr);
    assert.match(r.stdout, /engine owns the ingest drain/);
    assert.doesNotMatch(r.stdout, /would (write|run)|LaunchAgent|systemd/i, r.stdout);
  } finally { rm(home); rm(repo); }
});

test('doctor --repair: engine owns ingest -> the ingest step installs nothing (an absent unit is what a retired one looks like)', () => {
  const home = tmp('doctor-owned');
  const repo = makeGitRepo('doctor-owned');
  try {
    writeMarker(home, 'engine', Date.now(), 3600000);
    const results = repair.runRepairs({ cwd: repo, env: { DEVSWARM_REPO_ID: 'r1' }, home, dryRun: true, platform: 'linux' });
    const ingest = results.filter((x) => x.id === 'ingest');
    assert.strictEqual(ingest.length, 1, JSON.stringify(ingest));
    assert.strictEqual(ingest[0].status, 'skipped');
    assert.match(ingest[0].msg, /engine owns the ingest drain/);
  } finally { rm(home); rm(repo); }
});

test('doctor --repair: an absent unit for an unregistered project (scratch clone) is not installed', () => {
  const home = tmp('doctor-scratch');
  const repo = makeGitRepo('doctor-scratch');
  try {
    const results = repair.runRepairs({ cwd: repo, env: { DEVSWARM_REPO_ID: 'r1' }, home, dryRun: true, platform: 'linux' });
    const ingest = results.filter((x) => x.id === 'ingest');
    assert.strictEqual(ingest.length, 1, JSON.stringify(ingest));
    assert.strictEqual(ingest[0].status, 'skipped');
    assert.match(ingest[0].msg, /not a registered DevSwarm project/);
    assert.doesNotMatch(ingest[0].msg, /would \(re\)install/);
  } finally { rm(home); rm(repo); }
});

test('update heal: engine owns ingest -> gated, the installer is never spawned', () => {
  const home = tmp('update-owned');
  const repo = makeGitRepo('update-owned');
  try {
    writeMarker(home, 'engine', Date.now(), 3600000);
    let spawned = 0;
    const r = update.healIngestDaemon({ paths: { pluginSrcDir: PLUGIN }, env: ACTIVE_ENV, cwd: repo, home, spawnFn: () => { spawned++; return { status: 0 }; } });
    assert.strictEqual(spawned, 0);
    assert.strictEqual(r.attempted, false);
    assert.match(r.detail, /engine owns the ingest drain/);
  } finally { rm(home); rm(repo); }
});
