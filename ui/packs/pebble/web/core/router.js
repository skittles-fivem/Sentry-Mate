// Sentry Mate, "Pebble" UI pack: the router.
//
// Follows the address (shared/state.js) and shows one page at a time in <main>, the bento grid. Every page id
// has a module in pages/<id>.js, loaded on first use. For each page the router: leaves the old one (its
// unmount(), its API calls cancelled, its clean-ups run), shows grey placeholder tiles of the page's shape,
// loads the car of the address (a 404 shows "nothing here"), resolves the period in the car's time zone, tells
// the frame, sets the tab title, loads the page's own style sheet when it has one, and calls mount(ctx).
// The module and its style sheet load through shared/build.js's loadPage: after a server update the tab loads the
// address in full instead, and a file that fails is tried once more that way before the page says page_failed.
//
// A page module (core/README.md has the whole of ctx):
//   export const id = 'drives';
//   export const css = true;                        // optional: load /pages/drives.css first
//   export const shape = [[2, 'h2'], [1, 'h1']];    // optional: the placeholder tiles while it loads
//   export function title(ctx) { return 'Drives'; }  // optional: the tab title's first part
//   export async function mount(ctx) { ...; ctx.ready(); return { unmount() {}, update(ctx, why) {} }; }
// update() is optional: without it a new period mounts the page again.

import * as state from '../shared/state.js';
import * as api from '../shared/api.js';
import { loadPage, reloadPage, isPageFailed } from '../shared/build.js';
import { h, ready, failed, unready, setTitle, cover } from '../shared/dom.js';
import { periodParams } from '../shared/time.js';
import { pageInfo, navId } from './pages.js';
import { resolveFor } from './period.js';
import { loadingTiles, errorTile } from './states.js';
import { showPending, clearToasts, toast } from './toast.js';
import { onPrefs } from './prefs.js';
import { scrollToTop } from './display.js';

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
  'settings-server': () => import('../pages/settings-server.js'),
  import: () => import('../pages/import.js'),
  health: () => import('../pages/health.js'),
  help: () => import('../pages/help.js'),
  'sign-in': () => import('../pages/sign-in.js'),
  setup: () => import('../pages/setup.js'),
});
/** Screens shown without the frame (no account yet, or not signed in): no header, dock or footer. */
export const AUTH_SCREENS = Object.freeze(['sign-in', 'setup']);

const main = () => document.getElementById('main');
const same = (a, b) => JSON.stringify(a ?? null) === JSON.stringify(b ?? null);
const keyOf = (s) => [s.page, s.car, s.drive, s.charge].join('|');
const browserZone = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC'; } catch { return 'UTC'; } })();

let frame = null;
let me = null;
let carsCount = null;
let current = null; // { key, id, ctx, instance, scope, cleanups }
let token = 0;
let first = true;
let shownCar = null; // the car the header shows
let selfQuery = false;

