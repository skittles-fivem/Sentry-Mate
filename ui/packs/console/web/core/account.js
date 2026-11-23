// The account menu: who is signed in, the Interface size on this device (kept in this browser only), the
// account's Interface size and Follow screen size, theme, sign out. The account's choices apply at once and
// are saved to the person's preferences (they follow them to other devices).

import { h, replace } from '../shared/dom.js';
import * as state from '../shared/state.js';
import { openDialog } from './dialog.js';
import { getMe, savePrefs } from './prefs.js';
import { getInterface, applyInterface, applyDeviceSize, stepSize, getTheme, applyTheme } from './display.js';
import { deviceSizeOptions } from '../shared/devicesize.js';
import * as session from './session.js';
import { toast } from './toast.js';
import { errorWords } from './states.js';

const save = (patch, undo) => savePrefs(patch).catch((e) => { undo?.(); toast(`Not saved: ${errorWords(e).text}`, { kind: 'error' }); });

export function openAccountMenu() {
  const me = getMe();
  const keyRow = (label, ...keys) => h('div', { class: 'am-row' }, h('span', { class: 'am-k' }, `${label}:`), h('div', { class: 'keys' }, keys));
  const key = (text, pressed, onclick, attrs = {}) => h('button', { class: 'key', type: 'button', 'aria-pressed': pressed == null ? null : String(pressed), onclick, ...attrs }, text);

  const body = h('div', { class: 'am' });
  function paint() {
    const i = getInterface();
    const th = getTheme().choice;
    replace(body,
      h('dl', { class: 'kv kv--one' },
        h('div', { class: 'kv-row' }, h('dt', { class: 'kv-k' }, 'user'), h('dd', { class: 'kv-v' }, me?.name || '-')),
        h('div', { class: 'kv-row' }, h('dt', { class: 'kv-k' }, 'role'), h('dd', { class: 'kv-v' }, me?.role === 'admin' ? 'admin: every car and setting' : 'watch: sees, does not change'))),
      keyRow('this device',
        ...deviceSizeOptions().map((o) => key(o.value === 'auto' ? 'auto' : o.label, String(i.device || 'auto') === o.value, () => { applyDeviceSize(o.value); paint(); }))),
      h('p', { class: 'note' }, i.device ? `This device uses ${i.device} %; the account size below applies on your other devices.` : 'auto: this device uses the account size below. Kept in this browser only.'),
      keyRow('account size',
        key('-', null, () => set(stepSize(-1)), { 'aria-label': 'Smaller', disabled: i.size <= 75 }),
        h('span', { class: 'am-val', 'aria-live': 'polite' }, `${i.size} %`),
        key('+', null, () => set(stepSize(1)), { 'aria-label': 'Bigger', disabled: i.size >= 200 }),
        key('reset', null, () => set(100), { disabled: i.size === 100 })),
      keyRow('follow screen', key('on', i.follow, () => follow(true)), key('off', !i.follow, () => follow(false))),
      keyRow('theme', key('auto', th === 'device', () => theme('device')), key('dark', th === 'dark', () => theme('dark')), key('light', th === 'light', () => theme('light'))),
      h('p', { class: 'note' }, 'The account size, follow screen and theme are saved for your account. Browser zoom (Ctrl + and Ctrl -) works on top.'),
      h('div', { class: 'acts' },
        h('a', { class: 'btn', href: state.linkTo('settings-display'), onclick: () => d.close() }, 'all display settings'),
        h('button', { class: 'btn btn--danger', type: 'button', onclick: signOut }, 'sign out')));
  }
  function set(size) {
    const before = getInterface();
    applyInterface({ size });
    paint();
    save({ interface_size: size }, () => { applyInterface(before); paint(); });
  }
  function follow(on) {
    const before = getInterface();
    applyInterface({ follow: on });
    paint();
    save({ follow_screen: on }, () => { applyInterface(before); paint(); });
  }
  function theme(choice) {
    const before = getTheme().choice;
    applyTheme(choice);
    paint();
    save({ theme: choice }, () => { applyTheme(before); paint(); });
  }
  async function signOut() {
    try { await session.signOut(); } catch (e) { toast(`Could not sign out: ${errorWords(e).text}`, { kind: 'error' }); return; }
    d.close();
    toast('Signed out.', { next: true, kind: 'info' });
    state.navigate('/sign-in', { replace: true });
  }
  paint();
  const d = openDialog({ title: 'account', cls: 'dlg--account', body });
  body.querySelector('button')?.focus();
  return d;
}
