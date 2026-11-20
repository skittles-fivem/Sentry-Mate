// Sentry Mate, "Pebble" UI pack: the frame every page shares.
//
// - The header of each page: the brand (home), the offline chip and the round account button on a thin bar;
//   under it the big page title, the car's name (a pill that opens the car switcher) and the date-range pill
//   (it opens the period sheet), and a slot for the page's own buttons.
// - The floating dock, 16 px above the bottom edge (plus the safe area) and centred on every screen: the round
//   car switcher, Fleet, Car, Drives, Charges, Battery and More. The pages are a tab list (arrow keys move);
//   the active one is a filled coral pill with its label, and the pill slides from item to item. On a phone or
//   a narrow window the dock shows icons only, with the label on the active item.
// - The More sheet: every other page as rounded rows in groups (Activity, Insights, Car, Settings, System, Help).
// - Notices (each with its cause and its fix), the footer (version, licence and notices, the release note) and
//   the offline chip. The router calls update(view) for every page; the rest keeps itself up to date (notices
//   and preferences from /api/v1/events, ages on a timer).

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { formatNumber } from '../shared/i18n.js';
import { PAGES, DOCK, SHEET, pageInfo, navId } from './pages.js';
import { icon, initial, refreshAges, safeLink, stateWord } from './ui.js';
import { createSheet, sheetRow, sheetGroup } from './sheet.js';
import { openSwitcher, setSwitcherCar } from './switcher.js';
import { openPeriod, pillWords } from './period.js';
import { openAccount, setAccount } from './account.js';
import { applyPrefs } from './prefs.js';
import { display, snap } from './display.js';

let me = null;
let about = null;
let view = { page: null, info: null, car: null, tz: 'UTC', period: null, carsCount: null };
const el = {};

/* ------------------------------------------------------------------ the header */

function buildHead() {
  const head = document.getElementById('head');
  el.title = h('h1', { class: 'page-title', id: 'page-title' }, '');
  el.carName = h('span', { class: 'pill-text' });
  el.carAvatar = h('span', { class: 'avatar avatar-sm', 'aria-hidden': 'true' });
  el.carPill = h('button', { type: 'button', class: 'pill car-pill', 'aria-haspopup': 'dialog', onclick: (e) => openSwitcher(e.currentTarget) },
    el.carAvatar, el.carName, icon('chevronDown', 'pill-caret'));
  el.dateName = h('span', { class: 'pill-text' });
  el.dateDates = h('span', { class: 'pill-sub' });
  el.datePill = h('button', { type: 'button', class: 'pill date-pill', 'aria-haspopup': 'dialog', onclick: (e) => openPeriod(view.period, view.tz, e.currentTarget) },
    icon('calendar', 'pill-ic'), el.dateName, el.dateDates, icon('chevronDown', 'pill-caret'));
  el.actions = h('div', { class: 'head-actions', id: 'head-actions' });
  el.offline = h('span', { class: 'chip tone-bad offline-chip', role: 'status', hidden: true }, icon('offline'), h('span', null, 'Offline, trying again'));
  el.account = h('button', { type: 'button', class: 'avatar-btn', 'aria-haspopup': 'dialog', 'aria-label': `You: ${me ? me.name : 'account'}`, title: me ? `${me.name}: look, links, sign out` : 'Account', onclick: (e) => openAccount(e.currentTarget) },
    h('span', { class: 'avatar', 'aria-hidden': 'true' }, initial(me && me.name)));
  head.replaceChildren(
    h('div', { class: 'head-bar' },
      h('a', { class: 'brand', href: '/', 'aria-label': 'Sentry Mate: Fleet', cover: ['shell.nav.home', 'web.shell.logo-home', 'web.shell.home-routing'] },
        h('img', { class: 'brand-mark', src: '/icon.svg', alt: '' }), h('span', { class: 'brand-name' }, 'Sentry Mate')),
      h('div', { class: 'head-bar-end' }, el.offline, el.account)),
    h('div', { class: 'head-main' },
      h('div', { class: 'head-text' }, el.title, h('div', { class: 'head-meta' }, el.carPill, el.datePill)),
      el.actions));
}

