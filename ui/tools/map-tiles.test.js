// Online map tiles in every pack, in Chromium on the dev API server (which sends the real server's per-page
// Content-Security-Policy and Referrer-Policy: the online tile server is named only while it is on):
//   - an open page that was loaded with tiles offline, after an admin turns OpenStreetMap on, shows the tiles when
//     the person moves to a map inside the app (the page loads itself again, once, for the server's new policy);
//   - the same after a switch to another tile source (OpenTopoMap);
//   - every tile request carries a Referer with the page's origin only (OpenStreetMap blocks tiles without one);
//   - Settings: Display and units at 360 px with a long zone name: its zone button and tile terms fit the window.
// No request leaves the machine: the tile servers are answered by page.route.
// Run: node --test ui/tools/map-tiles.test.js. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const zlib = require('zlib');
const { spawn } = require('child_process');

const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
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

// A 256 x 256 grey PNG, made here.
function png() {
  const crcTable = Array.from({ length: 256 }, (_, n) => { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; return c >>> 0; });
  const crc = (b) => { let c = 0xffffffff; for (const x of b) c = crcTable[(c ^ x) & 0xff] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0; };
  const chunk = (type, data) => {
    const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type), data]);
    const c = Buffer.alloc(4); c.writeUInt32BE(crc(td));
    return Buffer.concat([len, td, c]);
  };
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(256, 0); ihdr.writeUInt32BE(256, 4); ihdr[8] = 8; ihdr[9] = 0;
  const raw = Buffer.alloc(257 * 256, 0xc8); for (let y = 0; y < 256; y++) raw[y * 257] = 0;
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk('IHDR', ihdr), chunk('IDAT', zlib.deflateSync(raw)), chunk('IEND', Buffer.alloc(0))]);
}

const SOURCES = {
  osm: { mode: 'online', online_url: 'https://tile.openstreetmap.org/{z}/{x}/{y}.png', attribution: '© OpenStreetMap contributors', max_zoom: 19 },
  topo: { mode: 'online', online_url: 'https://tile.opentopomap.org/{z}/{x}/{y}.png', attribution: '© OpenStreetMap contributors, © OpenTopoMap', max_zoom: 17 },
};

