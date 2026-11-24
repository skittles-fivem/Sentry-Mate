// `node ui/tools/check.js --pack <pack> --jumps` hands the pack to the jump check (ui/tools/jumps.js). The two files
// need each other; run as the program, check.js must have its exports in place before it asks for jumps.js, or
// jumps.js gets none of them (no ROUTES, no startServer) and its parseArgs throws a TypeError.
// Run: node --test ui/tools/check-jumps-flag.test.js. Needs no browser: it asks for a page that is none, which the
// jump check refuses once it has read its options.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawnSync } = require('child_process');

const { BUILTIN_PACKS } = require('./builtin-packs');
const CHECK = path.join(__dirname, 'check.js');

for (const pack of BUILTIN_PACKS) {
  test(`${pack}: check.js --jumps reaches the jump check's own options`, { timeout: 60000 }, () => {
    const r = spawnSync(process.execPath, [CHECK, '--pack', pack, '--jumps', '--pages', 'no-such-page'], { encoding: 'utf8', timeout: 50000 });
    assert.doesNotMatch(r.stderr, /TypeError|circular dependency/, r.stderr);
    assert.match(r.stderr, /no page no-such-page/);
    assert.equal(r.status, 2, 'a page that is none is a usage error');
  });
}

test('jumps.js required first, and check.js required first, both see the other\'s exports', { timeout: 60000 }, () => {
  for (const first of ['./jumps', './check']) {
    const code = `const a = require(${JSON.stringify(first)}); const J = require('./jumps'); const C = require('./check');`
      + 'const o = J.parseArgs([\'--packs\', \'instrument\', \'--pages\', \'car\']);'
      + 'if (o.pages.join() !== \'car\' || !C.ROUTES.car || typeof C.startServer !== \'function\') throw new Error(\'exports missing\');';
    const r = spawnSync(process.execPath, ['-e', code], { cwd: __dirname, encoding: 'utf8', timeout: 50000 });
    assert.equal(r.status, 0, `${first} first: ${r.stderr}`);
    assert.doesNotMatch(r.stderr, /circular dependency/, r.stderr);
  }
});
