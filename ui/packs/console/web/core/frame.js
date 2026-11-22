// The frame: the command bar (the route as a path, period, car, command list, account), the link row of
// the current car's pages, the notices, the footer and the toasts. Pages never touch it except through ctx.
//
// Desktop: the command bar is fixed at the top, the link row under it. Phone (app.css): the command bar is a
// search field at the bottom, above the safe area, and the link row scrolls sideways.

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { h, replace, cover, safeUrl } from '../shared/dom.js';
import { pagesOf } from './pages.js';
import { initNotices, loadNotices, onNotices, setOffline } from './notices.js';
import { initToasts } from './toast.js';
import { openPalette } from './palette.js';
import { openCarSwitcher } from './carswitch.js';
import { openPeriodPicker, setPeriodShown, periodShort } from './period.js';
import { openAccountMenu } from './account.js';
import { onSession } from './session.js';
import { getMe, applyPrefs } from './prefs.js';
import { slug } from './fmt.js';

const $ = (sel) => document.querySelector(sel);
const els = {};
let shown = { car: null, page: null };
const names = new Map();   // car id -> its name as a path segment
let linkCar = null;        // the car the link row is for (the page's, else the last one, else the first)
let stopEvents = null;
let started = false;

/* ------------------------------------------------------------------ command bar */

function buildCmd() {
  const home = h('a', { class: 'cmd-home', href: '/', 'aria-label': 'Sentry Mate: fleet', cover: ['shell.nav.home', 'web.shell.logo-home', 'web.shell.home-routing'] }, 'sentry-mate');
  const path = h('nav', { class: 'cmd-path', 'aria-label': 'Where you are' });
  const live = h('span', { class: 'cmd-live', role: 'status', title: 'Live updates from the server' }, '[...]');
  const period = h('button', { class: 'cmd-btn', type: 'button', hidden: true, dataset: { desk: '1' }, onclick: () => openPeriodPicker(), title: 'Change the period' });
  const car = h('button', { class: 'cmd-btn', type: 'button', hidden: true, onclick: () => openCarSwitcher(), title: 'Switch car' });
  const find = h('button', { class: 'cmd-find', type: 'button', onclick: () => openPalette(), 'aria-label': 'Command list: find a page, car, drive or setting',
    cover: ['shell.nav.mobile_menu', 'web.shell.menu-toggle'] },
  h('span', { class: 'cmd-find-p' }, '>'), h('span', { class: 'cmd-find-t' }, 'find'), h('span', { class: 'kbd cmd-find-k' }, 'ctrl k'));
  const acct = h('button', { class: 'cmd-btn cmd-acct', type: 'button', onclick: () => openAccountMenu(), title: 'Account, size and theme' }, 'account');
  replace($('#cmd'), h('div', { class: 'wrap cmd-wrap' },
    home, h('span', { class: 'cmd-prompt', 'aria-hidden': 'true' }, '>'), path, h('span', { class: 'grow' }), live, period, car, find, acct));
  Object.assign(els, { path, live, period, car, acct, find });
}

// the route as a path, each segment a link: cars / model-y / drives / 1234
function paintPath(s, info, car) {
  const segs = [];
  const add = (text, href) => segs.push([text, href]);
  const page = info?.id || s.page;
  if (page === 'fleet' || !page) add('fleet', '/');
  else if (page === 'health' && !s.car) add('system', '/system');
  else if (page.startsWith('settings-') || page === 'import') {
    add('settings', state.linkTo('settings-display'));
    add(info?.link || page, state.current().path);
  } else if (s.car) {
    add('cars', '/');
    add(names.get(s.car) || slug(car?.name || s.car), state.buildUrl('car', { car: s.car, period: s.period }));
    if (page !== 'car') {
      const parent = info?.parent;
      add(parent || info?.link || page, state.linkTo(parent || page));
      if (s.drive) add(String(s.drive), state.current().path);
      if (s.charge) add(String(s.charge), state.current().path);
    }
  } else add(info?.link || page, state.current().path);
  replace(els.path, segs.map(([text, href], i) => [
    i ? h('span', { class: 'cmd-sep', 'aria-hidden': 'true' }, '/') : null,
    h('a', { class: 'cmd-seg', href, 'aria-current': i === segs.length - 1 ? 'page' : null }, text)]));
}

/* ------------------------------------------------------------------ link row */

function buildLinks() {
  replace($('#links'), h('div', { class: 'wrap links-wrap' }, h('div', { class: 'links-main' }), h('div', { class: 'links-side' })));
}

function linkEl(id, text, active, params = {}) {
  const a = h('a', { class: 'lk', href: state.linkTo(id, params), 'aria-current': active ? 'page' : null }, text);
  if (id === 'places') cover(a, 'shell.nav.geofences', 'web.shell.geofences-nav');
  if (id === 'settings-display') cover(a, 'shell.nav.settings', 'web.shell.settings-nav');
  return a;
}

function paintLinks(s, info, carId) {
  const page = info?.parent || info?.id || s.page;
  const settings = info?.scope === 'settings';
  const main = $('#links .links-main');
  const side = $('#links .links-side');
  if (settings) {
    replace(main, h('span', { class: 'lk-label' }, 'settings:'), pagesOf('settings').map((p) => linkEl(p.id, p.link, p.id === page)));
  } else if (carId) {
    replace(main, h('span', { class: 'lk-label' }, `${names.get(carId) || carId}:`),
      pagesOf('car').map((p) => linkEl(p.id, p.link, p.id === page && (p.id !== 'health' || !!s.car), { car: carId })));
  } else {
    replace(main, h('span', { class: 'lk-label' }, 'no car yet'));
  }
  replace(side,
    linkEl('fleet', 'fleet', page === 'fleet'),
    linkEl('travel', 'travel', page === 'travel'),
    linkEl('places', 'places', page === 'places'),
    h('a', { class: 'lk', href: '/system', 'aria-current': page === 'health' && !s.car ? 'page' : null }, 'system'),
    linkEl('settings-display', 'settings', settings),
    linkEl('help', 'help', page === 'help'));
  // the current link stays in view on a phone, where the row scrolls sideways
  main.querySelector('[aria-current="page"]')?.scrollIntoView?.({ block: 'nearest', inline: 'nearest' });
}

