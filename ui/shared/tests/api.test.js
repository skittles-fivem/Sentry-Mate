// Tests for api.js with a fake fetch and a fake EventSource: URLs, CSRF header, error mapping, 401,
// retries, de-duplication and cancelling, paging, CSV links, the server clock and live events.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createClient, createServerClock, ApiError, isAbort, apiPath, CSRF_HEADER } from '../api.js';

const json = (status, body, headers = {}) => new Response(body === undefined ? null : JSON.stringify(body), {
  status, headers: { 'Content-Type': 'application/json', ...headers },
});
function fakeFetch(handler) {
  const calls = [];
  const fn = async (url, init) => { calls.push({ url, init }); return handler(url, init, calls.length); };
  fn.calls = calls;
  return fn;
}
// a fetch that only ends when it is aborted
const hanging = (url, init) => new Promise((resolve, reject) => {
  init.signal.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')), { once: true });
});
function deferred() {
  let resolve, reject;
  const promise = new Promise((a, b) => { resolve = a; reject = b; });
  return { promise, resolve, reject };
}
const tick = () => new Promise((r) => setImmediate(r));

test('URLs: base path, sorted parameters, blanks left out, lists joined, path values encoded', async () => {
  const f = fakeFetch(() => json(200, { ok: true }));
  const api = createClient({ fetch: f });
  await api.get('cars', { search: 'Model Y', state: null, sort: '', cars: ['1', '7'], limit: 50, flag: true });
  assert.equal(f.calls[0].url, '/api/v1/cars?cars=1%2C7&flag=true&limit=50&search=Model+Y');
  assert.equal(f.calls[0].init.method, 'GET');
  assert.equal(f.calls[0].init.headers.Accept, 'application/json');
  assert.equal(f.calls[0].init.headers[CSRF_HEADER], undefined, 'reads carry no CSRF header');
  assert.equal(api.url('/api/v1/me'), '/api/v1/me');
  assert.equal(api.url('/me'), '/api/v1/me');
  assert.equal(apiPath`cars/${'a/b c'}/drives/${'9007199254740993'}`, 'cars/a%2Fb%20c/drives/9007199254740993');
});

test('errors map to ApiError with status, code, message and fields', async () => {
  const answers = {
    '/api/v1/a': () => json(404, { error: { code: 'no_such_car', message: 'No such car.' } }),
    '/api/v1/b': () => json(400, { error: { code: 'bad_filter', message: 'Check the filters.', fields: { min_distance_km: 'must be a number' } } }),
    '/api/v1/c': () => new Response('<html>oops</html>', { status: 502 }),
    '/api/v1/d': () => json(409, { error: { code: 'stale_version' } }),
    '/api/v1/e': () => json(403, { error: { code: 'csrf' } }),
    '/api/v1/f': () => new Response('{not json', { status: 200 }),
    '/api/v1/g': () => json(422, { error: { code: 'invalid', fields: { radius_m: 'too large' } } }),
    '/api/v1/h': () => { throw new TypeError('Failed to fetch'); },
  };
  const api = createClient({ fetch: fakeFetch((url) => answers[url]()) });
  const err = async (path) => { try { await api.get(path); } catch (e) { return e; } assert.fail('no error'); };

  const a = await err('a');
  assert.ok(a instanceof ApiError);
  assert.deepEqual([a.status, a.code, a.message, a.fields, a.notFound], [404, 'no_such_car', 'No such car.', null, true]);
  const b = await err('b');
  assert.deepEqual([b.status, b.code, b.fields], [400, 'bad_filter', { min_distance_km: 'must be a number' }]);
  const c = await err('c');
  assert.deepEqual([c.status, c.code], [502, 'server_error']);
  assert.match(c.message, /server had a problem/);
  const d = await err('d');
  assert.deepEqual([d.status, d.code], [409, 'stale_version']);
  assert.match(d.message, /changed this/);
  const e = await err('e');
  assert.deepEqual([e.status, e.code], [403, 'csrf']);
  const f = await err('f');
  assert.deepEqual([f.status, f.code], [200, 'bad_response']);
  const g = await err('g');
  assert.deepEqual([g.status, g.code, g.fields], [422, 'invalid', { radius_m: 'too large' }]);
  const h = await err('h');
  assert.deepEqual([h.status, h.code], [0, 'network']);
  assert.equal(api.network().ok, false);
});

