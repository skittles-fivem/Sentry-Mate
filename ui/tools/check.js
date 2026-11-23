#!/usr/bin/env node
// Sentry Mate UI page checker.
//
//   node ui/tools/check.js --pack instrument [--pages fleet,car,...|all] [--sizes 1440x900,3840x2160]
//        [--out DIR] [--server-args "<dev server flags>"] [--json] [--allow-missing] [--full]
//        [--fixture-matrix FILE] [--ready-timeout-ms N] [--idle-timeout-ms N] [--jobs N]
//        [--car ID] [--drive ID] [--charge ID] [--place ID] [--pixel] [--stable [SECONDS]] [--stable-fast] [--hover]
//        [--base https://HOST:PORT --user NAME (--password PW | --password-file FILE)]
//   node ui/tools/check.js --self-test
//
// It starts the dev API server (ui/devserver/serve.py) on a free port with the pack, opens every chosen
// page at every chosen size in Chromium (Playwright), waits for the page to say it is ready
// (document.documentElement.dataset.ready = '<page id>'), and reports per page and size: coverage items
// that are missing, marked on the wrong page or unknown; console errors; page errors; Content Security
// Policy violations; failed /api requests; requests to other hosts; horizontal overflow; visible text
// under 10 CSS px. It saves a screenshot of each. Exit code 0 only when every checked page passes. Before it
// measures it scrolls through the page and back, so parts drawn only when they come near the screen are there.
// With --pixel it also measures whether the page is pixel centred and correct (ui/tools/pixel.js): text and
// icons off centre, rows and columns that just miss, borders off device pixels, text cut off, sticking out,
// overlapping or hidden, sideways scroll; and saves a screenshot with the bad boxes outlined.
// With --stable it stays on each ready page while the dev server sends live updates (ui/tools/stable.js) and
// fails a page whose layout shifts, whose blocks move or whose elements and loading texts come and go.
// With --hover it moves the mouse over a grid of points, every interactive element and every chart, focuses every
// element that takes focus (ui/tools/hover.js), and fails a page where anything outside what is hovered moves.
// With --base it starts no dev server: it signs in to a real `sentry-mate serve` (made-up data only),
// accepting that server's self-signed certificate for this run only.
// See ui/tools/README.md. Plain Node (CommonJS) plus Playwright; nothing else.

'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const http = require('http');
const https = require('https');
const zlib = require('zlib');
const { spawn } = require('child_process');
const { pixelChecks, pixelOutline, pixelOutlineRemove } = require('./pixel');
const { stableInit, stableCollect, stableOutline, stableOutlineRemove } = require('./stable');
const { hoverRun } = require('./hover');

const TOOLS_DIR = __dirname;
const UI_DIR = path.dirname(TOOLS_DIR);
const REPO_DIR = path.dirname(UI_DIR);
const SERVE_PY = path.join(UI_DIR, 'devserver', 'serve.py');
const MATRIX_FILE = path.join(REPO_DIR, 'docs', 'parity', 'coverage-matrix.json');
const FIXTURES_DIR = path.join(TOOLS_DIR, 'fixtures');
// No fixed default folder: a fixed path under a shared temp folder could be made (or linked elsewhere) first by
// another user. Without --out or UI_CHECK_OUT each run gets a new folder only this user can open (mkdtemp, 0700).
const DEFAULT_OUT_PREFIX = path.join(os.tmpdir(), 'sentry-mate-ui-check-');
const DEFAULT_SIZES = '1440x900,3840x2160';
// the preset tile servers (places::tiles::PRESETS): answered with a blank tile, never reached
const TILE_HOSTS = new Set(['tile.openstreetmap.org', 'tile.opentopomap.org', 'server.arcgisonline.com', 'basemap.nationalmap.gov']);
const MIN_TEXT_PX = 10;
const OTHER_ID_GRACE_MS = 2000; // a data-ready value of another page, unchanged this long, ends the wait
const QUIET_MS = 500; // "network idle": no request in flight for this long
const UNREAD_BODY_MS = 1500; // Chromium never finishes a fetch whose body the page does not read
const LIST_CAP = 100; // entries kept per list and size; the rest are counted
// --pixel: off centre, or rows that miss, by more than PIXEL_TOL CSS px; near misses up to PIXEL_NEAR px
// (more than that is taken as meant); a border edge more than PIXEL_FRAC device px off a whole device pixel
const PIXEL = { tol: 0.5, near: 3, fracTol: 0.05, maxFindings: LIST_CAP };
const PIXEL_LINES = 15; // pixel findings listed per size in the readable report
// --stable: how long to stay on a ready page, what fails, and the dev server's pace of live updates
const STABLE_DEFAULT_S = 30;
const STABLE_MAX_SHIFT = 0.01; // layout shift score (CLS) after ready
const STABLE_LINES = 8; // moved and toggling entries listed per size in the readable report
const STABLE_PACE = {
  // the normal pace (live values every 2 s, as the dev server always does) plus a `rows` event every 10 s, as if a
  // drive or charge had changed, so the lists load again during the watch
  normal: ['--rows-every-s', '10'],
  // --stable-fast: live values every 0.5 s and rows every 3 s
  fast: ['--sse-interval-ms', '500', '--rows-every-s', '3'],
};

// --hover: what moves by more than HOVER.tol CSS px while the mouse is on something else fails; a grid of
// cols x rows points per window, at most perKind elements of one kind (a row, a cell of a column) and
// chartPoints across each chart (two rows of them), at most maxScreens windows down the page
const HOVER = { tol: 0.5, grid: [10, 6], perKind: 4, maxTargets: 600, chartPoints: 10, maxElements: 12000, maxScreens: 6, budgetMs: 240000, maxFindings: LIST_CAP };
const HOVER_LINES = 10; // hover findings listed per size in the readable report

// Page id -> routes (contract 3). A page with two routes (health) is loaded at both; its own items
// may sit on either one, the frame items must be on each.
const ROUTES = {
  fleet: ['/'],
  car: ['/cars/{car}'],
  timeline: ['/cars/{car}/timeline'],
  drives: ['/cars/{car}/drives'],
  drive: ['/cars/{car}/drives/{drive}'],
  trip: ['/cars/{car}/trip'],
  logbook: ['/cars/{car}/logbook'],
  charges: ['/cars/{car}/charges'],
  charge: ['/cars/{car}/charges/{charge}'],
  battery: ['/cars/{car}/battery'],
  efficiency: ['/cars/{car}/efficiency'],
  statistics: ['/cars/{car}/statistics'],
  travel: ['/travel'],
  software: ['/cars/{car}/software'],
  places: ['/places'],
  'settings-cars': ['/settings/cars/{car}', '/settings/cars'],
  'settings-display': ['/settings/display'],
  'settings-maps': ['/settings/maps'],
  'settings-integrations': ['/settings/integrations'],
  'settings-server': ['/settings/server'],
  import: ['/settings/import'],
  health: ['/cars/{car}/health', '/system'],
  help: ['/help'],
};

// Frame screens: not pages of the matrix. Each gets its own dev server in the state it needs, requires
// no items, and expects 401 and 404 from /api (signed out, nothing set up yet).
const FRAME_SCREENS = {
  'sign-in': { route: '/sign-in', serverArgs: ['--signed-out'], expect: [401, 404] },
  setup: { route: '/setup', serverArgs: ['--needs-setup'], expect: [401, 404] },
};

class UsageError extends Error {}
class StartError extends Error {} // the dev server or the browser did not start

// ------------------------------------------------------------------ small helpers

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
// A promise that gives up after ms (an evaluate in a page that stopped running never settles).
function within(ms, promise, what) {
  let t;
  const limit = new Promise((_, reject) => { t = setTimeout(() => reject(new Error(`${what} took more than ${(ms / 1000).toFixed(0)} s`)), ms); });
  return Promise.race([promise, limit]).finally(() => clearTimeout(t));
}
const clip = (s, n) => { s = String(s == null ? '' : s); return s.length > n ? s.slice(0, n - 1) + '…' : s; };
const rel = (p) => { const r = path.relative(process.cwd(), p); return r && !r.startsWith('..') ? r : p; };
const plural = (n, one, many) => `${n} ${n === 1 ? one : (many || one + 's')}`;

function shortUrl(u, origin) {
  if (!u) return '';
  if (origin && u.startsWith(origin)) return u.slice(origin.length) || '/';
  return clip(u, 200);
}

// A list that keeps the first LIST_CAP entries and counts the rest.
function push(res, key, item) {
  if (res[key].length < LIST_CAP) res[key].push(item);
  else res._dropped[key] = (res._dropped[key] || 0) + 1;
}

function splitArgs(s) {
  const out = [];
  let cur = '', quote = null, has = false;
  for (const ch of String(s || '')) {
    if (quote) { if (ch === quote) quote = null; else cur += ch; continue; }
    if (ch === '"' || ch === "'") { quote = ch; has = true; continue; }
    if (/\s/.test(ch)) { if (has) { out.push(cur); cur = ''; has = false; } continue; }
    cur += ch; has = true;
  }
  if (quote) throw new UsageError('--server-args has an unclosed quote');
  if (has) out.push(cur);
  return out;
}

// A blank (transparent) 256 x 256 PNG, answered for every map tile request.
const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();
function crc32(buf) {
  let c = 0xffffffff;
  for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
function blankPng(w, h) {
  const chunk = (type, data) => {
    const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type, 'ascii'), data]);
    const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(td));
    return Buffer.concat([len, td, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8; ihdr[9] = 6; // 8 bits, RGBA
  const raw = Buffer.alloc((w * 4 + 1) * h); // filter byte 0 and transparent pixels on every row
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr), chunk('IDAT', zlib.deflateSync(raw)), chunk('IEND', Buffer.alloc(0)),
  ]);
}
const BLANK_TILE = blankPng(256, 256);

// ------------------------------------------------------------------ processes and clean-up

const live = { children: new Set(), browsers: new Set() };

function killChildren(signal) {
  for (const c of live.children) { try { c.kill(signal); } catch (_) { /* gone */ } }
}
process.on('exit', () => killChildren('SIGKILL'));
for (const sig of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
  process.on(sig, () => {
    process.stderr.write(`\nstopped (${sig}): closing the browser and the dev server\n`);
    for (const b of live.browsers) b.close().catch(() => {});
    killChildren('SIGTERM');
    setTimeout(() => process.exit(sig === 'SIGINT' ? 130 : 143), 200);
  });
}

