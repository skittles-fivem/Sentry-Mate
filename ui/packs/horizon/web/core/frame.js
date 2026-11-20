// The frame every page shares.
//
// Computers: the dashboard strip. The brand at the left, the six bands (Cars, Driving, Charging | Energy,
// Reports, Set-up) on both sides, and in the middle the radio display: the page name, its band and channel,
// the car and the live status. The arrows beside it tune to the previous or next page; the face opens the
// scan box (type to find a page). Each band button opens its menu with a line per page. The account knob
// at the right holds the size, look, background and sign out.
// Phones: the same radio (swipe it to tune), a swipeable tuner bar with every page, and a round Quick knob.
// Under the strip: the console row with the car cartridge (car switcher), the period dial and the car's
// clock; then the server's notices; the footer has the version, the licences and the release notice.
//
// The router calls update() for every page it opens; pages reach the frame only through ctx.

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { h, replace, cover, safeUrl } from '../shared/dom.js';
import { formatTime, zoneName } from '../shared/time.js';
import { icon, logo, btn, lamp } from './ui.js';
import { BANDS, inBand, station, band, tune, channel, DIAL, fullName } from './stations.js';
import { popover, openDialog, initToasts } from './layer.js';
import { periodDial, showPeriod } from './period.js';
import { openCarPicker, openScan, CAR_KEYS } from './pickers.js';
import { accountPanel } from './account.js';
import { initNotices, loadNotices, onNotices } from './notices.js';
import { getMe, usePrefs } from './prefs.js';
import { onSession } from './session.js';
import { refreshAgo, carLine, carStatus, stateWord } from './fmt.js';

const $ = (id) => document.getElementById(id);
const E = {};                 // the frame's elements
let shown = { page: null, car: null, tz: null };
let live = { state: 'connecting' };
let stopEvents = null;
let started = false;

/** The element pages draw into. */
export const view = () => E.view || $('view');

/* ------------------------------------------------------------------ the dashboard strip */

function bandButton(b) {
  const pop = popover({
    cls: 'band-pop', label: `${b.full || b.name} pages`, align: b.side === 'left' ? 'start' : 'end',
    fill(el) {
      const here = shown.page && (station(shown.page)?.parent || shown.page);
      replace(el, h('p', { class: 'band-pop-head' }, icon(b.icon, 'i--sm'), b.full || b.name),
        inBand(b.id).map((s) => {
          const a = h('a', { class: 'station', href: state.linkTo(s.id), 'aria-current': s.id === here ? 'page' : null, onclick: () => pop.hide() },
            icon(s.icon), h('span', { class: 'station-text' }, h('span', { class: 'station-name' }, fullName(s)), h('span', { class: 'station-line' }, s.line)),
            h('span', { class: 'station-ch' }, `CH ${String(channel(s.id)).padStart(2, '0')}`));
          return a;
        }));
    },
  });
  const b2 = h('button', { class: 'band-btn', type: 'button', 'aria-haspopup': 'dialog', 'aria-expanded': 'false', dataset: { band: b.id }, title: `${b.full || b.name}: ${inBand(b.id).map((s) => s.short || s.name).join(', ')}`,
    onclick: () => pop.toggle(b2) },
  icon(b.icon), h('span', { class: 'band-btn-label' }, b.name));
  // the menus hold links to Places and to the settings: mark the buttons that open them
  if (b.id === 'setup') cover(b2, 'shell.nav.geofences', 'web.shell.geofences-nav', 'shell.nav.settings', 'web.shell.settings-nav');
  return b2;
}

