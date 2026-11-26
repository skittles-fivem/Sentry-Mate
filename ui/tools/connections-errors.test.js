// Settings: Integrations, Overview tab, in Chromium on the dev API server, every built-in pack (Instrument; the others
// are archived): a CAN Bridge row's "Errors 28 / 29" says what the errors were, by kind (the logs the server left out of
// the car's history: could not be signed, failed their check, ...), and counts apart the logs the server asked the car
// to send again (not errors). A kind with a page opens it (the car's unsigned logs). A row with errors the server named
// no kind for (counted before it kept the reason) says "not named"; the other rows say only how many.
// Run: node --test ui/tools/connections-errors.test.js. `PACKS=a,b` runs only those. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawn } = require('child_process');

const { BUILTIN_PACKS } = require('./builtin-packs');
const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }

const PACKS = (process.env.PACKS || BUILTIN_PACKS.join(',')).split(',').filter(Boolean);

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

/** The first CAN Bridge row's card text and the page, with the overview answer changed by `change(row)`. */
async function bridgeRow(context, origin, change) {
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  await page.route((u) => u.pathname === '/api/v1/integrations/overview', async (route) => {
    const res = await route.fetch();
    const body = await res.json();
    change(body.connections.find((c) => c.kind === 'smo'), body);
    await route.fulfill({ response: res, json: body });
  });
  await page.goto(`${origin}/settings/integrations`);
  await page.waitForFunction(() => document.documentElement.dataset.ready === 'settings-integrations', null, { timeout: 30000 });
  await page.waitForSelector('.co-row', { timeout: 30000 });
  return { page, errors };
}

for (const pack of PACKS) {
  test(`${pack}: a CAN Bridge's errors are named by kind, and logs asked of the car again are no error`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const browser = await playwright.chromium.launch();
    const server = await devServer(pack);
    try {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
      const named = (r) => {
        r.day = { count: 998, ok: 970, errors: 28, success_pct: 97.2, expected: 2,
          by_kind: [{ kind: 'could_not_sign', count: 20, link: `${r.link}#unsigned-logs` }, { kind: 'failed_check', count: 8, link: r.link }] };
        r.week = { count: 6000, ok: 5971, errors: 29, success_pct: 99.5, expected: 5,
          by_kind: [{ kind: 'could_not_sign', count: 21, link: `${r.link}#unsigned-logs` }, { kind: 'failed_check', count: 8, link: r.link }] };
      };
      const { page, errors } = await bridgeRow(context, server.origin, named);
      const cell = page.locator('.co-row').first().locator('.co-ok');
      await cell.locator('.co-kind').first().waitFor({ timeout: 30000 });
      const text = await cell.innerText();
      assert.match(text, /Errors 28 \/ 29/, `${pack}: the totals stay:\n${text}`);
      assert.match(text, /20 \/ 21 could not be signed/, `${pack}: the kinds are named:\n${text}`);
      assert.match(text, /8 \/ 8 failed their check/, text);
      assert.match(text, /2 \/ 5 asked of the car again/, `${pack}: what was no error is counted apart:\n${text}`);
      const title = await cell.getAttribute('title');
      assert.match(title, /2 asked of the car again \(not errors\)/, `${pack}: ${title}`);

      // a kind with a page opens it (the car's Data health, at its unsigned logs)
      await cell.locator('.co-kind', { hasText: 'could not be signed' }).click();
      await page.waitForFunction(() => location.pathname.endsWith('/health'), null, { timeout: 30000 });
      assert.equal(await page.evaluate(() => location.hash), '#unsigned-logs', `${pack}: where the kind opens`);
      assert.deepEqual(errors, []);
      await page.close();

      // errors the server named no kind for (before it kept the reason), and a row with none
      const unnamed = await bridgeRow(context, server.origin, (r) => {
        r.day = { count: 100, ok: 90, errors: 10, success_pct: 90, expected: 0, by_kind: [{ kind: 'unknown', count: 10, link: r.link }] };
        r.week = { count: 100, ok: 90, errors: 10, success_pct: 90, expected: 0, by_kind: [{ kind: 'unknown', count: 10, link: r.link }] };
      });
      const t2 = await unnamed.page.locator('.co-row').first().locator('.co-ok').innerText();
      assert.match(t2, /10 \/ 10 not named/, `${pack}: ${t2}`);
      await unnamed.page.close();
      const none = await bridgeRow(context, server.origin, (r) => {
        r.day = { count: 100, ok: 100, errors: 0, success_pct: 100, expected: 0, by_kind: [] };
        r.week = { count: 100, ok: 100, errors: 0, success_pct: 100, expected: 0, by_kind: [] };
      });
      const t3 = await none.page.locator('.co-row').first().locator('.co-ok').innerText();
      assert.match(t3, /No errors/, t3);
      assert.doesNotMatch(t3, /asked of the car|could not/, t3);
      assert.deepEqual([...errors, ...unnamed.errors, ...none.errors], []);
      await context.close();
    } finally {
      await server.stop();
      await browser.close();
    }
  });
}
