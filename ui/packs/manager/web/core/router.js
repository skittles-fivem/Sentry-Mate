// The router: turns the address into a mounted page.
//
// shared/state.js owns the address (page, car, drive, charge, period, query). On every change this
// module decides whether the page must be built again (another page, car, drive, charge or period, or a
// query change it did not make itself), then:
//   1. asks core/session.js where the page may be shown (set-up first, then sign-in, then the page);
//   2. loads the car for car pages (GET /cars/{car}); an unknown or hidden car shows "not found";
//   3. works out the period in the car's time zone (the URL's, else the tab's, else the page's default);
//   4. imports pages/<id>.js (and links pages/<id>.css while the page is mounted, if it says css = true) with
//      shared/build.js's loadPage: after a server update the tab loads the address in full instead, and a
//      file that fails is tried once more that way before the page says page_failed;
//   5. calls mount(ctx). The page draws into ctx.view and calls ctx.ready() after its first render with
//      data, or ctx.fail(err) when it cannot load. See core/README.md for ctx.
// Leaving a page cancels everything it started (ctx.api, ctx.on, ctx.every, ctx.events, ctx.onCleanup).
//
// Fast page changes: a link under the pointer (or touched, or focused) gets its page ready ahead (warm():
// the module, the car and the page's own first reads, see prefetch() in core/README.md), and once the first
// page is drawn the main sections' modules load while the browser is idle. A page that may show kept answers
// (not the settings forms) takes them at once for its first reads; <html data-stale> marks them until they
// are checked, and a page whose answers changed is built again in place, like a live refresh.

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { loadPage, reloadPage } from '../shared/build.js';
import { h, replace, ready as markReady, failed as markFailed, unready, setTitle as docTitle, on } from '../shared/dom.js';
import { resolvePeriod, periodParams } from '../shared/time.js';
import { pageInfo } from './pages.js';
import * as frame from './frame.js';
import * as session from './session.js';
import { errorState, statePanel, loadingPage } from './states.js';
import { showPending } from './flash.js';
import { getMe, getPrefs, isAdmin } from './prefs.js';
import { holdSnap } from './snap.js';
import { watchLinks, warmModules, prefetchApi, pageSheet, pageSheetsOff, setStale } from '../shared/prefetch.js';

const BARE = new Set(['sign-in', 'setup']);
let mounted = null;      // { key, queryKey, ctx, instance, cleanups, scope, cssHref }
let seq = 0;
let firstMount = true;
let warmedMain = false;
// the main sections: their modules load when the browser is idle after the first page
const MAIN = ['car', 'drives', 'drive', 'charges', 'battery', 'statistics', 'timeline', 'fleet'];
// pages that may show kept answers at once (marked stale until checked): not forms
const takesStale = (id) => !BARE.has(id) && !/^(settings-|import$|sign-in$|setup$)/.test(id);
const pageModule = (id) => import(`../pages/${id}.js`);

const periodKey = (p) => (p ? JSON.stringify(p) : '');
const routeKey = (s) => [s.page, s.car, s.drive, s.charge, periodKey(s.period)].join('|');
const queryKey = (q) => JSON.stringify(Object.entries(q || {}).sort());

/** The zone for pages without a car: the last car's, else this browser's. */
let fallbackZone = Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
export const setFallbackZone = (tz) => { if (tz) fallbackZone = tz; };
export const defaultZone = () => fallbackZone;

/** Start following the address. Call once, after the frame exists. */
export function startRouter() {
  state.subscribe((s) => queueMicrotask(() => onState(s)));
  state.startRouting();
  watchLinks(warm);
}