test('a 401 tells the frame to show sign-in', async () => {
  const api = createClient({ fetch: fakeFetch(() => json(401, { error: { code: 'signed_out' } })) });
  const seen = [];
  const off = api.on('unauthorized', (d) => seen.push(d.path));
  await assert.rejects(api.get('me'), (e) => e.status === 401 && e.signedOut);
  await assert.rejects(api.patch('me/preferences', { theme: 'dark' }), (e) => e.status === 401);
  off();
  await assert.rejects(api.get('me'));
  assert.deepEqual(seen, ['me', 'me/preferences']);
});

test('a 403 two_step_setup tells the frame this account may only set two-step sign-in up', async () => {
  const api = createClient({ fetch: fakeFetch((url) => (url.includes('csrf')
    ? json(403, { error: { code: 'csrf' } }) : json(403, { error: { code: 'two_step_setup', message: 'Set up two-step sign-in for your account first.' } }))) });
  const seen = [];
  const off = api.on('two_step_setup', (d) => seen.push(d.path));
  await assert.rejects(api.get('cars'), (e) => e.status === 403 && e.code === 'two_step_setup');
  await assert.rejects(api.patch('me/preferences', { theme: 'dark' }), (e) => e.code === 'two_step_setup');
  await assert.rejects(api.get('csrf'), (e) => e.code === 'csrf');
  off();
  await assert.rejects(api.get('cars'));
  assert.deepEqual(seen, ['cars', 'me/preferences']);
});

test('changes carry the CSRF header and a JSON body; 204 gives null', async () => {
  const f = fakeFetch((url, init) => (init.method === 'DELETE' ? new Response(null, { status: 204 }) : json(200, { version: 2 })));
  const api = createClient({ fetch: f });
  assert.deepEqual(await api.patch('me/preferences', { units: { distance: 'mi' } }), { version: 2 });
  const c = f.calls[0];
  assert.equal(c.init.method, 'PATCH');
  assert.equal(c.init.headers['X-Sentry-Mate'], '1');
  assert.equal(c.init.headers['Content-Type'], 'application/json');
  assert.equal(c.init.body, '{"units":{"distance":"mi"}}');
  assert.equal(await api.del(apiPath`cars/${'c1'}/upload-tokens/${'t9'}`), null);
  assert.equal(f.calls[1].url, '/api/v1/cars/c1/upload-tokens/t9');
  assert.equal(f.calls[1].init.headers['X-Sentry-Mate'], '1');
  assert.equal(f.calls[1].init.body, undefined);
  const chunk = new Uint8Array([1, 2, 3]);
  await api.send('patch', 'imports/i1', chunk, { headers: { 'Upload-Offset': '0' } });
  assert.equal(f.calls[2].init.body, chunk, 'binary bodies are sent as they are');
  assert.equal(f.calls[2].init.headers['Content-Type'], undefined);
  assert.equal(f.calls[2].init.headers['Upload-Offset'], '0');
  await api.post('session', { name: 'owner', password: 'x' });
  assert.equal(f.calls[3].init.method, 'POST');
});

test('a read limited by Retry-After waits once by itself; a long wait is the caller’s choice', async () => {
  const waits = [];
  const timers = { setTimeout: (fn, ms) => { waits.push(ms); return setTimeout(fn, 0); }, clearTimeout: (h) => clearTimeout(h) };
  let n = 0;
  const f = fakeFetch(() => (++n === 1 ? json(429, { error: { code: 'too_many_requests' } }, { 'Retry-After': '2' }) : json(200, { ok: n })));
  const api = createClient({ fetch: f, timeoutMs: 0, ...timers });
  assert.deepEqual(await api.get('fleet/summary'), { ok: 2 });
  assert.deepEqual(waits, [2000]);
  assert.equal(f.calls.length, 2);

  const slow = createClient({ fetch: fakeFetch(() => json(429, {}, { 'Retry-After': '120' })), timeoutMs: 0, ...timers });
  await assert.rejects(slow.get('fleet/summary'), (e) => e.status === 429 && e.retryAfterMs === 120000 && e.code === 'too_many_requests');
  const writes = fakeFetch(() => json(429, {}, { 'Retry-After': '1' }));
  await assert.rejects(createClient({ fetch: writes, timeoutMs: 0, ...timers }).post('places', {}), (e) => e.status === 429);
  assert.equal(writes.calls.length, 1, 'writes are never repeated by themselves');
});