function drawHead() {
  const info = view.info;
  const id = navId(view.page, !!view.car);
  el.title.textContent = view.title || (id === 'system' ? 'System' : info ? info.title : '');
  // the car: its name, or how many cars on a page that shows them all
  const car = view.car;
  const allCars = !car && info && (info.car === 'optional' || view.page === 'fleet');
  el.carPill.hidden = !car && !allCars;
  if (car) {
    const st = (car.now && car.now.state) || 'unknown';
    el.carName.textContent = car.name;
    el.carAvatar.textContent = initial(car.name);
    el.carAvatar.className = `avatar avatar-sm st-${st}`;
    el.carPill.setAttribute('aria-label', `${car.name}, ${stateWord(st, !!(car.now && car.now.stale))}: switch car`);
  } else {
    const n = view.carsCount;
    el.carName.textContent = typeof n === 'number' ? (n === 1 ? '1 car' : `${formatNumber(n)} cars`) : 'All cars';
    el.carAvatar.textContent = '';
    el.carAvatar.className = 'avatar avatar-sm avatar-all';
    el.carAvatar.append(icon('fleet'));
    el.carPill.setAttribute('aria-label', `${el.carName.textContent}: open a car`);
  }
  // the period
  const p = view.period;
  el.datePill.hidden = !p;
  if (p) {
    const w = pillWords(p, view.tz);
    el.dateName.textContent = w.name;
    el.dateDates.textContent = w.dates;
    el.dateDates.hidden = !w.dates;
    el.datePill.setAttribute('aria-label', `Period: ${w.name}${w.dates ? `, ${w.dates}` : ''}. Change the period`);
  }
  el.actions.replaceChildren();
}

/* ------------------------------------------------------------------ the dock */

function buildDock() {
  const nav = document.getElementById('dock');
  el.dockCarAvatar = h('span', { class: 'avatar', 'aria-hidden': 'true' }, '');
  el.dockCar = h('button', { type: 'button', class: 'dock-car', 'aria-haspopup': 'dialog', 'aria-label': 'Switch car', onclick: (e) => openSwitcher(e.currentTarget) }, el.dockCarAvatar);
  el.tabs = h('div', { class: 'dock-tabs', role: 'tablist', 'aria-label': 'Pages' }, DOCK.map((id) => h('a', {
    class: 'dock-item', role: 'tab', href: '/', dataset: { page: id }, 'aria-selected': 'false', tabindex: '-1',
  }, icon(PAGES[id].icon), h('span', { class: 'dock-label' }, PAGES[id].label))));
  el.moreLabel = h('span', { class: 'dock-label' }, 'More');
  el.more = h('button', { type: 'button', class: 'dock-item dock-more', 'aria-haspopup': 'dialog', 'aria-label': 'More pages', cover: ['shell.nav.mobile_menu', 'web.shell.menu-toggle'], onclick: (e) => el.moreSheet.open(e.currentTarget) },
    icon('more'), el.moreLabel);
  el.pill = h('span', { class: 'dock-pill', 'aria-hidden': 'true' });
  nav.replaceChildren(el.pill, el.dockCar, el.tabs, el.more);
  // arrow keys move between the tabs (one tab stop)
  el.tabs.addEventListener('keydown', (e) => {
    const step = { ArrowRight: 1, ArrowLeft: -1, Home: -99, End: 99 }[e.key];
    if (!step) return;
    e.preventDefault();
    const tabs = [...el.tabs.children];
    const i = tabs.indexOf(document.activeElement);
    const j = step === -99 ? 0 : step === 99 ? tabs.length - 1 : (Math.max(0, i) + step + tabs.length) % tabs.length;
    tabs.forEach((t, k) => { t.tabIndex = k === j ? 0 : -1; });
    tabs[j].focus();
  });
  el.tabs.addEventListener('focusout', (e) => { if (!el.tabs.contains(e.relatedTarget)) rovingToActive(); });
  new ResizeObserver(() => placeDock()).observe(nav);
  display.onGrid(() => placeDock());
  addEventListener('resize', () => placeDock());
}

function rovingToActive() {
  const tabs = [...el.tabs.children];
  const at = Math.max(0, tabs.findIndex((t) => t.getAttribute('aria-selected') === 'true'));
  tabs.forEach((t, k) => { t.tabIndex = k === at ? 0 : -1; });
}

