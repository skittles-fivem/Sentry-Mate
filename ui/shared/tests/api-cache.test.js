// Tests for api.js's kept answers with a fake fetch: kept by URL and accept with their tag, asked again with
// If-None-Match (a 304 hands the kept answer over), no-store answers never kept (but for keep + maxAgeMs), everything forgotten on
// sign-out and on a 401, every kept answer asked again after a change, the size bounds, prefetch() and its
// sharing with reads under way, a similar answer for a period that ends now, and the stale tracker (kept
// answers at once, the worker's stale answers, changed or not).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createClient, STALE_OK_HEADER, STALE_HEADER } from '../api.js';
import { linkState, prefetchApi } from '../prefetch.js';

const tick = () => new Promise((r) => setImmediate(r));
const settle = async () => { for (let i = 0; i < 20; i++) await tick(); };
function deferred() {
  let resolve;
  const promise = new Promise((a) => { resolve = a; });
  return { promise, resolve };
}

// A fake server: answers[path](init, n) -> { status, body, etag, cacheControl, headers }; a request with the
// answer's own tag in If-None-Match gets 304. Every call is kept in calls.
function server(answers) {
  const calls = [];
  const fn = async (url, init) => {
    const u = new URL(url, 'http://x');
    calls.push({ url, path: u.pathname.replace('/api/v1/', ''), init });
    const a = (answers[u.pathname.replace('/api/v1/', '')] || answers['*'])(init, calls.length, u);
    if (init.method === 'GET' && a.etag && init.headers['If-None-Match'] === a.etag) return new Response(null, { status: 304, headers: { ETag: a.etag } });
    const headers = { 'Content-Type': 'application/json', ...(a.headers || {}) };
    if (a.etag) headers.ETag = a.etag;
    if (a.cacheControl) headers['Cache-Control'] = a.cacheControl;
    return new Response(a.body === undefined ? null : JSON.stringify(a.body), { status: a.status || 200, headers });
  };
  fn.calls = calls;
  fn.gets = (path) => calls.filter((c) => c.init.method === 'GET' && (!path || c.path === path));
  return fn;
}

test('an answer is kept with its tag: the next read asks with If-None-Match, and a 304 gives the kept answer', async () => {
  let v = 1;
  const f = server({ 'cars/c1': () => ({ body: { v }, etag: `W/"v${v}"` }) });
  const api = createClient({ fetch: f });
  const a = await api.get('cars/c1');
  assert.deepEqual(a, { v: 1 });
  assert.equal(f.calls[0].init.headers['If-None-Match'], undefined, 'nothing kept yet: no tag');
  assert.equal(f.calls[0].init.cache, 'no-cache');
  const b = await api.get('cars/c1');
  assert.equal(f.calls[1].init.headers['If-None-Match'], 'W/"v1"');
  assert.equal(f.calls[1].init.cache, 'no-store', 'with its own tag the browser cache stays out of it');
  assert.equal(b, a, 'the 304 hands over the kept answer itself');
  v = 2;
  assert.deepEqual(await api.get('cars/c1'), { v: 2 }, 'a changed answer replaces the kept one');
  assert.equal(api.cacheSize().entries, 1);
  // another accept is another answer, and only JSON is kept
  f.calls.length = 0;
  await api.get('cars/c1', null, { accept: 'text' });
  assert.equal(f.calls[0].init.headers['If-None-Match'], undefined);
});

test('no-store answers (secret paths) are never kept, and drop what was kept for the path', async () => {
  let store = true;
  const f = server({
    me: () => ({ body: { name: 'Ada' }, cacheControl: 'no-store' }),
    'settings/mqtt': () => (store ? { body: { a: 1 }, etag: 'W/"m"', cacheControl: 'private, no-cache' } : { body: { a: 1 }, cacheControl: 'no-store' }),
  });
  const api = createClient({ fetch: f });
  await api.get('me');
  await api.get('me');
  assert.equal(f.gets('me')[1].init.headers['If-None-Match'], undefined, 'nothing kept: no tag sent');
  assert.equal(api.cacheSize().entries, 0);
  await api.get('settings/mqtt');
  assert.equal(api.cacheSize().entries, 1);
  store = false;
  await api.get('settings/mqtt'); // the server now marks it no-store: the kept one goes
  assert.equal(api.cacheSize().entries, 0);
  const f2 = server({ me: () => ({ body: { n: 1 }, cacheControl: 'no-store' }) });
  const api2 = createClient({ fetch: f2 });
  await api2.prefetch('me');
  assert.equal(api2.cacheSize().entries, 0, 'a prefetch of a no-store path keeps nothing either');
  await api2.get('me');
  assert.equal(f2.gets('me').length, 2);
});

