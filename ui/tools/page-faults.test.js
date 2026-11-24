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
// a pack named here that is not built in (ui/tools/builtin-packs.js) drops out
const packs = (...list) => list.filter((p) => BUILTIN_PACKS.includes(p) && (!ONLY || ONLY.has(p)));
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

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: the empty note of a wide table sits inside the part of the table that is in view`, { skip: SKIP, timeout: 120000 }, async () => {
    await withPack(pack, ['--cars', '0'], async ({ origin, context }) => {
      const { page, errors } = await openPage(context, `${origin}/statistics`, 'statistics');
      await page.locator('.tbl-scroll .tbl-empty .empty').first().waitFor({ timeout: 15000 });
      const box = () => page.evaluate(() => {
        const e = document.querySelector('.tbl-scroll .tbl-empty .empty');
        const sc = e.closest('.tbl-scroll');
        const r = e.getBoundingClientRect(), s = sc.getBoundingClientRect();
        const texts = [...e.querySelectorAll('h3, p')].map((x) => { const range = document.createRange(); range.selectNodeContents(x); return range.getBoundingClientRect(); });
        return { wide: sc.scrollWidth > sc.clientWidth + 1, left: r.left - s.left, right: r.right - s.right, textLeft: Math.min(...texts.map((t) => t.left)) - s.left, textRight: Math.max(...texts.map((t) => t.right)) - s.right };
      });
      let b = await box();
      assert.ok(b.wide, 'the table is wider than its box (else this checks nothing)');
      assert.ok(b.left >= -1 && b.right <= 1 && b.textLeft >= 0 && b.textRight <= 0, `the note is inside the box: ${JSON.stringify(b)}`);
      // and stays there when the table is scrolled sideways
      await page.evaluate(() => { const sc = document.querySelector('.tbl-scroll:has(.tbl-empty)'); sc.scrollLeft = 300; });
      await page.waitForTimeout(100);
      b = await box();
      assert.ok(b.left >= -1 && b.right <= 1, `the note is still inside the box after scrolling: ${JSON.stringify(b)}`);
      assert.deepEqual(errors, []);
    });
  });
}

/** The frame's links (outside the page) that lead to All cars although they name another page. */
const deadLinks = (page) => page.evaluate(() => [...document.querySelectorAll('a[data-page], a[data-section]')]
  .filter((a) => !a.closest('main') && a.checkVisibility() && a.getAttribute('href') === '/' && a.dataset.page !== 'fleet' && a.getAttribute('aria-current') !== 'page')
  .map((a) => (a.dataset.page ? `a[data-page="${a.dataset.page}"]` : `a[data-section="${a.dataset.section}"]`)));

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: with no car, a menu item of a car's page says there is no car yet, and Charges and Statistics ask for no car`, { skip: SKIP, timeout: 180000 }, async () => {
    await withPack(pack, ['--cars', '0'], async ({ origin, context }) => {
      const { page, errors } = await openPage(context, `${origin}/`, 'fleet');
      const links = await deadLinks(page);
      assert.ok(links.length >= 3, `the car pages' links: ${links}`);
      for (const sel of links) {
        await page.locator(`${sel}:visible`).first().click();
        const dlg = page.locator('dialog.switcher[open]');
        await dlg.waitFor({ timeout: 5000 }).catch(() => assert.fail(`${sel}: a click opens nothing and says nothing`));
        await dlg.getByText('No cars yet').waitFor({ timeout: 5000 });
        await page.keyboard.press('Escape');
        await dlg.waitFor({ state: 'detached', timeout: 2000 }).catch(() => {});
        assert.equal(new URL(page.url()).pathname, '/');
      }
      await page.close();

      for (const [route, id] of [['/charges', 'charges'], ['/statistics', 'statistics']]) {
        const asked = [];
        const p2 = await openPage(context, origin + route, id, (pg) => pg.on('request', (r) => { if (r.url().includes('/api/')) asked.push(new URL(r.url()).pathname); }));
        await p2.page.waitForLoadState('networkidle', { timeout: 20000 }).catch(() => {});
        await p2.page.evaluate(() => window.scrollTo(0, document.body.scrollHeight));
        await p2.page.waitForTimeout(800);
        const text = await mainText(p2.page);
        assert.deepEqual(asked.filter((u) => /undefined|null/.test(u)), [], `${route} asks for no car that is not there`);
        assert.doesNotMatch(text, /undefined|All 0 cars/i, `${route} names no car that is not there`);
        assert.match(text, /no car on this server yet/i, `${route} says why it is empty`);
        assert.deepEqual(p2.errors, []);
        await p2.page.close();
      }
      assert.deepEqual(errors, []);
    });
  });

  test(`${pack}: before any car was opened, a menu item of a car's page asks which car and then opens that page for it`, { skip: SKIP, timeout: 180000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      const { page, errors } = await openPage(context, `${origin}/`, 'fleet');
      await page.waitForLoadState('networkidle', { timeout: 20000 }).catch(() => {});
      const links = (await deadLinks(page)).filter((s) => s.includes('data-page'));
      if (!links.length) return;   // the frame knows a car already (the person's main car): its links lead there
      const want = /data-page="([^"]+)"/.exec(links[0])[1];
      await page.locator(`${links[0]}:visible`).first().click();
      const dlg = page.locator('dialog.switcher[open]');
      await dlg.waitFor({ timeout: 5000 });
      await dlg.locator('.sw-opt').first().waitFor({ timeout: 5000 });
      await page.keyboard.press('Enter');
      await page.waitForFunction((id) => document.documentElement.dataset.ready === id, want, { timeout: 30000 });
      assert.match(new URL(page.url()).pathname, /^\/cars\/c\d+/);
      assert.deepEqual(errors, []);
    });
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: Places asks for the drives per car of only the cars that have them, a few at a time`, { skip: SKIP, timeout: 180000 }, async () => {
    await withPack(pack, ['--cars', '200'], async ({ origin, context }) => {
      const asked = [], bad = [];
      const { page, errors } = await openPage(context, `${origin}/places`, 'places', (pg) => {
        pg.on('request', (r) => { if (/\/api\/v1\/places\?(.*&)?car=/.test(r.url())) asked.push(new URL(r.url()).searchParams.get('car')); });
        pg.on('response', (r) => { if (r.url().includes('/api/') && r.status() >= 400) bad.push(`${r.status()} ${r.url()}`); });
      });
      await page.waitForLoadState('networkidle', { timeout: 20000 }).catch(() => {});
      const cars = new Set(asked);
      // four of the 200 cars have drives (c1 to c3 and the sold c0); a few more may be asked before the last answer is in
      assert.ok(cars.size >= 4 && cars.size <= 10, `asked for ${cars.size} cars: ${[...cars].slice(0, 20)}`);
      assert.deepEqual(bad, []);
      assert.match(await page.locator('.pl-hist').first().innerText(), /drives?( \(|: )(Comet|Maple|Atlas|Ember)/, 'the drives are split by car');
      assert.deepEqual(errors, []);
    });
  });

  test(`${pack}: Places whose drives of one car cannot be read says so, and reads them again on Try again`, { skip: SKIP, timeout: 180000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      let fail = true;
      const { page, errors } = await openPage(context, `${origin}/places`, 'places', (pg) => pg.route(/\/api\/v1\/places\?(.*&)?car=c1(&|$)/, (r) => (fail
        ? r.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify({ error: { code: 'internal', message: 'Something went wrong on the server.' } }) }) : r.continue())));
      const banner = page.locator('.pl-banner', { hasText: 'could not be read for Comet' });
      await banner.waitFor({ timeout: 15000 });
      const home = page.locator('tr', { hasText: 'Home' }).locator('.pl-hist').first();
      assert.doesNotMatch(await home.innerText(), /Comet/, 'Comet’s drives are not shown as read');
      assert.match(await home.innerText(), /of a car not read|(^|\n)[\d,]+ drives(\n|$)/, 'the total is not given to another car');
      fail = false;
      await banner.getByRole('button', { name: 'Try again' }).click();
      await banner.waitFor({ state: 'hidden', timeout: 15000 });
      await page.locator('tr', { hasText: 'Home' }).locator('.pl-hist', { hasText: 'Comet' }).first().waitFor({ timeout: 15000 });
      assert.deepEqual(errors, []);
    });
  });
}

/** Opens `route` with every GET whose address matches `fail` answered 500, and gives the page's text once it is quiet. */
async function textWithFailing(context, origin, route, fail) {
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  let hit = 0;
  await page.route((u) => u.pathname.startsWith('/api/v1/') && fail.test(u.pathname + u.search), (r) => {
    if (r.request().method() !== 'GET') return r.continue();
    hit += 1;
    return r.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify({ error: { code: 'internal', message: 'Something went wrong on the server.' } }) });
  });
  await page.goto(origin + route);
  await page.waitForFunction(() => document.documentElement.dataset.ready, null, { timeout: 30000 });
  await page.waitForLoadState('networkidle', { timeout: 20000 }).catch(() => {});
  // what is below the fold is drawn when it comes near: go down the page and back
  const tall = await page.evaluate(() => document.documentElement.scrollHeight);
  for (let y = 0; y < tall; y += 700) { await page.mouse.wheel(0, 700); await page.waitForTimeout(120); }
  await page.waitForLoadState('networkidle', { timeout: 20000 }).catch(() => {});
  await page.waitForTimeout(500);
  // every text of the page, also in a part folded away now (Horizon and Manager fold the overview's sections)
  const text = await page.evaluate(() => document.querySelector('main').textContent);
  const ready = await page.evaluate(() => document.documentElement.dataset.ready);
  await page.close();
  return { text, ready, hit, errors };
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: a side read that fails is said as not loaded, never shown as "there is none"`, { skip: SKIP, timeout: 300000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      const get = async (p) => (await context.request.get(`${origin}/api/v1/${p}`)).json();
      const drive = (await get('cars/c1/drives?limit=1')).items[0].id;
      const charge = (await get('cars/c1/charges?kind=dc&limit=50')).items.find((c) => c.place?.id && !c.flags?.in_progress);
      assert.ok(charge, 'a fast charge at a saved place');
      const rules = (await get('cars/c1/purpose-rules')).items.length;
      const look = (await get('fleet/attention?limit=8')).items.length;
      assert.ok(rules > 0 && look > 0, 'the made-up data has purpose rules and cars that need a look');
      const esc = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
      // [page, the read that fails, what must not be claimed, what is said instead]
      const cases = [
        ['/cars/c1', /^\/api\/v1\/cars\/c1\/charges(\?|$)/, /No charging in this period/, /charges of this period could not be loaded/],
        [`/cars/c1/drives/${drive}`, /\/track(\?|$)/, /No route was recorded/, /route could not be loaded/],
        [`/cars/c1/charges/${charge.id}`, new RegExp(`/charges/${charge.id}/curve`), /No curve was recorded/, /curve could not be loaded/],
        [`/cars/c1/charges/${charge.id}`, new RegExp(`/places/${esc(charge.place.id)}(\\?|$)`), /: no price|has no price yet/, /price could not be loaded/],
        ['/cars/c1/logbook', /\/purpose-rules/, /No rules yet/, /rules could not be loaded/],
        ['/', /\/fleet\/attention/, /^$/, /cars that need a look could not be loaded/],
        ['/cars/c1/drives', /^\/api\/v1\/places\?/, /^$/, /places could not be loaded/],
        ['/cars/c1/trip', /\/cars\/c1\/series/, /No battery readings in this window/, /battery readings could not be loaded/],
        ['/cars/c1/trip', /\/api\/v1\/trips\?/, /No saved trips yet/, /Saved trips could not be loaded/],
        ['/places', /\/places\/suggestions/, /No suggestions/, /Suggestions could not be loaded/],
      ];
      for (const [route, fail, never, said] of cases) {
        const got = await textWithFailing(context, origin, route, fail);
        const what = `${route} with ${fail} failing`;
        assert.ok(got.hit > 0, `${what}: the read was made`);
        assert.doesNotMatch(got.ready, /^error:/, `${what}: the page still opens`);
        if (String(never) !== '/^$/') assert.doesNotMatch(got.text, never, `${what}: says what is not known`);
        assert.match(got.text, said, `${what}: says the read failed`);
        assert.deepEqual(got.errors, [], what);
      }
    });
  });
}

