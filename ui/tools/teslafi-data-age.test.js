// The car overview in Chromium on the dev API server, every built-in pack (Instrument; the others are archived):
// TeslaFi's data is stamped with TeslaFi's own time, so a connection that polls without errors can still give old
// data. Under the live inputs line the page says whose problem it is: TeslaFi has had no new data from the car for N h
// (its side), or its times are read in the wrong zone (the account's setting), and says nothing for current data or a
// sleeping car. Thresholds as the server's: 2 h without new data, a fresh poll stamped an hour or more off.
// Run: node --test ui/tools/teslafi-data-age.test.js. `PACKS=a,b` runs only those. Skipped when Playwright is not installed.
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

const PACKS = (process.env.PACKS || BUILTIN_PACKS.join(',')).split(',').filter(Boolean);
const MIN = 60_000, HOUR = 3_600_000;

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

/** The inputs line's note on TeslaFi's data ('' when the page says none) for TeslaFi data `last` (its own time) that
 *  came at `came`, relative to the server's clock, with the car in `state`. */
async function note(context, origin, nowMs, { last, came, state }) {
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  await page.route((u) => u.pathname === '/api/v1/cars/c3/now', async (route) => {
    const res = await route.fetch();
    const body = await res.json();
    if (state) body.state = state;
    body.inputs = (body.inputs || []).filter((i) => i.kind !== 'teslafi_api');
    body.inputs.push({ kind: 'teslafi_api', label: 'TeslaFi', last_ms: nowMs + last, received_ms: nowMs + came });
    await route.fulfill({ response: res, json: body });
  });
  await page.goto(`${origin}/cars/c3`);
  await page.waitForSelector('.now-inputs .src', { timeout: 30000 });
  await page.waitForTimeout(300);
  const text = await page.evaluate(() => document.querySelector('.now-inputs-data')?.innerText || '');
  assert.deepEqual(errors, []);
  await page.close();
  return text;
}

for (const pack of PACKS) {
  test(`${pack}: the car overview says whose problem old TeslaFi data is`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const browser = await playwright.chromium.launch();
    const server = await devServer(pack);
    try {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
      const nowMs = (await (await context.request.get(`${server.origin}/api/v1/integrations/overview`)).json()).read_ms;

      // nothing new for six hours: TeslaFi's side
      const none = await note(context, server.origin, nowMs, { last: -6 * HOUR, came: -6 * HOUR + MIN, state: 'parked' });
      assert.match(none, /TeslaFi has had no new data from the car for 5 h 59 min/, `${pack}: ${none}`);
      assert.match(none, /problem is on TeslaFi's side/, none);

      // polls that come but are stamped six hours before they are read: the zone
      const zone = await note(context, server.origin, nowMs, { last: -MIN - 6 * HOUR, came: -MIN, state: 'parked' });
      assert.match(zone, /stamped 6 h before the moment this server read it/, `${pack}: ${zone}`);
      assert.match(zone, /time zone/, zone);

      // a sleeping car is silent as expected; current data says nothing
      assert.equal(await note(context, server.origin, nowMs, { last: -6 * HOUR, came: -6 * HOUR, state: 'asleep' }), '', `${pack}: asleep`);
      assert.equal(await note(context, server.origin, nowMs, { last: -50_000, came: -45_000, state: 'parked' }), '', `${pack}: current`);
      await context.close();
    } finally {
      await server.stop();
      await browser.close();
    }
  });
}
