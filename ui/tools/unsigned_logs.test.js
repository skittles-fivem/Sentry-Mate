// Data health's logs without a signature, in Chromium on the dev API server (its made-up car c1 was paired by its
// first signed log, then sent older logs without a signature; one has a bad signature, one another key's): each
// unsigned log says why in plain words; an admin's "Use unsigned logs" opens a dialog that says exactly how many logs
// of which kinds and that bad or other-key logs are never included, offers only those from before the first signed
// log (preselected), asks the password again, and marks them "unsigned, used by the owner"; "Stop using" takes it
// back. A watch account sees none of it.
// Run: node --test ui/tools/unsigned_logs.test.js. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawn } = require('child_process');

const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
const PASSWORD = 'sentry-mate-dev';
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }

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

async function openHealth(page, origin) {
  await page.goto(`${origin}/cars/c1/health?preset=all`);
  await page.waitForFunction(() => document.documentElement.dataset.ready === 'health', null, { timeout: 30000 });
  await page.waitForLoadState('networkidle', { timeout: 20000 });
}

const blockText = (page) => page.evaluate(() => document.querySelector('#unsigned-logs')?.textContent || null);

test('an admin uses the unsigned logs of a paired car, never a bad one, and stops using them', { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
  const browser = await playwright.chromium.launch();
  const server = await devServer();
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));
    const sent = [];
    page.on('request', (r) => {
      if (new URL(r.url()).pathname === '/api/v1/cars/c1/logs/use-unsigned') sent.push({ method: r.method(), body: r.postDataJSON() });
    });
    await openHealth(page, server.origin);
    await page.waitForSelector('#unsigned-logs .hl-unsigned-row', { timeout: 20000 });
    let text = await blockText(page);
    // each kind in plain words
    for (const words of ['made before the CAN Bridge app signed logs', 'no sidecar came with it', 'the app sent it marked as not signed',
      'the app tried to sign it and could not: keystore was busy']) assert.ok(text.includes(words), `${words}: ${text}`);
    assert.match(text, /11 logs failed their check only for having no signature and are not used/);
    assert.match(text, /2 logs failed for something else/);
    assert.ok(!text.includes('used by the owner'), text);
    assert.equal(await page.locator('#unsigned-logs button', { hasText: 'Stop using' }).count(), 0);

    // the dialog: how many, which kinds, what is never included, the logs from before the first signed log chosen
    await page.locator('#unsigned-logs button', { hasText: 'Use unsigned logs' }).click();
    const dlg = page.locator('dialog[open]');
    await dlg.waitFor();
    const only = dlg.locator('input[name="only_before"]');
    assert.equal(await only.isChecked(), true, 'only those from before the first signed log, preselected');
    let said = await dlg.innerText();
    assert.match(said, /Use 9 logs of Comet .*all from before its first signed log/);
    assert.match(said, /bad signature, signed by another key, or changed after signing are never included \(2 logs of this car\)/);
    assert.match(said, /Only the 9 logs from before this car’s first signed log/);
    await only.uncheck();
    said = await dlg.innerText();
    assert.match(said, /Use 11 logs of Comet .*\(7 made before the CAN Bridge app signed logs, 1 with no sidecar, 2 marked as not signed by the app, 1 the app could not sign\)/);
    await only.check();
    // a wrong password is said in the dialog, which stays open
    await dlg.locator('input[name="password"]').fill('not it');
    await dlg.locator('button[type="submit"]').click();
    await dlg.locator('.form-error:not([hidden])').waitFor();
    assert.match(await dlg.innerText(), /not your password/i);
    await dlg.locator('input[name="password"]').fill(PASSWORD);
    await dlg.locator('button[type="submit"]').click();
    await page.waitForFunction(() => !document.querySelector('dialog[open]'), null, { timeout: 10000 });
    const ok = sent.filter((s) => s.method === 'POST').pop();
    assert.deepEqual(ok.body, { all: true, before_first_signed: true, confirm_password: PASSWORD });
    await page.waitForFunction(() => document.querySelectorAll('#unsigned-logs .tag').length === 9, null, { timeout: 10000 });
    text = await blockText(page);
    assert.match(text, /unsigned, used by the owner/);
    assert.match(text, /9 logs without a signature are used, as an admin chose/);
    assert.match(text, /2 logs failed their check only for having no signature/);

    // and back
    await page.locator('#unsigned-logs button', { hasText: 'Stop using' }).click();
    await dlg.waitFor();
    assert.match(await dlg.innerText(), /Stop using the 9 logs of Comet/);
    await dlg.locator('input[name="password"]').fill(PASSWORD);
    await dlg.locator('button[type="submit"]').click();
    await page.waitForFunction(() => !document.querySelector('dialog[open]'), null, { timeout: 10000 });
    assert.equal(sent.filter((s) => s.method === 'DELETE').pop().body.all, true);
    await page.waitForFunction(() => document.querySelectorAll('#unsigned-logs .tag').length === 0, null, { timeout: 10000 });
    assert.equal(await page.locator('#unsigned-logs button', { hasText: 'Stop using' }).count(), 0);
    assert.deepEqual(errors, []);
    await context.close();
  } finally {
    await server.stop();
    await browser.close();
  }
});

test('a watch account sees no unsigned logs block and no buttons', { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
  const browser = await playwright.chromium.launch();
  const server = await devServer(['--role', 'watch']);
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));
    const asked = [];
    page.on('request', (r) => { if (new URL(r.url()).pathname.includes('/logs')) asked.push(r.url()); });
    await openHealth(page, server.origin);
    assert.equal(await blockText(page), null);
    assert.equal(await page.locator('button', { hasText: 'Use unsigned logs' }).count(), 0);
    assert.deepEqual(asked, [], 'the admin reads are not asked');
    assert.deepEqual(errors, []);
    await context.close();
  } finally {
    await server.stop();
    await browser.close();
  }
});
