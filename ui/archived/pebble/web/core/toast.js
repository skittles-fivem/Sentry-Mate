// Sentry Mate, "Pebble" UI pack: toasts, the short notes after an action.
//
// toast('Place saved.') shows a rounded note just above the dock (in #toasts, a polite live region) for a few
// seconds; errors stay until closed. toast(text, { next: true }) keeps it for the next page instead (an action
// that ends in a navigation, such as signing in): it waits in this tab's sessionStorage and showPending() shows
// it once the next page has mounted.

import { h } from '../shared/dom.js';
import { icon } from './ui.js';

const KEY = 'pebble:toast';
const zone = () => document.getElementById('toasts');
const TIMES = { ok: 5000, info: 7000, error: 0 };

/** toast('Saved.'), toast('Not saved: …', { kind: 'error' }). kind: 'ok' | 'info' | 'error'. */
export function toast(text, { kind = 'ok', next = false, ms = null } = {}) {
  if (next) {
    try {
      const list = JSON.parse(sessionStorage.getItem(KEY) || '[]');
      list.push({ text: String(text), kind });
      sessionStorage.setItem(KEY, JSON.stringify(list.slice(-3)));
      return;
    } catch { /* storage blocked: show it now */ }
  }
  show(String(text), kind, ms);
}

function show(text, kind, ms) {
  const z = zone();
  if (!z) return;
  const el = h('div', { class: ['toast', `toast-${kind}`], role: kind === 'error' ? 'alert' : null },
    h('span', { class: 'toast-ic', 'aria-hidden': 'true' }, icon(kind === 'error' ? 'warning' : kind === 'info' ? 'info' : 'check')),
    h('p', { class: 'toast-text' }, text),
    h('button', { type: 'button', class: 'toast-close', 'aria-label': 'Close this note', onclick: () => close() }, icon('close')));
  const close = () => { el.classList.add('is-leaving'); setTimeout(() => el.remove(), 180); };
  z.append(el);
  while (z.children.length > 3) z.firstElementChild.remove();
  const t = ms ?? TIMES[kind] ?? 5000;
  if (t > 0) setTimeout(close, t);
}

/** Show the notes kept for this page (the router calls it after each page mounts). */
export function showPending() {
  let list = [];
  try { list = JSON.parse(sessionStorage.getItem(KEY) || '[]'); sessionStorage.removeItem(KEY); } catch { /* none */ }
  for (const f of Array.isArray(list) ? list : []) if (f && f.text) show(String(f.text), f.kind || 'ok', null);
}

/** A new page starts clean, except errors the person has not closed. */
export function clearToasts() {
  const z = zone();
  if (z) for (const el of [...z.children]) if (!el.classList.contains('toast-error')) el.remove();
}
