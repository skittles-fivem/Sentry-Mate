// The account forms of Settings: Server and access in every built-in pack, in Chromium on the dev API server. The
// server asks for the acting admin's own password again before every account change (confirm_password, 403
// password_needed without it), so:
//   - "Add an account" and "Edit" have a "Your password" field and send it as confirm_password; a wrong one is said
//     under that field and the dialog stays open; the right one makes or changes the account;
//   - "Remove" asks for the password in its dialog and sends it with the DELETE.
// And anyone's own password: "Change your password" beside the sessions asks for the current one and the new one,
// says a wrong current one under its field, and sends PUT /me/password; the new password then signs in.
// Run: node --test ui/tools/account-password.test.js. Skipped when Playwright is not installed.
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

const OWN = 'sentry-mate-dev'; // the dev server's password of every made-up account

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

for (const pack of BUILTIN_PACKS) {
  test(`${pack}: adding, changing and removing an account sends the admin's own password, typed in the dialog`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const server = await devServer(pack);
    const browser = await playwright.chromium.launch();
    try {
      const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', (e) => errors.push(e.message));
      const sent = [];
      page.on('response', (r) => {
        const q = r.request();
        if (q.method() !== 'GET' && /\/api\/v1\/accounts(\/|$)/.test(new URL(r.url()).pathname)) sent.push({ method: q.method(), status: r.status(), body: JSON.parse(q.postData() || '{}') });
      });
      const last = async (method) => { await page.waitForTimeout(150); return sent.filter((s) => s.method === method).at(-1); };
      await page.goto(`${server.origin}/settings/server`);
      await page.waitForFunction(() => document.documentElement.dataset.ready === 'settings-server', null, { timeout: 30000 });
      const row = (name) => page.locator('main tr', { has: page.locator('b', { hasText: new RegExp(`^${name}$`) }) });
      const dlg = page.locator('dialog[open]');

      // add: without the right password nothing is made and the field says so
      await page.locator('main button', { hasText: /^\s*Add account\s*$/ }).first().click();
      await dlg.waitFor({ timeout: 5000 });
      await dlg.locator('[name=name]').fill('viewer-nine');
      await dlg.locator('[name=password]').fill('correct horse battery staple 9');
      const own = dlg.locator('[name=confirm_password]');
      assert.equal(await own.getAttribute('type'), 'password');
      assert.equal(await own.getAttribute('autocomplete'), 'current-password');
      await own.fill('not my password');
      let answered = page.waitForResponse((r) => r.request().method() === 'POST' && new URL(r.url()).pathname === '/api/v1/accounts');
      await dlg.locator('button[type=submit]').click();
      await answered;
      assert.equal((await last('POST')).status, 403);
      await page.waitForFunction(() => document.querySelector('dialog[open] [name=confirm_password]')?.getAttribute('aria-invalid') === 'true', null, { timeout: 5000 });
      assert.equal(await row('viewer-nine').count(), 0);
      await own.fill(OWN);
      answered = page.waitForResponse((r) => r.request().method() === 'POST' && new URL(r.url()).pathname === '/api/v1/accounts');
      await dlg.locator('button[type=submit]').click();
      await answered;
      const made = await last('POST');
      assert.equal(made.status, 201);
      assert.equal(made.body.confirm_password, OWN);
      await dlg.waitFor({ state: 'detached', timeout: 5000 }).catch(() => dlg.waitFor({ state: 'hidden', timeout: 5000 }));
      await row('viewer-nine').waitFor({ timeout: 5000 });

      // edit: a new role goes with the password
      await row('viewer-nine').locator('button', { hasText: /^\s*Edit\s*$/ }).click();
      await dlg.waitFor({ timeout: 5000 });
      await dlg.locator('[name=role]').selectOption('admin');
      await dlg.locator('[name=confirm_password]').fill(OWN);
      answered = page.waitForResponse((r) => r.request().method() === 'PATCH');
      await dlg.locator('button[type=submit]').click();
      await answered;
      const changed = await last('PATCH');
      assert.equal(changed.status, 200);
      assert.equal(changed.body.confirm_password, OWN);
      await page.waitForFunction(() => !document.querySelector('dialog[open]'), null, { timeout: 5000 });
      await row('viewer-nine').getByText('Admin', { exact: true }).waitFor({ timeout: 5000 });

      // remove: the dialog asks for the password; a wrong one keeps the account
      await row('viewer-nine').locator('button', { hasText: /^\s*Remove\s*$/ }).click();
      await dlg.waitFor({ timeout: 5000 });
      await dlg.locator('input[type=password]').fill('not my password');
      answered = page.waitForResponse((r) => r.request().method() === 'DELETE');
      await dlg.locator('button[type=submit]').click();
      await answered;
      assert.equal((await last('DELETE')).status, 403);
      assert.equal(await row('viewer-nine').count(), 1);
      await dlg.locator('input[type=password]').fill(OWN);
      answered = page.waitForResponse((r) => r.request().method() === 'DELETE');
      await dlg.locator('button[type=submit]').click();
      await answered;
      const gone = await last('DELETE');
      assert.equal(gone.status, 200);
      assert.equal(gone.body.confirm_password, OWN);
      await page.waitForFunction(() => !document.querySelector('dialog[open]'), null, { timeout: 5000 });
      await row('viewer-nine').waitFor({ state: 'detached', timeout: 5000 });
      assert.deepEqual(errors, []);
    } finally {
      await browser.close();
      await server.stop();
    }
  });

  test(`${pack}: Change your password asks for the current one and the new one, and the new one signs in`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const server = await devServer(pack);
    const browser = await playwright.chromium.launch();
    try {
      const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', (e) => errors.push(e.message));
      const sent = [];
      page.on('response', (r) => {
        const q = r.request();
        if (q.method() === 'PUT' && new URL(r.url()).pathname === '/api/v1/me/password') sent.push({ status: r.status(), body: JSON.parse(q.postData() || '{}') });
      });
      await page.goto(`${server.origin}/settings/server`);
      await page.waitForFunction(() => document.documentElement.dataset.ready === 'settings-server', null, { timeout: 30000 });
      await page.locator('main button', { hasText: /^\s*Change your password\s*$/ }).first().click();
      const dlg = page.locator('dialog[open]');
      await dlg.waitFor({ timeout: 5000 });
      const NEW = 'correct horse battery staple 7';
      await dlg.locator('[name=password]').fill('not my password');
      await dlg.locator('[name=new_password]').fill(NEW);
      let answered = page.waitForResponse((r) => r.request().method() === 'PUT');
      await dlg.locator('button[type=submit]').click();
      await answered;
      await page.waitForFunction(() => document.querySelector('dialog[open] [name=password]')?.getAttribute('aria-invalid') === 'true', null, { timeout: 5000 });
      assert.equal(sent.at(-1).status, 403);
      await dlg.locator('[name=password]').fill(OWN);
      answered = page.waitForResponse((r) => r.request().method() === 'PUT');
      await dlg.locator('button[type=submit]').click();
      await answered;
      await page.waitForFunction(() => !document.querySelector('dialog[open]'), null, { timeout: 5000 });
      assert.deepEqual(sent.at(-1), { status: 200, body: { password: OWN, new_password: NEW } });
      // this browser stays signed in, and the new password is the account's
      const who = await page.evaluate(async () => (await (await fetch('/api/v1/me')).json()).name);
      const tries = await page.evaluate(async ([name, old, now]) => {
        const go = (password) => fetch('/api/v1/session', { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Sentry-Mate': '1' }, body: JSON.stringify({ name, password }) }).then((r) => r.status);
        return [await go(old), await go(now)];
      }, [who, OWN, NEW]);
      assert.deepEqual(tries, [401, 200]);
      assert.deepEqual(errors, []);
    } finally {
      await browser.close();
      await server.stop();
    }
  });
}
