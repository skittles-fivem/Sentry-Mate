// The Efficiency page of a car with no drives must not say "the — the pack gave while driving"
// (dev server with --cars 12, synthetic car c4). Made-up data only.
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

test('instrument: Efficiency of a car with no drives has no "the —" in its explanations', { skip: !playwright && 'no Playwright', timeout: 180000 }, async () => {
  const browser = await playwright.chromium.launch();
  const server = await devServer('instrument', ['--cars', '12']);
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    const page = await context.newPage();
    let answer = null;
    page.on('response', async (r) => { if (new URL(r.url()).pathname === '/api/v1/cars/c4/efficiency' && r.ok()) answer = await r.json().catch(() => null); });
    await page.goto(`${server.origin}/cars/c4/efficiency`);
    await page.waitForFunction(() => document.documentElement.dataset.ready === 'efficiency', null, { timeout: 30000 });
    await page.locator('.ef-cons-figs .explain').first().waitFor({ timeout: 10000 });
    const texts = await page.evaluate(() => [...document.querySelectorAll('.ef-cons-figs .explain')].map((n) => n.textContent));
    console.log('drives', answer?.drives, 'energy_kwh', answer?.energy_kwh);
    console.log(texts.join('\n'));
    const bad = texts.filter((t) => /the —/.test(t));
    assert.deepEqual(bad, [], `explanations with a blank figure in the sentence: ${bad.join(' | ')}`);
    await context.close();
  } finally {
    await server.stop();
    await browser.close();
  }
});
