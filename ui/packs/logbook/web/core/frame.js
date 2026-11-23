// Sentry Mate, "Logbook" UI pack: the frame every page shares.
//
// The masthead (home link, car switcher, period picker, clock, the "Aa" display menu, the account menu),
// the navigation (a Pages menu grouped by task with a line per page, the current group's pages as tabs,
// Places and Settings, and a type-to-find box), the notices bar (each notice with its cause and fix),
// the flash notes and the footer (version, licence and notices, an opt-in release note, map credit).
// The router calls update(view) for every page; everything else here keeps itself up to date (live
// notices and preferences from /api/v1/events, ages and the clock on a timer).

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { clearServiceWorker } from '../shared/offline.js';
import * as state from '../shared/state.js';
import { formatTime, zoneName } from '../shared/time.js';
import { GROUPS, PAGES, findPages, groupOf } from './pages.js';
import { icon, refreshAges, button } from './ui.js';
import { popover } from './dialog.js';
import { choice, switchField } from './form.js';
import { createSwitcher } from './switcher.js';
import { createPeriodPicker } from './period.js';
import { currentPrefs, savePrefs, applyPrefs, onPrefs } from './prefs.js';
import { SIZES, sizeState, setSize, sizeNote, display } from './display.js';
import { flash } from './flash.js';
import { buildLabel, buildTitle, releaseDismissed, dismissRelease } from '../shared/release.js';
import { isTmNotice, tmNoticeFix, TM_NOTICE_ACTION } from '../shared/teslamate.js';

const browserZone = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC'; } catch { return 'UTC'; } })();

let me = null;
let about = null;
let view = { page: null, info: null, car: null, tz: browserZone, period: null };
const parts = {};

/* ------------------------------------------------------------------ saving preferences */

/** Save a preference change: applied at once, saved to the account, put back if the server says no. */
export async function savePref(patch, { quiet = true } = {}) {
  const before = currentPrefs();
  if (before) applyPrefs({ ...before, ...patch, units: { ...before.units, ...(patch.units || {}) } });
  try {
    await savePrefs(patch);
    if (!quiet) flash('Saved. Your choice applies on every page and device you sign in on.');
  } catch (err) {
    if (before) applyPrefs(before);
    flash(`Not saved: ${err.message || err}`, { kind: 'error' });
    throw err;
  }
}

/* ------------------------------------------------------------------ masthead tools */

function displayMenu() {
  const trigger = h('button', { type: 'button', class: 'size-trigger', 'aria-haspopup': 'dialog', title: 'Display: theme, units and interface size' },
    h('span', { class: 'size-trigger-glyph', 'aria-hidden': 'true' }, 'Aa'), h('span', { class: 'size-trigger-value' }), h('span', { class: 'sr-only' }, 'Display settings'));
  const p = currentPrefs() || {};
  const theme = choice({ label: 'Theme', name: 'theme', value: p.theme || 'device', options: [['light', 'Light'], ['dark', 'Dark'], ['device', 'Device']], onchange: (v) => savePref({ theme: v }).catch(() => {}) });
  const units = choice({ label: 'Units', name: 'units', value: (p.units && p.units.distance) || 'km', options: [['km', 'km · °C · bar'], ['mi', 'mi · °F · psi']],
    onchange: (v) => savePref({ units: v === 'mi' ? { distance: 'mi', temperature: 'f', pressure: 'psi' } : { distance: 'km', temperature: 'c', pressure: 'bar' } }).catch(() => {}) });
  const size = choice({ label: 'Interface size', name: 'interface_size', value: sizeState().size, cls: 'size-options', options: SIZES.map((s) => [s, `${s}%`]), onchange: (v) => pickSize(Number(v)) });
  const follow = switchField({ label: 'Follow screen size', checked: sizeState().follow, onchange: (on) => pickFollow(on), hint: 'Larger on big screens, such as a 4K monitor.' });
  const noteEl = h('p', { class: 'size-pop-note' });
  const more = h('a', { class: 'size-pop-more', href: '/settings/display' }, 'More in Settings: Display and units');
  const panel = h('div', { class: 'pop size-pop', id: 'display-pop', role: 'dialog', 'aria-label': 'Display' }, theme, units, size, follow, noteEl, more);
  const wrap = h('div', { class: 'pop-wrap size-menu' }, trigger, panel);
  const pop = popover(trigger, panel, { focus: '[aria-checked="true"]' });
  more.addEventListener('click', () => pop.close());
  const sync = () => {
    const pr = currentPrefs() || {};
    theme.set(pr.theme || 'device');
    units.set((pr.units && pr.units.distance) || 'km');
    const s = sizeState();
    size.set(s.size);
    follow.set(s.follow);
    trigger.querySelector('.size-trigger-value').textContent = s.size === 100 ? '' : `${s.size}%`;
    noteEl.textContent = sizeNote();
  };
  onPrefs(sync);
  display.onChange(sync);
  sync();
  return { el: wrap, sync };
}

