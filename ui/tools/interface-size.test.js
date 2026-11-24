// Pages at the Interface sizes a person can pick for a device (80 to 125), in Chromium on the dev API server:
// on a phone at 110 and 125 the export file's steps and its card's lines stay inside the card and off one another,
// and at 80 the Travel map's legend stays inside the map.
// Each test names the packs it runs on; `PACKS=a,b` runs only those.
// Run: node --test ui/tools/interface-size.test.js. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawn } = require('child_process');

const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }

const { BUILTIN_PACKS } = require('./builtin-packs');
const ONLY = process.env.PACKS ? new Set(process.env.PACKS.split(',')) : null;
// a pack named here that is not built in (ui/tools/builtin-packs.js) drops out
const packs = (...list) => list.filter((p) => BUILTIN_PACKS.includes(p) && (!ONLY || ONLY.has(p)));
const BUILT_ON_INSTRUMENT = ['instrument', 'horizon', 'manager'];
const SKIP = !playwright && 'Playwright is not installed';
const PHONE = { width: 390, height: 844 };

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

/** Runs `body({ origin, browser })` with a dev server for the pack. */
async function withPack(pack, body) {
  const browser = await playwright.chromium.launch();
  const server = await devServer(pack);
  try {
    await body({ origin: server.origin, browser });
  } finally {
    await server.stop();
    await browser.close();
  }
}

