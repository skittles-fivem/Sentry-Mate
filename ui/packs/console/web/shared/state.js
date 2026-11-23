// Sentry Mate shared module: state.js (page, car and period, carried in the URL).
// Canonical source: ui/shared/state.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// The car is in the path (/cars/{car}/drives), the period in the query (?preset=7d or ?from=<ms>&to=<ms>).
// parseUrl() and buildUrl() turn one into the other for every page id. The current state sits in a small
// store (subscribe() to hear changes). The period also stays with the browser tab (sessionStorage), so a
// menu link without one keeps it. The car a page shows when its URL names none is the person's own
// (`main_car`, else `last_car` in /me/preferences, kept with the account so every device agrees; see
// preferredCar()); localStorage keeps a copy only for a fast first paint. Storage is a convenience only:
// everything works without it. navigate() changes the URL and removes data-ready.

import { normalizePeriod, PRESETS } from './time.js';
import { unready } from './dom.js';
import * as api from './api.js';

/** Page id -> routes, the most specific first. ':car', ':drive' and ':charge' are path parameters. */
export const ROUTES = Object.freeze([
  ['fleet', '/'],
  ['car', '/cars/:car'],
  ['timeline', '/cars/:car/timeline'],
  ['drives', '/cars/:car/drives'],
  ['drive', '/cars/:car/drives/:drive'],
  ['trip', '/cars/:car/trip'],
  ['logbook', '/cars/:car/logbook'],
  ['charges', '/cars/:car/charges'],
  ['charges', '/charges'],
  ['charge', '/cars/:car/charges/:charge'],
  ['battery', '/cars/:car/battery'],
  ['efficiency', '/cars/:car/efficiency'],
  ['statistics', '/cars/:car/statistics'],
  ['statistics', '/statistics'],
  ['travel', '/travel'],
  ['software', '/cars/:car/software'],
  ['places', '/places'],
  ['settings-cars', '/settings/cars/:car'],
  ['settings-cars', '/settings/cars'],
  ['settings-display', '/settings/display'],
  ['settings-integrations', '/settings/integrations'],
  ['settings-server', '/settings/server'],
  ['import', '/settings/import'],
  ['health', '/cars/:car/health'],
  ['health', '/system'],
  ['help', '/help'],
  ['sign-in', '/sign-in'],
  ['setup', '/setup'],
].map(([page, pattern]) => Object.freeze({ page, pattern, keys: (pattern.match(/:(\w+)/g) || []).map((k) => k.slice(1)) })));

/** Every page id with a route (the frame has none of its own). */
export const PAGE_IDS = Object.freeze([...new Set(ROUTES.map((r) => r.page))]);
/** Pages that show a period, and the detail pages under them; links to them carry the chosen one. */
export const PERIOD_PAGES = Object.freeze(['car', 'timeline', 'drives', 'drive', 'trip', 'logbook', 'charges', 'charge', 'battery', 'efficiency', 'statistics', 'travel', 'software', 'health']);
/** Pages tied to one car: switching car keeps the page. Detail pages go back to their list. */
export const CAR_PAGES = Object.freeze(['car', 'timeline', 'drives', 'trip', 'logbook', 'charges', 'battery', 'efficiency', 'statistics', 'software', 'settings-cars', 'health']);
const DETAIL_PARENT = { drive: 'drives', charge: 'charges' };
const STATE_KEYS = new Set(['preset', 'from', 'to']);