function buildDash() {
  const brand = h('a', { class: 'brand', href: '/', 'aria-label': 'Sentry Mate: all cars', cover: ['shell.nav.home', 'web.shell.logo-home', 'web.shell.home-routing'] },
    logo(), h('span', { class: 'brand-words' }, h('span', { class: 'brand-name chrome' }, 'Sentry Mate'), h('span', { class: 'brand-sub' }, 'Horizon')));
  const left = h('div', { class: 'bands' }, BANDS.filter((b) => b.side === 'left').map(bandButton));
  const right = h('div', { class: 'bands' }, BANDS.filter((b) => b.side === 'right').map(bandButton));

  E.rBand = h('span', { class: 'radio-ch' });
  E.rLive = h('span', { class: 'radio-live' }, h('span', { class: 'lamp', 'aria-hidden': 'true' }), h('span', { class: 'radio-live-text' }, 'TUNING'));
  E.rName = h('span', { class: 'radio-name' }, 'Sentry Mate');
  E.rSub = h('span', { class: 'radio-sub' }, ' ');
  E.face = h('button', { class: 'radio-face', type: 'button', 'aria-label': 'Find a page', title: 'Find a page (/)', onclick: openScan },
    h('span', { class: 'radio-band' }, E.rBand, E.rLive), E.rName, E.rSub);
  E.seekL = h('button', { class: 'radio-seek', type: 'button', onclick: () => tuneBy(-1) }, icon('seek-l'));
  E.seekR = h('button', { class: 'radio-seek', type: 'button', onclick: () => tuneBy(1) }, icon('seek-r'));
  swipe(E.face);

  const acctPop = popover({
    cls: 'acct-pop', label: 'Your account and display', align: 'end',
    fill(el, p) {
      const panel = accountPanel(() => p.hide());
      replace(el, panel.el);
      el.addEventListener('hz-closed', panel.stop, { once: true });
    },
  });
  E.acct = h('button', { class: 'acct-btn', type: 'button', 'aria-haspopup': 'dialog', 'aria-expanded': 'false', 'aria-label': 'Your account and display', title: 'Your account and display', onclick: () => acctPop.toggle(E.acct) }, '·');
  const knob = h('button', { class: 'quick-knob', type: 'button', 'aria-haspopup': 'dialog', 'aria-label': 'Quick: pages, car, period and display', title: 'Quick', onclick: openQuick,
    cover: ['shell.nav.mobile_menu', 'web.shell.menu-toggle'] }, icon('knob'));

  replace($('dash'),
    h('div', { class: 'dash-side dash-side--l' }, brand, left),
    h('div', { class: 'radio', role: 'group', 'aria-label': 'Radio: this page' }, E.seekL, E.face, E.seekR),
    h('div', { class: 'dash-side dash-side--r' }, right, h('span', { class: 'dash-end' }, E.acct, knob)));
  E.bandBtns = [...$('dash').querySelectorAll('.band-btn')];
}

// swiping the radio face left or right tunes like the arrows
function swipe(el) {
  let x0 = null, y0 = null, moved = false;
  el.addEventListener('pointerdown', (e) => { if (e.pointerType !== 'mouse') { x0 = e.clientX; y0 = e.clientY; moved = false; } });
  el.addEventListener('pointerup', (e) => {
    if (x0 == null) return;
    const dx = e.clientX - x0, dy = e.clientY - y0;
    x0 = null;
    if (Math.abs(dx) > 40 && Math.abs(dx) > Math.abs(dy) * 1.5) { moved = true; tuneBy(dx < 0 ? 1 : -1); }
  });
  el.addEventListener('click', (e) => { if (moved) { e.stopImmediatePropagation(); e.preventDefault(); moved = false; } }, true);
}

function tuneBy(step) {
  const next = tune(shown.page || 'fleet', step);
  if (next) state.navigate(state.linkTo(next.id));
}

function showRadio(info, car) {
  const s = info ? station(info.id) : null;
  const b = s ? band(s.band || station(s.parent)?.band) : null;
  const ch = s ? channel(s.id) : null;
  E.rBand.textContent = [ch ? `CH ${String(ch).padStart(2, '0')}` : 'CH --', b ? b.name.toUpperCase() : ''].filter(Boolean).join(' · ');
  E.rName.textContent = s ? (s.parent ? `${s.name}` : s.name) : 'Not found';
  E.rSub.textContent = car ? `${car.name} · ${stateWord(car.now?.state)}` : s ? s.line : 'No such page';
  const prev = tune(shown.page || 'fleet', -1), next = tune(shown.page || 'fleet', 1);
  E.seekL.setAttribute('aria-label', `Tune back to ${fullName(prev)} ([)`);
  E.seekL.title = `${fullName(prev)} ([)`;
  E.seekR.setAttribute('aria-label', `Tune on to ${fullName(next)} (])`);
  E.seekR.title = `${fullName(next)} (])`;
  for (const btnEl of E.bandBtns) btnEl.classList.toggle('is-on', !!b && btnEl.dataset.band === b.id);
}

function showLive(info) {
  live = info || live;
  const on = live.state === 'open';
  const late = ['reconnecting', 'closed', 'unsupported'].includes(live.state);
  E.rLive.classList.toggle('is-on', on);
  E.rLive.classList.toggle('is-late', late);
  E.rLive.querySelector('.radio-live-text').textContent = on ? 'LIVE' : late ? 'NO SIGNAL' : 'TUNING';
  E.rLive.title = on ? 'Pages update by themselves when a new log has been read (about a minute after the car records it).'
    : late ? `Live updates paused${live.retryInMs ? `, trying again in ${Math.ceil(live.retryInMs / 1000)} s` : ''}.` : 'Connecting to live updates.';
}

/* ------------------------------------------------------------------ the phone tuner and the Quick sheet */

