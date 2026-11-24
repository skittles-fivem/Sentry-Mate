// Recovery codes and the two-step set-up wall, in the Instrument pack, in Chromium on the dev API server:
//   - a recovery code (xxxx-xxxx-xxxx-xxxx, 19 characters, with or without dashes and spaces) can be typed in every
//     field that asks for a two-step code: the sign-in's second step, the set-up and the changes that ask again;
//     six digits still lose their spaces, a recovery code is sent as typed;
//   - when the rule asks every account for two-step sign-in and this account has none (GET /me says
//     two_step_setup_only, or any call answers 403 two_step_setup), the app shows one page that explains it, offers
//     the turn-on flow and Sign out, and once it is on loads the normal app.
// Run: node --test ui/tools/two_step_recovery.test.js. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawn } = require('child_process');

const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }

const OWN = 'sentry-mate-dev';
const CODE = '123456';
const DEV_CODE = '000000';
const ALEX_CODE = 'A3K7 M2P9 Q4R8 S5T6'; // one of alex's made-up recovery codes, typed in capitals with spaces
const SHAPE = /^[0-9a-hjkmnp-tv-z]{4}(-[0-9a-hjkmnp-tv-z]{4}){3}$/;
const SKIP = !playwright && 'Playwright is not installed';

function devServer(extra = []) {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', path.join(UI_DIR, 'packs', 'instrument'),
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

async function withServer(extra, body) {
  const server = await devServer(extra);
  const browser = await playwright.chromium.launch();
  try {
    const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));
    await body({ origin: server.origin, page });
    assert.deepEqual(errors, []);
    await context.close();
  } finally {
    await browser.close();
    await server.stop();
  }
}

const ready = (page, id) => page.waitForFunction((x) => document.documentElement.dataset.ready === x, id, { timeout: 30000 });
const answered = (page, method, p) => page.waitForResponse((r) => r.request().method() === method && new URL(r.url()).pathname === p);
const putRule = (page) => page.evaluate(async (pw) => (await fetch('/api/v1/two-step-policy', { method: 'PUT',
  headers: { 'X-Sentry-Mate': '1', 'Content-Type': 'application/json' }, body: JSON.stringify({ everyone: true, confirm_password: pw }) })).status, OWN);

test('sign-in: a recovery code is typed in the second step, once', { skip: SKIP, timeout: 120000 }, async () => {
  await withServer(['--signed-out'], async ({ origin, page }) => {
    const second = async () => {
      await page.goto(`${origin}/sign-in`);
      await ready(page, 'sign-in');
      await page.fill('input[name=name]', 'alex');
      await page.fill('input[name=password]', OWN);
      await page.keyboard.press('Enter');
      await page.locator('input[name=code]').waitFor({ timeout: 5000 });
    };
    await second();
    const attrs = await page.locator('input[name=code]').evaluate((e) => ({ max: e.maxLength, pattern: e.pattern }));
    assert.ok(attrs.max >= 24 || attrs.max < 0, `a recovery code (19 characters) fits: maxlength ${attrs.max}`);
    assert.equal(attrs.pattern, '', 'no digits-only pattern');
    await page.fill('input[name=code]', ALEX_CODE);
    assert.equal((await page.inputValue('input[name=code]')).length, ALEX_CODE.length, 'nothing cut off');
    const sent = answered(page, 'POST', '/api/v1/session/two-step');
    await page.keyboard.press('Enter');
    const r = await sent;
    assert.equal(r.status(), 200);
    assert.equal(JSON.parse(r.request().postData()).code, ALEX_CODE, 'a recovery code goes as typed');
    await page.waitForFunction(() => location.pathname !== '/sign-in', null, { timeout: 10000 });
    // it works once
    await page.evaluate(() => fetch('/api/v1/session', { method: 'DELETE', headers: { 'X-Sentry-Mate': '1' } }));
    await second();
    await page.fill('input[name=code]', ALEX_CODE);
    const again = answered(page, 'POST', '/api/v1/session/two-step');
    await page.keyboard.press('Enter');
    assert.equal((await again).status(), 401);
    await page.waitForFunction(() => document.querySelector('input[name=code]')?.getAttribute('aria-invalid') === 'true', null, { timeout: 5000 });
    // six digits lose their spaces
    await page.fill('input[name=code]', '000 000');
    const digits = answered(page, 'POST', '/api/v1/session/two-step');
    await page.keyboard.press('Enter');
    const d = await digits;
    assert.equal(JSON.parse(d.request().postData()).code, DEV_CODE);
    assert.equal(d.status(), 200);
  });
});

