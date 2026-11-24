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
import { loadPage, reloadPage, isPageFailed } from '../shared/build.js';
import { scrolling, afterScroll, SCROLL_REST_MS } from './stage.js';
import { h, replace, holdScroll, builtWith, ready as markReady, failed as markFailed, unready, setTitle as docTitle, on } from '../shared/dom.js';
import { resolvePeriod, periodParams } from '../shared/time.js';
import { pageInfo } from './pages.js';
import * as frame from './frame.js';
import * as session from './session.js';
import { forgetSorts } from './table.js';
import { noticesLoaded } from './notices.js';
import { errorState, statePanel, loadingPage } from './states.js';
import { showPending } from './flash.js';
import { fieldIdsFromStart } from './form.js';
import { getMe, getPrefs, isAdmin } from './prefs.js';
import { holdSnap } from './snap.js';
import { watchLinks, warmModules, prefetchApi, pageSheet, pageSheetsOff, setStale, signedInHint } from '../shared/prefetch.js';

const BARE = new Set(['sign-in', 'setup', 'two-step-setup']);
// the main sections: their modules load when the browser is idle after the first page
const MAIN = ['car', 'drives', 'drive', 'charges', 'battery', 'statistics', 'timeline', 'fleet'];
// pages that may show kept answers at once (marked stale until checked): not forms
// (kept answers first are on again: the first draw is built off screen and a page drawn again from the fresh answers
// is swapped in at once, and the jump check, ui/tools/jumps.js, passes with them)
const takesStale = (id) => !BARE.has(id) && !/^(settings-|import$)/.test(id);
const pageModule = (id) => import(`../pages/${id}.js`);
let mounted = null;      // { key, queryKey, ctx, instance, cleanups, scope, cssHref }
let seq = 0;
let firstMount = true;
let warmedMain = false;

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
  // the first page's module, car and first reads load while the session is checked, not after it (a signed-out
  // browser's reads come back 401 and are forgotten; the sign-in page shows as it would)
  const first = state.current();
  if (first && signedInHint()) warm(first).catch(() => {});
}

