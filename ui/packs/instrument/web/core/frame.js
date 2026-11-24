// The app frame every page shares: the sidebar (logo, page finder, car switcher, the menu grouped by
// task, the account menu), the top bar (menu button, breadcrumb, the shared period picker, live status,
// the car's clock), the notices bar, flash notes and the footer (version, licences, release notice).
// On a phone (<html data-phone>, set by boot.js) the sidebar gives way to a dock at the bottom: the
// period bar (upright phones) and a tab bar (All cars, Overview, Drives, Charges, Menu); the top bar
// then holds the car and the account.
// The router calls update() for every page it mounts; pages never touch the frame except through ctx
// (ctx.setCrumbs, ctx.setTitle).

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { buildLabel, buildTitle } from '../shared/release.js';
import { h, replace, cover, safeUrl } from '../shared/dom.js';
import { formatTime, zoneName } from '../shared/time.js';
import { liveWords } from '../shared/live.js';
import { format } from '../shared/units.js';
import { icon, brandMark } from './icons.js';
import { GROUPS, pagesIn, pageInfo } from './pages.js';
import { periodPicker, setPeriodShown } from './period.js';
import { openFinder } from './finder.js';
import { openCarSwitcher, carLink, SWITCH_KEYS } from './carswitch.js';
import { accountMenu, toggleAccountMenu, roleText } from './account.js';
import { initNotices, loadNotices, onNotices } from './notices.js';
import { initFlash } from './flash.js';
import { applyPrefs, getMe } from './prefs.js';
import { onSession } from './session.js';
import { signedInHint } from '../shared/prefetch.js';
import { refreshAgo, stateLabel, carMeta } from './fmt.js';

const $ = (sel) => document.querySelector(sel);
const NAV_KEY = 'sentrymate.nav-collapsed';
let els = {};
let shown = { car: null, tz: null, page: null };
let stopEvents = null;
let started = false;

/** The element pages draw into (<main id="view">). */
export const view = () => els.view || $('#view');

function collapsed() {
  try { const v = JSON.parse(localStorage.getItem(NAV_KEY)); return new Set(Array.isArray(v) ? v : ['setup']); } catch { return new Set(['setup']); }
}
function setCollapsed(set) { try { localStorage.setItem(NAV_KEY, JSON.stringify([...set])); } catch { /* fine */ } }

/* ------------------------------------------------------------------ sidebar */

function buildSidebar() {
  const shut = collapsed();
  const navLinks = new Map();
  const groups = GROUPS.map((g) => {
    const body = h('div', { class: 'nav-group-body', id: `nav-${g.id}` }, pagesIn(g.id).map((p) => {
      const a = h('a', { class: 'nav-link', href: '/', dataset: { page: p.id }, title: p.desc },
        icon(p.icon), h('span', null, p.nav || p.title),
        p.id === 'fleet' ? h('span', { class: 'nav-count', id: 'nav-fleet-count' }) : null);
      if (p.id === 'places') cover(a, 'shell.nav.geofences', 'web.shell.geofences-nav');
      if (p.id === 'settings-cars') cover(a, 'shell.nav.settings', 'web.shell.settings-nav');
      navLinks.set(p.id, a);
      return a;
    }));
    const head = h('button', {
      class: 'nav-group-head', type: 'button', 'aria-expanded': String(!shut.has(g.id)), 'aria-controls': `nav-${g.id}`, dataset: { group: g.id },
      onclick: () => {
        const open = head.getAttribute('aria-expanded') !== 'true';
        head.setAttribute('aria-expanded', String(open));
        const s = collapsed();
        if (open) s.delete(g.id); else s.add(g.id);
        setCollapsed(s);
      },
    }, h('span', null, g.title), h('span', { class: 'nav-group-current', 'aria-hidden': 'true' }), icon('chev-d'));
    return h('div', { class: 'nav-group', dataset: { group: g.id } }, head, body);
  });

  const carBtn = h('button', { class: 'car-switch', type: 'button', 'aria-haspopup': 'dialog', title: 'Switch car', onclick: openCarSwitcher },
    icon('car', 'car-switch-ico'),
    h('span', { class: 'car-switch-dot', 'aria-hidden': 'true' }),
    h('span', { class: 'car-switch-text' }, h('span', { class: 'car-switch-name' }, 'Choose a car'), h('span', { class: 'car-switch-sub' }, h('span', { class: 'car-switch-sub-text' }, 'No car open'))),
    h('span', { class: 'kbd' }, SWITCH_KEYS.join(' ')));

  const acct = h('button', { class: 'account', type: 'button', 'aria-haspopup': 'dialog', 'aria-expanded': 'false', onclick: (e) => toggleAccountMenu(e.currentTarget) },
    h('span', { class: 'avatar', 'aria-hidden': 'true' }, '·'),
    h('span', { class: 'account-text' }, h('span', { class: 'account-name' }, 'Account'), h('span', { class: 'account-role' })),
    icon('chev-ud', 'account-chev'));

  const side = $('#sidebar');
  replace(side,
    h('a', { class: 'brand', href: '/', 'aria-label': 'Sentry Mate: all cars', cover: ['shell.nav.home', 'web.shell.logo-home', 'web.shell.home-routing'] },
      brandMark(), h('span', { class: 'brand-text' }, h('span', { class: 'brand-name' }, 'Sentry Mate'), h('span', { class: 'brand-sub' }, 'Car logger'))),
    h('button', { class: 'finder-btn', type: 'button', onclick: () => openFinder(), title: 'Find a page (/)' }, icon('search'), h('span', null, 'Find a page'), h('span', { class: 'kbd' }, '/')),
    h('div', { class: 'nav-section' }, h('span', { class: 'nav-label' }, 'Car'), carBtn),
    h('nav', { class: 'nav', 'aria-label': 'Pages' }, groups),
    h('div', { class: 'sidebar-foot' }, acct));
  // the account menu lives outside the sidebar, which a phone hides
  document.body.append(accountMenu());
  Object.assign(els, { navLinks, carBtn, acct });
}