let sizeTimer = 0;
function pickSize(v) {
  setSize({ size: v });
  clearTimeout(sizeTimer);
  sizeTimer = setTimeout(() => savePref({ interface_size: v }).catch(() => {}), 400);
}
function pickFollow(on) {
  setSize({ follow: on });
  savePref({ follow_screen: on }).catch(() => {});
}

function accountMenu() {
  const nameEl = h('span', { class: 'account-name' }, me ? me.name : 'Account');
  const trigger = h('button', { type: 'button', class: 'account-trigger', 'aria-haspopup': 'dialog', 'aria-label': `Account: ${me ? me.name : ''}` }, icon('person'), nameEl);
  const admin = me && me.role === 'admin';
  const panel = h('div', { class: 'pop account-pop', id: 'account-pop', role: 'dialog', 'aria-label': 'Account' },
    h('p', { class: 'pop-title' }, 'Signed in'),
    h('p', { class: 'account-who' }, me ? me.name : '', h('span', { class: 'account-role' }, admin ? 'Admin' : 'Watch only')),
    h('ul', { class: 'pop-links' },
      h('li', null, h('a', { href: '/settings/display' }, 'Display and units')),
      admin ? h('li', null, h('a', { href: '/settings/server' }, 'Accounts, sessions and access')) : null,
      h('li', null, h('a', { href: '/help' }, 'Help and about'))),
    button('Sign out', { onclick: signOut, cls: 'account-signout' }));
  const wrap = h('div', { class: 'pop-wrap account-menu' }, trigger, panel);
  const pop = popover(trigger, panel);
  panel.addEventListener('click', (e) => { if (e.target.closest('a')) pop.close(); });
  return wrap;
}

async function signOut() {
  try { await api.del('session'); } catch { /* signed out either way */ }
  await clearServiceWorker();
  flash('Signed out.', { kind: 'info', next: true });
  location.assign('/sign-in');
}

/* ------------------------------------------------------------------ navigation */

