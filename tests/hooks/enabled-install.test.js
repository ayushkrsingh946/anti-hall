'use strict';
// #216: every registry reader follows the ENABLED anti-hall@* install, not the anti-hall@anti-hall pin.
// Fixture: two registered keys, only anti-hall@anti-hall-engine-live enabled. Fake HOME only.
const { test } = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const PLUGIN = path.join(__dirname, '..', '..', 'plugins', 'anti-hall');
const ei = require(path.join(PLUGIN, 'hooks', 'lib', 'enabled-install.js'));
const cachePrune = require(path.join(PLUGIN, 'hooks', 'lib', 'cache-prune.js'));
const update = require(path.join(PLUGIN, 'skills', 'update', 'scripts', 'update.js'));
const doctorDevswarm = require(path.join(PLUGIN, 'companion', 'lib', 'doctor-devswarm.js'));

const LIVE = 'anti-hall@anti-hall-engine-live';

function fixture(versions) {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'antihall-enabled-install-'));
  const root = cachePrune.cacheRootFor(home);
  for (const v of versions) {
    fs.mkdirSync(path.join(root, v, 'hooks'), { recursive: true });
    fs.writeFileSync(path.join(root, v, 'hooks', 'x.js'), 'x'.repeat(100));
  }
  return home;
}

function register(home, plugins, enabled) {
  const dir = path.join(home, '.claude', 'plugins');
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, 'installed_plugins.json'), JSON.stringify({ version: 2, plugins }));
  if (enabled) fs.writeFileSync(path.join(home, '.claude', 'settings.json'), JSON.stringify({ enabledPlugins: enabled }));
}

test('selectKeys: enabled anti-hall@* wins; the pin only when none is enabled', () => {
  const reg = { 'anti-hall@anti-hall': [{}], [LIVE]: [{}], 'other@x': [{}] };
  assert.deepStrictEqual(ei.selectKeys(reg, { [LIVE]: true, 'anti-hall@anti-hall': false }), { keys: [LIVE], choice: 'enabled' });
  assert.deepStrictEqual(ei.selectKeys(reg, {}), { keys: ['anti-hall@anti-hall'], choice: 'fallback' });
  assert.deepStrictEqual(ei.selectKeys(reg, { 'other@x': true }), { keys: ['anti-hall@anti-hall'], choice: 'fallback' });
  assert.deepStrictEqual(ei.selectKeys(reg, undefined), { keys: ['anti-hall@anti-hall'], choice: 'fallback' });
});

test('cache-prune never prunes the enabled install\'s cache dir (PRIMARY RULE)', () => {
  const versions = ['0.100.0', '0.101.0', '0.102.0', '0.103.0', '0.104.0', '0.105.0'];
  const home = fixture(versions);
  try {
    const root = cachePrune.cacheRootFor(home);
    // The enabled install runs from an OLD cache dir (not among the newest 3); the pin points elsewhere.
    register(home, {
      'anti-hall@anti-hall': [{ scope: 'user', version: '0.105.0', installPath: path.join(root, '0.105.0') }],
      [LIVE]: [{ scope: 'user', version: '0.100.0', installPath: path.join(root, '0.100.0') }],
    }, { 'anti-hall@anti-hall': false, [LIVE]: true });
    const plan = cachePrune.planCachePrune({
      home, runningVersion: '0.105.0', runningRoot: os.tmpdir(),
      scanCwds: () => ['/'], scanArgv: () => [], scanTranscripts: () => new Set(),
    });
    const e = Object.fromEntries(plan.entries.map((x) => [x.name, x]));
    assert.strictEqual(e['0.100.0'].action, 'keep', 'the enabled install\'s dir must be kept');
    assert.ok(e['0.100.0'].reasons.includes('registered installPath'));
    assert.strictEqual(e['0.101.0'].action, 'remove');
  } finally { fs.rmSync(home, { recursive: true, force: true }); }
});

test('update.js reports the ENABLED install\'s registered version', () => {
  const home = fixture([]);
  try {
    register(home, {
      'anti-hall@anti-hall': [{ scope: 'user', version: '0.300.2', installPath: '/x/old' }],
      [LIVE]: [{ scope: 'user', version: '0.300.1', installPath: '/x/new' }],
    }, { 'anti-hall@anti-hall': false, [LIVE]: true });
    const p = path.join(home, '.claude', 'plugins', 'installed_plugins.json');
    assert.strictEqual(update.versionFromInstalledJson(p), '0.300.1');
    fs.writeFileSync(path.join(home, '.claude', 'settings.json'), JSON.stringify({ enabledPlugins: {} }));
    assert.strictEqual(update.versionFromInstalledJson(p), '0.300.2', 'none enabled -> the pin');
  } finally { fs.rmSync(home, { recursive: true, force: true }); }
});

test('doctor-devswarm resolves the scope of the ENABLED install', () => {
  const home = fixture([]);
  try {
    register(home, {
      'anti-hall@anti-hall': [{ scope: 'user', installPath: '/x/old' }],
      [LIVE]: [{ scope: 'project', installPath: '/x/new' }],
    }, { [LIVE]: true });
    assert.strictEqual(doctorDevswarm.resolveInstallScope(home, '/x/new', fs), 'project');
  } finally { fs.rmSync(home, { recursive: true, force: true }); }
});
