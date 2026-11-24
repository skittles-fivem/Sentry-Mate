// Live list updates in Chromium on the dev API server: each built-in pack's Charges page loads its list again when a
// `rows` event says charges changed, and shows the new charge without a reload of the page.
// The dev server sends a `rows` event for every kind every 2 s (--rows-every-s 2), and in a second run every second
// (as while a rebuild runs: the page must not wait for the events to stop); the answers for the list after the page
// is ready get one more charge at the top, and the test waits for it to be drawn.
// Run: node --test ui/tools/live-rows.test.js. Skipped when Playwright is not installed.
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

const STALE_OK = 'x-sentry-mate-stale-ok';

function devServer(pack, extra) {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', path.join(UI_DIR, 'packs', pack),
      '--port', '0', '--freeze-clock', ...extra], { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
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

// Every 2 s, and every second as while a rebuild runs: words that keep coming must not put the load off for good.
for (const every of ['2', '1']) for (const pack of BUILTIN_PACKS) test(`${pack}: Charges loads its list again on a rows event${every === '1' ? ', also while one comes every second,' : ''} and shows the new charge`, { skip: !playwright && 'Playwright is not installed', timeout: 120000 }, async () => {
  const browser = await playwright.chromium.launch();
  const server = await devServer(pack, ['--rows-every-s', every, '--sse-interval-ms', every === '1' ? '1000' : '2000']);
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    const cars = await (await context.request.get(`${server.origin}/api/v1/cars`)).json();
    const car = (cars.items || cars)[0].id;
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));

    // The list itself is the request with limit=25 (Instrument's table) and no flags (the missing-data box and the
    // statistics differ).
    // The event stream is held until the test has seen the page ready with its one list load: on a busy machine the
    // first load can take longer than the 2 s between events, and an event's load would start before "ready" is said.
    // A read that only asks the service worker's memory (sent beside a list load whose answer takes more than
    // 100 ms, ui/shared/api.js) is no list load: it is answered like one and not counted.
    let listLoads = 0, armed = false, letEvents;
    const eventsHeld = new Promise((r) => { letEvents = r; });
    await page.route((u) => u.pathname === '/api/v1/events', async (route) => { await eventsHeld; await route.continue().catch(() => {}); });
    const NEW_ID = 'live-rows-new-charge';
    await page.route((u) => u.pathname === `/api/v1/cars/${car}/charges` && u.searchParams.get('limit') === '25' && !u.searchParams.has('flags') && !u.searchParams.has('cursor'), async (route) => {
      if (!route.request().headers()[STALE_OK]) listLoads++;
      const add = armed;
      // asked without the kept answer's tag (the page's cache revalidates; a 304 would carry no list to change),
      // and answered without a tag (the changed list is not the server's)
      const headers = { ...route.request().headers() };
      delete headers['if-none-match'];
      const res = await route.fetch({ headers });
      const body = await res.json();
      if (add && body.items?.length) body.items.unshift({ ...body.items[0], id: NEW_ID });
      const out = { ...res.headers() };
      delete out.etag;
      await route.fulfill({ status: 200, headers: out, json: body });
    });

    await page.goto(`${server.origin}/cars/${encodeURIComponent(car)}/charges`);
    await page.waitForFunction(() => document.documentElement.dataset.ready === 'charges', null, { timeout: 30000 });
    assert.equal(listLoads, 1, 'one list load before ready');
    assert.equal(await page.locator(`a[href*="${NEW_ID}"]`).count(), 0);
    armed = true;
    letEvents();

    await page.locator(`a[href*="${NEW_ID}"]`).first().waitFor({ state: 'attached', timeout: 20000 });
    assert.ok(listLoads >= 2, `the list loaded again (${listLoads} loads)`);
    assert.equal(await page.evaluate(() => document.documentElement.dataset.ready), 'charges', 'the page itself did not load again');
    assert.deepEqual(errors, []);
    await context.close();
  } finally {
    await server.stop();
    await browser.close();
  }
});