for (const pack of packs('horizon', 'manager')) {
  test(`${pack}: the old short address /charging opens Charges, and a section's short address opens one of its pages`, { skip: SKIP, timeout: 120000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      // with a car opened before (the car's Charges) and with none (Charges of all cars)
      for (const [first, want] of [['/cars/c1', '/cars/c1/charges'], [null, '/charges']]) {
        const ctx = first ? context : await context.browser().newContext({ viewport: { width: 1440, height: 900 } });
        if (first) (await openPage(ctx, origin + first, 'car')).page.close();
        const { page, errors } = await openPage(ctx, `${origin}/charging`, 'charges');
        assert.equal(await page.evaluate(() => location.pathname), want);
        assert.deepEqual(errors, []);
        await page.close();
        if (!first) await ctx.close();
      }
      for (const [short, id] of [['/battery', 'charges'], ['/trips', 'drives'], ['/stats', 'statistics'], ['/settings', 'settings-cars']]) {
        const { page, errors } = await openPage(context, origin + short, id);
        assert.deepEqual(errors, [], short);
        await page.close();
      }
    });
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT, 'console')) {
  test(`${pack}: a #fragment in the address that is not valid percent-encoding throws nothing`, { skip: SKIP, timeout: 120000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      const { page, errors } = await openPage(context, `${origin}/help#%E0%A4%A`, 'help');
      await page.waitForTimeout(300);
      assert.deepEqual(errors, []);
      // and a section that is there is still opened
      const ok = await openPage(context, `${origin}/help#licences`, 'help');
      await ok.page.waitForFunction(() => window.scrollY > 0, null, { timeout: 10000 });
      assert.deepEqual(ok.errors, []);
    });
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: the empty note of a wide table breaks into lines inside its own width`, { skip: SKIP, timeout: 120000 }, async () => {
    await withPack(pack, ['--cars', '200'], async ({ origin, context }) => {
      // (the made-up car c4 has no data in the default period)
      const { page, errors } = await openPage(context, `${origin}/cars/c4/statistics`, 'statistics');
      await page.locator('.tbl-scroll .tbl-empty .empty p').first().waitFor({ timeout: 15000 });
      const out = await page.evaluate(() => [...document.querySelectorAll('.tbl-scroll .tbl-empty .empty p')].map((p) => {
        const range = document.createRange(); range.selectNodeContents(p);
        const t = range.getBoundingClientRect(), b = p.getBoundingClientRect();
        return Math.round(Math.max(b.left - t.left, t.right - b.right));
      }).filter((px) => px > 0));
      assert.deepEqual(out, [], 'no note text sticks out of its paragraph');
      assert.deepEqual(errors, []);
    });
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: a number a field cannot read is said under the field, and the value stored stays`, { skip: SKIP, timeout: 180000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      const sent = [];
      const { page, errors } = await openPage(context, `${origin}/settings/cars/c1`, 'settings-cars',
        (pg) => pg.on('request', (r) => { if (r.method() === 'PATCH') sent.push(`${new URL(r.url()).pathname} ${r.postData()}`); }));
      for (const [name, good, bad, read] of [
        ['sc-capacity_new_kwh', '75', '1e', async () => (await (await context.request.get(`${origin}/api/v1/cars/c1/settings`)).json()).capacity_new_kwh],
        ['sc-tyre_pressure_front_kpa', '2.9', '-.', async () => (await (await context.request.get(`${origin}/api/v1/cars/c1`)).json()).config.tyre_pressure_front_kpa?.value],
      ]) {
        const input = page.locator(`main input[name="${name}"]`);
        await input.waitFor({ timeout: 15000 });
        const saved = () => page.waitForResponse((r) => r.request().method() === 'PATCH', { timeout: 10000 });
        let done = saved();
        await input.fill(good); await input.press('Tab');
        await done;
        await page.waitForTimeout(500);   // the panel is drawn again with the saved value
        const stored = await read();
        assert.ok(stored > 0, `${name}: ${good} is stored (${stored})`);
        sent.length = 0;
        // typed, as a person does: a number input gives "" for text it cannot read
        const again = page.locator(`main input[name="${name}"]`);
        await again.click(); await again.press('Control+a'); await page.keyboard.type(bad); await again.press('Tab');
        await page.waitForTimeout(800);
        assert.deepEqual(sent, [], `${name}: "${bad}" is not a number, so nothing is sent`);
        assert.equal(await read(), stored, `${name}: the stored value stays`);
        const said = await page.evaluate((name) => {
          const i = document.querySelector(`main input[name="${name}"]`);
          const e = i.closest('.form-row')?.querySelector('.form-error:not([hidden])');
          return { invalid: i.getAttribute('aria-invalid'), text: e ? e.textContent : null };
        }, name);
        assert.equal(said.invalid, 'true', `${name}: the field is marked as wrong`);
        assert.match(said.text || '', /A number from/, `${name}: and says what it takes`);
      }
      assert.deepEqual(errors, []);
    });
  });

  test(`${pack}: saving a place whose price and circle were not touched asks nothing about past charges`, { skip: SKIP, timeout: 180000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      // a priced place with past charges a reprice would change
      const places = (await (await context.request.get(`${origin}/api/v1/places?limit=200`)).json()).items;
      let place = null;
      for (const p of places.filter((x) => x.price)) {
        const eff = await (await context.request.post(`${origin}/api/v1/places/preview`, { headers: { 'X-Sentry-Mate': '1' }, data: { id: p.id } })).json();
        if (eff.charges_repriced > 0) { place = p; break; }
      }
      assert.ok(place, 'the made-up data has a priced place whose past charges would cost differently (else this checks nothing)');
      const { page, errors } = await openPage(context, `${origin}/places?place=${place.id}`, 'places');
      await page.locator('main input[name="name"]').waitFor({ timeout: 15000 });
      await page.fill('main input[name="name"]', 'Renamed place');
      const saved = page.waitForResponse((r) => r.request().method() === 'PATCH' && /\/places\//.test(new URL(r.url()).pathname));
      await page.locator('main button[type="submit"]', { hasText: 'Save changes' }).first().click();
      const res = await saved;
      assert.deepEqual(Object.keys(JSON.parse(res.request().postData())).sort(), ['name', 'version'], 'only the name was sent');
      assert.ok((await res.json()).effect.charges_repriced > 0, 'the answer counts past charges a reprice would change');
      await page.waitForFunction(() => !location.search.includes('place=') && document.documentElement.dataset.ready === 'places', null, { timeout: 15000 })
        .catch(async () => assert.fail(`a rename goes back to the list; instead: ${await page.evaluate(() => document.querySelector('dialog[open]')?.innerText.slice(0, 120) || location.search)}`));
      assert.equal(await page.locator('dialog[open]').count(), 0);

      // a changed price still asks
      const p2 = await openPage(context, `${origin}/places?place=${place.id}`, 'places');
      const price = p2.page.locator('main input[name="per_kwh"]');
      await price.waitFor({ timeout: 15000 });
      await price.fill('0.77');
      await p2.page.locator('main button[type="submit"]', { hasText: 'Save changes' }).first().click();
      await p2.page.locator('dialog[open]', { hasText: 'Reprice past charges too?' }).waitFor({ timeout: 15000 });
      assert.deepEqual([...errors, ...p2.errors], []);
    });
  });

  test(`${pack}: Charges and Statistics for cars that are not there say so once, and ask the server for none of them`, { skip: SKIP, timeout: 180000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      for (const [route, id] of [['/charges?cars=zzz', 'charges'], ['/statistics?cars=zzz,yyy', 'statistics']]) {
        const asked = [], missing = [];
        const { page, errors } = await openPage(context, origin + route, id, (pg) => {
          pg.on('request', (r) => { if (r.url().includes('/api/') && /zzz|yyy/.test(r.url())) asked.push(r.url().replace(origin, '')); });
          pg.on('response', (r) => { if (r.status() === 404) missing.push(r.url().replace(origin, '')); });
        });
        await page.waitForLoadState('networkidle', { timeout: 20000 }).catch(() => {});
        const text = await mainText(page);
        assert.match(text, /None of these cars is yours to see/, `${route} says it`);
        assert.doesNotMatch(text, /zzz|No charge matches|0 cars|Not found/i, `${route} shows no figures for no car: ${text.slice(0, 200)}`);
        assert.deepEqual(asked, [], `${route} asks for no car that is not there`);
        assert.deepEqual(missing, []);
        // the way out: every car
        await page.locator('main a.btn', { hasText: 'Every car' }).click();
        await page.waitForFunction((id) => !location.search.includes('zzz') && document.documentElement.dataset.ready === id, id, { timeout: 30000 });
        assert.deepEqual(errors, []);
        await page.close();
      }
      // one car that is there beside one that is not: the page is the one car's
      const asked = [];
      const { page } = await openPage(context, `${origin}/charges?cars=c1,zzz`, 'charges', (pg) => pg.on('response', (r) => { if (r.status() === 404) asked.push(r.url().replace(origin, '')); }));
      await page.waitForLoadState('networkidle', { timeout: 20000 }).catch(() => {});
      assert.deepEqual(asked, [], 'no request of the page is answered 404');
      assert.doesNotMatch(await mainText(page), /zzz/);
    });
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: more side reads that fail are said as not loaded: the other cars, a place's name, the battery's totals, a car's name`, { skip: SKIP, timeout: 300000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      // [page, the read that fails, what must not be claimed, what is said instead]
      const cases = [
        ['/cars/c1/trip', /^\/api\/v1\/cars\?/, /Only one car/, /other cars could not be loaded/],
        ['/cars/c1/logbook', /^\/api\/v1\/places\?/, /place p\d/, /a place whose name could not be loaded/],
        ['/cars/c1/battery', /\/cars\/c1\/efficiency/, /Not worked out yet/, /Could not be loaded/],
        ['/cars/c1/battery', /\/cars\/c1\/totals/, /^$/, /logged drives and charges could not be loaded/],
        ['/', /^\/api\/v1\/cars\/c3(\?|$)/, /\bc3\b/, /a car whose name could not be loaded/i],
      ];
      for (const [route, fail, never, said] of cases) {
        const got = await textWithFailing(context, origin, route, fail);
        const what = `${route} with ${fail} failing`;
        assert.ok(got.hit > 0, `${what}: the read was made`);
        assert.doesNotMatch(got.ready, /^error:/, `${what}: the page still opens`);
        if (String(never) !== '/^$/') assert.doesNotMatch(got.text, never, `${what}: says what is not known`);
        assert.match(got.text, said, `${what}: says the read failed`);
        assert.deepEqual(got.errors, [], what);
      }
      // the cars themselves cannot be read: Charges and Statistics for every car say that, not "no car yet" with bare ids
      for (const route of ['/charges', '/statistics']) {
        const got = await textWithFailing(context, origin, route, /^\/api\/v1\/cars\?/);
        assert.ok(got.hit > 0, `${route}: the cars were asked for`);
        assert.doesNotMatch(got.text, /no car on this server yet|No car yet|\bc[123]\b/i, `${route}: claims no empty server and shows no bare id`);
        assert.match(got.text, /The server had a problem|could not be loaded/, `${route}: says the read failed`);
        assert.match(got.text, /Try again/, `${route}: with a way to read again`);
        assert.deepEqual(got.errors, [], route);
      }
    });
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  for (const [what, viewport] of [['a phone', { width: 390, height: 844 }], ['a computer', { width: 1440, height: 900 }]]) {
    test(`${pack}: a car and a place whose names are one long word leave no page wider than the window of ${what}`, { skip: SKIP, timeout: 420000 }, async () => {
      await withPack(pack, [], async ({ origin, context }) => {
        const patch = async (path, name) => {
          const cur = await (await context.request.get(origin + path)).json();
          const res = await context.request.patch(origin + path, { headers: { 'X-Sentry-Mate': '1' }, data: { name, version: cur.version } });
          assert.equal(res.status(), 200, `${path}: the long name is accepted`);
        };
        await patch('/api/v1/cars/c1', 'Wolfeschlegelsteinhausenbergerdorffvoralternwarengewissenhaftsch');   // 64 letters: the longest a car's name can be
        await patch('/api/v1/places/p1', 'Supercalifragilisticexpialidocious-Garage-And-Workshop-Number-12');
        const drive = (await (await context.request.get(`${origin}/api/v1/cars/c1/drives?limit=2`)).json()).items[1].id;
        const charge = (await (await context.request.get(`${origin}/api/v1/cars/c1/charges?limit=1`)).json()).items[0].id;
        const wide = [];
        for (const [route, id] of [['/cars/c1/timeline', 'timeline'], [`/cars/c1/drives/${drive}`, 'drive'], ['/cars/c1/trip', 'trip'], ['/cars/c1/logbook', 'logbook'],
          ['/cars/c1/charges', 'charges'], ['/charges', 'charges'], [`/cars/c1/charges/${charge}`, 'charge'], ['/cars/c1/efficiency', 'efficiency'], ['/cars/c1/statistics', 'statistics'],
          ['/statistics', 'statistics'], ['/travel', 'travel'], ['/cars/c1/software', 'software'], ['/places', 'places'], ['/settings/cars/c1', 'settings-cars'], ['/settings/cars', 'settings-cars'],
          ['/settings/integrations', 'settings-integrations'], ['/settings/server', 'settings-server'], ['/settings/import', 'import'], ['/cars/c1/health', 'health'], ['/cars/c1/drives', 'drives']]) {
          const { page } = await openPage(context, origin + route, id);
          await page.waitForLoadState('networkidle', { timeout: 20000 }).catch(() => {});
          // the parts built when they come near the screen
          await page.evaluate(async () => { for (let y = 0; y < document.body.scrollHeight; y += 700) { window.scrollTo(0, y); await new Promise((r) => setTimeout(r, 60)); } window.scrollTo(0, 0); });
          await page.waitForTimeout(300);
          const by = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
          if (by > 1) wide.push(`${route}: ${by} px`);
          await page.close();
        }
        assert.deepEqual(wide, [], 'pages that scroll sideways');
      }, viewport);
    });
  }
}