// Starts serve.py on a free port and resolves when it prints "listening on http://HOST:PORT/".
function startServer(packDir, args) {
  return new Promise((resolve, reject) => {
    const argv = [SERVE_PY, '--pack', packDir, '--port', '0', ...args];
    const child = spawn('python3', argv, { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
    live.children.add(child);
    let out = '', err = '', ready = false;
    const exited = new Promise((r) => child.once('exit', r));
    const stop = async () => {
      if (child.exitCode === null && child.signalCode === null) {
        child.kill('SIGTERM');
        const t = setTimeout(() => child.kill('SIGKILL'), 3000);
        await exited;
        clearTimeout(t);
      }
      live.children.delete(child);
    };
    const fail = (msg) => {
      if (ready) return;
      ready = true;
      stop().then(() => reject(new StartError(`${msg}\n  command: python3 ${argv.map((a) => rel(a)).join(' ')}${err.trim() ? `\n  stderr:\n${err.trim().split('\n').slice(-15).map((l) => '    ' + l).join('\n')}` : ''}`)));
    };
    const timer = setTimeout(() => fail('the dev server printed no "listening on" line within 60 s'), 60000);
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (d) => {
      if (ready) return; // keep reading so the pipe never fills
      out += d;
      const m = /listening on (http:\/\/\S+?)\/?\s/.exec(out);
      if (!m) return;
      ready = true;
      clearTimeout(timer);
      const u = new URL(m[1]);
      if (u.hostname === '0.0.0.0') u.hostname = '127.0.0.1';
      if (u.hostname === '[::]') u.hostname = '[::1]';
      resolve({ origin: u.origin, host: u.host, args, stop, stderr: () => err });
    });
    child.stderr.on('data', (d) => { err = (err + d).slice(-20000); });
    child.once('error', (e) => { clearTimeout(timer); fail(`could not start python3: ${e.message}`); });
    child.once('exit', (code, sig) => { clearTimeout(timer); fail(`the dev server stopped before it was ready (${sig || 'exit code ' + code})`); });
  });
}

// One request to a server's API (the dev server over HTTP, a real server over HTTPS with its own certificate
// accepted for this run only). Resolves { status, json, setCookies }.
function apiRequest(server, method, p, body, cookie) {
  return new Promise((resolve, reject) => {
    const u = new URL(server.origin + p);
    const secure = u.protocol === 'https:';
    const headers = { Accept: 'application/json' };
    if (cookie) headers.Cookie = cookie;
    let data = null;
    if (body !== undefined) {
      data = Buffer.from(JSON.stringify(body));
      headers['Content-Type'] = 'application/json';
      headers['Content-Length'] = data.length;
    }
    if (method !== 'GET' && method !== 'HEAD') headers['X-Sentry-Mate'] = '1'; // the server's CSRF header
    const opts = { method, headers, timeout: 30000 };
    if (secure) opts.rejectUnauthorized = !server.insecure;
    const req = (secure ? https : http).request(u, opts, (res) => {
      let text = '';
      res.setEncoding('utf8');
      res.on('data', (d) => { text += d; });
      res.on('end', () => {
        let json = null;
        try { json = JSON.parse(text); } catch (_) { /* not JSON */ }
        resolve({ status: res.statusCode, json, setCookies: res.headers['set-cookie'] || [] });
      });
    });
    req.on('timeout', () => req.destroy(new Error(`${method} ${p} took more than 30 s`)));
    req.on('error', reject);
    if (data) req.write(data);
    req.end();
  });
}

async function getJson(server, p, jar) {
  const r = await apiRequest(server, 'GET', p, undefined, jar.cookie || server.cookieHeader);
  if (r.setCookies.length && !server.real) jar.cookie = r.setCookies.map((c) => c.split(';')[0]).join('; ');
  if (r.status >= 400 || !r.json) {
    const code = r.json && r.json.error && r.json.error.code ? ` ${r.json.error.code}` : '';
    throw new Error(`GET ${p} answered ${r.status}${code}`);
  }
  return r.json;
}

// A real server (--base): signs in with the given account, makes the pack being checked that account's own
// UI pack (PATCH /me/preferences ui_pack, so two checkers with two accounts can run at once), and returns the
// server object the pages are opened on, with the session cookie for the browser.
async function startReal(o, packId) {
  let u;
  try { u = new URL(o.base); } catch (_) { throw new UsageError(`--base ${o.base} is not a URL`); }
  if (u.protocol !== 'https:') throw new UsageError('--base must be https:// (the server only takes passwords over HTTPS)');
  const server = { origin: u.origin, host: u.host, args: [], real: true, insecure: true, cookies: [], cookieHeader: '', stop: async () => {} };
  let r;
  try {
    r = await apiRequest(server, 'POST', '/api/v1/session', { name: o.user, password: o.password });
  } catch (e) { throw new StartError(`the server at ${u.origin} did not answer: ${e.message}`); }
  if (r.status !== 200) {
    const code = r.json && r.json.error ? r.json.error.code : r.status;
    throw new StartError(`signing in to ${u.origin} as ${o.user} failed (${code})`);
  }
  if (r.json && r.json.status === 'two_step_pending') throw new StartError(`the account ${o.user} needs a two-step code: use an account without two-step sign-in`);
  for (const c of r.setCookies) {
    const [pair] = c.split(';');
    const i = pair.indexOf('=');
    if (i > 0 && !/Max-Age=0/i.test(c)) server.cookies.push({ name: pair.slice(0, i), value: pair.slice(i + 1) });
  }
  if (!server.cookies.length) throw new StartError(`signing in to ${u.origin} set no session cookie`);
  server.cookieHeader = server.cookies.map((c) => `${c.name}=${c.value}`).join('; ');
  const packs = await getJson(server, '/api/v1/ui-packs', {});
  const ids = (packs.items || []).map((p) => p.id);
  if (!ids.includes(packId)) throw new UsageError(`the server has no UI pack "${packId}" (it has ${ids.join(', ') || 'none'})`);
  const set = await apiRequest(server, 'PATCH', '/api/v1/me/preferences', { ui_pack: packId }, server.cookieHeader);
  if (set.status !== 200) throw new StartError(`choosing the UI pack ${packId} for ${o.user} failed (${set.json && set.json.error ? set.json.error.code : set.status})`);
  server.servedSignedOut = packs.chosen || packs.default || null;
  return server;
}

// {car} is the first car of GET /api/v1/cars; {drive} and {charge} that car's newest drive and charge
// (the lists come newest first). --car, --drive and --charge override them.
async function resolveIds(server, o, need) {
  const ids = { car: o.car || null, drive: o.drive || null, charge: o.charge || null };
  const problems = {};
  if (!server) return { ids, problems };
  const jar = {};
  const enc = encodeURIComponent;
  if (need.car && !ids.car) {
    try {
      const cars = await getJson(server, '/api/v1/cars?limit=1', jar);
      ids.car = cars.items && cars.items[0] ? String(cars.items[0].id) : null;
      if (!ids.car) problems.car = 'no car to open: GET /api/v1/cars lists none';
    } catch (e) { problems.car = `no car to open: ${e.message}`; }
  }
  for (const [name, list] of [['drive', 'drives'], ['charge', 'charges']]) {
    if (!need[name] || ids[name]) continue;
    if (!ids.car) { problems[name] = problems.car || 'no car to open'; continue; }
    const p = `/api/v1/cars/${enc(ids.car)}/${list}?limit=1`;
    try {
      const page = await getJson(server, p, jar);
      ids[name] = page.items && page.items[0] ? String(page.items[0].id) : null;
      if (!ids[name]) problems[name] = `no ${name} to open: car ${ids.car} has none`;
    } catch (e) { problems[name] = `no ${name} to open: ${e.message}`; }
  }
  return { ids, problems };
}

function fillRoute(route, ids, problems) {
  let problem = null;
  const url = route.replace(/\{([a-z]+)\}/g, (m, name) => {
    if (!(name in ids)) { problem = problem || `the route ${route} has an unknown part {${name}}`; return m; }
    if (!ids[name]) { problem = problem || problems[name] || `no ${name} to open`; return m; }
    return encodeURIComponent(ids[name]);
  });
  return { url, problem };
}

// ------------------------------------------------------------------ the matrix and the page plan

function loadMatrix(file) {
  let m;
  try { m = JSON.parse(fs.readFileSync(file, 'utf8')); } catch (e) { throw new UsageError(`cannot read the matrix ${file}: ${e.message}`); }
  if (!m || typeof m.pages !== 'object' || !Array.isArray(m.items)) throw new UsageError(`${file} is not a coverage matrix (it needs "pages" and "items")`);
  const itemPage = new Map();
  for (const it of m.items) if (it && typeof it.id === 'string') itemPage.set(it.id, String(it.page));
  const byPage = new Map();
  for (const [id, page] of itemPage) { if (!byPage.has(page)) byPage.set(page, []); byPage.get(page).push(id); }
  return { file, pages: m.pages, itemPage, byPage };
}

// Routes of a page the built-in table does not know, from the matrix text ("/a  and  /b", "/x?from=&to=").
function routesFromMatrix(text) {
  const head = String(text || '').split('(')[0];
  return (head.match(/\/[^\s]*/g) || []).map((r) => r.replace(/\?.*$/, '') || '/');
}

function slug(route) {
  return route.replace(/\{[^}]*\}/g, '').replace(/[^a-z0-9]+/gi, '-').replace(/^-+|-+$/g, '') || 'root';
}

function planPages(matrix, pagesOpt, real) {
  const known = Object.keys(matrix.pages).filter((id) => id !== 'frame');
  // a real server is set up already: its setup screen cannot be shown
  const screens = Object.keys(FRAME_SCREENS).filter((id) => !(real && id === 'setup'));
  const all = [...known, ...screens];
  const ids = pagesOpt === 'all' ? all : [...new Set(String(pagesOpt).split(',').map((s) => s.trim()).filter(Boolean))];
  if (!ids.length) throw new UsageError('--pages names no page');
  const entries = [];
  for (const id of ids) {
    if (id === 'frame') throw new UsageError('"frame" is checked on every page; name the pages to check');
    if (real && id === 'setup') throw new UsageError('the setup screen needs a server that is not set up: check it on the dev server (no --base)');
    if (FRAME_SCREENS[id]) {
      const f = FRAME_SCREENS[id];
      entries.push({ id, key: id, route: f.route, mode: id, serverArgs: f.serverArgs, expect: f.expect, screen: true });
      continue;
    }
    if (!matrix.pages[id]) throw new UsageError(`unknown page "${id}". Pages: ${all.join(', ')}`);
    const routes = ROUTES[id] || routesFromMatrix(matrix.pages[id].route);
    if (!routes.length) throw new UsageError(`no route for page "${id}"`);
    routes.forEach((route, i) => entries.push({
      id, key: i === 0 ? id : `${id}-${slug(route)}`, route, mode: 'normal', serverArgs: [], expect: [], screen: false,
    }));
  }
  return entries;
}

// ------------------------------------------------------------------ in the page

// Runs before any page script, in every frame: Content Security Policy violations of the document.
function initScript() {
  document.addEventListener('securitypolicyviolation', (e) => {
    try {
      window.__smCheckCsp({
        directive: e.effectiveDirective || e.violatedDirective, blocked: e.blockedURI,
        source: e.sourceFile, line: e.lineNumber, sample: e.sample,
      });
    } catch (_) { /* the binding is gone */ }
  }, true);
}

// Runs in each worker once it exists (a violation in a worker's first line can be missed). A worker
// cannot call the page binding, but its console reaches the page, so it reports through that.
function workerHook() {
  self.addEventListener('securitypolicyviolation', (e) => {
    console.error('[sm-check-csp] ' + JSON.stringify({
      directive: e.effectiveDirective || e.violatedDirective, blocked: e.blockedURI,
      source: e.sourceFile, line: e.lineNumber, sample: e.sample, worker: true,
    }));
  });
}

