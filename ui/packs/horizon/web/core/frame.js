// The app frame every page shares: Simple's structure in Horizon's look. Along the top a dashboard strip
// (Horizon's HUD bar, without its radio display) with the brand, the six sections (Car, Trips, Charging,
// Battery, Stats, Settings) as buttons with an icon over the name, and the tools ("Find a page" (/), live
// status, the car's clock, the car switcher, the account); under it where you are ("Sentry Mate / Car",
// a detail page's trail), then the open section's tabs as a row of chips and the shared period picker;
// the notices bar, flash notes and the footer.
// Each section opens on the tab the person used last there (this browser). On a phone (<html
// data-phone>, set by boot.js) the strip becomes one bar of icons with the menu (every page) at its end,
// the tools move to a line under it, the tabs scroll sideways, and the period sits at the bottom on an
// upright phone.
// The router calls update() for every page it mounts; pages never touch the frame except through ctx
// (ctx.setCrumbs, ctx.setTitle).

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { buildLabel, buildTitle } from '../shared/release.js';
import { h, replace, cover, safeUrl } from '../shared/dom.js';
import { formatTime, zoneName } from '../shared/time.js';
import { format } from '../shared/units.js';
import { icon, brandMark } from './icons.js';
import { GROUPS, pagesIn, pageInfo, sectionOf, tabOf, tabLabel } from './pages.js';
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
const TAB_KEY = 'sentrymate.horizon.tabs';
let els = {};
let shown = { car: null, tz: null, page: null };
let stopEvents = null;
let started = false;

/** The element pages draw into (<main id="view">). */
export const view = () => els.view || $('#view');

// the last tab opened in each section, so a section opens where the person left it (this browser only)
function lastTabs() {
  try { const v = JSON.parse(localStorage.getItem(TAB_KEY)); return v && typeof v === 'object' ? v : {}; } catch { return {}; }
}
function rememberTab(section, page) {
  if (!section || !page) return;
  const v = lastTabs();
  if (v[section] === page) return;
  v[section] = page;
  try { localStorage.setItem(TAB_KEY, JSON.stringify(v)); } catch { /* fine */ }
}
/** The page a section opens on: its last tab, else its first. */
export function sectionPage(section) {
  const tabs = pagesIn(section);
  const last = lastTabs()[section];
  return (tabs.find((p) => p.id === last) || tabs[0])?.id || null;
}

/* ------------------------------------------------------------------ the header: brand, sections, tools */

function sectionLink(g, cls) {
  const a = h('a', { class: cls, href: '/', dataset: { section: g.id }, title: g.desc },
    icon(g.icon), h('span', { class: `${cls}-label` }, g.title));
  if (g.id === 'settings') cover(a, 'shell.nav.settings', 'web.shell.settings-nav');
  if (g.id === 'trips') cover(a, 'shell.nav.geofences', 'web.shell.geofences-nav'); // Places is a tab of Trips
  return a;
}

function carButton(cls) {
  return h('button', { class: ['car-switch', cls], type: 'button', 'aria-haspopup': 'dialog', title: 'Switch car (' + SWITCH_KEYS.join(' ') + ')', onclick: openCarSwitcher },
    h('span', { class: 'car-switch-dot', 'aria-hidden': 'true' }),
    h('span', { class: 'car-switch-text' }, h('span', { class: 'car-switch-name' }, 'Choose a car'), h('span', { class: 'car-switch-sub' }, h('span', { class: 'car-switch-sub-text' }, 'No car open'))),
    icon('chev-d', 'ico--sm top-car-chev'));
}

