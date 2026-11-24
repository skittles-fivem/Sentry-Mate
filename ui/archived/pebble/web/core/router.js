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
//   export const headLater = true;                  // optional: its title or buttons come with its data (holdFrame)
//   export function title(ctx) { return 'Drives'; }  // optional: the tab title's first part
//   export async function mount(ctx) { ...; ctx.ready(); return { unmount() {}, update(ctx, why) {} }; }
// update() is optional: without it a new period mounts the page again.
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
import { h, ready, failed, unready, setTitle, cover, holdScroll } from '../shared/dom.js';
import { periodParams } from '../shared/time.js';
import { pageInfo, navId } from './pages.js';
import { resolveFor } from './period.js';
import { loadingTiles, errorTile } from './states.js';
import { showPending, clearToasts, toast } from './toast.js';
import { onPrefs } from './prefs.js';
import { scrollToTop } from './display.js';
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
/** Screens shown without the frame (no account yet, or not signed in): no header, dock or footer. */
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
let current = null; // { key, id, ctx, instance, scope, cleanups }
let token = 0;
let first = true;
let shownCar = null; // the car the header shows
let selfQuery = false;
let warmedMain = false;
let staged = null; // a page drawn again in a hidden box: { box, commit, drop }
let holding = null; // the hold of the page being shown (holdFrame)

// A new page's part of the frame (its header row, the notices, the grid, the footer) stays hidden until its header is
// final: the car's name in its pill, the page's own buttons, a title from its data (a page that sets its header once
// its data is in says so with `export const headLater = true`). A header that changed after it was shown would push
// the whole page down on a phone (a longer title wraps, a pill wraps to a new row). At most HOLD_MS: a slow answer
// then shows the grey tiles and the header as they are.
const HOLD_MS = 2000;
function holdFrame() {
  const root = document.documentElement;
  const me = () => {
    if (holding !== me) return;
    holding = null;
    clearTimeout(t);
    root.removeAttribute('data-hold');
  };
  if (holding) holding();
  holding = me;
  root.setAttribute('data-hold', '');
  const t = setTimeout(me, HOLD_MS);
  return me;
}

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
    // (what is on screen stays where it is: the page drawn again may be taller or shorter above it)
    holdScroll(m);
    m.replaceChildren(...nodes);
    // once in place, the page's view is <main> itself: what it draws later (a live refresh of its list) goes there,
    // not into the box that was taken away
    staged_.onCommit?.();
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
  const staged_ = staged = { box, commit, settle, drop, onCommit: null };
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
  const info = pageInfo(id === 'health' && !s.car ? 'system' : id) || pageInfo(id);
  const tz = (car && car.time_zone) || browserZone;
  const period = resolveFor(info, s, tz);
  await mod.prefetch({
    id, info, state: s, query: { ...s.query }, carId: s.car, driveId: s.drive, chargeId: s.charge,
    car, tz, period, params: period ? periodParams(period) : {}, api: prefetchApi(api), me,
    admin: !!(me && me.role === 'admin'), prefetching: true,
  });
}

function jumpToHash() {
  let target = null;
  try { const k = decodeURIComponent((location.hash || '').slice(1)); target = k ? document.getElementById(k) : null; } catch { target = null; }
  if (target) target.scrollIntoView({ block: 'start' });
}