function navigation(navEl) {
  const toggleLabel = h('span', { class: 'menu-toggle-group' }, 'Pages');
  const toggle = h('button', { type: 'button', class: 'menu-toggle', 'aria-haspopup': 'dialog', cover: ['shell.nav.mobile_menu', 'web.shell.menu-toggle'] },
    icon('menu'), toggleLabel, icon('chevronDown', 'icon trigger-caret'));
  const groups = GROUPS.map((g) => h('section', { class: 'menu-group', 'aria-labelledby': `menu-g-${g.id}` },
    h('h2', { class: 'menu-group-name', id: `menu-g-${g.id}` }, g.name),
    h('ul', { class: 'menu-list' }, g.pages.map((id) => h('li', null,
      h('a', { class: 'menu-link', href: '/', dataset: { page: id }, cover: id === 'places' ? ['shell.nav.geofences', 'web.shell.geofences-nav'] : null },
        h('span', { class: 'menu-link-name' }, PAGES[id].name), h('span', { class: 'menu-link-blurb' }, PAGES[id].blurb)))))));
  const menu = h('div', { class: 'pop menu-pop', id: 'menu-pop', role: 'dialog', 'aria-label': 'All pages' },
    h('p', { class: 'menu-intro' }, 'Every page, grouped by task. The car and the period you chose go with you.'),
    h('div', { class: 'menu-groups' }, groups));
  const menuWrap = h('div', { class: 'pop-wrap menu-wrap' }, toggle, menu);
  const menuPop = popover(toggle, menu, { focus: '.menu-link[aria-current="page"], .menu-link' });
  menu.addEventListener('click', (e) => { if (e.target.closest('a')) menuPop.close(); });

  const tabs = h('div', { class: 'nav-tabs' });
  const places = h('a', { class: 'nav-link nav-quick', href: '/places', dataset: { page: 'places' } }, 'Places');
  const settings = h('a', { class: 'nav-link nav-quick', href: '/settings/display', dataset: { page: 'settings-display' }, cover: ['shell.nav.settings', 'web.shell.settings-nav'] }, icon('settings'), 'Settings');

  // Type to find a page: 'drain' finds Battery, 'cost' finds Charges.
  const input = h('input', { type: 'search', class: 'finder-input', placeholder: 'Find a page', role: 'combobox', 'aria-expanded': 'false', 'aria-controls': 'finder-results', 'aria-autocomplete': 'list', autocomplete: 'off', spellcheck: 'false', 'aria-label': 'Find a page by name or what it shows' });
  const results = h('ul', { class: 'pop finder-results', id: 'finder-results', role: 'listbox', 'aria-label': 'Pages found', hidden: true });
  const finder = h('div', { class: 'pop-wrap finder' }, icon('search', 'icon finder-icon'), input, h('kbd', { class: 'kbd finder-kbd' }, '/'), results);
  let found = [], active = 0;
  const showResults = (on) => { results.hidden = !on; input.setAttribute('aria-expanded', String(on)); };
  const setActive = (i) => {
    const opts = [...results.querySelectorAll('[role="option"]')];
    if (!opts.length) return;
    active = (i + opts.length) % opts.length;
    opts.forEach((o, k) => o.setAttribute('aria-selected', String(k === active)));
    input.setAttribute('aria-activedescendant', opts[active].id);
  };
  const fillResults = () => {
    const q = input.value.trim();
    if (!q) { showResults(false); return; }
    found = findPages(q).slice(0, 8);
    results.replaceChildren(...(found.length ? found.map((p, i) => h('li', { role: 'option', id: `finder-opt-${i}`, 'aria-selected': 'false', class: 'finder-option', dataset: { page: p.id } },
      h('span', { class: 'finder-name' }, p.name), h('span', { class: 'finder-group' }, groupOf(p.id)?.name || ''), h('span', { class: 'finder-blurb' }, p.blurb)))
      : [h('li', { class: 'finder-none', role: 'presentation' }, `No page matches “${q}”. Try a word such as drain, cost or tyres.`)]));
    showResults(true);
    setActive(0);
  };
  const go = (id) => { input.value = ''; showResults(false); input.blur(); state.navigate(state.linkTo(id)); };
  input.addEventListener('input', fillResults);
  input.addEventListener('focus', () => { if (input.value.trim()) fillResults(); });
  input.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') { e.preventDefault(); setActive(active + 1); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); setActive(active - 1); }
    else if (e.key === 'Enter') { e.preventDefault(); if (found[active]) go(found[active].id); }
    else if (e.key === 'Escape') { input.value = ''; showResults(false); }
  });
  results.addEventListener('pointerdown', (e) => { const o = e.target.closest('[role="option"]'); if (o) { e.preventDefault(); go(o.dataset.page); } });
  input.addEventListener('blur', () => setTimeout(() => showResults(false), 120));
  document.addEventListener('keydown', (e) => {
    if (e.key !== '/' || e.ctrlKey || e.metaKey || e.altKey) return;
    const t = e.target;
    if (t && (t.isContentEditable || /^(input|textarea|select)$/i.test(t.tagName))) return;
    e.preventDefault();
    input.focus();
  });

  navEl.replaceChildren(h('div', { class: 'wrap nav-row' }, menuWrap, h('span', { class: 'nav-divider', 'aria-hidden': 'true' }), tabs,
    h('div', { class: 'nav-end' }, places, settings, finder)));

  return {
    update(page) {
      const info = PAGES[page];
      const group = info ? groupOf(page) : null;
      const current = info && info.detail ? { drive: 'drives', charge: 'charges' }[page] : page;
      toggleLabel.textContent = group ? group.name : 'Pages';
      for (const a of menu.querySelectorAll('.menu-link')) {
        a.setAttribute('href', safeLink(a.dataset.page));
        if (a.dataset.page === current) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current');
      }
      const ids = group ? group.pages : [];
      tabs.replaceChildren(...ids.map((id) => h('a', { class: 'nav-link', href: safeLink(id), 'aria-current': id === current ? 'page' : null, title: PAGES[id].blurb }, shortName(id))));
      places.setAttribute('href', safeLink('places'));
      navEl.classList.toggle('in-setup', group && group.id === 'setup');
      for (const a of [places, settings]) { if (a.dataset.page === current) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current'); }
    },
    close() { menuPop.close(); showResults(false); },
  };
}

