// The Battery page of all six packs in Chromium on the dev API server, with an answer of `/battery` the made-up
// cars do not give: a capacity that reads above the value when new (the server then answers no loss, full health
// and the note `reading_above_new`), and logged charges of no known kind (`lifetime.unknown_kwh`). Every pack says
// why no loss is shown, and shows the charges of no known kind as their own part of the energy into the pack,
// not as AC. Without them the page says neither.
// Run: node --test ui/tools/battery-figures.test.js. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawn } = require('child_process');

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

const ABOVE = /reads above the value when new/i;
const UNKNOWN = /kind not known/i;

/** The Battery page's text once it is ready and `wanted` (or nothing more) has been drawn. */
async function batteryText(context, origin, car, change, wanted) {
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  if (change) {
    await page.route((u) => u.pathname === `/api/v1/cars/${car}/battery`, async (route) => {
      const res = await route.fetch();
      const body = await res.json();
      change(body);
      await route.fulfill({ response: res, json: body });
    });
  }
  await page.goto(`${origin}/cars/${encodeURIComponent(car)}/battery`);
  await page.waitForFunction(() => document.documentElement.dataset.ready === 'battery', null, { timeout: 30000 });
  if (wanted.length) {
    await page.waitForFunction((res) => res.every((r) => new RegExp(r, 'i').test(document.body.innerText)), wanted.map((r) => r.source), { timeout: 20000 });
  } else {
    await page.waitForLoadState('networkidle', { timeout: 20000 });
  }
  const text = await page.evaluate(() => document.body.innerText);
  assert.deepEqual(errors, []);
  await page.close();
  return text;
}

for (const pack of ['instrument', 'horizon', 'manager', 'logbook', 'console', 'pebble']) {
  test(`${pack}: Battery says why no loss is shown, and shows charges of no known kind as their own part`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const browser = await playwright.chromium.launch();
    const server = await devServer(pack);
    try {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
      const cars = await (await context.request.get(`${server.origin}/api/v1/cars`)).json();
      const car = (cars.items || cars)[0].id;

      // as the made-up car is: a loss, every charge AC or DC
      const plain = await batteryText(context, server.origin, car, null, []);
      assert.doesNotMatch(plain, ABOVE);
      assert.doesNotMatch(plain, UNKNOWN);

      const text = await batteryText(context, server.origin, car, (b) => {
        Object.assign(b.capacity, { difference_kwh: -4, degradation_pct: 0, health_pct: 100 });
        b.notes = [...(b.notes || []), 'reading_above_new'];
        b.lifetime.unknown_kwh = 4321;
        b.lifetime.charged_kwh += 4321;
      }, [ABOVE, UNKNOWN]);
      assert.match(text, ABOVE);
      assert.match(text, UNKNOWN);
      // the energy of those charges is on the page (4,321 kWh, or 4.32 MWh, however the pack writes it)
      assert.match(text.replace(/[\s,.\u00a0\u202f]/g, ''), /432/, 'the energy of the charges of no known kind');
      await context.close();
    } finally {
      await server.stop();
      await browser.close();
    }
  });
}
