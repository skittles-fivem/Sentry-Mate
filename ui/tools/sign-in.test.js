// Signing in, in Chromium on the dev API server (`--signed-out`): the sign-in screen shows its card alone (no
// section links, car switcher or account menu of the app behind it), and the page that was asked for is opened
// after signing in at the section its address names (`/help#licences`), also when a session ends on such a page.
// Each test names the packs it runs on; `PACKS=a,b` runs only those.
// Run: node --test ui/tools/sign-in.test.js. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawn } = require('child_process');

const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }

const { BUILTIN_PACKS } = require('./builtin-packs');
const ONLY = process.env.PACKS ? new Set(process.env.PACKS.split(',')) : null;
// a pack named here that is not built in (ui/tools/builtin-packs.js) drops out
const packs = (...list) => list.filter((p) => BUILTIN_PACKS.includes(p) && (!ONLY || ONLY.has(p)));
const BUILT_ON_INSTRUMENT = ['instrument', 'horizon', 'manager'];
const ALL = [...BUILT_ON_INSTRUMENT, 'console', 'pebble', 'logbook'];
// Help has a section with the id "licences" in these; in Pebble and Logbook only the address is checked
const HAS_LICENCES = new Set([...BUILT_ON_INSTRUMENT, 'console']);
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

/** Runs `body({ origin, context })` with a signed-out dev server for the pack and a desktop browser window. */
async function withPack(pack, body) {
  const browser = await playwright.chromium.launch();
  const server = await devServer(pack, ['--signed-out']);
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
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

const ready = (page, id) => page.waitForFunction((id) => document.documentElement.dataset.ready === id, id, { timeout: 30000 });
const address = (page) => page.evaluate(() => location.pathname + location.search + location.hash);

/** Types the dev server's admin account into the sign-in form and sends it. */
async function signIn(page) {
  await page.fill('input[name="name"]', 'admin');
  await page.fill('input[name="password"]', 'sentry-mate-dev');
  await page.keyboard.press('Enter');
}

/** Waits until the Licences section of Help is at the top part of the window. */
const atLicences = (page) => page.waitForFunction(() => {
  const e = document.getElementById('licences');
  if (!e || !window.scrollY) return false;
  const top = e.getBoundingClientRect().top;
  return top >= 0 && top < window.innerHeight / 2;
}, null, { timeout: 15000 });

for (const pack of packs(...BUILT_ON_INSTRUMENT)) {
  test(`${pack}: the sign-in screen shows its card alone, nothing of the app's menus`, { skip: SKIP, timeout: 120000 }, async () => {
    await withPack(pack, async ({ origin, context }) => {
      const page = await context.newPage();
      await page.goto(`${origin}/sign-in`);
      await ready(page, 'sign-in');
      // every link and button that can be seen and is not inside the page's own part
      const outside = await page.evaluate(() => [...document.querySelectorAll('a[href], button')]
        .filter((e) => !e.closest('main') && !e.classList.contains('skip-link') && !e.closest('.skip-link, .skip'))
        .filter((e) => { const r = e.getBoundingClientRect(); const s = getComputedStyle(e); return r.width > 1 && r.height > 1 && s.visibility !== 'hidden' && r.bottom > 0 && r.right > 0; })
        .map((e) => (e.getAttribute('aria-label') || e.textContent || e.getAttribute('href') || '').trim().replace(/\s+/g, ' ').slice(0, 40)));
      assert.deepEqual(outside, [], 'no control of the app is shown to someone who is not signed in');
      await page.close();
    });
  });
}

for (const pack of packs(...ALL)) {
  test(`${pack}: signing in opens the page that was asked for at the section its address names`, { skip: SKIP, timeout: 180000 }, async () => {
    await withPack(pack, async ({ origin, context }) => {
      const page = await context.newPage();
      await page.goto(`${origin}/help#licences`);
      await ready(page, 'sign-in');
      assert.equal(await address(page), `/sign-in?next=${encodeURIComponent('/help#licences')}`, 'the address to come back to keeps its section');
      await signIn(page);
      await ready(page, 'help');
      assert.equal(await address(page), '/help#licences');
      if (HAS_LICENCES.has(pack)) await atLicences(page);

      // the session ends while that page is open: the same again
      await page.evaluate(async () => {
        await fetch('/api/v1/session', { method: 'DELETE', headers: { 'X-Sentry-Mate': '1' } });
        const api = await import('/shared/api.js');
        await api.get('me', null, { maxAgeMs: 0 }).catch(() => {});
      }).catch(() => {}); // (Pebble and Logbook load the sign-in screen in full, which ends this call)
      await ready(page, 'sign-in');
      assert.equal(await address(page), `/sign-in?next=${encodeURIComponent('/help#licences')}`, 'a session that ended keeps the section too');
      await signIn(page);
      await ready(page, 'help');
      assert.equal(await address(page), '/help#licences');
      if (HAS_LICENCES.has(pack)) await atLicences(page);
      await page.close();
    });
  });
}
