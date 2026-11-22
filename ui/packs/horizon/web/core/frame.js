// The app frame every page shares: the sidebar (logo, page finder, car switcher, the menu grouped by
// task, the account menu), the top bar (menu button, breadcrumb, the shared period picker, live status,
// the car's clock), the notices bar, flash notes and the footer (version, licences, release notice).
// The router calls update() for every page it mounts; pages never touch the frame except through ctx
// (ctx.setCrumbs, ctx.setTitle).

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { h, replace, cover, safeUrl } from '../shared/dom.js';
import { formatTime, zoneName } from '../shared/time.js';
import { format } from '../shared/units.js';
import { icon, brandMark } from './icons.js';
import { GROUPS, pagesIn, pageInfo } from './pages.js';
import { periodPicker, setPeriodShown } from './period.js';
import { openFinder } from './finder.js';
import { openCarSwitcher, SWITCH_KEYS } from './carswitch.js';
import { accountMenu, toggleAccountMenu, roleText } from './account.js';
import { initNotices, loadNotices, onNotices } from './notices.js';
import { initFlash } from './flash.js';
import { applyPrefs, getMe } from './prefs.js';
import { onSession } from './session.js';
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
    h('div', { class: 'sidebar-foot' }, acct),
    accountMenu());
  Object.assign(els, { navLinks, carBtn, acct });
}

function showCarInSidebar(car) {
  const b = els.carBtn;
  if (!b) return;
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
  replace($('#topbar'),
    h('div', { class: 'topbar-start' }, menuBtn, crumbs),
    h('div', { class: 'topbar-tools' }, periodPicker(), fresh, clock));
  Object.assign(els, { crumbs, fresh, clock });
}

function showFreshness(info) {
  const f = els.fresh;
  if (!f) return;
  const t = f.querySelector('.freshness-text');
  const late = info.state === 'reconnecting' || info.state === 'closed' || info.state === 'unsupported';
  f.classList.toggle('is-late', late);
  f.hidden = info.state === 'signed_out';
  t.textContent = info.state === 'open' ? 'Live' : info.state === 'connecting' ? 'Connecting'
    : info.state === 'reconnecting' ? `Live updates paused${info.retryInMs ? `, trying again in ${Math.ceil(info.retryInMs / 1000)} s` : ''}`
      : 'Not live: reload to update';
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
  const carCrumb = car ? { label: car.name, href: state.buildUrl('car', { car: car.id, period: s.period }) } : null;
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
    els.version.textContent = `Sentry Mate ${a.version} · ${a.ui_pack?.name || 'Instrument'} ${a.ui_pack?.version || ''}`.trim();
  } catch { /* the footer keeps the plain name */ }
}

// the opt-in release notice: shown in the footer, not the notices bar
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

// the car shown in the sidebar on pages without a car: the last one opened
async function lastCarForSidebar() {
  const id = state.lastCar();
  try {
    if (id) return await api.get(api.apiPath`cars/${id}`, null, { maxAgeMs: 30000 });
    const p = await api.page('cars', { sort: 'custom', limit: 10 });
    return state.splitSold(p.items).active[0] || p.items[0] || null;
  } catch (e) {
    if (e.status === 404 && id) state.forgetCar(id);
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
    else if (ev.type === 'now' && shown.car && ev.data?.car === shown.car.id) refreshCar();
  }, { types: ['prefs', 'notice', 'fleet', 'now'] });
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
  buildFooter();
  initNotices($('#notices'));
  initFlash($('#flash'));
  onNotices(showRelease);
  api.on('events', showFreshness);
  showFreshness(api.eventsState());
  document.addEventListener('keydown', onKey);
  setInterval(() => { tickClock(); refreshAgo(); }, 15000);
  onSession((s) => {
    showAccount();
    if (s.signedIn) {
      if (!started) { started = true; loadAbout(); loadNotices(); loadCarCount(); startEvents(); }
    } else {
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
    a.setAttribute('href', state.linkTo(id));
    if (id === activePage) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current');
  }
  for (const g of document.querySelectorAll('.nav-group')) {
    const has = pagesIn(g.dataset.group).some((p) => p.id === activePage);
    g.classList.toggle('has-current', has);
    const head = g.querySelector('.nav-group-head');
    // the group holding the open page is always open
    if (has && head.getAttribute('aria-expanded') === 'false') head.setAttribute('aria-expanded', 'true');
  }
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