test('a no-store answer asked for with keep is reused by maxAgeMs only (the session’s /me), and never sent with a tag', async () => {
  const f = server({ me: () => ({ body: { name: 'Ada' }, cacheControl: 'no-store' }) });
  const api = createClient({ fetch: f });
  const a = await api.get('me', null, { keep: true });
  assert.equal(await api.get('me', null, { maxAgeMs: 60000 }), a, 'the kept one: no second request');
  assert.equal(f.gets('me').length, 1);
  await api.get('me');
  assert.equal(f.gets('me').length, 2, 'a plain read asks again');
  assert.equal(f.gets('me')[1].init.headers['If-None-Match'], undefined);
  await api.prefetch('me');
  assert.equal(f.gets('me').length, 3, 'no warm answer from a no-store one');
});

test('a change that succeeds makes every kept answer be asked again; maxAgeMs and prefetch do not reuse them', async () => {
  let local = 1000;
  const f = server({ '*': (init, n) => ({ body: { n }, etag: 'W/"same"' }) });
  const api = createClient({ fetch: f, localNow: () => local });
  await api.get('cars', null, { maxAgeMs: 60000 });
  assert.equal(f.gets().length, 1);
  await api.get('cars', null, { maxAgeMs: 60000 });
  assert.equal(f.gets().length, 1, 'young and kept for maxAgeMs: no request');
  await api.patch('cars/c1', { name: 'Comet' });
  await api.get('cars', null, { maxAgeMs: 60000 });
  assert.equal(f.gets().length, 2, 'after the change it is asked again');
  assert.equal(f.gets()[1].init.headers['If-None-Match'], 'W/"same"', 'still with its tag: an unchanged answer is a 304');
  await api.get('cars', null, { maxAgeMs: 60000 });
  assert.equal(f.gets().length, 2, 'the 304 made it fresh again');
});

test('signing out (DELETE session) and a 401 forget every kept answer', async () => {
  let signedIn = true;
  const f = server({
    session: () => ({ status: 204 }),
    '*': () => (signedIn ? { body: { ok: 1 }, etag: 'W/"a"' } : { status: 401, body: { error: { code: 'signed_out' } } }),
  });
  const api = createClient({ fetch: f });
  await api.get('cars');
  await api.get('fleet/summary');
  assert.equal(api.cacheSize().entries, 2);
  await api.del('session');
  assert.equal(api.cacheSize().entries, 0, 'signed out: nothing kept');
  await api.get('cars');
  assert.equal(f.gets('cars')[1].init.headers['If-None-Match'], undefined);
  assert.equal(api.cacheSize().entries, 1);
  signedIn = false;
  await assert.rejects(api.get('fleet/summary'), (e) => e.status === 401);
  assert.equal(api.cacheSize().entries, 0, 'a 401 forgets everything');
  // clearApiCache() does the same by hand
  signedIn = true;
  await api.get('cars');
  api.clearApiCache();
  assert.equal(api.cacheSize().entries, 0);
});

test('the kept answers are bounded by count and size; the least recently used go first', async () => {
  const f = server({ '*': (init, n, u) => ({ body: { p: u.pathname, pad: 'x'.repeat(u.pathname.endsWith('big') ? 900 : 10) }, etag: `W/"${u.pathname}"` }) });
  const api = createClient({ fetch: f, cacheEntries: 3, cacheBytes: 1000 });
  await api.get('a'); await api.get('b'); await api.get('c');
  await api.get('a'); // a used last: b is the oldest now
  await api.get('d');
  assert.equal(api.cacheSize().entries, 3);
  f.calls.length = 0;
  await api.get('b');
  assert.equal(f.calls[0].init.headers['If-None-Match'], undefined, 'b was dropped');
  await api.get('big');
  assert.ok(api.cacheSize().bytes <= 1000, 'the size bound holds');
  f.calls.length = 0;
  await api.get('a');
  assert.equal(f.calls[0].init.headers['If-None-Match'], undefined, 'older ones made room for the big one');
});