function makeCtx(s, my, scope, cleanups, view = main(), stage = null, tracker = null, hold = null) {
  const id = s.page;
  let jumped = !!stage;
  const info = pageInfo(id === 'health' && !s.car ? 'system' : id) || pageInfo(id);
  const ctx = {
    id, info, state: s, query: { ...s.query }, carId: s.car, driveId: s.drive, chargeId: s.charge,
    car: null, tz: browserZone, period: null, params: {},
    api: scope, signal: scope.signal, view, main: view, me,
    admin: !!(me && me.role === 'admin'),
    /** True while this page is still the one on screen. */
    alive: () => my === token,
    /** The first render with data is done: data-ready = page id. A link with #id lands on that part. */
    ready: () => {
      if (my !== token) return;
      tracker?.close();
      hold?.();
      if (!warmedMain) { warmedMain = true; warmModules(MAIN.map((p) => MODULES[p])); }
      main().style.minHeight = '';
      ready(id);
      if (!jumped) { jumped = true; jumpToHash(); }
    },
    /** The page could not load: its error tile, and data-ready = 'error:<id>'. retry: null hides Try again. */
    fail: (err, { retry = () => remount() } = {}) => {
      if (my !== token || api.isAbort(err)) return;
      console.warn(`page ${id}:`, err);
      main().removeAttribute('aria-busy');
      main().style.minHeight = '';
      holdScroll(view);
      view.replaceChildren(errorTile(err, { retry }));
      hold?.();
      stage?.commit();
      failed(id);
    },
    onCleanup: (fn) => { cleanups.push(fn); },
    on: (target, type, fn, opts) => { target.addEventListener(type, fn, opts); cleanups.push(() => target.removeEventListener(type, fn, opts)); },
    every: (ms, fn) => { const t = setInterval(() => { if (my === token) fn(); }, ms); cleanups.push(() => clearInterval(t)); },
    /** The tab title: ctx.title('Home to Work') -> 'Home to Work · Comet · Sentry Mate'. */
    title: (...parts) => { if (my === token) setTitle(...parts, ctx.car ? ctx.car.name : null); },
    /** The page header: ctx.head({ title: 'Home to Work', actions: [button] }). */
    head: (patch) => { if (my === token) { frame?.setHead(patch); hold?.(); } },
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

// live: draw the page again in place (its kept answers changed), see stageIn
async function mount(s, { live = false } = {}) {
  const my = ++token;
  const finishOld = leave(live);
  if (!live) { unready(); clearToasts(); setStale(false); }
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
  const auth = AUTH_SCREENS.includes(id);
  const hold = !live && !auth ? holdFrame() : null;
  if (!live && auth) holding?.(); // (a sign-in screen has no header to wait for)
  const ctx = makeCtx(s, my, scope, cleanups, stage ? stage.box : m, stage, tracker, hold);
  if (stage) stage.onCommit = () => { ctx.view = ctx.main = m; };
  rec = current = { key: keyOf(s), id, ctx, instance: null, scope, cleanups, tracker, shown: false, changed: false };
  if (!live) {
    document.documentElement.classList.toggle('is-auth', auth);
    document.documentElement.dataset.page = id;
    m.className = `bento page-${id}`;
    m.setAttribute('aria-busy', 'true');
    if (!first) scrollToTop();
  }
  try {
    if (id === 'not-found') {
      frame?.update({ page: null, info: null, car: null, tz: browserZone, period: null, carsCount });
      frame?.setHead({ title: 'Not found' });
      setTitle('Not found');
      m.replaceChildren(errorTile(Object.assign(new Error('There is no page at this address. The dock below leads to every page.'), { status: 404, code: 'not_found' })));
      m.removeAttribute('aria-busy');
      hold?.();
      failed('not-found');
      return;
    }
    const modP = loadPage(async () => {
      const mod = await MODULES[id]();
      if (mod.css) await pageCss(id);
      return mod;
    });
    modP.catch(() => { /* answered where it is awaited */ });
    if (!auth && !live) m.replaceChildren(...loadingTiles(id));
    const carP = s.car && !auth ? scope.get(api.apiPath`cars/${s.car}`) : null;
    // the header gets its car and period pills at once (the car's name follows), so it does not grow under the
    // loading tiles when the car arrives
    if (carP && !auth && !live) frame?.update({ page: id, info: ctx.info, car: shownCar && shownCar.id === s.car ? shownCar : { id: s.car, name: '' }, tz: browserZone, period: resolveFor(ctx.info, s, browserZone), carsCount });
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
    // (a page drawn again keeps its header: the one drawn again sets the same title and buttons when it is put in)
    if (!auth) { frame?.update({ page: id, info: ctx.info, car, tz: ctx.tz, period: ctx.period, carsCount, keepHead: live }); if (car) shownCar = car; }
    const mod = await modP;
    if (my !== token) return;
    if (mod.shape && !auth && !live) m.replaceChildren(...loadingTiles(mod.shape));
    setTitle(mod.title ? mod.title(ctx) : (ctx.info ? ctx.info.title : id), car ? car.name : null);
    // the loading tiles stay until the page puts its own content in (taken away in the same task, before a paint),
    // so the page does not collapse to nothing while it waits for its data and then grow again
    // Until the page is ready the view keeps the loading tiles' height, so a page that fills its boxes one by one
    // does not first shrink to its filter row (the footer coming up) and then grow again.
    const ghosts = live ? [] : [...m.children];
    if (ghosts.length) m.style.minHeight = `${m.offsetHeight}px`;
    const unghost = () => { mo.disconnect(); for (const g of ghosts) if (g.parentNode === m) g.remove(); };
    const mo = new MutationObserver(unghost);
    mo.observe(m, { childList: true });
    let inst;
    try {
      const mounted = mod.mount(ctx);
      // (its own buttons are in the header by now: what mount() does before its first await)
      if (!mod.headLater) hold?.();
      inst = await mounted;
    } finally { if (my === token) { unghost(); m.style.minHeight = ''; } else mo.disconnect(); }
    if (my !== token) { try { inst?.unmount?.(); } catch { /* left */ } return; }
    current.instance = inst || null;
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
    hold?.();
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
  watchLinks(warm);
  onPrefs((p, changed) => {
    if (!current || !['units', 'range', 'language'].some((k) => changed.has(k))) return;
    if (current.instance?.update) { unready(); Promise.resolve(current.instance.update(current.ctx, { prefs: true })).catch((err) => current?.ctx.fail(err)); } else remount();
  });
}

export { navId };
