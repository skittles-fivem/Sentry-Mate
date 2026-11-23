// Sentry Mate shared module: api.js (the one client for /api/v1).
// Canonical source: ui/shared/api.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// Pages never call fetch themselves. get() reads JSON; the same GET asked for twice while the first is
// still under way is sent once. list() walks cursor pages; pager() loads one page at a time for a "Show
// more" button. send() changes something and adds the CSRF header. Every call takes { signal }, and
// scope() gives a page its own client whose abort() cancels everything it started. Errors are ApiError
// { status, code, message, fields }; a 401 also tells on('unauthorized') listeners (the frame shows
// sign-in). events() follows /api/v1/events and reconnects with backoff. now() is the server's clock,
// estimated from response headers, for period presets and "5 min ago". Each answer's X-Sentry-Mate-Build goes to
// build.js (a tab open across an update loads its next page in full).

import { observeBuild, BUILD_HEADER } from './build.js';

export const API_BASE = '/api/v1';
export const CSRF_HEADER = 'X-Sentry-Mate';
// Headers the server's clock is read from: an exact one if the server sends it, else the standard Date.
export const NOW_HEADER = 'X-Sentry-Mate-Now-Ms';
export const EVENT_TYPES = Object.freeze(['now', 'fleet', 'rows', 'prefs', 'notice', 'job', 'health', 'connections', 'storage']);

const DEFAULT_MESSAGES = {
  0: 'Could not reach the server. Check the connection and try again.',
  400: 'Some of the request was not valid.',
  401: 'You are signed out. Please sign in again.',
  403: 'Your account may not do this.',
  404: 'Not found. It may have been removed, or your account may not see it.',
  409: 'Someone else changed this in the meantime. Reload and try again.',
  422: 'Some values are not valid.',
  429: 'Too many requests. Wait a moment and try again.',
  500: 'The server had a problem. Try again in a moment.',
};
const DEFAULT_CODES = { 0: 'network', 400: 'bad_request', 401: 'signed_out', 403: 'forbidden', 404: 'not_found', 409: 'conflict', 422: 'invalid', 429: 'too_many_requests', 500: 'server_error' };

/** Every failed call throws this. status 0 means the server could not be reached (code 'network' or 'timeout'). */
export class ApiError extends Error {
  constructor({ status = 0, code = null, message = null, fields = null, retryAfterMs = null, method = 'GET', path = '' } = {}) {
    const group = status >= 500 ? 500 : status;
    super(message || DEFAULT_MESSAGES[group] || `The server answered ${status}.`);
    this.name = 'ApiError';
    this.status = status;
    this.code = code || DEFAULT_CODES[group] || `http_${status}`;
    this.fields = fields || null;
    this.retryAfterMs = retryAfterMs;
    this.method = method;
    this.path = path;
  }
  get notFound() { return this.status === 404; }
  get signedOut() { return this.status === 401; }
}

/** True for the error a cancelled call rejects with. */
export const isAbort = (e) => !!e && (e.name === 'AbortError' || e.code === 20);
function abortError(signal) {
  const r = signal && signal.reason;
  if (r && isAbort(r)) return r;
  try { return new DOMException('The request was cancelled.', 'AbortError'); } catch {
    return Object.assign(new Error('The request was cancelled.'), { name: 'AbortError' });
  }
}

function parseRetryAfter(v, nowMs) {
  if (v == null || v === '') return null;
  if (/^\d+(\.\d+)?$/.test(v.trim())) return Math.round(parseFloat(v) * 1000);
  const at = Date.parse(v);
  return Number.isFinite(at) ? Math.max(0, at - nowMs) : null;
}

function encodeParam(v) {
  if (Array.isArray(v)) return v.map(encodeParam).join(',');
  if (v instanceof Date) return String(v.getTime());
  return String(v);
}

/** `cars/${id}/drives` with every ${value} URL-encoded: apiPath`cars/${car}/drives/${drive}`. */
export function apiPath(strings, ...values) {
  let out = strings[0];
  values.forEach((v, i) => { out += encodeURIComponent(String(v)) + strings[i + 1]; });
  return out;
}

/**
 * The server's clock from response headers. Each response bounds the offset between the server's clock
 * and this browser's: the server stamped it between sending and receiving. Bounds from many responses are
 * combined; the estimate is their middle. A clock jump (bounds that disagree) starts over.
 */
