// Keys: the console is keyboard first, and every key has a mouse or tap way too.
//
//   Ctrl/Cmd+K or /   the command list           j / k   next / previous row      Enter   open the row
//   g f  fleet   g d  drives   g c  charges   g b  battery   g s  settings        Esc     close, else back
//   ?                 this list
// Keys are off while an input, a select or a text area has focus (Ctrl/Cmd+K still works), and on the
// sign-in and set-up screens. Rows are elements with [data-row] in the page; the selected one has
// aria-selected="true" and the class is-sel. A row opens its first link (or its data-href).

import * as state from '../shared/state.js';
import { h } from '../shared/dom.js';
import { pageForKey } from './pages.js';
import { openPalette } from './palette.js';
import { openDialog } from './dialog.js';
import { navCar } from './frame.js';

export const KEYS = Object.freeze([
  ['ctrl k  or  /', 'open the command list: pages, cars, recent drives and charges, settings keys'],
  ['j  /  k', 'next / previous row of the list on the page'],
  ['enter', 'open the selected row'],
  ['esc', 'close the list or dialog; else go back'],
  ['g f', 'fleet'], ['g d', 'drives'], ['g c', 'charges'], ['g b', 'battery'], ['g s', 'settings'],
  ['?', 'this list of keys'],
]);

let sel = -1;
let chord = 0;   // the time 'g' was pressed, for 'g <key>'

const typing = (t) => !!t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName));
const bare = () => document.documentElement.dataset.screen === 'bare';
const rows = () => [...document.querySelectorAll('#view [data-row]')].filter((r) => r.offsetParent !== null);

/** Select row i of the page (the router calls resetRows() on every page). */
export function selectRow(i) {
  const r = rows();
  if (!r.length) return;
  sel = Math.max(0, Math.min(r.length - 1, i));
  r.forEach((x, k) => { const on = k === sel; x.classList.toggle('is-sel', on); if (x.getAttribute('role') === 'row' || x.hasAttribute('aria-selected')) x.setAttribute('aria-selected', String(on)); });
  r[sel].scrollIntoView({ block: 'nearest' });
}
export function resetRows() { sel = -1; }

function openRow() {
  const r = rows()[sel];
  if (!r) return false;
  const href = r.dataset.href || r.querySelector('a[href]')?.getAttribute('href');
  if (href) { state.navigate(href); return true; }
  const edit = r.querySelector('[data-focus]');   // a settings line: Enter goes into its value
  if (edit) { (edit.matches('input') ? edit : edit.querySelector('[aria-pressed="true"]') || edit.querySelector('button'))?.focus(); return true; }
  r.querySelector('button')?.click();
  return true;
}

export function openKeyList() {
  if (document.querySelector('dialog.dlg--keys')) return null;
  return openDialog({ title: 'keys', cls: 'dlg--keys', body: h('dl', { class: 'keylist' },
    KEYS.map(([k, what]) => h('div', { class: 'keylist-row' }, h('dt', { class: 'kbd-line' }, k), h('dd', null, what)))) });
}

function onKey(e) {
  if (e.defaultPrevented || e.isComposing) return;
  const mod = e.metaKey || e.ctrlKey;
  if (mod && !e.altKey && (e.key === 'k' || e.key === 'K')) {
    if (bare()) return;
    e.preventDefault();
    openPalette();
    return;
  }
  if (mod || e.altKey || bare() || typing(e.target)) return;
  if (document.querySelector('dialog[open]')) return;   // a dialog has its own keys (Esc closes it)
  const k = e.key;
  if (chord && Date.now() - chord < 1200) {
    chord = 0;
    const p = pageForKey(k);
    if (p) { e.preventDefault(); state.navigate(state.linkTo(p.id, p.car ? { car: navCar() } : {})); }
    return;
  }
  chord = 0;
  if (k === '/') { e.preventDefault(); openPalette(); }
  else if (k === '?') { e.preventDefault(); openKeyList(); }
  else if (k === 'g') { chord = Date.now(); }
  else if (k === 'j') { e.preventDefault(); selectRow(sel + 1); }
  else if (k === 'k') { e.preventDefault(); selectRow(sel < 0 ? 0 : sel - 1); }
  else if (k === 'Enter' && sel >= 0 && !(e.target instanceof HTMLAnchorElement) && !(e.target instanceof HTMLButtonElement)) {
    if (openRow()) e.preventDefault();
  } else if (k === 'Escape') {
    if (sel >= 0) { rows().forEach((x) => x.classList.remove('is-sel')); sel = -1; return; }
    if (state.current().page !== 'fleet' && history.length > 1) history.back();
  }
}

export function initKeys() { document.addEventListener('keydown', onKey); }
