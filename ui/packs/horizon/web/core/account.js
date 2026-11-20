// The account controls: who is signed in, the Interface size and Follow screen size, the look (Dusk, Daybreak,
// as the device), the Background (this browser only), links to the settings, and sign out. Used by the
// account knob's menu on computers and by the Quick sheet on phones. Size and look are the person's
// preferences on the server (core/prefs.js), so they follow them to every screen.

import * as state from '../shared/state.js';
import { h } from '../shared/dom.js';
import { icon, seg } from './ui.js';
import { getMe, getPrefs, savePrefs, onPrefs } from './prefs.js';
import { SIZES, getInterface, onInterface, autoPercent, shownPercent, getTheme } from './display.js';
import { CHOICES, getSky, setSky, onSky } from './sky.js';
import { toast } from './layer.js';
import { signOut } from './session.js';

/** 'Admin · all cars' or 'Watch only · 1 car' */
export function roleLine(me) {
  if (!me) return '';
  const cars = me.cars === 'all' ? 'all cars' : `${me.cars?.length ?? 0} ${me.cars?.length === 1 ? 'car' : 'cars'}`;
  return `${me.role === 'admin' ? 'Admin' : 'Watch only'} · ${cars}`;
}

/** One line on what the size means on this screen. */
export function sizeLine() {
  const i = getInterface();
  const a = autoPercent();
  if (!i.follow) return `Follow screen size is off: ${i.size} % is the same size on every screen.`;
  if (a === 100) return `This screen uses the standard size, so ${i.size} % shows at ${i.size} %.`;
  return `This is a large screen, so Auto adds ${a - 100} %: ${i.size} % shows at ${shownPercent()} %.`;
}

async function save(patch, what) {
  try { await savePrefs(patch); } catch (e) { toast(`${what} is used here, but could not be saved: ${e.message}`, { kind: 'error' }); }
}

/** The Interface size presets. */
export function sizeChoice() {
  const s = seg(SIZES.map((v) => [v, `${v} %`]), getInterface().size, (v) => save({ interface_size: Number(v) }, 'The size'), { label: 'Interface size', cls: 'sizes' });
  return { el: s.el, sync: (i) => s.set(i.size) };
}

/** The Follow screen size switch. */
export function followSwitch(id) {
  const input = h('input', { type: 'checkbox', id, role: 'switch', checked: getInterface().follow, onchange: () => save({ follow_screen: input.checked }, 'Follow screen size') });
  return { el: h('span', { class: 'switch' }, input, h('i', { 'aria-hidden': 'true' })), sync: (i) => { input.checked = i.follow; } };
}

/** The look: Dusk (dark), Daybreak (light), Device. */
export function lookChoice() {
  const now = getPrefs()?.theme || getTheme().choice;
  const s = seg([['dark', 'Dusk', 'moon'], ['light', 'Daybreak', 'sun'], ['device', 'Device', 'monitor']], now, (v) => save({ theme: v }, 'The look'), { label: 'Look' });
  return { el: s.el, sync: (p) => s.set(p?.theme || 'device') };
}

/** The Background: Auto, Full, Calm, Off (this browser only). */
export function skyChoice() {
  const s = seg(CHOICES, getSky().choice, (v) => setSky(v), { label: 'Background' });
  return { el: s.el, sync: (k) => s.set(k.choice) };
}

/** The account panel's content; `onDone` closes the menu or sheet before a link or sign out. */
export function accountPanel(onDone = () => {}) {
  const me = getMe();
  const size = sizeChoice(), follow = followSwitch('hz-follow'), look = lookChoice(), sky = skyChoice();
  const out = h('output', null, `${getInterface().size} %`);
  const line = h('p', { class: 'acct-note' }, sizeLine());
  const skyLine = h('p', { class: 'acct-note' });
  const skyText = (k) => { skyLine.textContent = k.mode === 'still' ? 'Your device asks for less motion, so the grid stands still.' : k.choice === 'auto' ? `Auto is ${k.mode === 'calm' ? 'Calm on this phone' : 'Full on this screen'}. Kept in this browser only.` : 'Kept in this browser only.'; };
  skyText(getSky());
  const offs = [
    onInterface((i) => { size.sync(i); follow.sync(i); out.textContent = `${i.size} %`; line.textContent = sizeLine(); }),
    onPrefs((p) => look.sync(p)),
    onSky((k) => { sky.sync(k); skyText(k); }),
  ];
  const link = (href, ic, text) => h('a', { class: 'acct-link', href, onclick: onDone }, icon(ic), text);
  const el = h('div', { class: 'acct-panel' },
    h('div', { class: 'acct-who' }, h('p', { class: 'acct-name' }, me?.name || 'Signed in'), h('p', { class: 'acct-role' }, roleLine(me))),
    h('div', { class: 'acct-sep' }),
    h('p', { class: 'acct-label' }, h('span', null, 'Interface size'), out), size.el,
    h('div', { class: 'switch-row' }, h('label', { for: 'hz-follow' }, 'Follow screen size'), follow.el), line,
    h('div', { class: 'acct-sep' }),
    h('p', { class: 'acct-label' }, h('span', null, 'Look')), look.el,
    h('p', { class: 'acct-label' }, h('span', null, 'Background')), sky.el, skyLine,
    h('div', { class: 'acct-sep' }),
    link(state.buildUrl('settings-display'), 'monitor', 'Display and units'),
    link(state.buildUrl('settings-server'), 'user', 'Accounts and sessions'),
    h('button', { class: 'acct-link', type: 'button', onclick: async () => { onDone(); await doSignOut(); } }, icon('signout'), 'Sign out'));
  return { el, stop: () => offs.forEach((f) => f()) };
}

async function doSignOut() {
  try {
    await signOut();
    toast('Signed out.', { next: true });
    state.navigate('/sign-in', { replace: true });
  } catch (e) { toast(`Could not sign out: ${e.message}`, { kind: 'error' }); }
}
