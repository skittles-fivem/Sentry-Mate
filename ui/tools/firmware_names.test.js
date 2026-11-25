// Software: Module firmware history, a module's first fingerprint segment without a name, in Chromium on the dev API
// server (made-up car c3: its imported history ends on 2025.20.6 before its first CAN fingerprints, with no update
// between; c1 has no import): an unnamed segment says why ("no input reported a version name while this ran"); a
// segment whose last known version has no update between it and the segment shows "probably 2025.20.6 (last known
// before)" styled as an inference, in the table and the strip; an admin's "Name this version" opens a box with the
// inferred version in it, asks no password, sends PATCH /cars/c3/firmware/history/{row} and the segment then shows
// the typed name; a watch account is offered no such button.
// Run: node --test ui/tools/firmware_names.test.js. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawn } = require('child_process');

const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
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

async function openSoftware(page, origin, car) {
  await page.goto(`${origin}/cars/${car}/software?preset=all`);
  await page.waitForFunction(() => document.documentElement.dataset.ready === 'software', null, { timeout: 30000 });
  await page.waitForLoadState('networkidle', { timeout: 20000 });
}

/** The "Came with" cell of the oldest bms fingerprint ("first one seen"). */
const firstBms = (page) => page.locator('#firmware-history tbody tr', { hasText: 'Battery management' }).filter({ hasText: 'first one seen' }).first();

test('an unnamed first segment says why, an inferred one says what it probably is, an admin names it', { skip: !playwright && 'Playwright is not installed', timeout: 240000 }, async () => {
  const browser = await playwright.chromium.launch();
  const server = await devServer();
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));
    const sent = [];
    page.on('request', (r) => {
      if (r.method() === 'PATCH' && new URL(r.url()).pathname.includes('/firmware/history/')) sent.push({ path: new URL(r.url()).pathname, body: r.postDataJSON() });
    });

    // c1: nothing was ever imported, so nothing can be inferred: "unnamed", and why
    await openSoftware(page, server.origin, 'c1');
    let row = firstBms(page);
    await row.waitFor();
    let text = await row.innerText();
    assert.match(text, /unnamed/);
    assert.match(text, /no input reported a version name while this ran/);
    assert.ok(!/probably/.test(text), text);
    assert.equal(await row.locator('.sw-ver--inferred').count(), 0);

    // c3: the import's last version (2025.20.6) is known before the segment, no update between: an inference
    await openSoftware(page, server.origin, 'c3');
    row = firstBms(page);
    await row.waitFor();
    text = await row.innerText();
    assert.match(text, /probably 2025\.20\.6 \(last known before\)/);
    assert.ok(!/unnamed/.test(text), text);
    const inferred = row.locator('.sw-ver--inferred');
    assert.equal(await inferred.count(), 1);
    assert.equal(await inferred.evaluate((el) => getComputedStyle(el).fontStyle), 'italic', 'drawn as an inference, not as a name');
    // the strip's lanes say it too
    const lane = await page.locator('.sw-fh-track').first().getAttribute('aria-label');
    assert.match(lane, /probably 2025\.20\.6 from /);
    assert.ok(await page.locator('.sw-fh-seg-bar.is-inferred').count() >= 4, 'every module\'s first bar');

    // an admin names it: the box has the inferred version in it and asks no password
    const btn = row.locator('button', { hasText: 'Name this version' });
    await btn.click();
    const dlg = page.locator('dialog[open]');
    await dlg.waitFor();
    assert.equal(await dlg.locator('input[type="password"]').count(), 0, 'no password asked');
    const input = dlg.locator('input[name="version"]');
    assert.equal(await input.inputValue(), '2025.20.6');
    await input.fill('latest');
    await dlg.locator('button[type="submit"]').click();
    assert.equal(sent.length, 0, 'a bad version is not sent');
    await input.fill('2026.38.6');
    await dlg.locator('button[type="submit"]').click();
    await page.locator('dialog[open]').waitFor({ state: 'detached', timeout: 10000 });
    assert.equal(sent.length, 1);
    assert.match(sent[0].path, /^\/api\/v1\/cars\/c3\/firmware\/history\/bms(:|%3A)\d+$/);
    assert.deepEqual(sent[0].body, { version: '2026.38.6' });
    await page.waitForFunction(() => /2026\.38\.6/.test(document.querySelector('#firmware-history')?.textContent || ''), null, { timeout: 10000 });
    row = firstBms(page);
    text = await row.innerText();
    assert.match(text, /2026\.38\.6/);
    assert.ok(!/probably|unnamed/.test(text), text);
    assert.equal(await row.locator('.sw-ver--inferred').count(), 0);
    // the audit log has it
    const audit = await page.evaluate(async () => (await fetch('/api/v1/audit?limit=5')).text());
    assert.match(audit, /update-version/);
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
    await server.stop();
  }
});

test('a watch account sees the inference and no button to name it', { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
  const browser = await playwright.chromium.launch();
  const server = await devServer(['--role', 'watch']);
  try {
    const page = await (await browser.newContext({ viewport: { width: 390, height: 844 } })).newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));
    await page.goto(`${server.origin}/cars/c1/software?preset=all`);
    await page.waitForFunction(() => document.documentElement.dataset.ready === 'software', null, { timeout: 30000 });
    await page.waitForLoadState('networkidle', { timeout: 20000 });
    const text = await page.evaluate(() => document.querySelector('#firmware-history')?.textContent || '');
    assert.match(text, /no input reported a version name while this ran/);
    assert.equal(await page.locator('#firmware-history button', { hasText: 'Name this version' }).count(), 0);
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
    await server.stop();
  }
});
