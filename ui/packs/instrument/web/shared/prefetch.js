// Sentry Mate shared module: prefetch.js (the next page made ready before it is opened).
// Canonical source: ui/shared/prefetch.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// A router calls watchLinks(warm) once. When a link to a page of the app is under the pointer for 50 ms, is
// touched or gets the keyboard focus, warm(s) gets the state that link leads to (as shared/state.js would
// make it), so the router can import the page's module and ask for its first reads ahead (api.prefetch):
// the page then draws at once when the link is followed. warmModules(loaders) loads the main sections'
// modules while the browser is idle after the first page (never when the person asked to save data).
// prefetchApi(api) is the client a page's prefetch(ctx) gets: get, page, list and pager that only warm the
// reads the page will make. setStale(on) puts data-stale on <html> while a page shows an answer that is
// still being checked.

import { parseUrl, tabPeriod } from './state.js';

/** The state a link leads to, or null for a link the router does not follow (or the address shown now). */
export function linkState(a, loc = globalThis.location) {
  if (!a || !a.href || !loc) return null;
  if ((a.target && a.target !== '_self') || a.hasAttribute?.('download') || /\bexternal\b/.test(a.rel || '')) return null;
  let u;
  try { u = new URL(a.href, loc.href); } catch { return null; }
  if (u.origin !== loc.origin || u.pathname.startsWith('/api/')) return null;
  if (u.pathname === loc.pathname && u.search === loc.search) return null;
  const s = parseUrl(u);
  if (s.page === 'not-found') return null;
  return { ...s, period: s.period || tabPeriod(), periodFromUrl: !!s.period, href: u.pathname + u.search };
}

/**
 * Warm the page behind a link before it is followed. warm(s) may return a promise; its errors are ignored.
 * The same address is warmed again only after againMs. Returns stop().
 */
export function watchLinks(warm, { root = globalThis.document, delayMs = 50, againMs = 5000 } = {}) {
  if (!root || typeof root.addEventListener !== 'function') return () => {};
  const last = new Map(); // href -> when it was warmed
  let pending = null, timer = null;
  const linkOf = (t) => (t && t.closest ? t.closest('a[href]') : null);
  const fire = (a) => {
    const s = linkState(a);
    if (!s) return;
    const now = Date.now();
    if (now - (last.get(s.href) ?? -Infinity) < againMs) return;
    last.set(s.href, now);
    if (last.size > 100) last.delete(last.keys().next().value);
    try { Promise.resolve(warm(s)).catch(() => {}); } catch { /* a page that cannot be warmed loads as usual */ }
  };
  const cancel = () => { clearTimeout(timer); timer = null; pending = null; };
  // the pointer rests on a link for delayMs: it is likely to be followed
  const over = (e) => {
    const a = linkOf(e.target);
    if (a === pending) return;
    cancel();
    if (!a) return;
    pending = a;
    timer = setTimeout(() => { const p = pending; cancel(); fire(p); }, delayMs);
  };
  const out = (e) => { if (pending && !pending.contains(e.relatedTarget)) cancel(); };
  const now = (e) => { const a = linkOf(e.target); if (a) fire(a); };
  const opts = { passive: true, capture: true };
  root.addEventListener('mouseover', over, opts);
  root.addEventListener('mouseout', out, opts);
  root.addEventListener('touchstart', now, opts);
  root.addEventListener('focusin', now, opts);
  return () => {
    cancel();
    root.removeEventListener('mouseover', over, opts);
    root.removeEventListener('mouseout', out, opts);
    root.removeEventListener('touchstart', now, opts);
    root.removeEventListener('focusin', now, opts);
  };
}

/**
 * Load modules one at a time while the browser is idle (each within timeoutMs), starting afterMs from now:
 * loaders are functions that import one module. Nothing when the person asked to save data.
 */
export function warmModules(loaders, { timeoutMs = 3000, afterMs = 2000 } = {}) {
  if (globalThis.navigator?.connection?.saveData) return;
  const list = [...loaders];
  const idle = typeof globalThis.requestIdleCallback === 'function'
    ? (f) => globalThis.requestIdleCallback(f, { timeout: timeoutMs })
    : (f) => setTimeout(f, 300);
  const next = () => {
    const load = list.shift();
    if (!load) return;
    Promise.resolve().then(load).catch(() => {}).finally(() => idle(next));
  };
  // not while the page just drawn may still be loading what it shows (a chart, a map coming near)
  setTimeout(() => idle(next), afterMs);
}

/**
 * A page's own style sheet (<link data-page-css="id">). Once loaded it stays in the document: on (media all)
 * while its page is shown, off (media "not all") otherwise, so coming back needs no request and applies at
 * once. on: false loads it off (a page warmed ahead). Resolves once it has loaded (or failed).
 */
export function pageSheet(id, href, { on = true } = {}) {
  const d = globalThis.document;
  let l = d.head.querySelector(`link[data-page-css="${id}"]`);
  if (!l) {
    l = d.createElement('link');
    l.rel = 'stylesheet';
    l.media = on ? 'all' : 'not all';
    l.dataset.pageCss = id;
    const done = () => { l.dataset.loaded = ''; };
    l.addEventListener('load', done, { once: true });
    l.addEventListener('error', done, { once: true });
    l.href = href;
    d.head.append(l);
  } else if (on) l.media = 'all';
  if (!on || 'loaded' in l.dataset) return Promise.resolve();
  return new Promise((resolve) => {
    l.addEventListener('load', resolve, { once: true });
    l.addEventListener('error', resolve, { once: true });
  });
}
/** Every page style sheet off but keep's (null: all off). */
export function pageSheetsOff(keep = null) {
  for (const l of globalThis.document.head.querySelectorAll('link[data-page-css]')) if (l.dataset.pageCss !== keep) l.media = 'not all';
}

/**
 * The client a page's prefetch(ctx) gets: the same calls as ctx.api, so a page warms exactly the reads its
 * mount() makes. get, page, list (its first page) and pager (its first more()) only warm and resolve to the
 * answer, or null when it failed.
 */
export function prefetchApi(api) {
  const page = (path, params = {}, opts = {}) => {
    const q = { ...params };
    if (opts.after !== undefined) q.after = opts.after;
    if (opts.limit !== undefined) q.limit = opts.limit;
    return api.prefetch(path, q, opts);
  };
  const list = (path, params = {}, opts = {}) => {
    const first = () => page(path, params, { ...opts, after: params.after ?? null, limit: opts.limit ?? params.limit });
    return {
      async all() { const pg = await first(); return (pg && pg.items) || []; },
      async* pages() { const pg = await first(); if (pg) yield pg; },
      async* [Symbol.asyncIterator]() { const pg = await first(); for (const x of (pg && pg.items) || []) yield x; },
    };
  };
  const pager = (path, params = {}, opts = {}) => ({ more: () => page(path, params, { ...opts, after: undefined, limit: opts.limit }) });
  return {
    get: (path, params, opts) => api.prefetch(path, params, opts),
    page, list, pager,
    prefetch: api.prefetch, url: api.url, csvUrl: api.csvUrl, now: api.now,
    aborted: false, signal: null,
  };
}

/** data-stale on <html> while the page shows an answer that is still being checked (each pack styles it). */
export function setStale(on) {
  const el = globalThis.document?.documentElement;
  if (!el) return;
  if (on) el.setAttribute('data-stale', '');
  else el.removeAttribute('data-stale');
}