test('the same read asked for twice at once is sent once', async () => {
  const gate = deferred();
  const f = fakeFetch(() => gate.promise.then(() => json(200, { items: [1] })));
  const api = createClient({ fetch: f });
  const a = api.get('cars', { limit: 5 });
  const b = api.get('cars', { limit: 5 });
  const c = api.get('cars', { limit: 6 });
  gate.resolve();
  const [ra, rb] = await Promise.all([a, b, c]);
  assert.equal(f.calls.length, 2);
  assert.equal(ra, rb, 'both callers share one answer');
  await api.get('cars', { limit: 5 });
  assert.equal(f.calls.length, 3, 'a finished read is not kept unless asked');
});

test('reads in flight are shared only when URL, accept and headers match; changes are never shared', async () => {
  const gate = deferred();
  const f = fakeFetch((url, init) => gate.promise.then(() => json(200, { url, accept: init.headers.Accept, lang: init.headers['X-Lang'] ?? null })));
  const api = createClient({ fetch: f });
  // three callers at once, headers given in another order and case: one request
  const same = [
    api.get('cars/c1/drives', { limit: 50 }, { headers: { 'X-Lang': 'en', 'X-Other': '1' } }),
    api.get('cars/c1/drives', { limit: 50 }, { headers: { 'x-other': '1', 'x-lang': 'en' } }),
    api.page('cars/c1/drives', {}, { limit: 50, headers: { 'X-Other': '1', 'X-Lang': 'en' } }),
  ];
  // another header value, another accept, no headers: each its own request
  const other = api.get('cars/c1/drives', { limit: 50 }, { headers: { 'X-Lang': 'de', 'X-Other': '1' } });
  const csv = api.get('cars/c1/drives', { limit: 50 }, { accept: 'csv' });
  const plain = api.get('cars/c1/drives', { limit: 50 });
  // changes at the same path at once: each sent
  const posts = [api.post('cars/c1/drives', { a: 1 }), api.post('cars/c1/drives', { a: 1 })];
  await tick();
  const gets = f.calls.filter((c) => c.init.method === 'GET');
  assert.equal(gets.length, 4, 'one shared read, plus the other header, the CSV and the plain one');
  assert.equal(f.calls.filter((c) => c.init.method === 'POST').length, 2, 'changes are never shared');
  gate.resolve();
  const answers = await Promise.all(same);
  for (const a of answers) {
    assert.equal(a.lang, 'en');
    assert.match(a.url, /\/api\/v1\/cars\/c1\/drives\?limit=50$/);
  }
  assert.equal((await other).lang, 'de');
  assert.equal(typeof (await csv), 'string', 'the CSV caller gets text, not the JSON answer');
  assert.equal((await plain).lang, null);
  await Promise.all(posts);
  // once answered, the next read is sent again (nothing is kept unless maxAgeMs asks)
  await api.get('cars/c1/drives', { limit: 50 }, { headers: { 'X-Lang': 'en', 'X-Other': '1' } });
  assert.equal(f.calls.filter((c) => c.init.method === 'GET').length, 5);
  // a caller asking after a change started does not join a read from before it
  const gate2 = deferred();
  const g = fakeFetch((url, init) => (init.method === 'GET' ? gate2.promise.then(() => json(200, { n: g.calls.length })) : json(204)));
  const api2 = createClient({ fetch: g });
  const before = api2.get('me');
  await api2.patch('me/preferences', { theme: 'dark' });
  const after = api2.get('me');
  gate2.resolve();
  await Promise.all([before, after]);
  assert.equal(g.calls.filter((c) => c.init.method === 'GET').length, 2);
});

test('cancelling: one caller leaving does not cancel the others; the last one leaving cancels the request', async () => {
  const f = fakeFetch(hanging);
  const api = createClient({ fetch: f });
  const one = new AbortController(), two = new AbortController();
  const p1 = api.get('cars/c1/series', { fields: 'soc_pct' }, { signal: one.signal });
  const p2 = api.get('cars/c1/series', { fields: 'soc_pct' }, { signal: two.signal });
  await tick();
  assert.equal(f.calls.length, 1);
  const requestSignal = f.calls[0].init.signal;
  one.abort();
  await assert.rejects(p1, (e) => isAbort(e));
  assert.equal(requestSignal.aborted, false, 'still wanted by the second caller');
  two.abort();
  await assert.rejects(p2, (e) => isAbort(e));
  assert.equal(requestSignal.aborted, true);
  const pre = new AbortController();
  pre.abort();
  await assert.rejects(api.get('cars', null, { signal: pre.signal }), (e) => isAbort(e));
});

