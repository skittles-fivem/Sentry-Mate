// A car named 64 characters with no spaces must not widen the page heads (Data health, Logbook,
// Software) at 1440x900: the page must not scroll sideways. Made-up data only.
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

async function rename(page, origin, car, name) {
  return page.evaluate(async ({ car, name }) => {
    const hd = { 'X-Sentry-Mate': '1', 'Content-Type': 'application/json' };
    const c = await (await fetch(`/api/v1/cars/${car}`, { headers: hd })).json();
    const r = await fetch(`/api/v1/cars/${car}`, { method: 'PATCH', headers: hd, body: JSON.stringify({ version: c.version, name }) });
    return r.status;
  }, { car, name });
}

const PAGES = [['health', 'health'], ['logbook', 'logbook'], ['software', 'software']];

for (const [label, name] of [['no spaces', 'W'.repeat(64)], ['with spaces', 'WWWWWWW '.repeat(8).trim()]]) {
  test(`instrument: a 64-character car name (${label}) does not widen the page at 1440x900`, { skip: !playwright && 'no Playwright', timeout: 180000 }, async () => {
    const browser = await playwright.chromium.launch();
    const server = await devServer('instrument');
    try {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
      const page = await context.newPage();
      await page.goto(`${server.origin}/cars/c2/health`);
      await page.waitForFunction(() => document.documentElement.dataset.ready === 'health', null, { timeout: 30000 });
      assert.equal(await rename(page, server.origin, 'c2', name), 200);
      const wide = [];
      for (const [seg, ready] of PAGES) {
        await page.goto(`${server.origin}/cars/c2/${seg}`);
        await page.waitForFunction((r) => document.documentElement.dataset.ready === r, ready, { timeout: 30000 });
        await page.waitForTimeout(300);
        const m = await page.evaluate(() => ({ sw: document.documentElement.scrollWidth, cw: document.documentElement.clientWidth }));
        console.log(label, seg, JSON.stringify(m));
        if (m.sw > m.cw) wide.push(`${seg}: scrollWidth ${m.sw} > ${m.cw}`);
      }
      assert.deepEqual(wide, [], wide.join('; '));
      await context.close();
    } finally {
      await server.stop();
      await browser.close();
    }
  });
}