test('settings: every code field takes a recovery code', { skip: SKIP, timeout: 180000 }, async () => {
  await withServer([], async ({ origin, page }) => {
    const dlg = page.locator('dialog[open]');
    const closed = () => page.waitForFunction(() => !document.querySelector('dialog[open]'), null, { timeout: 5000 });
    await page.goto(`${origin}/settings/server`);
    await ready(page, 'settings-server');
    const panel = page.locator('main section, main .panel', { has: page.locator('h2', { hasText: /^Two-step sign-in$/ }) }).last();
    await panel.locator('button', { hasText: /^\s*Turn on\s*$/ }).click();
    await dlg.waitFor({ timeout: 5000 });
    // room for a whole recovery code, and nothing that refuses letters
    const room = (loc) => loc.evaluate((e) => { const long = 'a3k7-m2p9-q4r8-s5t6'; e.value = long; return e.value === long && e.pattern === ''; });
    assert.equal(await room(dlg.locator('[name=code]')), true, 'the set-up code field');
    await dlg.locator('[name=password]').fill(OWN);
    await dlg.locator('[name=code]').fill(CODE);
    await dlg.locator('button[type=submit]').click();
    const codes = dlg.locator('.ss-rcodes li');
    await codes.first().waitFor({ timeout: 5000 });
    const list = [];
    for (let i = 0; i < await codes.count(); i++) list.push((await codes.nth(i).innerText()).trim());
    assert.equal(list.length, 10);
    for (const c of list) assert.match(c, SHAPE);
    await dlg.locator('button', { hasText: /^\s*I have kept them\s*$/ }).click();
    await closed();
    await panel.getByText('10 recovery codes left').waitFor({ timeout: 5000 });

    // the other fields that ask for a code
    await page.locator('main button', { hasText: /^\s*Change your password\s*$/ }).first().click();
    await dlg.waitFor({ timeout: 5000 });
    assert.equal(await room(dlg.locator('[name=code]')), true, 'change password');
    await dlg.locator('button', { hasText: /^\s*Cancel\s*$/ }).click();
    await closed();
    const accountRow = page.locator('main tr', { has: page.locator('b', { hasText: /^admin$/ }) });
    await accountRow.locator('button', { hasText: /^\s*Edit\s*$/ }).click();
    await dlg.waitFor({ timeout: 5000 });
    assert.equal(await room(dlg.locator('[name=code]')), true, 'account editor');
    await dlg.locator('button', { hasText: /^\s*Cancel\s*$/ }).click();
    await closed();
    await panel.locator('input[name=rule-everyone]').check({ noWaitAfter: true });
    await dlg.waitFor({ timeout: 5000 });
    assert.equal(await room(dlg.locator('[name=two_step]')), true, 'rule change');
    await dlg.locator('button', { hasText: /^\s*Cancel\s*$/ }).click();
    await closed();

    // new recovery codes, with a recovery code typed in capitals and spaces, once
    await panel.locator('button', { hasText: /^\s*New recovery codes\s*$/ }).click();
    await dlg.waitFor({ timeout: 5000 });
    await dlg.locator('[name=password]').fill(OWN);
    const typed = list[0].toUpperCase().replace(/-/g, ' ');
    await dlg.locator('[name=code]').fill(typed);
    let resp = answered(page, 'POST', '/api/v1/me/two-step/recovery-codes');
    await dlg.locator('button[type=submit]').click();
    const r = await resp;
    assert.equal(r.status(), 200);
    assert.equal(JSON.parse(r.request().postData()).code, typed, 'a recovery code goes as typed');
    await dlg.locator('.ss-rcodes li').first().waitFor({ timeout: 5000 });
    await dlg.locator('button', { hasText: /^\s*I have kept them\s*$/ }).click();
    await closed();
    // the old ones stopped
    await panel.locator('button', { hasText: /^\s*New recovery codes\s*$/ }).click();
    await dlg.waitFor({ timeout: 5000 });
    await dlg.locator('[name=password]').fill(OWN);
    await dlg.locator('[name=code]').fill(list[1]);
    resp = answered(page, 'POST', '/api/v1/me/two-step/recovery-codes');
    await dlg.locator('button[type=submit]').click();
    assert.equal((await resp).status(), 401);
  });
});