// The strip along the top: the brand, the six sections and (not on a phone) the tools; under it where you
// are ("Sentry Mate / Car"), then the open section's tabs and the period.
function buildTopbar() {
  const sections = new Map();
  const nav = h('nav', { class: 'sections', 'aria-label': 'Sections' }, GROUPS.map((g) => {
    const a = sectionLink(g, 'section');
    sections.set(g.id, a);
    return a;
  }));
  const menuBtn = h('button', {
    class: 'section menu-btn', type: 'button', 'aria-label': 'Menu: every section and page',
    title: 'Menu', onclick: () => openFinder(), cover: ['shell.nav.mobile_menu', 'web.shell.menu-toggle'],
  }, icon('menu'), h('span', { class: 'section-label' }, 'Menu'));
  const brand = h('a', { class: 'brand', href: '/', 'aria-label': 'Sentry Mate: all cars', cover: ['shell.nav.home', 'web.shell.logo-home', 'web.shell.home-routing'] },
    brandMark(), h('span', { class: 'brand-text' }, h('span', { class: 'brand-name' }, 'Sentry Mate'), h('span', { class: 'brand-sub' }, 'Car data')));
  const stripTools = h('div', { class: 'strip-tools' });
  replace($('#rail'), h('div', { class: 'rail-card' }, brand, nav, menuBtn, stripTools));

  const find = h('button', { class: 'finder-btn', type: 'button', onclick: () => openFinder(), title: 'Find a page (/)', 'aria-label': 'Find a page' },
    icon('search'), h('span', { class: 'finder-text' }, 'Find'), h('span', { class: 'kbd' }, '/'));
  const fresh = h('span', { class: 'freshness', title: 'Pages update by themselves when the server has processed a new log. Logs arrive about a minute after the car records them.' },
    h('span', { class: 'live-dot', 'aria-hidden': 'true' }), icon('warn', 'freshness-late'), h('span', { class: 'freshness-text' }, 'Connecting'));
  const clock = h('span', { class: 'clock', 'aria-label': "The car's local time" });
  const topCar = carButton('top-car');
  const acct = h('button', { class: 'account', type: 'button', 'aria-haspopup': 'dialog', 'aria-expanded': 'false', 'aria-label': 'Your account and display', onclick: (e) => toggleAccountMenu(e.currentTarget) },
    h('span', { class: 'avatar', 'aria-hidden': 'true' }, '·'),
    h('span', { class: 'account-text' }, h('span', { class: 'account-name' }, 'Account'), h('span', { class: 'account-role' })));

  // where you are: "Sentry Mate / Car", a detail page's trail after the app's name
  const where = h('span', { class: 'where-section' });
  const crumbs = h('nav', { class: 'crumbs', 'aria-label': 'Breadcrumb' });
  const crumb = h('div', { class: 'crumb' }, h('span', { class: 'crumb-app' }, 'Sentry Mate'), h('span', { class: 'sep', 'aria-hidden': 'true' }, '/'), where, crumbs);
  const topTools = h('div', { class: 'top-tools' }, find, fresh, clock, topCar, acct);
  const topline = h('div', { class: 'topline' }, crumb, topTools);

  // the tabs of the open section and the period
  const tabs = h('nav', { class: 'subtabs', 'aria-label': 'Pages of this section' });
  const period = periodPicker();
  const tools = h('div', { class: 'subbar-tools' }, period);
  const subbar = h('div', { class: 'subbar' }, tabs, tools);
  replace($('#topbar'), topline, subbar);
  document.body.append(accountMenu());
  Object.assign(els, { sections, tabsNav: tabs, crumbs, where, fresh, clock, topCar, acct, period, tools, subbar, topTools, topline, stripTools });
  placeTools();
  window.addEventListener('phonelayout', placeTools);
}

// the tools sit in the strip on a computer, and on their own line under the bar of icons on a phone
function placeTools() {
  const t = els.topTools;
  if (!t) return;
  const phone = !!document.documentElement.dataset.phone;
  if (phone && t.parentNode !== els.topline) els.topline.append(t);
  else if (!phone && t.parentNode !== els.stripTools) els.stripTools.append(t);
}

function showCarInSidebar(car) {
  for (const b of [els.topCar]) if (b) showCarIn(b, car);
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
  sub.textContent = sold ? 'Sold' : n.stale ? 'last known: ' + stateLabel(n.state).toLowerCase() : stateLabel(n.state);
  b.title = [car.name, carMeta(car)].filter(Boolean).join(' · ') + '. Switch car (' + SWITCH_KEYS.join(' ') + ')';
  dot.classList.toggle('is-stale', !!n.stale || sold);
  b.setAttribute('aria-label', `Car: ${car.name}, ${stateLabel(n.state)}. Switch car`);
}

// the open section's tabs: built again only when the section changes, so they never flicker
let tabsFor = null;
function showSubtabs(section, activeTab) {
  const nav = els.tabsNav;
  if (!nav) return;
  if (tabsFor !== section) {
    tabsFor = section;
    const list = section ? pagesIn(section) : [];
    replace(nav, list.map((p) => {
      return h('a', { class: 'subtab', href: '/', dataset: { page: p.id }, title: p.desc }, h('span', null, tabLabel(p)));
    }));
  }
  for (const a of nav.querySelectorAll('.subtab')) {
    a.setAttribute('href', state.linkTo(a.dataset.page));
    if (a.dataset.page === activeTab) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current');
  }
  nav.hidden = !section;
  const cur = nav.querySelector('[aria-current]');
  // a phone: the current tab in view in the strip
  if (cur && nav.scrollWidth > nav.clientWidth) {
    const l = cur.offsetLeft - nav.offsetLeft, r = l + cur.offsetWidth;
    if (l < nav.scrollLeft || r > nav.scrollLeft + nav.clientWidth) nav.scrollLeft = Math.max(0, l - 16);
  }
}

