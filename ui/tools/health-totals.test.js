// Data health of every built-in pack (Instrument; the others are archived) in Chromium on the dev API server, for a
// car with more chain gaps, dropped frames and unknown spans than its health lists (`--health-list-max 3`; the
// real server lists the newest 500): the page's counts are the answer's `*_total`, not the length of the lists.
// Run: node --test ui/tools/health-totals.test.js. Skipped when Playwright is not installed.
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
      '--port', '0', '--freeze-clock', '--health-list-max', '3'], { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
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
  test(`${pack}: Data health counts every dropped frame and unknown span, not only the ones listed`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const browser = await playwright.chromium.launch();
    const server = await devServer(pack);
    try {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', (e) => errors.push(e.message));
      // the health answers the page reads; the newest one is what it shows
      const answers = [];
      page.on('response', async (res) => {
        const u = new URL(res.url());
        if (u.pathname === '/api/v1/cars/c1/health' && res.ok()) answers.push(await res.json().catch(() => null));
      });
      await page.goto(`${server.origin}/cars/c1/health?preset=all`);
      await page.waitForFunction(() => document.documentElement.dataset.ready === 'health', null, { timeout: 30000 });
      await page.waitForLoadState('networkidle', { timeout: 20000 });
      const hl = answers.filter(Boolean).pop();
      assert.ok(hl, 'the page read the car\'s health');
      assert.equal(hl.dropped_frames.length, 3);
      assert.ok(hl.dropped_frames_total > 3 && hl.unknown_spans_total > 3, 'more than the lists hold');

      const figure = (label) => page.evaluate((l) => {
        const m = [...document.querySelectorAll('.metric')].find((x) => x.querySelector('.metric-label')?.textContent.trim() === l);
        return m ? { value: m.querySelector('.metric-value').textContent.trim(), explain: m.querySelector('.explain')?.textContent || '' } : null;
      }, label);
      assert.equal((await figure('Dropped frames')).value, String(hl.dropped_frames_total));
      assert.equal((await figure('Chain gaps')).value, String(hl.chain_gaps_total));
      assert.match((await figure('Unknown time')).explain, new RegExp(`\\b${hl.unknown_spans_total} spans\\b`));
      assert.deepEqual(errors, []);
      await context.close();
    } finally {
      await server.stop();
      await browser.close();
    }
  });
}