test('prefetch warms the answer the page then takes at once, shares a read under way, and asks at a low priority', async () => {
  const gate = deferred();
  const f = server({ '*': (init, n) => ({ body: { n }, etag: 'W/"x"' }) });
  const slow = async (url, init) => { await gate.promise; return f(url, init); };
  slow.calls = f.calls;
  const api = createClient({ fetch: slow });
  const p1 = api.prefetch('cars/c1/drives', { limit: 25, from_ms: 5 });
  const p2 = api.prefetch('cars/c1/drives', { from_ms: 5, limit: 25 });
  const page = api.get('cars/c1/drives', { from_ms: 5, limit: 25 });
  await tick();
  gate.resolve();
  const [a, b, c] = await Promise.all([p1, p2, page]);
  assert.equal(f.gets().length, 1, 'two prefetches and the page read: one request');
  assert.equal(f.calls[0].init.priority, 'low');
  assert.deepEqual([a, b, c].map((x) => x.n), [1, 1, 1]);
  // the page that follows takes the warm answer without asking
  const again = await api.get('cars/c1/drives', { from_ms: 5, limit: 25 });
  assert.equal(again, c);
  assert.equal(f.gets().length, 1);
  // a failed prefetch resolves to null, never throws
  const g = server({ '*': () => ({ status: 500, body: { error: {} } }) });
  assert.equal(await createClient({ fetch: g }).prefetch('x'), null);
});

test('a prefetched answer is taken for 10 s; a change or a rows event ends that', async () => {
  let local = 1_000_000;
  const f = server({ '*': (init, n) => ({ body: { n }, etag: 'W/"x"' }) });
  const api = createClient({ fetch: f, localNow: () => local });
  await api.prefetch('cars/c1/now');
  await api.get('cars/c1/now');
  assert.equal(f.gets().length, 1);
  local += 11000;
  await api.get('cars/c1/now');
  assert.equal(f.gets().length, 2, 'too old: asked again (with its tag)');
  await api.prefetch('cars/c1/now');
  await api.post('cars/c1/wake', {});
  await api.get('cars/c1/now');
  assert.equal(f.gets().length, 4, 'a change ends it');
});

test('a period that ends now: a kept answer of the same read stands in (warm, or stale), and lends its tag', async () => {
  let local = 5_000_000;
  const f = server({ '*': (init, n, u) => ({ body: { to: u.searchParams.get('to_ms') }, etag: 'W/"rows"' }) });
  const api = createClient({ fetch: f, localNow: () => local });
  const a = await api.prefetch('cars/c1/drives', { from_ms: 1000, to_ms: local, limit: 25 });
  local += 1500; // the click, a moment later: the period now ends 1.5 s later
  const b = await api.get('cars/c1/drives', { from_ms: 1000, to_ms: local, limit: 25 });
  assert.equal(b, a, 'the warm answer of the moment before is taken');
  assert.equal(f.gets().length, 1);
  // not warm any more: asked for, with the similar answer's tag; the same rows give a 304
  local += 20000;
  const c = await api.get('cars/c1/drives', { from_ms: 1000, to_ms: local, limit: 25 });
  assert.equal(f.gets().length, 2);
  assert.equal(f.gets()[1].init.headers['If-None-Match'], 'W/"rows"');
  assert.equal(c, a, 'a 304: the same rows');
  // another start, or another filter, is another read
  f.calls.length = 0;
  await api.get('cars/c1/drives', { from_ms: 2000, to_ms: local, limit: 25 });
  await api.get('cars/c1/drives', { from_ms: 1000, to_ms: local, limit: 50 });
  assert.deepEqual(f.calls.map((x) => x.init.headers['If-None-Match']), [undefined, undefined]);
  // a period that ended long ago is never stood in for
  f.calls.length = 0;
  await api.get('cars/c1/drives', { from_ms: 1000, to_ms: local - 3_600_000, limit: 25 });
  assert.equal(f.calls[0].init.headers['If-None-Match'], undefined);
});