export function createServerClock(localNow = () => Date.now()) {
  let lo = -Infinity, hi = Infinity, offset = 0, known = false;
  return {
    /** Feed one response: the stamp (ms), its resolution (1000 for the Date header), local send and receive times. */
    observe(stampMs, resolutionMs, sentMs, receivedMs) {
      if (!Number.isFinite(stampMs)) return;
      const l = stampMs - receivedMs, h = stampMs + resolutionMs - sentMs;
      let nlo = Math.max(lo, l), nhi = Math.min(hi, h);
      if (nlo > nhi) { nlo = l; nhi = h; }
      lo = nlo; hi = nhi;
      offset = Math.round((lo + hi) / 2);
      known = true;
    },
    observeHeaders(headers, sentMs, receivedMs) {
      if (!headers || typeof headers.get !== 'function') return;
      const exact = headers.get(NOW_HEADER);
      if (exact != null && /^\d+$/.test(exact.trim())) { this.observe(+exact, 1, sentMs, receivedMs); return; }
      const date = headers.get('Date');
      const ms = date ? Date.parse(date) : NaN;
      if (Number.isFinite(ms)) this.observe(ms, 1000, sentMs, receivedMs);
    },
    /** The server's time now, in ms. */
    now: () => localNow() + offset,
    get offset() { return offset; },
    get known() { return known; },
    /** Use a fixed offset (tests, or a page told the time some other way). */
    set(serverNowMs) { offset = serverNowMs - localNow(); lo = hi = offset; known = true; },
  };
}

function isRawBody(b) {
  return (typeof Blob !== 'undefined' && b instanceof Blob)
    || (typeof ArrayBuffer !== 'undefined' && (b instanceof ArrayBuffer || ArrayBuffer.isView(b)))
    || (typeof FormData !== 'undefined' && b instanceof FormData)
    || (typeof URLSearchParams !== 'undefined' && b instanceof URLSearchParams)
    || (typeof ReadableStream !== 'undefined' && b instanceof ReadableStream);
}

function anySignal(signals) {
  const list = signals.filter(Boolean);
  if (list.length <= 1) return list[0] || null;
  if (typeof AbortSignal.any === 'function') return AbortSignal.any(list);
  const c = new AbortController();
  for (const s of list) {
    if (s.aborted) { c.abort(s.reason); break; }
    s.addEventListener('abort', () => c.abort(s.reason), { once: true });
  }
  return c.signal;
}

/** The reads that decode stored logs (they take turns on the server). The battery and efficiency reads use the
 * days' pack values the server works out ahead: they do not take turns (docs/ui-api.md). */
const HEAVY_PATH = /^\/cars\/[^/]+\/signals(\/latest)?$/;

/**
 * A client. The module's own exports use one made with the defaults; tests and special pages can make
 * their own. Options: base ('/api/v1'), fetch, EventSource, timers (setTimeout, clearTimeout), random,
 * localNow, timeoutMs (30000 for reads; writes get twice that), retry429Ms (the longest Retry-After a
 * read waits for once by itself, 10000), heavyMax (reads that decode stored logs sent at once, 2).
 */