// Coverage tokens, horizontal overflow and small text, measured in the page after it is ready.
function pageChecks({ minPx, maxElements }) {
  const de = document.documentElement;
  const body = document.body;

  function sel(e) {
    const parts = [];
    for (let x = e; x && x.nodeType === 1 && parts.length < 4; x = x.parentElement) {
      let s = x.localName;
      if (x.id) { parts.unshift(s + '#' + CSS.escape(x.id)); break; }
      const cls = (x.getAttribute('class') || '').trim().split(/\s+/).filter(Boolean).slice(0, 2);
      if (cls.length) s += '.' + cls.map((c) => CSS.escape(c)).join('.');
      const p = x.parentElement;
      if (p && x !== body) {
        const same = Array.prototype.filter.call(p.children, (c) => c.localName === x.localName);
        if (same.length > 1) s += ':nth-of-type(' + (same.indexOf(x) + 1) + ')';
      }
      parts.unshift(s);
      if (x === body) break;
    }
    return parts.join(' > ');
  }

  // every data-cover token, open shadow roots included
  const tokens = new Set();
  const roots = [document];
  while (roots.length) {
    const r = roots.pop();
    for (const e of r.querySelectorAll('*')) {
      const v = e.getAttribute('data-cover');
      if (v) for (const t of v.split(/\s+/)) if (t) tokens.add(t);
      if (e.shadowRoot) roots.push(e.shadowRoot);
    }
  }

  const opacityCache = new Map();
  function transparent(e) { // opacity 0 on the element or an ancestor
    if (!e || e === de) return false;
    if (opacityCache.has(e)) return opacityCache.get(e);
    const v = getComputedStyle(e).opacity === '0' || transparent(e.parentElement);
    opacityCache.set(e, v);
    return v;
  }

  // the document is wider than the window
  const vw = de.clientWidth;
  const overflow = { document: de.scrollWidth > vw + 1, scrollWidth: de.scrollWidth, clientWidth: vw, elements: [], count: 0 };

  // elements cut by the left or right edge of the window, after the clipping of ancestors that hide or
  // scroll their overflow (body and html excepted: content they cut off is lost); outermost ones only
  const cut = new Set();
  const crosses = (l, r) => (r > vw + 1 && l < vw - 1) || (l < -1 && r > 1);
  if (body) {
    for (const e of body.querySelectorAll('*')) {
      const r = e.getBoundingClientRect();
      if (r.width < 1 || r.height < 1 || !crosses(r.left, r.right)) continue;
      const cs = getComputedStyle(e);
      if (cs.visibility !== 'visible' || transparent(e)) continue;
      let l = r.left, rt = r.right;
      if (cs.position !== 'fixed') {
        for (let a = e.parentElement; a && a !== body && a !== de; a = a.parentElement) {
          const as = getComputedStyle(a);
          if (as.overflowX !== 'visible') { const ar = a.getBoundingClientRect(); l = Math.max(l, ar.left); rt = Math.min(rt, ar.right); }
          if (as.position === 'fixed') break;
        }
      }
      if (rt - l < 1 || !crosses(l, rt)) continue;
      let inner = false;
      for (let a = e.parentElement; a; a = a.parentElement) if (cut.has(a)) { inner = true; break; }
      if (inner) continue;
      cut.add(e);
      overflow.count++;
      if (overflow.elements.length < maxElements) {
        overflow.elements.push({ selector: sel(e), left: Math.round(l), right: Math.round(rt), width: Math.round(r.width) });
      }
    }
  }

  // visible text smaller than minPx: the computed font size, times the SVG scale (viewBox, transforms)
  // or a CSS transform that shrinks it
  const sizeCache = new Map();
  function rendered(e) {
    if (sizeCache.has(e)) return sizeCache.get(e);
    const cs = getComputedStyle(e);
    let px = parseFloat(cs.fontSize) || 0;
    if (e instanceof SVGElement) {
      const g = typeof e.getScreenCTM === 'function' ? e : e.closest('svg');
      const m = g && g.getScreenCTM ? g.getScreenCTM() : null;
      if (m) px *= Math.hypot(m.c, m.d);
    } else if (e.offsetHeight > 0) {
      const s = e.getBoundingClientRect().height / e.offsetHeight;
      if (s > 0 && s < 0.98) px *= s;
    }
    const v = { px: Math.round(px * 10) / 10, visible: cs.visibility === 'visible' && !transparent(e) };
    sizeCache.set(e, v);
    return v;
  }
  const small = [];
  let smallCount = 0;
  if (body) {
    const walker = document.createTreeWalker(body, NodeFilter.SHOW_TEXT);
    const range = document.createRange();
    const docW = Math.max(de.scrollWidth, vw);
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      if (!/\S/.test(n.nodeValue)) continue;
      const e = n.parentElement;
      if (!e || e.closest('script, style, noscript, template, title, desc')) continue;
      const s = rendered(e);
      if (!s.visible || s.px >= minPx) continue;
      range.selectNodeContents(n);
      const rects = Array.from(range.getClientRects()).filter((q) => q.width > 1 && q.height > 1);
      const sx = window.scrollX, sy = window.scrollY;
      if (!rects.some((q) => q.right + sx > 0 && q.left + sx < docW && q.bottom + sy > 0)) continue;
      smallCount++;
      small.push({ px: s.px, selector: sel(e), text: n.nodeValue.trim().replace(/\s+/g, ' ').slice(0, 60) });
    }
  }
  small.sort((a, b) => a.px - b.px);
  const smallest = [];
  const seen = new Set();
  for (const s of small) {
    if (seen.has(s.selector)) continue;
    seen.add(s.selector);
    smallest.push(s);
    if (smallest.length >= 5) break;
  }

  return { tokens: [...tokens], overflow, smallText: { count: smallCount, smallest } };
}

// ------------------------------------------------------------------ one page at one size

// Runs in the page after ready (and after the --stable watch): scrolls the page down about a screen at a time to its
// end and back, so blocks that a page draws only when they come near the screen (charts, maps and lists below the
// fold, ui/shared/lazy.js) are drawn before the page is measured and photographed. The page scrolls in the window,
// else in its tallest box that scrolls up and down. It also notes the layout top of every block before and after:
// a block that moved meanwhile (a part drawn on the way that had no room kept for it) is listed.
async function scrollThrough({ maxBlocks }) {
  const frames = (n) => new Promise((r) => { const f = () => (n-- > 0 ? requestAnimationFrame(f) : r()); f(); });
  const pause = (ms) => new Promise((r) => setTimeout(r, ms));
  const SCROLLS = new Set(['auto', 'scroll', 'overlay']);
  const de = document.scrollingElement || document.documentElement;
  let box = de.scrollHeight > de.clientHeight + 1 ? de : null;
  if (!box) {
    let best = 0;
    for (const e of document.querySelectorAll('body *')) {
      if (e.scrollHeight > e.clientHeight + 1 && e.clientHeight > best && SCROLLS.has(getComputedStyle(e).overflowY)) { best = e.clientHeight; box = e; }
    }
  }
  if (!box) return { steps: 0, moved: [] };
  const sel = (e) => {
    const parts = [];
    for (let x = e; x && x !== document.body && parts.length < 4; x = x.parentElement) {
      let k = x.localName;
      if (x.id) k += '#' + x.id;
      else if (typeof x.className === 'string' && x.className.trim()) k += '.' + x.className.trim().split(/\s+/)[0];
      parts.unshift(k);
    }
    return parts.join(' > ');
  };
  const layoutTop = (e) => { let y = 0; for (let x = e; x; x = x.offsetParent) y += x.offsetTop; return y; };
  const tops = () => {
    const m = new Map();
    for (const e of document.querySelectorAll('body *')) {
      if (m.size >= maxBlocks) break;
      if (!(e instanceof HTMLElement) || e.offsetWidth < 40 || e.offsetHeight < 8) continue;
      if (e.closest('svg, canvas, .maplibregl-map, [data-stable-ignore], [data-offstage], [inert]')) continue;
      const cs = getComputedStyle(e);
      if (cs.position === 'fixed' || cs.position === 'sticky') continue;
      m.set(e, layoutTop(e));
    }
    return m;
  };
  const before = tops();
  const start = box.scrollTop;
  let steps = 0;
  for (; steps < 300; steps++) {
    const at = box.scrollTop;
    box.scrollTo({ top: at + Math.max(100, Math.round(box.clientHeight * 0.8)), behavior: 'instant' });
    await frames(2);
    await pause(40);
    if (box.scrollTop <= at + 1) break;
  }
  await frames(2);
  await pause(250);
  // back where it was, so blocks in sticky bars are measured where they were
  box.scrollTo({ top: start, behavior: 'instant' });
  await frames(2);
  const moved = [];
  const after = tops();
  for (const [e, y] of before) {
    if (!e.isConnected || !after.has(e)) continue;
    const dy = after.get(e) - y;
    if (Math.abs(dy) <= 1) continue;
    // only the outermost of blocks that moved together
    let p = e.parentElement, inner = false;
    for (; p && p !== document.body; p = p.parentElement) if (before.has(p) && after.has(p) && Math.abs(after.get(p) - before.get(p) - dy) <= 1) { inner = true; break; }
    if (!inner) moved.push({ selector: sel(e), px: Math.round(dy) });
  }
  return { steps, moved: moved.slice(0, 100), movedCount: moved.length };
}