// A link to s may be followed: load its page's module and style sheet, its car, and the first reads the page
// asks for (the page's own prefetch(ctx), with the same ctx mount() will get), so it draws at once.
async function warm(s) {
  const id = s.page;
  const info = pageInfo(id);
  if (!info || BARE.has(id) || getMe()?.two_step_setup_only) return; // (behind the wall nothing is read ahead)
  const modP = pageModule(id);
  // a file that fails while the car is still being read is not an error nobody handles: whoever warms waits for
  // modP below and takes its failure there (and the page itself loads the file again, see mount())
  modP.catch(() => {});
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
// The controls of a page, each with a key that stays the same when the page is built again: its name, id or label,
// and its place among the controls with that key.
const CONTROLS = 'input, select, textarea';
function controlKeys(root) {
  const seen = new Map();
  return [...root.querySelectorAll(CONTROLS)].map((el) => {
    const k = `${el.tagName}|${el.type}|${el.name || el.id || el.getAttribute('aria-label') || ''}`;
    const n = seen.get(k) || 0;
    seen.set(k, n + 1);
    return [el, `${k}|${n}`];
  });
}
// (what the page built a control with is what h() set, builtWith; the markup's defaults for a control it left alone)
function changedByReader(el) {
  if (el.type === 'file' || el.type === 'password' && !el.value) return false;
  if (el.tagName === 'SELECT') return [...el.options].some((o) => o.selected !== (builtWith(o, 'selected') ?? o.defaultSelected));
  if (el.type === 'checkbox' || el.type === 'radio') return el.checked !== (builtWith(el, 'checked') ?? el.defaultChecked);
  if (el.type === 'hidden' || el.type === 'button' || el.type === 'submit') return false;
  return el.value !== (builtWith(el, 'value') ?? el.defaultValue);
}
function readerChanges(view) {
  const active = document.activeElement;
  const out = new Map();
  for (const [el, key] of controlKeys(view)) {
    if (!changedByReader(el) && el !== active) continue;
    const c = { el, focus: el === active, changed: changedByReader(el) };
    if (el.tagName === 'SELECT') c.value = el.value;
    else if (el.type === 'checkbox' || el.type === 'radio') c.checked = el.checked;
    else { c.value = el.value; try { c.sel = [el.selectionStart, el.selectionEnd, el.selectionDirection]; } catch { /* no selection */ } }
    out.set(key, c);
  }
  return out;
}
function putBack(view, kept) {
  if (!kept.size) return;
  for (const [el, key] of controlKeys(view)) {
    const c = kept.get(key);
    if (!c || el.disabled && !c.changed) continue;
    if (c.changed) {
      const isBox = 'checked' in c;
      if (isBox ? el.checked === c.checked : el.value === c.value) { /* the new page has it already */ } else {
        if (isBox) el.checked = c.checked; else el.value = c.value;
        // the page's own handlers (a search that filters, a total that adds up) hear it as they hear the reader
        el.dispatchEvent(new Event(isBox || el.tagName === 'SELECT' ? 'change' : 'input', { bubbles: true }));
      }
    }
    if (c.focus) {
      el.focus?.({ preventScroll: true });
      if (c.sel && c.sel[0] != null) { try { el.setSelectionRange(c.sel[0], c.sel[1], c.sel[2]); } catch { /* no selection */ } }
    }
  }
}

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
    // while the page scrolls the swap waits for the scroll to rest: the new page puts its maps and charts on screen in one task
    if (scrolling()) { clearTimeout(quietT); quietT = setTimeout(commit, SCROLL_REST_MS); return; }
    const nodes = [...stage.childNodes];
    end();
    // what the reader typed or chose in the page on screen is put into the same controls of the new page
    const kept = readerChanges(view);
    // the control that had the focus: the same place in the new page gets it back
    const path = [];
    for (let e = document.activeElement; e && e !== view && view.contains(e); e = e.parentElement) path.unshift([...e.parentElement.children].indexOf(e));
    holdScroll(view);
    view.replaceChildren(...nodes);
    putBack(view, kept);
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

// A page drawn for the first time (not a live refresh) is built in an invisible box of the view's size too, while the
// view shows the loading skeleton, and put in the view in one step when it says it is ready (or after FIRST_MS, a
// slow page: what it draws after that is drawn in place). Its parts come in one by one as their answers arrive; on
// screen they would each push what is under them down (a phone: the page jumps while it loads). Charts and maps
// that land near the screen are drawn in the box; the rest when they come near once in place (shared/lazy.js).
const FIRST_MS = 3000;
const NOTICES_MS = 3000;
function firstStage(view, onCommit) {
  // drawn in the view itself, hidden (with the footer) while it is built, under a copy of the skeleton laid over it:
  // the page lays itself out part by part as on screen, and putting it in place is only showing it (moving a whole
  // page in from a box laid it all out again at once, and a live update that came in meanwhile was drawn over the
  // answers it was ready with)
  const root = document.documentElement;
  const r = view.getBoundingClientRect();
  const cover = h('div', { class: view.className + ' view-cover', 'aria-hidden': 'true' }, [...view.childNodes].map((n) => n.cloneNode(true)));
  Object.assign(cover.style, {
    position: 'absolute', top: `${r.top + window.scrollY}px`, left: `${r.left + window.scrollX}px`, width: `${r.width}px`, boxSizing: 'border-box',
    minHeight: '0', pointerEvents: 'none', zIndex: '1',
  });
  document.body.append(cover);
  root.setAttribute('data-building', '');
  let done = false;
  const maxT = setTimeout(() => commit(), FIRST_MS);
  const end = () => { done = true; clearTimeout(maxT); cover.remove(); root.removeAttribute('data-building'); };
  function commit() {
    if (done) return;
    end();
    onCommit();
  }
  const drop = () => { if (!done) end(); };
  return { stage: view, commit, drop };
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

// wall: the account may only set two-step sign-in up (core/session.js gate): its page is shown at every address
async function mount(s, { keepScroll = false, live = false, wall = false } = {}) {
  const my = ++seq;
  // the wall page is no car's and no period's: it loads nothing of the address it stands at
  if (wall) s = { ...s, car: null, period: null };
  // (the clean-up of what was there is handed to the stage alone: this page's own functions must not hold it)
  let finishOld = both(dropStage(live), unmountCurrent(live));
  if (!live) { unready(); setStale(false); forgetSorts(); }
  // the page builds itself again for its own address (Places' list and its editor): the control that had the
  // focus goes away with the old drawing (or lost it already, a Save button disabled while its request ran), and
  // the focus must not be left on <body>: the page gets it, as on a move to another page
  const had = !live && keepScroll ? document.activeElement || document.body : null;
  const focusWas = had && (had === document.body || frame.view().contains(had)) ? had : null;
  const focusBack = () => {
    if (!focusWas || !alive() || (focusWas !== document.body && focusWas.isConnected)) return;
    if (!document.activeElement || document.activeElement === document.body) frame.view().focus({ preventScroll: true });
  };
  const id = wall ? 'two-step-setup' : s.page;
  const st = live ? stageView(frame.view(), finishOld) : null;
  finishOld = null;
  let view = st ? st.stage : frame.view();
  let first = null; // the first draw's box (firstStage), once the page module is in
  const shown = () => { if (st) st.commit(); first?.commit(); };
  const info = pageInfo(id);
  // the dock's period bar is there from the first paint on a page with a period (core/frame.js fills it in)
  if (!live) document.documentElement.toggleAttribute('data-has-period', !!info?.period);
  // the first page of a visit waits for the notices bar (at most NOTICES_MS) before it shows its skeleton and itself:
  // a bar that came in after them would push the whole page down
  const noticesIn = firstMount && !BARE.has(id) ? Promise.race([noticesLoaded, new Promise((r) => { setTimeout(r, NOTICES_MS); })]) : null;
  // kept answers at once for the first reads (after a reload: the service worker's); the page is built again
  // in place when one of them changed
  const tracker = !live && takesStale(id) ? api.staleTracker({
    // only a page the service worker serves: with none (the first visit, a browser without one) the worker's
    // answer would be a second request for the same read to the server
    worker: firstMount && !!globalThis.navigator?.serviceWorker?.controller,
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
      // (what is not there says so: the tab is not named after a page that is not shown)
      if (info && id !== 'not-found') docTitle(err?.status === 404 ? 'Not found' : info.title, shownCar?.name);
    }
    replace(view, statePanel(errorState(err, { retry, page: true, home: id !== 'fleet' && !BARE.has(id) })));
    rec.release();
    shown();
    frame.view().removeAttribute('aria-busy');
    focusBack();
    markFailed(id === 'not-found' ? 'not-found' : id);
  };

  // where may this page be shown?
  let gate;
  try {
    gate = await session.gate(id, s.path + (location.search || '') + (location.hash || '')); // the section too (/help#licences)
  } catch (err) {
    frame.update({ s, info, car: null, period: null });
    showError(err);
    return;
  }
  if (!alive()) return;
  if (gate.redirect) { state.navigate(gate.redirect, { replace: true }); return; }
  if (gate.wall) { mount(s, { keepScroll: false, live: false, wall: true }); return; }

  if (id === 'not-found' || !info) {
    frame.update({ s, info: null, car: null, period: null });
    docTitle('Not found');
    showError(Object.assign(new api.ApiError({ status: 404, code: 'not_found', message: 'There is no page at this address.' })), null);
    return;
  }

  // a new page opens at its top: once, when its skeleton (or, without one, the page) goes in, never after its mount
  // returns (a page that goes on loading after it is shown would throw the reader back up the page)
  let topped = false;
  const toTop = () => { if (keepScroll || live || topped) return; topped = true; window.scrollTo(0, 0); };
  if (noticesIn) noticesIn.then(() => { if (alive() && !frame.view().hasChildNodes()) { replace(frame.view(), loadingPage(info.loading ?? 3)); toTop(); } });
  else if (!keepScroll || firstMount) { replace(view, loadingPage(info.loading ?? 3)); toTop(); }

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
    // a file that could not load: Reload loads the address in full; no answer from the server: Try again opens the page again
    showError(err, isPageFailed(err) ? () => reloadPage() : undefined);
    return;
  }
  if (!alive()) return;
  dropCss(mod.css ? id : null);
  rec.mod = mod;
  if (!live && !BARE.has(id)) {
    // (once in place, the page's view is the view itself: what it looks up or draws later is there)
    first = firstStage(frame.view(), () => { view = frame.view(); if (rec.ctx) rec.ctx.view = view; toTop(); });
    cleanups.push(first.drop);
    view = first.stage;
  }

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
      if (!BARE.has(id)) signedInHint(true);
      if (!warmedMain) { warmedMain = true; warmModules(MAIN.map((m) => () => pageModule(m))); }
      shownOnce = true;
      if (refreshAsked) { refreshAsked = false; setTimeout(() => ctx.refresh(), 0); }
      st?.settle(() => scope.busy);
      // ready (once the fonts the page uses are in: a late swap would re-flow the text it just drew) and one snap of
      // what the page drew, after ready() too
      const settle = (markFirst) => {
        if (!alive()) return;
        focusBack();
        const fonts = document.fonts;
        const loading = fonts && fonts.status === 'loading';
        if (markFirst && !loading) markReady(id);
        rec.release();
        if (loading) fonts.ready.then(() => { if (alive()) markReady(id); });
        else if (!markFirst) markReady(id);
      };
      if (first) {
        // a first draw is put in place, said ready and snapped in one task, once the page's own code after ready() has
        // run: the snap lays out the page just put in place, and a live update that came in meanwhile would be drawn
        // over the answers the page was ready with
        Promise.resolve(noticesIn).then(() => { if (!alive()) return; first.commit(); settle(true); });
      }
      frame.view().removeAttribute('aria-busy');
      // otherwise at the next frame (the page's own code after ready() and the router's are done by then)
      // (a page built again while the page on screen scrolls: its snap, a run over the whole page, waits for the scroll to rest)
      if (!first) requestAnimationFrame(() => afterScroll(() => settle(false)));
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
    fieldIdsFromStart();
    const inst = await mod.mount(ctx);
    if (!alive()) { try { inst?.unmount?.(); } catch { /* left already */ } return; }
    rec.instance = inst || null;
  } catch (err) {
    if (api.isAbort(err) || !alive()) return;
    ctx.fail(err);
  }
  if (!alive()) return;
  // a page that never calls ready(): snapped all the same, a moment after its mount (a page that does is snapped
  // once, at ready: its mount often returns before its data is in, and every part it draws after would otherwise
  // start a whole-page snap of its own)
  setTimeout(() => afterScroll(rec.release), 2000);
  if (live) { showPending(); return; }
  if (!keepScroll) {
    toTop();
    if (!firstMount) frame.view().focus({ preventScroll: true });
  }
  // a link to a section of a page (/cars/c1/charges?…#habits): open the page there
  let target = null;
  try { target = location.hash.length > 1 ? document.getElementById(decodeURIComponent(location.hash.slice(1))) : null; } catch { /* not valid percent-encoding: no section to open */ }
  if (target && frame.view().contains(target)) {
    const bar = document.querySelector('.topbar')?.getBoundingClientRect().height || 0; // the sticky top bar
    const rem = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
    window.scrollTo(0, Math.max(0, target.getBoundingClientRect().top + window.scrollY - bar - rem));
  }
  firstMount = false;
  showPending();
}
