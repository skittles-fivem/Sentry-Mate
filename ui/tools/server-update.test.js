// A tab open across a server update, in every built-in pack, in Chromium on the dev API server:
//   - the tab shows Import; the server gets a new build (POST /__dev/new-build, the dev server's stand-in for an
//     update); the tab comes back into view; a link to Data health loads that address in full (not a module
//     import into the old page) and the page loads;
//   - a page module that fails to load once is loaded again with a full load of its address, and the page loads;
//   - one that keeps failing says "Sentry Mate was updated or a file could not load" (page_failed, never "No answer
//     from the server") with a Reload button, and the tab title and the trail name Data health, not Import.
// Run: node --test ui/tools/server-update.test.js. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawn } = require('child_process');

const { BUILTIN_PACKS } = require('./builtin-packs');
const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
const PACKS = (process.env.PACKS || BUILTIN_PACKS.join(',')).split(',');
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }

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

const IMPORT = '/settings/import';
const HEALTH = '/cars/c1/health';
const readyAs = (page, id) => page.waitForFunction((want) => document.documentElement?.dataset.ready === want, id, { timeout: 30000 });

// a link to Data health, clicked the way a person would (the router follows every same-origin link)
async function goToHealth(page) {
  await page.evaluate((href) => {
    const a = document.createElement('a');
    a.href = href;
    a.textContent = 'health';
    a.id = 'test-health-link';
    document.body.append(a);
    a.click();
  }, HEALTH);
}

async function openImport(page, origin) {
  await page.goto(origin + IMPORT);
  await readyAs(page, 'import');
  // a mark of this very document: a full load drops it
  await page.evaluate(() => { window.__sameDocument = true; });
}

const sameDocument = (page) => page.evaluate(() => window.__sameDocument === true);

for (const pack of PACKS) {
  test(`${pack}: a tab open across a server update loads the next page in full`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const server = await devServer(pack);
    const browser = await playwright.chromium.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
      const errors = [];
      page.on('pageerror', (e) => errors.push(String(e)));

      // 1. the server is updated while the tab shows Import
      await openImport(page, server.origin);
      const r = await page.request.post(server.origin + '/__dev/new-build');
      assert.equal(r.status(), 200);
      await page.evaluate(() => document.dispatchEvent(new Event('visibilitychange'))); // the tab comes back into view
      await page.waitForResponse((res) => res.url().endsWith('/health'), { timeout: 10000 }).catch(() => null);
      const loads = [];
      page.on('framenavigated', (f) => { if (f === page.mainFrame()) loads.push(f.url()); });
      await goToHealth(page);
      await readyAs(page, 'health');
      assert.equal(await sameDocument(page), false, 'the page after the update is a full load');
      assert.ok(loads.some((u) => new URL(u).pathname === HEALTH), `loaded ${HEALTH} in full: ${loads}`);
      assert.equal(new URL(page.url()).pathname, HEALTH);
      assert.equal(new URL(page.url()).search, '', 'the address is the one asked for');

      // 2. a page file that fails once: one more full load, and the page is there
      await openImport(page, server.origin);
      let blocked = 0;
      await page.route(/\/pages\/health\.js(\?|$)/, (route) => {
        if (blocked++ === 0) return route.abort('failed');
        return route.continue();
      });
      await goToHealth(page);
      await readyAs(page, 'health');
      assert.equal(blocked, 2, 'tried once more');
      assert.equal(await sameDocument(page), false, 'the second try is a full load');
      assert.equal(new URL(page.url()).pathname, HEALTH);
      await page.unroute(/\/pages\/health\.js(\?|$)/);

      // 3. one that keeps failing: the right words, and the frame names the page asked for
      await openImport(page, server.origin);
      const importTitle = await page.title();
      await page.route(/\/pages\/health\.js(\?|$)/, (route) => route.abort('failed'));
      await goToHealth(page);
      await readyAs(page, 'error:health');
      const box = page.locator('[role="alert"]').filter({ hasText: /Sentry Mate was updated or a file could not load/ });
      assert.equal(await box.count(), 1, `${pack}: ${await page.locator('body').innerText()}`);
      const text = await page.locator('body').innerText();
      assert.doesNotMatch(text, /No answer from the server|does not answer/i, `${pack}: ${text}`);
      const reload = box.locator('button', { hasText: /reload/i });
      assert.equal(await reload.count(), 1, `${pack}: a Reload button`);
      const title = await page.title();
      assert.notEqual(title, importTitle, `${pack}: the tab title moved on from Import`);
      assert.doesNotMatch(title, /^Import\b/, `${pack}: ${title}`);
      const crumbs = page.locator('.crumbs');
      if (await crumbs.count()) {
        const trail = await crumbs.first().innerText();
        assert.doesNotMatch(trail, /\bImport\b/, `${pack}: the trail still names Import: ${trail}`);
        assert.match(trail, /health/i, `${pack}: ${trail}`);
      }
      assert.equal(new URL(page.url()).pathname, HEALTH);
      // Reload loads the address in full; with the file back the page is there
      await page.unroute(/\/pages\/health\.js(\?|$)/);
      await reload.click();
      await readyAs(page, 'health');
      assert.equal(new URL(page.url()).search, '');
      assert.deepEqual(errors, [], `${pack}: no uncaught errors`);
    } finally {
      await browser.close();
      await server.stop();
    }
  });
}
