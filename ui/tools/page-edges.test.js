// Pages at their edges, in Chromium on the dev API server: a link to a section of the page that is open lands
// below the top bar, as a link from another page does; a sold date after today and a currency that is none are
// refused under their fields; a period still to come or before 1970 gives a page, not an error; a search for one very
// long word leaves the page as wide as the window; Battery of a car whose pack reported nothing writes no
// sentence around a dash; the car switcher of a large fleet counts the cars not sold and says where the sold ones
// are; Statistics without a car counts no "0 cars"; a Map or Satellite pick that could not be saved is said; a page
// opened while the server is away says so inside the app.
// `PACKS=a,b` runs only those. Run: node --test ui/tools/page-edges.test.js. Skipped when Playwright is not installed.
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
  test(`${pack}: a link to a section of the open page lands below the top bar`, { skip: SKIP, timeout: 120000 }, async () => {
    for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }]) {
      await withPack(pack, [], async ({ origin, context }) => {
        const { page, errors } = await openPage(context, `${origin}/help`, 'help');
        // Help's own link to its questions, then the footer's links to the licences and the data credits
        for (const [link, id] of [['main a[href$="#qa"]', 'qa'], ['a[href="/help#licences"]', 'licences'], ['a[href="/help#credits"]', 'credits']]) {
          await page.evaluate(() => window.scrollTo(0, 0));
          await page.locator(link).filter({ visible: true }).first().click();
          await page.waitForFunction((id) => location.hash === `#${id}`, id);
          await page.waitForTimeout(400);
          const at = await page.evaluate((id) => ({
            top: document.getElementById(id).getBoundingClientRect().top,
            bar: document.querySelector('.topbar').getBoundingClientRect().bottom,
            end: window.scrollY >= document.documentElement.scrollHeight - window.innerHeight - 1,
          }), id);
          assert.ok(at.top >= at.bar, `${viewport.width} px: #${id} starts at ${at.top}, under the top bar that ends at ${at.bar}`);
          assert.ok(at.top <= at.bar + 40 || at.end, `${viewport.width} px: #${id} starts at ${at.top}, far below the top bar`);
        }
        assert.deepEqual(errors, []);
      }, viewport);
    }
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: Settings: Cars refuses a sold date after today and a currency that is none, under their fields, and sends neither`, { skip: SKIP, timeout: 180000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      const { page, errors } = await openPage(context, `${origin}/settings/cars/c2`, 'settings-cars');
      const sent = [];
      page.on('request', (r) => { if (r.method() === 'PATCH' && /\/api\/v1\/cars\//.test(r.url())) sent.push(r.postData()); });
      const errorOf = (name) => page.evaluate((name) => {
        const input = document.querySelector(`main input[name="${name}"]`);
        const ids = (input.getAttribute('aria-describedby') || '').split(' ');
        return { invalid: input.getAttribute('aria-invalid'), text: ids.map((id) => document.getElementById(id)).filter((e) => e && !e.hidden && /error/.test(e.className)).map((e) => e.textContent).join(' '), focused: document.activeElement === input };
      }, name);

      // a sold date next year, typed (the picker's own limit does not stop the keyboard)
      await page.fill('main input[name="sc-sold-date"]', `${new Date().getFullYear() + 1}-01-01`);
      await page.getByRole('button', { name: 'Mark as sold' }).click();
      await page.waitForTimeout(700);
      const day = await errorOf('sc-sold-date');
      assert.deepEqual(sent, [], 'a sold date after today is not sent');
      assert.equal(day.invalid, 'true');
      assert.match(day.text, /today/i);
      assert.ok(day.focused, 'the date field has the focus');
      assert.doesNotMatch(await mainText(page), /Sold on/);

      // a currency of three letters that is none
      await page.fill('main input[name="sc-dp-value"]', '0.2');
      await page.fill('main input[name="sc-dp-currency"]', 'dol');
      await page.keyboard.press('Enter');
      await page.waitForTimeout(700);
      const cur = await errorOf('sc-dp-currency');
      assert.deepEqual(sent, [], 'a currency that is none is not sent');
      assert.equal(cur.invalid, 'true');
      assert.match(cur.text, /currency/i);

      // one that is a currency, in small letters, is saved in capitals; today as the sold date is saved
      await page.fill('main input[name="sc-dp-currency"]', 'nok');
      const saved = page.waitForResponse((r) => r.request().method() === 'PATCH' && /\/cars\/c2\/settings$/.test(r.url()));
      await page.keyboard.press('Enter');
      await saved;
      await page.waitForTimeout(500);
      assert.equal(sent.length, 1);
      assert.deepEqual(JSON.parse(sent[0]).default_price_per_kwh, { value: 0.2, currency: 'NOK' });
      const today = await page.evaluate(() => document.querySelector('main input[name="sc-sold-date"]').max);
      await page.fill('main input[name="sc-sold-date"]', today);
      await page.getByRole('button', { name: 'Mark as sold' }).click();
      await page.waitForFunction(() => /Sold on/.test(document.querySelector('main').innerText));
      assert.deepEqual(JSON.parse(sent[1]), { version: JSON.parse(sent[1]).version, sold: true, sold_date: today });
      assert.deepEqual(errors, []);
    });
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: a period still to come gives Trip and Efficiency their empty pages, and the period picker takes no day before 1970`, { skip: SKIP, timeout: 240000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      const now = Date.parse((await context.request.get(`${origin}/api/v1/me`)).headers().date);   // the dev server's clock
      const DAY = 86400000;
      for (const [id, query] of [['trip', `?from=${now + 40 * DAY}&to=${now + 45 * DAY}`], ['efficiency', `?from=${now + 40 * DAY}&to=${now + 45 * DAY}`],
        ['trip', '?from=-2208960000000&to=1700000000000'], ['logbook', '?from=-2208960000000&to=1700000000000']]) {
        const refused = [];
        const { page, errors } = await openPage(context, `${origin}/cars/c1/${id}${query}`, null, (p) => p.on('response', (r) => { if (r.status() >= 400 && /\/api\//.test(r.url())) refused.push(`${r.status()} ${new URL(r.url()).pathname}`); }));
        await page.waitForLoadState('networkidle');
        assert.equal(await page.evaluate(() => document.documentElement.dataset.ready), id, `${id}${query} is drawn, not an error page`);
        assert.deepEqual(refused, [], `${id}${query} asks the server for nothing it refuses`);
        assert.deepEqual(errors, []);
        await page.close();
      }

      // the picker: a year mistyped as 1900 is said in the picker and the period stays
      const { page } = await openPage(context, `${origin}/cars/c1/drives?preset=7d`, 'drives');
      await page.locator('.topbar .period-btn').click();
      await page.fill('#period-from', '1900-01-01');
      await page.fill('#period-to', '2026-11-01');
      await page.locator('.period-custom button[type="submit"]').click();
      await page.waitForTimeout(700);
      assert.match(page.url(), /\?preset=7d$/, 'the period in the address stays');
      const said = await page.evaluate(() => {
        const from = document.getElementById('period-from');
        const err = document.querySelector('.period-custom .form-error');
        return { shown: from.checkVisibility(), native: from.validationMessage, own: err && !err.hidden ? err.textContent : '' };
      });
      assert.ok(said.shown, 'the picker stays open');
      assert.ok(said.native || /1970/.test(said.own), 'the picker says the day is too early');
    });
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: a search for one very long word that finds nothing leaves the page as wide as the window`, { skip: SKIP, timeout: 240000 }, async () => {
    for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }]) {
      await withPack(pack, [], async ({ origin, context }) => {
        for (const [route, id, label] of [['/cars/c1/logbook', 'logbook', 'Search the drives'], ['/cars/c1/health', 'health', 'Find a signal']]) {
          const { page, errors } = await openPage(context, origin + route, id);
          const box = page.getByLabel(label).filter({ visible: true }).first();
          await box.fill('x'.repeat(300));
          await box.press('Enter');
          await page.waitForFunction(() => [...document.querySelectorAll('main .empty p')].some((p) => p.checkVisibility() && /x{20}/.test(p.textContent)), null, { timeout: 15000 });
          await page.waitForTimeout(300);
          const at = await page.evaluate(() => {
            const note = [...document.querySelectorAll('main .empty p')].find((p) => p.checkVisibility() && /x{20}/.test(p.textContent));
            const r = note.getBoundingClientRect();
            return { page: document.documentElement.scrollWidth, window: document.documentElement.clientWidth, left: r.left, right: r.right, inside: note.scrollWidth <= note.clientWidth + 1 };
          });
          assert.ok(at.page <= at.window, `${route} at ${viewport.width} px is ${at.page} px wide in a window of ${at.window}`);
          assert.ok(at.left >= 0 && at.right <= at.window && at.inside, `${route} at ${viewport.width} px: the note runs from ${at.left} to ${at.right} and ${at.inside ? 'holds' : 'does not hold'} its words`);
          assert.deepEqual(errors, []);
          await page.close();
        }
      }, viewport);
    }
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: Battery of a car whose pack has reported nothing writes no sentence around a dash`, { skip: SKIP, timeout: 180000 }, async () => {
    await withPack(pack, ['--cars', '200'], async ({ origin, context }) => {
      // c4 is a car with no history: no pack reading, no lifetime counter
      const bare = await openPage(context, `${origin}/cars/c4/battery`, 'battery');
      await bare.page.waitForLoadState('networkidle');
      const text = await mainText(bare.page);
      assert.match(text, /Usable now/);
      assert.doesNotMatch(text, /— readings?\b/, 'no count of readings that is not known');
      assert.doesNotMatch(text, /— (driving|while parked)/, 'no energy out of the pack that is not known');
      assert.deepEqual(bare.errors, []);
      // a car that has them still says them
      const full = await openPage(context, `${origin}/cars/c1/battery`, 'battery');
      await full.page.waitForLoadState('networkidle');
      const said = await mainText(full.page);
      assert.match(said, /From the BMS: .*\([\d,.]+ readings\)\./);
      assert.match(said, /Out of the pack: [\d,.]+\s\S+ driving, [\d,.]+\s\S+ while parked\./);
    });
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: the car switcher of a large fleet counts the cars not sold, as the header does, and says where the sold ones are`, { skip: SKIP, timeout: 180000 }, async () => {
    await withPack(pack, ['--cars', '200'], async ({ origin, context }) => {
      const summary = await (await context.request.get(`${origin}/api/v1/fleet/summary`)).json();
      assert.ok(summary.sold >= 1 && summary.cars > 100, 'the made-up fleet has sold cars');
      const { page, errors } = await openPage(context, `${origin}/`, 'fleet');
      await page.keyboard.press('Control+k');
      const dialog = page.locator('dialog[open]');
      await dialog.locator('.sw-opt').first().waitFor();
      const count = await dialog.locator('.sw-group-label').first().innerText();
      assert.match(count.replace(/\s+/g, ' '), new RegExp(`\\b${summary.cars} in all`), `the switcher counts the ${summary.cars} cars not sold`);
      // the sold cars are not among the first ones listed: the toggle still names how many there are and where they are
      const toggle = dialog.locator('.sw-sold-toggle');
      assert.match(await toggle.innerText(), new RegExp(`Sold cars \\(${summary.sold}\\)`, 'i'));   // (drawn in capitals)
      await toggle.click();
      const where = dialog.locator('.sw-sold-more');
      await where.waitFor();
      assert.match(await where.innerText(), /Fleet/);
      assert.match(await where.locator('a').getAttribute('href'), /^\/\?(.*&)?sold=1/);
      assert.deepEqual(errors, []);
    });
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: Statistics on a server without a car does not count "0 cars"`, { skip: SKIP, timeout: 120000 }, async () => {
    await withPack(pack, ['--cars', '0'], async ({ origin, context }) => {
      const { page, errors } = await openPage(context, `${origin}/statistics`, 'statistics');
      await page.waitForLoadState('networkidle');
      const text = await mainText(page);
      assert.match(text, /No car yet/i);
      assert.doesNotMatch(text, /\b0 cars\b/);
      assert.deepEqual(errors, []);
    });
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: a Map or Satellite pick that could not be saved is said, and stays for the page`, { skip: SKIP, timeout: 180000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      const drive = (await (await context.request.get(`${origin}/api/v1/cars/c1/drives?limit=3`)).json()).items[1].id;
      const { page, errors } = await openPage(context, `${origin}/cars/c1/drives/${drive}`, 'drive', (p) => p.route(/\/api\/v1\/me\/preferences$/, (r) => (r.request().method() === 'PATCH'
        ? r.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify({ error: { code: 'server_error', message: 'The made-up server could not save this.' } }) }) : r.fallback())));
      const other = page.locator('main .map-style-btn[data-map-style]:not(.is-active)').first();
      await other.scrollIntoViewIfNeeded();
      const name = (await other.innerText()).trim();
      const satellite = page.locator(`main .map-style-btn[data-map-style="${await other.getAttribute('data-map-style')}"]`).first();
      await satellite.click();
      const note = page.locator('.flash-note[data-kind="error"]');
      await note.waitFor({ timeout: 10000 });
      const said = await note.innerText();
      assert.match(said, /map style/i);
      assert.match(said, /The made-up server could not save this\./, 'with the server\'s own reason');
      assert.equal(await satellite.getAttribute('aria-pressed'), 'true', `${name} stays picked on this page`);
      assert.deepEqual(errors, []);
    });
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: a page opened while the server is away says so inside the app, and Try again opens it once the server is back`, { skip: SKIP, timeout: 180000 }, async () => {
    const browser = await playwright.chromium.launch();
    let server = await devServer(pack);
    try {
      const origin = server.origin;
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
      await context.route(/^https?:\/\/(?!127\.0\.0\.1)/, (r) => r.abort());   // nothing leaves the machine
      const { page } = await openPage(context, `${origin}/`, 'fleet');
      await server.stop();
      server = null;
      // a page whose files this tab has not loaded yet
      await page.locator('a[href="/travel"]').filter({ visible: true }).first().click();
      await page.waitForFunction(() => /^error:/.test(document.documentElement.dataset.ready || ''), null, { timeout: 30000 }).catch(() => {});
      assert.ok(page.url().startsWith(origin), `the tab is still in the app, not at ${page.url()}`);
      assert.equal(new URL(page.url()).pathname, '/travel');
      assert.match(await mainText(page), /No answer from the server/);
      assert.ok(await page.locator('.sidebar, nav').first().isVisible(), 'the frame is still there');

      // the server comes back at the same address
      server = await devServer(pack, ['--port', new URL(origin).port]);
      assert.equal(server.origin, origin);
      await page.getByRole('button', { name: 'Try again' }).click();
      await page.waitForFunction(() => document.documentElement.dataset.ready === 'travel', null, { timeout: 45000 });
      assert.equal(new URL(page.url()).pathname, '/travel');
      await context.close();
    } finally {
      if (server) await server.stop();
      await browser.close();
    }
  });
}
