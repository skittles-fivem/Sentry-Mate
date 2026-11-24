// Keyboard focus in Chromium on the dev API server: focus is never dropped to the page's <body>. A dialog gives
// the focus back to the button that opened it (also one that was disabled while its request ran), the account
// menu takes the focus when it opens and gives it back when it closes, a car's live updates leave the
// focused control where it is, one dialog is open at a time, and a page that builds itself again for its own
// address (Places' editor) or draws its list again (the Logbook after a purpose is saved) keeps the focus.
// Each test names the packs it runs on; `PACKS=a,b` runs only those.
// Run: node --test ui/tools/keyboard-focus.test.js. Skipped when Playwright is not installed.
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

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: the car switcher's key opens no second dialog over the page finder, and the focus comes back`, { skip: SKIP, timeout: 120000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      const { page } = await openPage(context, `${origin}/cars/c1/drives`, 'drives');
      await page.locator('main a[href], main button').first().focus();
      const before = await focused(page);
      assert.notEqual(before, 'BODY');
      await page.keyboard.press('/');
      await page.waitForFunction(() => document.querySelectorAll('dialog[open]').length === 1, null, { timeout: 5000 });
      await page.keyboard.press('Control+k');
      await page.waitForTimeout(500);
      assert.equal(await page.evaluate(() => document.querySelectorAll('dialog[open]').length), 1, 'one dialog at a time');
      await page.keyboard.press('Escape');
      await page.waitForFunction(() => !document.querySelector('dialog[open]'), null, { timeout: 5000 });
      assert.equal(await focused(page), before, 'the focus is back where it was');
      // the other way round: the finder's key types into the switcher's search, it opens nothing
      await page.keyboard.press('Control+k');
      await page.waitForFunction(() => document.querySelectorAll('dialog[open]').length === 1, null, { timeout: 5000 });
      await page.keyboard.press('Control+k');
      await page.waitForTimeout(300);
      assert.equal(await page.evaluate(() => document.querySelectorAll('dialog[open]').length), 1);
      await page.keyboard.press('Escape');
      await page.waitForFunction(() => !document.querySelector('dialog[open]'), null, { timeout: 5000 });
      assert.equal(await focused(page), before);
    });
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: opening the place editor, saving a place and saving a drive's purpose leave the focus on the page`, { skip: SKIP, timeout: 180000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      const inView = (page) => page.evaluate(() => { const a = document.activeElement; return !!a && a !== document.body && document.querySelector('main').contains(a); });
      const { page, errors } = await openPage(context, `${origin}/places`, 'places');
      const add = page.locator('main a', { hasText: 'New place' }).first();
      await add.focus();
      await page.keyboard.press('Enter');
      await page.waitForFunction(() => location.search.includes('new=1') && document.documentElement.dataset.ready === 'places' && document.querySelector('main input[name="name"]'), null, { timeout: 30000 });
      await page.waitForTimeout(300);
      assert.ok(await inView(page), `the editor has the focus, not the page's body (${await focused(page)})`);
      await page.fill('main input[name="name"]', 'Depot');
      await page.locator('main button[type="submit"]').first().focus();
      await page.keyboard.press('Enter');
      await page.waitForFunction(() => !location.search.includes('new=1') && document.documentElement.dataset.ready === 'places', null, { timeout: 30000 });
      await page.waitForTimeout(300);
      assert.ok(await inView(page), `back on the list the focus is on the page (${await focused(page)})`);
      assert.deepEqual(errors, []);
      await page.close();

      const lb = await openPage(context, `${origin}/cars/c1/logbook`, 'logbook');
      const edit = lb.page.locator('main .lb-edit').first();
      await edit.focus();
      const label = await focused(lb.page);
      await lb.page.keyboard.press('Enter');
      await lb.page.waitForSelector('dialog[open] select[name="purpose"]');
      await lb.page.selectOption('dialog[open] select[name="purpose"]', 'private');
      const saved = lb.page.waitForResponse((r) => r.request().method() === 'PUT' && /\/purpose$/.test(new URL(r.url()).pathname));
      await lb.page.locator('dialog[open] button[type="submit"]').click();
      await saved;
      await lb.page.waitForFunction(() => !document.querySelector('dialog[open]'), null, { timeout: 5000 });
      await lb.page.waitForTimeout(1500); // the list is read and drawn again
      assert.equal(await focused(lb.page), label, 'the focus is on the same drive\'s button in the list drawn again');
      assert.deepEqual(lb.errors, []);
    });
  });
}

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: one Esc closes the page finder with text typed in it`, { skip: SKIP, timeout: 120000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      const { page, errors } = await openPage(context, `${origin}/cars/c1/drives`, 'drives');
      await page.keyboard.press('/');
      await page.locator('dialog.menu-panel[open]').waitFor({ timeout: 5000 });
      await page.keyboard.type('zzz');
      await page.waitForTimeout(200);
      assert.match(await page.locator('dialog.menu-panel .sw-foot').innerText(), /Esc\s*close/, 'the finder says Esc closes it');
      await page.keyboard.press('Escape');
      await page.waitForTimeout(400);
      assert.equal(await page.locator('dialog[open]').count(), 0, 'the finder is closed after one Esc');
      // opened again it starts empty
      await page.keyboard.press('/');
      await page.locator('dialog.menu-panel[open]').waitFor({ timeout: 5000 });
      assert.equal(await page.evaluate(() => document.activeElement.value), '');
      assert.deepEqual(errors, []);
    });
  });

  test(`${pack}: a save in a dialog that the server refuses leaves the focus in the dialog, on its button or the wrong field`, { skip: SKIP, timeout: 180000 }, async () => {
    await withPack(pack, [], async ({ origin, context }) => {
      const { page, errors } = await openPage(context, `${origin}/settings/server`, 'settings-server');
      const add = page.locator('main button', { hasText: /^\s*Add account\s*$/ }).first();
      await add.waitFor({ timeout: 15000 });
      await add.click();
      const dlg = page.locator('dialog[open]');
      await dlg.waitFor({ timeout: 5000 });
      await dlg.locator('[name=name]').fill('viewer-nine');
      await dlg.locator('[name=password]').fill('correct horse battery staple 9');
      await dlg.locator('[name=confirm_password]').fill('sentry-mate-dev');
      // (the last one is said under the name's field by the page: that field is wrong, so it takes the focus)
      for (const [status, body, want] of [[500, { error: { code: 'internal', message: 'Injected.' } }, 'BUTTON:Add account'], [403, { error: { code: 'forbidden', message: 'Admins only.' } }, 'BUTTON:Add account'],
        [409, { error: { code: 'name_taken', message: 'That name is taken.' } }, 'INPUT:name']]) {
        await page.route('**/api/v1/accounts', (r) => (r.request().method() === 'POST' ? r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) }) : r.continue()));
        const submit = dlg.locator('button[type=submit]');
        await submit.focus();
        const answered = page.waitForResponse((r) => r.request().method() === 'POST' && new URL(r.url()).pathname === '/api/v1/accounts');
        await page.keyboard.press('Enter');
        await answered;
        await dlg.getByText(body.error.message).waitFor({ timeout: 5000 });
        await page.waitForTimeout(200);
        assert.equal(await focused(page), want, `after ${status} the focus is on the dialog's button, or on the field that is wrong`);
        assert.ok(await page.evaluate(() => document.querySelector('dialog[open]').contains(document.activeElement)));
        await page.unroute('**/api/v1/accounts');
      }
      // a field the server names takes the focus itself
      await page.route('**/api/v1/accounts', (r) => (r.request().method() === 'POST' ? r.fulfill({ status: 422, contentType: 'application/json', body: JSON.stringify({ error: { code: 'invalid', message: 'Some values are not valid.', fields: { name: 'That name is taken.' } } }) }) : r.continue()));
      await dlg.locator('button[type=submit]').focus();
      await page.keyboard.press('Enter');
      await page.waitForFunction(() => document.activeElement?.name === 'name', null, { timeout: 5000 });
      assert.deepEqual(errors, []);
    });
  });

  test(`${pack}: each step of the first-run set-up takes the keyboard focus`, { skip: SKIP, timeout: 180000 }, async () => {
    await withPack(pack, ['--fresh'], async ({ origin, context }) => {
      const { page, errors } = await openPage(context, `${origin}/`);
      const inCard = () => page.evaluate(() => { const a = document.activeElement; return !!a && a !== document.body && !!a.closest('main'); });
      const code = page.locator('main input[name=setup_code]');
      await code.waitFor({ timeout: 15000 });
      // a wrong code: the focus stays with the field that is wrong
      await code.fill('SETUP-WRONG-1');
      await page.locator('main button[type=submit]').focus();
      await page.keyboard.press('Enter');
      await page.locator('main .form-error:not([hidden])').first().waitFor({ timeout: 5000 });
      await page.waitForTimeout(200);
      assert.equal(await page.evaluate(() => document.activeElement?.name), 'setup_code', 'a wrong code leaves the focus in its field');
      await code.fill('SETUP-DEV-0000');
      await page.locator('main button[type=submit]').focus();
      await page.keyboard.press('Enter');
      await page.locator('main h1', { hasText: 'How should this server start?' }).waitFor({ timeout: 10000 });
      await page.waitForTimeout(200);
      assert.ok(await inCard(), `the choice of how to start has the focus, not the page's body (${await focused(page)})`);
      await page.locator('main button.su-choice', { hasText: 'Start fresh' }).focus();
      await page.keyboard.press('Enter');
      await page.locator('main input[name=name]').waitFor({ timeout: 10000 });
      assert.equal(await page.evaluate(() => document.activeElement?.name), 'name');
      await page.fill('main input[name=name]', 'admin2');
      await page.fill('main input[name=password]', 'correct horse battery staple 9');
      await page.fill('main input[name=again]', 'correct horse battery staple 9');
      await page.locator('main button[type=submit]').focus();
      await page.keyboard.press('Enter');
      await page.locator('main h1', { hasText: 'Save your recovery key' }).waitFor({ timeout: 15000 });
      await page.waitForTimeout(200);
      assert.ok(await inCard(), `the recovery key's step has the focus (${await focused(page)})`);
      assert.deepEqual(errors, []);
    });
  });
}
