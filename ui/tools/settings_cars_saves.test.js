// Settings: Cars and the dialogs that ask for a password, in Chromium on the dev API server (admin), with the
// answers slowed or broken by the test:
//   - the keyboard focus stays on a control after a change is saved on a car's page (the page is drawn again);
//   - "Make upload token" pressed twice makes one token and shows one "shown once" dialog (a car's page and the list);
//   - "Prefer this key" and "Mark as sold" pressed twice send one request each;
//   - a dialog whose request runs stays open (Esc, Cancel and the close button do nothing) until the answer is in,
//     so its result (a new recovery key, a Tesla account added) is never lost;
//   - a save that fails (500, offline, 422) puts the field back to the saved value; a 422 keeps its message.
// Run: node --test ui/tools/settings_cars_saves.test.js. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawn } = require('child_process');

const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }

function devServer() {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', path.join(UI_DIR, 'packs', 'instrument'),
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

const wait = (ms) => new Promise((r) => setTimeout(r, ms));

// A fresh page on the dev server; `posts` lists the writes (method, path and body) the page sent.
async function open(browser, server, url, { slow = 0 } = {}) {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  const errors = [], posts = [];
  page.on('pageerror', (e) => errors.push(e.message));
  page.on('request', (r) => { if (r.url().includes('/api/') && r.method() !== 'GET') posts.push(`${r.method()} ${new URL(r.url()).pathname} ${r.postData() || ''}`.trim()); });
  if (slow) {
    await page.route('**/api/v1/**', async (route) => {
      if (route.request().method() !== 'GET') await wait(slow);
      await route.continue();
    });
  }
  await page.goto(server.origin + url);
  await page.waitForFunction(() => /^settings-/.test(document.documentElement.dataset.ready || ''), null, { timeout: 30000 });
  return { page, context, errors, posts };
}
const count = (posts, re) => posts.filter((p) => re.test(p)).length;
const active = (page) => page.evaluate(() => { const a = document.activeElement; return a && a !== document.body ? `${a.tagName}${a.name ? `[${a.name}]` : ''}` : 'BODY'; });
const dialogs = (page) => page.locator('dialog[open]').count();

async function withServer(fn) {
  const browser = await playwright.chromium.launch();
  const server = await devServer();
  try { await fn(browser, server); } finally { await browser.close(); await server.stop(); }
}

const skip = !playwright && 'Playwright is not installed';

test('a car\'s page: the keyboard focus stays after a change is saved', { skip, timeout: 120000 }, async () => {
  await withServer(async (browser, server) => {
    const { page, context, errors, posts } = await open(browser, server, '/settings/cars/c1');
    const tz = page.locator('input[name="sc-tz"]');
    await tz.focus();
    await tz.fill('Europe/Amsterdam');
    await page.keyboard.press('Tab');
    await page.waitForFunction(() => /Time zone saved/.test(document.body.innerText), null, { timeout: 10000 });
    await wait(300);
    assert.equal(count(posts, /PATCH .*\/cars\/c1 /), 1, posts.join('\n'));
    assert.notEqual(await active(page), 'BODY', 'the focus is on a control after the page was drawn again');
    await page.keyboard.press('Tab');
    assert.notEqual(await active(page), 'BODY', 'Tab goes on from where it was');
    assert.deepEqual(errors, []);
    await context.close();
  });
});

test('Make upload token pressed twice makes one token and shows one dialog', { skip, timeout: 120000 }, async () => {
  await withServer(async (browser, server) => {
    for (const url of ['/settings/cars/c1', '/settings/cars']) {
      const { page, context, errors, posts } = await open(browser, server, url, { slow: 600 });
      await page.locator('button:has-text("Make upload token")').first().dblclick();
      await wait(2500);
      assert.equal(count(posts, /POST .*upload-tokens/), 1, `${url}: ${posts.join('\n')}`);
      assert.equal(await dialogs(page), 1, `${url}: one dialog`);
      await page.keyboard.press('Escape');
      await wait(300);
      assert.equal(await page.locator('dialog').count(), 0, `${url}: one Esc closes it`);
      assert.deepEqual(errors, []);
      await context.close();
    }
  });
});

test('Prefer this key and Mark as sold pressed twice send one request each', { skip, timeout: 120000 }, async () => {
  await withServer(async (browser, server) => {
    const { page, context, errors, posts } = await open(browser, server, '/settings/cars/c1', { slow: 600 });
    const prefer = page.locator('button:has-text("Prefer this key")').first();
    assert.ok(await prefer.count(), 'the car has a key that is not preferred');
    await prefer.dblclick();
    await wait(2500);
    assert.equal(count(posts, /PATCH .*can-keys/), 1, posts.join('\n'));
    const n = posts.length;
    await page.locator('button:has-text("Mark as sold")').dblclick();
    await wait(2500);
    assert.equal(count(posts.slice(n), /PATCH .*\/cars\/c1 /), 1, posts.slice(n).join('\n'));
    assert.deepEqual(errors, []);
    await context.close();
  });
});

test('a dialog whose request runs stays open: a new recovery key is not lost', { skip, timeout: 120000 }, async () => {
  await withServer(async (browser, server) => {
    const { page, context, errors } = await open(browser, server, '/settings/server', { slow: 1800 });
    await page.click('button:has-text("Make a new recovery key")');
    await page.fill('dialog input[name=password]', 'sentry-mate-dev');
    await page.keyboard.press('Enter');
    await wait(400);
    assert.equal(await page.locator('dialog button[type=submit]').getAttribute('aria-busy'), 'true');
    await page.keyboard.press('Escape');
    await wait(200);
    assert.equal(await dialogs(page), 1, 'Esc does nothing while the request runs');
    assert.ok(await page.locator('dialog button:has-text("Cancel")').isDisabled(), 'Cancel is disabled');
    assert.ok(await page.locator('dialog button[aria-label="Close"]').isDisabled(), 'the close button is disabled');
    await page.waitForFunction(() => document.querySelector('dialog .mv-key'), null, { timeout: 15000 });
    assert.deepEqual(errors, []);
    await context.close();
  });
});

test('adding a Tesla account: Esc while it runs does not lose the answer', { skip, timeout: 120000 }, async () => {
  await withServer(async (browser, server) => {
    const { page, context, errors } = await open(browser, server, '/settings/integrations?tab=details');
    await page.waitForSelector('button:has-text("Paste a refresh token")');
    const before = await page.locator('.ta-account').count();
    await page.route('**/api/v1/**/tesla/accounts', async (route) => { if (route.request().method() === 'POST') await wait(1500); await route.continue(); });
    await page.click('button:has-text("Paste a refresh token")');
    await page.fill('dialog textarea[name=refresh_token]', 'abc');
    await page.fill('dialog input[name=password]', 'sentry-mate-dev');
    await page.click('dialog button[type=submit]');
    await wait(300);
    await page.keyboard.press('Escape');
    await page.waitForFunction((n) => document.querySelectorAll('.ta-account').length > n && !document.querySelector('dialog[open]'), before, { timeout: 15000 });
    assert.deepEqual(errors, []);
    await context.close();
  });
});

test('a save that fails puts the field back to the saved value', { skip, timeout: 180000 }, async () => {
  await withServer(async (browser, server) => {
    const kinds = {
      500: (route) => route.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify({ error: { code: 'internal', message: 'Boom.' } }) }),
      offline: (route) => route.abort('internetdisconnected'),
      422: (route) => route.fulfill({ status: 422, contentType: 'application/json', body: JSON.stringify({ error: { code: 'invalid', message: 'bad', fields: { name: 'Name is taken.', time_zone: 'Not a zone.', capacity_new_kwh: 'Not that.' } } }) }),
    };
    for (const [kind, answer] of Object.entries(kinds)) {
      const { page, context, errors } = await open(browser, server, '/settings/cars/c1');
      await page.route('**/api/v1/cars/c1', (route) => (route.request().method() === 'PATCH' ? answer(route) : route.continue()));
      await page.route('**/api/v1/cars/c1/settings', (route) => (route.request().method() === 'PATCH' ? answer(route) : route.continue()));
      for (const [name, typed] of [['sc-name', 'Zed'], ['sc-tz', 'Europe/Paris'], ['sc-capacity_new_kwh', '77']]) {
        const input = page.locator(`input[name="${name}"]`);
        const saved = await input.inputValue();
        await input.fill(typed);
        await input.blur();
        await page.waitForFunction(([n, v]) => document.querySelector(`input[name="${n}"]`)?.value === v, [name, saved], { timeout: 5000 })
          .catch(() => assert.fail(`${kind}: ${name} still shows ${typed} instead of ${JSON.stringify(saved)}`));
        if (kind === '422') {
          const msg = await page.locator(`input[name="${name}"]`).evaluate((i) => i.closest('.form-row').querySelector('.form-error')?.textContent.trim() || '');
          assert.ok(msg, `422: ${name} keeps a message`);
        }
      }
      assert.deepEqual(errors.filter((e) => !/Failed to load resource|ERR_/.test(e)), []);
      await context.close();
    }
  });
});
