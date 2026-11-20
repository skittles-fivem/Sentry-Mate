// The router: the address -> a mounted page.
//
// shared/state.js owns the address (page, car, drive, charge, period, query). On a change that needs a new
// page (another page, car, drive, charge or period, or filters the page did not set itself) it:
//   1. asks core/session.js where the page may be shown (set-up, then sign-in, then the page);
//   2. loads the car of a car page (GET /cars/{car}); an unknown or hidden car is "not found";
//   3. works out the period in the car's zone (the address's, else this tab's, else the page's default);
//   4. imports pages/<id>.js (and links pages/<id>.css while it is shown, when the module says css = true);
//   5. calls mount(ctx); the page draws into ctx.view and calls ctx.ready() once every table and chart of
//      its first screen has drawn, or ctx.fail(err). See core/README.md for ctx.
// Leaving a page cancels everything it started.

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { h, replace, ready as markReady, failed as markFailed, unready, setTitle as docTitle, on } from '../shared/dom.js';
import { resolvePeriod, periodParams } from '../shared/time.js';
import { pageInfo } from './pages.js';
import * as frame from './frame.js';
import * as session from './session.js';
import { errorBlock, loadingPage } from './states.js';
import { showPending } from './toast.js';
import { getMe, getPrefs, isAdmin } from './prefs.js';
import { resetRows } from './keys.js';

let mounted = null;
let seq = 0;
let first = true;

const periodKey = (p) => (p ? JSON.stringify(p) : '');
const routeKey = (s) => [s.page, s.car, s.drive, s.charge, periodKey(s.period)].join('|');
const queryKey = (q) => JSON.stringify(Object.entries(q || {}).sort());

let fallbackZone = Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
/** The zone for pages without a car: the last car's, else this browser's. */
export const defaultZone = () => fallbackZone;

export function startRouter() {
  state.subscribe((s) => queueMicrotask(() => onState(s)));
  state.startRouting();
}

/** Build the current page again. */
export function reload() { mount(state.current(), { keepScroll: true }); }

function onState(s) {
  if (s !== state.current()) return;
  if (mounted && mounted.key === routeKey(s)) {
    const qk = queryKey(s.query);
    if (qk === mounted.queryKey) return;
    mounted.queryKey = qk;
    const onQuery = mounted.instance?.onQuery || mounted.mod?.onQuery;
    if (onQuery) {
      const m = mounted;
      m.ctx.query = s.query;
      m.ctx.state = s;
      Promise.resolve().then(() => onQuery(s.query, m.ctx))
        .then(() => { if (mounted === m) markReady(m.ctx.id); }, (e) => { if (mounted === m) m.ctx.fail(e); });
      return;
    }
  }
  mount(s, { keepScroll: !!mounted && mounted.ctx?.id === s.page && mounted.ctx?.carId === s.car });
}

function unmountCurrent() {
  if (!mounted) return;
  const m = mounted;
  mounted = null;
  try { m.instance?.unmount?.(); } catch (e) { console.error(e); }
  for (const f of m.cleanups.splice(0).reverse()) { try { f(); } catch (e) { console.error(e); } }
  m.scope.abort();
}

function pageCss(id) {
  if (document.head.querySelector(`link[data-page-css="${id}"]`)) return Promise.resolve();
  const link = h('link', { rel: 'stylesheet', href: `/pages/${id}.css`, dataset: { pageCss: id } });
  const done = new Promise((resolve) => { link.addEventListener('load', resolve, { once: true }); link.addEventListener('error', resolve, { once: true }); });
  document.head.append(link);
  return done;
}
const dropCss = (keep) => { for (const l of document.head.querySelectorAll('link[data-page-css]')) if (l.dataset.pageCss !== keep) l.remove(); };