function showCarInSidebar(car) {
  for (const b of [els.carBtn, els.topCar]) if (b) showCarIn(b, car);
}
function showCarIn(b, car) {
  const name = b.querySelector('.car-switch-name'), sub = b.querySelector('.car-switch-sub-text'), dot = b.querySelector('.car-switch-dot');
  if (!car) {
    name.textContent = 'Choose a car';
    sub.textContent = 'No car open';
    b.removeAttribute('data-state');
    dot.classList.remove('is-stale');
    return;
  }
  const n = car.now || {};
  b.dataset.state = n.state || 'unknown';
  name.textContent = car.name;
  const sold = state.isSold(car);
  sub.textContent = [carMeta(car), sold ? 'Sold' : n.stale ? 'last known: ' + stateLabel(n.state).toLowerCase() : stateLabel(n.state)].join(' · ');
  dot.classList.toggle('is-stale', !!n.stale || sold);
  b.setAttribute('aria-label', `Car: ${car.name}, ${stateLabel(n.state)}. Switch car`);
}

/* ------------------------------------------------------------------ top bar */

function buildTopbar() {
  const menuBtn = h('button', {
    class: 'btn btn--ghost btn--icon menu-btn', type: 'button', 'aria-label': 'Menu: every page, with a short description',
    title: 'Menu', onclick: () => openFinder(), cover: ['shell.nav.mobile_menu', 'web.shell.menu-toggle'],
  }, icon('menu'));
  const crumbs = h('nav', { class: 'crumbs', 'aria-label': 'Breadcrumb' });
  const fresh = h('span', { class: 'freshness', title: 'Pages update by themselves when the server has processed a new log. Logs arrive about a minute after the car records them.' },
    h('span', { class: 'live-dot', 'aria-hidden': 'true' }), icon('warn', 'freshness-late'), h('span', { class: 'freshness-text' }, 'Connecting'));
  const clock = h('span', { class: 'clock', 'aria-label': "The car's local time" });
  // a phone: the car and the account in the top bar (the sidebar is hidden there)
  const topCar = h('button', { class: 'car-switch top-car', type: 'button', 'aria-haspopup': 'dialog', title: 'Switch car', onclick: openCarSwitcher },
    h('span', { class: 'car-switch-dot', 'aria-hidden': 'true' }),
    h('span', { class: 'car-switch-text' }, h('span', { class: 'car-switch-name' }, 'Choose a car'), h('span', { class: 'car-switch-sub' }, h('span', { class: 'car-switch-sub-text' }, 'No car open'))),
    icon('chev-d', 'ico--sm top-car-chev'));
  const topAcct = h('button', { class: 'top-acct', type: 'button', 'aria-haspopup': 'dialog', 'aria-expanded': 'false', 'aria-label': 'Your account and display', onclick: (e) => toggleAccountMenu(e.currentTarget) },
    h('span', { class: 'avatar', 'aria-hidden': 'true' }, '·'));
  const period = periodPicker();
  const tools = h('div', { class: 'topbar-tools' }, period, fresh, clock, topAcct);
  replace($('#topbar'),
    h('div', { class: 'topbar-start' }, menuBtn, topCar, crumbs),
    tools);
  Object.assign(els, { crumbs, fresh, clock, topCar, topAcct, period, tools });
}