test('the stale tracker: a kept answer at once, then the fresh one; unchanged, changed, or failed', async () => {
  let v = 1, fail = false;
  const gate = { d: deferred() };
  const f = server({ '*': () => (fail ? { status: 500, body: { error: {} } } : { body: { v }, etag: `W/"${v}"` }) });
  const slow = async (url, init) => { await gate.d.promise; return f(url, init); };
  let local = 1_000_000;
  const api = createClient({ fetch: slow, localNow: () => local });
  gate.d.resolve();
  await api.get('cars/c1/now'); // kept now
  const seen = [];
  const t = () => api.staleTracker({ onStale: () => seen.push('stale'), onFresh: (r) => seen.push(r) });

  // unchanged: the kept answer at once, the check answers 304
  gate.d = deferred();
  let tr = t();
  const a = await api.get('cars/c1/now', null, { stale: tr });
  assert.deepEqual(a, { v: 1 });
  assert.deepEqual(seen, ['stale'], 'stale until checked');
  assert.equal(tr.stale, true);
  gate.d.resolve();
  await settle();
  assert.deepEqual(seen.pop(), { changed: false, failed: false });

  // changed: the fresh answer is kept warm, so the page built again takes it at once
  local += 11000; // (a checked answer is taken at once for 10 s)
  seen.length = 0;
  v = 2;
  gate.d = deferred();
  tr = t();
  assert.deepEqual(await api.get('cars/c1/now', null, { stale: tr }), { v: 1 });
  gate.d.resolve();
  await settle();
  assert.deepEqual(seen.pop(), { changed: true, failed: false });
  const n = f.calls.length;
  assert.deepEqual(await api.get('cars/c1/now'), { v: 2 });
  assert.equal(f.calls.length, n, 'no new request: the checked answer is warm');

  // closed (the page is drawn): no more stale answers; stopped (left): no more news
  tr = t();
  tr.close();
  await api.get('cars/c1/other', null, { stale: tr });
  assert.equal(tr.stale, false);
  fail = true;
  api.invalidate();
  await api.get('cars/c1/now').catch(() => {});
  fail = false;
  await api.get('cars/c1/now');
  fail = true;
  local += 11000;
  gate.d = deferred();
  tr = t();
  await api.get('cars/c1/now', null, { stale: tr });
  gate.d.resolve();
  await settle();
  assert.deepEqual(seen.pop(), { changed: false, failed: true }, 'a check that failed says so');
});

