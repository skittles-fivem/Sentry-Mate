// The car page's map of every built-in pack (Instrument; the others are archived) in Chromium on the dev API server,
// with a car that is driving (live values every 500 ms): the car's marker moves with the car while the page rests,
// does not move (the map is not drawn again) while the page is being scrolled, and catches up once the scroll has
// rested with the map in view. A map drawn again takes longer than a frame on a 4K screen and held the scroll up
// (ui/tools/scroll.test.js measures that; this says why).
// Run: node --test ui/tools/car-map-scroll.test.js. Skipped when Playwright is not installed.
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
      '--port', '0', '--sse-interval-ms', '500'], { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
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
  test(`${pack}: the car's marker waits while the page is scrolled and catches up after`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const browser = await playwright.chromium.launch();
    const server = await devServer(pack);
    try {
      const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', (e) => errors.push(e.message));
      await page.goto(`${server.origin}/cars/c1`);
      await page.waitForFunction(() => document.documentElement.dataset.ready === 'car', null, { timeout: 30000 });
      await page.waitForSelector('.car-marker', { timeout: 30000 });
      // every marker put on the map from now on (a move takes the old one off and puts a new one on), with its time
      await page.evaluate(() => {
        window.__moves = [];
        new MutationObserver((ms) => {
          for (const m of ms) for (const n of m.addedNodes) if (n.nodeType === 1 && n.matches('.car-marker')) window.__moves.push(performance.now());
        }).observe(document.querySelector('.car-marker').closest('.map, .maplibregl-map'), { childList: true, subtree: true });
      });
      const moves = () => page.evaluate(() => window.__moves.length);

      // 1. at rest the marker follows the car
      await page.waitForFunction(() => window.__moves.length >= 1, null, { timeout: 20000 });

      // 2. scrolled without a rest for 7 s (a wheel step every 50 ms, down and back up): no move
      await page.mouse.move(640, 400);
      const before = await moves();
      const t0 = Date.now();
      for (let i = 0; Date.now() - t0 < 7000; i++) {
        await page.mouse.wheel(0, Math.floor(i / 10) % 2 ? -60 : 60);
        await page.waitForTimeout(50);
      }
      assert.equal(await moves() - before, 0, 'the marker was moved (the map drawn again) while the page was being scrolled');

      // 3. the scroll rests with the map in view: the marker catches up, and goes on following the car
      await page.evaluate(() => document.querySelector('.car-marker').scrollIntoView({ block: 'center' }));
      await page.waitForFunction((n) => window.__moves.length > n, before, { timeout: 20000 });
      const caught = await moves();
      await page.waitForFunction((n) => window.__moves.length > n, caught, { timeout: 20000 });

      // 4. out of view the map is not drawn again; back in view it catches up
      await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
      const hidden = await page.evaluate(() => { const r = document.querySelector('.car-marker').getBoundingClientRect(); return r.bottom < 0 || r.top > innerHeight; });
      if (hidden) {
        await page.waitForTimeout(1000); // the scroll has rested
        const away = await moves();
        await page.waitForTimeout(6000);
        assert.equal(await moves() - away, 0, 'the marker was moved while the map was out of view');
        await page.evaluate(() => document.querySelector('.car-marker').scrollIntoView({ block: 'center' }));
        await page.waitForFunction((n) => window.__moves.length > n, away, { timeout: 20000 });
      }
      assert.deepEqual(errors, []);
      await context.close();
    } finally {
      await server.stop();
      await browser.close();
    }
  });
}