/** Shorter tab names inside a group ('Settings: Cars' -> 'Cars' under Set-up). */
const shortName = (id) => ({ 'settings-cars': 'Cars', 'settings-display': 'Display and units', 'settings-integrations': 'Integrations', 'settings-server': 'Server and access', health: 'Data health', efficiency: 'Efficiency and temperatures', help: 'Help' }[id] || PAGES[id].name);
function safeLink(id) { try { return state.linkTo(id); } catch { return '/'; } }

/* ------------------------------------------------------------------ notices and footer */

const DISMISS_KEY = 'logbook:dismissed';
const dismissed = () => { try { return new Set(JSON.parse(sessionStorage.getItem(DISMISS_KEY) || '[]')); } catch { return new Set(); } };
let notices = [];

function drawNotices() {
  const el = parts.notices;
  const gone = dismissed();
  // The release notice (admins only) is hidden per newer build in this browser; the others for this tab.
  const shown = notices.filter((n) => (n.code === 'release_available' ? !releaseDismissed(n) : !gone.has(`${n.code}:${n.since_ms}`)));
  el.hidden = !shown.length;
  el.replaceChildren(...shown.map((n) => h('div', { class: ['notice', `notice-${n.level || 'info'}`], role: n.level === 'error' ? 'alert' : null },
    h('div', { class: 'wrap notice-row' },
      icon(n.level === 'info' ? 'info' : 'warning', 'icon notice-icon'),
      h('p', { class: 'notice-text' }, n.message),
      n.fix_link ? h('a', { class: 'notice-fix', href: fixHref(n) }, /^tesla_/.test(n.code || '') ? 'Open the Tesla account' : isTmNotice(n) ? TM_NOTICE_ACTION : n.code === 'release_available' ? 'About this release' : fixWords(n.fix_link), icon('arrowRight')) : null,
      h('button', { type: 'button', class: 'notice-close', 'aria-label': n.code === 'release_available' ? 'Hide this notice until a later build' : 'Hide this notice for now', onclick: () => {
        if (n.code === 'release_available') { dismissRelease(n); drawNotices(); return; }
        try { const s = dismissed(); s.add(`${n.code}:${n.since_ms}`); sessionStorage.setItem(DISMISS_KEY, JSON.stringify([...s])); } catch { /* fine */ }
        drawNotices();
      } }, icon('close'))))));
  drawFooter();
}
/** A Tesla account's notice opens that account on Settings: Integrations (#tesla-<id>), a local TeslaMate notice its
 * section there (#teslamate). */
function fixHref(n) {
  const acc = n && n.detail && n.detail.account;
  if (/^tesla_/.test(n.code || '') && acc && /^[A-Za-z0-9_-]+$/.test(acc)) return `${String(n.fix_link).split('#')[0]}#tesla-${acc}`;
  if (isTmNotice(n)) return tmNoticeFix(n);
  return n.fix_link;
}
function fixWords(link) {
  const page = state.parseUrl(link).page;
  return PAGES[page] ? `Fix in ${PAGES[page].name}` : 'Fix it';
}

