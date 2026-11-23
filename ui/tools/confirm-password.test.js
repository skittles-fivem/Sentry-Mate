// The password a page asks again before a change goes in the request's JSON body (confirm_password, code), never in
// the X-Sentry-Mate-Password header: fetch() refuses a header value with a character above U+00FF and writes
// U+0080 to U+00FF as single bytes, so an account whose password is not plain ASCII could not make the change.
// A page may use the header only behind a check that the password is printable ASCII (Console's withCred).
// Run: node --test ui/tools/confirm-password.test.js
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');

const PACKS_DIR = path.join(path.dirname(__dirname), 'packs');

function scripts(dir, out = []) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) { if (e.name !== 'shared') scripts(p, out); } else if (/\.m?js$/.test(e.name)) out.push(p);
  }
  return out;
}

test('no page puts a typed password in a request header it cannot hold', () => {
  const files = fs.readdirSync(PACKS_DIR).flatMap((pack) => scripts(path.join(PACKS_DIR, pack, 'web')));
  assert.ok(files.length > 100, `the packs' scripts were found: ${files.length}`);
  const bad = [];
  for (const f of files) {
    const lines = fs.readFileSync(f, 'utf8').split('\n');
    lines.forEach((line, i) => {
      if (/^\s*(\/\/|\*)/.test(line)) return;
      if (/['"]X-Sentry-Mate-(Password|Code)['"]\s*:/.test(line) && !/ASCII\.test\(/.test(`${lines[i - 1] || ''}\n${line}`)) {
        bad.push(`${path.relative(PACKS_DIR, f)}:${i + 1}`);
      }
    });
  }
  assert.deepEqual(bad, [], 'send confirm_password (and code) in the JSON body instead');
});