function buildTuner() {
  const track = h('div', { class: 'tuner-track' }, BANDS.map((b) => [
    h('span', { class: 'tune-band', 'aria-hidden': 'true' }, b.name),
    inBand(b.id).map((s) => h('a', { class: 'tune', href: '/', dataset: { page: s.id } }, icon(s.icon), s.short || s.name)),
  ]));
  replace($('tuner'), track);
  E.track = track;
  E.tunes = [...track.querySelectorAll('.tune')];
}

function showTuner() {
  const here = shown.page && (station(shown.page)?.parent || shown.page);
  let on = null;
  for (const a of E.tunes) {
    a.setAttribute('href', state.linkTo(a.dataset.page));
    if (a.dataset.page === here) { a.setAttribute('aria-current', 'page'); on = a; } else a.removeAttribute('aria-current');
  }
  // the station tuned to sits in the middle of the bar
  if (on && E.track.clientWidth) {
    const left = on.offsetLeft - (E.track.clientWidth - on.offsetWidth) / 2;
    E.track.scrollTo({ left: Math.max(0, left), behavior: 'instant' });
  }
}

function openQuick() {
  let d = null;
  const close = () => d?.close();
  const panel = accountPanel(close);
  const tile = (ic, text, f) => h('button', { class: 'btn', type: 'button', onclick: () => { close(); f(); } }, icon(ic), h('span', null, text));
  d = openDialog({
    title: 'Quick', cls: 'quick', onClose: panel.stop,
    body: [
      h('div', { class: 'quick-grid' },
        tile('search', 'Find a page', openScan),
        tile('car', 'Switch car', openCarPicker),
        tile('grid', 'All cars', () => state.navigate('/')),
        tile('sliders', 'Settings', () => state.navigate(state.linkTo('settings-display')))),
      panel.el,
    ],
  });
}

/* ------------------------------------------------------------------ the console row */

function buildConsole() {
  E.cart = h('button', { class: 'cart', type: 'button', 'aria-haspopup': 'dialog', title: `Switch car (${CAR_KEYS})`, onclick: openCarPicker },
    icon('car'), lamp('unknown'), h('span', { class: 'cart-text' }, h('span', { class: 'cart-name' }, 'Choose a car'), h('span', { class: 'cart-sub' }, 'No car open')),
    icon('chev-d', 'i--sm'));
  E.clock = h('span', { class: 'clock', hidden: true, title: "The car's own time" }, icon('clock'), h('span', { class: 'clock-text' }));
  replace($('console'), E.cart, h('span', { class: 'console-grow' }), periodDial(), E.clock);
}

function showCar(car) {
  const name = E.cart.querySelector('.cart-name'), sub = E.cart.querySelector('.cart-sub'), l = E.cart.querySelector('.lamp');
  if (!car) {
    name.textContent = 'Choose a car';
    sub.textContent = 'No car open';
    l.dataset.state = 'unknown';
    l.classList.remove('is-stale');
    E.cart.setAttribute('aria-label', 'Choose a car');
    return;
  }
  name.textContent = car.name;
  sub.textContent = [carLine(car), carStatus(car)].filter(Boolean).join(' · ');
  l.dataset.state = car.now?.state || 'unknown';
  l.classList.toggle('is-stale', !!car.now?.stale);
  E.cart.setAttribute('aria-label', `Car: ${car.name}, ${carStatus(car)}. Switch car`);
}

function tickClock() {
  if (!E.clock) return;
  E.clock.hidden = !shown.tz;
  if (shown.tz) E.clock.querySelector('.clock-text').textContent = `${formatTime(api.now(), shown.tz)} ${zoneName(api.now(), shown.tz)}`;
}

/* ------------------------------------------------------------------ the footer */

function buildFoot() {
  E.ver = h('span', { class: 'foot-ver' }, 'Sentry Mate');
  E.update = h('a', { class: 'foot-update', hidden: true, target: '_blank', rel: 'noopener noreferrer external', cover: ['shell.footer.update_available', 'web.shell.update-footer'] },
    icon('arrow-r', 'i--sm'), h('span', null, 'A newer version is out'));
  const legal = h('span', { class: 'foot-legal', cover: ['shell.footer.legal', 'web.shell.legal-footer'] },
    'Bundled parts keep their own licences: MapLibre GL JS (BSD-3-Clause), uPlot (MIT), Orbitron and Chakra Petch (SIL Open Font License 1.1), JetBrains Mono (SIL Open Font License 1.1). ',
    h('a', { href: '/help#licences' }, 'Licences'), ' · ', h('a', { href: '/help#credits' }, 'Data credits'));
  replace($('foot'), h('div', { class: 'foot-box' }, E.ver, legal, E.update));
}

