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
//   export function prefetch(ctx) { ctx.api.get(...); } // optional: the first reads, warmed ahead (below)
//
// Fast page changes: a link under the pointer (or touched, or focused) gets its page ready ahead (warm(): the
// module, the car and the page's prefetch(ctx), the same reads its mount makes), and once the first page is
// drawn the main sections' modules load while the browser is idle. A page that may show kept answers (not the
// settings forms) takes them at once for its first reads; <html data-stale> marks them until they are checked,
// and a page whose answers changed is drawn again in a hidden box that takes the place of the old one in one
// step, so the page on screen never goes away.

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
import { holdSnap } from './snap.js';
import { watchLinks, warmModules, prefetchApi, pageSheet, setStale } from '../shared/prefetch.js';

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
// the main sections: their modules load when the browser is idle after the first page
const MAIN = ['car', 'drives', 'drive', 'charges', 'battery', 'statistics', 'timeline', 'fleet'];
// pages that may show kept answers at once (marked stale until checked): not forms
const takesStale = (id) => !AUTH_SCREENS.includes(id) && !/^(settings-|import$)/.test(id);

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
let warmedMain = false;
let staged = null; // a page drawn again in a hidden box: { box, commit, drop }

/** Load a page's own style sheet once (pages/<id>.css, kept off while another page is shown); resolves when it applies. */
function pageCss(id) { return pageSheet(id, new URL(`../pages/${id}.css`, import.meta.url).href); }

// keep: the page stays on screen until the one drawn again takes its place; its requests and events stop at
// once, its own clean-up (charts, maps) waits (the returned function)
function leave(keep = false) {
  const older = staged?.drop(keep) || null;
  staged = null;
  if (!current) return older;
  const c = current;
  current = null;
  c.tracker?.stop();
  c.release();
  c.scope.abort();
  const finish = () => {
    older?.();
    try { c.instance?.unmount?.(); } catch (e) { console.error(e); }
    for (const f of c.cleanups.splice(0)) { try { f(); } catch (e) { console.error(e); } }
  };
  if (keep) return finish;
  finish();
  return null;
}

// The page drawn again (its kept answers changed): it is built in an invisible box of <main>'s width and put in
// <main> in one step once it is drawn and has stopped changing, with no request of its own under way (at most
// 6 s), so nothing goes away and comes back.
function stageIn(m, finishOld, busy) {
  const r = m.getBoundingClientRect();
  const box = h('div', { class: m.className, 'aria-hidden': 'true', inert: '' });
  Object.assign(box.style, {
    position: 'fixed', top: '0', left: `${r.left}px`, width: `${r.width}px`, boxSizing: 'border-box', height: '100vh', overflow: 'hidden',
    visibility: 'hidden', pointerEvents: 'none', zIndex: '-1',
  });
  document.body.append(box);
  let done = false, quietT = 0, mo = null;
  const end = () => { done = true; clearTimeout(quietT); clearTimeout(maxT); mo?.disconnect(); box.remove(); if (staged?.box === box) staged = null; };
  const maxT = setTimeout(() => commit(), 12000);
  function commit() {
    if (done) return;
    const nodes = [...box.childNodes];
    end();
    m.replaceChildren(...nodes);
    setStale(false);
    finishOld?.();
  }
  function settle() {
    if (done) return;
    const later = setTimeout(() => commit(), 6000);
    const again = () => {
      clearTimeout(quietT);
      quietT = setTimeout(() => { if (busy() > 0) { again(); return; } clearTimeout(later); commit(); }, 200);
    };
    mo = new MutationObserver(again);
    mo.observe(box, { subtree: true, childList: true, attributes: true, characterData: true });
    again();
  }
  const drop = (keepOld) => { if (done) return null; end(); if (keepOld) return finishOld; finishOld?.(); return null; };
  staged = { box, commit, settle, drop };
  return staged;
}

// A page's API client that counts its requests under way (a page drawn again waits for them), and with a
// tracker takes kept answers at once until the page is drawn (api.staleTracker).
function counted(sc, tracker = null) {
  let n = 0;
  const track = (p) => { n++; const off = () => { n--; }; p.then(off, off); return p; };
  const st = (o) => (tracker?.open ? { ...o, stale: tracker } : o);
  return {
    ...sc,
    get busy() { return n; },
    get aborted() { return sc.aborted; },
    get: (path, params, o) => track(sc.get(path, params, st(o))),
    page: (path, params, o) => track(sc.page(path, params, st(o))),
    list: (path, params, o) => { const l = sc.list(path, params, st(o)); const all = l.all.bind(l); return Object.assign(l, { all: (...b) => track(all(...b)) }); },
    pager: (path, params, o) => sc.pager(path, params, st(o)),
  };
}

