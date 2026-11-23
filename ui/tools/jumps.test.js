// No page jumps: every page of every pack on the dev server, loaded and scrolled through on an iPhone 15 and an
// iPhone 13 mini (WebKit), a phone in Chromium's mobile emulation and a 1440x900 desktop (Chromium), with scroll
// anchoring off as in Safari on iOS. A page fails with a jump score or layout shift over 0.02, a block above the
// screen that changes its height while the page is scrolled, or a scroll the page makes by itself.
// ui/tools/jumps.js does the work; see ui/tools/README.md ("The jump check").
// Run: node --test ui/tools/jumps.test.js. Skipped when Playwright is not installed; the WebKit devices are skipped
// (with a note) when WebKit is not. JUMPS_PACKS, JUMPS_PAGES and JUMPS_DEVICES narrow it (comma separated).
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }
const { run, parseArgs, describe } = require('./jumps');

test('no page jumps while it loads or scrolls, on phones and desktop', { skip: !playwright && 'Playwright is not installed', timeout: 120 * 60000 }, async () => {
  const argv = ['--jobs', process.env.JUMPS_JOBS || '3'];
  if (process.env.JUMPS_PACKS) argv.push('--packs', process.env.JUMPS_PACKS);
  if (process.env.JUMPS_PAGES) argv.push('--pages', process.env.JUMPS_PAGES);
  if (process.env.JUMPS_DEVICES) argv.push('--devices', process.env.JUMPS_DEVICES);
  const rep = await run(parseArgs(argv));
  for (const n of rep.notes) process.stderr.write(`note: ${n}\n`);
  assert.ok(rep.results.length > 0, 'no page was visited');
  const bad = rep.results.filter((r) => !r.ok).map((r) => describe(r).join('\n'));
  assert.deepEqual(bad, []);
});
