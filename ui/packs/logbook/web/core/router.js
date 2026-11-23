// Sentry Mate, "Logbook" UI pack: the router.
//
// Follows the URL (shared/state.js) and shows one page at a time in <main>. Every page id of the plan
// has a module in pages/<id>.js, loaded on first use with import(). For each page the router: leaves the
// old one (its unmount(), its API calls cancelled, its clean-ups run), shows the loading view, loads the
// car of the URL (404 shows "not found"), resolves the period in the car's time zone, tells the frame,
// sets the tab title, loads the page's own style sheet when it has one, and calls mount(ctx).
//
// A page module:
//   export const id = 'drives';
//   export const css = true;                       // optional: load pages/drives.css first
//   export function title(ctx) { return 'Drives'; } // optional: the tab title's first part
//   export async function mount(ctx) { ...; ctx.ready(); return { unmount() {}, update(ctx, why) {} }; }
// update() is optional: without it a new period mounts the page again. See core/README.md for ctx.

import * as state from '../shared/state.js';
import * as api from '../shared/api.js';
import { loadPage, reloadPage, isPageFailed } from '../shared/build.js';
import { h, ready, failed, unready, setTitle, cover } from '../shared/dom.js';
import { periodParams } from '../shared/time.js';
import { pageInfo } from './pages.js';
import { resolveFor } from './period.js';
import { loadingView, errorView } from './ui.js';
import { showPending, clearFlash, flash } from './flash.js';
import { onPrefs } from './prefs.js';

/** Page id -> its module, loaded when first shown. */
export const MODULES = Object.freeze({
  fleet: () => import('../pages/fleet.js'),
  car: () => import('../pages/car.js'),
  timeline: () => import('../pages/timeline.js'),
  drives: () => import('../pages/drives.js'),
  drive: () => import('../pages/drive.js'),
  trip: () => import('../pages/trip.js'),
  logbook: () => import('../pages/logbook.js'),
  charges: () => import('../pages/charges.js'),
  charge: () => import('../pages/charge.js'),
  battery: () => import('../pages/battery.js'),
  efficiency: () => import('../pages/efficiency.js'),
  statistics: () => import('../pages/statistics.js'),
  travel: () => import('../pages/travel.js'),
  software: () => import('../pages/software.js'),
  places: () => import('../pages/places.js'),
  'settings-cars': () => import('../pages/settings-cars.js'),
  'settings-display': () => import('../pages/settings-display.js'),
  'settings-integrations': () => import('../pages/settings-integrations.js'),
  'settings-maps': () => import('../pages/settings-maps.js'),
  'settings-server': () => import('../pages/settings-server.js'),
  import: () => import('../pages/import.js'),
  health: () => import('../pages/health.js'),
  help: () => import('../pages/help.js'),
  'sign-in': () => import('../pages/sign-in.js'),
  setup: () => import('../pages/setup.js'),
});
/** Screens shown without the frame (no account yet, or not signed in). */
export const AUTH_SCREENS = Object.freeze(['sign-in', 'setup']);

const main = () => document.getElementById('main');
const same = (a, b) => JSON.stringify(a ?? null) === JSON.stringify(b ?? null);
const keyOf = (s) => [s.page, s.car, s.drive, s.charge].join('|');
const browserZone = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC'; } catch { return 'UTC'; } })();

let frame = null;
let me = null;
let carsCount = null;
let current = null; // { key, id, ctx, instance, scope, cleanups, mod }
let token = 0;
let first = true;
let selfQuery = false;

/** Load a page's own style sheet once (pages/<id>.css); resolves when it applies. */
function pageCss(id) {
  const href = new URL(`../pages/${id}.css`, import.meta.url).href;
  let link = document.head.querySelector(`link[data-page-css="${id}"]`);
  const wait = (l) => new Promise((resolve, reject) => {
    l.addEventListener('load', resolve, { once: true });
    l.addEventListener('error', () => { l.remove(); reject(new Error(`${href} did not load`)); }, { once: true });
  });
  if (link) return link.sheet ? Promise.resolve() : wait(link);
  link = h('link', { rel: 'stylesheet', href, dataset: { pageCss: id } });
  const p = wait(link);
  document.head.append(link);
  return p;
}

