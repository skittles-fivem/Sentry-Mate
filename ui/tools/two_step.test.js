// Two-step sign-in from Settings: Server and access in the Instrument pack, in Chromium on the dev API server:
//   - "Turn on" in the Two-step sign-in panel starts the set-up (POST /me/two-step), shows the key and the
//     otpauth link, asks for the password and a code (a wrong password is said under its field and the dialog
//     stays), and the right ones (POST /me/two-step/confirm) show the recovery codes once; the panel then says On;
//   - with it on, a dialog that asks for the password again (here: Change your password) shows the code field, and
//     "New recovery codes" and "Turn off" ask for the password and a code;
//   - an admin sets the rule (Require it for every account / from outside the home network) with PUT
//     /two-step-policy, sending their own password and code; the Accounts table says On / Password only.
// Run: node --test ui/tools/two_step.test.js. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawn } = require('child_process');

const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }

const OWN = 'sentry-mate-dev'; // the dev server's password of every made-up account
const CODE = '123456';         // the dev server takes any six digits for one's own two-step routes
const DEV_CODE = '000000';     // and this one where an admin confirms an account or rule change (as for the other admin changes)

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

test('instrument: two-step sign-in is turned on from the page, and an admin sets the rule', { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
  const server = await devServer('instrument');
  const browser = await playwright.chromium.launch();
  try {
    const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));
    const sent = [];
    page.on('response', (r) => {
      const q = r.request();
      const p = new URL(r.url()).pathname;
      if (q.method() !== 'GET' && /\/api\/v1\/(me\/two-step|two-step-policy|accounts)/.test(p)) sent.push({ method: q.method(), path: p, status: r.status(), body: JSON.parse(q.postData() || '{}') });
    });
    const answered = (method, p) => page.waitForResponse((r) => r.request().method() === method && new URL(r.url()).pathname === p);
    const dlg = page.locator('dialog[open]');
    const closed = () => page.waitForFunction(() => !document.querySelector('dialog[open]'), null, { timeout: 5000 });
    await page.goto(`${server.origin}/settings/server`);
    await page.waitForFunction(() => document.documentElement.dataset.ready === 'settings-server', null, { timeout: 30000 });
    const panel = page.locator('main section, main .panel', { has: page.locator('h2', { hasText: /^Two-step sign-in$/ }) }).last();
    await panel.waitFor({ timeout: 5000 });
    await panel.getByText('Your password alone signs in.').waitFor({ timeout: 5000 });
    const accountRow = (name) => page.locator('main tr', { has: page.locator('b', { hasText: new RegExp(`^${name}$`) }) });
    await accountRow('admin').getByText('Password only').waitFor({ timeout: 5000 });
    await accountRow('alex').getByText('On', { exact: true }).waitFor({ timeout: 5000 });
    // the account editor no longer offers a per-account rule
    await accountRow('admin').locator('button', { hasText: /^\s*Edit\s*$/ }).click();
    await dlg.waitFor({ timeout: 5000 });
    assert.equal(await dlg.locator('[name=two_step_required]').count(), 0);
    await dlg.locator('button', { hasText: /^\s*Cancel\s*$/ }).click();
    await closed();

    // turn on: the key and its link, then the password and a code
    let resp = answered('POST', '/api/v1/me/two-step');
    await panel.locator('button', { hasText: /^\s*Turn on\s*$/ }).click();
    assert.equal((await resp).status(), 201);
    await dlg.waitFor({ timeout: 5000 });
    const href = await dlg.locator('a', { hasText: 'Open in your authenticator app' }).getAttribute('href');
    assert.match(href, /^otpauth:\/\/totp\/.+secret=[A-Z2-7]{32}/);
    assert.match((await dlg.locator('.ss-secret').innerText()).replace(/\s+/g, ''), /^([A-Z2-7]{4}-?){8}$/);
    await dlg.locator('[name=password]').fill('not my password');
    await dlg.locator('[name=code]').fill(CODE);
    resp = answered('POST', '/api/v1/me/two-step/confirm');
    await dlg.locator('button[type=submit]').click();
    assert.equal((await resp).status(), 403);
    await page.waitForFunction(() => document.querySelector('dialog[open] [name=password]')?.getAttribute('aria-invalid') === 'true', null, { timeout: 5000 });
    await dlg.locator('[name=password]').fill(OWN);
    await dlg.locator('[name=code]').fill('12');
    resp = answered('POST', '/api/v1/me/two-step/confirm');
    await dlg.locator('button[type=submit]').click();
    assert.equal((await resp).status(), 401);
    await page.waitForFunction(() => document.querySelector('dialog[open] [name=code]')?.getAttribute('aria-invalid') === 'true', null, { timeout: 5000 });
    await dlg.locator('[name=code]').fill(CODE);
    resp = answered('POST', '/api/v1/me/two-step/confirm');
    await dlg.locator('button[type=submit]').click();
    assert.equal((await resp).status(), 200);
    // the recovery codes, once
    const codes = dlg.locator('.ss-rcodes li');
    await codes.first().waitFor({ timeout: 5000 });
    assert.equal(await codes.count(), 10);
    assert.match(await codes.first().innerText(), /^[0-9a-hjkmnp-tv-z]{4}(-[0-9a-hjkmnp-tv-z]{4}){3}$/);
    await dlg.getByText('Keep these recovery codes').waitFor();
    await dlg.locator('button', { hasText: /^\s*I have kept them\s*$/ }).click();
    await closed();
    await panel.getByText('10 recovery codes left').waitFor({ timeout: 5000 });
    await accountRow('admin').getByText('On', { exact: true }).waitFor({ timeout: 5000 });
    assert.equal(sent.filter((s) => s.path === '/api/v1/me/two-step/confirm').at(-1).body.code, CODE);

    // with two-step on, a dialog that asks for the password again shows the code field
    await page.locator('main button', { hasText: /^\s*Change your password\s*$/ }).first().click();
    await dlg.waitFor({ timeout: 5000 });
    assert.equal(await dlg.locator('[name=code]').isVisible(), true);
    await dlg.locator('button', { hasText: /^\s*Cancel\s*$/ }).click();
    await closed();

    // new recovery codes
    await panel.locator('button', { hasText: /^\s*New recovery codes\s*$/ }).click();
    await dlg.waitFor({ timeout: 5000 });
    await dlg.locator('[name=password]').fill(OWN);
    await dlg.locator('[name=code]').fill(CODE);
    resp = answered('POST', '/api/v1/me/two-step/recovery-codes');
    await dlg.locator('button[type=submit]').click();
    assert.equal((await resp).status(), 200);
    await dlg.locator('.ss-rcodes li').first().waitFor({ timeout: 5000 });
    assert.equal(await dlg.locator('.ss-rcodes li').count(), 10);
    await dlg.locator('button', { hasText: /^\s*I have kept them\s*$/ }).click();
    await closed();

    // the rule: every account (own password and code asked for), then from outside the home network
    const everyone = panel.locator('input[name=rule-everyone]');
    const outside = panel.locator('input[name=rule-outside]');
    assert.equal(await everyone.isChecked(), false);
    await everyone.check({ noWaitAfter: true });
    await dlg.waitFor({ timeout: 5000 });
    await dlg.locator('[name=password]').fill('not my password');
    await dlg.locator('[name=two_step]').fill(DEV_CODE);
    resp = answered('PUT', '/api/v1/two-step-policy');
    await dlg.locator('button[type=submit]').click();
    assert.equal((await resp).status(), 403);
    await dlg.locator('[name=password]').fill(OWN);
    resp = answered('PUT', '/api/v1/two-step-policy');
    await dlg.locator('button[type=submit]').click();
    assert.equal((await resp).status(), 200);
    await closed();
    await page.waitForFunction(() => document.querySelector('input[name=rule-everyone]')?.checked === true, null, { timeout: 5000 });
    assert.deepEqual(sent.filter((s) => s.path === '/api/v1/two-step-policy').at(-1).body, { everyone: true, outside: false, confirm_password: OWN, code: DEV_CODE });
    // turned on for everyone, a person cannot turn their own off
    await panel.locator('button', { hasText: /^\s*Turn off\s*$/ }).click();
    await dlg.waitFor({ timeout: 5000 });
    await dlg.locator('[name=password]').fill(OWN);
    await dlg.locator('[name=code]').fill(CODE);
    resp = answered('DELETE', '/api/v1/me/two-step');
    await dlg.locator('button[type=submit]').click();
    assert.equal((await resp).status(), 409);
    await dlg.locator('button', { hasText: /^\s*Cancel\s*$/ }).click();
    await closed();

    await outside.check({ noWaitAfter: true });
    await dlg.waitFor({ timeout: 5000 });
    await dlg.locator('[name=password]').fill(OWN);
    await dlg.locator('[name=two_step]').fill(DEV_CODE);
    resp = answered('PUT', '/api/v1/two-step-policy');
    await dlg.locator('button[type=submit]').click();
    assert.equal((await resp).status(), 200);
    await closed();
    // cancelling leaves a switch as it was
    await everyone.uncheck({ noWaitAfter: true });
    await dlg.waitFor({ timeout: 5000 });
    await dlg.locator('button', { hasText: /^\s*Cancel\s*$/ }).click();
    await closed();
    await page.waitForFunction(() => document.querySelector('input[name=rule-everyone]')?.checked === true, null, { timeout: 5000 });
    // the rule is saved on the server
    const rule = await page.evaluate(async () => (await fetch('/api/v1/two-step-policy')).json());
    assert.deepEqual(rule, { everyone: true, outside: true });
    // with the rule for every account off again, a person can turn their own off
    await everyone.uncheck({ noWaitAfter: true });
    await dlg.waitFor({ timeout: 5000 });
    await dlg.locator('[name=password]').fill(OWN);
    await dlg.locator('[name=two_step]').fill(DEV_CODE);
    resp = answered('PUT', '/api/v1/two-step-policy');
    await dlg.locator('button[type=submit]').click();
    await resp;
    await closed();
    await panel.locator('button', { hasText: /^\s*Turn off\s*$/ }).click();
    await dlg.waitFor({ timeout: 5000 });
    await dlg.locator('[name=password]').fill(OWN);
    await dlg.locator('[name=code]').fill(CODE);
    resp = answered('DELETE', '/api/v1/me/two-step');
    await dlg.locator('button[type=submit]').click();
    assert.equal((await resp).status(), 200);
    await closed();
    await panel.getByText('Your password alone signs in.').waitFor({ timeout: 5000 });
    assert.equal(sent.some((s) => s.path.startsWith('/api/v1/accounts') && 'two_step_required' in s.body), false);
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
    await server.stop();
  }
});
