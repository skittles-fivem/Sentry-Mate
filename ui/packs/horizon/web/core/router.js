// The router: from the address to a page on screen.
//
// shared/state.js owns the address (page, car, drive, charge, period, query). On each change the router
// decides whether the page must be built again (another page, car, drive, charge or period, or filters the
// page did not set itself), and then:
//   1. asks core/session.js where the page may open (set-up first, then sign-in, then the page);
//   2. reads the car of a car page (GET /cars/{car}); an unknown or hidden car is "not found";
//   3. works out the period in the car's zone (the address's, else the tab's, else the page's own);
//   4. loads pages/<id>.js (and pages/<id>.css while the page is open, if it says css = true);
//   5. calls mount(ctx). The page draws into ctx.view and calls ctx.ready() after its first render with data,
//      or ctx.fail(err) when it cannot load. core/README.md lists everything on ctx.
// Leaving a page cancels what it started (ctx.api calls, ctx.on listeners, ctx.every timers, ctx.events).

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { h, replace, ready as markReady, failed as markFailed, unready, setTitle as docTitle, on } from '../shared/dom.js';
import { resolvePeriod, periodParams } from '../shared/time.js';
import { station, fullName } from './stations.js';
import * as frame from './frame.js';
import * as session from './session.js';
import { loadingDeck, errorState, offlineState, statePanel } from './states.js';
import { showWaitingToast } from './layer.js';
import { getMe, getPrefs, isAdmin } from './prefs.js';

const BARE = new Set(['sign-in', 'setup']);
let open = null;   // { key, qkey, ctx, mod, inst, cleanups, scope }
let seq = 0;
let first = true;
// Rebuilds of the page that is on screen (a live update, a retry, a filter) draw off screen, in a hidden
// stage of the same width, and take the place of the old page in one step once the new one has its data:
// nothing on screen goes back to loading and nothing jumps. The old page's cleanups wait until then.
let stale = [];
const flushStale = () => { for (const o of stale.splice(0)) runCleanups(o); };

const routeKey = (s) => [s.page, s.car, s.drive, s.charge, s.period ? JSON.stringify(s.period) : ''].join('|');
const queryKey = (q) => JSON.stringify(Object.entries(q || {}).sort());

let zoneHere = Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
/** The zone for pages without a car: the last car's, else this browser's. */
export const fallbackZone = () => zoneHere;

/** Follow the address. Call once, after the frame is built. */
export function startRouter() {
  state.subscribe((s) => queueMicrotask(() => changed(s)));
  state.startRouting();
}

/** Build the page on screen again (keeps the scroll). */
export const rebuild = () => mount(state.current(), { keepScroll: true });

function changed(s) {
  if (s !== state.current()) return;   // a newer change follows
  if (open && open.key === routeKey(s)) {
    const qk = queryKey(s.query);
    if (qk === open.qkey) return;      // the same filters, or the page set them itself
    open.qkey = qk;
    const onQuery = open.inst?.onQuery || open.mod?.onQuery;
    if (onQuery) {
      const o = open;
      o.ctx.query = s.query;
      o.ctx.state = s;
      Promise.resolve().then(() => onQuery(s.query, o.ctx)).then(() => { if (open === o) markReady(o.ctx.id); }, (e) => { if (open === o) o.ctx.fail(e); });
      return;
    }
  }
  mount(s, { keepScroll: !!open && open.ctx?.id === s.page && open.ctx?.carId === s.car });
}

function runCleanups(o) {
  try { o.inst?.unmount?.(); } catch (e) { console.error(e); }
  for (const f of o.cleanups.splice(0).reverse()) { try { f(); } catch (e) { console.error(e); } }
}

function leave(quiet = false) {
  if (!open) return;
  const o = open;
  open = null;
  o.scope.abort();
  if (quiet) stale.push(o); else { flushStale(); runCleanups(o); }
}

