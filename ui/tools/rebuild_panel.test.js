// Data health's Rebuild row and "Rebuild all data" panel, and the GPS layout fit, in Chromium on the dev API server.
//  - A full rebuild that is paused (after an earlier one was cancelled, --full-rebuild paused) is said the same way by
//    the Rebuild row, the State line and the buttons: paused, with no step named, no speed and no time left; "Start
//    now" makes the row and the panel say it runs, together.
//  - The fit's progress is weighed by the work (--gps-fit running: 272 of 22,402 logs read is 1 %, not 30 %) and shows
//    a time left from the measured speed.
//  - Past fits (--gps-fit history): the list newest first with the best layout and its errors in metres, good or weak
//    and whether it was applied; a row opens to the full result; a past result is applied with the password asked
//    again; one can be removed; the layouts used over time are listed. Phone and desktop size.
// Run: node --test ui/tools/rebuild_panel.test.js. Skipped when Playwright is not installed.
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

const rowText = (page) => page.locator('[data-keep="rebuild-row"]').innerText();
const line = (page, key) => page.locator(`#full-rebuild .fr-line[data-key="${key}"] dd`).innerText();

async function withPage(extra, fn, viewport = { width: 1440, height: 900 }) {
  const browser = await playwright.chromium.launch();
  const server = await devServer(extra);
  try {
    const context = await browser.newContext({ viewport });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));
    await openHealth(page, server.origin);
    await fn(page, server);
    assert.deepEqual(errors, [], `page errors: ${errors}`);
  } finally {
    await browser.close();
    await server.stop();
  }
}

test('a paused full rebuild is one state in the Rebuild row, the panel and the buttons', { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
  await withPage(['--full-rebuild', 'paused'], async (page) => {
    await page.waitForSelector('#full-rebuild .fr-line[data-key="state"]');
    await page.waitForFunction(() => /Paused/.test(document.querySelector('#full-rebuild .fr-line[data-key="state"] dd')?.textContent || ''));
    // the panel: paused, nothing of a step under way, no speed, no time left
    assert.match(await line(page, 'state'), /^Paused: starts when you start it$/);
    assert.equal(await line(page, 'phase'), '');
    assert.equal(await line(page, 'rate'), '');
    // the row beside the CAN Bridge key says the same, not "Running: Decoding the stored logs"
    const row = await rowText(page);
    assert.match(row, /^Full rebuild paused: starts when you start it$/, row);
    assert.ok(!/Decoding|Running/.test(row), row);
    // the earlier full rebuild is in the list as cancelled, the paused one is a different run
    const hist = await page.locator('#full-rebuild .fr-history li').first().innerText();
    assert.match(hist, /Full rebuild of every car/);
    assert.match(hist, /cancelled/);
    // buttons: Cancel, Start now and Tonight, no Pause
    const buttons = (await page.locator('#full-rebuild .fr-acts button:visible').allInnerTexts()).map((t) => t.trim());
    assert.deepEqual(buttons, ['Cancel full rebuild', 'Start now', 'Tonight']);

    // Start now: both say it runs, in the same step
    await page.locator('#full-rebuild .fr-acts button', { hasText: 'Start now' }).click();
    await page.waitForFunction(() => /^Running/.test(document.querySelector('#full-rebuild .fr-line[data-key="state"] dd')?.textContent || ''));
    const step = await line(page, 'phase');
    assert.match(step, /^(Copying|Decoding|Working|Waiting|Switching)/i, step);
    await page.waitForFunction((s) => document.querySelector('[data-keep="rebuild-row"]')?.textContent === `Running: ${s}`, step);
    assert.equal(await rowText(page), `Running: ${step}`);
    assert.ok(!/Paused/.test(await line(page, 'state')));
    // Pause again: the row follows the panel
    await page.locator('#full-rebuild .fr-acts button', { hasText: 'Pause' }).click();
    await page.waitForFunction(() => /^Full rebuild paused/.test(document.querySelector('[data-keep="rebuild-row"]')?.textContent || ''));
    assert.match(await line(page, 'state'), /^Paused/);
    assert.equal(await line(page, 'phase'), '');
  });
});

test('the fit says how far it is by the work, with a time left', { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
  await withPage(['--gps-fit', 'running'], async (page) => {
    await page.waitForSelector('#gps-layout .gf-head');
    await page.waitForFunction(() => /Finding the layout/.test(document.querySelector('#gps-layout .gf-head')?.textContent || ''));
    const head = await page.locator('#gps-layout .gf-head').innerText();
    const step = await page.locator('#gps-layout .gf-step').innerText();
    // 272 of 22,402 logs read is about 1 %, not the 30 % the weights of the phases gave
    assert.equal(head, 'Finding the layout (1 %)');
    assert.match(step, /Reading the logs’ GPS frames: 272 of 22,402 logs/);
    assert.match(await page.locator('#gps-layout .gf-pct').innerText(), /^1 %$/);
    // the time left from the speed so far (272 logs in 55 s): about 1 h 19 min
    assert.match(step, /· about 1 h 1\d min left$/, step);
    assert.equal(await page.locator('#gps-layout .gf-bar').getAttribute('aria-valuenow'), '1');
  });
});