function showRelease(items) {
  const n = items.find((x) => x.code === 'release_available');
  E.update.hidden = !n;
  if (!n) return;
  E.update.querySelector('span').textContent = n.message || 'A newer version is out';
  const link = safeUrl(n.detail?.url) || safeUrl(n.fix_link) || '/help';
  E.update.setAttribute('href', link);
  if (link.startsWith('/')) { E.update.removeAttribute('target'); E.update.setAttribute('rel', 'noopener'); }
}

async function loadAbout() {
  try {
    const a = await api.get('about', null, { maxAgeMs: 600000 });
    E.ver.textContent = `Sentry Mate ${a.version} · ${a.ui_pack?.name || 'Horizon'} ${a.ui_pack?.version || ''}`.trim();
  } catch { /* the plain name stays */ }
}

/* ------------------------------------------------------------------ the account knob, live events, keys */

function showAccount() {
  const me = getMe();
  E.acct.textContent = (me?.name || '·').slice(0, 1).toUpperCase();
}

function startEvents() {
  stopEvents?.();
  stopEvents = api.events((ev) => {
    if (ev.type === 'prefs' && ev.data) usePrefs(ev.data);
    else if (ev.type === 'notice') loadNotices();
    else if ((ev.type === 'now' && shown.car && ev.data?.car === shown.car.id) || (ev.type === 'fleet' && shown.car)) refreshCar();
  }, { types: ['prefs', 'notice', 'fleet', 'now'] });
}
let carTimer = 0;
function refreshCar() {
  if (carTimer) return; // at most one look every 2 s, however fast live values come
  carTimer = setTimeout(async () => {
    carTimer = 0;
    if (!shown.car) return;
    try { shown.car = await api.get(api.apiPath`cars/${shown.car.id}`); showCar(shown.car); showRadio(station(shown.page), shown.carOfPage ? shown.car : null); } catch { /* keep the last one */ }
  }, 2000);
}

async function lastCar() {
  const id = state.lastCar();
  try {
    if (id) return await api.get(api.apiPath`cars/${id}`, null, { maxAgeMs: 30000 });
    return (await api.page('cars', { sort: 'custom', limit: 1 })).items[0] || null;
  } catch (e) {
    if (e.status === 404 && id) state.forgetCar(id);
    return null;
  }
}

function onKey(e) {
  if (document.documentElement.dataset.screen === 'bare' || e.altKey) return;
  const t = e.target;
  const typing = t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName));
  if ((e.metaKey || e.ctrlKey) && (e.key === 'k' || e.key === 'K')) { e.preventDefault(); openCarPicker(); return; }
  if (typing || e.metaKey || e.ctrlKey || document.querySelector('dialog[open]')) return;
  if (e.key === '/') { e.preventDefault(); openScan(); }
  else if (e.key === '[') { e.preventDefault(); tuneBy(-1); }
  else if (e.key === ']') { e.preventDefault(); tuneBy(1); }
}

/* ------------------------------------------------------------------ public */

/** Build the frame once, before the first page. */
export function initFrame() {
  E.view = $('view');
  buildDash();
  buildTuner();
  buildConsole();
  buildFoot();
  initNotices($('notices'));
  initToasts($('toasts'));
  onNotices(showRelease);
  api.on('events', showLive);
  showLive(api.eventsState());
  document.addEventListener('keydown', onKey);
  setInterval(() => { tickClock(); refreshAgo(); }, 15000);
  onSession((s) => {
    showAccount();
    if (s.signedIn && !started) { started = true; loadAbout(); loadNotices(); startEvents(); }
    if (!s.signedIn) { started = false; stopEvents?.(); stopEvents = null; }
  });
}

/** A page renamed its car or moved its time zone: the cartridge, radio and clock follow at once. */
export function carChanged(car) {
  if (!car || shown.car?.id !== car.id) return;
  shown.car = { ...shown.car, ...car };
  if (car.time_zone) shown.tz = car.time_zone;
  tickClock();
  showCar(shown.car);
  showRadio(station(shown.page), shown.car);
}

/** The router tells the frame what is shown: { page, info, car, period, tz }. */
export async function update({ page, info, car, period, tz = null }) {
  shown = { page, car, tz: tz || car?.time_zone || null, carOfPage: !!car };
  showRadio(info, car);
  showTuner();
  showPeriod(period, shown.tz);
  tickClock();
  if (car) { showCar(car); return; }
  if (page === 'sign-in' || page === 'setup') return;
  // a page without a car shows the last car in the cartridge, and its clock
  const my = state.current();
  const c = await lastCar();
  if (state.current() !== my) return;
  shown.car = c;
  showCar(c);
  if (c && !shown.tz) { shown.tz = c.time_zone; tickClock(); }
}

export { DIAL };
