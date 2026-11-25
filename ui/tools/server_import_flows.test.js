// Flows on Settings > Server and Settings > Import in Chromium on the dev API server: one request at a time
// from the host names buttons, Start import usable again after a failed start, a discarded upload that stops
// quietly, and the keyboard focus kept on the Storage group head and back on the page after the certificate
// and recovery key dialogs close.
// Run: node --test ui/tools/server_import_flows.test.js. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const crypto = require('crypto');
const { spawn } = require('child_process');

const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
const PACK = path.join(UI_DIR, 'packs', 'instrument');
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }
const SKIP = !playwright && 'Playwright is not installed';

function devServer(extra = []) {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', PACK, '--port', '0', ...extra], { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
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

/** Runs `body({ page, reqs, errors })` on a route of the dev server (its clock stopped unless freeze is false). */
async function withPage(route, body, { freeze = true } = {}) {
  const browser = await playwright.chromium.launch();
  const server = await devServer(freeze ? ['--freeze-clock'] : []);
  try {
    const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
    const page = await context.newPage();
    const errors = [], reqs = [];
    page.on('pageerror', (e) => errors.push(e.message));
    page.on('request', (r) => { if (r.method() !== 'GET') reqs.push(`${r.method()} ${r.url().replace(server.origin, '')}`); });
    await page.goto(server.origin + route);
    await page.waitForFunction(() => document.documentElement.dataset.ready, null, { timeout: 30000 });
    await body({ page, reqs, errors });
    await context.close();
  } finally {
    await server.stop();
    await browser.close();
  }
}

const slow = (page, pattern, method, ms) => page.route(pattern, async (r) => { if (r.request().method() === method) await new Promise((x) => setTimeout(x, ms)); r.continue(); });
const focused = (page) => page.evaluate(() => { const a = document.activeElement; return a === document.body ? 'BODY' : `${a.tagName} ${(a.textContent || '').trim().slice(0, 40)}`; });
const flashes = async (page) => (await page.locator('.flash').allInnerTexts()).map((x) => x.replace(/\s+/g, ' '));

test('a double click on Save host names sends one save and no false "someone else" message', { skip: SKIP }, async () => {
  await withPage('/settings/server', async ({ page, reqs }) => {
    const dlg = page.locator('dialog[open]');
    await page.getByRole('button', { name: 'Change names' }).click();
    await dlg.getByLabel('Host name to add').fill('new-host.local');
    await dlg.getByRole('button', { name: 'Add', exact: true }).click();
    reqs.length = 0;
    await slow(page, '**/api/v1/settings', 'PATCH', 400);
    await dlg.getByRole('button', { name: 'Save host names' }).dblclick();
    await page.waitForTimeout(1200);
    assert.deepEqual(reqs.filter((r) => r.startsWith('PATCH')), ['PATCH /api/v1/settings']);
    const msgs = (await flashes(page)).join('\n');
    assert.doesNotMatch(msgs, /Someone else/);
    assert.match(msgs, /Host names saved/);
  });
});

test('Start import is usable again after a start that failed', { skip: SKIP }, async () => {
  await withPage('/settings/import', async ({ page }) => {
    await page.setInputFiles('.im-file-input', [{ name: 'history.sql', mimeType: 'application/octet-stream', buffer: Buffer.from(crypto.randomBytes(150000).toString('base64')) }]);
    await page.waitForFunction(() => /Match |ready to start/.test(document.querySelector('.im-why')?.textContent || ''), null, { timeout: 30000 });
    const sels = page.locator('select.im-select');
    for (let i = 0; i < await sels.count(); i++) { await sels.nth(i).selectOption('new'); await page.waitForTimeout(300); }
    const start = page.getByRole('button', { name: 'Start import' });
    await page.waitForFunction(() => !document.querySelector('.im-actions .btn--primary').disabled, null, { timeout: 10000 });
    await page.route('**/api/v1/imports/*/start', (r) => r.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify({ error: { code: 'internal', message: 'Server broke' } }) }));
    await start.click();
    await page.locator('.im-err:visible').waitFor();
    await page.waitForTimeout(500);
    assert.equal(await start.isEnabled(), true);
  }, { freeze: false });
});

test('discarding an import while its file is uploading stops the upload quietly', { skip: SKIP }, async () => {
  await withPage('/settings/import', async ({ page, errors }) => {
    await slow(page, '**/api/v1/imports/**', 'PATCH', 600);
    await page.setInputFiles('.im-file-input', [{ name: 'history.sql', mimeType: 'application/octet-stream', buffer: crypto.randomBytes(4 * 1024 * 1024) }]);
    await page.waitForTimeout(700);
    await page.getByRole('button', { name: 'Discard' }).click();
    await page.locator('dialog[open]').getByRole('button', { name: 'Discard' }).click();
    await page.waitForTimeout(3000);
    assert.deepEqual(await page.locator('.form-error:visible, [role=alert]:visible').allInnerTexts(), []);
    assert.doesNotMatch((await page.innerText('main')).replace(/\s+/g, ' '), /upload of .* stopped|no such import/i);
    assert.deepEqual(errors, []);
  }, { freeze: false });
});

test('toggling a Storage group keeps the keyboard focus on its head', { skip: SKIP }, async () => {
  await withPage('/settings/server', async ({ page }) => {
    await page.waitForTimeout(800);
    const head = page.locator('.st-group-head').nth(1);
    await head.focus();
    const before = await head.getAttribute('aria-expanded');
    await page.keyboard.press('Enter');
    await page.waitForTimeout(200);
    const now = page.locator('.st-group-head').nth(1);
    assert.notEqual(await now.getAttribute('aria-expanded'), before);
    assert.equal(await now.evaluate((e) => e === document.activeElement), true, `focus is on ${await focused(page)}`);
    await page.keyboard.press('Enter');
    await page.waitForTimeout(200);
    assert.equal(await page.locator('.st-group-head').nth(1).evaluate((e) => e === document.activeElement), true);
  });
});

test('the focus is not lost when the new certificate and new recovery key dialogs close', { skip: SKIP }, async () => {
  await withPage('/settings/server', async ({ page }) => {
    const dlg = page.locator('dialog[open]');
    await page.getByRole('button', { name: 'Make a new certificate' }).first().click();
    await dlg.getByLabel('Your password').fill('sentry-mate-dev');
    await dlg.getByRole('button', { name: 'Make a new certificate' }).click();
    await dlg.getByRole('button', { name: 'Done' }).waitFor();
    await dlg.getByRole('button', { name: 'Done' }).click();
    await page.waitForTimeout(300);
    assert.match(await focused(page), /^BUTTON Make a new certificate/, 'after the certificate dialog');

    await page.getByRole('button', { name: 'Make a new recovery key' }).click();
    await dlg.getByLabel('Your password').fill('sentry-mate-dev');
    await dlg.getByRole('button', { name: 'Make a new key' }).click();
    await dlg.getByRole('button', { name: 'Close' }).first().waitFor();
    await dlg.getByRole('button', { name: 'Close' }).first().click();
    await page.waitForTimeout(300);
    assert.match(await focused(page), /^BUTTON Make a new recovery key/, 'after the recovery key dialog');
  });
});