test('past fits: listed, opened, applied with the password, removed; on desktop and phone', { skip: !playwright && 'Playwright is not installed', timeout: 240000 }, async () => {
  await withPage(['--gps-fit', 'history'], async (page, server) => {
    await page.waitForSelector('#gps-fits .gp-row');
    const rows = page.locator('#gps-fits .gp-list:not(.gp-changes) > .gp-row');
    assert.equal(await rows.count(), 4);
    // newest first; the best layout with its errors in metres, good or weak, applied or not
    const t = await rows.allInnerTexts();
    assert.match(t[0], /Good/);
    assert.match(t[0], /14 days · 412 logs · 18,240 pairs/);
    assert.match(t[0], /5 m median · 14 m 95th pct/);
    assert.match(t[1], /Applied by admin/);
    assert.match(t[2], /Weak/);
    assert.match(t[3], /Far off/);
    assert.match(t[3], /410 m median · 650 m 95th pct/);
    for (const x of t) assert.ok(!/-30\.|29\.9|latitude: -?\d/.test(x), x);
    // the layouts used over time, newest first: when, by whom, the layout before
    const changes = await page.locator('#gps-fits .gp-changes > .gp-row').allInnerTexts();
    assert.equal(changes.length, 2);
    assert.match(changes[0], /by admin/);
    assert.match(changes[0], /before: lat 0\|27@1- \(2e-6,0\)/);
    assert.match(changes[1], /before: standard \(DBC\)/);

    // a row opens to its full result
    await rows.nth(3).locator('button', { hasText: 'Open' }).click();
    await page.waitForSelector('#gps-fits .gp-result .gf-row');
    const open = await rows.nth(3).innerText();
    assert.match(open, /Best layout found, but it is far off/);
    assert.match(open, /Latitude/);
    assert.match(open, /Longitude/);
    await rows.nth(3).locator('button', { hasText: 'Close' }).click();
    assert.equal(await page.locator('#gps-fits .gp-result').count(), 0);

    // apply the far-off one: the password asked again, a wrong one said in the dialog
    await rows.nth(3).locator('button', { hasText: 'Apply' }).click();
    const dlg = page.locator('dialog[open]');
    await dlg.waitFor();
    assert.match(await dlg.innerText(), /lon 28\|27@1- \(2e-6,0\)/);
    await dlg.locator('input[name="password"]').fill('not it');
    await dlg.locator('button[type="submit"]').click();
    await dlg.locator('.form-error:not([hidden])').waitFor();
    await dlg.locator('input[name="password"]').fill(PASSWORD);
    await dlg.locator('button[type="submit"]').click();
    await dlg.waitFor({ state: 'detached' });
    await page.waitForFunction(() => /Applied by admin/.test(document.querySelectorAll('#gps-fits .gp-list:not(.gp-changes) > .gp-row')[3]?.textContent || ''));
    await page.waitForFunction(() => document.querySelectorAll('#gps-fits .gp-changes > .gp-row').length === 3);
    assert.match(await page.locator('#gps-fits .gp-changes > .gp-row').first().innerText(), /from a fit/);

    // remove the newest: asked first, then gone
    await rows.nth(0).locator('button', { hasText: 'Remove' }).click();
    const ask = page.locator('dialog[open]');
    await ask.waitFor();
    await ask.locator('button', { hasText: 'Remove' }).click();
    await page.waitForFunction(() => document.querySelectorAll('#gps-fits .gp-list:not(.gp-changes) > .gp-row').length === 3);

    // a phone: nothing wider than the window
    await page.setViewportSize({ width: 390, height: 844 });
    await page.waitForTimeout(300);
    const wide = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    assert.ok(wide <= 0, `the page is ${wide}px wider than the phone`);
    const outside = await page.evaluate(() => [...document.querySelectorAll('#gps-fits .gp-row *')]
      .filter((e) => e.getBoundingClientRect().right > window.innerWidth + 1).map((e) => e.className).slice(0, 5));
    assert.deepEqual(outside, []);
    void server;
  });
});

test('a watch account sees no past fits', { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
  const browser = await playwright.chromium.launch();
  const server = await devServer(['--gps-fit', 'history', '--role', 'watch']);
  try {
    const page = await (await browser.newContext({ viewport: { width: 1440, height: 900 } })).newPage();
    await openHealth(page, server.origin);
    assert.equal(await page.locator('#gps-fits').count(), 0);
    assert.match(await page.locator('#gps-layout').innerText(), /GPS layout/);
  } finally {
    await browser.close();
    await server.stop();
  }
});