test('a rule for every account walls an account without two-step into setting it up', { skip: SKIP, timeout: 180000 }, async () => {
  await withServer([], async ({ origin, page }) => {
    const dlg = page.locator('dialog[open]');
    const closed = () => page.waitForFunction(() => !document.querySelector('dialog[open]'), null, { timeout: 5000 });
    await page.goto(`${origin}/`);
    await ready(page, 'fleet');
    // the rule goes on while the page is open: the next call that answers 403 two_step_setup brings the wall
    assert.equal(await putRule(page), 200);
    await page.evaluate(() => document.querySelector('nav a[href="/places"]').click()); // (its menu group is folded)
    const wall = page.locator('main h1', { hasText: 'Set up two-step sign-in' });
    await wall.waitFor({ timeout: 10000 });
    await page.getByText(/asks every account for two-step sign-in/).first().waitFor({ timeout: 5000 });
    assert.equal(await page.locator('nav a[href="/drives"]').count(), 0, 'no app behind the wall');
    assert.equal(await page.evaluate(async () => (await fetch('/api/v1/cars')).status), 403);

    // a reload shows it at once (GET /me says two_step_setup_only), at any address
    await page.goto(`${origin}/settings/server`);
    await wall.waitFor({ timeout: 10000 });
    assert.equal(await page.evaluate(() => document.documentElement.dataset.screen), 'bare');

    // turn it on here, then the normal app loads
    await page.locator('button', { hasText: /^\s*Turn on two-step sign-in\s*$/ }).click();
    await dlg.waitFor({ timeout: 5000 });
    await dlg.locator('[name=password]').fill(OWN);
    await dlg.locator('[name=code]').fill(CODE);
    const resp = answered(page, 'POST', '/api/v1/me/two-step/confirm');
    await dlg.locator('button[type=submit]').click();
    assert.equal((await resp).status(), 200);
    await dlg.locator('.ss-rcodes li').first().waitFor({ timeout: 5000 });
    assert.match(await dlg.locator('.ss-rcodes li').first().innerText(), SHAPE);
    await dlg.locator('button', { hasText: /^\s*I have kept them\s*$/ }).click();
    await closed();
    await ready(page, 'settings-server');
    assert.equal(await page.evaluate(() => document.documentElement.dataset.screen), 'app');
    assert.equal(await page.evaluate(async () => (await fetch('/api/v1/cars')).status), 200);
  });
});

test('the set-up wall offers Sign out', { skip: SKIP, timeout: 120000 }, async () => {
  await withServer([], async ({ origin, page }) => {
    await page.goto(`${origin}/`);
    await ready(page, 'fleet');
    assert.equal(await putRule(page), 200);
    await page.goto(`${origin}/`);
    await page.locator('main h1', { hasText: 'Set up two-step sign-in' }).waitFor({ timeout: 10000 });
    await page.locator('button', { hasText: /^\s*Sign out\s*$/ }).click();
    await page.waitForFunction(() => location.pathname === '/sign-in', null, { timeout: 10000 });
    await ready(page, 'sign-in');
  });
});