/* ------------------------------------------------------------------ the phone dock */

// the pages in the tab bar; the last tab opens the menu with every page
const TABS = [
  { page: 'fleet', label: 'All cars', icon: 'fleet' },
  { page: 'car', label: 'Overview', icon: 'gauge' },
  { page: 'drives', label: 'Drives', icon: 'route' },
  { page: 'charges', label: 'Charges', icon: 'bolt' },
];

function buildDock() {
  const dock = $('#dock');
  if (!dock) return;
  const tabs = new Map();
  const list = TABS.map((t) => {
    const a = h('a', { class: 'tab', href: '/', dataset: { page: t.page } }, icon(t.icon), h('span', { class: 'tab-label' }, t.label));
    tabs.set(t.page, a);
    return a;
  });
  const more = h('button', { class: 'tab tab--menu', type: 'button', 'aria-haspopup': 'dialog', onclick: () => openFinder() },
    icon('menu'), h('span', { class: 'tab-label' }, 'Menu'));
  const periodSlot = h('div', { class: 'dock-period' });
  replace(dock, periodSlot, h('nav', { class: 'tabbar', 'aria-label': 'Main pages' }, list, more));
  Object.assign(els, { tabs, periodSlot, more });
  placePeriod();
  window.addEventListener('phonelayout', placePeriod);
}

// an upright phone: the period bar sits in the dock, in reach of the thumb; otherwise in the top bar
function placePeriod() {
  const p = els.period;
  if (!p || !els.periodSlot) return;
  const inDock = document.documentElement.dataset.phone === 'tall';
  if (inDock && p.parentNode !== els.periodSlot) els.periodSlot.append(p);
  else if (!inDock && p.parentNode !== els.tools) els.tools.prepend(p);
}

function showTabs(activePage) {
  if (!els.tabs) return;
  let any = false;
  for (const [id, a] of els.tabs) {
    carLink(a, id);
    const on = id === activePage;
    any ||= on;
    if (on) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current');
  }
  els.more.classList.toggle('is-current', !any && !!activePage);
}

let freshTitle = null;   // the dot's own explanation, the state's words put before it
function showFreshness(info) {
  const f = els.fresh;
  if (!f) return;
  const t = f.querySelector('.freshness-text');
  const late = info.state === 'reconnecting' || info.state === 'closed' || info.state === 'unsupported';
  f.classList.toggle('is-late', late);
  f.hidden = info.state === 'signed_out';
  // while the stream is down the pages read their figures every few seconds instead: the dot says which it is
  const w = liveWords(info);
  f.dataset.live = w.tone;
  t.textContent = info.state === 'open' ? 'Live' : info.state === 'connecting' ? 'Connecting'
    : info.state === 'reconnecting' ? `Reconnecting${info.retryInMs ? ` in ${Math.ceil(info.retryInMs / 1000)} s` : ''}`
      : info.state === 'unsupported' ? w.text : 'Not live: reload to update';
  freshTitle ??= f.title;
  f.title = info.state === 'open' ? freshTitle : `${w.text}. ${freshTitle}`;
}

function tickClock() {
  if (!els.clock) return;
  const tz = shown.tz;
  els.clock.hidden = !tz;
  if (tz) els.clock.textContent = `${formatTime(api.now(), tz)} ${zoneName(api.now(), tz)}`;
}

/** The breadcrumb: [{ label, href }]; the last one is the page itself. */
export function setCrumbs(list) {
  if (!els.crumbs) return;
  const out = [];
  list.filter(Boolean).forEach((c, i, all) => {
    if (i) out.push(h('span', { class: 'sep', 'aria-hidden': 'true' }, '/'));
    out.push(i === all.length - 1 || !c.href ? h('span', { 'aria-current': i === all.length - 1 ? 'page' : null }, c.label) : h('a', { href: c.href }, c.label));
  });
  replace(els.crumbs, out);
}

/** A page changed a car (renamed it, moved its time zone): the sidebar card and the clock follow at once. */
export function carChanged(car) {
  if (!car || shown.car?.id !== car.id) return;
  shown.car = { ...shown.car, ...car };
  if (car.time_zone) shown.tz = car.time_zone;
  tickClock();
  showCarInSidebar(shown.car);
}

