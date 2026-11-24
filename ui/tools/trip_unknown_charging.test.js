// Trip: a charge of unknown kind (the server's time_ms.charging_unknown and energy_kwh.added_unknown) is a part of the
// time split and of the energy figures, so the time bar adds up to the period. Browser test in Chromium on the dev
// server with made-up data; the dev server's Trip answer is changed in the browser to carry such a charge.
// Run: node --test ui/tools/trip_unknown_charging.test.js. Skipped when Playwright is not installed.
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

const HOUR = 3600000;

test('Trip: charging of unknown kind is in the time split and the energy figures', { skip: SKIP, timeout: 90000 }, async () => {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: 'en-US', timezoneId: 'America/Los_Angeles' });
  const page = await context.newPage();
  try {
    // the answer as the real server gives it while a charge of no known kind is in the period: 2 h out of the sleep
    await page.route(/\/api\/v1\/cars\/c1\/trip\?/, async (route) => {
      const res = await route.fetch();
      const body = await res.json();
      body.time_ms.asleep -= 2 * HOUR;
      body.time_ms.charging_unknown = 2 * HOUR;
      body.energy_kwh.added_unknown = 12.5;
      await route.fulfill({ response: res, json: body });
    });
    await page.goto(`${server.origin}/cars/c1/trip?preset=week`);
    await page.waitForFunction(() => document.documentElement.dataset.ready === 'trip', null, { timeout: 30000 });

    const legend = page.locator('.trip-split-legend li', { hasText: 'Charging (kind not known)' });
    assert.equal(await legend.count(), 1, 'the time split names the charging of no known kind');
    assert.match(await legend.innerText(), /2\s*h/, 'with its 2 hours');

    const parts = await page.$$eval('.trip-split > i', (els) => els.map((e) => ({ title: e.title, w: parseFloat(e.style.getPropertyValue('--w')) })));
    assert.ok(parts.some((p) => p.title.startsWith('Charging (kind not known)')), 'the bar has a part for it');
    const sum = parts.reduce((s, p) => s + p.w, 0);
    assert.ok(Math.abs(sum - 100) < 0.01, `the parts of the bar add up to the whole: ${sum}`);
    const label = await page.getAttribute('.trip-split', 'aria-label');
    assert.match(label, /Charging \(kind not known\) 2\s*h/);

    const row = page.locator('.trip-energy li', { hasText: 'Added (kind not known)' });
    assert.equal(await row.count(), 1, 'the energy figures have the energy added by such a charge');
    assert.match(await row.innerText(), /12\.5/);
  } finally { await context.close(); }
});
