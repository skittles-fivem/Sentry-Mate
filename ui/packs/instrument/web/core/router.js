// The router: turns the address into a mounted page.
//
// shared/state.js owns the address (page, car, drive, charge, period, query). On every change this
// module decides whether the page must be built again (another page, car, drive, charge or period, or a
// query change it did not make itself), then:
//   1. asks core/session.js where the page may be shown (set-up first, then sign-in, then the page);
//   2. loads the car for car pages (GET /cars/{car}); an unknown or hidden car shows "not found";
//   3. works out the period in the car's time zone (the URL's, else the tab's, else the page's default);
//   4. imports pages/<id>.js (and links pages/<id>.css while the page is mounted, if it says css = true);
//   5. calls mount(ctx). The page draws into ctx.view and calls ctx.ready() after its first render with
//      data, or ctx.fail(err) when it cannot load. See core/README.md for ctx.
// Leaving a page cancels everything it started (ctx.api, ctx.on, ctx.every, ctx.events, ctx.onCleanup).

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { h, replace, ready as markReady, failed as markFailed, unready, setTitle as docTitle, on } from '../shared/dom.js';
import { resolvePeriod, periodParams } from '../shared/time.js';
import { pageInfo } from './pages.js';
import * as frame from './frame.js';
import * as session from './session.js';
import { errorState, statePanel, loadingPage } from './states.js';
import { showPending } from './flash.js';
import { getMe, getPrefs, isAdmin } from './prefs.js';

const BARE = new Set(['sign-in', 'setup']);
let mounted = null;      // { key, queryKey, ctx, instance, cleanups, scope, cssHref }
let seq = 0;
let firstMount = true;

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

function unmountCurrent() {
  if (!mounted) return;
  const m = mounted;
  mounted = null;
  try { m.instance?.unmount?.(); } catch (e) { console.error(e); }
  for (const f of m.cleanups.splice(0).reverse()) { try { f(); } catch (e) { console.error(e); } }
  m.scope.abort();
}

// pages/<id>.css, linked while the page is mounted; resolves when it has loaded (no unstyled flash)
function pageCss(id) {
  const href = `/pages/${id}.css`;
  let link = document.head.querySelector(`link[data-page-css="${id}"]`);
  if (link) return Promise.resolve(href);
  link = h('link', { rel: 'stylesheet', href, dataset: { pageCss: id } });
  const done = new Promise((resolve) => {
    link.addEventListener('load', () => resolve(href), { once: true });
    link.addEventListener('error', () => resolve(href), { once: true });
  });
  document.head.append(link);
  return done;
}
function dropCss(keep) {
  for (const l of document.head.querySelectorAll('link[data-page-css]')) if (l.dataset.pageCss !== keep) l.remove();
}

async function mount(s, { keepScroll = false } = {}) {
  const my = ++seq;
  unmountCurrent();
  unready();
  const id = s.page;
  const view = frame.view();
  const info = pageInfo(id);
  const scope = api.scope();
  const cleanups = [];
  const rec = { key: routeKey(s), queryKey: queryKey(s.query), ctx: null, instance: null, mod: null, cleanups, scope };
  mounted = rec;
  const alive = () => mounted === rec && my === seq;

  document.documentElement.dataset.screen = BARE.has(id) ? 'bare' : 'app';
  view.setAttribute('aria-busy', 'true');

  const showError = (err, retry = () => mount(state.current(), { keepScroll: true })) => {
    if (!alive()) return;
    replace(view, statePanel(errorState(err, { retry, home: id !== 'fleet' && !BARE.has(id) })));
    view.removeAttribute('aria-busy');
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
    setFallbackZone(car.time_zone);
  } else if (info.period && state.lastCar()) {
    // a page with a period but no car (Travel): days are the last car's days
    try {
      const last = await scope.get(api.apiPath`cars/${state.lastCar()}`, null, { maxAgeMs: 60000 });
      setFallbackZone(last.time_zone);
    } catch (err) {
      if (api.isAbort(err) || !alive()) return;
      if (err.status === 404) state.forgetCar(state.lastCar());
    }
    if (!alive()) return;
  }
  const tz = car?.time_zone || fallbackZone;
  const period = info.period ? resolvePeriod(s.period ?? { preset: info.period }, { now: api.now(), tz, fallback: info.period }) : null;

  frame.update({ s, info, car, period, tz });

  // the page module and its style sheet
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
      view.removeAttribute('aria-busy');
      markReady(id);
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
  };
  rec.ctx = ctx;

  docTitle(typeof mod.title === 'function' ? mod.title(ctx) : info.title, car?.name);
  frame.setCrumbs(frame.defaultCrumbs(s, info, car));

  try {
    const inst = await mod.mount(ctx);
    if (!alive()) { try { inst?.unmount?.(); } catch { /* left already */ } return; }
    rec.instance = inst || null;
  } catch (err) {
    if (api.isAbort(err) || !alive()) return;
    ctx.fail(err);
  }
  if (!alive()) return;
  if (!keepScroll) {
    window.scrollTo(0, 0);
    if (!firstMount) view.focus({ preventScroll: true });
  }
  // a link to a section of a page (/cars/c1/charges?…#habits): open the page there
  const target = location.hash.length > 1 ? document.getElementById(decodeURIComponent(location.hash.slice(1))) : null;
  if (target && view.contains(target)) {
    const bar = document.querySelector('.topbar')?.getBoundingClientRect().height || 0; // the sticky top bar
    const rem = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
    window.scrollTo(0, Math.max(0, target.getBoundingClientRect().top + window.scrollY - bar - rem));
  }
  firstMount = false;
  showPending();
}