/** The usual breadcrumb for a page: All cars / car / page. */
export function defaultCrumbs(s, info, car) {
  if (!info) return [{ label: 'Not found' }];
  const fleet = { label: 'All cars', href: '/' };
  if (info.id === 'fleet') return [{ label: 'All cars' }];
  const group = GROUPS.find((g) => g.id === info.group);
  const carCrumb = car ? { label: car.name, href: state.linkTo('car', { car: car.id }) } : null;
  if (info.id === 'car') return [fleet, { label: car?.name || 'Car' }];
  if (info.parent) {
    const parent = pageInfo(info.parent);
    return [fleet, carCrumb, { label: parent.title, href: state.linkTo(info.parent) }, { label: info.title }];
  }
  if (car) return [fleet, carCrumb, { label: info.nav || info.title }];
  return [group && group.id !== 'cars' ? { label: group.title } : fleet, { label: info.title }];
}

/* ------------------------------------------------------------------ footer */

function buildFooter() {
  const version = h('span', { class: 'footer-version' }, 'Sentry Mate');
  const update = h('a', {
    class: 'footer-update', hidden: true, target: '_blank', rel: 'noopener noreferrer external',
    cover: ['shell.footer.update_available', 'web.shell.update-footer'],
  }, icon('arrow-r'), h('span', null, 'A newer version is out'));
  const legal = h('span', { class: 'footer-legal', cover: ['shell.footer.legal', 'web.shell.legal-footer'] },
    'Bundled parts keep their own licences: MapLibre GL JS (BSD-3-Clause), uPlot (MIT), Inter (SIL Open Font License 1.1) and JetBrains Mono (Apache-2.0). ',
    h('a', { href: '/help#licences' }, 'Licences and notices'), ' · ',
    h('a', { href: '/help#credits' }, 'Data credits'));
  replace($('#footer'), version, legal, h('span', { class: 'grow' }), update);
  Object.assign(els, { version, update });
}

async function loadAbout() {
  try {
    const a = await api.get('about', null, { maxAgeMs: 600000 });
    els.version.textContent = `Sentry Mate ${buildLabel(a)} · ${a.ui_pack?.name || 'Instrument'} ${a.ui_pack?.version || ''}`.trim();
    els.version.title = buildTitle(a);
  } catch { /* the footer keeps the plain name */ }
}

// the opt-in release notice: also a link in the footer (the bar shows it too, until dismissed)
function showRelease(items) {
  const n = items.find((x) => x.code === 'release_available');
  const u = els.update;
  u.hidden = !n;
  if (!n) return;
  u.querySelector('span').textContent = n.message || 'A newer version is out';
  const link = safeUrl(n.detail?.url) || safeUrl(n.fix_link) || '/help';
  u.setAttribute('href', link);
  if (link.startsWith('/')) { u.removeAttribute('target'); u.setAttribute('rel', 'noopener'); }
}

/* ------------------------------------------------------------------ the account, the car count */

function showAccount() {
  const me = getMe();
  const a = els.acct;
  if (!a) return;
  a.querySelector('.avatar').textContent = (me?.name || '?').slice(0, 1).toUpperCase();
  if (els.topAcct) els.topAcct.querySelector('.avatar').textContent = a.querySelector('.avatar').textContent;
  a.querySelector('.account-name').textContent = me?.name || 'Signed out';
  a.querySelector('.account-role').textContent = roleText(me);
}

async function loadCarCount() {
  try {
    const p = await api.get('fleet/summary', null, { maxAgeMs: 30000 });
    const el = document.getElementById('nav-fleet-count');
    if (el) el.textContent = format(p.cars, 'count');
  } catch { /* the count is a convenience */ }
}

// the car shown in the sidebar on pages without a car: the person's preferred car (Main car, else the last one opened)
async function lastCarForSidebar() {
  try {
    const p = await api.page('cars', { sort: 'custom', limit: 50 }, { maxAgeMs: 30000 });
    const id = state.preferredCar(p.items);
    return p.items.find((c) => c.id === id) || null;
  } catch {
    return null;
  }
}

/* ------------------------------------------------------------------ live events */

