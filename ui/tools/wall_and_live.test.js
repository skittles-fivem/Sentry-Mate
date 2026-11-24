// The two-step set-up wall at car addresses, the Battery Now panel on a stale car, a live refresh that keeps what the
// reader unticked, print layout of wide tables and Drives' filters beyond what the server takes, in the Instrument
// pack, in Chromium on the dev API server. Run: node --test ui/tools/wall_and_live.test.js
// Skipped when Playwright is not installed.
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
const SKIP = !playwright && 'Playwright is not installed';

function devServer(extra = []) {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', path.join(UI_DIR, 'packs', 'instrument'),
      '--port', '0', ...extra], { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
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

async function withServer(extra, body, viewport = { width: 1280, height: 900 }) {
  const server = await devServer(extra);
  const browser = await playwright.chromium.launch();
  try {
    const context = await browser.newContext({ viewport });
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
const putRule = (page) => page.evaluate(async (pw) => (await fetch('/api/v1/two-step-policy', { method: 'PUT',
  headers: { 'X-Sentry-Mate': '1', 'Content-Type': 'application/json' }, body: JSON.stringify({ everyone: true, confirm_password: pw }) })).status, OWN);

const WALL = { hasText: 'Set up two-step sign-in' };

test('the set-up wall shows at a car address, by a deep link', { skip: SKIP, timeout: 120000 }, async () => {
  await withServer(['--freeze-clock'], async ({ origin, page }) => {
    await page.goto(`${origin}/`);
    await ready(page, 'fleet');
    assert.equal(await putRule(page), 200);
    await page.goto(`${origin}/cars/c1/drives`);
    await page.locator('main h1', WALL).waitFor({ timeout: 10000 });
    assert.equal(await page.locator('button', { hasText: /^\s*Sign out\s*$/ }).count(), 1);
    assert.equal(await page.getByText('Not allowed').count(), 0);
    // behind the wall the address's car is not read again
    const carCalls = [];
    page.on('request', (r) => { if (/\/api\/v1\/cars\/c1(\?|$)/.test(r.url())) carCalls.push(r.url()); });
    await page.evaluate(() => history.pushState({}, '', '/cars/c1/charges'));
    await page.evaluate(() => dispatchEvent(new PopStateEvent('popstate')));
    await page.locator('main h1', WALL).waitFor({ timeout: 10000 });
    await page.waitForTimeout(1000);
    assert.equal(await page.getByText('Not allowed').count(), 0);
    assert.deepEqual(carCalls, []);
  });
});

test('the set-up wall shows when the rule goes on while a car page is open', { skip: SKIP, timeout: 120000 }, async () => {
  await withServer(['--freeze-clock'], async ({ origin, page }) => {
    await page.goto(`${origin}/cars/c1/drives`);
    await ready(page, 'drives');
    assert.equal(await putRule(page), 200);
    await page.locator('a[href^="/cars/c1/charges"]').first().click();
    await page.locator('main h1', WALL).waitFor({ timeout: 10000 });
    assert.equal(await page.locator('button', { hasText: /^\s*Sign out\s*$/ }).count(), 1);
    assert.equal(await page.getByText('Not allowed').count(), 0);
  });
});

test('Battery: a stale car stays "Last known" when a now event comes', { skip: SKIP, timeout: 120000 }, async () => {
  await withServer([], async ({ origin, page }) => {
    await page.goto(`${origin}/cars/c3/battery`);
    await ready(page, 'battery');
    const side = () => page.evaluate(() => [...document.querySelectorAll('.bat-nowp span.faint')].map((e) => e.textContent).filter((x) => /Live|Last known/.test(x)).join(','));
    const seen = new Set();
    for (let i = 0; i < 8; i++) { seen.add(await side()); await page.waitForTimeout(1000); }
    assert.deepEqual([...seen], ['Last known']);
  });
});