function leave() {
  if (!current) return;
  const c = current;
  current = null;
  try { c.instance?.unmount?.(); } catch (e) { console.error(e); }
  c.scope.abort();
  for (const f of c.cleanups.splice(0)) { try { f(); } catch (e) { console.error(e); } }
}

/** After a page's first render: scroll to the part a link's #id names, when the page has it. */
function jumpToHash() {
  let target = null;
  try { const k = decodeURIComponent((window.location.hash || '').slice(1)); target = k ? document.getElementById(k) : null; } catch { target = null; }
  if (target) target.scrollIntoView({ block: 'start' });
}

/** The context a page gets. Everything a page needs from the frame goes through it. */
function makeCtx(s, my, scope, cleanups) {
  const id = s.page;
  let jumped = false;
  const info = pageInfo(id);
  const ctx = {
    id, info, state: s, query: { ...s.query }, carId: s.car, driveId: s.drive, chargeId: s.charge,
    car: null, tz: browserZone, period: null, params: {},
    api: scope, signal: scope.signal, main: main(), me,
    /** True while this page is still the one on screen. */
    alive: () => my === token,
    /** The first render with data is done: data-ready = page id. A link with #id lands on that part. */
    ready: () => { if (my === token) { ready(id); if (!jumped) { jumped = true; jumpToHash(); } } },
    /** The page could not load: its error state, and data-ready = 'error:<id>'. */
    fail: (err, { retry = () => remount() } = {}) => {
      if (my !== token || api.isAbort(err)) return;
      console.warn(`page ${id}:`, err);
      main().removeAttribute('aria-busy');
      main().replaceChildren(errorView(err, { retry }));
      failed(id);
    },
    /** Run fn when the page is left. */
    onCleanup: (fn) => { cleanups.push(fn); },
    /** The tab title: ctx.title('Costco to Home') -> 'Costco to Home · Comet · Sentry Mate'. */
    title: (...parts) => { if (my === token) setTitle(...parts, ctx.car ? ctx.car.name : null); },
    /** Change the page's own query parameters (filters); the page updates itself, the router does not. */
    setQuery: (patch, opts) => {
      if (my !== token) return; // a late timer of a page that was left must not change the next page's address
      selfQuery = true;
      try { state.setQuery(patch, opts); } finally { selfQuery = false; }
      ctx.query = { ...state.current().query };
    },
    /** A link to another page, carrying the car and the period. */
    link: (page, params) => state.linkTo(page, params),
    /** Go to another page inside the app. */
    go: (href, opts) => state.navigate(href, opts),
    /** Live events while the page is shown: ctx.events(['now'], (ev) => ...). */
    events: (types, fn) => scope.events((ev) => { if (my === token) fn(ev); }, { types }),
    /** A short note after an action (core/flash.js). */
    flash,
    /** Mark an element as covering inventory items. */
    cover,
  };
  return ctx;
}