/* ------------------------------------------------------------------ the phone dock: the period */

function buildDock() {
  const dock = $('#dock');
  if (!dock) return;
  // the sections are in the bar at the top (as in Sentry Manager); the dock holds the period, in reach of the thumb
  const periodSlot = h('div', { class: 'dock-period' });
  replace(dock, periodSlot);
  Object.assign(els, { periodSlot });
  placePeriod();
  window.addEventListener('phonelayout', placePeriod);
}

// an upright phone: the period bar sits in the dock, in reach of the thumb; otherwise in the tab row
function placePeriod() {
  const p = els.period;
  if (!p || !els.periodSlot) return;
  const inDock = document.documentElement.dataset.phone === 'tall';
  if (inDock && p.parentNode !== els.periodSlot) els.periodSlot.append(p);
  else if (!inDock && p.parentNode !== els.tools) els.tools.prepend(p);
}

function showSections(section, page) {
  for (const map of [els.sections]) {
    for (const [id, a] of map || []) {
      // the open section links to its first tab; the others to where the person left them
      a.setAttribute('href', state.linkTo(id === section && page === 'fleet' ? 'fleet' : sectionPage(id)));
      if (id === section) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current');
    }
  }
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
  const carCrumb = car ? { label: car.name, href: state.linkTo('car', { car: car.id }) } : null;
  if (info.id === 'car') return [fleet, { label: car?.name || 'Car' }];
  if (info.parent) {
    const parent = pageInfo(info.parent);
    return [fleet, carCrumb, { label: parent.title, href: state.linkTo(info.parent) }, { label: info.title }];
  }
  if (car) return [fleet, carCrumb, { label: info.nav || info.title }];
  return [group && group.id !== 'car' ? { label: group.title } : fleet, { label: info.title }];
}

/* ------------------------------------------------------------------ footer */

function buildFooter() {
  const version = h('span', { class: 'footer-version' }, 'Sentry Mate');
  const update = h('a', {
    class: 'footer-update', hidden: true, target: '_blank', rel: 'noopener noreferrer external',
    cover: ['shell.footer.update_available', 'web.shell.update-footer'],
  }, icon('arrow-r'), h('span', null, 'A newer version is out'));
  const legal = h('span', { class: 'footer-legal', cover: ['shell.footer.legal', 'web.shell.legal-footer'] },
    'Bundled parts keep their own licences: MapLibre GL JS (BSD-3-Clause), uPlot (MIT), Orbitron and Chakra Petch (SIL Open Font License 1.1) and JetBrains Mono (Apache-2.0). ',
    h('a', { href: '/help#licences' }, 'Licences and notices'), ' · ',
    h('a', { href: '/help#credits' }, 'Data credits'));
  replace($('#footer'), version, legal, h('span', { class: 'grow' }), update);
  Object.assign(els, { version, update });
}

async function loadAbout() {
  try {
    const a = await api.get('about', null, { maxAgeMs: 600000 });
    els.version.textContent = `Sentry Mate ${buildLabel(a)} · ${a.ui_pack?.name || 'Horizon'} ${a.ui_pack?.version || ''}`.trim();
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
  a.querySelector('.account-name').textContent = me?.name || 'Signed out';
  a.querySelector('.account-role').textContent = roleText(me);
}

async function loadCarCount() {
  try {
    const p = await api.get('fleet/summary', null, { maxAgeMs: 30000 });
    const a = els.sections?.get('car');
    if (a) a.title = `${GROUPS[0].desc} ${format(p.cars, 'count')} ${p.cars === 1 ? 'car' : 'cars'} in all.`;
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
  const section = sectionOf(page);
  const tab = tabOf(page);
  if (section && tab && info?.group) rememberTab(section, tab);
  showSections(section, page);
  showSubtabs(section, tab);
  if (els.where) els.where.textContent = GROUPS.find((g) => g.id === section)?.title || info?.title || '';
  // the trail shows only on a page inside a tab (a drive, a charge); a tab's own name is the tab
  document.documentElement.toggleAttribute('data-detail', !!info?.parent);
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
