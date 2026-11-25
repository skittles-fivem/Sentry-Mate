// Tests for offline.js (registering and clearing the service worker) and for the worker itself
// (crates/web/src/sw.js, the script the server serves at /sw.js), run against stand-ins for the browser.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import { registerServiceWorker, clearServiceWorker } from '../offline.js';

const ORIGIN = 'https://sentry.example';

function page({ protocol = 'https:', secure = true, sw = true } = {}) {
  const log = [];
  const controller = { postMessage: (m, ports) => { log.push(['controller', m.type]); if (ports) ports[0].postMessage('done'); } };
  const reg = { active: { postMessage: (m) => log.push(['active', m.type]) } };
  const names = new Set(['sm-shell-v1', 'sm-assets-v1', 'other']);
  const g = {
    isSecureContext: secure,
    location: { protocol },
    document: { readyState: 'complete' },
    addEventListener: () => {},
    setTimeout,
    clearTimeout,
    requestIdleCallback: (f) => f(),
    MessageChannel,
    caches: { keys: async () => [...names], delete: async (n) => names.delete(n) },
    navigator: sw ? { serviceWorker: {
      controller,
      ready: Promise.resolve(reg),
      register: async (url, o) => { log.push(['register', url, o.scope]); return reg; },
    } } : {},
  };
  return { g, log, names };
}

test('registers /sw.js for the whole site over HTTPS, then asks it to keep the page', async () => {
  const { g, log } = page();
  assert.ok(await registerServiceWorker(g));
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(log, [['register', '/sw.js', '/'], ['active', 'sentry-mate:warm']]);
});

test('the warm message names the versioned files this load fetched before the worker answered it', async () => {
  const { g } = page();
  const sent = [];
  g.location = { protocol: 'https:', origin: ORIGIN, href: `${ORIGIN}/cars/c1` };
  g.performance = { getEntriesByType: () => [`${ORIGIN}/_v/aaa/pages/car.js`, `${ORIGIN}/_v/aaa/fonts/a.woff2`, `${ORIGIN}/api/v1/cars`,
    'https://elsewhere.example/_v/aaa/x.js', `${ORIGIN}/_v/aaa/pages/car.js`].map((name) => ({ name })) };
  (await g.navigator.serviceWorker.ready).active.postMessage = (m) => sent.push(m);
  await registerServiceWorker(g);
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(sent, [{ type: 'sentry-mate:warm', files: ['/_v/aaa/pages/car.js', '/_v/aaa/fonts/a.woff2'] }]);
});

test('files that finish loading later without the worker are told to it as they come in', async () => {
  const { g } = page();
  const sent = [];
  let observer = null;
  g.location = { protocol: 'https:', origin: ORIGIN, href: `${ORIGIN}/cars/c1` };
  g.performance = { getEntriesByType: () => [] };
  g.PerformanceObserver = class { constructor(f) { this.f = f; observer = this; } observe(o) { assert.equal(o.type, 'resource'); } };
  (await g.navigator.serviceWorker.ready).active.postMessage = (m) => sent.push(m);
  await registerServiceWorker(g);
  await new Promise((r) => setTimeout(r, 0));
  const entries = (list) => ({ getEntries: () => list });
  observer.f(entries([{ name: `${ORIGIN}/_v/aaa/core/c-1.js`, workerStart: 0 }, { name: `${ORIGIN}/_v/aaa/core/c-2.js`, workerStart: 12 },
    { name: `${ORIGIN}/api/v1/cars`, workerStart: 0 }]));
  assert.deepEqual(sent.at(-1), { type: 'sentry-mate:warm', files: ['/_v/aaa/core/c-1.js'] }, 'the worker keeps the one it answered itself');
  const n = sent.length;
  observer.f(entries([{ name: `${ORIGIN}/_v/aaa/core/c-2.js`, workerStart: 9 }]));
  assert.equal(sent.length, n, 'nothing to tell');
});

test('does nothing on plain HTTP (the dev server), outside a secure context, or without service workers', async () => {
  for (const o of [{ protocol: 'http:' }, { secure: false }, { sw: false }]) {
    const { g, log } = page(o);
    assert.equal(await registerServiceWorker(g), null);
    assert.deepEqual(log, []);
  }
});

test('a failed registration resolves quietly', async () => {
  const { g } = page();
  g.navigator.serviceWorker.register = async () => { throw new Error('404'); };
  assert.equal(await registerServiceWorker(g), null);
});

test('clearing tells the worker and drops its caches, not others', async () => {
  const { g, log, names } = page();
  await clearServiceWorker(g);
  assert.deepEqual(log, [['controller', 'sentry-mate:clear']]);
  assert.deepEqual([...names], ['other']);
  await clearServiceWorker({}); // nothing there: no error
});

/* ------------------------------------------------------------------ the worker */

const SCRIPT = readFileSync(new URL('../../../crates/web/src/sw.js', import.meta.url), 'utf8');

