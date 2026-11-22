// The account menu at the right of the header: who is signed in, the Interface size and Follow screen
// size, the theme, a link to the Display settings, and sign out. Changes apply at once and are saved to
// the person's preferences (core/prefs.js), so they follow them to every screen.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { icon } from './icons.js';
import { getMe, getPrefs, savePrefs, onPrefs } from './prefs.js';
import { getInterface, autoPercent, effectivePercent, SIZES, onInterface, getTheme } from './display.js';
import { flash } from './flash.js';
import { placePopover } from './popover.js';
import { signOut } from './session.js';

let pop = null, anchor = null;

/** One line on what the size means on this screen: 'Auto is 145 % here, so 100 % shows at 145 %.' */
export function sizeNote() {
  const i = getInterface();
  const a = autoPercent();
  if (!i.follow) return `Follow screen size is off: ${i.size} % is ${i.size} % of the standard size on every screen.`;
  if (a === 100) return `This screen uses the standard size, so ${i.size} % shows at ${i.size} %.`;
  return `This screen is large, so Auto adds ${a - 100} %: ${i.size} % shows at ${effectivePercent()} % of the standard size.`;
}

/**
 * The Interface size picker: preset sizes as radio buttons (arrow keys move between them).
 * Used here and on Settings: Display and units. name: a unique radio group name.
 */
export function sizePicker(name, { grid = false } = {}) {
  const now = getInterface().size;
  const group = h('div', { class: ['size-picker', grid && 'size-picker--grid'], role: 'radiogroup', 'aria-label': 'Interface size' },
    SIZES.map((s) => h('label', { class: 'size-opt' },
      h('input', { type: 'radio', name, value: String(s), checked: s === now, onchange: () => setSize(s) }),
      h('span', null, `${s} %`))));
  const sync = (i) => { for (const r of group.querySelectorAll('input')) r.checked = Number(r.value) === i.size; };
  return { el: group, sync };
}

/** A "Follow screen size" switch (a real checkbox). */
export function followSwitch(id) {
  const input = h('input', { type: 'checkbox', id, role: 'switch', checked: getInterface().follow, onchange: () => setFollow(input.checked) });
  return { el: h('span', { class: 'switch' }, input, h('i', { 'aria-hidden': 'true' })), input, sync: (i) => { input.checked = i.follow; } };
}

export async function setSize(size) {
  try { await savePrefs({ interface_size: size }); } catch (e) { flash(`The size is used here, but could not be saved: ${e.message}`, { kind: 'error' }); }
}
export async function setFollow(on) {
  try { await savePrefs({ follow_screen: !!on }); } catch (e) { flash(`Could not save Follow screen size: ${e.message}`, { kind: 'error' }); }
}
export async function setTheme(t) {
  try { await savePrefs({ theme: t }); } catch (e) { flash(`Could not save the theme: ${e.message}`, { kind: 'error' }); }
}

/** Theme buttons: Light, Dark, Device. */
export function themeButtons() {
  const choice = getPrefs()?.theme || getTheme().choice;
  const seg = h('div', { class: 'seg', role: 'group', 'aria-label': 'Theme' },
    [['light', 'Light', 'sun'], ['dark', 'Dark', 'moon'], ['device', 'Device', 'monitor']].map(([v, l, ic]) =>
      h('button', { class: 'seg-btn', type: 'button', 'aria-pressed': String(choice === v), dataset: { theme: v }, onclick: () => setTheme(v) }, icon(ic, 'ico--sm'), l)));
  const sync = (p) => { for (const b of seg.querySelectorAll('[data-theme]')) b.setAttribute('aria-pressed', String((p?.theme || 'device') === b.dataset.theme)); };
  return { el: seg, sync };
}

function fill() {
  const me = getMe();
  const size = sizePicker('acct-size', { grid: true });
  const follow = followSwitch('acct-follow');
  const theme = themeButtons();
  const out = h('output', { for: 'acct-size' }, `${getInterface().size} %`);
  const noteEl = h('p', { class: 'setting-note' }, sizeNote());
  const offI = onInterface((i) => { size.sync(i); follow.sync(i); out.textContent = `${i.size} %`; noteEl.textContent = sizeNote(); });
  const offP = onPrefs((p) => theme.sync(p));
  pop.addEventListener('toggle', (e) => { if (e.newState === 'closed') { offI(); offP(); } }, { once: true });
  replace(pop,
    h('div', { class: 'account-menu-head' }, h('b', null, me?.name || 'Signed in'), h('span', null, roleText(me))),
    h('div', { class: 'account-menu-label' }, h('span', { id: 'acct-size-label' }, 'Interface size'), out),
    size.el,
    h('div', { class: 'switch-row' }, h('label', { for: 'acct-follow' }, 'Follow screen size'), follow.el),
    noteEl,
    h('div', { class: 'account-menu-sep' }),
    h('div', { class: 'account-menu-label' }, 'Theme'),
    theme.el,
    h('div', { class: 'account-menu-sep' }),
    h('a', { class: 'account-menu-link', href: state.buildUrl('settings-display'), onclick: () => pop.hidePopover() }, icon('monitor'), 'Display and units'),
    h('a', { class: 'account-menu-link', href: state.buildUrl('settings-server'), onclick: () => pop.hidePopover() }, icon('user'), 'Accounts and sessions'),
    h('button', { class: 'account-menu-link', type: 'button', onclick: doSignOut }, icon('signout'), 'Sign out'));
}

async function doSignOut() {
  pop.hidePopover();
  try {
    await signOut();
    flash('Signed out.', { next: true });
    state.navigate('/sign-in', { replace: true });
  } catch (e) {
    flash(`Could not sign out: ${e.message}`, { kind: 'error' });
  }
}

/** 'Admin · all cars' or 'Watch only · 1 car'. */
export function roleText(me) {
  if (!me) return '';
  const role = me.role === 'admin' ? 'Admin' : 'Watch only';
  const cars = me.cars === 'all' ? 'all cars' : `${me.cars.length} ${me.cars.length === 1 ? 'car' : 'cars'}`;
  return `${role} · ${cars}`;
}

/** The menu element; the frame puts it in the page once and calls toggleAccountMenu(button). */
export function accountMenu() {
  pop = h('div', { class: 'account-menu popover', popover: 'auto', role: 'dialog', 'aria-label': 'Your account and display' });
  pop.addEventListener('toggle', (e) => { anchor?.setAttribute('aria-expanded', String(e.newState === 'open')); });
  return pop;
}

export function toggleAccountMenu(button) {
  anchor = button;
  if (pop.matches(':popover-open')) { pop.hidePopover(); return; }
  fill();
  pop.showPopover();
  placePopover(pop, button, { side: 'below', align: 'end' });
}