function splitUrl(u) {
  if (u && typeof u === 'object' && 'pathname' in u) return { path: u.pathname, search: u.search || '', hash: u.hash || '' };
  const s = String(u ?? '/');
  const m = /^(?:[a-z][a-z0-9+.-]*:\/\/[^/?#]*)?([^?#]*)(\?[^#]*)?(#.*)?$/i.exec(s);
  return { path: m[1] || '/', search: m[2] || '', hash: m[3] || '' };
}

function matchRoute(path) {
  const segs = path.replace(/\/+$/, '').split('/').filter(Boolean);
  for (const r of ROUTES) {
    const ps = r.pattern.split('/').filter(Boolean);
    if (ps.length !== segs.length) continue;
    const params = {};
    let ok = true;
    for (let i = 0; i < ps.length; i++) {
      if (ps[i].startsWith(':')) {
        let v;
        try { v = decodeURIComponent(segs[i]); } catch { ok = false; break; }
        if (!v) { ok = false; break; }
        params[ps[i].slice(1)] = v;
      } else if (ps[i] !== segs[i]) { ok = false; break; }
    }
    if (ok) return { page: r.page, params };
  }
  return null;
}

/** The period in a query: { preset } or { from, to } (ms), or null when there is none or it is not valid. */
export function parsePeriod(search) {
  const q = search instanceof URLSearchParams ? search : new URLSearchParams(search || '');
  const int = (v) => (v != null && /^-?\d{1,16}$/.test(v) ? Number(v) : null);
  const from = int(q.get('from')), to = int(q.get('to'));
  if (from != null || to != null) return normalizePeriod({ from, to });
  const p = q.get('preset');
  return p && PRESETS.includes(p) ? { preset: p } : null;
}

/** A period as query parameters: { preset: '7d' } or { from: '…', to: '…' }. */
export function periodQuery(period) {
  const p = normalizePeriod(period);
  if (!p) return {};
  if (p.preset) return { preset: p.preset };
  const out = {};
  if (p.from != null) out.from = String(Math.round(p.from));
  if (p.to != null) out.to = String(Math.round(p.to));
  return out;
}

/**
 * Read a URL (a string, or a Location / URL object): { page, car, drive, charge, period, query, path }.
 * page is 'not-found' for a path no route matches. query holds the other parameters as text (cars,
 * page filters); period is null when the URL has none.
 */
export function parseUrl(u) {
  const { path, search } = splitUrl(u);
  const q = new URLSearchParams(search);
  const m = matchRoute(path);
  const query = {};
  for (const [k, v] of q) if (!STATE_KEYS.has(k)) query[k] = v;
  return {
    page: m ? m.page : 'not-found',
    car: m?.params.car ?? null,
    drive: m?.params.drive ?? null,
    charge: m?.params.charge ?? null,
    period: parsePeriod(q),
    query,
    path,
  };
}

/**
 * The URL for a page: buildUrl('drive', { car: 'c1', drive: '9007199254740993', period: { preset: '7d' } })
 * -> '/cars/c1/drives/9007199254740993?preset=7d'. A page that can be shown with or without a car
 * (charges, statistics, settings-cars, health) uses the car form when a car is given. query adds other
 * parameters (null and '' are left out). Throws for an unknown page or a missing path parameter.
 */
export function buildUrl(page, { car = null, drive = null, charge = null, period = null, query = null } = {}) {
  const have = { car, drive, charge };
  const candidates = ROUTES.filter((r) => r.page === page);
  if (!candidates.length) throw new RangeError(`unknown page: ${page}`);
  const route = candidates.find((r) => r.keys.every((k) => have[k] != null && have[k] !== '') && (r.keys.length || !car || candidates.length === 1))
    || candidates.find((r) => r.keys.every((k) => have[k] != null && have[k] !== ''));
  if (!route) throw new RangeError(`page ${page} needs ${candidates[0].keys.join(', ')}`);
  const path = route.pattern.replace(/:(\w+)/g, (_, k) => encodeURIComponent(String(have[k])));
  const q = new URLSearchParams(periodQuery(period));
  if (query) for (const k of Object.keys(query)) {
    const v = query[k];
    if (v == null || v === '' || STATE_KEYS.has(k)) continue;
    q.set(k, Array.isArray(v) ? v.join(',') : String(v));
  }
  const s = q.toString();
  return s ? `${path}?${s}` : path;
}

/* ------------------------------------------------------------------ a small store */

/** { get(), set(patch), subscribe(fn(state, prev)) -> unsubscribe }. set() notifies only on a change. */
export function createStore(initial = {}) {
  let state = Object.freeze({ ...initial });
  const subs = new Set();
  return {
    get: () => state,
    set(patch) {
      const next = { ...state, ...(typeof patch === 'function' ? patch(state) : patch) };
      if (Object.keys(next).every((k) => sameValue(next[k], state[k])) && Object.keys(state).every((k) => k in next)) return state;
      const prev = state;
      state = Object.freeze(next);
      for (const f of [...subs]) { try { f(state, prev); } catch (e) { console.error(e); } }
      return state;
    },
    subscribe(f) { subs.add(f); return () => subs.delete(f); },
  };
}
function sameValue(a, b) {
  if (a === b) return true;
  if (!a || !b || typeof a !== 'object' || typeof b !== 'object') return false;
  const ka = Object.keys(a), kb = Object.keys(b);
  return ka.length === kb.length && ka.every((k) => sameValue(a[k], b[k]));
}

/* ------------------------------------------------------------------ storage (conveniences only) */

const KEY_PERIOD = 'sentrymate.period';
const KEY_CAR = 'sentrymate.last-car';
function readStore(area, key) {
  try { const v = globalThis[area]?.getItem(key); return v == null ? null : JSON.parse(v); } catch { return null; }
}
function writeStore(area, key, value) {
  try {
    if (value == null) globalThis[area]?.removeItem(key);
    else globalThis[area]?.setItem(key, JSON.stringify(value));
  } catch { /* storage blocked: fine */ }
}
/** The period this tab chose last, or null. */
export const tabPeriod = () => normalizePeriod(readStore('sessionStorage', KEY_PERIOD));
/* ------------------------------------------------------------------ the person's car */

const KEY_CAR_PREFS = 'sentrymate.car-prefs';
let carMem = null; // { main_car, last_car, pinned_cars, car_order } from the server, once known
let saveTimer = null, pendingCar = null;
const memory = { delayMs: 1500, save: (id) => api.patch('me/preferences', { last_car: id }) };

/**
 * Tell this module the person's preferences (the /me/preferences object, on load and on every `prefs`
 * event). They are the truth; the local copy follows them.
 */
export function setCarPrefs(prefs) {
  if (!prefs || typeof prefs !== 'object') return;
  const pick = (k) => (typeof prefs[k] === 'string' && prefs[k] ? prefs[k] : null);
  const list = (k) => (Array.isArray(prefs[k]) ? prefs[k].filter((c) => typeof c === 'string') : []);
  carMem = { main_car: pick('main_car'), last_car: pick('last_car'), pinned_cars: list('pinned_cars'), car_order: list('car_order') };
  writeStore('localStorage', KEY_CAR_PREFS, carMem);
  if (carMem.last_car && pendingCar == null) writeStore('localStorage', KEY_CAR, carMem.last_car);
}
/** The car choices known now: { main_car, last_car, pinned_cars, car_order } (the server's, else the local copy). */
export function carPrefs() {
  if (carMem) return carMem;
  const c = readStore('localStorage', KEY_CAR_PREFS);
  const local = lastOpened();
  return {
    main_car: typeof c?.main_car === 'string' ? c.main_car : null,
    last_car: local || (typeof c?.last_car === 'string' ? c.last_car : null),
    pinned_cars: Array.isArray(c?.pinned_cars) ? c.pinned_cars : [],
    car_order: Array.isArray(c?.car_order) ? c.car_order : [],
  };
}

/**
 * The car a page shows when its URL names none, from the cars the account sees (/cars items, with `sold`):
 * the main car if set and seen; else the last car opened if seen and not sold; else the first pinned car
 * not sold, then the first of the car order not sold; else the first car not sold; else the first car.
 * Returns its id, or null when there are no cars.
 */
export function preferredCar(cars, prefs = carPrefs()) {
  const list = (cars || []).filter((c) => c && c.id != null);
  const byId = new Map(list.map((c) => [String(c.id), c]));
  const p = prefs || {};
  const seen = (id) => (id != null && byId.has(String(id)) ? byId.get(String(id)) : null);
  const main = seen(p.main_car);
  if (main) return main.id;
  const last = seen(p.last_car);
  if (last && !isSold(last)) return last.id;
  for (const id of [...(p.pinned_cars || []), ...(p.car_order || [])]) {
    const c = seen(id);
    if (c && !isSold(c)) return c.id;
  }
  return (list.find((c) => !isSold(c)) || list[0] || {}).id ?? null;
}

const lastOpened = () => { const c = readStore('localStorage', KEY_CAR); return typeof c === 'string' && c ? c : null; };
/**
 * The car to use before the car list is at hand (links, a first paint): the main car, else the car opened
 * last, or null. Pages that have the car list use preferredCar(cars), which also checks sold and visible.
 */
export const lastCar = () => carPrefs().main_car || lastOpened();

/**
 * The person picked a car (car switcher, a page's car picker, a car page that loaded): remember it as
 * `last_car`, here at once and on the server a moment later (only the newest of quick picks is sent). Call it
 * only for a car the account sees (after it loaded). A sold car counts only when picked explicitly:
 * rememberCar(id, { sold: true, explicit: false }) does nothing.
 */
export function rememberCar(id, { sold = false, explicit = true } = {}) {
  if (!id || (sold && !explicit)) return;
  id = String(id);
  writeStore('localStorage', KEY_CAR, id);
  if (carMem && carMem.last_car === id && pendingCar == null) return;
  pendingCar = id;
  if (saveTimer) clearTimeout(saveTimer);
  saveTimer = setTimeout(flushCar, memory.delayMs);
}
/** Send a waiting rememberCar() now (tests, leaving the page). */
export async function flushCar() {
  if (saveTimer) { clearTimeout(saveTimer); saveTimer = null; }
  const id = pendingCar;
  if (id == null) return;
  try {
    const saved = await memory.save(id);
    if (pendingCar === id) pendingCar = null;
    if (saved && typeof saved === 'object' && 'last_car' in saved) setCarPrefs(saved);
    else if (carMem) carMem = { ...carMem, last_car: id };
  } catch {
    if (pendingCar === id) pendingCar = null; // the car is gone or the network is: the local copy stays
  }
}
/** For tests: how a pick is saved and after how long. */
export function configureCarMemory({ save, delayMs } = {}) {
  if (save) memory.save = save;
  if (delayMs != null) memory.delayMs = delayMs;
  carMem = null; pendingCar = null;
  if (saveTimer) { clearTimeout(saveTimer); saveTimer = null; }
}
/** Forget the last car here (it was not found, or the account may not see it). */
export function forgetCar(id) {
  if (!id || lastOpened() === String(id)) writeStore('localStorage', KEY_CAR, null);
  const c = readStore('localStorage', KEY_CAR_PREFS);
  if (c && id && (c.main_car === String(id) || c.last_car === String(id))) {
    writeStore('localStorage', KEY_CAR_PREFS, { ...c, main_car: c.main_car === String(id) ? null : c.main_car, last_car: c.last_car === String(id) ? null : c.last_car });
  }
  if (carMem && id) carMem = { ...carMem, main_car: carMem.main_car === String(id) ? null : carMem.main_car, last_car: carMem.last_car === String(id) ? null : carMem.last_car };
}

/* ------------------------------------------------------------------ the app's state and navigation */

/**
 * The current state: { page, car, drive, charge, period, periodFromUrl, query, path }. period is the
 * URL's, else the tab's last choice, else null (the page then uses its own default).
 */
export const store = createStore({ page: null, car: null, drive: null, charge: null, period: null, periodFromUrl: false, query: {}, path: '/' });
export const current = () => store.get();
export const subscribe = (f) => store.subscribe(f);

function stateFromUrl(u) {
  const s = parseUrl(u);
  const fromUrl = !!s.period;
  const period = s.period || tabPeriod();
  if (fromUrl) writeStore('sessionStorage', KEY_PERIOD, s.period);
  if (s.car) writeStore('localStorage', KEY_CAR, s.car); // the fast copy; the router calls rememberCar() once the car loaded
  return { ...s, period, periodFromUrl: fromUrl };
}

/**
 * True when the URL shows what the store already shows (the same page, ids, period and query): it differs
 * at most in its #hash, so going there is no new page: the page stays (and stays ready), only the hash moves.
 */
export function sameRoute(u) {
  const s = parseUrl(u ?? '/');
  const next = { ...s, period: s.period || tabPeriod(), periodFromUrl: !!s.period };
  const now = store.get();
  return now.page != null && Object.keys(next).every((k) => sameValue(next[k], now[k]));
}

/** Scroll to the element the address's #hash names (when it is shown); pages that keep it in a closed tab open it on hashchange. */
function landOnHash(w = globalThis.window) {
  let id = '';
  try { id = decodeURIComponent((w?.location?.hash || '').slice(1)); } catch { return; }
  const t = id ? w.document?.getElementById(id) : null;
  if (t && !t.closest('[hidden]')) t.scrollIntoView({ block: 'start' });
}

/** Read the browser's address into the store (on start and after back / forward). */
export function syncFromLocation(u = globalThis.location) {
  return store.set(stateFromUrl(u ?? '/'));
}

const browserHistory = () => (globalThis.history && typeof globalThis.history.pushState === 'function' ? globalThis.history : null);

/**
 * Go to a URL inside the app: changes the address (push, or replace with opts.replace), removes
 * data-ready, and updates the store, which tells subscribers (the frame's router). Returns the state.
 * A URL that differs from the shown one only in its #hash is no new page: data-ready stays, the browser
 * moves to the hash (hashchange fires, so a page can open the part holding it) and the store is unchanged.
 */
export function navigate(href, { replace = false } = {}) {
  const h = browserHistory();
  if (sameRoute(href)) {
    const loc = globalThis.location;
    const hash = splitUrl(href).hash;
    if (loc && typeof loc.replace === 'function') {
      if (hash && hash !== loc.hash) { if (replace) loc.replace(hash); else loc.hash = hash; }
      else if (!hash && loc.hash && h) { if (replace) h.replaceState(null, '', href); else h.pushState(null, '', href); }
    } else if (h) {
      if (replace) h.replaceState(null, '', href); else h.pushState(null, '', href);
    }
    return store.get();
  }
  unready();
  if (h) {
    if (replace) h.replaceState(null, '', href); else h.pushState(null, '', href);
  }
  return store.set(stateFromUrl(href));
}

/**
 * The link to a page, carrying the current car (for car pages) and the chosen period (for period pages):
 * linkTo('drives') on /cars/c1/timeline?preset=7d -> '/cars/c1/drives?preset=7d'. params override.
 * A car page without a car to carry uses the last car; with no car known it links to Fleet.
 */
export function linkTo(page, params = {}) {
  const s = store.get();
  const car = params.car !== undefined ? params.car : (s.car ?? lastCar());
  const period = params.period !== undefined ? params.period : PERIOD_PAGES.includes(page) ? s.period : null;
  const needsCar = ROUTES.filter((r) => r.page === page).every((r) => r.keys.includes('car'));
  if (needsCar && !car) return buildUrl('fleet');
  return buildUrl(page, { ...params, car: CAR_PAGES.includes(page) || needsCar ? car : params.car ?? null, period });
}

/**
 * Where the car switcher goes for another car: the same page for that car. A drive or charge belongs to
 * one car, so those go to the new car's list; pages without a car go to the car's overview.
 */
export function linkForCar(car, s = store.get()) {
  const page = DETAIL_PARENT[s.page] || (CAR_PAGES.includes(s.page) ? s.page : 'car');
  const period = PERIOD_PAGES.includes(page) ? s.period : null;
  const query = s.page === page ? s.query : null;
  return buildUrl(page, { car, period, query });
}

/**
 * Choose a period: puts it in the URL (replacing the entry unless opts.push), in the tab's memory and in
 * the store. null clears it (the page's default applies again).
 */
export function setPeriod(period, { push = false } = {}) {
  const p = period == null ? null : normalizePeriod(period);
  if (period != null && !p) throw new RangeError('not a period');
  writeStore('sessionStorage', KEY_PERIOD, p);
  const s = store.get();
  const page = s.page && s.page !== 'not-found' ? s.page : null;
  if (!page) { store.set({ period: p, periodFromUrl: false }); return store.get(); }
  const href = buildUrl(page, { car: s.car, drive: s.drive, charge: s.charge, period: p, query: s.query });
  const h = browserHistory();
  if (h) { if (push) h.pushState(null, '', href); else h.replaceState(null, '', href); }
  return store.set({ period: p, periodFromUrl: !!p });
}

/**
 * Change page filters in the query (for example { min_distance_km: 10 }); null removes one. Replaces the
 * history entry unless opts.push. Does not count as a navigation (data-ready stays).
 */
export function setQuery(patch, { push = false } = {}) {
  const s = store.get();
  const query = { ...s.query };
  for (const [k, v] of Object.entries(patch || {})) {
    if (STATE_KEYS.has(k)) continue;
    if (v == null || v === '') delete query[k]; else query[k] = Array.isArray(v) ? v.join(',') : String(v);
  }
  if (s.page && s.page !== 'not-found') {
    const href = buildUrl(s.page, { car: s.car, drive: s.drive, charge: s.charge, period: s.periodFromUrl ? s.period : null, query });
    const h = browserHistory();
    if (h) { if (push) h.pushState(null, '', href); else h.replaceState(null, '', href); }
  }
  return store.set({ query });
}

/**
 * Start following the browser: reads the address now, listens to back / forward (a step that only moves
 * the #hash on the same page keeps the page and its data-ready, and scrolls to the hash), and turns clicks on
 * same-origin links inside root into navigate() (links with target, download, rel="external" or a
 * modifier key behave as usual; so do links outside the app's routes and /api/ links). Returns stop().
 */
export function startRouting({ root = globalThis.document } = {}) {
  syncFromLocation();
  const w = globalThis.window;
  if (!w || !root) return () => {};
  // back / forward, or an in-page #anchor (the browser fires popstate for it too): only another page re-routes
  const onPop = () => { if (sameRoute(w.location)) { landOnHash(w); return; } unready(); syncFromLocation(); };
  const onClick = (e) => {
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    const a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
    if (!a || (a.target && a.target !== '_self') || a.hasAttribute('download') || /\bexternal\b/.test(a.rel || '')) return;
    const u = new URL(a.href, w.location.href);
    if (u.origin !== w.location.origin || u.pathname.startsWith('/api/')) return;
    if (parseUrl(u).page === 'not-found') return;
    if (u.pathname === w.location.pathname && u.search === w.location.search && u.hash) return; // in-page anchor
    e.preventDefault();
    navigate(u.pathname + u.search + u.hash);
  };
  w.addEventListener('popstate', onPop);
  root.addEventListener('click', onClick);
  return () => { w.removeEventListener('popstate', onPop); root.removeEventListener('click', onClick); };
}

/**
 * Sold cars (`sold: true` on a /cars item): their history stays browsable, but car pickers, the car switcher and
 * fleet cards leave them out by default and show them behind a "Sold cars (n)" toggle.
 * splitSold(items) -> { active: [...], sold: [...] }, each in the order given.
 */
export function isSold(car) { return !!(car && car.sold); }
export function splitSold(items = []) {
  const active = [], sold = [];
  for (const c of items || []) (isSold(c) ? sold : active).push(c);
  return { active, sold };
}
