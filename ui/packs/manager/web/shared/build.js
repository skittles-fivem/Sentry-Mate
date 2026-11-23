// Sentry Mate shared module: build.js (which build of the interface the server runs, and loading a page's files
// across an update).
// Canonical source: ui/shared/build.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// Every answer of the server carries X-Sentry-Mate-Build: a short hash of the UI packs it serves, which changes
// with a new image. api.js hands each API answer's value to observeBuild(); watchBuild() also asks /health when
// the tab comes back into view and every few minutes. The first value seen is the build this tab loaded.
//
// A tab open across an update still has the old modules in memory, and a page module it imports now comes from
// the new build: the two may not fit (an export that moved). So the routers load a page with loadPage():
//   - once the build changed (the last answer older than 30 s: asked first), the next page is not imported: the tab loads the target address in full (the same
//     address, so nothing is lost);
//   - a page module (or its style sheet) that fails to load is tried once more the same way, with a full load of
//     the address that skips the cache;
//   - only when that fails too does loadPage throw pageFailedError() (code page_failed): "Sentry Mate was updated
//     or a file could not load", with a Reload button (PAGE_FAILED). It is not "No answer from the server", which
//     is for requests that got no answer.

export const BUILD_HEADER = 'X-Sentry-Mate-Build';
/** What a page that could not load says: a title, a line, and the button that reloads (reloadPage). */
export const PAGE_FAILED = Object.freeze({
  code: 'page_failed',
  title: 'Sentry Mate was updated or a file could not load',
  text: 'Reload to load this page again with the current version.',
  action: 'Reload',
});

// the query parameter of a full load that skips the cache; taken off the address again as soon as the page loads
export const FRESH_PARAM = '_fresh';
const RETRY_KEY = 'sentrymate.page-retry';
const RETRY_WINDOW_MS = 60000;
const CHECK_EVERY_MS = 5 * 60000;
// a page asked for when the last answer is older than this checks the build first (at most CHECK_WAIT_MS)
const STALE_MS = 30000;
const CHECK_WAIT_MS = 1500;

let loaded = null, latest = null, seenAt = 0;

/** Feed the X-Sentry-Mate-Build value of an answer (null or '' is ignored). */
export function observeBuild(value) {
  if (typeof value !== 'string') return;
  const v = value.trim();
  if (!v) return;
  if (loaded === null) loaded = v;
  latest = v;
  seenAt = Date.now();
}

/** True once an answer named another build than the first one this tab saw. */
export const buildChanged = () => loaded !== null && latest !== null && latest !== loaded;
/** The build this tab loaded, and the one the server runs now (null until an answer said). */
export const loadedBuild = () => loaded;
export const serverBuild = () => latest;

/** Ask the server which build it runs (GET /health: no sign-in, no body). Never rejects. */
export function checkBuild({ fetch = globalThis.fetch } = {}) {
  if (typeof fetch !== 'function') return Promise.resolve(latest);
  return Promise.resolve()
    .then(() => fetch('/health', { cache: 'no-store', credentials: 'same-origin' }))
    .then((res) => { observeBuild(res?.headers?.get?.(BUILD_HEADER)); return latest; }, () => latest);
}

let watching = false;
/** Check the build when the tab comes back into view, and every few minutes while it is shown. Once per tab. */
export function watchBuild({ everyMs = CHECK_EVERY_MS } = {}) {
  const doc = globalThis.document;
  if (watching || !doc || typeof doc.addEventListener !== 'function') return;
  watching = true;
  doc.addEventListener('visibilitychange', () => { if (doc.visibilityState === 'visible') checkBuild(); });
  setInterval(() => { if (doc.visibilityState !== 'hidden') checkBuild(); }, everyMs);
}

/** The error a page that could not load shows (status 0, code page_failed); `cause` is what failed. */
export function pageFailedError(cause = null) {
  const e = new Error(PAGE_FAILED.title + '.');
  e.name = 'PageLoadError';
  e.status = 0;
  e.code = PAGE_FAILED.code;
  e.reload = true;
  if (cause) e.cause = cause;
  return e;
}
export const isPageFailed = (e) => !!e && e.code === PAGE_FAILED.code;

// the address without FRESH_PARAM and the fragment: what a retry is remembered by
function placeKey(href) {
  const u = new URL(href, globalThis.location?.href);
  u.searchParams.delete(FRESH_PARAM);
  return u.pathname + u.search;
}

function storage() {
  try { return globalThis.sessionStorage || null; } catch { return null; }
}

/** Load `href` (default: this address) in full, past the cache. The history entry is the same one. */
export function reloadPage(href = globalThis.location?.href) {
  const loc = globalThis.location;
  if (!loc || !href) return;
  const u = new URL(href, loc.href);
  u.searchParams.set(FRESH_PARAM, Date.now().toString(36));
  loc.replace(u.href);
}

const never = () => new Promise(() => {});

/**
 * Load a page's files: `load()` imports the module (and waits for its style sheet) and resolves with the module.
 * See the notes at the top: after an update the tab loads `href` in full instead (the promise then never settles,
 * the tab is going away); a failure is tried once more with a full load; a second failure throws
 * pageFailedError().
 */
export async function loadPage(load, { href = globalThis.location?.href } = {}) {
  watchBuild();
  // a tab that heard nothing from the server for a while asks first (the update may have come in the meantime)
  if (latest !== null && Date.now() - seenAt > STALE_MS) await Promise.race([checkBuild(), new Promise((r) => setTimeout(r, CHECK_WAIT_MS))]);
  if (buildChanged() && href) { reloadPage(href); return never(); }
  return Promise.resolve().then(load).then((mod) => {
    const st = storage();
    try { if (st?.getItem(RETRY_KEY)) st.removeItem(RETRY_KEY); } catch { /* no storage: nothing to forget */ }
    return mod;
  }, (err) => {
    const st = storage();
    const key = href ? placeKey(href) : null;
    let tried = true; // without storage the tab can't tell it already tried: no second load, no loop
    try {
      const prev = JSON.parse(st?.getItem(RETRY_KEY) || 'null');
      tried = !st || !key || (prev && prev.key === key && Date.now() - prev.at < RETRY_WINDOW_MS);
      if (!tried) st.setItem(RETRY_KEY, JSON.stringify({ key, at: Date.now() }));
      else st?.removeItem(RETRY_KEY);
    } catch { tried = true; }
    if (!tried) {
      console.warn('A page file did not load; loading the address in full once more.', err);
      reloadPage(href);
      return never();
    }
    throw pageFailedError(err);
  });
}

// a full load past the cache came with FRESH_PARAM: take it off the address before the router reads it
(function dropFreshParam() {
  const loc = globalThis.location, hist = globalThis.history;
  if (!loc || !hist || typeof hist.replaceState !== 'function') return;
  try {
    const u = new URL(loc.href);
    if (!u.searchParams.has(FRESH_PARAM)) return;
    u.searchParams.delete(FRESH_PARAM);
    hist.replaceState(hist.state, '', u.pathname + u.search + u.hash);
  } catch { /* an address that can't be read: leave it */ }
})();