async function checkOne(browser, server, entry, size, sizeIndex, o, outDir) {
  const t0 = Date.now();
  const res = {
    size: size.label, ready: { ok: false, value: null, ms: null, landed: null, reason: null },
    missing: [], foreign: [], unknown: [], console: [], pageErrors: [], csp: [], apiFailures: [], outside: [],
    overflow: null, smallText: null, pixel: null, stable: null, scrolled: null, hover: null, shot: null, fullShot: null, tiles: 0, notes: [], loadError: null,
    _tokens: new Set(), _dropped: {},
  };
  if (entry.problem) { res.loadError = entry.problem; res.ready.reason = 'not_loaded'; return finish(); }

  const origin = server.origin;
  const isApi = (u) => u.host === server.host && u.pathname.startsWith('/api/');
  const cspEvents = [];
  const cspConsole = [];
  const pending = [];
  const inflight = new Set();
  const watch = { on: false, api: 0 }; // --stable: /api requests while the page is watched
  let lastBusy = Date.now();
  let context;

  function finish() {
    res.ms = Date.now() - t0;
    return res;
  }

  try {
    context = await browser.newContext({
      viewport: { width: size.w, height: size.h }, screen: { width: size.w, height: size.h }, deviceScaleFactor: size.scale,
      // a real server's self-signed certificate, for this run (every other host is blocked or answered below)
      ...(server.real ? { ignoreHTTPSErrors: true } : {}),
    });
    if (server.real && server.cookies.length) {
      await context.addCookies(server.cookies.map((c) => ({ name: c.name, value: c.value, url: origin + '/', secure: true, httpOnly: true, sameSite: 'Strict' })));
    }
    // every host but the dev server: map tiles get a blank tile, anything else is recorded and blocked
    await context.route((u) => /^(https?|wss?):$/.test(u.protocol) && u.host !== server.host, (route) => {
      const req = route.request();
      let u = null;
      try { u = new URL(req.url()); } catch (_) { /* odd URL */ }
      if (u && u.protocol === 'https:' && TILE_HOSTS.has(u.host)) {
        res.tiles++;
        return route.fulfill({ status: 200, contentType: 'image/png', headers: { 'access-control-allow-origin': '*' }, body: BLANK_TILE }).catch(() => {});
      }
      push(res, 'outside', { url: clip(req.url(), 300), method: req.method(), type: req.resourceType() });
      return route.abort('blockedbyclient').catch(() => {});
    });
    const page = await context.newPage();
    // a popup (window.open, target=_blank) is closed again; its requests are checked by the route above.
    // While it is open the page is in the background, where animation frames stop: bring it back.
    let popups = 0;
    context.on('page', (p) => {
      if (p === page) return;
      popups++;
      setTimeout(() => p.close().catch(() => {}).then(() => page.bringToFront().catch(() => {})), 200);
    });
    await page.exposeFunction('__smCheckCsp', (v) => { if (v) cspEvents.push(v); });
    await page.addInitScript(initScript);
    // the Interface size on this device (ui/shared/devicesize.js), in this browser window's storage before boot.js runs
    if (o.deviceSize) await page.addInitScript((n) => { try { localStorage.setItem('sentrymate.device-size', JSON.stringify(n)); } catch (_) { /* opaque origin */ } }, o.deviceSize);
    if (o.stable) await page.addInitScript(stableInit, { pageId: entry.id });
    page.on('worker', (w) => { w.evaluate(workerHook).catch(() => {}); });

    page.on('console', (msg) => {
      const type = msg.type();
      if (type !== 'error' && type !== 'assert') return;
      const text = msg.text();
      if (text.startsWith('[sm-check-csp] ')) {
        try { cspEvents.push(JSON.parse(text.slice(15))); } catch (_) { /* not ours */ }
        return;
      }
      if (/Content[- ]Security[- ]Policy/i.test(text)) { cspConsole.push(text); return; }
      const loc = msg.location() || {};
      if (/^Failed to load resource/i.test(text) && loc.url) {
        let u = null;
        try { u = new URL(loc.url); } catch (_) { /* odd URL */ }
        // failed /api answers and other hosts have their own lists
        if (u && (u.host !== server.host || u.pathname.startsWith('/api/'))) return;
      }
      push(res, 'console', { text: clip(text, 500), url: shortUrl(loc.url, origin), line: loc.url ? (loc.lineNumber || 0) + 1 : null });
    });
    page.on('pageerror', (err) => {
      const stack = String(err.stack || '').split('\n').slice(1, 4).map((l) => l.trim().replace(/^at /, '')).join(' | ');
      push(res, 'pageErrors', { message: clip(err.message || String(err), 500), stack: clip(stack.replaceAll(origin, ''), 400) });
    });

    // requests in flight, for "network idle"; event streams and sockets stay open and do not count
    const counts = (r) => { const t = r.resourceType(); return t !== 'eventsource' && t !== 'websocket'; };
    const settle = (r) => { if (inflight.delete(r)) lastBusy = Date.now(); };
    page.on('request', (r) => {
      if (counts(r)) { inflight.add(r); lastBusy = Date.now(); }
      if (watch.on && counts(r)) { try { if (isApi(new URL(r.url()))) watch.api++; } catch (_) { /* odd URL */ } }
    });
    page.on('requestfinished', settle);
    page.on('response', (resp) => {
      const req = resp.request();
      const headers = resp.headers();
      if (String(headers['content-type'] || '').includes('text/event-stream')) settle(req);
      else if (inflight.has(req)) setTimeout(() => settle(req), UNREAD_BODY_MS);
      let u = null;
      try { u = new URL(resp.url()); } catch (_) { return; }
      const status = resp.status();
      if (isApi(u) && status >= 400 && !entry.expect.includes(status)) {
        const item = { method: req.method(), url: clip(u.pathname + u.search, 300), status, code: null };
        push(res, 'apiFailures', item);
        // the error code from the body; a GET whose body the page never read is asked again
        const codeOf = (t) => { try { const j = JSON.parse(t); return j && j.error && j.error.code ? String(j.error.code) : null; } catch (_) { return null; } };
        pending.push(within(UNREAD_BODY_MS + 500, resp.text(), 'reading the body').then((t) => { item.code = codeOf(t); }, () => {
          if (req.method() !== 'GET') return null;
          return context.request.get(resp.url(), { headers: { Accept: 'application/json' }, timeout: 10000 })
            .then((again) => again.text()).then((t) => { item.code = codeOf(t); }, () => {});
        }));
      } else if (u.host === server.host && !isApi(u) && status < 400 && ['script', 'stylesheet', 'font', 'image'].includes(req.resourceType())
        && String(headers['content-type'] || '').startsWith('text/html')) {
        res.notes.push(`${req.resourceType()} ${clip(u.pathname, 120)} came back as HTML (the page's index.html): is the file missing or the path relative?`);
      }
    });
    page.on('requestfailed', (r) => {
      settle(r);
      const err = r.failure() ? r.failure().errorText : 'failed';
      let u = null;
      try { u = new URL(r.url()); } catch (_) { return; }
      if (!isApi(u) || /ERR_ABORTED/.test(err)) return; // cancelled on purpose: a newer request, a page change
      push(res, 'apiFailures', { method: r.method(), url: clip(u.pathname + u.search, 300), status: null, code: null, error: err });
    });

    // wait until no request has been in flight for QUIET_MS (at most --idle-timeout-ms)
    const idle = async (what = '') => {
      const idleStart = Date.now();
      while (!(inflight.size === 0 && Date.now() - lastBusy >= QUIET_MS)) {
        if (Date.now() - idleStart >= o.idleTimeoutMs) {
          const busy = [...inflight].slice(0, 3).map((r) => shortUrl(r.url(), origin)).join(', ');
          res.notes.push(`the network was still busy${what} after ${(o.idleTimeoutMs / 1000).toFixed(0)} s (${plural(inflight.size, 'request')} open: ${busy})`);
          break;
        }
        await sleep(50);
      }
    };

    // navigate and wait for data-ready
    const navStart = Date.now();
    try {
      await page.goto(origin + entry.url, { waitUntil: 'commit', timeout: 30000 });
    } catch (e) {
      res.loadError = `the page did not load: ${clip(e.message.split('\n')[0], 300)}`;
    }
    if (!res.loadError) {
      let other = null, otherSince = 0, last = null;
      for (;;) {
        let v;
        try { v = await within(5000, page.evaluate(() => document.documentElement.dataset.ready ?? null), 'reading data-ready'); } catch (_) { v = undefined; }
        if (v !== undefined) last = v;
        const ms = Date.now() - navStart;
        if (v === entry.id) { Object.assign(res.ready, { ok: true, value: v, ms }); break; }
        if (typeof v === 'string' && v.startsWith('error:')) { Object.assign(res.ready, { value: v, ms, reason: 'error' }); break; }
        if (typeof v === 'string' && v) {
          if (v !== other) { other = v; otherSince = Date.now(); } else if (Date.now() - otherSince >= OTHER_ID_GRACE_MS) {
            Object.assign(res.ready, { value: v, ms, reason: 'other_page' }); break;
          }
        } else other = null;
        if (ms >= o.readyTimeoutMs) { Object.assign(res.ready, { value: last, ms, reason: 'timeout' }); break; }
        await sleep(100);
      }
      // then wait until no request has been in flight for QUIET_MS
      await idle();
      if (popups) await within(5000, page.bringToFront(), 'bringing the page to the front').catch(() => {});
      await within(2000, page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(() => r())))), 'two animation frames').catch(() => {});
    }
    try { res.ready.landed = shortUrl(page.url(), origin); } catch (_) { /* closed */ }

    // --stable: stay on the ready page while live updates arrive, then read what moved and what came and went
    if (o.stable && !res.loadError && res.ready.ok) {
      const base = path.join(outDir, `${entry.key}-${size.label}-stable`);
      const shot = async (file, full) => {
        try {
          await page.screenshot({ path: file, fullPage: !!full, animations: 'allow', caret: 'initial', timeout: 30000 });
          return file;
        } catch (e) { res.notes.push(`no stability screenshot: ${clip(e.message.split('\n')[0], 200)}`); return null; }
      };
      const before = await shot(base + '-before.png');
      watch.on = true;
      await sleep(o.stableSeconds * 1000);
      watch.on = false;
      try {
        const st = await within(30000, page.evaluate(stableCollect, { maxList: LIST_CAP }), 'reading the stability watch');
        const boxes = st.boxes.map((b, i) => ({ n: i + 1, ...b }));
        delete st.boxes;
        res.stable = { ...st, apiRequests: watch.api, before, after: null, outlined: null };
        res.stable.after = await shot(base + '-after.png');
        if (boxes.length) {
          await within(10000, page.evaluate(stableOutline, boxes), 'outlining').catch(() => {});
          res.stable.outlined = await shot(base + '-outlined.png', true);
          await within(10000, page.evaluate(stableOutlineRemove), 'removing the outlines').catch(() => {});
        }
        if (st.cost && (st.cost.snapMaxMs > 30 || st.cost.sampleMaxMs > 60)) {
          res.notes.push(`a big page: the watch looked at it every ${Math.round(3 * st.cost.snapMaxMs)} ms at most for flicker and every ${Math.round(Math.max(250, 4 * st.cost.sampleMaxMs))} ms for moves, so short flashes can be missed`);
        }
        if (st.noShiftApi) res.notes.push('this browser has no layout-shift entries: only moved blocks were measured');
        if (!st.eventCount && !watch.api) res.notes.push(`nothing was updated during the ${o.stableSeconds} s watch (no event and no /api request)`);
      } catch (e) {
        res.stable = { error: clip(e.message.split('\n')[0], 200) };
      }
    }

    // scroll through the page and back, so the parts drawn only when they come near the screen are there for the
    // checks and screenshots below; then wait for what they loaded
    if (!res.loadError && res.ready.ok) {
      try {
        res.scrolled = await within(120000, page.evaluate(scrollThrough, { maxBlocks: 20000 }), 'scrolling through the page');
        if (res.scrolled.steps) {
          await idle(' after scrolling through the page');
          await within(2000, page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(() => r())))), 'two animation frames').catch(() => {});
        }
        // with --stable, a block that moved while the page was scrolled through is a moved block too
        if (res.stable && !res.stable.error && res.scrolled.movedCount) {
          res.stable.scrollMoved = res.scrolled.moved;
          res.stable.scrollMovedCount = res.scrolled.movedCount;
        }
      } catch (e) {
        res.notes.push(`could not scroll through the page: ${clip(e.message.split('\n')[0], 200)}`);
      }
    }

    // --hover: the mouse over a grid of points, every interactive element and every chart, then focus on each
    // element that takes it: nothing outside what is hovered may move
    if (o.hover && !res.loadError && res.ready.ok) {
      try {
        res.hover = await within(HOVER.budgetMs + 60000, hoverRun(page, HOVER), 'hovering');
        if (res.hover.truncated) res.notes.push(`hover: stopped after ${Math.round(HOVER.budgetMs / 1000)} s; not everything was hovered`);
        if (res.hover.findings.length) {
          const base = path.join(outDir, `${entry.key}-${size.label}-hover`);
          const boxes = [];
          for (const f of res.hover.findings) for (const m of f.moved.slice(0, 3)) boxes.push({ n: f.n, kind: 'hover', ...m.box });
          await within(10000, page.evaluate(stableOutline, boxes), 'outlining').catch(() => {});
          try {
            await page.screenshot({ path: base + '.png', fullPage: true, animations: 'disabled', caret: 'hide', timeout: 30000 });
            res.hover.shot = base + '.png';
          } catch (e) { res.notes.push(`no hover screenshot: ${clip(e.message.split('\n')[0], 200)}`); }
          await within(10000, page.evaluate(stableOutlineRemove), 'removing the outlines').catch(() => {});
        }
      } catch (e) {
        res.hover = { error: clip(e.message.split('\n')[0], 200) };
      }
    }

    // measure the page
    try {
      const dom = await within(30000, page.evaluate(pageChecks, { minPx: MIN_TEXT_PX, maxElements: 10 }), 'measuring the page');
      res._tokens = new Set(dom.tokens);
      res.overflow = dom.overflow;
      res.smallText = dom.smallText;
    } catch (e) {
      res.notes.push(`could not measure the page: ${clip(e.message.split('\n')[0], 200)}`);
    }
    if (o.pixel) {
      try {
        res.pixel = await within(60000, page.evaluate(pixelChecks, PIXEL), 'measuring pixels');
        res.pixel.shot = null;
      } catch (e) {
        res.notes.push(`could not measure pixels: ${clip(e.message.split('\n')[0], 200)}`);
      }
    }

    // screenshots
    const base = path.join(outDir, `${entry.key}-${size.label}`);
    try {
      await page.screenshot({ path: base + '.png', animations: 'disabled', caret: 'hide', timeout: 20000 });
      res.shot = base + '.png';
    } catch (e) { res.notes.push(`no screenshot: ${clip(e.message.split('\n')[0], 200)}`); }
    if (res.pixel && res.pixel.findings.length) {
      // the whole page with every listed finding outlined and numbered as in the list
      try {
        await within(10000, page.evaluate(pixelOutline, res.pixel.findings.map((f) => ({ n: f.n, kind: f.kind, ...f.box }))), 'outlining');
        await page.screenshot({ path: base + '-pixel.png', fullPage: true, animations: 'disabled', caret: 'hide', timeout: 30000 });
        res.pixel.shot = base + '-pixel.png';
        await within(10000, page.evaluate(pixelOutlineRemove), 'removing the outlines').catch(() => {});
      } catch (e) { res.notes.push(`no outlined screenshot: ${clip(e.message.split('\n')[0], 200)}`); }
    }
    if (o.full && sizeIndex === 0) {
      try {
        await page.screenshot({ path: base + '-full.png', fullPage: true, animations: 'disabled', caret: 'hide', timeout: 30000 });
        res.fullShot = base + '-full.png';
      } catch (e) { res.notes.push(`no full-page screenshot: ${clip(e.message.split('\n')[0], 200)}`); }
    }
    await within(10000, Promise.allSettled(pending), 'reading error answers').catch(() => {});
  } catch (e) {
    res.loadError = res.loadError || `checker error: ${clip(e.message.split('\n')[0], 300)}`;
  } finally {
    if (context) await within(15000, context.close(), 'closing the page').catch(() => {});
  }

  // one list of CSP violations: the events, plus console reports that match no event (a worker's first
  // line, a policy the event listener did not see)
  const used = new Set();
  for (const ev of cspEvents) push(res, 'csp', { directive: ev.directive || null, blocked: clip(ev.blocked, 300) || null, source: shortUrl(ev.source, origin) || null, line: ev.line || null, sample: ev.sample ? clip(ev.sample, 120) : null, ...(ev.worker ? { worker: true } : {}) });
  for (const text of cspConsole) {
    const i = cspEvents.findIndex((ev, k) => !used.has(k) && ev.directive && text.includes(ev.directive.replace(/-(attr|elem)$/, '')));
    if (i >= 0) used.add(i);
    else push(res, 'csp', { directive: null, blocked: null, source: null, line: null, sample: null, message: clip(text, 400) });
  }
  return finish();
}

// ------------------------------------------------------------------ verdicts

function failures(r, o) {
  const f = [];
  if (r.loadError) f.push('load');
  if (!r.ready.ok) f.push('ready');
  if (r.missing.length && !o.allowMissing) f.push('missing');
  for (const k of ['foreign', 'unknown', 'console', 'pageErrors', 'csp', 'apiFailures', 'outside']) if (r[k].length) f.push(k);
  if (r.overflow && (r.overflow.document || r.overflow.count)) f.push('overflow');
  if (r.smallText && r.smallText.count) f.push('smallText');
  if (o.pixel && (!r.pixel || r.pixel.count)) f.push('pixel');
  if (o.stable && !r.loadError && r.ready.ok && stableFails(r.stable)) f.push('stable');
  if (o.hover && !r.loadError && r.ready.ok && (!r.hover || r.hover.error || r.hover.count)) f.push('hover');
  return f;
}

// A watched page fails when its layout shifted after ready, a block moved, or something came and went more than once.
function stableFails(st) {
  return !st || !!st.error || st.shift > STABLE_MAX_SHIFT || st.movedCount > 0 || st.togglingCount > 0 || st.scrollMovedCount > 0;
}

