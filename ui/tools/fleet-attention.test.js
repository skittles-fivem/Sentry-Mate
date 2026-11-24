// "Need a look" on the Fleet page of every built-in pack, in Chromium on the dev API server: a car is called silent
// (upload_silence) on the age of its newest log's data (since_ms), not on when its last upload came
// (detail.last_upload_ms). The row says how old the data is; an upload that came since, with older logs, is said
// apart, and never as "No upload for 4 min".
// Run: node --test ui/tools/fleet-attention.test.js. Skipped when Playwright is not installed.
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
      '--port', '0', '--freeze-clock'], { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
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

const H = 3600000;

for (const pack of BUILTIN_PACKS) {
  test(`${pack}: a silent car's row says how old its newest data is, and a later upload apart`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const server = await devServer(pack);
    const browser = await playwright.chromium.launch();
    try {
      const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', (e) => errors.push(e.message));
      // the server's clock, near enough: the newest reading of any car
      await page.goto(`${server.origin}/`);
      await page.waitForFunction(() => document.documentElement.dataset.ready === 'fleet', null, { timeout: 30000 });
      const now = await page.evaluate(async () => Math.max(...(await (await fetch('/api/v1/cars?limit=100')).json()).items.map((c) => c.now?.at_ms || 0)));
      assert.ok(now > 0);
      const items = [
        // a backlog: the bridge uploaded 4 minutes ago, its newest data is 38 hours old
        { car: 'c1', code: 'upload_silence', since_ms: now - 38 * H, detail: { last_upload_ms: now - 4 * 60000 } },
        // nothing came since the newest data
        { car: 'c2', code: 'upload_silence', since_ms: now - 50 * H, detail: { last_upload_ms: now - 50 * H + 60000 } },
        // never an upload
        { car: 'c3', code: 'upload_silence', since_ms: null, detail: { last_upload_ms: null } },
      ];
      await page.route('**/api/v1/fleet/attention*', (r) => r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ items, next: null }) }));
      await page.reload();
      await page.waitForFunction(() => document.documentElement.dataset.ready === 'fleet' && document.querySelectorAll('.attn-row .attn-text').length >= 3, null, { timeout: 30000 });
      const texts = await page.evaluate(() => [...document.querySelectorAll('.attn-row .attn-text')].map((e) => e.textContent.trim()).slice(0, 3));   // (set-up rows follow the cars')
      assert.match(texts[0], /^No new data for (38|39)\sh; the last upload came \d+\smin ago, with older logs\.$/);
      assert.match(texts[1], /^No upload for 2\sdays\. The CAN Bridge may be off or out of range\.$/);
      assert.equal(texts[2], 'No upload for a long time. The CAN Bridge may be off or out of range.');
      for (const t of texts) assert.doesNotMatch(t, /No upload for \d+\smin/);
      assert.deepEqual(errors, []);
    } finally {
      await browser.close();
      await server.stop();
    }
  });
}
