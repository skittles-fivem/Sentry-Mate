// Sentry Mate · the service worker. The server serves it at /sw.js (crates/web/src/sw.rs), scope /. Every UI pack
// registers it (ui/shared/offline.js) once its first page is drawn.
//
// - The page (index.html: a GET navigation of this site that is no file, not /api/, /_v/ or /original): the kept
//   copy at once when there is one, while the network's copy is fetched behind it and kept for the next load.
// - Files under /_v/<version>/: the kept copy first; they never change. Once a new page names another version,
//   the files of older versions go. After the first page is drawn, the files the page names, and those it loaded
//   before this worker answered it (its page module, that module's parts, the fonts it drew with), are kept.
// - Other files of the site: the network; the kept copy only when the network fails.
// - API reads: the network first. The last JSON answer of each address is kept in this worker's memory only
//   (never on disk), at most MEMORY_ENTRIES answers and MEMORY_BYTES, each for MEMORY_AGE_MS; never one that says
//   `no-store`. A read with `X-Sentry-Mate-Stale-Ok: 1` gets the kept answer at once, with
//   `X-Sentry-Mate-Stale: <age in ms>`; so does any read while the network fails. A stale-ok read with no kept
//   answer joins the read of the same address under way (one sent without a tag), instead of asking again. The live
//   stream and every change go as they are.
// - Who is signed in and which pack they see: signing in or out, a change of preferences or packs, and any 401 on
//   a read drop the kept answers and the kept page; signing out drops the files too, and so does the message
//   { type: 'sentry-mate:clear' } from a page. /original and /original/off drop the kept page.

const SHELL = 'sm-shell-v1';
const ASSETS = 'sm-assets-v1';
const CURRENT = [SHELL, ASSETS];
// the one kept page, whatever address asked for it (every page address gets the same index.html)
const SHELL_KEY = '/';
const MEMORY_ENTRIES = 300;
const MEMORY_BYTES = 4 << 20;
const MEMORY_AGE_MS = 10 * 60 * 1000;
const STALE_OK = 'X-Sentry-Mate-Stale-Ok';
const STALE = 'X-Sentry-Mate-Stale';
const VERSIONED = /^\/_v\/([^/]+)\//;

// url -> { body: ArrayBuffer, status, type, at }; in the order they were kept (the oldest first)
const memory = new Map();
let memoryBytes = 0;
// one more on every clear: work started before a clear keeps nothing
let generation = 0;

self.addEventListener('install', () => { self.skipWaiting(); });

self.addEventListener('activate', (event) => {
  event.waitUntil((async () => {
    for (const name of await caches.keys()) if (!CURRENT.includes(name)) await caches.delete(name);
    await self.clients.claim();
  })());
});

self.addEventListener('message', (event) => {
  const type = event.data && event.data.type;
  const reply = () => { if (event.ports && event.ports[0]) event.ports[0].postMessage({ type: `${type}:done` }); };
  if (type === 'sentry-mate:clear') event.waitUntil(clear({ files: true }).then(reply, reply));
  else if (type === 'sentry-mate:warm') event.waitUntil(warm(event.data.files).then(reply, reply));
});

self.addEventListener('fetch', (event) => {
  const req = event.request;
  const url = new URL(req.url);
  if (url.origin !== self.location.origin) return;
  const path = url.pathname;
  if (path === '/api' || path.startsWith('/api/')) return api(event, req, path, url);
  if (req.method !== 'GET' || req.headers.has('range') || path === '/sw.js') return;
  if (path === '/original' || path === '/original/off') {
    event.respondWith(clear({ files: false }).then(() => fetch(req)));
    return;
  }
  if (VERSIONED.test(path)) return event.respondWith(asset(event, req, url));
  const last = path.slice(path.lastIndexOf('/') + 1);
  if (req.mode === 'navigate' && !last.includes('.')) return event.respondWith(shell(event, req));
  if (last.includes('.')) return event.respondWith(file(event, req, url));
});

/* ------------------------------------------------------------------ the page */