// Missing, foreign and unknown ids. A page's own items count when they are on any of its routes at that
// size; frame items must be on every route. Frame screens (sign-in, setup) need no items.
function coverage(entries, results, matrix, o) {
  const frameItems = matrix.byPage.get('frame') || [];
  for (let si = 0; si < o.sizes.length; si++) {
    const union = new Map();
    entries.forEach((e, ei) => {
      if (!union.has(e.id)) union.set(e.id, new Set());
      for (const t of results[ei][si]._tokens) union.get(e.id).add(t);
    });
    entries.forEach((e, ei) => {
      const r = results[ei][si];
      const tokens = r._tokens;
      if (!e.screen) {
        const own = (matrix.byPage.get(e.id) || []).filter((id) => !union.get(e.id).has(id));
        const frame = frameItems.filter((id) => !tokens.has(id));
        r.missing = [...new Set([...frame, ...own])].sort();
      }
      for (const t of [...tokens].sort()) {
        const page = matrix.itemPage.get(t);
        if (page === undefined) r.unknown.push(t);
        else if (page !== 'frame' && page !== e.id) r.foreign.push({ id: t, page });
      }
    });
  }
}

function clean(r, o) {
  const out = { ...r };
  out.covered = r._tokens.size;
  if (Object.keys(r._dropped).length) out.dropped = r._dropped;
  delete out._tokens;
  delete out._dropped;
  out.fails = failures(r, o);
  out.pass = out.fails.length === 0;
  return out;
}

// ------------------------------------------------------------------ a run

async function run(o) {
  const log = o.log || (() => {});
  const matrix = loadMatrix(o.matrixFile);
  const packName = path.basename(o.packDir);
  const entries = planPages(matrix, o.pages, !!o.base);
  const outDir = path.join(o.out, packName);
  fs.mkdirSync(outDir, { recursive: true });

  const servers = {};
  let browser = null;
  try {
    const modes = [...new Map(entries.map((e) => [e.mode, e.serverArgs])).entries()];
    if (o.base) {
      // one real server for every page: the pages signed in, the sign-in screen without the session
      const real = await startReal(o, packName);
      for (const [mode] of modes) servers[mode] = mode === 'normal' ? real : { ...real, cookies: [], cookieHeader: '', stop: async () => {} };
      for (const e of entries) {
        if (e.id === 'sign-in' && real.servedSignedOut !== packName) {
          e.problem = `the server shows pack ${real.servedSignedOut} to signed-out browsers, not ${packName}: the sign-in screen of ${packName} cannot be opened there`;
        }
      }
    } else {
      await Promise.all(modes.map(async ([mode, args]) => { servers[mode] = await startServer(o.packDir, [...o.serverArgs, ...args]); }));
    }
    const need = {};
    for (const e of entries) if (e.mode === 'normal') for (const m of e.route.matchAll(/\{([a-z]+)\}/g)) need[m[1]] = true;
    const { ids, problems } = await resolveIds(servers.normal, o, need);
    for (const e of entries) if (!e.problem) Object.assign(e, fillRoute(e.route, ids, problems));
    // --place: the places page opens that place's editor
    if (o.place) for (const e of entries) if (e.id === 'places' && !e.problem) e.url += `?place=${encodeURIComponent(o.place)}`;
    log(`checking ${plural(entries.length, 'route')} of pack ${packName} at ${o.sizes.map((s) => s.label).join(', ')} on ${servers.normal ? servers.normal.origin : Object.values(servers)[0].origin}`);

    let chromium;
    try { ({ chromium } = require('playwright')); } catch (e) { throw new StartError(`Playwright is not installed here: ${e.message.split('\n')[0]}`); }
    // --hover: with the scrollbars the window really has (Playwright hides them by default, and a scrollbar that
    // comes and goes on hover moves the whole page). A real server's self-signed certificate: the windows'
    // ignoreHTTPSErrors does not reach the service worker's script fetch (it would fail with a console error), so
    // this run's browser ignores certificate errors as a whole; every other host is still blocked or answered inside
    // the browser
    const real = Object.values(servers).some((x) => x && x.real);
    const launch = { ...(o.hover ? { ignoreDefaultArgs: ['--hide-scrollbars'] } : {}), ...(real ? { args: ['--ignore-certificate-errors'] } : {}) };
    try { browser = await chromium.launch(launch); } catch (e) { throw new StartError(`Chromium did not start: ${e.message.split('\n')[0]}`); }
    live.browsers.add(browser);

    const tasks = [];
    entries.forEach((e, ei) => o.sizes.forEach((s, si) => tasks.push({ e, ei, s, si })));
    const results = entries.map(() => new Array(o.sizes.length));
    let next = 0;
    const worker = async () => {
      while (next < tasks.length) {
        const t = tasks[next++];
        const r = await checkOne(browser, servers[t.e.mode], t.e, t.s, t.si, o, outDir);
        results[t.ei][t.si] = r;
        log(`  ${t.e.key} at ${t.s.label}: ${r.loadError ? 'not loaded' : r.ready.ok ? 'ready' : 'not ready'} (${(r.ms / 1000).toFixed(1)} s)`);
      }
    };
    await Promise.all(Array.from({ length: Math.max(1, Math.min(o.jobs, tasks.length)) }, worker));

    coverage(entries, results, matrix, o);
    const pages = entries.map((e, ei) => {
      const sizes = results[ei].map((r) => clean(r, o));
      return { id: e.id, url: e.url, route: e.route, sizes, pass: sizes.every((s) => s.pass) };
    });
    return {
      pack: packName,
      packDir: rel(o.packDir),
      matrix: rel(matrix.file),
      server: { origin: servers.normal ? servers.normal.origin : o.base || null, args: o.serverArgs, real: !!o.base, user: o.base ? o.user : null },
      opened: ids,
      sizes: o.sizes.map((s) => s.label),
      out: outDir,
      allowMissing: !!o.allowMissing,
      pixel: !!o.pixel,
      stable: o.stable ? { seconds: o.stableSeconds, pace: o.stableFast ? 'fast' : 'normal', maxShift: STABLE_MAX_SHIFT } : null,
      hover: o.hover ? { tol: HOVER.tol, grid: HOVER.grid } : null,
      pages,
      pass: pages.every((p) => p.pass),
    };
  } finally {
    if (browser) { await browser.close().catch(() => {}); live.browsers.delete(browser); }
    await Promise.all(Object.values(servers).map((s) => s.stop()));
  }
}

// ------------------------------------------------------------------ the readable report

const COLUMNS = [
  ['size', (s) => s.size],
  ['ready', (s) => (s.loadError ? 'no load' : s.ready.ok ? 'yes' : s.ready.reason === 'error' ? 'error' : 'no')],
  ['missing', (s) => s.missing.length],
  ['foreign', (s) => s.foreign.length],
  ['unknown', (s) => s.unknown.length],
  ['console', (s) => s.console.length + ((s.dropped && s.dropped.console) || 0)],
  ['page err', (s) => s.pageErrors.length + ((s.dropped && s.dropped.pageErrors) || 0)],
  ['csp', (s) => s.csp.length + ((s.dropped && s.dropped.csp) || 0)],
  ['api', (s) => s.apiFailures.length + ((s.dropped && s.dropped.apiFailures) || 0)],
  ['outside', (s) => s.outside.length + ((s.dropped && s.dropped.outside) || 0)],
  ['overflow', (s) => (!s.overflow ? '?' : s.overflow.document || s.overflow.count ? 'yes' : 'no')],
  ['small text', (s) => (!s.smallText ? '?' : s.smallText.count)],
];
const PIXEL_COLUMN = ['pixel', (s) => (!s.pixel ? '?' : s.pixel.count)];
const STABLE_COLUMNS = [
  ['shift', (s) => (!s.stable || s.stable.error ? '?' : s.stable.shift.toFixed(3))],
  ['moved', (s) => (!s.stable || s.stable.error ? '?' : s.stable.movedCount)],
  ['flicker', (s) => (!s.stable || s.stable.error ? '?' : s.stable.togglingCount)],
];

const HOVER_COLUMN = ['hover', (s) => (!s.hover || s.hover.error ? '?' : s.hover.count)];

const listOf = (arr, n, f = String) => arr.slice(0, n).map(f).join(', ') + (arr.length > n ? `, and ${arr.length - n} more` : '');

function details(s, pageId) {
  const d = [];
  if (s.loadError) d.push(`not loaded: ${s.loadError}`);
  else if (!s.ready.ok) {
    const secs = `${((s.ready.ms || 0) / 1000).toFixed(1)} s`;
    if (s.ready.reason === 'error') d.push(`load error: data-ready is "${s.ready.value}"`);
    else if (s.ready.reason === 'other_page') d.push(`not ready: data-ready is "${s.ready.value}", expected "${pageId}"`);
    else d.push(`not ready: data-ready ${s.ready.value ? `is "${s.ready.value}"` : 'was never set'} (waited ${secs})`);
  }
  if (s.ready.landed && s.ready.landed !== s.url) d.push(`landed on ${s.ready.landed}`);
  if (s.missing.length) d.push(`missing (${s.missing.length}): ${listOf(s.missing, 12)}`);
  if (s.foreign.length) d.push(`items of other pages (${s.foreign.length}): ${listOf(s.foreign, 8, (f) => `${f.id} (${f.page})`)}`);
  if (s.unknown.length) d.push(`unknown items (${s.unknown.length}): ${listOf(s.unknown, 8)}`);
  for (const c of s.console.slice(0, 3)) d.push(`console: ${clip(c.text, 200)}${c.url ? ` (${c.url}:${c.line})` : ''}`);
  if (s.console.length > 3) d.push(`console: and ${s.console.length - 3} more`);
  for (const e of s.pageErrors.slice(0, 3)) d.push(`page error: ${clip(e.message, 200)}${e.stack ? ` at ${clip(e.stack.split(' | ')[0], 120)}` : ''}`);
  if (s.pageErrors.length > 3) d.push(`page error: and ${s.pageErrors.length - 3} more`);
  for (const c of s.csp.slice(0, 4)) {
    d.push(c.message ? `csp: ${clip(c.message, 200)}`
      : `csp: ${c.directive} blocked ${c.blocked || '?'}${c.worker ? ' in a worker' : ''}${c.source ? ` (${c.source}${c.line ? ':' + c.line : ''})` : ''}`);
  }
  if (s.csp.length > 4) d.push(`csp: and ${s.csp.length - 4} more`);
  for (const a of s.apiFailures.slice(0, 5)) d.push(`api: ${a.method} ${a.url} -> ${a.status == null ? a.error : a.status}${a.code ? ' ' + a.code : ''}`);
  if (s.apiFailures.length > 5) d.push(`api: and ${s.apiFailures.length - 5} more`);
  for (const x of s.outside.slice(0, 4)) d.push(`outside: ${x.method} ${x.url} (${x.type})`);
  if (s.outside.length > 4) d.push(`outside: and ${s.outside.length - 4} more`);
  if (s.overflow && s.overflow.document) d.push(`overflow: the page is ${s.overflow.scrollWidth} px wide in a ${s.overflow.clientWidth} px window`);
  if (s.overflow && s.overflow.count) d.push(`overflow: ${plural(s.overflow.count, 'element')} cut by the window edge: ${listOf(s.overflow.elements, 3, (e) => `${e.selector} (${e.left} to ${e.right} px)`)}`);
  if (s.smallText && s.smallText.count) d.push(`small text: ${plural(s.smallText.count, 'text')} under ${MIN_TEXT_PX} px; smallest: ${s.smallText.smallest.map((t) => `${t.px} px ${t.selector} "${clip(t.text, 30)}"`).join('; ')}`);
  if (s.pixel && s.pixel.count) {
    d.push(`pixel: ${plural(s.pixel.count, 'problem')}${s.pixel.shot ? `, outlined in ${rel(s.pixel.shot)}` : ''}`);
    for (const f of s.pixel.findings.slice(0, PIXEL_LINES)) d.push(`pixel: ${f.kind} ${f.selector}: ${f.what}`);
    if (s.pixel.count > PIXEL_LINES) d.push(`pixel: and ${s.pixel.count - PIXEL_LINES} more${s.pixel.count > s.pixel.findings.length ? ` (${s.pixel.findings.length} in --json)` : ' (all in --json)'}`);
  }
  if (s.stable) d.push(...stableDetails(s.stable));
  if (s.hover) d.push(...hoverDetails(s.hover));
  for (const n of s.notes) d.push(`note: ${n}`);
  if (s.dropped) d.push(`not listed: ${Object.entries(s.dropped).map(([k, n]) => `${n} more ${k}`).join(', ')}`);
  return d;
}

