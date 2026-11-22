// The "UI pack" choice of Settings: Display and units in every built-in pack, in Chromium on the dev API server:
//   - it offers every pack the server lists (GET /api/v1/ui-packs: five built-in ones), the one in use chosen;
//   - choosing Pebble saves it, "Switch now" reloads, and the reloaded page is the Pebble pack.
// In the Pebble pack itself it first chooses Instrument, then Pebble again from there.
// Run: node --test ui/tools/pack-picker.test.js. Skipped when Playwright is not installed.
// PACK_PICKER_SHOTS=DIR also saves the picker at desktop and phone size, for a look by eye.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const { spawn } = require('child_process');

const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
const PACKS = ['instrument', 'logbook', 'horizon', 'console', 'pebble'];
const SHOTS = process.env.PACK_PICKER_SHOTS || null;
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

const ready = (page) => page.waitForFunction(() => document.documentElement.dataset.ready === 'settings-display', null, { timeout: 30000 });
const savedPack = (page) => page.waitForResponse((r) => r.url().endsWith('/api/v1/me/preferences') && r.request().method() === 'PATCH', { timeout: 15000 });
const indexOf = (pack) => fs.readFileSync(path.join(UI_DIR, 'packs', pack, 'web', 'index.html'), 'utf8');

// How each pack draws the choice: a <select>, a row of keys (Console), or a row that opens a sheet (Pebble).
async function choices(page, pack) {
  if (pack === 'console') return page.$$eval('[role="group"][aria-label="ui_pack"] button', (bs) => bs.map((b) => ({ v: b.dataset.v, on: b.getAttribute('aria-pressed') === 'true' })));
  if (pack === 'pebble') {
    await page.locator('button.set-pick[aria-label^="UI pack:"]').click();
    const radios = page.locator('[role="radiogroup"][aria-label="UI pack"] [role="radio"]');
    await radios.first().waitFor();
    const out = await radios.evaluateAll((rs) => rs.map((r) => ({ v: r.textContent.trim(), on: r.getAttribute('aria-checked') === 'true' })));
    await page.keyboard.press('Escape');
    await page.locator('[role="radiogroup"][aria-label="UI pack"]').waitFor({ state: 'detached' });
    return out;
  }
  return page.$$eval('select[aria-label="UI pack"] option', (os) => os.map((o) => ({ v: o.value, text: o.textContent, on: o.selected })));
}

async function choose(page, pack, id, name) {
  const saved = savedPack(page);
  if (pack === 'console') await page.locator(`[role="group"][aria-label="ui_pack"] button[data-v="${id}"]`).click();
  else if (pack === 'pebble') {
    await page.locator('button.set-pick[aria-label^="UI pack:"]').click();
    await page.locator('[role="radiogroup"][aria-label="UI pack"] [role="radio"]', { hasText: name }).click();
  } else await page.locator('select[aria-label="UI pack"]').selectOption(id);
  const r = await saved;
  assert.equal(r.status(), 200, `saving ui_pack ${id} in ${pack}`);
  assert.equal((await r.json()).ui_pack, id);
}

async function switchNow(page) {
  const b = page.getByRole('button', { name: /switch now/i });
  await b.waitFor({ state: 'visible' });
  const [nav] = await Promise.all([page.waitForNavigation(), b.click()]);
  await ready(page);
  return nav;
}

for (const pack of PACKS) {
  test(`${pack}: the UI pack choice lists every pack, and choosing Pebble serves Pebble`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const browser = await playwright.chromium.launch();
    const server = await devServer(pack);
    try {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', (e) => errors.push(e.message));
      await page.goto(`${server.origin}/settings/display`);
      await ready(page);
      const list = await choices(page, pack);
      assert.equal(list.length, 5, `${pack} offers ${JSON.stringify(list)}`);
      const on = list.filter((c) => c.on);
      assert.equal(on.length, 1);
      if (pack === 'pebble') assert.match(on[0].v, /^Pebble/);
      else assert.equal(on[0].v, pack);
      if (pack !== 'console' && pack !== 'pebble') assert.ok(list.every((c) => /: \S/.test(c.text)), 'each option has its short description');
      if (SHOTS) {
        fs.mkdirSync(SHOTS, { recursive: true });
        for (const [w, h, s] of [[1440, 900, 1], [390, 844, 2]]) {
          const c = await browser.newContext({ viewport: { width: w, height: h }, deviceScaleFactor: s });
          const p = await c.newPage();
          await p.goto(`${server.origin}/settings/display`);
          await ready(p);
          const el = pack === 'pebble' ? p.locator('button.set-pick[aria-label^="UI pack:"]')
            : pack === 'console' ? p.locator('[role="group"][aria-label="ui_pack"]') : p.locator('select[aria-label="UI pack"]');
          await el.scrollIntoViewIfNeeded();
          await p.screenshot({ path: path.join(SHOTS, `${pack}-${w}x${h}.png`) });
          await c.close();
        }
      }

      if (pack === 'pebble') {  // Pebble is in use here: go to Instrument first
        await choose(page, 'pebble', 'instrument', 'Instrument');
        const nav = await switchNow(page);
        assert.equal(await nav.text(), indexOf('instrument'));
      }
      const from = pack === 'pebble' ? 'instrument' : pack;
      await choose(page, from, 'pebble', 'Pebble');
      const nav = await switchNow(page);
      assert.equal(await nav.text(), indexOf('pebble'), 'the reloaded page is the Pebble pack');
      const about = await page.evaluate(() => fetch('/api/v1/about').then((r) => r.json()));
      assert.equal(about.ui_pack.name, 'Pebble');
      const again = await choices(page, 'pebble');
      assert.equal(again.length, 5);
      assert.match(again.find((c) => c.on).v, /^Pebble/);
      assert.deepEqual(errors, []);
      await context.close();
    } finally {
      await server.stop();
      await browser.close();
    }
  });
}
