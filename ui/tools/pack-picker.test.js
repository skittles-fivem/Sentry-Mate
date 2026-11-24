// The "UI pack" choice of Settings: Display and units in every built-in pack (Instrument alone: the other designs
// are archived in ui/archived), in Chromium on the dev API server:
//   - it offers every pack the server lists (GET /api/v1/ui-packs), the one in use chosen, each with its short
//     description;
//   - an archived pack can't be saved (422 on ui_pack), and the page and /about stay on the pack in use;
//   - choosing the pack in use again saves it and shows no "Switch now".
// Run: node --test ui/tools/pack-picker.test.js. Skipped when Playwright is not installed.
// PACK_PICKER_SHOTS=DIR also saves the picker at desktop and phone size, for a look by eye.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const { spawn } = require('child_process');

const { BUILTIN_PACKS } = require('./builtin-packs');
const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
const ARCHIVED = ['horizon', 'manager', 'console', 'logbook', 'pebble'];
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
const choices = (page) => page.$$eval('select[aria-label="UI pack"] option', (os) => os.map((o) => ({ v: o.value, text: o.textContent, on: o.selected })));

for (const pack of BUILTIN_PACKS) {
  test(`${pack}: the UI pack choice lists every built-in pack, and an archived one can't be chosen`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const browser = await playwright.chromium.launch();
    const server = await devServer(pack);
    try {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', (e) => errors.push(e.message));
      await page.goto(`${server.origin}/settings/display`);
      await ready(page);
      const list = await choices(page);
      assert.deepEqual(list.map((c) => c.v), BUILTIN_PACKS, `${pack} offers ${JSON.stringify(list)}`);
      const on = list.filter((c) => c.on);
      assert.equal(on.length, 1);
      assert.equal(on[0].v, pack);
      assert.ok(list.every((c) => /: \S/.test(c.text)), 'each option has its short description');
      if (SHOTS) {
        fs.mkdirSync(SHOTS, { recursive: true });
        for (const [w, h, s] of [[1440, 900, 2], [390, 844, 2]]) {
          const c = await browser.newContext({ viewport: { width: w, height: h }, deviceScaleFactor: s });
          const p = await c.newPage();
          await p.goto(`${server.origin}/settings/display`);
          await ready(p);
          await p.locator('select[aria-label="UI pack"]').scrollIntoViewIfNeeded();
          await p.screenshot({ path: path.join(SHOTS, `${pack}-picker-${w}x${h}.png`) });
          await c.close();
        }
      }

      // an archived pack is not on the server: refused, and the pack in use stays
      for (const gone of ARCHIVED) {
        const r = await page.evaluate((id) => fetch('/api/v1/me/preferences', { method: 'PATCH', headers: { 'Content-Type': 'application/json', 'X-Sentry-Mate': '1' }, body: JSON.stringify({ ui_pack: id }) }).then((x) => x.status), gone);
        assert.equal(r, 422, `${gone} can't be chosen`);
      }
      const about = await page.evaluate(() => fetch('/api/v1/about').then((r) => r.json()));
      assert.equal(about.ui_pack.id, pack);
      const [nav] = await Promise.all([page.waitForNavigation(), page.reload()]);
      await ready(page);
      assert.equal(await nav.text(), indexOf(pack), 'the reloaded page is the same pack');

      // choosing the pack in use again: saved, nothing to switch to
      const saved = savedPack(page);
      await page.locator('select[aria-label="UI pack"]').selectOption(pack);
      await page.locator('select[aria-label="UI pack"]').dispatchEvent('change');
      const r = await saved;
      assert.equal(r.status(), 200);
      assert.equal((await r.json()).ui_pack, pack);
      assert.equal(await page.getByRole('button', { name: /switch now/i }).isVisible(), false);
      assert.deepEqual(errors, []);
      await context.close();
    } finally {
      await server.stop();
      await browser.close();
    }
  });
}