/** Opens `route` in a new window of that size with that Interface size on the device, and waits until it is ready. */
async function openAt(browser, origin, route, id, viewport, deviceSize) {
  const context = await browser.newContext({ viewport });
  // nothing leaves the machine: a request to another host gets a 1 x 1 picture
  await context.route(/^https?:\/\/(?!127\.0\.0\.1)/, (r) => r.fulfill({ status: 200, contentType: 'image/png',
    body: Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==', 'base64') }));
  const page = await context.newPage();
  await page.addInitScript((n) => { try { localStorage.setItem('sentrymate.device-size', JSON.stringify(n)); } catch { /* opaque origin */ } }, deviceSize);
  await page.goto(origin + route);
  await page.waitForFunction((id) => document.documentElement.dataset.ready === id, id, { timeout: 30000 });
  await page.waitForLoadState('networkidle', { timeout: 20000 }).catch(() => {});
  return { page, context };
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: on a phone at Interface size 110 and 125 the export file's steps and lines stay inside the card, apart`, { skip: SKIP, timeout: 180000 }, async () => {
    await withPack(pack, async ({ origin, browser }) => {
      for (const size of [100, 110, 125]) {
        const { page, context } = await openAt(browser, origin, '/settings/server', 'settings-server', PHONE, size);
        const xp = page.locator('.xp').first();
        await xp.waitFor({ state: 'attached', timeout: 15000 });
        await xp.scrollIntoViewIfNeeded();
        const got = await xp.evaluate((card) => {
          const textBox = (e) => { const r = document.createRange(); r.selectNodeContents(e); return r.getBoundingClientRect(); };
          const c = card.getBoundingClientRect();
          const inner = { left: c.left + card.clientLeft, right: c.right - card.clientLeft };
          const out = [...card.querySelectorAll('*')].filter((e) => { const b = e.getBoundingClientRect(); return b.width > 0 && (b.right > inner.right + 0.5 || b.left < inner.left - 0.5); })
            .map((e) => `${e.tagName.toLowerCase()}.${String(e.className.baseVal ?? e.className)}`);
          const parts = [...card.querySelectorAll('.xp-step > *')].map((e) => ({ t: e.textContent, b: textBox(e) }));
          const over = [];
          for (let i = 0; i < parts.length; i++) for (let j = i + 1; j < parts.length; j++) {
            const a = parts[i].b, b = parts[j].b;
            if (Math.min(a.right, b.right) - Math.max(a.left, b.left) > 0.5 && Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top) > 0.5) over.push(`${parts[i].t} / ${parts[j].t}`);
          }
          return { steps: parts.length, out, over };
        });
        assert.equal(got.steps, 6, 'three steps, each a number and a word');
        assert.deepEqual(got.out, [], `size ${size}: nothing sticks out of the export card`);
        assert.deepEqual(got.over, [], `size ${size}: no step lies on another`);
        await context.close();
      }
    });
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: on a phone at Interface size 80 the Travel map's legend stays inside the map`, { skip: SKIP, timeout: 180000 }, async () => {
    await withPack(pack, async ({ origin, browser }) => {
      for (const size of [80, 100]) {
        const { page, context } = await openAt(browser, origin, '/travel', 'travel', PHONE, size);
        const legend = page.locator('.tv-map-box .tv-legend');
        await legend.waitFor({ timeout: 15000 });
        const got = await legend.evaluate((l) => {
          const box = l.closest('.tv-map-box').getBoundingClientRect(), b = l.getBoundingClientRect();
          const help = document.querySelector('.tv-map-help').getBoundingClientRect();
          return { below: b.bottom - box.bottom, overHelp: b.bottom - help.top, cars: l.querySelectorAll('.tv-line').length };
        });
        assert.ok(got.cars >= 2, 'the legend lists the cars');
        assert.ok(got.below <= 0 && got.overHelp <= 0, `size ${size}: the legend ends ${got.below} px below the map and ${got.overHelp} px into the note under it`);
        // every control of the legend can still be reached: the switch between cities and places scrolls into view
        const places = legend.locator('.seg-btn', { hasText: 'Places' });
        await places.scrollIntoViewIfNeeded();
        await places.click();
        assert.equal(await places.getAttribute('aria-pressed'), 'true');
        await context.close();
      }
    });
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: at Interface size 80 on a desktop no unit and no code text is under 10 px`, { skip: SKIP, timeout: 180000 }, async () => {
    await withPack(pack, async ({ origin, browser }) => {
      for (const [route, id] of [['/cars/c1/logbook', 'logbook'], ['/cars/c1/software', 'software'], ['/cars/c1/health', 'health'], ['/system', 'health']]) {
        const { page, context } = await openAt(browser, origin, route, id, { width: 1440, height: 900 }, 80);
        await page.locator('main .q-u, main .mono').first().waitFor({ state: 'attached', timeout: 15000 });
        const small = await page.evaluate(() => {
          const out = [];
          const walk = document.createTreeWalker(document.querySelector('main'), NodeFilter.SHOW_TEXT);
          for (let n = walk.nextNode(); n; n = walk.nextNode()) {
            const e = n.parentElement;
            if (!n.nodeValue.trim() || !e.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true }) || e.closest('svg')) continue;
            const px = parseFloat(getComputedStyle(e).fontSize);
            if (px < 10) out.push(`${px} px ${e.tagName.toLowerCase()}.${e.className} "${n.nodeValue.trim().slice(0, 30)}"`);
          }
          return out;
        });
        assert.deepEqual(small.slice(0, 5), [], `${route}: ${small.length} texts under 10 px`);
        await context.close();
      }
    });
  });
}

/** The texts of `selector` whose words reach out of the left or right of the box of their closest `boxSelector` (or of the window), as "text: N px". */
const stickingOut = (page, selector, boxSelector) => page.evaluate(([selector, boxSelector]) => {
  const out = [];
  for (const e of document.querySelectorAll(selector)) {
    if (!e.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true })) continue;
    const range = document.createRange(); range.selectNodeContents(e);
    const t = range.getBoundingClientRect();
    const box = boxSelector ? e.closest(boxSelector).getBoundingClientRect() : { left: 0, right: document.documentElement.clientWidth };
    const by = Math.max(box.left - t.left, t.right - box.right);
    if (by > 0.5) out.push(`${e.textContent.trim().slice(0, 30)}: ${Math.round(by * 10) / 10} px`);
  }
  return out;
}, [selector, boxSelector]);

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: on a phone at Interface size 125 Charges does not scroll sideways, and the Logbook's counts and the Drives' figures stay in their boxes`, { skip: SKIP, timeout: 240000 }, async () => {
    await withPack(pack, async ({ origin, browser }) => {
      let o = await openAt(browser, origin, '/cars/c1/charges', 'charges', PHONE, 125);
      await o.page.locator('#habits .panel-side').waitFor({ timeout: 15000 });
      const wide = await o.page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
      assert.ok(wide <= 0, `Charges is ${wide} px wider than the window`);
      assert.deepEqual(await stickingOut(o.page, '#habits .panel-side > *', null), [], 'the charging habits\' controls end inside the window');
      await o.context.close();

      o = await openAt(browser, origin, '/cars/c1/logbook', 'logbook', PHONE, 125);
      await o.page.locator('.lb-seg .seg-btn .count').first().waitFor({ timeout: 15000 });
      assert.deepEqual(await stickingOut(o.page, '.lb-seg .seg-btn', '.lb-seg'), [], 'every purpose and its count is inside the filter');
      await o.context.close();

      o = await openAt(browser, origin, '/cars/c1/drives', 'drives', PHONE, 125);
      await o.page.locator('.pattern-grid .metric-value').first().waitFor({ timeout: 15000 });
      assert.deepEqual(await stickingOut(o.page, '.pattern-grid .metric-value', '.metric'), [], 'every pattern figure is inside its column');
      await o.context.close();
    });
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: on a phone at Interface size 80 the first-start panel's labels, the charge level labels and the overview's units are whole`, { skip: SKIP, timeout: 240000 }, async () => {
    await withPack(pack, async ({ origin, browser }) => {
      let o = await openAt(browser, origin, '/cars/c1/health', 'health', PHONE, 80);
      await o.page.locator('.fr-line dt').first().waitFor({ timeout: 15000 });
      // (a label is one line, cut with an ellipsis when too long: only its height is checked)
      const cut = await o.page.evaluate(() => [...document.querySelectorAll('.fr-line dt')].filter((e) => e.scrollHeight > e.clientHeight + 0.5).map((e) => `${e.textContent}: ${e.scrollHeight} px of text in ${e.clientHeight} px`));
      assert.deepEqual(cut, [], 'no label of the panel is cut at the top or bottom');
      await o.context.close();

      o = await openAt(browser, origin, '/cars/c1/charges', 'charges', PHONE, 80);
      await o.page.locator('.socmap-y').first().waitFor({ timeout: 15000 });
      assert.deepEqual(await stickingOut(o.page, '.socmap-y', '.socmap-y'), [], 'every charge level label is inside its column');
      await o.context.close();

      o = await openAt(browser, origin, '/cars/c1', 'car', PHONE, 80);
      await o.page.locator('main .q-u').first().waitFor({ timeout: 15000 });
      const small = await o.page.evaluate(() => [...document.querySelectorAll('main .q-u')].filter((e) => e.checkVisibility()).map((e) => parseFloat(getComputedStyle(e).fontSize)).filter((px) => px < 10));
      assert.deepEqual(small, [], 'no unit of the overview is under 10 px');
      await o.context.close();
    });
  });
}

/** The texts of `selector` whose words reach out of the top or bottom of their own box, as "text: N px". */
const tallerThanBox = (page, selector) => page.evaluate((selector) => {
  const out = [];
  for (const e of document.querySelectorAll(selector)) {
    if (!e.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true }) || !e.textContent.trim()) continue;
    const range = document.createRange(); range.selectNodeContents(e);
    const t = range.getBoundingClientRect(), b = e.getBoundingClientRect();
    const by = Math.max(b.top - t.top, t.bottom - b.bottom);
    if (by > 0.5) out.push(`${e.textContent.trim().slice(0, 30)}: ${Math.round(by * 10) / 10} px`);
  }
  return [...new Set(out)];
}, selector);

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: on a phone the charge page's Download CSV stands clear of "Power against battery %"`, { skip: SKIP, timeout: 240000 }, async () => {
    await withPack(pack, async ({ origin, browser }) => {
      for (const size of [100, 80, 125]) {
        const first = await openAt(browser, origin, '/cars/c1/charges', 'charges', PHONE, size);
        const id = await first.page.evaluate(async () => (await (await fetch('/api/v1/cars/c1/charges?limit=1')).json()).items[0].id);
        await first.context.close();
        const o = await openAt(browser, origin, `/cars/c1/charges/${id}`, 'charge', PHONE, size);
        const head = o.page.locator('.charts-panel .panel-head', { hasText: 'Power against battery' });
        await head.waitFor({ timeout: 15000 });
        const over = await head.evaluate((el) => {
          const words = (e) => { const r = document.createRange(); r.selectNodeContents(e); return r.getBoundingClientRect(); };
          const t = words(el.querySelector('.panel-title')), b = el.querySelector('.row .btn').getBoundingClientRect();
          return { x: Math.min(t.right, b.right) - Math.max(t.left, b.left), y: Math.min(t.bottom, b.bottom) - Math.max(t.top, b.top), title: t.width, button: b.width };
        });
        assert.ok(over.title > 0 && over.button > 0, 'the heading and the button are drawn');
        assert.ok(over.x <= 0 || over.y <= 0, `at Interface size ${size} the button covers ${Math.round(over.x)} x ${Math.round(over.y)} px of the heading`);
        await o.context.close();
      }
    });
  });

  test(`${pack}: on a phone at Interface size 80 and 90 a tag's text, the charging week's labels and Travel's ranks are inside their boxes`, { skip: SKIP, timeout: 300000 }, async () => {
    await withPack(pack, async ({ origin, browser }) => {
      for (const size of [80, 90]) {
        for (const [route, id] of [['/settings/cars', 'settings-cars'], ['/cars/c1/software', 'software'], ['/settings/integrations', 'settings-integrations']]) {
          const o = await openAt(browser, origin, route, id, PHONE, size);
          await o.page.locator('main .tag:visible').first().waitFor({ timeout: 15000 });
          assert.deepEqual(await tallerThanBox(o.page, 'main .tag'), [], `${route} at ${size}: every tag is as high as its text`);
          await o.context.close();
        }
        let o = await openAt(browser, origin, '/cars/c1/charges', 'charges', PHONE, size);
        await o.page.locator('.weekmap-x').first().waitFor({ timeout: 15000 });
        assert.deepEqual(await tallerThanBox(o.page, '.weekmap-x'), [], `at ${size} no hour label of the charging week is cut at the top or bottom`);
        assert.deepEqual(await stickingOut(o.page, '.weekmap-y', '.weekmap-y'), [], `at ${size} every weekday of the charging week is inside its column`);
        await o.context.close();

        o = await openAt(browser, origin, '/travel', 'travel', PHONE, size);
        await o.page.locator('.tv-bar-rank').first().waitFor({ timeout: 15000 });
        assert.deepEqual(await stickingOut(o.page, '.tv-bar-rank', '.tv-bar-rank'), [], `at ${size} every rank of Travel's lists is inside its column`);
        await o.context.close();
      }
    });
  });
}