// A link to s may be followed: load its page's module and style sheet, its car, and the first reads the page
// asks for (the page's own prefetch(ctx), with the same ctx mount() will get), so it draws at once.
async function warm(s) {
  const id = s.page;
  const info = pageInfo(id);
  if (!info || BARE.has(id)) return;
  const modP = pageModule(id);
  let car = null;
  if (s.car) {
    // a car page may show a map: its tile settings, as core/map.js reads them
    api.prefetch('maps/config', null, { maxAgeMs: 300000 });
    car = await api.prefetch(api.apiPath`cars/${s.car}`, null, { maxAgeMs: 15000 });
    if (!car) return;
  }
  const tz = car?.time_zone || fallbackZone;
  const period = info.period ? resolvePeriod(s.period ?? { preset: info.period }, { now: api.now(), tz, fallback: info.period }) : null;
  const mod = await modP;
  if (mod.css) pageSheet(id, new URL(`../pages/${id}.css`, import.meta.url).href, { on: false });
  if (typeof mod.prefetch !== 'function') return;
  await mod.prefetch({
    id, info, state: s, query: s.query, car, carId: s.car, tz, period, params: period ? periodParams(period) : {},
    api: prefetchApi(api), me: getMe(), prefs: getPrefs(), admin: isAdmin(), prefetching: true,
  });
}

/** Build the current page again (after signing in, after a change that affects everything). */
export function reload() { const s = state.current(); mount(s, { keepScroll: true }); }

function onState(s) {
  if (s !== state.current()) return; // a newer change is on its way
  if (mounted && mounted.key === routeKey(s)) {
    const qk = queryKey(s.query);
    if (qk === mounted.queryKey) return; // the same filters, or the page set them itself (ctx.setQuery)
    mounted.queryKey = qk;
    const onQuery = mounted.instance?.onQuery || mounted.mod?.onQuery;
    if (onQuery) {
      // back / forward between filters: the page updates itself, and is ready again once onQuery resolves
      const m = mounted;
      m.ctx.query = s.query;
      m.ctx.state = s;
      Promise.resolve().then(() => onQuery(s.query, m.ctx))
        .then(() => { if (mounted === m) markReady(m.ctx.id); }, (e) => { if (mounted === m) m.ctx.fail(e); });
      return;
    }
  }
  const samePage = mounted && mounted.ctx?.id === s.page && mounted.ctx?.carId === s.car;
  mount(s, { keepScroll: samePage });
}

/** One function that runs both (each may be missing), or null. */
const both = (a, b) => (a || b ? () => { a?.(); b?.(); } : null);

// keep: a live refresh builds the new page off screen first (see stageView), so the old page's own clean-up
// (charts and maps it removes) waits until the new one takes its place; its requests and events stop at once
function unmountCurrent(keep = false) {
  if (!mounted) return null;
  const m = mounted;
  mounted = null;
  m.tracker?.stop();
  m.release();
  m.scope.abort();
  const finish = () => {
    try { m.instance?.unmount?.(); } catch (e) { console.error(e); }
    for (const f of m.cleanups.splice(0).reverse()) { try { f(); } catch (e) { console.error(e); } }
  };
  if (keep) return finish;
  finish();
  return null;
}