function drawFooter() {
  const release = notices.find((n) => n.code === 'release_available');
  const pack = about && about.ui_pack ? `${about.ui_pack.name || 'Logbook'} UI pack ${about.ui_pack.version || ''}`.trim() : 'Logbook UI pack';
  parts.footer.replaceChildren(h('div', { class: 'wrap colophon-row' },
    h('p', { class: 'colophon-name', title: buildTitle(about) || null }, `Sentry Mate${about ? ` ${buildLabel(about)}` : ''}`, h('span', { class: 'colophon-sep', 'aria-hidden': 'true' }, ' · '), pack,
      about ? h('span', { class: 'colophon-api' }, ` · API ${about.api_version}`) : null),
    h('p', { class: 'colophon-legal', cover: ['shell.footer.legal', 'web.shell.legal-footer'] },
      'Sentry Mate only reads from your cars. ', h('a', { href: '/help#licence' }, 'Licence and notices'), ' · ', h('a', { href: '/help' }, 'Help')),
    h('p', { class: ['colophon-update', { 'is-empty': !release }], cover: ['shell.footer.update_available', 'web.shell.update-footer'] },
      release ? [h('span', { class: 'update-dot', 'aria-hidden': 'true' }), release.message, ' ', h('a', { href: release.fix_link || '/help' }, 'About this release')] : 'Release check off'),
    h('p', { class: 'colophon-credit' }, about && about.data_credits && about.data_credits[0] ? about.data_credits[0].text : 'Map data © OpenStreetMap contributors')));
}

async function loadNotices() {
  // kept: a page that lists the notices too reuses this answer
  try { notices = (await api.get('notices', null, { keep: true })).items || []; } catch { notices = []; }
  drawNotices();
}

/* ------------------------------------------------------------------ the clock */

function tick() {
  if (parts.clock) {
    const now = api.now();
    parts.clockTime.textContent = formatTime(now, view.tz);
    parts.clockTime.setAttribute('datetime', new Date(now).toISOString());
    parts.clockZone.textContent = zoneName(now, view.tz);
    parts.clockZone.title = `Times are in ${view.tz.replace(/_/g, ' ')} time${view.car ? `, ${view.car.name}'s time zone` : ''}.`;
  }
  refreshAges();
}

/* ------------------------------------------------------------------ start and update */

/**
 * Build the frame (once, after sign-in): mountFrame({ me, about }). Returns { update(view) }, where view is
 * { page, info, car, tz, period }.
 */
export function mountFrame(opts) {
  me = opts.me;
  about = opts.about;
  parts.notices = document.getElementById('notices');
  parts.footer = document.getElementById('colophon');
  parts.switcher = createSwitcher(document.getElementById('car-slot'));
  parts.period = createPeriodPicker(document.getElementById('period-slot'));
  parts.clockTime = h('time', { class: 'clock-time' });
  parts.clockZone = h('abbr', { class: 'clock-zone' });
  parts.clock = h('p', { class: 'clock' }, parts.clockTime, ' ', parts.clockZone);
  parts.display = displayMenu();
  document.getElementById('tools-slot').replaceChildren(parts.clock, parts.display.el, accountMenu());
  parts.nav = navigation(document.getElementById('primary-nav'));
  drawFooter();
  loadNotices();
  tick();
  setInterval(tick, 20000);

  // Live: notices and preferences changed elsewhere (another tab, an admin's server defaults).
  api.events((ev) => {
    if (ev.type === 'notice') {
      loadNotices(); // the whole list again: a notice may also have gone (a certificate renewed)
    } else if (ev.type === 'prefs' && ev.data) {
      applyPrefs(ev.data);
    }
  }, { types: ['notice', 'prefs'] });
  api.on('network', (n) => document.documentElement.classList.toggle('is-offline', !n.ok));
  return { update };
}

function update(v) {
  view = { ...view, ...v, tz: v.tz || browserZone };
  parts.switcher.update({ car: v.car, cars: v.carsCount });
  parts.period.update({ info: v.info, period: v.period, tz: view.tz });
  parts.nav.update(v.page);
  parts.nav.close();
  tick();
}

/** The account signed in (from /api/v1/me). */
export const currentMe = () => me;
/** The server's /about answer. */
export const currentAbout = () => about;