test('the first page after a reload: the network first; the service worker\u2019s kept answer when that is slow', async () => {
  let v = 1;
  const calls = [];
  let gate = deferred();
  const fetch = async (url, init) => {
    calls.push(init);
    if (init.headers[STALE_OK_HEADER] === '1') {
      // the worker has an answer from before the reload: v0
      return new Response(JSON.stringify({ v: 0 }), { status: 200, headers: { 'Content-Type': 'application/json', ETag: 'W/"0"', [STALE_HEADER]: '42000' } });
    }
    await gate.promise;
    return new Response(JSON.stringify({ v }), { status: 200, headers: { 'Content-Type': 'application/json', ETag: `W/"${v}"` } });
  };
  const api = createClient({ fetch });
  const seen = [];
  const tr = api.staleTracker({ worker: true, onStale: () => seen.push('stale'), onFresh: (r) => seen.push(r) });
  assert.deepEqual(await api.get('cars/c1/now', null, { stale: tr }), { v: 0 }, 'a slow network: the worker\u2019s answer draws');
  assert.equal(calls[0].headers[STALE_OK_HEADER], undefined, 'the network is asked first');
  assert.equal(calls[1].headers[STALE_OK_HEADER], '1', 'then the worker');
  assert.deepEqual(seen, ['stale']);
  gate.resolve();
  await settle();
  assert.deepEqual(seen.pop(), { changed: true, failed: false });
  assert.deepEqual(await api.get('cars/c1/now'), { v: 1 }, 'the fresh answer is kept');

  // a fast network: the fresh answer within the grace, the worker never asked, nothing marked
  calls.length = 0;
  seen.length = 0;
  const tr2 = api.staleTracker({ worker: true, onStale: () => seen.push('stale'), onFresh: (r) => seen.push(r) });
  assert.deepEqual(await api.get('cars/c1/other', null, { stale: tr2 }), { v: 1 });
  assert.equal(calls.length, 1);
  assert.deepEqual(seen, []);

  // no worker (the dev server): no stale header in the answer, so it is a fresh answer and nothing follows
  const plain = [];
  const slow2 = deferred();
  const api2 = createClient({ fetch: async (url, init) => { plain.push(init); if (!init.headers[STALE_OK_HEADER]) await slow2.promise; return new Response('{"v":5}', { status: 200, headers: { 'Content-Type': 'application/json' } }); } });
  const seen2 = [];
  const tr3 = api2.staleTracker({ worker: true, onStale: () => seen2.push('stale'), onFresh: (r) => seen2.push(r) });
  assert.deepEqual(await api2.get('cars/c1/now', null, { stale: tr3 }), { v: 5 });
  slow2.resolve();
  await settle();
  assert.deepEqual(seen2, [], 'never marked stale');

  // the worker's stale answer equal to the fresh one (same tag): no change, just unmarked
  const slow3 = deferred();
  const api3 = createClient({ fetch: async (url, init) => {
    if (init.headers[STALE_OK_HEADER]) return new Response('{"v":7}', { status: 200, headers: { 'Content-Type': 'application/json', ETag: 'W/"7"', [STALE_HEADER]: '10' } });
    await slow3.promise;
    return new Response('{"v":7}', { status: 200, headers: { 'Content-Type': 'application/json', ETag: 'W/"7"' } });
  } });
  const seen3 = [];
  const tr4 = api3.staleTracker({ worker: true, onStale: () => seen3.push('stale'), onFresh: (r) => seen3.push(r) });
  await api3.get('x', null, { stale: tr4 });
  slow3.resolve();
  await settle();
  assert.deepEqual(seen3, ['stale', { changed: false, failed: false }]);
});

test('prefetchApi makes the same requests as the page calls it stands for', async () => {
  const f = server({ '*': () => ({ body: { items: [{ id: 1 }], next: null }, etag: 'W/"l"' }) });
  const api = createClient({ fetch: f });
  const p = prefetchApi(api);
  await p.page('cars', { sort: 'name' }, { limit: 200 });
  await p.list('places', { car: 'c1' }, { limit: 200 }).all();
  await p.pager('cars/c1/drives', { from_ms: 1, sort: 'start', dir: 'desc' }, { limit: 25 }).more();
  const warm = f.gets().length;
  await api.page('cars', { sort: 'name' }, { limit: 200 });
  await api.list('places', { car: 'c1' }, { limit: 200 }).all();
  const pg = api.pager('cars/c1/drives', { from_ms: 1, sort: 'start', dir: 'desc' }, { limit: 25 });
  await pg.more();
  assert.equal(f.gets().length, warm, 'every page read was warm: no new request');
});

test('linkState: only links the router follows', () => {
  const loc = { href: 'https://h/cars/c1', origin: 'https://h', pathname: '/cars/c1', search: '' };
  const a = (href, attrs = {}) => ({ href: new URL(href, loc.href).href, target: attrs.target || '', rel: attrs.rel || '', hasAttribute: (k) => k in attrs });
  assert.equal(linkState(a('/cars/c1/drives?preset=7d'), loc).page, 'drives');
  assert.deepEqual(linkState(a('/cars/c1/drives?preset=7d'), loc).period, { preset: '7d' });
  assert.equal(linkState(a('/cars/c1'), loc), null, 'the page shown now');
  assert.equal(linkState(a('https://other/cars/c1/drives'), loc), null);
  assert.equal(linkState(a('/api/v1/cars'), loc), null);
  assert.equal(linkState(a('/cars/c1/drives', { target: '_blank' }), loc), null);
  assert.equal(linkState(a('/cars/c1/drives', { download: '' }), loc), null);
  assert.equal(linkState(a('/no/such/page/here'), loc), null);
});