test('a page scope cancels everything it started', async () => {
  const f = fakeFetch(hanging);
  const api = createClient({ fetch: f });
  const page = api.scope();
  const reads = [page.get('cars'), page.get('fleet/summary'), page.post('trips', { name: 'x' })];
  await tick();
  assert.equal(f.calls.length, 3);
  page.abort();
  for (const r of reads) await assert.rejects(r, (e) => isAbort(e));
  assert.ok(f.calls.every((c) => c.init.signal.aborted));
  await assert.rejects(page.get('cars'), (e) => isAbort(e), 'a left page cannot start new calls');
});

test('a read that takes too long is a timeout, not a cancel', async () => {
  const api = createClient({ fetch: fakeFetch(hanging), timeoutMs: 20 });
  await assert.rejects(api.get('system/health'), (e) => e instanceof ApiError && e.code === 'timeout' && e.status === 0);
});

test('lists: every item across cursor pages, pages one by one, and a pager for Show more', async () => {
  const pages = { '': { items: [1, 2], next: 'p2', summary: { count: 5 } }, p2: { items: [3, 4], next: 'p3' }, p3: { items: [5], next: null } };
  const f = fakeFetch((url) => json(200, pages[new URL(url, 'http://x').searchParams.get('after') || '']));
  const api = createClient({ fetch: f });
  const all = [];
  for await (const x of api.list('cars/c1/drives', { from_ms: 1 }, { limit: 2 })) all.push(x);
  assert.deepEqual(all, [1, 2, 3, 4, 5]);
  assert.deepEqual(f.calls.map((c) => c.url), ['/api/v1/cars/c1/drives?from_ms=1&limit=2', '/api/v1/cars/c1/drives?after=p2&from_ms=1&limit=2', '/api/v1/cars/c1/drives?after=p3&from_ms=1&limit=2']);
  assert.deepEqual(await api.list('cars/c1/drives', {}, { max: 3 }).all(), [1, 2, 3]);
  const seen = [];
  for await (const pg of api.list('cars/c1/drives').pages()) seen.push(pg.items.length);
  assert.deepEqual(seen, [2, 2, 1]);

  const p = api.pager('cars/c1/drives', { from_ms: 1 }, { limit: 2 });
  const first = p.more();
  assert.equal(p.more(), first, 'a second click while loading waits for the same page');
  assert.deepEqual(await first, [1, 2]);
  assert.equal(p.last.summary.count, 5);
  await p.more();
  await p.more();
  assert.deepEqual([p.items, p.done], [[1, 2, 3, 4, 5], true]);
  assert.deepEqual(await p.more(), []);
  p.reset({ from_ms: 2 });
  assert.deepEqual([p.items, p.done], [[], false]);

  const loop = createClient({ fetch: fakeFetch(() => json(200, { items: ['x'], next: 'same' })) });
  assert.deepEqual(await loop.list('x').all(), ['x', 'x'], 'a cursor that repeats stops the walk');
});

test('Download CSV links keep the filters, not the page position', () => {
  const api = createClient({ fetch: fakeFetch(() => json(200, {})) });
  assert.equal(api.csvUrl('cars/c1/drives', { from_ms: 5, to_ms: 9, after: 'p2', limit: 50, places: ['h', 'w'] }), '/api/v1/cars/c1/drives?format=csv&from_ms=5&places=h%2Cw&to_ms=9');
});

test('kept results are reused while young and dropped by any change', async () => {
  let n = 0;
  let local = 1000;
  const f = fakeFetch((url, init) => json(200, { n: ++n, method: init.method }));
  const api = createClient({ fetch: f, localNow: () => local });
  assert.equal((await api.get('me', null, { maxAgeMs: 5000 })).n, 1);
  local += 1000;
  assert.equal((await api.get('me', null, { maxAgeMs: 5000 })).n, 1);
  local += 10000;
  assert.equal((await api.get('me', null, { maxAgeMs: 5000 })).n, 2);
  await api.patch('me/preferences', { theme: 'dark' });
  assert.equal((await api.get('me', null, { maxAgeMs: 5000 })).n, 4);
  api.invalidate('me');
  assert.equal((await api.get('me', null, { maxAgeMs: 5000 })).n, 5);
});