// A response as the browser gives the worker one: same-origin ("basic"), and so are its clones.
function basic(res) {
  Object.defineProperty(res, 'type', { value: 'basic' });
  const clone = res.clone.bind(res);
  Object.defineProperty(res, 'clone', { value: () => basic(clone()) });
  return res;
}

function fakeCaches() {
  const store = new Map();
  const key = (k) => (typeof k === 'string' ? new URL(k, ORIGIN).href : k.url);
  const open = (name) => {
    if (!store.has(name)) store.set(name, new Map());
    const m = store.get(name);
    return {
      match: async (k) => (m.has(key(k)) ? m.get(key(k)).clone() : undefined),
      put: async (k, res) => { m.set(key(k), basic(new Response(await res.arrayBuffer(), res))); },
      delete: async (k) => m.delete(typeof k === 'string' ? key(k) : k.url),
      keys: async () => [...m.keys()].map((url) => ({ url })),
    };
  };
  return {
    store,
    open: async (name) => open(name),
    keys: async () => [...store.keys()],
    delete: async (name) => store.delete(name),
  };
}

function worker() {
  const listeners = {};
  const caches = fakeCaches();
  const asked = [];
  let answer = () => { throw new TypeError('offline'); };
  const fetch = async (req) => {
    const url = typeof req === 'string' ? new URL(req, ORIGIN).href : req.url;
    asked.push(url);
    return basic(await answer(url, req));
  };
  const self = {
    location: new URL(`${ORIGIN}/sw.js`),
    addEventListener: (t, f) => { listeners[t] = f; },
    skipWaiting: () => {},
    clients: { claim: async () => {} },
  };
  vm.runInNewContext(SCRIPT, { self, caches, fetch, Response, Headers, URL, Date, Promise, Map, Set, String, Math, console });
  const dispatch = async (type, init) => {
    const waits = [];
    let responded = null;
    const ev = { ...init, waitUntil: (p) => waits.push(p), respondWith: (p) => { responded = p; } };
    listeners[type](ev);
    const res = responded ? await responded : null;
    await Promise.all(waits);
    return res;
  };
  const request = (path, { method = 'GET', mode = 'cors', headers = {} } = {}) =>
    ({ url: `${ORIGIN}${path}`, method, mode, headers: new Headers(headers) });
  return {
    caches, asked,
    set answer(f) { answer = f; },
    go: (path, o) => dispatch('fetch', { request: request(path, o) }),
    message: (data) => dispatch('message', { data, ports: [] }),
  };
}