async function mount(s, { keepScroll = false } = {}) {
  const my = ++seq;
  unmountCurrent();
  unready();
  resetRows();
  const id = s.page;
  const view = frame.view();
  const info = pageInfo(id);
  const scope = api.scope();
  const cleanups = [];
  const rec = { key: routeKey(s), queryKey: queryKey(s.query), ctx: null, instance: null, mod: null, cleanups, scope };
  mounted = rec;
  const alive = () => mounted === rec && my === seq;

  document.documentElement.dataset.screen = info?.bare ? 'bare' : 'app';
  view.setAttribute('aria-busy', 'true');

  const showError = (err, retry = () => mount(state.current(), { keepScroll: true })) => {
    if (!alive()) return;
    replace(view, h('div', { class: 'page' }, h('header', { class: 'ph' }, h('h1', { class: 'ph-title' }, info?.name || 'not found')), errorBlock(err, { retry, home: id !== 'fleet' && !info?.bare })));
    view.removeAttribute('aria-busy');
    markFailed(id || 'not-found');
  };

  let gate;
  try {
    gate = await session.gate(id, s.path + (location.search || ''));
  } catch (err) {
    document.documentElement.dataset.screen = 'app';
    frame.update({ s, info, car: null, period: null });
    showError(err);
    return;
  }
  if (!alive()) return;
  if (gate.redirect) { state.navigate(gate.redirect, { replace: true }); return; }

  if (id === 'not-found' || !info) {
    frame.update({ s, info: null, car: null, period: null });
    docTitle('Not found');
    showError(new api.ApiError({ status: 404, code: 'not_found', message: 'There is no page at this address.' }), null);
    return;
  }

  if (!keepScroll || first) replace(view, loadingPage(`loading ${info.link}`));

  let car = null;
  if (s.car) {
    try {
      car = await scope.get(api.apiPath`cars/${s.car}`, null, { maxAgeMs: 15000 });
    } catch (err) {
      if (api.isAbort(err) || !alive()) return;
      if (err.status === 404) { state.forgetCar(s.car); err.message = 'There is no car at this address, or your account may not see it.'; }
      frame.update({ s, info, car: null, period: null });
      docTitle(info.name);
      showError(err);
      return;
    }
    if (!alive()) return;
    if (car.time_zone) fallbackZone = car.time_zone;
    state.rememberCar(car.id);
  } else if (info.period && state.lastCar()) {
    try {
      const last = await scope.get(api.apiPath`cars/${state.lastCar()}`, null, { maxAgeMs: 60000 });
      if (last.time_zone) fallbackZone = last.time_zone;
    } catch (err) {
      if (api.isAbort(err) || !alive()) return;
      if (err.status === 404) state.forgetCar(state.lastCar());
    }
    if (!alive()) return;
  }
  const tz = car?.time_zone || fallbackZone;
  const period = info.period ? resolvePeriod(s.period ?? { preset: info.period }, { now: api.now(), tz, fallback: info.period }) : null;
  frame.update({ s, info, car, period, tz });

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
    ready() { if (!alive()) return; view.removeAttribute('aria-busy'); markReady(id); },
    fail(err, opts = {}) { if (api.isAbort(err) || !alive()) return; console.warn(err); showError(err, opts.retry === undefined ? undefined : opts.retry); },
    setTitle(...parts) { docTitle(...parts, car?.name); },
    link: (page, params) => state.linkTo(page, params),
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
  docTitle(typeof mod.title === 'function' ? mod.title(ctx) : info.name, car?.name);

  try {
    const inst = await mod.mount(ctx);
    if (!alive()) { try { inst?.unmount?.(); } catch { /* left */ } return; }
    rec.instance = inst || null;
  } catch (err) {
    if (api.isAbort(err) || !alive()) return;
    ctx.fail(err);
  }
  if (!alive()) return;
  if (!keepScroll) {
    window.scrollTo(0, 0);
    document.getElementById('scroller')?.scrollTo?.(0, 0);   // phones scroll inside it
    if (!first) view.focus({ preventScroll: true });
  }
  const target = location.hash.length > 1 ? document.getElementById(decodeURIComponent(location.hash.slice(1))) : null;
  if (target && view.contains(target)) {
    const bar = document.getElementById('top')?.getBoundingClientRect().bottom || 0;
    const sc = document.getElementById('scroller');
    if (sc && sc.scrollHeight > sc.clientHeight && getComputedStyle(sc).overflowY !== 'visible') target.scrollIntoView({ block: 'start' });
    else window.scrollTo(0, Math.max(0, Math.round(target.getBoundingClientRect().top + window.scrollY - bar - 8)));
    target.classList.add('is-target');
  }
  first = false;
  showPending();
}
