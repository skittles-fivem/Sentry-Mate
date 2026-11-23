// Sentry Mate, "Pebble" UI pack: the account sheet, opened from the round button at the top right of every page.
//
// Who is signed in, the look of this pack (theme Auto / Light / Dark, Interface size 50 to 200 %, Follow screen
// size), quick links and Sign out. Every choice is saved to the account at once (PATCH /me/preferences), so it
// follows the person to every device; a choice the server refuses is put back with a note.

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { clearServiceWorker } from '../shared/offline.js';
import { createSheet, sheetRow, sheetGroup } from './sheet.js';
import { segmented, toggle } from './form.js';
import { btn, initial } from './ui.js';
import { currentPrefs, savePref, onPrefs } from './prefs.js';
import { SIZES, SIZE_MIN, SIZE_MAX, sizeState, setSize, sizeNote, display } from './display.js';
import { toast } from './toast.js';

let sheet = null;
let me = null;

const notSaved = (err) => toast(`Not saved: ${(err && err.message) || 'the server did not answer'}.`, { kind: 'error' });

let sizeTimer = 0;
function pickSize(v) {
  setSize({ size: v });
  clearTimeout(sizeTimer);
  sizeTimer = setTimeout(() => savePref({ interface_size: sizeState().size }).catch(notSaved), 400);
}

function build() {
  const p = currentPrefs() || {};
  const admin = me && me.role === 'admin';
  const theme = segmented({ label: 'Theme', name: 'theme', value: p.theme || 'device', options: [['device', 'Auto'], ['light', 'Light'], ['dark', 'Dark']], onchange: (v) => savePref({ theme: v }).catch(notSaved) });
  const sizeValue = h('output', { class: 'size-value', 'aria-live': 'polite' });
  const smaller = btn('Smaller', { kind: 'soft', icon: 'minus', iconOnly: true, onclick: () => stepSize(-1) });
  const bigger = btn('Bigger', { kind: 'soft', icon: 'plus', iconOnly: true, onclick: () => stepSize(1) });
  const stepper = h('div', { class: 'seg-field' }, h('span', { class: 'label' }, 'Interface size'),
    h('div', { class: 'stepper', role: 'group', 'aria-label': 'Interface size' }, smaller, sizeValue, bigger));
  const follow = toggle({ label: 'Follow screen size', sub: 'Larger on big screens, such as a 4K monitor.', checked: sizeState().follow, onchange: (on) => { setSize({ follow: on }); savePref({ follow_screen: on }).catch(notSaved); } });
  const sizeLine = h('p', { class: 'sheet-note' });
  function stepSize(d) {
    const now = sizeState().size;
    const next = d > 0 ? (SIZES.find((s) => s > now) ?? SIZE_MAX) : ([...SIZES].reverse().find((s) => s < now) ?? SIZE_MIN);
    pickSize(next);
  }
  const who = h('div', { class: 'who' },
    h('span', { class: 'avatar avatar-lg', 'aria-hidden': 'true' }, initial(me && me.name)),
    h('span', { class: 'row-text' }, h('span', { class: 'who-name' }, me ? me.name : 'Signed in'), h('span', { class: 'row-sub' }, admin ? 'Admin: every car and setting' : 'Watch only: the cars shared with you')));
  sheet = createSheet({ title: 'You', cls: 'account-sheet' });
  sheet.body.append(
    who,
    h('section', { class: 'rows' }, h('h3', { class: 'rows-name' }, 'Look'),
      h('div', { class: 'rows-card' }, theme, stepper, sizeLine), h('div', { class: 'rows-list' }, follow)),
    sheetGroup('Go to', [
      sheetRow({ href: '/settings/display', icon: 'sun', name: 'Display and units', sub: 'Units, language, UI pack' }),
      admin ? sheetRow({ href: '/settings/server', icon: 'server', name: 'Server and access', sub: 'Accounts, sessions, moving' }) : null,
      sheetRow({ href: '/help', icon: 'help', name: 'Help and about' }),
    ].filter(Boolean)),
    h('div', { class: 'sheet-actions' }, btn('Sign out', { kind: 'danger', icon: 'signout', onclick: signOut })));
  sheet.body.addEventListener('click', (e) => { if (e.target.closest('a[href]')) sheet.close(); });
  const sync = () => {
    const pr = currentPrefs() || {};
    theme.set(pr.theme || 'device');
    const s = sizeState();
    sizeValue.textContent = `${s.size}%`;
    smaller.disabled = s.size <= SIZE_MIN;
    bigger.disabled = s.size >= SIZE_MAX;
    follow.set(s.follow);
    sizeLine.textContent = sizeNote();
  };
  onPrefs(sync);
  display.onChange(sync);
  sync();
}

async function signOut() {
  try { await api.del('session'); } catch { /* signed out either way */ }
  await clearServiceWorker();
  toast('Signed out.', { kind: 'info', next: true });
  location.assign('/sign-in');
}

/** Who is signed in (from /api/v1/me), for the sheet. */
export function setAccount(m) { me = m; }
/** Open the account sheet. */
export function openAccount(opener) {
  if (!sheet) build();
  sheet.open(opener);
}