function startEvents() {
  stopEvents?.();
  stopEvents = api.events((ev) => {
    if (ev.type === 'prefs' && ev.data) applyPrefs(ev.data);
    else if (ev.type === 'notice') loadNotices();
    else if (ev.type === 'fleet') { loadCarCount(); if (shown.car) refreshCar(); }
    else if (ev.type === 'now' && shown.car && ev.data?.car === shown.car.id) liveCar(ev.data.now);
  }, { types: ['prefs', 'notice', 'fleet', 'now'] });
}
// A live `now` delta carries what the side bar shows of the car (its state, and whether it is stale): take it from
// the event instead of asking for the car again, which would repeat the router's GET of the page being loaded.
function liveCar(d) {
  if (!d || !shown.car) return;
  const n = { ...(shown.car.now || {}) };
  let changed = false;
  for (const k of ['state', 'stale', 'since_ms']) if (k in d && d[k] !== n[k]) { n[k] = d[k]; changed = true; }
  if (!changed) return;
  shown.car = { ...shown.car, now: n };
  showCarInSidebar(shown.car);
}
let carTimer = 0;
function refreshCar() {
  if (carTimer) return; // at most one refresh every 2 s, also while live values keep coming
  carTimer = setTimeout(async () => {
    carTimer = 0;
    if (!shown.car) return;
    try { shown.car = await api.get(api.apiPath`cars/${shown.car.id}`); showCarInSidebar(shown.car); } catch { /* keep the last one */ }
  }, 2000);
}

/* ------------------------------------------------------------------ keys */

function onKey(e) {
  const t = e.target;
  const typing = t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName));
  if ((e.metaKey || e.ctrlKey) && !e.altKey && (e.key === 'k' || e.key === 'K')) {
    if (document.documentElement.dataset.screen === 'bare') return;
    e.preventDefault();
    if (document.querySelector('dialog[open]')) return; // one dialog at a time (the finder, the switcher itself)
    openCarSwitcher();
  } else if (e.key === '/' && !typing && !e.metaKey && !e.ctrlKey && !e.altKey && document.documentElement.dataset.screen !== 'bare') {
    if (document.querySelector('dialog[open]')) return;
    e.preventDefault();
    openFinder();
  }
}

/* ------------------------------------------------------------------ public */

/** Build the frame once, before the first page. */
export function initFrame() {
  els.view = $('#view');
  buildSidebar();
  buildTopbar();
  buildDock();
  buildFooter();
  initNotices($('#notices'));
  initFlash($('#flash'));
  onNotices(showRelease);
  api.on('events', showFreshness);
  showFreshness(api.eventsState());
  document.addEventListener('keydown', onKey);
  setInterval(() => { tickClock(); refreshAgo(); }, 15000);
  // a browser signed in last time asks for the frame's first reads with the session check, not after it: the first
  // page waits for the notices bar (a signed-out browser's answers are 401s, forgotten; signing in asks again)
  let early = signedInHint();
  if (early) { loadAbout(); loadNotices(); loadCarCount(); }
  onSession((s) => {
    showAccount();
    if (s.signedIn) {
      if (!started) { started = true; if (!early) { loadAbout(); loadNotices(); loadCarCount(); } startEvents(); }
      early = false;
    } else {
      early = false;
      started = false;
      stopEvents?.(); stopEvents = null;
    }
  });
}

/** The router tells the frame what page is shown: { s (state), info, car, period, tz }. */
export async function update({ s, info, car, period, tz = null }) {
  const page = info?.id || s.page;
  const activePage = info?.parent || page;
  for (const [id, a] of els.navLinks || []) {
    carLink(a, id);
    if (id === activePage) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current');
  }
  for (const g of document.querySelectorAll('.nav-group')) {
    const has = pagesIn(g.dataset.group).some((p) => p.id === activePage);
    g.classList.toggle('has-current', has);
    const head = g.querySelector('.nav-group-head');
    // the group holding the open page is always open
    if (has && head.getAttribute('aria-expanded') === 'false') head.setAttribute('aria-expanded', 'true');
  }
  showTabs(activePage);
  document.documentElement.toggleAttribute('data-has-period', !!period);
  setPeriodShown(period, tz);
  shown = { car, tz: tz || (car ? car.time_zone : null), page };
  tickClock();
  if (car) showCarInSidebar(car);
  else if (s.page !== 'sign-in' && s.page !== 'setup') {
    const my = s;
    const c = await lastCarForSidebar();
    // a page without a car shows the last car's clock
    if (state.current() === my) { showCarInSidebar(c); shown.car = c; if (c) { shown.tz = c.time_zone; tickClock(); } }
  }
}
