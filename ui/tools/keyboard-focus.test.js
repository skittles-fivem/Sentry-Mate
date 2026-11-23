// Keyboard focus in Chromium on the dev API server: focus is never dropped to the page's <body>. A dialog gives
// the focus back to the button that opened it (also one that was disabled while its request ran), the account
// menu takes the focus when it opens and gives it back when it closes, and a car's live updates leave the
// focused control where it is.
// Each test names the packs it runs on; `PACKS=a,b` runs only those.
// Run: node --test ui/tools/keyboard-focus.test.js. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawn } = require('child_process');

const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }

const ONLY = process.env.PACKS ? new Set(process.env.PACKS.split(',')) : null;
const packs = (...list) => list.filter((p) => !ONLY || ONLY.has(p));
const BUILT_ON_INSTRUMENT = ['instrument', 'horizon', 'manager'];
const SKIP = !playwright && 'Playwright is not installed';

function devServer(pack, extra = [], freeze = true) {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', path.join(UI_DIR, 'packs', pack),
      '--port', '0', ...(freeze ? ['--freeze-clock'] : []), ...extra], { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
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

/** Runs `body({ origin, context })` with a dev server for the pack (its clock stopped unless freeze is false) and a browser window of that size. */
async function withPack(pack, extra, body, { viewport = { width: 1440, height: 900 }, freeze = true } = {}) {
  const browser = await playwright.chromium.launch();
  const server = await devServer(pack, extra, freeze);
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

/** What has the focus: "BODY", or the element's tag and its text or label. */
const focused = (page) => page.evaluate(() => {
  const a = document.activeElement;
  return !a || a === document.body ? 'BODY' : `${a.tagName}:${(a.getAttribute('aria-label') || a.textContent || a.name || '').trim().replace(/\s+/g, ' ').slice(0, 40)}`;
});

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: a dialog opened by a button that is disabled while its request runs gives the focus back to that button`, { skip: SKIP, timeout: 180000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      for (const [route, id, name] of [['/settings/cars/c1', 'settings-cars', 'Make upload token'], [`/settings/integrations?tab=${pack === 'instrument' ? 'details' : 'setup'}`, 'settings-integrations', 'Sign in at Tesla'],
        ...(pack === 'instrument' ? [['/settings/integrations?tab=details', 'settings-integrations', 'Save']] : [])]) {
        const { page, errors } = await openPage(context, origin + route, id);
        const button = page.locator('main button', { hasText: new RegExp(`^\\s*${name}\\s*$`) }).first();
        await button.waitFor({ timeout: 15000 });
        await button.focus();
        await page.keyboard.press('Enter');
        await page.locator('dialog[open]').waitFor({ timeout: 15000 });
        assert.ok(await page.evaluate(() => document.querySelector('dialog[open]').contains(document.activeElement)), `${name}: the focus is in the dialog`);
        await page.keyboard.press('Escape');
        await page.locator('dialog[open]').waitFor({ state: 'detached', timeout: 5000 });
        await page.waitForTimeout(200);
        assert.equal(await focused(page), `BUTTON:${name}`, `${route}: after Esc the focus is back on "${name}"`);
        assert.deepEqual(errors, []);
        await page.close();
      }
    });
  });
}

const LIVE_PARTS = '.now-head, .now-inputs, .map-tools, .now-panels, .today-tl, .window-head';
for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: the car overview's live updates leave the focused control in place`, { skip: SKIP, timeout: 240000 }, async () => {
    // live values every 0.5 s (the page draws them every 2 s) and a list change every 3 s
    await withPack(pack, ['--sse-interval-ms', '500', '--rows-every-s', '3'], async ({ origin, context }) => {
      const { page, errors } = await openPage(context, `${origin}/cars/c1`, 'car');
      await page.waitForLoadState('networkidle', { timeout: 20000 }).catch(() => {});
      // the controls of the parts drawn again on live updates, by their text
      const names = await page.evaluate((sel) => {
        window.__seen = [...document.querySelectorAll(sel)].flatMap((p) => [...p.querySelectorAll('a[href], button')]).filter((e) => e.getBoundingClientRect().width > 0);
        return window.__seen.map((e) => (e.getAttribute('aria-label') || e.textContent).trim().replace(/\s+/g, ' '));
      }, LIVE_PARTS);
      for (const want of ['Switch car', 'Timeline', 'Full screen', ...(pack === 'instrument' ? ['Change order'] : [])]) assert.ok(names.includes(want), `the page has "${want}" (${names.join(' | ')})`);
      // each is focused for longer than a live update and a list change take, and must still have the focus
      for (const name of [...new Set(names)]) {
        const ok = await page.evaluate(async ({ sel, name }) => {
          const label = (e) => (e.getAttribute('aria-label') || e.textContent).trim().replace(/\s+/g, ' ');
          const el = [...document.querySelectorAll(sel)].flatMap((p) => [...p.querySelectorAll('a[href], button')]).find((e) => e.getBoundingClientRect().width > 0 && label(e) === name);
          if (!el) return 'gone before it was focused';
          el.focus({ preventScroll: true });
          await new Promise((r) => setTimeout(r, 5200));
          const a = document.activeElement;
          return a && a !== document.body && label(a) === name ? true : `the focus is on ${a === document.body ? '<body>' : `${a.tagName} "${label(a)}"`}`;
        }, { sel: LIVE_PARTS, name });
        assert.equal(ok, true, `"${name}" focused through live updates`);
      }
      // a click on Full screen (which draws the map's tools again) keeps the focus on a control
      await page.locator('.map-tools button').first().click();
      await page.waitForTimeout(700);
      assert.notEqual(await focused(page), 'BODY', 'after a click on Full screen the focus is not dropped');
      assert.deepEqual(errors, []);
    }, { freeze: false });
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: the account menu takes the focus when it opens, keeps Tab inside and gives the focus back when it closes`, { skip: SKIP, timeout: 120000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      const { page, errors } = await openPage(context, `${origin}/cars/c1/drives`, 'drives');
      const button = page.locator('button[aria-haspopup="dialog"][aria-label*="account" i], button.account').locator('visible=true').first();
      await button.focus();
      await page.keyboard.press('Enter');
      await page.locator('.account-menu:popover-open').waitFor({ timeout: 5000 });
      const inMenu = () => page.evaluate(() => !!document.activeElement?.closest('.account-menu'));
      assert.ok(await inMenu(), `the focus is in the menu once it is open (it is on ${await focused(page)})`);
      await button.evaluate((b) => new Promise((r) => { const t = setInterval(() => { if (b.getAttribute('aria-expanded') === 'true') { clearInterval(t); r(); } }, 20); setTimeout(() => { clearInterval(t); r(); }, 2000); }));
      assert.equal(await button.getAttribute('aria-expanded'), 'true');
      // Tab goes through the menu's own controls and reaches Sign out, never the page behind
      const controls = await page.locator('.account-menu').locator('a[href], button, input, select, [tabindex="0"]').locator('visible=true').count();
      let reached = false;
      for (let i = 0; i < controls + 2 && !reached; i++) {
        await page.keyboard.press('Tab');
        assert.ok(await inMenu(), `Tab ${i + 1} stays in the open menu (the focus is on ${await focused(page)})`);
        reached = (await focused(page)) === 'BUTTON:Sign out';
      }
      assert.ok(reached, 'Tab reaches Sign out');
      await page.keyboard.press('Tab');
      assert.ok(await inMenu(), 'Tab after the last control goes back to the first');
      await page.keyboard.press('Shift+Tab');
      assert.equal(await focused(page), 'BUTTON:Sign out', 'Shift+Tab from the first control goes to the last');
      await page.keyboard.press('Escape');
      await page.locator('.account-menu:popover-open').waitFor({ state: 'detached', timeout: 5000 }).catch(() => {});
      await page.waitForTimeout(100);
      assert.equal(await page.locator('.account-menu:popover-open').count(), 0, 'Escape closes the menu');
      assert.ok(await button.evaluate((b) => document.activeElement === b), `after Escape the focus is back on the account button (it is on ${await focused(page)})`);
      assert.equal(await button.getAttribute('aria-expanded'), 'false');
      assert.deepEqual(errors, []);
    });
  });
}