test('keep stores an answer for later maxAgeMs callers but never reuses an old one itself', async () => {
  let n = 0;
  let local = 1000;
  const api = createClient({ fetch: fakeFetch(() => json(200, { n: ++n })), localNow: () => local });
  assert.equal((await api.get('notices')).n, 1);
  assert.equal((await api.get('notices', null, { maxAgeMs: 5000 })).n, 2, 'a plain read is not kept');
  assert.equal((await api.get('fleet/summary', null, { keep: true })).n, 3);
  local += 1000;
  assert.equal((await api.get('fleet/summary', null, { keep: true })).n, 4, 'keep itself always asks again');
  local += 1000;
  assert.equal((await api.get('fleet/summary', null, { maxAgeMs: 5000 })).n, 4, 'a page loaded with it reuses the kept answer');
  local += 10000;
  assert.equal((await api.get('fleet/summary', null, { maxAgeMs: 5000 })).n, 5, 'too old: asked again');
});

test('the server clock is read from the response headers', async () => {
  let local = 1_000_000;
  const c = createServerClock(() => local);
  // the server's exact time, stamped between send (local 1000000) and receive (1000100)
  c.observe(5_000_050, 1, 1_000_000, 1_000_100);
  assert.ok(Math.abs(c.offset - 4_000_000) <= 51);
  c.observe(5_000_060, 1, 1_000_010, 1_000_020);
  assert.ok(Math.abs(c.offset - 4_000_045) <= 6, 'a tighter answer narrows the estimate');
  local = 2_000_000;
  assert.ok(Math.abs(c.now() - 6_000_045) <= 6);
  c.observe(9_000_000_000, 1, 2_000_000, 2_000_010);
  assert.ok(Math.abs(c.offset - 8_998_000_000) <= 10, 'a clock jump starts over');

  const nowMs = Date.parse('2026-11-16T17:30:00Z');
  const api = createClient({ fetch: fakeFetch(() => json(200, {}, { Date: new Date(nowMs).toUTCString() })) });
  await api.get('me');
  assert.ok(Math.abs(api.now() - nowMs) < 2000, 'the Date header gives the time to the second');
  const exact = createClient({ fetch: fakeFetch(() => json(200, {}, { 'X-Sentry-Mate-Now-Ms': String(nowMs + 123) })) });
  await exact.get('me');
  assert.ok(Math.abs(exact.now() - (nowMs + 123)) < 200);
});

/* ------------------------------------------------------------------ live events */

class FakeEventSource {
  static all = [];
  constructor(url) {
    this.url = url;
    this.listeners = {};
    this.closed = false;
    FakeEventSource.all.push(this);
  }
  addEventListener(type, fn) { (this.listeners[type] ||= []).push(fn); }
  close() { this.closed = true; }
  fire(type, data, id = '') { for (const fn of this.listeners[type] || []) fn({ data: data === undefined ? '' : JSON.stringify(data), lastEventId: id }); }
}
function manualTimers() {
  const q = [];
  return {
    q,
    setTimeout: (fn, ms) => { const t = { fn, ms, done: false }; q.push(t); return t; },
    clearTimeout: (t) => { if (t) t.done = true; },
    async runNext() { const t = q.find((x) => !x.done); assert.ok(t, 'a timer is waiting'); t.done = true; await t.fn(); return t.ms; },
    pending: () => q.filter((x) => !x.done).map((x) => x.ms),
  };
}

