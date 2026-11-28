// The re-dating of a TeslaFi account whose car is not matched: the server answers { cars: [], errors: [] } (no car
// moved, none was done before), and the page says so, not "That was done before". Made-up data only.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawn } = require('child_process');

const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
let playwright = null;
try { playwright = require('playwright'); } catch { /* skip */ }

function devServer(pack) {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', path.join(UI_DIR, 'packs', pack),
      '--port', '0', '--freeze-clock'], { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
    let out = '', err = '';
    const timer = setTimeout(() => { child.kill('SIGKILL'); reject(new Error(`no dev server: ${err}`)); }, 60000);
    child.stderr.on('data', (d) => { err = (err + d).slice(-5000); });
    child.stdout.on('data', (d) => {
      out += d;
      const m = /listening on (http:\/\/\S+?)\/?\s/.exec(out);
      if (m) { clearTimeout(timer); resolve({ origin: m[1], stop: () => new Promise((r) => { child.once('exit', r); child.kill('SIGTERM'); }) }); }
    });
    child.once('exit', (code) => { clearTimeout(timer); reject(new Error(`dev server stopped (${code}): ${err}`)); });
  });
}

test('instrument: re-dating an account with no matched car does not say it was done before', { skip: !playwright && 'no Playwright', timeout: 180000 }, async () => {
  const browser = await playwright.chromium.launch();
  const server = await devServer('instrument');
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    const page = await context.newPage();
    let posted = false;
    await page.route((u) => /\/api\/v1\/integrations\/teslafi\/accounts\/[^/]+\/redate$/.test(u.pathname), async (route) => {
      if (route.request().method() !== 'POST') return route.continue();
      posted = true;
      await route.fulfill({ json: { zone_before: 'UTC', zone: 'Europe/Berlin', before_ms: Date.now() - 3600000, cars: [], errors: [] } });
    });
    await page.goto(`${server.origin}/settings/integrations?tab=details#teslafi`);
    await page.waitForFunction(() => document.documentElement.dataset.ready === 'settings-integrations', null, { timeout: 30000 });
    const btn = page.locator('button', { hasText: 'Re-date stored data' }).first();
    await btn.waitFor({ timeout: 30000 });
    await btn.click();
    const dlg = page.locator('dialog[open]');
    await dlg.waitFor({ timeout: 10000 });
    await dlg.locator('input[name="password"]').fill('made-up-password');
    await dlg.locator('button[type="submit"]').click();
    await page.waitForSelector('.flash-note', { timeout: 10000 });
    const text = await page.evaluate(() => [...document.querySelectorAll('.flash-note')].map((n) => n.innerText).join('\n'));
    assert.ok(posted, 'the POST was sent');
    assert.doesNotMatch(text, /done before/, `the toast for cars: [] says: ${text}`);
    assert.match(text, /no car of this account is matched/);
    await context.close();
  } finally {
    await server.stop();
    await browser.close();
  }
});
