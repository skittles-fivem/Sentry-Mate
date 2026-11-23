// Online map tiles in every pack, in Chromium on the dev API server (which sends the real server's per-page
// Content-Security-Policy and Referrer-Policy: the online tile server is named only while it is on):
//   - an open page that was loaded with tiles offline, after an admin turns OpenStreetMap on, shows the tiles when
//     the person moves to a map inside the app (the page loads itself again, once, for the server's new policy);
//   - the same after a switch to another tile source (OpenTopoMap);
//   - every tile request carries a Referer with the page's origin only (OpenStreetMap blocks tiles without one);
//   - Settings: Display and units at 360 px with a long zone name: its zone button and tile terms fit the window;
//   - a new server (no map file, tiles offline): the car page's map says "No map tiles yet"; an admin's one click
//     (after the confirm) turns the default online map on (Esri satellite pictures with place names and roads) and the
//     map loads its tiles; the Map / Satellite switch then draws OpenStreetMap; a watch account is told to ask an admin;
//   - a tile server that refuses (403) gives a short note with the cause.
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

function devServer(pack, args = []) {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', path.join(UI_DIR, 'packs', pack),
      '--port', '0', '--freeze-clock', ...args], { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
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
          .filter((e) => e.children.length <= 3 && (/browser.s zone/i.test(e.textContent) || /free, no key/i.test(e.textContent) || e.textContent.includes(zone) || e.textContent.includes(zone.replace(/_/g, ' '))) && e.textContent.length < 300);
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

// A new server: no map file and offline tiles. Every map says so; an admin turns the online map on from the map
// itself, with the default preset (Esri satellite pictures with place names and roads over them); the Map / Satellite
// switch then draws OpenStreetMap instead.
const SAT_BASE = /^https:\/\/server\.arcgisonline\.com\/ArcGIS\/rest\/services\/World_Imagery\//;
const SAT_LABELS = /^https:\/\/server\.arcgisonline\.com\/ArcGIS\/rest\/services\/Reference\//;
const TILE_HOSTS = /^https:\/\/(server\.arcgisonline\.com|tile\.openstreetmap\.org)\//;
for (const pack of ['instrument', 'logbook', 'horizon', 'console', 'pebble']) {
  test(`${pack}: a map with no tiles says so, and an admin turns the satellite map on from it`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const browser = await playwright.chromium.launch({ args: ['--use-gl=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'] });
    const server = await devServer(pack, ['--map-tiles', 'none']);
    try {
      const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
      const tiles = [];
      const body = png();
      await context.route(TILE_HOSTS, (route) => { tiles.push(route.request().url()); return route.fulfill({ status: 200, contentType: 'image/png', body, headers: { 'access-control-allow-origin': '*' } }); });
      await context.route((u) => !u.href.startsWith(server.origin) && !TILE_HOSTS.test(u.href), (route) => route.abort());
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', (e) => errors.push(e.message));
      await page.goto(`${server.origin}/cars/c1`);
      const note = page.locator('.map-empty').first();
      await note.waitFor({ timeout: 30000 });
      assert.match(await note.textContent(), /No map tiles yet/);
      const use = note.getByRole('button', { name: 'Use online map (satellite)' });
      await use.waitFor({ timeout: 10000 });
      assert.equal(await note.getByRole('link', { name: 'Add an offline map file' }).getAttribute('href'), '/settings/display#offline-map');
      await use.click();
      // the confirm is drawn once maps/config is read: wait for it, not for the next frame
      await page.waitForFunction(() => document.querySelector('.map-empty')?.dataset.mapEmpty === 'confirm', null, { timeout: 10000 });
      assert.match(await note.textContent(), /server\.arcgisonline\.com/, 'the confirm names the tile server');
      assert.match(await note.textContent(), /sees the viewer.s IP address/, 'the confirm says what the tile server sees');
      assert.equal(tiles.length, 0, 'nothing asked before the confirm');
      await note.getByRole('button', { name: 'Turn on' }).click();
      const has = (re) => tiles.some((u) => re.test(u));
      for (let i = 0; i < 150 && !(has(SAT_BASE) && has(SAT_LABELS)); i++) await page.waitForTimeout(100);
      assert.ok(has(SAT_BASE), 'the map loads the satellite pictures after the confirm');
      assert.ok(has(SAT_LABELS), 'and the place names and roads over them');
      assert.ok(!tiles.some((u) => u.includes('openstreetmap')), 'no OpenStreetMap tiles while satellite is shown');
      assert.equal(await page.locator('.map-empty').count(), 0, 'no note once tiles are there');

      // the Map / Satellite switch: OpenStreetMap, drawn in place
      const toMap = page.locator('[data-map-style="osm"]').first();
      await toMap.waitFor({ timeout: 10000 });
      tiles.length = 0;
      await toMap.click();
      for (let i = 0; i < 150 && !tiles.some((u) => u.includes('tile.openstreetmap.org')); i++) await page.waitForTimeout(100);
      assert.ok(tiles.some((u) => u.includes('tile.openstreetmap.org')), 'the switch to Map loads OpenStreetMap tiles');
      assert.equal(await toMap.getAttribute('aria-pressed'), 'true');
      assert.deepEqual(errors, []);
      await context.close();
    } finally {
      await server.stop();
      await browser.close();
    }
  });

  test(`${pack}: a watch account is told to ask an admin; a refusing tile server gives its cause`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const browser = await playwright.chromium.launch({ args: ['--use-gl=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'] });
    const watch = await devServer(pack, ['--map-tiles', 'none', '--role', 'watch']);
    try {
      const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
      await context.route((u) => !u.href.startsWith(watch.origin), (route) => route.abort());
      const page = await context.newPage();
      await page.goto(`${watch.origin}/cars/c1`);
      const note = page.locator('.map-empty').first();
      await note.waitFor({ timeout: 30000 });
      await page.waitForFunction(() => /Ask an admin/.test(document.querySelector('.map-empty')?.textContent || ''), null, { timeout: 10000 });
      assert.equal(await note.getByRole('button').count(), 0, 'no switch for a watch account');
      await context.close();
    } finally {
      await watch.stop();
    }
    // the dev server starts online with the default preset (satellite): its tile server refuses every picture
    const server = await devServer(pack);
    try {
      const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
      await context.route(TILE_HOSTS, (route) => route.fulfill({ status: 403, contentType: 'text/plain', body: 'Access blocked', headers: { 'access-control-allow-origin': '*' } }));
      await context.route((u) => !u.href.startsWith(server.origin) && !TILE_HOSTS.test(u.href), (route) => route.abort());
      const page = await context.newPage();
      await page.goto(`${server.origin}/cars/c1`);
      const err = page.locator('[data-tile-error="403"]').first();
      await err.waitFor({ timeout: 30000 });
      assert.match(await err.textContent(), /server\.arcgisonline\.com refused them \(403\)/);
      await context.close();
    } finally {
      await server.stop();
      await browser.close();
    }
  });
}
