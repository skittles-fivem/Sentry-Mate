// The Battery page of every built-in pack in Chromium on the dev API server: each capacity figure names where it
// comes from. Usable now is the BMS's; usable when new, when the earliest imported charges give it, says it is worked
// out per charge from them on the BMS's basis (energy stored, not energy added); the capacity chart keeps the
// imported charges' readings and SMO's own (CAN) apart and calls the monthly median the BMS's; the lifetime energies
// say which is pack side and which wall side. A per-charge reading that could only use the energy added says it
// reads high.
// Run: node --test ui/tools/battery-capacity-labels.test.js. Skipped when Playwright is not installed.
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

/** The Battery page's text once the capacity chart is drawn, with `battery` and `capacity` answers changed. */
async function pageText(context, origin, car, { battery, capacity }) {
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  const change = (url, f) => page.route((u) => u.pathname === url, async (route) => {
    const res = await route.fetch();
    const body = await res.json();
    if (f) f(body, new URL(route.request().url()).searchParams);
    await route.fulfill({ response: res, json: body });
  });
  await change(`/api/v1/cars/${car}/battery`, battery);
  await change(`/api/v1/cars/${car}/battery/capacity`, capacity);
  await page.goto(`${origin}/cars/${encodeURIComponent(car)}/battery`);
  await page.waitForFunction(() => document.documentElement.dataset.ready === 'battery', null, { timeout: 30000 });
  await page.waitForFunction(() => /Monthly median \(BMS\)/.test(document.body.innerText), null, { timeout: 30000 });
  const text = await page.evaluate(() => document.body.innerText);
  assert.deepEqual(errors, []);
  await page.close();
  return text;
}

for (const pack of BUILTIN_PACKS) {
  test(`${pack}: Battery labels each capacity figure's source`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const browser = await playwright.chromium.launch();
    const server = await devServer(pack);
    try {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
      const cars = await (await context.request.get(`${server.origin}/api/v1/cars`)).json();
      const car = (cars.items || cars)[0].id;

      // the owner's shape: logged late, "when new" from the earliest imported charges; one per-charge reading of
      // an imported charge, the rest SMO's own
      let marked = false;
      const text = await pageText(context, server.origin, car, {
        battery: (b) => {
          Object.assign(b.capacity, { new_source: 'imported_charges' });
          b.notes = [...(b.notes || []).filter((n) => !/^new_value_/.test(n)), 'new_value_from_imported_charges'];
        },
        capacity: (c, q) => {
          if (q.get('group') !== 'charge') return;
          for (const it of c.items) Object.assign(it, { source: 'can' });
          if (c.items[0] && !marked) { Object.assign(c.items[0], { source: 'teslamate', usable_basis: 'energy_added' }); marked = true; }
        },
      });
      assert.match(text, /From the BMS: full pack/, 'usable now names the BMS');
      assert.match(text, /usable when new is worked out per charge from the earliest imported charges, on the same basis \(energy stored, pack side, not energy added\)/);
      assert.match(text, /per charge, from the earliest imported charges: the energy each stored/i, 'the when-new source');
      assert.match(text, /Per charge, imported charges \(energy stored\)/);
      assert.match(text, /Per charge, CAN \(energy stored\)/);
      assert.match(text, /Monthly median \(BMS\)/);
      assert.match(text, /Not the energy added/);
      assert.match(text, /marked “energy added” had no BMS energy readings/);
      assert.doesNotMatch(text, /energy added over the % it added, plus the buffer/i, 'the old wording is gone');
      assert.match(text, /Energy added to the pack \(pack side\)/);
      assert.match(text, /Energy from the grid \(wall side\)/);
      await context.close();
    } finally {
      await server.stop();
      await browser.close();
    }
  });
}