/** The dock's width in whole device pixels, centred with left: 50% and a shift rounded to the pixel; the pill under the active item. */
let placing = false;
function placeDock() {
  if (placing) return;
  placing = true;
  try {
    const nav = document.getElementById('dock');
    const dpr = display.dpr;
    nav.style.removeProperty('width');
    const natural = nav.getBoundingClientRect().width;
    const w = Math.ceil(natural * dpr - 0.01) / dpr;
    nav.style.setProperty('width', `${w}px`);
    const vw = document.documentElement.clientWidth;
    const left = snap((vw - w) / 2, dpr);
    nav.style.setProperty('--dock-shift', `${left - vw / 2}px`);
    const active = el.tabs.querySelector('[aria-selected="true"]') || (el.more.classList.contains('is-active') ? el.more : null);
    if (active) {
      const nb = nav.getBoundingClientRect(), ab = active.getBoundingClientRect();
      el.pill.style.setProperty('--pill-x', `${snap(ab.left - nb.left - nav.clientLeft, dpr)}px`);
      el.pill.style.setProperty('--pill-w', `${snap(ab.width, dpr)}px`);
      el.pill.hidden = false;
    } else el.pill.hidden = true;
  } finally { placing = false; }
}

function drawDock() {
  const id = navId(view.page, !!view.car);
  for (const a of el.tabs.children) {
    const on = a.dataset.page === id;
    a.setAttribute('href', safeLink(a.dataset.page));
    a.setAttribute('aria-selected', String(on));
    a.classList.toggle('is-active', on);
    if (on) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current');
  }
  rovingToActive();
  const inSheet = !!id && !DOCK.includes(id) && id !== 'sign-in' && id !== 'setup';
  el.more.classList.toggle('is-active', inSheet);
  el.moreLabel.textContent = inSheet ? (pageInfo(id) || {}).label || 'More' : 'More';
  el.more.setAttribute('aria-label', inSheet ? `More pages (now: ${el.moreLabel.textContent})` : 'More pages');
  const car = view.car;
  el.dockCarAvatar.replaceChildren(car ? initial(car.name) : icon('car'));
  el.dockCarAvatar.className = `avatar ${car ? `st-${(car.now && car.now.state) || 'unknown'}` : 'avatar-all'}`;
  el.dockCar.setAttribute('aria-label', car ? `Switch car (now: ${car.name})` : 'Open a car');
  el.dockCar.title = car ? `${car.name}: switch car` : 'Open a car';
  // the More sheet: links that carry the car and the period, the current one marked
  for (const a of el.moreSheet.body.querySelectorAll('a.row[data-page]')) {
    const p = a.dataset.page;
    a.setAttribute('href', p === 'system' ? state.buildUrl('health', { period: state.current().period }) : safeLink(p));
    const on = p === id;
    a.classList.toggle('is-current', on);
    if (on) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current');
  }
  requestAnimationFrame(placeDock);
}

function buildMoreSheet() {
  el.moreSheet = createSheet({ title: 'More', cls: 'more-sheet' });
  const covers = { places: ['shell.nav.geofences', 'web.shell.geofences-nav'], 'settings-display': ['shell.nav.settings', 'web.shell.settings-nav'] };
  for (const g of SHEET) {
    const rows = g.pages.map((p) => {
      const info = pageInfo(p);
      const r = sheetRow({ href: '/', icon: info.icon, name: info.label, sub: info.blurb, cover: covers[p] || null, tone: TONES[g.name] });
      r.dataset.page = p;
      return r;
    });
    el.moreSheet.body.append(sheetGroup(g.name === 'More' ? null : g.name, rows));
  }
  el.moreSheet.body.addEventListener('click', (e) => { if (e.target.closest('a[href]')) el.moreSheet.close(); });
}
const TONES = { Activity: 'coral', Insights: 'sky', Car: 'teal', Settings: 'sun', More: 'muted' };

/* ------------------------------------------------------------------ notices and the footer */

const DISMISS_KEY = 'pebble:dismissed';
const dismissed = () => { try { return new Set(JSON.parse(sessionStorage.getItem(DISMISS_KEY) || '[]')); } catch { return new Set(); } };
let notices = [];

function fixWords(link) {
  const page = state.parseUrl(link).page;
  return PAGES[page] ? `Fix it in ${PAGES[page].title}` : 'Fix it';
}