async function shell(event, req) {
  const gen = generation;
  let kept = null;
  try { kept = await (await caches.open(SHELL)).match(SHELL_KEY); } catch { /* no cache: the network */ }
  // With nothing kept the page goes to the network as it is, and is not kept from here: a navigation from another
  // site carries no session cookie (SameSite=Strict), so its answer may be the signed-out page. The page keeps
  // itself once it runs (the 'sentry-mate:warm' message, a request of this site's own).
  if (!kept) return fetch(req);
  const served = kept.clone();
  // the refresh behind the kept page is this worker's own request, so it carries the session cookie
  const network = fetch(req.url, { credentials: 'same-origin', cache: 'no-cache', redirect: 'manual' });
  event.waitUntil(network.then((res) => keepShell(res, gen, served), () => {}).catch(() => {}));
  return kept;
}

// Keeps a page answer for the next load, then drops the files of versions the pages no longer name (the one the
// page just answered from the cache still loads its files from there).
async function keepShell(res, gen, served) {
  if (res.status !== 200 || res.redirected || res.type !== 'basic') return;
  if (!(res.headers.get('content-type') || '').startsWith('text/html')) return;
  if (/no-store/i.test(res.headers.get('cache-control') || '')) return;
  const text = await res.clone().text();
  if (gen !== generation) return;
  await (await caches.open(SHELL)).put(SHELL_KEY, res);
  const version = versionOf(text);
  if (!version) return;
  const keep = [version];
  if (served) keep.push(versionOf(await served.text()));
  await prune(keep);
}