function stableDetails(st) {
  const d = [];
  if (st.error) { d.push(`stable: could not read the watch: ${st.error}`); return d; }
  const ms = (a) => a.map((t) => `${(t / 1000).toFixed(1)}`).join(', ') + ' s';
  if (st.shift > STABLE_MAX_SHIFT) {
    d.push(`stable: layout shift ${st.shift.toFixed(3)} after ready (more than ${STABLE_MAX_SHIFT})${st.shiftSources.length ? `; moved: ${listOf(st.shiftSources, 4, (x) => `${x.selector} (${x.px == null ? 'into or out of the window' : `${x.px > 0 ? '+' : ''}${x.px} px`}, ${x.times}x)`)}` : ''}`);
  }
  for (const m of st.moved.slice(0, STABLE_LINES)) {
    d.push(`stable: moved ${m.selector} by ${m.px > 0 ? '+' : ''}${m.px} px, ${m.times}x (at ${ms(m.atMs)})${m.cause ? ` after ${m.cause}` : ''}`);
  }
  if (st.movedCount > STABLE_LINES) d.push(`stable: and ${st.movedCount - STABLE_LINES} more moved blocks (--json)`);
  for (const m of (st.scrollMoved || []).slice(0, STABLE_LINES)) d.push(`stable: moved ${m.selector} by ${m.px > 0 ? '+' : ''}${m.px} px while the page was scrolled through (a part drawn on the way had no room kept)`);
  if (st.scrollMovedCount > STABLE_LINES) d.push(`stable: and ${st.scrollMovedCount - STABLE_LINES} more blocks moved while scrolling (--json)`);
  for (const t of st.toggling.slice(0, STABLE_LINES)) {
    const how = t.first === 'appeared' ? 'appeared and went away again' : 'went away and came back';
    d.push(`stable: flicker ${t.selector}${t.text ? ` "${clip(t.text, 30)}"` : ''} ${how} (${t.times} changes, at ${ms(t.atMs)})`);
  }
  if (st.togglingCount > STABLE_LINES) d.push(`stable: and ${st.togglingCount - STABLE_LINES} more toggling elements (--json)`);
  for (const l of st.late.slice(0, 4)) d.push(`stable: late block ${l.selector} (${l.h} px) appeared ${(l.atMs / 1000).toFixed(1)} s after ready and pushed ${plural(l.pushed, 'block')} down`);
  if (stableFails(st) && st.outlined) d.push(`stable: outlined in ${rel(st.outlined)}`);
  return d;
}

// One line per size under the table: the watch itself (how long, what arrived, the time from the first paint).
function hoverDetails(h) {
  if (h.error) return [`hover: could not hover: ${h.error}`];
  const d = [];
  const px = (v) => `${v > 0 ? '+' : ''}${v}`;
  const what = (m) => [m.dx && `x ${px(m.dx)}`, m.dy && `y ${px(m.dy)}`, m.dw && `width ${px(m.dw)}`, m.dh && `height ${px(m.dh)}`].filter(Boolean).join(', ');
  for (const f of h.findings.slice(0, HOVER_LINES)) {
    const how = f.kind === 'focus' ? 'focus on' : f.kind === 'point' ? 'the mouse at' : f.kind === 'chart' ? 'the mouse on the chart' : 'hover on';
    const page = [f.scrolled && 'the page scrolled', f.scrollbar && 'a page scrollbar came or went', f.stays && 'and it stayed so with the mouse away'].filter(Boolean);
    d.push(`hover: ${f.n}. ${how} ${f.target} (${f.x}, ${f.y}) moved ${listOf(f.moved, 2, (m) => `${m.selector} (${what(m)} px)`)}${f.count > f.moved.length ? ` (${f.count} in all)` : ''}${page.length ? '; ' + page.join(', ') : ''}`);
  }
  if (h.findings.length > HOVER_LINES) d.push(`hover: and ${h.findings.length - HOVER_LINES} more (--json)`);
  if (h.shot) d.push(`hover: outlined in ${rel(h.shot)}`);
  return d;
}

function stableSummary(s) {
  const st = s.stable;
  if (!st || st.error) return null;
  const ev = Object.entries(st.events).map(([k, n]) => `${n} ${k}`).join(', ') || 'no events';
  return `watched ${(st.watchedMs / 1000).toFixed(0)} s: ${ev}, ${plural(st.apiRequests, '/api request')}; first paint ${st.firstPaintMs == null ? '?' : st.firstPaintMs} ms, ready ${st.readyMs} ms, shift before ready ${st.shiftBeforeReady.toFixed(3)}`;
}

function formatReport(rep) {
  const L = [];
  L.push(`Page check of pack ${rep.pack} (${rep.packDir}), matrix ${rep.matrix}`);
  const opened = Object.entries(rep.opened).filter(([, v]) => v).map(([k, v]) => `${k} ${v}`).join(', ');
  if (rep.server.origin) L.push(`${rep.server.real ? `Real server (signed in as ${rep.server.user})` : 'Dev server'} ${rep.server.origin}${rep.server.args.length ? ` (${rep.server.args.join(' ')})` : ''}${opened ? `; opened ${opened}` : ''}`);
  L.push(`Screenshots in ${rep.out}`);
  if (rep.stable) L.push(`Stability watch: ${rep.stable.seconds} s per page and size after ready, ${rep.stable.pace} pace of live updates; fails above a layout shift of ${rep.stable.maxShift}, on a block that moves more than 1 px or anything that comes and goes more than once`);
  L.push('');
  let pass = 0, warn = 0;
  for (const p of rep.pages) {
    const warned = p.pass && p.sizes.some((s) => s.missing.length || s.notes.length);
    if (p.pass) pass++;
    if (warned) warn++;
    L.push(`${p.pass ? (warned ? 'WARN' : 'PASS') : 'FAIL'}  ${p.id}  ${p.url}`);
    const cols = [...COLUMNS, ...(rep.pixel ? [PIXEL_COLUMN] : []), ...(rep.stable ? STABLE_COLUMNS : []), ...(rep.hover ? [HOVER_COLUMN] : [])];
    const rows = [cols.map(([h]) => h), ...p.sizes.map((s) => cols.map(([, f]) => String(f(s))))];
    const widths = cols.map((_, i) => Math.max(...rows.map((r) => r[i].length)));
    for (const r of rows) L.push('  ' + r.map((c, i) => c.padEnd(widths[i])).join('  ').trimEnd());
    // detail lines, once for the sizes that share them
    const bySize = p.sizes.map((s) => details({ ...s, url: p.url }, p.id));
    const order = [];
    const where = new Map();
    bySize.forEach((lines, i) => lines.forEach((l) => {
      if (!where.has(l)) { where.set(l, []); order.push(l); }
      where.get(l).push(p.sizes[i].size);
    }));
    for (const l of order) {
      const sizes = where.get(l);
      const tag = p.sizes.length > 1 ? (sizes.length === p.sizes.length ? 'all sizes' : sizes.join(', ')) + ': ' : '';
      L.push(`  ${tag}${l}`);
    }
    if (rep.stable) for (const s of p.sizes) { const line = stableSummary(s); if (line) L.push(`  ${s.size}: ${line}`); }
    L.push('');
  }
  L.push(`${pass} of ${plural(rep.pages.length, 'page route')} pass${warn ? ` (${warn} with warnings)` : ''}.${rep.allowMissing ? ' Missing items only warn (--allow-missing).' : ''}`);
  return L.join('\n') + '\n';
}

// ------------------------------------------------------------------ self-test