export function createClient(options = {}) {
  const base = (options.base ?? API_BASE).replace(/\/+$/, '');
  const doFetch = options.fetch || ((...a) => globalThis.fetch(...a));
  const EventSourceImpl = options.EventSource || globalThis.EventSource;
  const setT = options.setTimeout || ((f, ms) => globalThis.setTimeout(f, ms));
  const clearT = options.clearTimeout || ((h) => globalThis.clearTimeout(h));
  const random = options.random || Math.random;
  const localNow = options.localNow || (() => Date.now());
  const readTimeoutMs = options.timeoutMs ?? 30000;
  const retry429Ms = options.retry429Ms ?? 10000;
  const clock = createServerClock(localNow);
  // Reads that decode stored logs take turns on the server: at most 4 per account running or waiting, one
  // more is 429 at once (docs/ui-api.md, "Answers that decode stored logs take turns"). A page that asks for
  // several at once (a chart per car) sends at most heavyMax of them, so two open pages fit.
  const heavyMax = options.heavyMax ?? 2;
  const heavy = { running: 0, waiting: [] };
  function heavyTurn(signal) {
    if (heavy.running < heavyMax) { heavy.running++; return Promise.resolve(); }
    return new Promise((resolve, reject) => {
      const w = { resolve, reject, signal, onAbort: null };
      w.onAbort = () => { heavy.waiting.splice(heavy.waiting.indexOf(w), 1); reject(abortError(signal)); };
      signal?.addEventListener('abort', w.onAbort, { once: true });
      heavy.waiting.push(w);
    });
  }
  function heavyDone() {
    const w = heavy.waiting.shift();
    if (!w) { heavy.running--; return; }
    w.signal?.removeEventListener('abort', w.onAbort);
    w.resolve();
  }

  const listeners = new Map(); // type -> Set(fn)
  function on(type, fn) {
    if (!listeners.has(type)) listeners.set(type, new Set());
    listeners.get(type).add(fn);
    return () => listeners.get(type)?.delete(fn);
  }
  function emit(type, detail) {
    for (const fn of [...(listeners.get(type) || [])]) { try { fn(detail); } catch (e) { console.error(e); } }
  }

  // Whether the server answers at all, for the frame's freshness note. Any HTTP answer counts.
  const net = { ok: true, lastOkMs: null, lastFailMs: null };
  function markNet(ok) {
    const was = net.ok;
    net.ok = ok;
    if (ok) net.lastOkMs = localNow(); else net.lastFailMs = localNow();
    if (was !== ok || ok === false) emit('network', { ...net });
  }

  /** The full URL path for an API path and query: url('cars', { search: 'y' }) -> '/api/v1/cars?search=y'. */
  function url(path, params) {
    let p = String(path || '');
    if (!p.startsWith(base + '/') && p !== base) p = base + '/' + p.replace(/^\/+/, '');
    const q = new URLSearchParams();
    if (params) {
      for (const k of Object.keys(params).sort()) {
        const v = params[k];
        if (v == null || v === '' || (Array.isArray(v) && !v.length)) continue;
        q.append(k, encodeParam(v));
      }
    }
    const s = q.toString();
    return s ? `${p}${p.includes('?') ? '&' : '?'}${s}` : p;
  }

  async function toError(res, method, path) {
    let body = null;
    try { body = await res.json(); } catch { /* not JSON */ }
    const e = body && typeof body === 'object' && body.error && typeof body.error === 'object' ? body.error : {};
    return new ApiError({
      status: res.status, code: e.code || null, message: e.message || null, fields: e.fields || null,
      retryAfterMs: parseRetryAfter(res.headers?.get?.('Retry-After'), clock.now()), method, path,
    });
  }

  const timeoutError = (method, path) => new ApiError({ status: 0, code: 'timeout', message: 'The server took too long to answer. Try again.', method, path });

  function sleep(ms, signal) {
    return new Promise((resolve, reject) => {
      if (signal?.aborted) { reject(abortError(signal)); return; }
      const h = setT(() => { signal?.removeEventListener('abort', onAbort); resolve(); }, ms);
      function onAbort() { clearT(h); reject(abortError(signal)); }
      signal?.addEventListener('abort', onAbort, { once: true });
    });
  }

  // One HTTP exchange; signal is the request's own (shared requests have one per group of callers).
  async function exchange(method, path, opts) {
    if (method !== 'GET' || !HEAVY_PATH.test(url(path).slice(base.length))) return exchangeNow(method, path, opts);
    if (opts.signal?.aborted) throw abortError(opts.signal);
    await heavyTurn(opts.signal);
    try { return await exchangeNow(method, path, opts); } finally { heavyDone(); }
  }
  async function exchangeNow(method, path, { params, body, headers = {}, signal, accept = 'json', timeoutMs }) {
    const href = url(path, params);
    const h = { ...headers };
    if (accept === 'json' && !('Accept' in h)) h.Accept = 'application/json';
    if (accept === 'csv' && !('Accept' in h)) h.Accept = 'text/csv';
    let payload;
    if (method !== 'GET' && method !== 'HEAD') {
      h[CSRF_HEADER] = '1';
      if (body !== undefined) {
        if (isRawBody(body)) payload = body;
        else { payload = JSON.stringify(body); if (!('Content-Type' in h)) h['Content-Type'] = 'application/json'; }
      }
    }
    // the timeout aborts the request, and tells "too slow" apart from "cancelled"
    const ctl = new AbortController();
    let timedOut = false;
    const onAbort = () => ctl.abort(signal.reason);
    if (signal) { if (signal.aborted) throw abortError(signal); signal.addEventListener('abort', onAbort, { once: true }); }
    const timer = timeoutMs > 0 ? setT(() => { timedOut = true; ctl.abort(); }, timeoutMs) : null;

    // a read is always asked again, but with the tag of the answer the browser kept: an unchanged answer comes
    // back as 304 with no body (the browser hands the kept one over); a change is never kept
    for (let attempt = 0; ; attempt++) {
      const sent = localNow();
      let res;
      try {
        res = await doFetch(href, { method, headers: h, body: payload, signal: ctl.signal, credentials: 'same-origin', cache: method === 'GET' ? 'no-cache' : 'no-store' });
      } catch (err) {
        clearT(timer);
        signal?.removeEventListener('abort', onAbort);
        if (timedOut) { markNet(false); throw timeoutError(method, path); }
        if (signal?.aborted || isAbort(err)) throw abortError(signal);
        markNet(false);
        throw new ApiError({ status: 0, code: 'network', method, path });
      }
      clock.observeHeaders(res.headers, sent, localNow());
      observeBuild(res.headers?.get?.(BUILD_HEADER));
      markNet(true);
      if (res.status === 429 && method === 'GET' && attempt === 0) {
        const wait = parseRetryAfter(res.headers?.get?.('Retry-After'), clock.now());
        if (wait != null && wait <= retry429Ms) {
          try { await sleep(wait, ctl.signal); } catch { /* aborted or timed out: handled below */ }
          if (!ctl.signal.aborted) continue;
          clearT(timer);
          signal?.removeEventListener('abort', onAbort);
          if (timedOut) throw timeoutError(method, path);
          throw abortError(signal);
        }
      }
      try {
        if (res.status === 401) emit('unauthorized', { method, path });
        if (!res.ok) throw await toError(res, method, path);
        if (res.status === 204 || method === 'HEAD') return null;
        if (accept === 'json') {
          const text = await res.text();
          if (!text) return null;
          try { return JSON.parse(text); } catch { throw new ApiError({ status: res.status, code: 'bad_response', message: 'The server sent an answer the page could not read.', method, path }); }
        }
        if (accept === 'blob') return await res.blob();
        return await res.text();
      } catch (err) {
        if (err instanceof ApiError) throw err;
        if (timedOut) throw timeoutError(method, path);
        if (signal?.aborted || isAbort(err)) throw abortError(signal);
        throw new ApiError({ status: 0, code: 'network', method, path });
      } finally {
        clearT(timer);
        signal?.removeEventListener('abort', onAbort);
      }
    }
  }

  // GETs in flight, shared by every caller asking for the same URL. Each caller can cancel its own
  // wait; the request itself is cancelled only when every caller has.
  const inflight = new Map();
  // Results kept for opts.maxAgeMs, dropped on any write. writes counts the writes, so an answer that
  // was under way during a write is not kept.
  const cache = new Map();
  let writes = 0;

  function join(entry, signal, key) {
    entry.refs++;
    let released = false;
    const release = () => {
      if (released) return;
      released = true;
      if (--entry.refs === 0 && !entry.settled) {
        entry.controller.abort();
        if (inflight.get(key) === entry) inflight.delete(key);
      }
    };
    if (!signal) return entry.promise.finally(release);
    if (signal.aborted) { release(); return Promise.reject(abortError(signal)); }
    return new Promise((resolve, reject) => {
      const onAbort = () => { release(); reject(abortError(signal)); };
      signal.addEventListener('abort', onAbort, { once: true });
      entry.promise.then(
        (v) => { signal.removeEventListener('abort', onAbort); release(); resolve(v); },
        (e) => { signal.removeEventListener('abort', onAbort); release(); reject(e); },
      );
    });
  }

  // the request headers that change the answer, in one order: names in lower case, sorted
  function headerKey(headers) {
    if (!headers) return '';
    const e = Object.entries(headers).map(([k, v]) => [String(k).toLowerCase(), String(v)]).sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
    return e.length ? ' ' + JSON.stringify(e) : '';
  }

  /**
   * GET an API path; resolves to the parsed JSON (treat it as read-only: callers asking at the same time
   * share it). Identical reads in flight (same URL, same accept, same headers) are sent once and every
   * caller gets the one answer; only GETs are shared, never a change or the event stream.
   * opts: signal, maxAgeMs (reuse a result this young), keep (keep this answer for callers that pass
   * maxAgeMs, without reusing an older one: the frame's own reads, which a page loaded at the same time asks
   * for again), timeoutMs, headers.
   */
  function get(path, params, opts = {}) {
    const href = url(path, params);
    const key = 'GET ' + href + ' ' + (opts.accept || 'json') + headerKey(opts.headers);
    if (opts.maxAgeMs > 0) {
      const hit = cache.get(key);
      if (hit && localNow() - hit.at <= opts.maxAgeMs) return Promise.resolve(hit.value);
    }
    let entry = inflight.get(key);
    if (!entry) {
      const controller = new AbortController();
      entry = { controller, refs: 0, settled: false, promise: null };
      const e = entry, w = writes;
      e.promise = exchange('GET', path, { params, signal: controller.signal, headers: opts.headers, accept: opts.accept || 'json', timeoutMs: opts.timeoutMs ?? readTimeoutMs })
        .then((value) => { if (w === writes && (opts.maxAgeMs > 0 || opts.keep || cache.has(key))) cache.set(key, { at: localNow(), value }); return value; })
        .finally(() => { e.settled = true; if (inflight.get(key) === e) inflight.delete(key); });
      e.promise.catch(() => {}); // a caller that left must not leave an unhandled rejection behind
      inflight.set(key, entry);
    }
    return join(entry, opts.signal, key);
  }

  /** Drop kept results (all, or those whose path starts with prefix). */
  function invalidate(prefix = '') {
    const p = prefix ? url(prefix) : '';
    for (const k of [...cache.keys()]) if (!p || k.startsWith('GET ' + p)) cache.delete(k);
  }

  /**
   * Change something: send('PATCH', 'me/preferences', { theme: 'dark' }). Adds the CSRF header and a JSON
   * body (a Blob, ArrayBuffer or FormData body is sent as it is). Resolves to the JSON answer or null.
   * opts: signal, headers, timeoutMs, params.
   */
  async function send(method, path, body, opts = {}) {
    const m = String(method).toUpperCase();
    if (m === 'GET') return get(path, opts.params, opts);
    // a GET started before this change must not answer a caller who asks after it
    inflight.clear();
    cache.clear();
    writes++;
    return exchange(m, path, { params: opts.params, body, headers: opts.headers, signal: opts.signal, accept: opts.accept || 'json', timeoutMs: opts.timeoutMs ?? readTimeoutMs * 2 });
  }

  /** One page of a list: { items, next, ... }. opts.after and opts.limit, or the same keys in params. */
  function page(path, params = {}, opts = {}) {
    const q = { ...params };
    if (opts.after !== undefined) q.after = opts.after;
    if (opts.limit !== undefined) q.limit = opts.limit;
    return get(path, q, opts);
  }

  /**
   * Every item of a cursor-paged list, fetched page by page as you read:
   *   for await (const d of api.list(apiPath`cars/${car}/drives`, { from_ms })) ...
   * The result also has pages() (each page as it comes) and all({ max }) (an array).
   * opts: signal, limit (page size), max (stop after this many items).
   */
  function list(path, params = {}, opts = {}) {
    const max = opts.max ?? Infinity;
    async function* pages() {
      let after = params.after ?? null;
      const seen = new Set();
      for (;;) {
        const pg = await page(path, params, { ...opts, after, limit: opts.limit ?? params.limit });
        yield pg;
        const next = pg && pg.next;
        if (!next || seen.has(next)) return; // a cursor seen before would loop forever
        seen.add(next);
        after = next;
      }
    }
    async function* items() {
      let n = 0;
      if (n >= max) return;
      for await (const pg of pages()) {
        for (const item of (pg && pg.items) || []) {
          yield item;
          if (++n >= max) return;
        }
      }
    }
    return {
      [Symbol.asyncIterator]: items,
      pages,
      async all({ max: m } = {}) {
        const out = [];
        for await (const x of items()) { out.push(x); if (m != null && out.length >= m) break; }
        return out;
      },
    };
  }

  /**
   * Page-at-a-time loading for a table with a "Show more" button:
   *   const p = api.pager(apiPath`cars/${car}/charges`, { from_ms }, { limit: 50 });
   *   await p.more(); p.items; p.done; p.last (the last page, with its summary or totals)
   * more() while a page is loading returns the same promise. reset(params) starts again.
   */
  function pager(path, params = {}, opts = {}) {
    let q = { ...params }, after = null, done = false, busy = null, gen = 0, last = null;
    let items = [];
    return {
      get items() { return items; },
      get done() { return done; },
      get loading() { return !!busy; },
      get last() { return last; },
      more() {
        if (done) return Promise.resolve([]);
        if (busy) return busy;
        const g = gen;
        busy = page(path, q, { ...opts, after: after ?? undefined, limit: opts.limit })
          .then((pg) => {
            if (g !== gen) return [];
            last = pg;
            const got = (pg && pg.items) || [];
            items = items.concat(got);
            done = !(pg && pg.next) || pg.next === after;
            after = pg && pg.next;
            return got;
          })
          .finally(() => { if (g === gen) busy = null; });
        return busy;
      },
      reset(newParams) {
        gen++;
        if (newParams) q = { ...newParams };
        after = null; done = false; busy = null; last = null; items = [];
      },
    };
  }

  /** A link for a Download CSV button: the list's path and filters with format=csv. */
  function csvUrl(path, params = {}) {
    const q = { ...params, format: 'csv' };
    delete q.after;
    delete q.limit;
    return url(path, q);
  }

  /* ---------------------------------------------------------------- server-sent events */

  const hub = { subs: new Set(), es: null, timer: null, failures: 0, state: 'closed', retryInMs: null };
  function setEventState(state, retryInMs = null) {
    hub.state = state;
    hub.retryInMs = retryInMs;
    const info = { state, retryInMs, failures: hub.failures };
    for (const s of [...hub.subs]) { try { s.onStatus?.(info); } catch (e) { console.error(e); } }
    emit('events', info);
  }
  function deliver(type, ev) {
    let data = null;
    try { data = ev.data ? JSON.parse(ev.data) : null; } catch { data = ev.data; }
    const t = type === 'message' && data && typeof data.type === 'string' ? data.type : type;
    const msg = { type: t, data, id: ev.lastEventId || null };
    for (const s of [...hub.subs]) {
      if (s.types && !s.types.includes(t)) continue;
      try { s.onEvent(msg); } catch (e) { console.error(e); }
    }
  }
  function connect() {
    hub.timer = null;
    if (!hub.subs.size) return;
    if (typeof EventSourceImpl !== 'function') { setEventState('unsupported'); return; }
    setEventState('connecting');
    let es;
    try { es = new EventSourceImpl(url('events')); } catch { scheduleReconnect(); return; }
    hub.es = es;
    for (const type of [...EVENT_TYPES, 'message']) es.addEventListener(type, (ev) => deliver(type, ev));
    es.addEventListener('open', () => { hub.failures = 0; setEventState('open'); });
    es.addEventListener('error', () => {
      if (hub.es !== es) return;
      es.close();
      hub.es = null;
      scheduleReconnect();
    });
  }
  function scheduleReconnect() {
    if (!hub.subs.size || hub.timer) return;
    // 1 s, 2 s, 4 s ... up to 60 s, each between half and all of that so many tabs do not come back at once
    const ceiling = Math.min(60000, 1000 * 2 ** hub.failures);
    const delay = Math.round(ceiling / 2 + random() * (ceiling / 2));
    hub.failures++;
    setEventState('reconnecting', delay);
    hub.timer = setT(async () => {
      hub.timer = null;
      if (!hub.subs.size) return;
      // An EventSource cannot see a 401; ask once whether the session is still there.
      try { await get('me', null, { timeoutMs: 15000 }); } catch (e) {
        if (e instanceof ApiError && e.status === 401) { setEventState('signed_out'); return; }
      }
      if (hub.subs.size && !hub.es && !hub.timer) connect();
    }, delay);
  }
  function closeHub() {
    if (hub.timer) { clearT(hub.timer); hub.timer = null; }
    if (hub.es) { hub.es.close(); hub.es = null; }
    hub.failures = 0;
    setEventState('closed');
  }

  /**
   * Follow /api/v1/events. onEvent({ type, data, id }) for every event (or only opts.types). One connection
   * is shared by every caller and closed when the last one stops. opts: types, signal, onStatus(info) with
   * info.state 'connecting' | 'open' | 'reconnecting' (info.retryInMs) | 'signed_out' | 'closed'.
   * Returns stop(). After a sign-in, call reconnectEvents().
   */
  function events(onEvent, opts = {}) {
    const sub = { onEvent, types: opts.types ? [...opts.types] : null, onStatus: opts.onStatus || null };
    if (opts.signal?.aborted) return () => {};
    hub.subs.add(sub);
    if (!hub.es && !hub.timer && hub.state !== 'signed_out') connect();
    else sub.onStatus?.({ state: hub.state, retryInMs: hub.retryInMs, failures: hub.failures });
    let stopped = false;
    const stop = () => {
      if (stopped) return;
      stopped = true;
      hub.subs.delete(sub);
      opts.signal?.removeEventListener('abort', stop);
      if (!hub.subs.size) closeHub();
    };
    opts.signal?.addEventListener('abort', stop, { once: true });
    return stop;
  }
  /** Connect again now (after signing in, or a "Reconnect" button). */
  function reconnectEvents() {
    if (hub.timer) { clearT(hub.timer); hub.timer = null; }
    if (hub.es) { hub.es.close(); hub.es = null; }
    hub.failures = 0;
    hub.state = 'closed';
    if (hub.subs.size) connect();
  }

  /* ---------------------------------------------------------------- scopes */

  /**
   * A client for one page: every call made through it is cancelled by abort() (call it when the page is
   * left). Pass a parent signal to tie it to something larger.
   */
  function scope(parentSignal = null) {
    const ctl = new AbortController();
    if (parentSignal) {
      if (parentSignal.aborted) ctl.abort(parentSignal.reason);
      else parentSignal.addEventListener('abort', () => ctl.abort(parentSignal.reason), { once: true });
    }
    const s = ctl.signal;
    const o = (opts = {}) => ({ ...opts, signal: anySignal([opts.signal, s]) });
    return {
      signal: s,
      get aborted() { return s.aborted; },
      abort: (reason) => ctl.abort(reason),
      get: (path, params, opts) => get(path, params, o(opts)),
      page: (path, params, opts) => page(path, params, o(opts)),
      list: (path, params, opts) => list(path, params, o(opts)),
      pager: (path, params, opts) => pager(path, params, o(opts)),
      send: (method, path, body, opts) => send(method, path, body, o(opts)),
      post: (path, body, opts) => send('POST', path, body, o(opts)),
      put: (path, body, opts) => send('PUT', path, body, o(opts)),
      patch: (path, body, opts) => send('PATCH', path, body, o(opts)),
      del: (path, opts) => send('DELETE', path, undefined, o(opts)),
      events: (onEvent, opts) => events(onEvent, o(opts)),
      csvUrl, url, now: clock.now, on,
    };
  }

  return {
    get, page, list, pager, send,
    post: (path, body, opts) => send('POST', path, body, opts),
    put: (path, body, opts) => send('PUT', path, body, opts),
    patch: (path, body, opts) => send('PATCH', path, body, opts),
    del: (path, opts) => send('DELETE', path, undefined, opts),
    csvUrl, url, invalidate, events, reconnectEvents, scope, on,
    /** The server's time now, in ms (falls back to this browser's clock until a response has arrived). */
    now: clock.now,
    clock,
    /** { ok, lastOkMs, lastFailMs }: whether the last call reached the server. */
    network: () => ({ ...net }),
    /** The live-events connection: { state, retryInMs, failures }. */
    eventsState: () => ({ state: hub.state, retryInMs: hub.retryInMs, failures: hub.failures }),
  };
}

const client = createClient();
/** The page's client; the named exports below are its methods. */
export default client;
export const {
  get, page, list, pager, send, post, put, patch, del, csvUrl, url, invalidate,
  events, reconnectEvents, scope, on, now, clock, network, eventsState,
} = client;
