// Pages at their edges, in Chromium on the dev API server: a link to a section of the page that is open lands
// below the top bar, as a link from another page does.
// `PACKS=a,b` runs only those. Run: node --test ui/tools/page-edges.test.js. Skipped when Playwright is not installed.
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

const ONLY = process.env.PACKS ? new Set(process.env.PACKS.split(',')) : null;
// a pack named here that is not built in (ui/tools/builtin-packs.js) drops out
const packs = (...list) => list.filter((p) => BUILTIN_PACKS.includes(p) && (!ONLY || ONLY.has(p)));
const BUILT_ON_INSTRUMENT = BUILTIN_PACKS.filter((p) => p === 'instrument');
const SKIP = !playwright && 'Playwright is not installed';

function devServer(pack, extra = []) {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', path.join(UI_DIR, 'packs', pack),
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

/** Runs `body({ origin, context })` with a dev server for the pack and a browser window of that size. */
async function withPack(pack, extra, body, viewport = { width: 1440, height: 900 }) {
  const browser = await playwright.chromium.launch();
  const server = await devServer(pack, extra);
  try {
    const context = await browser.newContext({ viewport });
    // nothing leaves the machine: a request to another host gets a 1 x 1 picture
    await context.route(/^https?:\/\/(?!127\.0\.0\.1)/, (r) => r.fulfill({ status: 200, contentType: 'image/png',
      body: Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==', 'base64') }));
    await body({ origin: server.origin, context });
    await context.close();
  } finally {
    await server.stop();
    await browser.close();
  }
}

/** Opens `url` and waits until the page says it is ready (any page id, or `id`). Returns { page, errors }. */
async function openPage(context, url, id = null, before = null) {
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  if (before) await before(page);
  await page.goto(url);
  await page.waitForFunction((id) => { const r = document.documentElement.dataset.ready; return r && (!id || r === id); }, id, { timeout: 30000 });
  return { page, errors };
}

const mainText = (page) => page.evaluate(() => document.querySelector('main').innerText);

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: a link to a section of the open page lands below the top bar`, { skip: SKIP, timeout: 120000 }, async () => {
    for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }]) {
      await withPack(pack, [], async ({ origin, context }) => {
        const { page, errors } = await openPage(context, `${origin}/help`, 'help');
        // Help's own link to its questions, then the footer's links to the licences and the data credits
        for (const [link, id] of [['main a[href$="#qa"]', 'qa'], ['a[href="/help#licences"]', 'licences'], ['a[href="/help#credits"]', 'credits']]) {
          await page.evaluate(() => window.scrollTo(0, 0));
          await page.locator(link).filter({ visible: true }).first().click();
          await page.waitForFunction((id) => location.hash === `#${id}`, id);
          await page.waitForTimeout(400);
          const at = await page.evaluate((id) => ({
            top: document.getElementById(id).getBoundingClientRect().top,
            bar: document.querySelector('.topbar').getBoundingClientRect().bottom,
            end: window.scrollY >= document.documentElement.scrollHeight - window.innerHeight - 1,
          }), id);
          assert.ok(at.top >= at.bar, `${viewport.width} px: #${id} starts at ${at.top}, under the top bar that ends at ${at.bar}`);
          assert.ok(at.top <= at.bar + 40 || at.end, `${viewport.width} px: #${id} starts at ${at.top}, far below the top bar`);
        }
        assert.deepEqual(errors, []);
      }, viewport);
    }
  });
}