function pageCss(id) {
  if (document.head.querySelector(`link[data-page-css="${id}"]`)) return Promise.resolve();
  const link = h('link', { rel: 'stylesheet', href: `/pages/${id}.css`, dataset: { pageCss: id } });
  const done = new Promise((r) => { link.addEventListener('load', r, { once: true }); link.addEventListener('error', r, { once: true }); });
  document.head.append(link);
  return done;
}
const dropCss = (keep) => { for (const l of document.head.querySelectorAll('link[data-page-css]')) if (l.dataset.pageCss !== keep) l.remove(); };

async function mount(s, { keepScroll = false } = {}) {
  const my = ++seq;
  const view = frame.view();
  const quiet = keepScroll && !first && !!open && open.ctx?.id === s.page && view.childElementCount > 0;
  leave(quiet);
  if (!quiet) unready();
  const id = s.page;
  const info = station(id);
  const scope = api.scope();
  const cleanups = [];
  const rec = { key: routeKey(s), qkey: queryKey(s.query), ctx: null, mod: null, inst: null, cleanups, scope, stage: null };
  open = rec;
  const alive = () => open === rec && my === seq;
  document.documentElement.dataset.screen = BARE.has(id) ? 'bare' : 'app';
  document.documentElement.dataset.page = id;
  if (quiet) {
    // the stage: a second .view (same width, padding and container queries), hidden and out of the flow
    rec.stage = h('main', { class: 'view view--stage', 'aria-hidden': 'true', 'aria-busy': 'true' });
    rec.stage.inert = true;
    for (const old of document.querySelectorAll('.view--stage')) old.remove();
    view.after(rec.stage);
  } else {
    flushStale();
    view.setAttribute('aria-busy', 'true');
  }
  let swapTimer = 0;
  const dropStage = () => { clearTimeout(swapTimer); rec.stage?.remove(); rec.stage = null; };
  // the new page takes the old one's place in one step
  const swap = () => {
    if (!rec.stage || !alive()) return;
    const st = rec.stage;
    dropStage();
    flushStale();
    view.replaceChildren(...st.childNodes);
    view.removeAttribute('aria-busy');
  };
  cleanups.push(dropStage);

  const showError = (err, retry = () => mount(state.current(), { keepScroll: true })) => {
    if (!alive()) return;
    dropStage();
    flushStale();
    const offline = !navigator.onLine && (err?.status === 0);
    replace(view, statePanel(offline ? offlineState(retry) : errorState(err, { retry, home: id !== 'fleet' && !BARE.has(id) })));
    view.removeAttribute('aria-busy');
    markFailed(id);
  };

  let gate;
  try { gate = await session.admit(id, s.path + (location.search || '')); } catch (err) {
    frame.update({ page: id, info, car: null, period: null });
    showError(err);
    return;
  }
  if (!alive()) return;
  if (gate.go) { state.navigate(gate.go, { replace: true }); return; }

  if (!info) {
    frame.update({ page: id, info: null, car: null, period: null });
    docTitle('Not found');
    showError(new api.ApiError({ status: 404, code: 'not_found', message: 'There is no page at this address. Tune to another one with the arrows, or open All cars.' }), null);
    return;
  }

  if (!keepScroll || first) replace(view, loadingDeck(3));

  let car = null;
  if (s.car) {
    try { car = await scope.get(api.apiPath`cars/${s.car}`, null, { maxAgeMs: 15000 }); } catch (err) {
      if (api.isAbort(err) || !alive()) return;
      if (err.status === 404) { state.forgetCar(s.car); err.message = 'There is no car at this address, or your account may not see it.'; }
      frame.update({ page: id, info, car: null, period: null });
      docTitle(fullName(info));
      showError(err);
      return;
    }
    if (!alive()) return;
    if (car.time_zone) zoneHere = car.time_zone;
    state.rememberCar(car.id);
  } else if (info.period && state.lastCar()) {
    // a page with a period and no car (Travel): its days are the last car's days
    try { const last = await scope.get(api.apiPath`cars/${state.lastCar()}`, null, { maxAgeMs: 60000 }); if (last.time_zone) zoneHere = last.time_zone; } catch (err) {
      if (api.isAbort(err) || !alive()) return;
      if (err.status === 404) state.forgetCar(state.lastCar());
    }
    if (!alive()) return;
  }
  const tz = car?.time_zone || zoneHere;
  const period = info.period ? resolvePeriod(s.period ?? { preset: info.period }, { now: api.now(), tz, fallback: info.period }) : null;
  frame.update({ page: id, info, car, period, tz });

  let mod;
  try {
    mod = await import(`../pages/${id}.js`);
    if (mod.css) await pageCss(id);
  } catch (err) {
    if (!alive()) return;
    console.error(err);
    showError(new api.ApiError({ status: 0, code: 'page_failed', message: 'This page could not be loaded. Reload the browser tab.' }));
    return;
  }
  if (!alive()) return;
  dropCss(mod.css ? id : null);
  rec.mod = mod;

  const ctx = {
    id, info, get view() { return rec.stage || view; }, state: s, query: s.query, car, carId: s.car, tz, period,
    params: period ? periodParams(period) : {},
    api: scope, me: getMe(), prefs: getPrefs(), admin: isAdmin(),
    alive,
    onCleanup(f) { cleanups.push(f); return f; },
    on(target, type, f, o) { const off = on(target, type, f, o); cleanups.push(off); return off; },
    every(ms, f) { const t = setInterval(() => { if (alive()) f(); }, ms); cleanups.push(() => clearInterval(t)); return t; },
    events(types, f) { return scope.events((ev) => { if (alive()) f(ev); }, { types: [].concat(types) }); },
    // ready only once the bundled fonts are in: the page is then drawn as it will stay (no swap after it)
    ready() {
      if (!alive()) return;
      // a page that goes on loading after its first render: show it at the latest a moment after
      if (rec.stage) { clearTimeout(swapTimer); swapTimer = setTimeout(swap, 4000); } else view.removeAttribute('aria-busy');
      const done = () => { if (alive()) markReady(id); };
      const fonts = document.fonts?.ready;
      if (fonts) fonts.then(done, done); else done();
    },
    fail(err, o = {}) { if (api.isAbort(err) || !alive()) return; console.warn(err); showError(err, o.retry === undefined ? undefined : o.retry); },
    setTitle(...parts) { docTitle(...parts, car?.name); },
    link: (page, params) => state.linkTo(page, params),
    setQuery(patch, o) {
      if (!alive()) return state.current();
      const next = state.setQuery(patch, o);
      rec.qkey = queryKey(next.query);
      ctx.query = next.query;
      ctx.state = next;
      return next;
    },
    navigate: (href, o) => state.navigate(href, o),
    reload: () => mount(state.current(), { keepScroll: true }),
    carChanged: (c) => frame.carChanged(c),
  };
  rec.ctx = ctx;
  docTitle(typeof mod.title === 'function' ? mod.title(ctx) : fullName(info), car?.name);

  try {
    const inst = await mod.mount(ctx);
    if (!alive()) { try { inst?.unmount?.(); } catch { /* gone */ } return; }
    rec.inst = inst || null;
  } catch (err) {
    if (api.isAbort(err) || !alive()) return;
    ctx.fail(err);
  }
  if (!alive()) return;
  swap();
  if (!keepScroll) { window.scrollTo(0, 0); if (!first) view.focus({ preventScroll: true }); }
  const target = location.hash.length > 1 ? document.getElementById(decodeURIComponent(location.hash.slice(1))) : null;
  if (target && view.contains(target)) {
    const top = document.querySelector('.dash')?.getBoundingClientRect().height || 0;
    const rem = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
    window.scrollTo(0, Math.max(0, target.getBoundingClientRect().top + window.scrollY - top - rem));
  }
  first = false;
  showWaitingToast();
}