test('live events: one shared connection, filtered delivery, backoff, sign-out and stop', async () => {
  FakeEventSource.all = [];
  const timers = manualTimers();
  let signedIn = true;
  const f = fakeFetch(() => (signedIn ? json(200, { id: 'a1' }) : json(401, {})));
  const api = createClient({ fetch: f, EventSource: FakeEventSource, random: () => 1, timeoutMs: 0, ...timers });
  const got = [], rowsOnly = [], states = [];
  const stopA = api.events((m) => got.push(m), { onStatus: (s) => states.push(s.state) });
  const stopB = api.events((m) => rowsOnly.push(m.type), { types: ['rows'] });
  assert.equal(FakeEventSource.all.length, 1, 'one connection for both');
  const es = FakeEventSource.all[0];
  assert.equal(es.url, '/api/v1/events');
  es.fire('open');
  es.fire('now', { car: 'c1', now: { soc_pct: { value: 80, at_ms: 1 } } }, '17');
  es.fire('rows', { car: 'c1', kinds: ['drives'] });
  es.fire('message', { type: 'notice', code: 'cert' });
  assert.deepEqual(got.map((m) => m.type), ['now', 'rows', 'notice']);
  assert.equal(got[0].data.car, 'c1');
  assert.equal(got[0].id, '17');
  assert.deepEqual(rowsOnly, ['rows']);

  // the connection drops: closed, then tried again after 1 s, 2 s, 4 s ... (random() = 1: the full step)
  es.fire('error');
  assert.ok(es.closed);
  assert.deepEqual(timers.pending(), [1000]);
  assert.equal(await timers.runNext(), 1000);
  assert.equal(FakeEventSource.all.length, 2);
  FakeEventSource.all[1].fire('error');
  assert.deepEqual(timers.pending(), [2000]);
  await timers.runNext();
  FakeEventSource.all[2].fire('open');
  FakeEventSource.all[2].fire('error');
  assert.deepEqual(timers.pending(), [1000], 'an open connection resets the backoff');

  // signed out meanwhile: the check before reconnecting sees the 401 and stops trying
  signedIn = false;
  const unauthorized = [];
  api.on('unauthorized', () => unauthorized.push(1));
  await timers.runNext();
  assert.equal(FakeEventSource.all.length, 3);
  assert.equal(api.eventsState().state, 'signed_out');
  assert.deepEqual(unauthorized, [1]);
  assert.deepEqual(timers.pending(), []);
  signedIn = true;
  api.reconnectEvents();
  assert.equal(FakeEventSource.all.length, 4);

  stopB();
  assert.equal(FakeEventSource.all[3].closed, false, 'still used by the first caller');
  stopA();
  assert.equal(FakeEventSource.all[3].closed, true, 'closed when the last caller stops');
  assert.equal(api.eventsState().state, 'closed');
  assert.ok(states.includes('open') && states.includes('reconnecting') && states.includes('signed_out'));
});

test('live events stop with the page scope', () => {
  FakeEventSource.all = [];
  const api = createClient({ fetch: fakeFetch(() => json(200, {})), EventSource: FakeEventSource });
  const page = api.scope();
  page.events(() => {});
  assert.equal(FakeEventSource.all.length, 1);
  page.abort();
  assert.equal(FakeEventSource.all[0].closed, true);
});

test('reads that decode stored logs go at most two at a time; others are not held up; a cancelled wait leaves', async () => {
  const open = [];
  const f = fakeFetch((url, init) => {
    if (!/signals/.test(url)) return json(200, { ok: url });
    const d = deferred();
    open.push({ url, d });
    init.signal.addEventListener('abort', () => d.reject(new DOMException('aborted', 'AbortError')), { once: true });
    return d.promise;
  });
  const api = createClient({ fetch: f, timeoutMs: 0 });
  const heavy = (p) => f.calls.filter((c) => c.url.includes(p)).length;
  const a = api.get('cars/c1/signals');
  const b = api.get('cars/c1/signals/latest', { search: 'BMS' });
  const c = api.get('cars/c2/signals/latest');
  const ctl = new AbortController();
  const d = api.get('cars/c2/signals', {}, { signal: ctl.signal });
  const e = api.get('cars/c3/signals');
  await tick();
  assert.equal(open.length, 2, 'two sent');
  assert.equal(heavy('/cars/c2/'), 0, 'the third and fourth wait');
  assert.deepEqual(await api.get('cars/c1/drives'), { ok: '/api/v1/cars/c1/drives' }, 'a light read goes at once');
  assert.deepEqual(await api.get('cars/c1/battery'), { ok: '/api/v1/cars/c1/battery' }, 'the battery reads do not wait');
  ctl.abort();
  await assert.rejects(d, (x) => isAbort(x));
  open[0].d.resolve(json(200, { n: 1 }));
  assert.deepEqual(await a, { n: 1 });
  await tick();
  assert.equal(open.length, 3, 'the next one in line went');
  assert.ok(open[2].url.startsWith('/api/v1/cars/c2/signals/latest'));
  open[1].d.resolve(json(200, { n: 2 }));
  open[2].d.resolve(json(200, { n: 3 }));
  assert.deepEqual(await b, { n: 2 });
  assert.deepEqual(await c, { n: 3 });
  await tick();
  assert.equal(f.calls.filter((x) => x.url === '/api/v1/cars/c2/signals').length, 0, 'the cancelled one was never sent');
  assert.equal(open.length, 4);
  open[3].d.resolve(json(200, { n: 4 }));
  assert.deepEqual(await e, { n: 4 });
});
