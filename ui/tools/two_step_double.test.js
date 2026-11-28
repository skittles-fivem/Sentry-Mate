// A double click on Settings: Server's "Turn on" (two-step sign-in) must start one set-up, not two
// stacked dialogs with different keys. Made-up data only.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawn } = require('child_process');

const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
let playwright = null;
try { playwright = require('playwright'); } catch { /* skip */ }

function devServer(pack, extra = []) {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', path.join(UI_DIR, 'packs', pack),
      '--port', '0', '--freeze-clock', ...extra], { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
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

test('instrument: a double click on two-step "Turn on" starts one set-up', { skip: !playwright && 'no Playwright', timeout: 180000 }, async () => {
  const browser = await playwright.chromium.launch();
  const server = await devServer('instrument', ['--latency-ms', '200']);
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    const page = await context.newPage();
    let starts = 0;
    page.on('request', (r) => { if (r.method() === 'POST' && new URL(r.url()).pathname === '/api/v1/me/two-step') starts += 1; });
    await page.goto(`${server.origin}/settings/server`);
    await page.waitForFunction(() => document.documentElement.dataset.ready === 'settings-server', null, { timeout: 30000 });
    const btn = page.locator('button', { hasText: /^Turn on$/ }).first();
    await btn.waitFor({ timeout: 30000 });
    await btn.dblclick();
    await page.locator('dialog[open]').first().waitFor({ timeout: 10000 });
    await page.waitForTimeout(1500);
    const keys = await page.evaluate(() => [...document.querySelectorAll('dialog[open] .ss-secret')].map((n) => n.textContent));
    assert.equal(starts, 1, `POST /me/two-step sent ${starts} times`);
    assert.equal(keys.length, 1, `open set-up dialogs: ${keys.length} (keys ${keys.join(' / ')})`);
    await context.close();
  } finally {
    await server.stop();
    await browser.close();
  }
});