const html = (v) => new Response(`<script src="/_v/${v}/core/app.js" type="module"></script><link rel="modulepreload" href="/_v/${v}/core/x.js">`,
  { headers: { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-cache' } });
const json = (body, headers = {}) => new Response(JSON.stringify(body), { headers: { 'Content-Type': 'application/json', 'Cache-Control': 'private, no-cache', ...headers } });
const nav = { mode: 'navigate' };

test('the page: the network the first time, then the kept copy at once while the network refreshes it', async () => {
  const w = worker();
  w.answer = () => html('aaa');
  assert.match(await (await w.go('/cars/c1', nav)).text(), /_v\/aaa/);
  assert.equal(w.caches.store.get('sm-shell-v1')?.size || 0, 0, 'a first navigation is not kept (it may come from another site, signed out)');
  await w.message({ type: 'sentry-mate:warm' });
  w.answer = () => html('bbb');
  assert.match(await (await w.go('/cars/c1/drives', nav)).text(), /_v\/aaa/, 'the kept page first');
  assert.match(await (await w.go('/', nav)).text(), /_v\/bbb/, 'refreshed behind');
  w.answer = () => { throw new TypeError('offline'); };
  assert.match(await (await w.go('/settings', nav)).text(), /_v\/bbb/, 'offline: the kept page');
  // files, the API, /original and other files are no page
  w.asked.length = 0;
  w.answer = () => new Response('x', { headers: { 'Content-Type': 'text/plain' } });
  await w.go('/favicon.svg', nav);
  assert.equal(await w.go('/api/v1/events', nav), null, 'the live stream goes as it is');
  await w.go('/original', nav);
  assert.equal(w.caches.store.has('sm-shell-v1'), false, '/original drops the kept page');
});

test('files under /_v/: cache first; a new version drops the old ones', async () => {
  const w = worker();
  w.answer = (url) => (url.includes('/_v/') ? new Response(`file ${url}`) : html('aaa'));
  await w.go('/', nav);
  await w.message({ type: 'sentry-mate:warm' });
  await w.go('/_v/aaa/core/app.js');
  w.asked.length = 0;
  assert.equal(await (await w.go('/_v/aaa/core/app.js')).text(), `file ${ORIGIN}/_v/aaa/core/app.js`);
  assert.deepEqual(w.asked, [], 'no network for a kept file');
  // the server moves on: the old page answers this load (its files stay), the next load drops them
  w.answer = (url) => (url.includes('/_v/') ? new Response(`file ${url}`) : html('bbb'));
  await w.go('/', nav);
  assert.ok(w.caches.store.get('sm-assets-v1').has(`${ORIGIN}/_v/aaa/core/app.js`));
  await w.go('/', nav);
  assert.ok(!w.caches.store.get('sm-assets-v1').has(`${ORIGIN}/_v/aaa/core/app.js`));
  // warm: the page's files, fetched and kept
  await w.message({ type: 'sentry-mate:warm' });
  assert.ok(w.caches.store.get('sm-assets-v1').has(`${ORIGIN}/_v/bbb/core/x.js`));
  // and the files the page says it loaded: this version's only
  await w.message({ type: 'sentry-mate:warm', files: ['/_v/bbb/pages/car.js', '/_v/aaa/pages/old.js', '/_v/bbb/../x.js', 7, '/favicon.svg'] });
  const kept = [...w.caches.store.get('sm-assets-v1').keys()];
  assert.ok(kept.includes(`${ORIGIN}/_v/bbb/pages/car.js`));
  assert.ok(!kept.some((k) => /old\.js|favicon|\.\./.test(k)), kept.join(' '));
});

test('API reads: the network first, the last answer in memory for a stale-ok read and offline', async () => {
  const w = worker();
  const stale = { headers: { 'X-Sentry-Mate-Stale-Ok': '1' } };
  let n = 0;
  w.answer = () => json({ n: ++n });
  assert.deepEqual(await (await w.go('/api/v1/cars', stale)).json(), { n: 1 }, 'nothing kept: the network');
  const kept = await w.go('/api/v1/cars', stale);
  assert.deepEqual(await kept.json(), { n: 1 });
  assert.match(kept.headers.get('X-Sentry-Mate-Stale'), /^\d+$/);
  assert.equal(n, 1, 'answered from memory');
  assert.deepEqual(await (await w.go('/api/v1/cars')).json(), { n: 2 }, 'a plain read goes to the network');
  w.answer = () => { throw new TypeError('offline'); };
  const off = await w.go('/api/v1/cars');
  assert.deepEqual(await off.json(), { n: 2 });
  assert.ok(off.headers.has('X-Sentry-Mate-Stale'));
  await assert.rejects(w.go('/api/v1/other'), TypeError, 'nothing kept: the error');
  // never a no-store answer
  w.answer = () => json({ me: 1 }, { 'Cache-Control': 'no-store' });
  await w.go('/api/v1/me');
  w.answer = () => { throw new TypeError('offline'); };
  await assert.rejects(w.go('/api/v1/me', stale));
  // nothing on disk
  assert.ok([...w.caches.store.values()].every((m) => [...m.keys()].every((k) => !k.includes('/api/'))));
});

test('signing out, a 401, a changed pack and the clear message drop what is kept', async () => {
  const w = worker();
  const stale = { headers: { 'X-Sentry-Mate-Stale-Ok': '1' } };
  const fill = async () => {
    w.answer = (url) => (url.includes('/api/') ? json({ ok: 1 }) : url.includes('/_v/') ? new Response('f') : html('aaa'));
    await w.go('/', nav);
    await w.go('/_v/aaa/core/app.js');
    await w.go('/api/v1/cars');
  };
  const memoryHas = async () => {
    w.answer = () => { throw new TypeError('offline'); };
    return w.go('/api/v1/cars', stale).then(() => true, () => false);
  };
  await fill();
  w.answer = () => new Response(null, { status: 204 });
  assert.equal(await w.go('/api/v1/cars/c1', { method: 'PATCH' }), null, 'other changes go as they are');
  assert.ok(await memoryHas());

  w.answer = () => new Response(null, { status: 204 });
  await w.go('/api/v1/session', { method: 'DELETE' });
  assert.deepEqual([...w.caches.store.keys()], [], 'sign-out: everything');
  assert.equal(await memoryHas(), false);

  await fill();
  w.answer = () => json({ ok: 1 });
  await w.go('/api/v1/me/preferences', { method: 'PATCH' });
  assert.equal(w.caches.store.has('sm-shell-v1'), false, 'a change of preferences: the page');
  assert.ok(w.caches.store.has('sm-assets-v1'), 'but not the files');
  assert.equal(await memoryHas(), false);

  await fill();
  w.answer = () => new Response('{}', { status: 401, headers: { 'Content-Type': 'application/json' } });
  await w.go('/api/v1/fleet');
  assert.equal(w.caches.store.has('sm-shell-v1'), false, 'a 401: the page');
  assert.equal(await memoryHas(), false);

  await fill();
  await w.message({ type: 'sentry-mate:clear' });
  assert.deepEqual([...w.caches.store.keys()], []);
  assert.equal(await memoryHas(), false);
});
