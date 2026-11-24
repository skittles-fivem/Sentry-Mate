// Live figures in Chromium on the dev API server: with a full rebuild under way (--full-rebuild running), Data
// health's progress panel moves on by itself in every pack, from the live stream's `job` events: the logs decoded and
// the data go up, the days are filled in, the bar's percent follows the logs, and the page is never loaded again, no
// Reload is pressed and the status endpoint is not polled while the stream is open.
// Run: node --test ui/tools/live-figures.test.js. Skipped when Playwright is not installed.
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

const PACKS = BUILTIN_PACKS;

function devServer(pack, extra) {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', path.join(UI_DIR, 'packs', pack),
      '--port', '0', ...extra], { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
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

// The panel's figures as the page shows them (whatever the pack's look): "Logs decoded 1,234 of 20,494" and so on.
function figures() {
  const text = document.body.innerText.replace(/\s+/g, ' ');
  const num = (s) => (s == null ? null : Number(s.replace(/,/g, '')));
  const logs = /Logs decoded:? ([\d,]+) of ([\d,]+)/i.exec(text);
  const days = /Days:? ([\d,]+) of ([\d,]+)/i.exec(text);
  const data = /Data:? ([\d.,]+ [kMGT]?B) of ([\d.,]+ [kMGT]?B)/i.exec(text);
  const bar = document.querySelector('[role="progressbar"][aria-label*="ull rebuild" i], [role="progressbar"][aria-valuenow]');
  return {
    logsDone: num(logs?.[1]), logsTotal: num(logs?.[2]),
    daysTotal: num(days?.[2]), data: data ? `${data[1]} of ${data[2]}` : null,
    pct: bar ? Number(bar.getAttribute('aria-valuenow')) : null,
    ready: document.documentElement.dataset.ready || null,
    marker: window.__liveMarker === 'kept',
  };
}

for (const pack of PACKS) {
  test(`${pack}: Data health's full rebuild figures move without a reload`, { skip: !playwright && 'Playwright is not installed', timeout: 120000 }, async () => {
    const browser = await playwright.chromium.launch();
    const server = await devServer(pack, ['--full-rebuild', 'running']);
    try {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', (e) => errors.push(e.message));
      let statusReads = 0;
      let documents = 0;
      page.on('request', (r) => {
        const u = new URL(r.url());
        if (/\/full-rebuild$/.test(u.pathname) && r.method() === 'GET') statusReads++;
        if (r.resourceType() === 'document') documents++;
      });

      await page.goto(`${server.origin}/cars/c1/health`);
      await page.waitForFunction(() => !!document.documentElement.dataset.ready, null, { timeout: 30000 });
      await page.waitForFunction(() => /Logs decoded:?\s*[\d,]+ of/i.test(document.body.innerText), null, { timeout: 20000 });
      await page.evaluate(() => { window.__liveMarker = 'kept'; });
      const first = await page.evaluate(figures);
      const readsAtStart = statusReads;
      assert.ok(first.logsTotal > 0, `${pack}: ${JSON.stringify(first)}`);
      assert.ok(first.daysTotal > 0, `${pack}: the days are filled in while decoding: ${JSON.stringify(first)}`);
      assert.ok(first.data, `${pack}: the data line: ${JSON.stringify(first)}`);

      // the next figures come in by themselves
      await page.waitForFunction((n) => {
        const m = /Logs decoded:?\s*([\d,]+) of/i.exec(document.body.innerText);
        return m && Number(m[1].replace(/,/g, '')) > n;
      }, first.logsDone, { timeout: 30000 });
      const later = await page.evaluate(figures);
      assert.ok(later.logsDone > first.logsDone, `${pack}: ${JSON.stringify([first, later])}`);
      assert.ok(later.marker, `${pack}: the page was not loaded again`);
      assert.equal(later.ready, first.ready, `${pack}: the same page`);
      assert.equal(documents, 1, `${pack}: one document load`);
      if (later.pct != null && later.logsTotal) {
        const expect = Math.floor((100 * later.logsDone) / later.logsTotal);
        assert.ok(Math.abs(later.pct - expect) <= 1, `${pack}: the bar (${later.pct} %) matches the logs (${expect} %)`);
      }
      assert.ok(statusReads - readsAtStart <= 1, `${pack}: no polling while the stream is open (${statusReads - readsAtStart} reads)`);
      assert.deepEqual(errors, []);
      await context.close();
    } finally {
      await server.stop();
      await browser.close();
    }
  });
}
