// Pages that must say what is true when something is wrong, in Chromium on the dev API server: a read that
// failed is not shown as "there is none", a server without a car asks for no car and says so, a value typed out
// of range draws nothing and throws nothing, and an address that is no page has a heading.
// Each test names the packs it runs on; `PACKS=a,b` runs only those.
// Run: node --test ui/tools/page-faults.test.js. Skipped when Playwright is not installed.
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

const ONLY = process.env.PACKS ? new Set(process.env.PACKS.split(',')) : null;
const packs = (...list) => list.filter((p) => !ONLY || ONLY.has(p));
const BUILT_ON_INSTRUMENT = BUILTIN_PACKS.filter((p) => p === 'instrument');
const SKIP = !playwright && 'Playwright is not installed';

function devServer(pack, extra = []) {
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

/** Runs `body({ origin, context })` with a dev server for the pack and a browser window of that size. */
async function withPack(pack, extra, body, viewport = { width: 1440, height: 900 }) {
  const browser = await playwright.chromium.launch();
  const server = await devServer(pack, extra);
  try {
    const context = await browser.newContext({ viewport });
    // nothing leaves the machine: a request to another host gets a 1 x 1 picture
    await context.route(/^https?:\/\/(?!127\.0\.0\.1)/, (r) => r.fulfill({ status: 200, contentType: 'image/png',
      body: Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==', 'base64') }));
    await body({ origin: server.origin, context });
    await context.close();
  } finally {
    await server.stop();
    await browser.close();
  }
}

/** Opens `url` and waits until the page says it is ready (any page id, or `id`). Returns { page, errors }. */
async function openPage(context, url, id = null, before = null) {
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  if (before) await before(page);
  await page.goto(url);
  await page.waitForFunction((id) => { const r = document.documentElement.dataset.ready; return r && (!id || r === id); }, id, { timeout: 30000 });
  return { page, errors };
}

const mainText = (page) => page.evaluate(() => document.querySelector('main').innerText);

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: a latitude or longitude typed out of range in the place editor throws nothing`, { skip: SKIP, timeout: 120000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      const { page, errors } = await openPage(context, `${origin}/places?new=1`, 'places');
      const lat = page.locator('input[name=lat]'), lon = page.locator('input[name=lon]');
      await lat.waitFor({ timeout: 15000 });
      await page.locator('.pl-handle--centre').waitFor({ state: 'attached', timeout: 20000 });   // the map is up
      for (const v of ['999', '100', '-91']) { await lat.fill(v); await lat.blur(); }
      for (const v of ['999', '-181']) { await lon.fill(v); await lon.blur(); }
      await lat.fill('10.5'); await lat.blur();
      await lon.fill('20.25'); await lon.blur();
      await page.waitForTimeout(300);
      assert.deepEqual(errors, []);
    });
  });
}

/** How far the page scrolls sideways, and the elements cut by the window's right edge (not inside a scroller). */
const sideways = (page) => page.evaluate(() => ({
  by: document.documentElement.scrollWidth - document.documentElement.clientWidth,
  cut: [...document.querySelectorAll('main *')].filter((e) => {
    const b = e.getBoundingClientRect();
    if (!(b.width > 0) || b.right <= window.innerWidth + 1) return false;
    for (let p = e.parentElement; p && p !== document.body; p = p.parentElement) { if (getComputedStyle(p).overflowX !== 'visible') return false; }
    return true;
  }).slice(0, 5).map((e) => `${e.tagName.toLowerCase()}.${String(e.className).slice(0, 40)}`),
}));

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: a car name of one long word stays inside a phone's window on the car's pages`, { skip: SKIP, timeout: 120000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      const car = await (await context.request.get(`${origin}/api/v1/cars/c1`)).json();
      const name = `Family ${'Supercalifragilistic'.repeat(3)}`.slice(0, 64);
      const res = await context.request.patch(`${origin}/api/v1/cars/c1`, { headers: { 'X-Sentry-Mate': '1' }, data: { name, version: car.version } });
      assert.equal(res.status(), 200, 'the long name is accepted');
      for (const [route, id] of [['/cars/c1', 'car'], ['/cars/c1/drives', 'drives'], ['/cars/c1/battery', 'battery'], ['/', 'fleet']]) {
        const { page } = await openPage(context, origin + route, id);
        await page.waitForLoadState('networkidle', { timeout: 20000 }).catch(() => {});
        assert.match(await page.locator('body').innerText(), /Supercalifragilistic/i, `${route} shows the name`);
        const s = await sideways(page);
        assert.ok(s.by <= 1 && !s.cut.length, `${route} at 390 px scrolls sideways by ${s.by} px; cut: ${s.cut.join(', ')}`);
        await page.close();
      }
    }, { width: 390, height: 844 });
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: an address that is no page, or no car, has one h1 that says so, and the tab title says it too`, { skip: SKIP, timeout: 120000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      for (const route of ['/no-such-page', '/software', '/cars/zzz', '/cars/zzz/software']) {
        const page = await context.newPage();
        await page.goto(origin + route);
        await page.waitForFunction(() => /^error:/.test(document.documentElement.dataset.ready || ''), null, { timeout: 30000 });
        const h1 = await page.locator('main h1').allInnerTexts();
        assert.deepEqual(h1, ['Not found'], `${route}: the error screen's heading is the page's h1`);
        assert.match(await page.title(), /^Not found · /, `${route}: the tab title`);
        await page.close();
      }
      // the registry agrees with the routes: Software is a page of one car
      const { page } = await openPage(context, `${origin}/cars/c1/software`, 'software');
      const kind = await page.evaluate(async () => (await import('/core/pages.js')).PAGES.find((p) => p.id === 'software').car);
      assert.equal(kind, 'required');
    });
  });
}
