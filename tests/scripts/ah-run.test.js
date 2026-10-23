'use strict';
// scripts/ah-run.sh: the skills' launcher runs the engine verb and passes every engine exit through; it runs no Node.
// With no engine it prints the missing-engine message and exits 127.
require('../helpers/isolate-home.js');
const { test } = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const path = require('node:path');
const cp = require('node:child_process');

const RUN = path.join(__dirname, '..', '..', 'plugins', 'anti-hall', 'scripts', 'ah-run.sh');
const HOME = fs.realpathSync(fs.mkdtempSync(path.join(process.env.HOME, 'ah-run-')));
process.on('exit', () => { try { fs.rmSync(HOME, { recursive: true, force: true }); } catch (_) { /* best effort */ } });

function fakeEngine(exitCode) {
  const bin = path.join(HOME, '.anti-hall', 'ah-engine', 'bin');
  fs.mkdirSync(bin, { recursive: true });
  fs.writeFileSync(path.join(bin, 'ah-engine'), `#!/bin/sh\necho "engine $*"\nexit ${exitCode}\n`, { mode: 0o755 });
}
function run(args) {
  return cp.spawnSync('sh', [RUN, ...args], { encoding: 'utf8', env: { PATH: '/usr/bin:/bin:' + path.dirname(process.execPath), HOME } });
}

test('engine absent: one line on stderr and exit 127, nothing else runs', () => {
  const r = run(['harvest', '--dir', HOME]);
  assert.strictEqual(r.status, 127, r.stderr);
  assert.strictEqual(r.stdout, '');
  assert.ok(r.stderr.trim().length > 0, 'the missing-engine message is printed');
});
test('engine answers: its output and exit code pass through', () => {
  fakeEngine(3);
  const r = run(['defect', 'list']);
  assert.strictEqual(r.status, 3);
  assert.match(r.stdout, /^engine defect list/);
});
test('engine exit 75 and 70 are the engine\'s own answer (no Node fallback)', () => {
  for (const code of [75, 70]) {
    fakeEngine(code);
    const r = run(['harvest', '--dir', HOME]);
    assert.strictEqual(r.status, code, r.stderr);
    assert.match(r.stdout, /engine harvest/);
  }
});
test('an unknown verb is refused', () => {
  assert.strictEqual(run(['nope']).status, 64);
});
test('jev-report is a known verb: the engine answers with its own exit code', () => {
  fakeEngine(0);
  const answered = run(['jev-report', '--weekly']);
  assert.strictEqual(answered.status, 0);
  assert.match(answered.stdout, /^engine jev-report --weekly/);
  fakeEngine(75);
  assert.strictEqual(run(['jev-report', '--weekly']).status, 75);
});