/** Load a page's own style sheet once (pages/<id>.css); resolves when it applies. */
function pageCss(id) {
  let link = document.head.querySelector(`link[data-page-css="${id}"]`);
  const wait = (l) => new Promise((resolve, reject) => {
    l.addEventListener('load', resolve, { once: true });
    l.addEventListener('error', () => { l.remove(); reject(new Error(`${l.href} did not load`)); }, { once: true });
  });
  if (link) return link.sheet ? Promise.resolve() : wait(link);
  link = h('link', { rel: 'stylesheet', href: new URL(`../pages/${id}.css`, import.meta.url).href, dataset: { pageCss: id } });
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

function jumpToHash() {
  let target = null;
  try { const k = decodeURIComponent((location.hash || '').slice(1)); target = k ? document.getElementById(k) : null; } catch { target = null; }
  if (target) target.scrollIntoView({ block: 'start' });
}

function makeCtx(s, my, scope, cleanups) {
  const id = s.page;
  let jumped = false;
  const info = pageInfo(id === 'health' && !s.car ? 'system' : id) || pageInfo(id);
  const ctx = {
    id, info, state: s, query: { ...s.query }, carId: s.car, driveId: s.drive, chargeId: s.charge,
    car: null, tz: browserZone, period: null, params: {},
    api: scope, signal: scope.signal, view: main(), main: main(), me,
    admin: !!(me && me.role === 'admin'),
    /** True while this page is still the one on screen. */
    alive: () => my === token,
    /** The first render with data is done: data-ready = page id. A link with #id lands on that part. */
    ready: () => { if (my === token) { main().style.minHeight = ''; ready(id); if (!jumped) { jumped = true; jumpToHash(); } } },
    /** The page could not load: its error tile, and data-ready = 'error:<id>'. retry: null hides Try again. */
    fail: (err, { retry = () => remount() } = {}) => {
      if (my !== token || api.isAbort(err)) return;
      console.warn(`page ${id}:`, err);
      main().removeAttribute('aria-busy');
      main().style.minHeight = '';
      main().replaceChildren(errorTile(err, { retry }));
      failed(id);
    },
    onCleanup: (fn) => { cleanups.push(fn); },
    on: (target, type, fn, opts) => { target.addEventListener(type, fn, opts); cleanups.push(() => target.removeEventListener(type, fn, opts)); },
    every: (ms, fn) => { const t = setInterval(() => { if (my === token) fn(); }, ms); cleanups.push(() => clearInterval(t)); },
    /** The tab title: ctx.title('Home to Work') -> 'Home to Work · Comet · Sentry Mate'. */
    title: (...parts) => { if (my === token) setTitle(...parts, ctx.car ? ctx.car.name : null); },
    /** The page header: ctx.head({ title: 'Home to Work', actions: [button] }). */
    head: (patch) => { if (my === token) frame?.setHead(patch); },
    setQuery: (patch, opts) => {
      if (my !== token) return;
      selfQuery = true;
      try { state.setQuery(patch, opts); } finally { selfQuery = false; }
      ctx.query = { ...state.current().query };
    },
    link: (page, params) => state.linkTo(page, params),
    navigate: (href, opts) => state.navigate(href, opts),
    go: (href, opts) => state.navigate(href, opts),
    reload: () => remount(),
    events: (types, fn) => scope.events((ev) => { if (my === token) fn(ev); }, { types }),
    toast,
    cover,
  };
  return ctx;
}

async function mount(s) {
  const my = ++token;
  leave();
  unready();
  clearToasts();
  const id = s.page;
  const scope = api.scope();
  const cleanups = [];
  const ctx = makeCtx(s, my, scope, cleanups);
  current = { key: keyOf(s), id, ctx, instance: null, scope, cleanups };
  const m = main();
  const auth = AUTH_SCREENS.includes(id);
  document.documentElement.classList.toggle('is-auth', auth);
  document.documentElement.dataset.page = id;
  m.className = `bento page-${id}`;
  m.setAttribute('aria-busy', 'true');
  if (!first) scrollToTop();
  try {
    if (id === 'not-found') {
      frame?.update({ page: null, info: null, car: null, tz: browserZone, period: null, carsCount });
      frame?.setHead({ title: 'Not found' });
      setTitle('Not found');
      m.replaceChildren(errorTile(Object.assign(new Error('There is no page at this address. The dock below leads to every page.'), { status: 404, code: 'not_found' })));
      m.removeAttribute('aria-busy');
      failed('not-found');
      return;
    }
    const modP = loadPage(async () => {
      const mod = await MODULES[id]();
      if (mod.css) await pageCss(id);
      return mod;
    });
    modP.catch(() => { /* answered where it is awaited */ });
    if (!auth) m.replaceChildren(...loadingTiles(id));
    const carP = s.car && !auth ? scope.get(api.apiPath`cars/${s.car}`) : null;
    // the header gets its car and period pills at once (the car's name follows), so it does not grow under the
    // loading tiles when the car arrives
    if (carP && !auth) frame?.update({ page: id, info: ctx.info, car: shownCar && shownCar.id === s.car ? shownCar : { id: s.car, name: '' }, tz: browserZone, period: resolveFor(ctx.info, s, browserZone), carsCount });
    let car = null;
    try { car = carP ? await carP : null; } catch (err) {
      if (err && err.status === 404) state.forgetCar(s.car);
      throw err;
    }
    if (my !== token) return;
    if (car) state.rememberCar(car.id, { sold: !!car.sold });
    ctx.car = car;
    ctx.tz = (car && car.time_zone) || browserZone;
    ctx.period = resolveFor(ctx.info, s, ctx.tz);
    ctx.params = ctx.period ? periodParams(ctx.period) : {};
    if (!auth) { frame?.update({ page: id, info: ctx.info, car, tz: ctx.tz, period: ctx.period, carsCount }); shownCar = car; }
    const mod = await modP;
    if (my !== token) return;
    if (mod.shape && !auth) m.replaceChildren(...loadingTiles(mod.shape));
    setTitle(mod.title ? mod.title(ctx) : (ctx.info ? ctx.info.title : id), car ? car.name : null);
    // the loading tiles stay until the page puts its own content in (taken away in the same task, before a paint),
    // so the page does not collapse to nothing while it waits for its data and then grow again
    // Until the page is ready the view keeps the loading tiles' height, so a page that fills its boxes one by one
    // does not first shrink to its filter row (the footer coming up) and then grow again.
    const ghosts = [...m.children];
    if (ghosts.length) m.style.minHeight = `${m.offsetHeight}px`;
    const unghost = () => { mo.disconnect(); for (const g of ghosts) if (g.parentNode === m) g.remove(); };
    const mo = new MutationObserver(unghost);
    mo.observe(m, { childList: true });
    let inst;
    try { inst = await mod.mount(ctx); } finally { if (my === token) { unghost(); m.style.minHeight = ''; } else mo.disconnect(); }
    if (my !== token) { try { inst?.unmount?.(); } catch { /* left */ } return; }
    current.instance = inst || null;
    m.removeAttribute('aria-busy');
    if (!first) m.focus({ preventScroll: true });
    first = false;
    showPending();
  } catch (err) {
    if (my !== token || api.isAbort(err)) return;
    first = false;
    if (!auth && !ctx.car) frame?.update({ page: id, info: ctx.info, car: null, tz: ctx.tz, period: null, carsCount });
    // failed before the page was built: the tab's title is this page's, never the one before
    if (!current?.instance) setTitle(ctx.info ? ctx.info.title : id, ctx.car ? ctx.car.name : null);
    ctx.fail(err, isPageFailed(err) ? { retry: () => reloadPage() } : undefined);
  }
}

/** Show the current page again from scratch (new units, or Try again). */
export function remount() { mount(state.current()); }

async function onState(s, prev) {
  if (current && current.key === keyOf(s)) {
    const periodChanged = !same(s.period, prev && prev.period);
    const queryChanged = !same(s.query, prev && prev.query);
    if (queryChanged && selfQuery && !periodChanged) return; // the page changed its own filters
    if (!periodChanged && !queryChanged) return;
    const inst = current.instance;
    if (inst && inst.onQuery && queryChanged && !periodChanged) {
      const c = current.ctx;
      c.state = s; c.query = { ...s.query };
      unready();
      try { await inst.onQuery(c.query, c); c.ready(); } catch (err) { c.fail(err); }
      return;
    }
    if (inst && inst.update && !queryChanged) {
      const c = current.ctx;
      c.state = s;
      c.query = { ...s.query };
      c.period = resolveFor(c.info, s, c.tz);
      c.params = c.period ? periodParams(c.period) : {};
      frame?.update({ page: c.id, info: c.info, car: c.car, tz: c.tz, period: c.period, carsCount }); shownCar = c.car;
      unready();
      try { await inst.update(c, { period: true }); } catch (err) { c.fail(err); }
      return;
    }
  }
  mount(s);
}

/** Start following the address: startRouter({ frame, me, carsCount }). */
export function startRouter(opts) {
  frame = opts.frame || null;
  me = opts.me || null;
  carsCount = opts.carsCount ?? null;
  state.subscribe(onState);
  state.startRouting();
  onPrefs((p, changed) => {
    if (!current || !['units', 'range', 'language'].some((k) => changed.has(k))) return;
    if (current.instance?.update) { unready(); Promise.resolve(current.instance.update(current.ctx, { prefs: true })).catch((err) => current?.ctx.fail(err)); } else remount();
  });
}

export { navId };