// A live refresh (ctx.refresh: a drive or charge started or ended) builds the page again in an invisible box of
// the view's size, and puts it in the view in one step once it is drawn and has stopped changing: the old page
// stays on screen until then, so nothing goes away, shows a loading skeleton, and comes back.
let staged = null;   // { stage, commit, drop }
// keepOld: a newer live refresh takes over, and the page still on screen is cleaned up when that one commits
function dropStage(keepOld = false) { const f = staged?.drop(keepOld) || null; staged = null; return f; }
function stageView(view, finishOld) {
  // the old page's clean-up is let go of once it is handed on or run: the new page keeps this stage as long as it
  // lives, and would keep the page before it, and that one the page before, each with everything it drew
  const takeOld = () => { const f = finishOld; finishOld = null; return f; };
  const r = view.getBoundingClientRect();
  const stage = h('div', { class: view.className + ' view-stage', 'aria-hidden': 'true', inert: '' });
  Object.assign(stage.style, {
    position: 'fixed', top: '0', left: `${r.left}px`, width: `${r.width}px`, boxSizing: 'border-box', height: '100vh', overflow: 'hidden',
    visibility: 'hidden', pointerEvents: 'none', zIndex: '-1',
  });
  document.body.append(stage);
  let done = false, quietT = 0, mo = null;
  const maxT = setTimeout(() => commit(), 12000);
  const end = () => { done = true; clearTimeout(maxT); clearTimeout(quietT); mo?.disconnect(); stage.remove(); if (staged?.stage === stage) staged = null; };
  function commit() {
    if (done) return;
    const nodes = [...stage.childNodes];
    end();
    // the control that had the focus: the same place in the new page gets it back
    const path = [];
    for (let e = document.activeElement; e && e !== view && view.contains(e); e = e.parentElement) path.unshift([...e.parentElement.children].indexOf(e));
    view.replaceChildren(...nodes);
    setStale(false); // the page built again has checked answers only
    takeOld()?.();
    if (path.length) {
      let e = view;
      for (const i of path) e = e?.children[i];
      e?.focus?.({ preventScroll: true });
    }
  }
  // after ready: wait until the new page has stopped changing (charts drawn, late parts loaded)
  // and has no request of its own under way (a route or a list that loads after ready), at most 6 s
  function settle(busy = () => 0) {
    if (done) return;
    const later = setTimeout(() => commit(), 6000);
    const again = () => {
      clearTimeout(quietT);
      quietT = setTimeout(() => { if (busy() > 0) { again(); return; } clearTimeout(later); commit(); }, 200);
    };
    mo = new MutationObserver(again);
    mo.observe(stage, { subtree: true, childList: true, attributes: true, characterData: true });
    again();
  }
  const drop = (keepOld) => { if (done) return null; end(); const f = takeOld(); if (keepOld) return f; f?.(); return null; };
  staged = { stage, commit, settle, drop };
  return staged;
}

// A page's API client that also counts its requests under way (a live refresh waits for them, see stageView).
// With a tracker its reads may take kept answers at once until the page is drawn (api.staleTracker).
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

// pages/<id>.css: on while the page is mounted, off (kept) once it is left, see pageSheet(); resolves when it has
// loaded (no unstyled flash)
function pageCss(id) { return pageSheet(id, new URL(`../pages/${id}.css`, import.meta.url).href); }
const dropCss = pageSheetsOff;