function versionOf(html) {
  const m = /["']\/_v\/([A-Za-z0-9_-]+)\//.exec(html || '');
  return m ? m[1] : null;
}

async function prune(keep) {
  const cache = await caches.open(ASSETS);
  for (const r of await cache.keys()) {
    const m = VERSIONED.exec(new URL(r.url).pathname);
    if (m && !keep.includes(m[1])) await cache.delete(r);
  }
}

/* ------------------------------------------------------------------ files */

async function asset(event, req, url) {
  const key = url.origin + url.pathname;
  try {
    const hit = await (await caches.open(ASSETS)).match(key);
    if (hit) return hit;
  } catch { /* no cache: the network */ }
  const gen = generation;
  const res = await fetch(req);
  if (keepable(res)) event.waitUntil(keepFile(key, res.clone(), gen));
  return res;
}

async function file(event, req, url) {
  const key = url.origin + url.pathname;
  const gen = generation;
  try {
    const res = await fetch(req);
    if (keepable(res)) event.waitUntil(keepFile(key, res.clone(), gen));
    return res;
  } catch (err) {
    const hit = await caches.open(ASSETS).then((c) => c.match(key), () => null);
    if (hit) return hit;
    throw err;
  }
}

function keepable(res) {
  return res.status === 200 && res.type === 'basic' && !/no-store/i.test(res.headers.get('cache-control') || '');
}

async function keepFile(key, res, gen) {
  try {
    const cache = await caches.open(ASSETS);
    if (gen === generation) await cache.put(key, res);
  } catch { /* full: the network next time */ }
}

// The page and the files it names, fetched and kept once the page has drawn (the first visit loads them before
// this worker answers anything), and the files of this version the page says it loaded before (`files`: paths;
// they never change, so the browser's own cache answers these fetches): without them the next visit's page module
// and fonts came from the network.
const WARM_FILES = 200;
const WARM_AT_ONCE = 6;
const warming = new Set(); // the files a warm-up is fetching now
async function warm(files) {
  const gen = generation;
  let page = await (await caches.open(SHELL)).match(SHELL_KEY);
  if (!page) {
    const res = await fetch('/', { credentials: 'same-origin' });
    await keepShell(res.clone(), gen, null);
    page = res;
  }
  const html = await page.text();
  const version = versionOf(html);
  if (!version) return;
  const cache = await caches.open(ASSETS);
  const want = new Set();
  for (const m of html.matchAll(/(?:href|src)="(\/_v\/[^"]+)"/g)) {
    if (m[1].startsWith(`/_v/${version}/`)) want.add(m[1]);
  }
  for (const f of Array.isArray(files) ? files.slice(0, WARM_FILES) : []) {
    if (typeof f === 'string' && f.startsWith(`/_v/${version}/`) && !/[?#]|\.\./.test(f)) want.add(f);
  }
  // a few at a time (one after another, each a round trip, the next visit came before the last were kept), and
  // never a file another warm-up is fetching already (the page tells this worker its files in batches)
  const list = [...want];
  const one = async () => {
    for (let path = list.shift(); path !== undefined; path = list.shift()) {
      const key = self.location.origin + path;
      if (gen !== generation) return;
      if (warming.has(key) || (await cache.match(key))) continue;
      warming.add(key);
      try {
        const res = await fetch(key, { credentials: 'same-origin' });
        if (keepable(res)) await keepFile(key, res, gen);
      } catch { /* offline: next time */ } finally { warming.delete(key); }
    }
  };
  await Promise.all(Array.from({ length: WARM_AT_ONCE }, one));
}

/* ------------------------------------------------------------------ the API */

// Changes that change who is signed in, what they see or which pack they get.
function changesWho(path) {
  return path === '/api/v1/session' || path.startsWith('/api/v1/session/') || path === '/api/v1/setup'
    || path === '/api/v1/me/preferences' || path === '/api/v1/ui-packs' || path.startsWith('/api/v1/ui-packs/');
}

function api(event, req, path, url) {
  if (path === '/api/v1/events' || path.startsWith('/api/v1/events/')) return; // the live stream
  if (req.method !== 'GET') {
    if (!changesWho(path)) return;
    const out = req.method === 'DELETE' && path === '/api/v1/session';
    // dropped before (no read started now is kept) and again once the server has answered
    event.respondWith(clear({ files: out }).then(() => fetch(req)).then(
      (res) => clear({ files: out }).then(() => res),
      (err) => clear({ files: out }).then(() => { throw err; })));
    return;
  }
  if ((req.headers.get('accept') || '').includes('text/event-stream')) return;
  event.respondWith(read(event, req, url.href));
}

// the network reads under way that a stale-ok read may join: url -> { joiners: [{ resolve, reject }] }. Only reads
// sent without a tag (an answer to one is never a 304 that the joiner could not read).
const underway = new Map();

async function read(event, req, key) {
  if (req.headers.get(STALE_OK) === '1') {
    const e = recall(key);
    if (e) return stale(e);
    // nothing kept: the read of the same address under way answers this one too (the page asks with stale-ok when
    // its own read is slow; a second request to the server would only make both slower)
    const w = underway.get(key);
    if (w) return new Promise((resolve, reject) => w.joiners.push({ resolve, reject }));
  }
  const gen = generation;
  const mine = req.headers.has('if-none-match') || underway.has(key) ? null : { joiners: [] };
  if (mine) underway.set(key, mine);
  const done = () => { if (mine && underway.get(key) === mine) underway.delete(key); };
  let res;
  try {
    res = await fetch(req);
  } catch (err) {
    done();
    for (const j of mine?.joiners || []) j.reject(err);
    const e = recall(key);
    if (e) return stale(e);
    throw err;
  }
  done();
  // each joiner gets its own copy, taken before the page reads this one
  for (const j of mine?.joiners || []) j.resolve(res.clone());
  if (res.status === 401) {
    await clear({ files: false });
    return res;
  }
  if (res.ok) event.waitUntil(remember(key, res.clone(), gen).catch(() => {}));
  return res;
}

async function remember(key, res, gen) {
  const type = res.headers.get('content-type') || '';
  if (!/^application\/([\w.-]+\+)?json/i.test(type)) return;
  if (/no-store/i.test(res.headers.get('cache-control') || '')) return;
  const body = await res.arrayBuffer();
  if (gen !== generation || body.byteLength > MEMORY_BYTES) return;
  forget(key);
  memory.set(key, { body, status: res.status, type, at: Date.now() });
  memoryBytes += body.byteLength;
  const now = Date.now();
  for (const [k, e] of memory) {
    if (memory.size <= MEMORY_ENTRIES && memoryBytes <= MEMORY_BYTES && now - e.at <= MEMORY_AGE_MS) break;
    forget(k);
  }
}

function recall(key) {
  const e = memory.get(key);
  if (!e) return null;
  if (Date.now() - e.at > MEMORY_AGE_MS) {
    forget(key);
    return null;
  }
  return e;
}

function forget(key) {
  const e = memory.get(key);
  if (!e) return;
  memoryBytes -= e.body.byteLength;
  memory.delete(key);
}

function stale(e) {
  const headers = { 'Content-Type': e.type, 'Cache-Control': 'no-store' };
  headers[STALE] = String(Math.max(0, Date.now() - e.at));
  return new Response(e.body.slice(0), { status: e.status, headers });
}

/* ------------------------------------------------------------------ clearing */

async function clear({ files }) {
  generation += 1;
  memory.clear();
  memoryBytes = 0;
  try {
    await caches.delete(SHELL);
    if (files) await caches.delete(ASSETS);
  } catch { /* nothing kept */ }
}
