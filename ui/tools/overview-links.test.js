// The connections overview's links in Chromium on the dev API server, in all six packs: the rows for the online
// map tiles, the address lookup and the release check name where each is changed
// (`/settings/display#maps`, `#addresses`, `#release`, as the real server sends them), and each opens the section
// where it lives now (shared/state.js MOVED: `#map-tiles` and `#address-lookup` of Settings: Maps and addresses,
// `#release-check` of Settings: Server and access), drawn and scrolled to.
// Run: node --test ui/tools/overview-links.test.js. `PACKS=a,b` runs only those. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawn } = require('child_process');

const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }

const PACKS = (process.env.PACKS || 'instrument,horizon,manager,logbook,console,pebble').split(',').filter(Boolean);
// the link the real server sends, and where the pack opens it: the page, its address and the section's id
const LINKS = {
  tiles: { link: '/settings/display#maps', page: 'settings-maps', at: '/settings/maps#map-tiles' },
  geocoding: { link: '/settings/display#addresses', page: 'settings-maps', at: '/settings/maps#address-lookup' },
  release_check: { link: '/settings/display#release', page: 'settings-server', at: '/settings/server#release-check' },
};

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

for (const pack of PACKS) {
  test(`${pack}: the overview's links for map tiles, address lookup and release check open their section`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const browser = await playwright.chromium.launch();
    const server = await devServer(pack);
    try {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
      const overview = await (await context.request.get(`${server.origin}/api/v1/integrations/overview`)).json();
      const rows = overview.connections || overview.items;
      for (const [kind, want] of Object.entries(LINKS)) {
        const row = rows.find((r) => r.kind === kind);
        assert.ok(row, `the overview has a ${kind} row`);
        assert.equal(row.link, want.link, `${kind}: the link the real server sends`);
        const page = await context.newPage();
        const errors = [];
        page.on('pageerror', (e) => errors.push(e.message));
        await page.goto(server.origin + row.link);
        await page.waitForFunction((p) => document.documentElement.dataset.ready === p, want.page, { timeout: 30000 })
          .catch(() => assert.fail(`${pack}: ${row.link} does not open ${want.page}`));
        assert.equal(await page.evaluate(() => location.pathname + location.hash), want.at, `${pack}: where ${row.link} opens`);
        const id = want.at.split('#')[1];
        const box = await page.waitForFunction((id) => {
          const el = document.getElementById(id);
          const r = el?.getBoundingClientRect();
          return r && r.height > 0 ? { top: r.top, bottom: r.bottom, vh: window.innerHeight } : null;
        }, id, { timeout: 15000 }).then((x) => x.jsonValue()).catch(() => null);
        assert.ok(box, `${pack}: ${row.link} has a drawn element with id "${id}"`);
        // scrolled to: its top is in the window once the page has settled
        await page.waitForFunction((id) => { const r = document.getElementById(id).getBoundingClientRect(); return r.top >= -1 && r.top < window.innerHeight - 40; }, id, { timeout: 10000 })
          .catch(() => assert.fail(`${pack}: ${row.link} is not scrolled into the window`));
        assert.deepEqual(errors, []);
        await page.close();
      }
      await context.close();
    } finally {
      await server.stop();
      await browser.close();
    }
  });
}