// A link to s may be followed: load its page's module and style sheet, its car, and the first reads the page
// asks for (its prefetch(ctx), with the ctx mount() will get), so it draws at once.
async function warm(s) {
  const id = s.page;
  const load = MODULES[id];
  if (!load || AUTH_SCREENS.includes(id)) return;
  const modP = load();
  // a car page may show a map: its tile settings, as core/map.js reads them
  if (s.car) api.prefetch('maps/config', null, { maxAgeMs: 300000 });
  const car = s.car ? await api.prefetch(api.apiPath`cars/${s.car}`) : null;
  if (s.car && !car) return;
  const mod = await modP;
  if (mod.css) pageSheet(id, new URL(`../pages/${id}.css`, import.meta.url).href, { on: false });
  if (typeof mod.prefetch !== 'function') return;
  const info = pageInfo(id);
  const tz = (car && car.time_zone) || browserZone;
  const period = resolveFor(info, s, tz);
  await mod.prefetch({
    id, info, state: s, query: { ...s.query }, carId: s.car, driveId: s.drive, chargeId: s.charge,
    car, tz, period, params: period ? periodParams(period) : {}, api: prefetchApi(api), me, prefetching: true,
  });
}

/** After a page's first render: scroll to the part a link's #id names, when the page has it. */
function jumpToHash() {
  let target = null;
  try { const k = decodeURIComponent((window.location.hash || '').slice(1)); target = k ? document.getElementById(k) : null; } catch { target = null; }
  if (target) target.scrollIntoView({ block: 'start' });
}

/** The context a page gets. Everything a page needs from the frame goes through it. */
function makeCtx(s, my, scope, cleanups, view = main(), stage = null, tracker = null, release = () => {}) {
  const id = s.page;
  let jumped = !!stage;
  const info = pageInfo(id);
  const ctx = {
    id, info, state: s, query: { ...s.query }, carId: s.car, driveId: s.drive, chargeId: s.charge,
    car: null, tz: browserZone, period: null, params: {},
    api: scope, signal: scope.signal, main: view, me,
    /** True while this page is still the one on screen. */
    alive: () => my === token,
    /** The first render with data is done: data-ready = page id. A link with #id lands on that part. */
    ready: () => {
      if (my !== token) return;
      release();
      tracker?.close();
      if (!warmedMain) { warmedMain = true; warmModules(MAIN.map((p) => MODULES[p])); }
      ready(id);
      if (!jumped) { jumped = true; jumpToHash(); }
    },
    /** The page could not load: its error state, and data-ready = 'error:<id>'. */
    fail: (err, { retry = () => remount() } = {}) => {
      if (my !== token || api.isAbort(err)) return;
      console.warn(`page ${id}:`, err);
      main().removeAttribute('aria-busy');
      view.replaceChildren(errorView(err, { retry }));
      release();
      stage?.commit();
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

// live: draw the page again in place (its kept answers changed), see stageIn
async function mount(s, { live = false } = {}) {
  const my = ++token;
  const finishOld = leave(live);
  if (!live) { unready(); clearFlash(); setStale(false); }
  const id = s.page;
  const m = main();
  let rec = null;
  // kept answers at once for the first reads (after a reload: the service worker's); the page is drawn again
  // in place when one of them changed
  const tracker = !live && takesStale(id) ? api.staleTracker({
    worker: first,
    onStale: () => { if (my === token) setStale(true); },
    onFresh: ({ changed, failed: no }) => {
      if (my !== token || no) return;
      if (!changed) setStale(false);
      else if (rec.shown) mount(state.current(), { live: true });
      else rec.changed = true; // drawn again once it is shown
    },
  }) : null;
  const scope = counted(api.scope(), tracker);
  const stage = live ? stageIn(m, finishOld, () => scope.busy) : null;
  const cleanups = [];
  // the page is built in one go from kept answers: the whole-pixel snap runs once, when it is drawn (core/snap.js)
  const release = holdSnap();
  const ctx = makeCtx(s, my, scope, cleanups, stage ? stage.box : m, stage, tracker, release);
  rec = current = { key: keyOf(s), id, ctx, instance: null, scope, cleanups, mod: null, tracker, shown: false, changed: false, release };
  if (!live) {
    m.className = `wrap main page-${id}`;
    m.setAttribute('aria-busy', 'true');
    m.replaceChildren(loadingView());
    if (!first) { window.scrollTo(0, 0); }
  }
  try {
    if (id === 'not-found') {
      frame?.update({ page: null, info: null, car: null, tz: null, period: null, carsCount });
      setTitle('Not found');
      const err = Object.assign(new Error('There is no page at this address.'), { status: 404, code: 'not_found' });
      m.replaceChildren(errorView(err));
      m.removeAttribute('aria-busy');
      release();
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
    ctx.main.replaceChildren();
    const inst = await mod.mount(ctx);
    if (my !== token) { try { inst?.unmount?.(); } catch { /* left */ } return; }
    current.instance = inst || null;
    // a page that never calls ready() is snapped a moment after its mount; one that does, once at ready (its mount
    // often returns before its data is in, and every part it draws after would start a whole-page snap of its own)
    setTimeout(release, 2000);
    rec.shown = true;
    if (rec.changed) setTimeout(() => { if (my === token) mount(state.current(), { live: true }); }, 0);
    if (stage) { stage.settle(); return; }
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
  watchLinks(warm);
  // New units, range or language: every number on the page changes, so draw it again.
  onPrefs((p, changed) => {
    if (!current || !['units', 'range', 'language'].some((k) => changed.has(k))) return;
    if (current.instance?.update) { unready(); Promise.resolve(current.instance.update(current.ctx, { prefs: true })).catch((err) => current?.ctx.fail(err)); } else remount();
  });
}