async function mount(s, { keepScroll = false, live = false } = {}) {
  const my = ++seq;
  // (the clean-up of what was there is handed to the stage alone: this page's own functions must not hold it)
  let finishOld = both(dropStage(live), unmountCurrent(live));
  if (!live) { unready(); setStale(false); }
  const id = s.page;
  const st = live ? stageView(frame.view(), finishOld) : null;
  finishOld = null;
  const view = st ? st.stage : frame.view();
  const shown = () => { if (st) st.commit(); };
  const info = pageInfo(id);
  // kept answers at once for the first reads (after a reload: the service worker's); the page is built again
  // in place when one of them changed
  const tracker = !live && takesStale(id) ? api.staleTracker({
    worker: firstMount,
    onStale: () => { if (alive()) setStale(true); },
    onFresh: ({ changed, failed }) => {
      if (!alive() || failed) return;
      if (!changed) setStale(false);
      else if (rec.ctx) rec.ctx.refresh();
      else rec.changed = true; // the page gets it once it is there
    },
  }) : null;
  const scope = counted(api.scope(), tracker);
  const cleanups = [];
  // the page is built in one go from kept answers: the whole-pixel snap runs once, when it is drawn (core/snap.js)
  const rec = { key: routeKey(s), queryKey: queryKey(s.query), ctx: null, instance: null, mod: null, cleanups, scope, tracker, release: holdSnap() };
  mounted = rec;
  const alive = () => mounted === rec && my === seq;

  document.documentElement.dataset.screen = BARE.has(id) ? 'bare' : 'app';
  view.setAttribute('aria-busy', 'true');

  let shownCar = null; // the car once loaded: the trail of a page that fails before it is built names it
  const showError = (err, retry = () => mount(state.current(), { keepScroll: true })) => {
    if (!alive()) return;
    // failed before the page was built: the trail and the title are this page's, never the one before
    if (!rec.ctx && !live) {
      frame.setCrumbs(frame.defaultCrumbs(s, info || null, shownCar));
      if (info && id !== 'not-found') docTitle(info.title, shownCar?.name);
    }
    replace(view, statePanel(errorState(err, { retry, home: id !== 'fleet' && !BARE.has(id) })));
    rec.release();
    shown();
    frame.view().removeAttribute('aria-busy');
    markFailed(id === 'not-found' ? 'not-found' : id);
  };

  // where may this page be shown?
  let gate;
  try {
    gate = await session.gate(id, s.path + (location.search || ''));
  } catch (err) {
    frame.update({ s, info, car: null, period: null });
    showError(err);
    return;
  }
  if (!alive()) return;
  if (gate.redirect) { state.navigate(gate.redirect, { replace: true }); return; }

  // a section's own short address (/trips, /charging, /stats, /settings ...): its last tab, or its first
  const sec = id === 'not-found' && /^\/(car|trips|charging|battery|stats|settings)\/?$/.exec(s.path);
  if (sec) { state.navigate(state.linkTo(frame.sectionPage(sec[1])), { replace: true }); return; }

  if (id === 'not-found' || !info) {
    frame.update({ s, info: null, car: null, period: null });
    docTitle('Not found');
    showError(Object.assign(new api.ApiError({ status: 404, code: 'not_found', message: 'There is no page at this address.' })), null);
    return;
  }

  if (!keepScroll || firstMount) replace(view, loadingPage(info.loading ?? 3));

  // the car of a car page
  let car = null;
  if (s.car) {
    try {
      car = await scope.get(api.apiPath`cars/${s.car}`, null, { maxAgeMs: 15000 });
    } catch (err) {
      if (api.isAbort(err) || !alive()) return;
      if (err.status === 404) state.forgetCar(s.car);
      frame.update({ s, info, car: null, period: null });
      docTitle(info.title);
      showError(err.status === 404 ? Object.assign(err, { message: 'There is no car at this address, or your account may not see it.' }) : err);
      return;
    }
    if (!alive()) return;
    shownCar = car;
    state.rememberCar(car.id, { sold: state.isSold(car) }); // the car opened: the one to show next time
    setFallbackZone(car.time_zone);
  } else if (info.period) {
    // a page with a period but no car (Travel): days are the preferred car's days (Main car, else the last opened)
    try {
      const p = await scope.page('cars', { sort: 'custom', limit: 50 }, { maxAgeMs: 60000 });
      const id = state.preferredCar(p.items);
      const pick = p.items.find((c) => c.id === id);
      if (pick?.time_zone) setFallbackZone(pick.time_zone);
    } catch (err) {
      if (api.isAbort(err) || !alive()) return;
    }
    if (!alive()) return;
  }
  const tz = car?.time_zone || fallbackZone;
  const period = info.period ? resolvePeriod(s.period ?? { preset: info.period }, { now: api.now(), tz, fallback: info.period }) : null;

  frame.update({ s, info, car, period, tz });

  // the page module and its style sheet
  let mod;
  try {
    mod = await loadPage(async () => {
      const m = await pageModule(id);
      if (m.css) await pageCss(id);
      return m;
    });
  } catch (err) {
    if (!alive()) return;
    console.error(err);
    showError(err, () => reloadPage());
    return;
  }
  if (!alive()) return;
  dropCss(mod.css ? id : null);
  rec.mod = mod;

  let shownOnce = false, refreshAsked = false;
  const ctx = {
    id, info, view, state: s, query: s.query, car, carId: s.car, tz, period,
    params: period ? periodParams(period) : {},
    api: scope, me: getMe(), prefs: getPrefs(), admin: isAdmin(),
    alive,
    onCleanup(f) { cleanups.push(f); return f; },
    on(target, type, f, opts) { const off = on(target, type, f, opts); cleanups.push(off); return off; },
    every(ms, f) { const t = setInterval(() => { if (alive()) f(); }, ms); cleanups.push(() => clearInterval(t)); return t; },
    events(types, f) { return scope.events((ev) => { if (alive()) f(ev); }, { types: [].concat(types) }); },
    ready() {
      if (!alive()) return;
      tracker?.close();
      if (!warmedMain) { warmedMain = true; warmModules(MAIN.map((m) => () => pageModule(m))); }
      shownOnce = true;
      if (refreshAsked) { refreshAsked = false; setTimeout(() => ctx.refresh(), 0); }
      st?.settle(() => scope.busy);
      frame.view().removeAttribute('aria-busy');
      // at the next frame (the page's own code after ready() and the router's are done by then), once the fonts the
      // page uses are in (a late swap would re-flow the text it just drew)
      requestAnimationFrame(() => {
        rec.release(); // what the page drew after ready() too: one snap, before the paint
        const fonts = document.fonts;
        if (fonts && fonts.status === 'loading') fonts.ready.then(() => { if (alive()) markReady(id); });
        else if (alive()) markReady(id);
      });
    },
    fail(err, opts = {}) {
      if (api.isAbort(err) || !alive()) return;
      console.warn(err);
      showError(err, opts.retry === undefined ? undefined : opts.retry);
    },
    setTitle(...parts) { docTitle(...parts, car?.name); },
    setCrumbs(list) { frame.setCrumbs(list); },
    link: (page, params) => state.linkTo(page, params),
    // The page's own filters: the router knows them at once, so it does not build the page again; a page
    // that was left changes nothing (a late timer must not write into the next page's address).
    setQuery(patch, opts) {
      if (!alive()) return state.current();
      const next = state.setQuery(patch, opts);
      rec.queryKey = queryKey(next.query);
      ctx.query = next.query;
      ctx.state = next;
      return next;
    },
    navigate: (href, opts) => state.navigate(href, opts),
    reload: () => mount(state.current(), { keepScroll: true }),
    // live data changed (a 'rows' event): build the page again without taking the one on screen away first
    // (asked before this page was ready: once it is, so a slow first load is never started over and over)
    refresh: () => {
      if (!alive()) return;
      if (!shownOnce) { refreshAsked = true; return; }
      mount(state.current(), { keepScroll: true, live: true });
    },
  };
  rec.ctx = ctx;
  if (rec.changed) ctx.refresh();

  docTitle(typeof mod.title === 'function' ? mod.title(ctx) : info.title, car?.name);
  // (a live refresh keeps the trail on screen: the page sets its own again, so it never flips to the usual one)
  if (!live) frame.setCrumbs(frame.defaultCrumbs(s, info, car));

  try {
    const inst = await mod.mount(ctx);
    if (!alive()) { try { inst?.unmount?.(); } catch { /* left already */ } return; }
    rec.instance = inst || null;
  } catch (err) {
    if (api.isAbort(err) || !alive()) return;
    ctx.fail(err);
  }
  if (!alive()) return;
  requestAnimationFrame(rec.release); // (a page that never called ready())
  if (live) { showPending(); return; }
  if (!keepScroll) {
    window.scrollTo(0, 0);
    if (!firstMount) view.focus({ preventScroll: true });
  }
  // a link to a section of a page (/cars/c1/charges?…#habits): open the page there
  const target = location.hash.length > 1 ? document.getElementById(decodeURIComponent(location.hash.slice(1))) : null;
  if (target && view.contains(target)) {
    const tb = document.querySelector('.topbar');
    const bar = tb && getComputedStyle(tb).position === 'sticky' ? tb.getBoundingClientRect().height : 0; // a sticky top bar
    const rem = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
    window.scrollTo(0, Math.max(0, target.getBoundingClientRect().top + window.scrollY - bar - rem));
  }
  firstMount = false;
  showPending();
}
