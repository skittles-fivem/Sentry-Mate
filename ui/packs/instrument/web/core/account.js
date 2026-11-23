// The account menu at the foot of the sidebar: who is signed in, the Interface size and Follow screen
// size, the theme, a link to the Display settings, and sign out. Changes apply at once and are saved to
// the person's preferences (core/prefs.js), so they follow them to every screen. Above them, the Interface
// size on this device (Automatic or a fixed step), kept in this browser only (core/display.js).

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { icon } from './icons.js';
import { getMe, getPrefs, savePrefs, onPrefs } from './prefs.js';
import { getInterface, autoPercent, accountPercent, SIZES, onInterface, getTheme, applyDeviceSize } from './display.js';
import { deviceSizeOptions, deviceSizeLabel, deviceSizeNote } from '../shared/devicesize.js';
import { flash } from './flash.js';
import { placePopover } from './popover.js';
import { signOut } from './session.js';

let pop = null, anchor = null;

/** One line on what the size means on this screen: 'Auto is 145 % here, so 100 % shows at 145 %.' */
export function sizeNote() {
  const i = getInterface();
  const a = autoPercent();
  if (i.device) return `This device uses its own size (${i.device} %), so your account's ${i.size} % applies on your other devices only.`;
  if (!i.follow) return `Follow screen size is off: ${i.size} % is ${i.size} % of the standard size on every screen.`;
  if (a === 100) return `This screen uses the standard size, so ${i.size} % shows at ${i.size} %.`;
  return `This screen is large, so Auto adds ${a - 100} %: ${i.size} % shows at ${accountPercent()} % of the standard size.`;
}

/** One line on the Interface size on this device. */
export const deviceNote = () => { const i = getInterface(); return deviceSizeNote(i.device, { account: i.size, shown: accountPercent() }); };

/**
 * The Interface size on this device: Automatic and the fixed steps as radio buttons. Kept in this browser
 * only (never saved to the account). name: a unique radio group name.
 */
export function devicePicker(name, { grid = false } = {}) {
  const now = getInterface().device ? String(getInterface().device) : 'auto';
  const group = h('div', { class: ['size-picker', 'device-picker', grid && 'size-picker--grid'], role: 'radiogroup', 'aria-label': 'Interface size on this device' },
    deviceSizeOptions().map((o) => h('label', { class: 'size-opt' },
      h('input', { type: 'radio', name, value: o.value, checked: o.value === now, onchange: () => applyDeviceSize(o.value) }),
      h('span', null, o.label))));
  const sync = (i) => { const v = i.device ? String(i.device) : 'auto'; for (const r of group.querySelectorAll('input')) r.checked = r.value === v; };
  return { el: group, sync };
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
  const dev = devicePicker('acct-device', { grid: true });
  const devOut = h('output', { for: 'acct-device' }, deviceSizeLabel(getInterface().device));
  const devNote = h('p', { class: 'setting-note' }, deviceNote());
  const size = sizePicker('acct-size', { grid: true });
  const follow = followSwitch('acct-follow');
  const theme = themeButtons();
  const out = h('output', { for: 'acct-size' }, `${getInterface().size} %`);
  const noteEl = h('p', { class: 'setting-note' }, sizeNote());
  const offI = onInterface((i) => { dev.sync(i); devOut.textContent = deviceSizeLabel(i.device); devNote.textContent = deviceNote(); size.sync(i); follow.sync(i); out.textContent = `${i.size} %`; noteEl.textContent = sizeNote(); });
  const offP = onPrefs((p) => theme.sync(p));
  pop.addEventListener('toggle', (e) => { if (e.newState === 'closed') { offI(); offP(); } }, { once: true });
  replace(pop,
    h('div', { class: 'account-menu-head' }, h('b', null, me?.name || 'Signed in'), h('span', null, roleText(me))),
    h('div', { class: 'account-menu-label' }, h('span', { id: 'acct-device-label' }, 'Size on this device'), devOut),
    dev.el,
    devNote,
    h('div', { class: 'account-menu-sep' }),
    h('div', { class: 'account-menu-label' }, h('span', { id: 'acct-size-label' }, 'Interface size (your account)'), out),
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
  pop.addEventListener('toggle', (e) => {
    anchor?.setAttribute('aria-expanded', String(e.newState === 'open'));
    // closed (Escape, a press outside, a link in it): the focus goes back to the button, unless the person
    // put it somewhere else with that press
    if (e.newState === 'closed' && anchor?.isConnected && (!document.activeElement || document.activeElement === document.body || pop.contains(document.activeElement))) anchor.focus();
  });
  // the menu is a dialog at the end of the document: Tab stays in it while it is open, round and round
  pop.addEventListener('keydown', (e) => {
    if (e.key !== 'Tab') return;
    const all = controls();
    if (!all.length) return;
    const first = all[0], last = all[all.length - 1];
    if (e.shiftKey && (document.activeElement === first || !pop.contains(document.activeElement))) { e.preventDefault(); last.focus(); }
    else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
  });
  return pop;
}

/** The menu's controls a Tab stops at, in order (of a group of radio buttons only the one Tab reaches). */
function controls() {
  return [...pop.querySelectorAll('a[href], button, input, select, [tabindex]')]
    .filter((el) => !el.disabled && el.tabIndex >= 0 && el.getClientRects().length > 0);
}

export function toggleAccountMenu(button) {
  anchor = button;
  if (pop.matches(':popover-open')) { pop.hidePopover(); return; }
  fill();
  pop.showPopover();
  // the menu sits at the end of the document, far from its button in Tab order: the focus moves into it
  controls()[0]?.focus({ preventScroll: true });
  placePopover(pop, button, { side: 'above', align: 'start' });
}
