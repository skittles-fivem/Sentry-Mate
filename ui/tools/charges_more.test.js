// Charges: "Show more" after a full page load of the list, in a browser that has already visited a charge page.
// Browser test in Chromium on the dev server with made-up data.
// Run: node --test ui/tools/charges_more.test.js. Skipped when Playwright is not installed.
'use strict';

const { test, before, after } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawn } = require('child_process');

const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }
const SKIP = !playwright && 'Playwright is not installed';

function devServer(extra) {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', path.join(UI_DIR, 'packs', 'instrument'),
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

let browser = null, server = null;
before(async () => {
  if (!playwright) return;
  browser = await playwright.chromium.launch();
  server = await devServer(['--freeze-clock']);
});
after(async () => {
  await browser?.close();
  await server?.stop();
});

const ready = (page, id) => page.waitForFunction((x) => document.documentElement.dataset.ready === x, id, { timeout: 30000 });

test('Charges: Show more after a charge page and a full reload of the list adds rows', { skip: SKIP, timeout: 90000 }, async () => {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: 'en-US', timezoneId: 'America/Los_Angeles' });
  const page = await context.newPage();
  try {
    const first = await (await page.request.get(`${server.origin}/api/v1/cars/c2/charges?limit=2`)).json();
    await page.goto(`${server.origin}/cars/c2/charges/${first.items[1].id}`);
    await ready(page, 'charge');
    await page.goto(`${server.origin}/cars/c2/charges?preset=all`);
    await ready(page, 'charges');
    const rows = () => page.locator('.charges-page table tbody tr').count();
    const before = await rows();
    assert.ok(before > 1, `the list starts with ${before} rows`);
    const more = page.locator('.charges-page button:has-text("Show more")').first();
    await more.click();
    await page.waitForTimeout(1500);
    const after = await rows();
    assert.ok(after >= before, `Show more left ${after} rows of ${before}`);
    assert.ok(after > 1, `the list collapsed to ${after} row`);
  } finally { await context.close(); }
});
