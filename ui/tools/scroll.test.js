// Scrolling keeps drawing: the key pages of every pack in Chromium on the dev server with a rebuild running
// (--rebuild-running: Data health reads itself again, a `rows` event every second, live values every 500 ms),
// scrolled down and back up with the mouse wheel on a 4K screen at 200 % (1920x1080 at device scale 2). No frame
// may take over 50 ms. ui/tools/scroll.js does the work; see ui/tools/README.md.
// Run: node --test ui/tools/scroll.test.js. Skipped when Playwright is not installed.
// SCROLL_PACKS=instrument,horizon and SCROLL_SIZES=3840x2160@1,1920x1080@2 narrow or widen it.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }
const { run, parseArgs } = require('./scroll');

test('the key pages keep drawing while they scroll and a rebuild runs', { skip: !playwright && 'Playwright is not installed', timeout: 30 * 60000 }, async () => {
  const argv = ['--steps', '20', '--sizes', process.env.SCROLL_SIZES || '1920x1080@2'];
  if (process.env.SCROLL_PACKS) argv.push('--packs', process.env.SCROLL_PACKS);
  const rep = await run(parseArgs(argv));
  const bad = rep.results.filter((r) => !r.ok)
    .map((r) => `${r.pack} ${r.page} ${r.size}: ${r.error || `a frame of ${r.maxFrameMs} ms, ${r.longTasks} long tasks (longest ${r.maxLongTaskMs} ms)`}`);
  assert.ok(rep.results.length > 0, 'no page was scrolled');
  assert.deepEqual(bad, []);
});