/* ------------------------------------------------------------------ footer */

function buildFoot() {
  const version = h('span', { class: 'foot-ver' }, 'sentry-mate');
  const update = h('a', { class: 'lnk foot-upd', hidden: true, target: '_blank', rel: 'noopener noreferrer external',
    cover: ['shell.footer.update_available', 'web.shell.update-footer'] }, 'a newer version is out');
  const legal = h('span', { class: 'foot-legal', cover: ['shell.footer.legal', 'web.shell.legal-footer'] },
    'Bundled: MapLibre GL JS (BSD-3-Clause), uPlot (MIT), JetBrains Mono and IBM Plex Sans (OFL 1.1). ',
    h('a', { class: 'lnk', href: '/help#licences' }, 'licences'), ' · ', h('a', { class: 'lnk', href: '/help#credits' }, 'data credits'));
  replace($('#foot'), h('div', { class: 'wrap foot-wrap' }, version, legal, h('span', { class: 'grow' }), update));
  Object.assign(els, { version, update });
}

async function loadAbout() {
  try {
    const a = await api.get('about', null, { maxAgeMs: 600000 });
    els.version.textContent = `sentry-mate ${a.version} · ${String(a.ui_pack?.name || 'console').toLowerCase()} ${a.ui_pack?.version || ''}`.trim();
  } catch { /* keeps the plain name */ }
}

function showRelease(items) {
  const n = items.find((x) => x.code === 'release_available');
  const u = els.update;
  u.hidden = !n;
  if (!n) return;
  u.textContent = n.message || 'a newer version is out';
  const href = safeUrl(n.detail?.url) || safeUrl(n.fix_link) || '/help';
  u.setAttribute('href', href);
  if (href.startsWith('/')) { u.removeAttribute('target'); u.setAttribute('rel', 'noopener'); }
}

/* ------------------------------------------------------------------ live status */

function showLive() {
  const net = api.network();
  const ev = api.eventsState();
  const off = net.ok === false;
  setOffline(off);
  const [text, tone] = off ? ['[offline]', 'bad'] : ev.state === 'open' ? ['[live]', 'accent'] : ev.state === 'closed' ? ['[idle]', 'muted'] : ['[...]', 'muted'];
  els.live.textContent = text;
  els.live.className = `cmd-live tone--${tone}`;
  els.live.title = off ? 'The server does not answer.' : ev.state === 'open' ? 'Pages update by themselves when the server has a new log.' : 'Connecting to live updates.';
}

function startEvents() {
  stopEvents?.();
  stopEvents = api.events((ev) => {
    if (ev.type === 'prefs' && ev.data) applyPrefs(ev.data);
    else if (ev.type === 'notice') loadNotices();
  }, { types: ['prefs', 'notice'], onStatus: showLive });
}

function showAccount() {
  const me = getMe();
  els.acct.textContent = me?.name || 'account';
}

/* ------------------------------------------------------------------ public */

export const view = () => $('#view');

/** Build the frame once, before the first page. */
export function initFrame() {
  buildCmd();
  buildLinks();
  buildFoot();
  initNotices($('#notices'));
  initToasts($('#toasts'));
  onNotices(showRelease);
  api.on('network', showLive);
  api.on('events', showLive);
  showLive();
  onSession((s) => {
    showAccount();
    if (s.signedIn && !started) { started = true; loadAbout(); loadNotices(); startEvents(); }
    if (!s.signedIn) { started = false; stopEvents?.(); stopEvents = null; }
  });
}

/** The router tells the frame what is shown: { s (state), info, car, period, tz }. */
export async function update({ s, info, car, period, tz = null }) {
  setPeriodShown(period, tz);
  els.period.hidden = !period;
  if (period) replace(els.period, h('span', { class: 'muted' }, 'period '), periodShort(period, tz));
  shown = { car, page: info?.id || s.page };
  if (car) names.set(car.id, slug(car.name || car.id));
  let carId = s.car || null;
  if (!carId && s.page !== 'sign-in' && s.page !== 'setup') {
    carId = state.lastCar();
    if (carId && !names.has(carId)) {
      try { const c = await api.get(api.apiPath`cars/${carId}`, null, { maxAgeMs: 60000 }); names.set(c.id, slug(c.name || c.id)); } catch { carId = null; }
      if (state.current() !== s) return;
    }
    if (!carId) {
      // no car opened yet in this browser: the first car, so the link row has the car pages
      try {
        const p = await api.get('cars', { sort: 'name', limit: 20 }, { maxAgeMs: 60000 });
        const c = (p.items || []).find((x) => !state.isSold(x)) || p.items?.[0];   // a sold car only when no other
        if (c) { carId = c.id; names.set(c.id, slug(c.name || c.id)); }
      } catch { /* no car pages then */ }
      if (state.current() !== s) return;
    }
  }
  linkCar = carId;
  els.car.hidden = !carId;
  if (carId) replace(els.car, h('span', { class: 'muted' }, 'car '), names.get(carId) || carId);
  paintPath(s, info, car);
  paintLinks(s, info, carId);
}

/** The car the link row and the keys use: the page's car, else the last one opened, else the first car. */
export const navCar = () => state.current().car || linkCar;

/** What the frame shows now: { car, page }. */
export const shownNow = () => shown;