async function mount(s) {
  const my = ++token;
  leave();
  unready();
  clearFlash();
  const id = s.page;
  const scope = api.scope();
  const cleanups = [];
  const ctx = makeCtx(s, my, scope, cleanups);
  current = { key: keyOf(s), id, ctx, instance: null, scope, cleanups, mod: null };
  const m = main();
  m.className = `wrap main page-${id}`;
  m.setAttribute('aria-busy', 'true');
  m.replaceChildren(loadingView());
  if (!first) { window.scrollTo(0, 0); }
  try {
    if (id === 'not-found') {
      frame?.update({ page: null, info: null, car: null, tz: null, period: null, carsCount });
      setTitle('Not found');
      const err = Object.assign(new Error('There is no page at this address.'), { status: 404, code: 'not_found' });
      m.replaceChildren(errorView(err));
      m.removeAttribute('aria-busy');
      failed('not-found');
      return;
    }
    const auth = AUTH_SCREENS.includes(id);
    document.documentElement.classList.toggle('is-auth', auth);
    // after a server update the tab loads the address in full; a file that fails is tried once more that way
    const modP = loadPage(async () => {
      const mod = await MODULES[id]();
      if (mod.css) await pageCss(id);
      return mod;
    });
    modP.catch(() => { /* answered where it is awaited */ });
    const carP = s.car && !auth ? scope.get(api.apiPath`cars/${s.car}`) : null;
    let car = null;
    try { car = carP ? await carP : null; } catch (err) {
      if (err && err.status === 404) state.forgetCar(s.car);
      throw err;
    }
    if (my !== token) return;
    // The car loaded, so the account sees it: it is the car looked at last (last_car).
    if (car) state.rememberCar(car.id, { sold: car.sold });
    ctx.car = car;
    ctx.tz = (car && car.time_zone) || browserZone;
    ctx.period = resolveFor(ctx.info, s, ctx.tz);
    ctx.params = ctx.period ? periodParams(ctx.period) : {};
    if (!auth) frame?.update({ page: id, info: ctx.info, car, tz: ctx.tz, period: ctx.period, carsCount });
    const mod = await modP;
    current.mod = mod;
    if (my !== token) return;
    setTitle(mod.title ? mod.title(ctx) : (ctx.info ? ctx.info.name : id), car ? car.name : null);
    m.replaceChildren();
    const inst = await mod.mount(ctx);
    if (my !== token) { try { inst?.unmount?.(); } catch { /* left */ } return; }
    current.instance = inst || null;
    m.removeAttribute('aria-busy');
    if (!first) m.focus({ preventScroll: true });
    first = false;
    showPending();
  } catch (err) {
    if (my !== token || api.isAbort(err)) return;
    first = false;
    // failed before the page was built: the frame and the tab's title are this page's, never the one before
    if (!current?.instance) {
      if (!AUTH_SCREENS.includes(id) && !ctx.car) frame?.update({ page: id, info: ctx.info, car: null, tz: ctx.tz, period: null, carsCount });
      setTitle(ctx.info ? ctx.info.name : id, ctx.car ? ctx.car.name : null);
    }
    ctx.fail(err, isPageFailed(err) ? { retry: () => reloadPage() } : undefined);
  }
}

/** Show the current page again from scratch (after a change of units, or "Try again"). */
export function remount() { mount(state.current()); }

async function onState(s, prev) {
  if (current && current.key === keyOf(s)) {
    const periodChanged = !same(s.period, prev && prev.period);
    const queryChanged = !same(s.query, prev && prev.query);
    if (queryChanged && selfQuery && !periodChanged) return; // the page changed its own filters
    if (!periodChanged && !queryChanged) return;
    const inst = current.instance;
    // A new period: the page updates itself when it can. Other filters from the address (back, forward,
    // a link) mount it again, so its controls show what the address says.
    if (inst && inst.update && !queryChanged) {
      const c = current.ctx;
      c.state = s;
      c.query = { ...s.query };
      if (periodChanged) {
        c.period = resolveFor(c.info, s, c.tz);
        c.params = c.period ? periodParams(c.period) : {};
        frame?.update({ page: c.id, info: c.info, car: c.car, tz: c.tz, period: c.period, carsCount });
      }
      unready();
      try { await inst.update(c, { period: true }); } catch (err) { c.fail(err); }
      return;
    }
  }
  mount(s);
}

/** Start following the URL: startRouter({ frame, me, carsCount }). */
export function startRouter(opts) {
  frame = opts.frame || null;
  me = opts.me || null;
  carsCount = opts.carsCount ?? null;
  state.subscribe(onState);
  state.startRouting();
  // New units, range or language: every number on the page changes, so draw it again.
  onPrefs((p, changed) => {
    if (!current || !['units', 'range', 'language'].some((k) => changed.has(k))) return;
    if (current.instance?.update) { unready(); Promise.resolve(current.instance.update(current.ctx, { prefs: true })).catch((err) => current?.ctx.fail(err)); } else remount();
  });
}