function drawNotices() {
  const box = document.getElementById('notices');
  const gone = dismissed();
  const shown = notices.filter((n) => n.code !== 'release_available' && !gone.has(`${n.code}:${n.since_ms}`));
  box.hidden = !shown.length;
  box.replaceChildren(...shown.map((n) => h('div', { class: ['notice', `lv-${n.level || 'info'}`], role: n.level === 'error' ? 'alert' : null },
    h('span', { class: 'dot', 'aria-hidden': 'true' }),
    h('p', { class: 'notice-text' }, n.message),
    n.fix_link ? h('a', { class: 'btn btn-soft btn-sm', href: n.fix_link }, h('span', { class: 'btn-text' }, fixWords(n.fix_link)), icon('arrowRight')) : null,
    h('button', { type: 'button', class: 'btn btn-ghost btn-icon btn-sm', 'aria-label': 'Hide this notice for now', title: 'Hide for now', onclick: () => {
      try { const s = dismissed(); s.add(`${n.code}:${n.since_ms}`); sessionStorage.setItem(DISMISS_KEY, JSON.stringify([...s])); } catch { /* fine */ }
      drawNotices();
    } }, icon('close')))));
  drawFoot();
}

function drawFoot() {
  const release = notices.find((n) => n.code === 'release_available');
  const pack = about && about.ui_pack ? `${about.ui_pack.name || 'Pebble'} ${about.ui_pack.version || ''}`.trim() : 'Pebble';
  document.getElementById('foot').replaceChildren(
    h('p', { class: 'foot-line' }, `Sentry Mate${about && about.version ? ` ${about.version}` : ''}`, h('span', { class: 'foot-sep', 'aria-hidden': 'true' }, '·'), `${pack} UI pack`,
      about && about.api_version ? [h('span', { class: 'foot-sep', 'aria-hidden': 'true' }, '·'), `API ${about.api_version}`] : null),
    h('p', { class: 'foot-line', cover: ['shell.footer.legal', 'web.shell.legal-footer'] },
      'Sentry Mate only reads from your cars.', h('span', { class: 'foot-sep', 'aria-hidden': 'true' }, '·'),
      h('a', { href: '/help#licence' }, 'Licence and notices'), h('span', { class: 'foot-sep', 'aria-hidden': 'true' }, '·'), h('a', { href: '/help' }, 'Help')),
    h('p', { class: ['foot-line', { 'has-release': !!release }], cover: ['shell.footer.update_available', 'web.shell.update-footer'] },
      release ? [h('span', { class: 'dot', 'aria-hidden': 'true' }), release.message, h('span', { class: 'foot-sep', 'aria-hidden': 'true' }, '·'), h('a', { href: release.fix_link || '/help' }, 'About this release')]
        : 'No release note: the release check is off, or this is the newest.'),
    h('p', { class: 'foot-line foot-quiet' }, about && about.data_credits && about.data_credits[0] ? about.data_credits[0].text : 'Map data © OpenStreetMap contributors'));
}

async function loadNotices() {
  try { notices = (await api.get('notices')).items || []; } catch { notices = []; }
  drawNotices();
}

/* ------------------------------------------------------------------ start and update */

/** Build the frame once, after sign-in: mountFrame({ me, about, carsCount }) -> { update(view), setHead(patch) }. */
export function mountFrame(opts) {
  me = opts.me;
  about = opts.about;
  view.carsCount = opts.carsCount ?? null;
  setAccount(me);
  buildHead();
  buildMoreSheet();
  buildDock();
  drawFoot();
  loadNotices();
  setInterval(refreshAges, 20000);
  api.events((ev) => {
    if (ev.type === 'notice') loadNotices();
    else if (ev.type === 'prefs' && ev.data) applyPrefs(ev.data);
  }, { types: ['notice', 'prefs'] });
  api.on('network', (n) => {
    const off = !n.ok;
    document.documentElement.classList.toggle('is-offline', off);
    el.offline.hidden = !off;
  });
  return { update, setHead };
}

/** The router: a new page (or period) is shown. view: { page, info, car, tz, period, carsCount }. */
function update(v) {
  view = { ...view, ...v, carsCount: v.carsCount ?? view.carsCount, title: null };
  setSwitcherCar(view.car ? view.car.id : null);
  drawHead();
  drawDock();
  if (el.moreSheet.isOpen) el.moreSheet.close();
}

/** A page changes its header: setHead({ title, actions: [nodes] }). */
function setHead({ title = undefined, actions = undefined } = {}) {
  if (title !== undefined) { view.title = title; el.title.textContent = title || (view.info ? view.info.title : ''); }
  if (actions !== undefined) el.actions.replaceChildren(...[].concat(actions || []).filter(Boolean));
}

/** The account signed in, and the server's /about answer. */
export const currentMe = () => me;
export const currentAbout = () => about;
