'use strict';
// Fails when plugins/anti-hall outgrows the plugin directory's file-count or per-file-size limits.
// The limits live in tests/plugin-limits.toml.
const test = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const path = require('node:path');

const ROOT = path.join(__dirname, '..', 'plugins', 'anti-hall');
const IMAGE_FONT = /\.(png|jpe?g|gif|webp|woff2?|ttf|otf)$/i;

function limit(name) {
  const t = fs.readFileSync(path.join(__dirname, 'plugin-limits.toml'), 'utf8');
  const m = new RegExp('^' + name + '\\s*=\\s*(\\d+)', 'm').exec(t);
  assert.ok(m, `${name} missing from tests/plugin-limits.toml`);
  return Number(m[1]);
}

function walk(dir, out = []) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, out);
    else if (e.isFile()) out.push(p);
  }
  return out;
}

const files = walk(ROOT);

test('plugin folder stays within the file-count limit', () => {
  const max = limit('max_files');
  assert.ok(files.length <= max, `${files.length} files under plugins/anti-hall, limit ${max}`);
});

test('every non-image, non-font file stays within the size limit', () => {
  const maxBytes = limit('max_file_kib') * 1024;
  const big = files
    .filter((f) => !IMAGE_FONT.test(f))
    .map((f) => [path.relative(ROOT, f), fs.statSync(f).size])
    .filter(([, s]) => s > maxBytes);
  assert.deepStrictEqual(big, [], `files over ${maxBytes} bytes`);
});