for (const pack of ['instrument', 'logbook', 'horizon', 'console', 'pebble']) {
  test(`${pack}: online tiles load after they are turned on or switched, without a reload by hand`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const browser = await playwright.chromium.launch({ args: ['--use-gl=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'] });
    const server = await devServer(pack);
    try {
      const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
      const tiles = [];
      const body = png();
      await context.route(/^https:\/\/tile\.(openstreetmap|opentopomap)\.org\//, (route) => {
        const r = route.request();
        tiles.push({ host: new URL(r.url()).host, referer: r.headers().referer || null });
        return route.fulfill({ status: 200, contentType: 'image/png', body, headers: { 'access-control-allow-origin': '*' } });
      });
      // nothing else may leave the machine
      await context.route((u) => !u.href.startsWith(server.origin) && !/^https:\/\/tile\.(openstreetmap|opentopomap)\.org\//.test(u.href), (route) => route.abort());
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', (e) => errors.push(e.message));
      let loads = 0;
      page.on('load', () => { loads++; });
      const api = (method, p, b) => page.evaluate(async ([method, p, b]) => {
        const r = await fetch(`/api/v1/${p}`, { method, headers: { 'content-type': 'application/json', 'X-Sentry-Mate': '1' }, body: b ? JSON.stringify(b) : undefined });
        return [r.status, await r.json().catch(() => null)];
      }, [method, p, b]);
      const setTiles = async (t) => {
        const s = (await api('GET', 'settings'))[1];
        const [st] = await api('PATCH', 'settings', { version: s.version, tiles: t });
        assert.equal(st, 200);
      };
      const toCar = async () => {
        // a move inside the app (no page load by the test): a click on a link to the car's page, which has a map
        await page.evaluate(() => { let a = document.querySelector('a[href="/cars/c1"], a[href^="/cars/c1?"]'); if (!a) { a = document.createElement('a'); a.href = '/cars/c1'; document.body.append(a); } a.click(); });
      };
      const waitTiles = async (host) => {
        for (let i = 0; i < 150 && !tiles.some((t) => t.host === host); i++) await page.waitForTimeout(100);
      };

      await page.goto(`${server.origin}/`);
      await setTiles({ mode: 'offline' });
      await page.goto(`${server.origin}/settings/display`);
      await page.waitForFunction(() => document.documentElement.dataset.ready === 'settings-display', null, { timeout: 30000 });
      const before = loads;

      // an admin turns OpenStreetMap on; the open page was loaded with tiles offline
      await setTiles(SOURCES.osm);
      await toCar();
      await waitTiles('tile.openstreetmap.org');
      assert.ok(tiles.some((t) => t.host === 'tile.openstreetmap.org'), 'OpenStreetMap tiles were asked for');
      assert.equal(loads - before, 1, 'the page loaded itself again once');
      assert.match(page.url(), /\/cars\/c1/);

      // then another source, again from a page loaded with the old policy
      tiles.length = 0;
      await page.goto(`${server.origin}/settings/display`);
      await page.waitForFunction(() => document.documentElement.dataset.ready === 'settings-display', null, { timeout: 30000 });
      await setTiles(SOURCES.topo);
      tiles.length = 0;
      await toCar();
      await waitTiles('tile.opentopomap.org');
      assert.ok(tiles.some((t) => t.host === 'tile.opentopomap.org'), 'OpenTopoMap tiles were asked for');
      assert.ok(!tiles.some((t) => t.host === 'tile.openstreetmap.org'), 'no OpenStreetMap tiles after the switch');

      for (const t of tiles) assert.equal(t.referer, `${server.origin}/`, 'the Referer is the page origin, nothing more');
      assert.deepEqual(errors, []);
      await context.close();
    } finally {
      await server.stop();
      await browser.close();
    }
  });
}

// Settings: Display and units at phone width, with a long browser zone name: the "Use this browser's zone" button
// and the tile source's terms note stay inside the window (they ran up to 88 px past it).
for (const pack of ['instrument', 'logbook', 'horizon', 'console', 'pebble']) {
  test(`${pack}: the zone button and the tile terms fit a 360 px phone`, { skip: !playwright && 'Playwright is not installed', timeout: 120000 }, async () => {
    const browser = await playwright.chromium.launch();
    const server = await devServer(pack);
    try {
      const context = await browser.newContext({ viewport: { width: 360, height: 800 }, timezoneId: 'America/Argentina/Buenos_Aires' });
      await context.route((u) => !u.href.startsWith(server.origin), (route) => route.abort());
      const page = await context.newPage();
      await page.goto(`${server.origin}/settings/display`);
      await page.waitForFunction(() => document.documentElement.dataset.ready === 'settings-display', null, { timeout: 30000 });
      const out = await page.evaluate(() => {
        const W = document.documentElement.clientWidth;
        const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
        const mine = [...document.querySelectorAll('button, p, span, small, div')]
          .filter((e) => e.children.length <= 3 && (/browser.s zone/i.test(e.textContent) || /free, no key/.test(e.textContent) || e.textContent.includes(zone)) && e.textContent.length < 300);
        return { found: mine.length, wide: mine.filter((e) => e.getBoundingClientRect().right > W + 0.5).map((e) => `${e.tagName} ${e.textContent.trim().slice(0, 60)} (${Math.round(e.getBoundingClientRect().right)} px)`) };
      });
      assert.ok(out.found > 0, 'the zone button or the terms note is on the page');
      assert.deepEqual(out.wide, [], 'past the right edge of a 360 px window');
      await context.close();
    } finally {
      await server.stop();
      await browser.close();
    }
  });
}