async function selfTest(o) {
  const out = path.join(o.out, 'self-test');
  const matrixFile = path.join(FIXTURES_DIR, 'matrix.json');
  const common = {
    pages: 'fleet,drives,drive,charge,health,sign-in,setup',
    sizes: parseSizes('1280x800,1600x1000'),
    out, matrixFile, serverArgs: [], allowMissing: false, full: true,
    readyTimeoutMs: 3000, idleTimeoutMs: 5000, jobs: Math.max(o.jobs, 3), log: o.log,
  };
  const checks = [];
  const expect = (what, ok) => checks.push({ what, ok: !!ok });
  const find = (rep, url) => rep.pages.find((p) => p.url === url || p.id === url);
  const every = (p, f) => !!p && p.sizes.every(f);

  const ok = await run({ ...common, packDir: path.join(FIXTURES_DIR, 'pack-ok') });
  expect('pack-ok passes', ok.pass);
  for (const p of ok.pages) for (const s of p.sizes) expect(`pack-ok ${p.url} at ${s.size} passes${s.fails.length ? ` (failed: ${s.fails.join(', ')})` : ''}`, s.pass);
  expect('{car} is the first car', ok.opened.car === 'c1');
  expect('{drive} and {charge} are resolved from the API', /^\d+$/.test(ok.opened.drive || '') && /^\d+$/.test(ok.opened.charge || ''));
  expect('health is checked at both routes', ok.pages.filter((p) => p.id === 'health').map((p) => p.url).join(' ') === '/cars/c1/health /system');
  expect('a map tile is answered locally, not counted as outside', every(find(ok, 'fleet'), (s) => s.tiles >= 1 && s.outside.length === 0));
  expect('an open event stream does not hold up "network idle"', every(find(ok, 'fleet'), (s) => !s.notes.some((n) => n.includes('busy'))));
  expect('hidden 6 px text is not counted', every(find(ok, 'fleet'), (s) => s.smallText && s.smallText.count === 0));
  expect('a 401 on the sign-in page is expected', every(find(ok, 'sign-in'), (s) => s.apiFailures.length === 0));
  expect('every route and size has a screenshot', ok.pages.every((p) => p.sizes.every((s) => s.shot && fs.existsSync(s.shot))));
  expect('--full saves a full-page screenshot at the first size', ok.pages.every((p) => p.sizes[0].fullShot && fs.existsSync(p.sizes[0].fullShot) && !p.sizes[1].fullShot));

  const bad = await run({ ...common, packDir: path.join(FIXTURES_DIR, 'pack-bad') });
  expect('pack-bad fails', !bad.pass);
  const fleet = find(bad, 'fleet');
  expect('missing item reported', every(fleet, (s) => s.missing.includes('fx.fleet.summary') && s.fails.includes('missing')));
  expect('item of another page reported', every(fleet, (s) => s.foreign.some((f) => f.id === 'fx.drives.table' && f.page === 'drives')));
  expect('unknown item reported', every(fleet, (s) => s.unknown.includes('fx.nope.unknown')));
  expect('console error reported', every(fleet, (s) => s.console.some((c) => c.text.includes('fixture console error'))));
  expect('CSP violation by an inline style attribute reported', every(fleet, (s) => s.csp.some((c) => c.directive === 'style-src-attr')));
  expect('document overflow reported', every(fleet, (s) => s.overflow.document && s.overflow.scrollWidth > s.overflow.clientWidth));
  expect('element cut by the window edge reported', every(fleet, (s) => s.overflow.elements.some((e) => e.selector.includes('div.wide'))));
  expect('8 px text reported', every(fleet, (s) => s.smallText.count >= 1 && s.smallText.smallest[0].px === 8 && s.smallText.smallest[0].selector.includes('p.tiny')));
  const drives = find(bad, 'drives');
  expect('page error reported', every(drives, (s) => s.pageErrors.some((e) => e.message.includes('fixture page error'))));
  expect('failed /api request reported', every(drives, (s) => s.apiFailures.some((a) => a.status === 404 && a.code === 'not_found')));
  expect('404 of a failed /api request is not also a console error', every(drives, (s) => s.console.length === 0));
  expect('outside request reported', every(drives, (s) => s.outside.some((x) => x.url.startsWith('https://outside.invalid/'))));
  expect('CSP violation in a worker reported', every(drives, (s) => s.csp.some((c) => c.worker && c.directive === 'connect-src')));
  expect('data-ready "error:drive" reported', every(find(bad, 'drive'), (s) => !s.ready.ok && s.ready.value === 'error:drive' && s.ready.reason === 'error'));
  expect('data-ready never set reported', every(find(bad, 'charge'), (s) => !s.ready.ok && s.ready.value === null && s.ready.reason === 'timeout'));
  expect('data-ready of another page reported', every(find(bad, '/cars/c1/health'), (s) => !s.ready.ok && s.ready.value === 'fleet' && s.ready.reason === 'other_page'));
  expect('a health item on neither route is missing', every(find(bad, '/system'), (s) => s.missing.includes('fx.health.server') && !s.missing.includes('fx.health.car')));
  expect('sign-in: 403 reported, 401 expected', every(find(bad, 'sign-in'), (s) => s.apiFailures.length === 1 && s.apiFailures[0].status === 403 && s.apiFailures[0].code === 'csrf'));
  expect('a clean page in a failing pack passes', find(bad, 'setup') && find(bad, 'setup').pass);
  expect('the report reads', /FAIL {2}fleet {2}\//.test(formatReport(bad)) && /PASS {2}setup/.test(formatReport(bad)));

  // --allow-missing turns missing items into warnings
  const extra = path.join(out, 'matrix-extra.json');
  const m = JSON.parse(fs.readFileSync(matrixFile, 'utf8'));
  m.items.push({ id: 'fx.fleet.extra', page: 'fleet', section: 'Not built' });
  fs.mkdirSync(out, { recursive: true });
  fs.writeFileSync(extra, JSON.stringify(m));
  const quick = { ...common, packDir: path.join(FIXTURES_DIR, 'pack-ok'), pages: 'fleet', sizes: parseSizes('1280x800'), full: false, matrixFile: extra };
  const strict = await run(quick);
  const lenient = await run({ ...quick, allowMissing: true });
  expect('a missing item fails without --allow-missing', !strict.pass && strict.pages[0].sizes[0].missing.join() === 'fx.fleet.extra');
  expect('a missing item only warns with --allow-missing', lenient.pass && /WARN {2}fleet/.test(formatReport(lenient)));

  // --pixel: the same cases right (fleet) and off by 1 px (drives), at scale 1 and on a phone at scale 3
  const px = await run({
    ...common, packDir: path.join(FIXTURES_DIR, 'pack-pixel'), pages: 'fleet,drives', sizes: parseSizes('1280x800,390x844@3'),
    full: false, pixel: true,
  });
  expect('390x844@3 is a phone size at device scale 3', px.sizes[1] === '390x844@3' && px.pages[0].sizes[1].pixel && px.pages[0].sizes[1].pixel.dpr === 3);
  const good = find(px, 'fleet'), off = find(px, 'drives');
  expect('pixel: the right cases pass with --pixel', good && good.pass);
  for (const s of good ? good.sizes : []) expect(`pixel: nothing reported on the right cases at ${s.size}${s.pixel && s.pixel.count ? ` (got: ${s.pixel.findings.map((f) => `${f.kind} ${f.selector}`).join('; ')})` : ''}`, s.pixel && s.pixel.count === 0 && !s.pixel.shot);
  expect('pixel: the cases off by 1 px fail', off && !off.pass && every(off, (s) => s.fails.includes('pixel')));
  const has = (s, kind, part, lo, hi) => s.pixel && s.pixel.findings.some((f) => f.kind === kind && f.selector.includes(part) && (lo == null || (f.by >= lo && f.by <= hi)));
  const want = [
    ['text 1 px right of centre (text-align)', 'centre', 'badge.bad', 0.9, 1.1],
    ['button text 1 px below centre', 'centre', 'chip.bad', 0.9, 1.1],
    ['icon 1 px low in a flex box', 'centre', 'svg.glyph', 0.9, 1.1],
    ['initials 1 px left in a flex box', 'centre', 'avatar.bad', 0.9, 1.1],
    ['centred text 1 px left of a 2 px spacer', 'centre', 'empty.bad', 0.9, 1.1],
    ['a field 1 px lower in a row', 'row', 'f-select', 0.9, 1.1],
    ['a table cell 1 px lower', 'row', 'td.c2', 0.9, 1.1],
    ['a stacked block 2 px in', 'column', 'div.s2', 1.9, 2.1],
    ['a border on a half pixel', 'fraction', 'frac.bad'],
    ['a 1 px line on a half pixel', 'fraction', 'rule.bad'],
    ['text cut off', 'clip', 'clipbox.bad'],
    ['text sticking out of its box', 'overflow', 'over.bad'],
    ['text on text', 'overlap', 'labels.bad'],
    ['text under an opaque box', 'hidden', 'covered'],
    ['sideways scroll', 'scroll', 'html'],
  ];
  for (const [what, kind, part, lo, hi] of want) expect(`pixel: ${what} reported (${kind})`, every(off, (s) => has(s, kind, part, lo, hi)));
  expect('pixel: nothing right is reported on the bad page', every(off, (s) => s.pixel.findings.every((f) => !/\.ok\b|\bstretch\b|tight|sr-only/.test(f.selector))));
  expect('pixel: an outlined screenshot of the problems', every(off, (s) => s.pixel.shot && fs.existsSync(s.pixel.shot)));
  expect('pixel: the report lists kind, selector and by how much', /pixel: centre [^\n]*badge\.bad: text (0\.9|1\.0)\d px right of the centre/.test(formatReport(px)));
  expect('pixel: no pixel column or check without --pixel', ok.pages.every((p) => p.sizes.every((s) => s.pixel === null)) && !/ pixel\n/.test(formatReport(ok)));

  // --stable: a page that stays still passes, one that jumps and one that flickers fail (fast pace, 6 s)
  const sb = await run({
    ...common, packDir: path.join(FIXTURES_DIR, 'pack-stable'), pages: 'fleet,drives,drive', sizes: parseSizes('1280x800,390x844@3'),
    full: false, stable: true, stableSeconds: 6, stableFast: true, serverArgs: [...STABLE_PACE.fast],
  });
  const still = find(sb, 'fleet'), jumps = find(sb, 'drives'), flickers = find(sb, 'drive');
  const st = (s) => s.stable || {};
  expect('stable: live updates arrive during the watch (now and rows events, /api requests)', every(still, (s) => (st(s).events || {}).now >= 5 && (st(s).events || {}).rows >= 1 && st(s).apiRequests >= 1 && st(s).watchedMs >= 5500));
  expect(`stable: the still page passes (values change in place, a list drawn again in one go, a spinner, a map and a chart redraw)${still && !still.pass ? ` (got: ${still.sizes.flatMap((s) => (s.stable ? stableDetails(s.stable) : ['no watch']).map((l) => `${s.size} ${l}`)).join('; ')})` : ''}`, still && still.pass);
  expect('stable: the still page has shift 0, nothing moved, nothing toggling', every(still, (s) => st(s).shift <= 0.01 && st(s).movedCount === 0 && st(s).togglingCount === 0));
  expect('stable: before and after screenshots', every(still, (s) => st(s).before && fs.existsSync(st(s).before) && st(s).after && fs.existsSync(st(s).after)));
  expect('stable: the jumping page fails', jumps && !jumps.pass && every(jumps, (s) => s.fails.join() === 'stable'));
  expect('stable: its layout shift is over 0.01', every(jumps, (s) => st(s).shift > 0.01 && st(s).shiftSources.some((x) => x.selector.includes('section.card'))));
  expect('stable: its list is reported moved down, with how much and why', every(jumps, (s) => st(s).moved.some((m) => m.selector.includes('section.card') && m.px > 1 && m.times >= 3 && /banner/.test(m.cause || ''))));
  expect('stable: the block that came late and pushed it is named', every(jumps, (s) => st(s).late.some((l) => l.selector.includes('div.banner.late') && l.pushed >= 1 && l.atMs >= 700)));
  expect('stable: the flickering page fails', flickers && !flickers.pass && every(flickers, (s) => s.fails.join() === 'stable'));
  expect('stable: its "Loading…" coming back is reported, with how often', every(flickers, (s) => st(s).toggling.some((t) => t.selector.includes('p.loading') && t.times >= 4 && t.first === 'appeared')));
  expect('stable: its rows going and coming back are reported', every(flickers, (s) => st(s).toggling.some((t) => /div\.box > p:nth-of-type\(1\)/.test(t.selector) && t.first === 'went away')));
  expect('stable: a badge hidden and shown again (visibility) is reported', every(flickers, (s) => st(s).toggling.some((t) => t.selector.includes('p.badge'))));
  expect('stable: the flicker moved nothing and shifted nothing', every(flickers, (s) => st(s).movedCount === 0 && st(s).shift <= 0.01));
  expect('stable: an outlined screenshot of what moved and flickered', [jumps, flickers].every((p) => every(p, (s) => st(s).outlined && fs.existsSync(st(s).outlined))));
  expect('stable: the report has shift, moved and flicker columns and says why', /shift {2}moved {2}flicker/.test(formatReport(sb)) && /stable: moved main#view > section\.card by \+\d+(\.\d)? px/.test(formatReport(sb)) && /stable: flicker [^\n]*p\.loading appeared and went away again/.test(formatReport(sb)));
  expect('stable: no stability column or watch without --stable', ok.pages.every((p) => p.sizes.every((s) => s.stable === null)) && !/ flicker\n/.test(formatReport(ok)));

  // --hover: the hStill page passes, each case on the moving page is found (with what moved and by how much)
  const hv = await run({
    ...common, packDir: path.join(FIXTURES_DIR, 'pack-hover'), pages: 'fleet,drives', sizes: parseSizes('1280x800,1600x1000'),
    full: false, hover: true, serverArgs: ['--freeze-clock'],
  });
  const hStill = find(hv, 'fleet'), hMoves = find(hv, 'drives');
  const hf = (s) => (s.hover && s.hover.findings) || [];
  const hfound = (s, kind, target, moved, test) => hf(s).some((f) => f.kind === kind && f.target.includes(target) && (!moved || f.moved.some((m) => m.selector.includes(moved) && (!test || test(m)))));
  expect('hover: something was hovered, focused and swept on each page (elements, grid points, chart points, focus)', [hStill, hMoves].every((p) => every(p, (s) => s.hover && s.hover.targets >= 5 && s.hover.points >= 1 && s.hover.charts >= 10 && s.hover.focused >= 3)));
  expect(`hover: the hStill page passes (colour, shadow, transform, an overlay tooltip, a readout of fixed width, opacity)${hStill && !hStill.pass ? ` (got: ${hStill.sizes.flatMap((s) => (s.hover ? hoverDetails(s.hover) : ['no hover']).map((l) => `${s.size} ${l}`)).join('; ')})` : ''}`, hStill && hStill.pass);
  expect('hover: the moving page fails on hover only', hMoves && !hMoves.pass && every(hMoves, (s) => s.fails.join() === 'hover'));
  const hwant = [
    ['a button that turns bold moves the one beside it', (s) => hfound(s, 'element', 'button.btn.bold', 'button.btn:nth-of-type(2)', (m) => m.dx > 0.5)],
    ['a row whose padding grows moves the rows under it', (s) => hfound(s, 'element', 'tr.pad', 'tr.pad:nth-of-type(2)', (m) => m.dy === 4)],
    ['an edit icon that takes room moves the text after it', (s) => hfound(s, 'element', 'div.row.icon', 'div.row.icon:nth-of-type(2) > span:nth-of-type(2)', (m) => m.dx > 5)],
    ['a chart tooltip in the flow hMoves what is under the chart', (s) => hfound(s, 'chart', 'svg.chart', 'section.card.bad > p', (m) => m.dy > 5)],
    ['a box that gets a scrollbar on hover narrows what is in it', (s) => hfound(s, 'element', 'div.scroll-host', 'div.scroll-host', (m) => m.dw < -5)],
    ['a tooltip that makes the page longer brings a page scrollbar', (s) => hf(s).some((f) => f.target.includes('span.low') && f.scrollbar)],
    ['focus: a thicker border hMoves what is beside the field', (s) => hfound(s, 'focus', 'input.input.thick', 'span', (m) => m.dx > 1)],
  ];
  for (const [what, test] of hwant) expect(`hover: ${what}`, every(hMoves, test));
  expect('hover: an outlined screenshot of what moved', every(hMoves, (s) => s.hover.shot && fs.existsSync(s.hover.shot)) && every(hStill, (s) => !s.hover.shot));
  expect('hover: the report has a hover column and says what moved', / hover\n/.test(formatReport(hv)) && /hover: \d+\. hover on [^\n]*button\.btn\.bold[^\n]* moved [^\n]*\(x \+\d/.test(formatReport(hv)));
  expect('hover: no hover column or check without --hover', ok.pages.every((p) => p.sizes.every((s) => s.hover === null)) && !/ hover\n/.test(formatReport(ok)));

  const failed = checks.filter((c) => !c.ok);
  const L = checks.map((c) => `${c.ok ? 'ok  ' : 'FAIL'}  ${c.what}`);
  L.push('', failed.length ? `self-test failed: ${failed.length} of ${checks.length} checks` : `self-test passed: ${checks.length} checks`);
  if (failed.length) {
    L.push('', 'pack-bad report:', formatReport(bad));
    if (!ok.pass) L.push('pack-ok report:', formatReport(ok));
    L.push('pack-pixel report:', formatReport(px));
    L.push('pack-stable report:', formatReport(sb));
    L.push('pack-hover report:', formatReport(hv));
  }
  return { pass: failed.length === 0, text: L.join('\n') + '\n', reports: { ok, bad, pixel: px, stable: sb, hover: hv } };
}

// ------------------------------------------------------------------ command line

const HELP = `Usage: node ui/tools/check.js --pack <name or folder> [options]
       node ui/tools/check.js --self-test

  --pack NAME|DIR        a pack in ui/packs (instrument, logbook, horizon, console, pebble, manager) or a pack folder with web/
  --pages LIST|all       page ids, comma separated (default all: every page of the matrix plus sign-in, setup)
  --sizes WxH[@S],...    window sizes, @S a device scale (390x844@3 is a phone; default ${DEFAULT_SIZES})
  --out DIR              screenshots go to DIR/<pack>/ (default UI_CHECK_OUT, else a new private ${DEFAULT_OUT_PREFIX}XXXXXX)
  --server-args "..."    extra dev server flags, e.g. "--cars 200 --latency-ms 150"
  --json                 print one JSON object instead of the table
  --allow-missing        missing items only warn
  --full                 also a full-page screenshot at the first size
  --fixture-matrix FILE  a coverage matrix to use instead of docs/parity/coverage-matrix.json
  --ready-timeout-ms N   how long to wait for data-ready (default 20000)
  --idle-timeout-ms N    how long to wait for the network to go quiet after that (default 10000)
  --jobs N               pages checked at the same time (default 2)
  --car ID, --drive ID, --charge ID   open these instead of the first car and its newest drive and charge
  --place ID                         the places page opens this place's editor (its list items then count as missing)
  --pixel                also check that the page is pixel centred and correct (off-centre text and icons, rows
                         and columns that just miss, borders off device pixels, cut, sticking out, overlapping or
                         hidden text, sideways scroll), with an outlined screenshot of the problems
  --stable [SECONDS]     also stay on each ready page this long (default ${STABLE_DEFAULT_S}) while the dev server sends
                         live updates, and fail it when its layout shifts (more than ${STABLE_MAX_SHIFT}), a block moves more
                         than 1 px, or an element or loading text comes and goes more than once
  --stable-fast          with --stable: live values every 0.5 s and list changes every 3 s (else every 2 s and 10 s)
  --hover                also move the mouse over a grid of points, every interactive element and every chart, and
                         focus each element that takes focus, and fail the page when anything outside what is
                         hovered moves or changes size by more than ${HOVER.tol} px (the dev server's clock is frozen)
  --jumps                instead of the checks above: the jump check of ui/tools/jumps.js for this pack (every page,
                         loaded and scrolled through on two iPhones in WebKit, a phone in Chromium and a desktop;
                         fails a page whose jump score or layout shift is over 0.02, that resizes a block above the
                         screen while it is scrolled, or that scrolls by itself); --pages narrows it
  --device-size N        the Interface size on this device (80, 90, 100, 110, 125; else auto): kept in each window's storage
  --base URL             a real sentry-mate serve (https://127.0.0.1:PORT) instead of the dev server; its
                         self-signed certificate is accepted for this run only (see ui/tools/README.md)
  --user NAME            the account to sign in with on --base (the pack becomes its own UI pack)
  --password PW, --password-file FILE   its password (or set UI_CHECK_PASSWORD)
  --self-test            check the fixtures in ui/tools/fixtures and assert the results

Exit code 0 when every checked page passes, 1 when one fails, 2 on a usage or start-up error.
`;

function parseSizes(s) {
  const sizes = String(s).split(',').map((x) => x.trim()).filter(Boolean).map((x) => {
    const m = /^(\d{2,5})x(\d{2,5})(?:@(\d+(?:\.\d+)?))?$/.exec(x);
    if (!m) throw new UsageError(`bad size "${x}": use WIDTHxHEIGHT or WIDTHxHEIGHT@SCALE, e.g. 1440x900 or 390x844@3`);
    const scale = m[3] === undefined ? 1 : +m[3];
    if (!(scale >= 1 && scale <= 4)) throw new UsageError(`bad size "${x}": the device scale must be 1 to 4`);
    return { w: +m[1], h: +m[2], scale, label: `${+m[1]}x${+m[2]}${scale !== 1 ? '@' + scale : ''}` };
  });
  if (!sizes.length) throw new UsageError('--sizes names no size');
  return sizes;
}

function parseArgs(argv) {
  const o = {
    pack: null, pages: 'all', sizes: DEFAULT_SIZES, out: null, serverArgs: '', json: false, allowMissing: false,
    full: false, pixel: false, fixtureMatrix: null, selfTest: false, readyTimeoutMs: '20000', idleTimeoutMs: '10000', jobs: '2',
    car: null, drive: null, charge: null, place: null, help: false, stable: false, hover: false, stableSeconds: STABLE_DEFAULT_S, stableFast: false, base: null, user: null, password: null, passwordFile: null,
  };
  const values = {
    '--pack': 'pack', '--pages': 'pages', '--sizes': 'sizes', '--out': 'out', '--server-args': 'serverArgs',
    '--fixture-matrix': 'fixtureMatrix', '--ready-timeout-ms': 'readyTimeoutMs', '--idle-timeout-ms': 'idleTimeoutMs',
    '--jobs': 'jobs', '--car': 'car', '--drive': 'drive', '--charge': 'charge', '--place': 'place',
    '--base': 'base', '--user': 'user', '--password': 'password', '--password-file': 'passwordFile',
    '--device-size': 'deviceSize',
  };
  const flags = { '--json': 'json', '--allow-missing': 'allowMissing', '--full': 'full', '--pixel': 'pixel', '--self-test': 'selfTest', '--help': 'help', '-h': 'help', '--stable-fast': 'stableFast', '--hover': 'hover', '--jumps': 'jumps' };
  for (let i = 0; i < argv.length; i++) {
    let a = argv[i];
    let v;
    const eq = a.indexOf('=');
    if (a.startsWith('--') && eq > 0) { v = a.slice(eq + 1); a = a.slice(0, eq); }
    if (flags[a] && v === undefined) { o[flags[a]] = true; continue; }
    if (a === '--stable') { // an optional number of seconds
      if (v === undefined && i + 1 < argv.length && /^\d+(\.\d+)?$/.test(argv[i + 1])) v = argv[++i];
      if (v !== undefined) {
        const n = Number(v);
        if (!(n >= 1 && n <= 3600)) throw new UsageError('--stable takes a number of seconds from 1 to 3600');
        o.stableSeconds = n;
      }
      o.stable = true;
      continue;
    }
    if (values[a]) {
      if (v === undefined) {
        if (i + 1 >= argv.length) throw new UsageError(`${a} needs a value`);
        v = argv[++i];
      }
      o[values[a]] = v;
      continue;
    }
    throw new UsageError(`unknown option ${argv[i]} (see --help)`);
  }
  for (const k of ['readyTimeoutMs', 'idleTimeoutMs', 'jobs']) {
    const n = Number(o[k]);
    if (!Number.isInteger(n) || n < (k === 'jobs' ? 1 : 0)) throw new UsageError(`--${k.replace(/[A-Z]/g, (c) => '-' + c.toLowerCase())} needs a whole number`);
    o[k] = n;
  }
  o.sizes = parseSizes(o.sizes);
  if (o.deviceSize != null) {
    if (o.deviceSize === 'auto') o.deviceSize = null;
    else if (![80, 90, 100, 110, 125].includes(Number(o.deviceSize))) throw new UsageError('--device-size takes auto, 80, 90, 100, 110 or 125');
    else o.deviceSize = Number(o.deviceSize);
  }
  o.serverArgs = splitArgs(o.serverArgs);
  if (o.stableFast && !o.stable) throw new UsageError('--stable-fast goes with --stable');
  if (o.stable && o.serverArgs.includes('--freeze-clock')) throw new UsageError('--stable needs the dev server\'s clock to run: leave out --freeze-clock');
  // --hover alone: nothing changes by itself while the mouse moves
  if (o.hover && !o.stable && !o.base && !o.serverArgs.includes('--freeze-clock')) o.serverArgs = ['--freeze-clock', ...o.serverArgs];
  // the pace of live updates goes first, so --server-args can still change it
  if (o.stable && !o.base) o.serverArgs = [...STABLE_PACE[o.stableFast ? 'fast' : 'normal'], ...o.serverArgs];
  o.matrixFile = o.fixtureMatrix ? path.resolve(o.fixtureMatrix) : MATRIX_FILE;
  if (o.base) {
    if (o.selfTest) throw new UsageError('--self-test runs on the dev server; leave out --base');
    if (o.serverArgs.length) throw new UsageError('--server-args is for the dev server; a real server (--base) takes none');
    if (!o.user) throw new UsageError('--base needs --user (an account on that server)');
    if (o.passwordFile) {
      try { o.password = fs.readFileSync(path.resolve(o.passwordFile), 'utf8').replace(/\r?\n$/, ''); } catch (e) { throw new UsageError(`--password-file: ${e.message}`); }
    }
    if (!o.password) o.password = process.env.UI_CHECK_PASSWORD || null;
    if (!o.password) throw new UsageError('--base needs --password, --password-file or UI_CHECK_PASSWORD');
    o.base = o.base.replace(/\/+$/, '');
  } else if (o.user || o.password || o.passwordFile) {
    throw new UsageError('--user and --password are for a real server: give --base too');
  }
  if (!o.help && !o.selfTest) {
    if (!o.pack) throw new UsageError('--pack is required (see --help)');
    const asDir = path.resolve(o.pack);
    const named = path.join(UI_DIR, 'packs', o.pack);
    if (fs.existsSync(path.join(asDir, 'web'))) o.packDir = asDir;
    else if (!o.pack.includes('/') && fs.existsSync(path.join(named, 'web'))) o.packDir = named;
    else throw new UsageError(`no pack "${o.pack}": give a name in ui/packs or a folder that has web/`);
  }
  // (last: a run refused above makes no folder)
  if (o.out === null) o.out = process.env.UI_CHECK_OUT || (o.help ? DEFAULT_OUT_PREFIX : fs.mkdtempSync(DEFAULT_OUT_PREFIX));
  o.out = path.resolve(o.out);
  return o;
}

async function main() {
  let o;
  try { o = parseArgs(process.argv.slice(2)); } catch (e) {
    if (e instanceof UsageError) { process.stderr.write(`check.js: ${e.message}\n`); process.exit(2); }
    throw e;
  }
  if (o.help) { process.stdout.write(HELP); return 0; }
  o.log = (line) => process.stderr.write(line + '\n');
  if (o.jumps) {
    // the jump check (ui/tools/jumps.js) on this pack: a built-in pack of ui/packs, on the dev server
    const J = require('./jumps');
    const name = path.basename(o.packDir);
    if (!J.PACKS.includes(name) || o.base) throw new UsageError('--jumps checks a built-in pack (ui/packs) on the dev server');
    const argv = ['--packs', name];
    if (o.pages !== 'all') argv.push('--pages', o.pages);
    const rep = await J.run({ ...J.parseArgs(argv), log: o.log });
    process.stdout.write(o.json ? JSON.stringify(rep, null, 2) + '\n' : J.formatReport(rep));
    return rep.pass ? 0 : 1;
  }
  if (o.selfTest) {
    const r = await selfTest(o);
    if (o.json) process.stdout.write(JSON.stringify({ pass: r.pass, ok: r.reports.ok, bad: r.reports.bad, pixel: r.reports.pixel, stable: r.reports.stable, hover: r.reports.hover }, null, 2) + '\n');
    else process.stdout.write(r.text);
    return r.pass ? 0 : 1;
  }
  const rep = await run(o);
  process.stdout.write(o.json ? JSON.stringify(rep, null, 2) + '\n' : formatReport(rep));
  return rep.pass ? 0 : 1;
}

if (require.main === module) {
  main().then((code) => { process.exitCode = code; }, (e) => {
    process.stderr.write(`check.js: ${e instanceof UsageError || e instanceof StartError ? e.message : e.stack || e}\n`);
    killChildren('SIGTERM');
    process.exitCode = 2;
  });
}

module.exports = { run, formatReport, selfTest, parseArgs, ROUTES, FRAME_SCREENS, startServer, killChildren, TILE_HOSTS, BLANK_TILE };
