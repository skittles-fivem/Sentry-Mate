// The logbook of every built-in pack (Instrument; the others are archived) in Chromium on the dev API server while
// a rebuild runs (a `rows` event every second: the logbook reads itself again): while the long list is scrolled
// without a rest, the page is not measured and set on whole pixels again frame after frame. The list's table
// changes height as its window moves over rows of other heights; that waits for the scroll to rest, as its rows
// do. (A run over the whole page takes most of a frame on a 4K screen: ui/tools/scroll.test.js measures the
// frames; this says why.) The page's own panels are made again only when their figures changed.
// Run: node --test ui/tools/logbook-scroll-snap.test.js. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawn } = require('child_process');

const { BUILTIN_PACKS } = require('./builtin-packs');
const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }

function devServer(pack) {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', path.join(UI_DIR, 'packs', pack),
      '--port', '0', '--rebuild-running', '--rows-every-s', '1'], { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
    let out = '', err = '';
    const timer = setTimeout(() => { child.kill('SIGKILL'); reject(new Error(`no dev server in 60 s: ${err}`)); }, 60000);
    child.stderr.on('data', (d) => { err = (err + d).slice(-5000); });
    child.stdout.on('data', (d) => {
      out += d;
      const m = /listening on (http:\/\/\S+?)\/?\s/.exec(out);
      if (m) {
        clearTimeout(timer);
        resolve({ origin: m[1], stop: () => new Promise((r) => { child.once('exit', r); child.kill('SIGTERM'); }) });
      }
    });
    child.once('exit', (code) => { clearTimeout(timer); reject(new Error(`the dev server stopped (${code}): ${err}`)); });
  });
}

for (const pack of BUILTIN_PACKS) {
  test(`${pack}: the logbook is not set on whole pixels again every frame while its list scrolls`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const browser = await playwright.chromium.launch();
    const server = await devServer(pack);
    try {
      const context = await browser.newContext({ viewport: { width: 1920, height: 1080 } });
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', (e) => errors.push(e.message));
      let reads = 0;
      // every answer the logbook reads, and how often the figures of the panels above the list changed in them
      let figures = null, figureChanges = 0;
      page.on('response', async (res) => {
        if (!new URL(res.url()).pathname.endsWith('/cars/c1/logbook') || !res.ok()) return;
        reads++;
        const d = await res.json().catch(() => null);
        if (!d) return;
        const key = JSON.stringify([d.count, d.totals_by_purpose, d.counts_by_purpose, d.continuity, d.rules_used]);
        if (figures !== null && key !== figures) figureChanges++;
        figures = key;
      });
      await page.goto(`${server.origin}/cars/c1/logbook?preset=all`);
      await page.waitForFunction(() => document.documentElement.dataset.ready === 'logbook', null, { timeout: 30000 });
      await page.waitForSelector('table.vl-table tbody tr:not(.vl-gap)', { timeout: 30000 });
      // the logbook has read itself again at least once (a `rows` event), as it does all through a rebuild
      const first = reads;
      for (let i = 0; i < 100 && reads === first; i++) await page.waitForTimeout(100);
      assert.ok(reads > first, 'the logbook read itself again on a rows event');
      await page.waitForTimeout(500);

      // what a run of the snap writes: inline sizes on the list's table and its heading cells, and the panels
      // above the list made again
      const changesBefore = figureChanges; // (read first: a change that is drawn as the watch starts counts)
      await page.evaluate(() => {
        window.__snaps = 0; window.__panels = 0;
        const table = document.querySelector('table.vl-table');
        const seen = () => { window.__snaps++; };
        const o = new MutationObserver(seen);
        o.observe(table, { attributes: true, attributeFilter: ['style'] });
        o.observe(table.tHead, { attributes: true, attributeFilter: ['style'], subtree: true });
        new MutationObserver(() => { window.__panels++; }).observe(document.querySelector('.lb-summary'), { childList: true, subtree: true });
      });
      await page.mouse.move(960, 700);
      const t0 = Date.now();
      let steps = 0;
      for (; Date.now() - t0 < 6000; steps++) {
        await page.mouse.wheel(0, Math.floor(steps / 40) % 2 ? -500 : 500);
        await page.waitForTimeout(40);
      }
      const got = await page.evaluate(() => ({ snaps: window.__snaps, panels: window.__panels, y: scrollY }));
      assert.ok(steps > 40, 'the list was scrolled');
      // a rest of 150 ms is taken for the scroll's end: a slow machine may leave such a hole a few times in 6 s
      assert.ok(got.snaps <= 6, `the page was set on whole pixels ${got.snaps} times in ${steps} steps of one scroll`);
      await page.waitForTimeout(300); // an answer on its way is read
      const changes = figureChanges - changesBefore;
      assert.ok(got.panels <= changes, `the panels above the list were made again ${got.panels} times; their figures changed ${changes} times`);

      // at rest it is set once more (the list's rows on whole pixels)
      await page.waitForTimeout(1000);
      assert.deepEqual(errors, []);
      await context.close();
    } finally {
      await server.stop();
      await browser.close();
    }
  });
}
