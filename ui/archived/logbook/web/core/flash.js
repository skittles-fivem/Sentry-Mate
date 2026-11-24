// Sentry Mate, "Logbook" UI pack: flash notes, the short success, info or error lines after an action.
//
// flash('Place saved.') shows a note at the top of the page (in #flash, a polite live region) for a few
// seconds; errors stay until closed. flash(text, { next: true }) keeps it for the next page instead (for
// an action that ends in a navigation, such as signing in): it is held in this tab's sessionStorage and
// shown once by showPending(), which the router calls after each page mounts.

import { h } from '../shared/dom.js';
import { icon } from './ui.js';

const KEY = 'logbook:flash';
const zone = () => document.getElementById('flash');
const TIMES = { success: 5000, info: 7000, error: 0 };

/**
 * Show a note: flash('Saved.'), flash('Could not save: …', { kind: 'error' }).
 * kind: 'success' (default) | 'info' | 'error'. next: show it on the next page. ms: how long (0 = until closed).
 */
export function flash(text, { kind = 'success', next = false, ms = null } = {}) {
  if (next) {
    try {
      const list = JSON.parse(sessionStorage.getItem(KEY) || '[]');
      list.push({ text: String(text), kind });
      sessionStorage.setItem(KEY, JSON.stringify(list.slice(-3)));
    } catch { /* storage blocked: show it now instead */ show(text, kind, ms); }
    return;
  }
  show(text, kind, ms);
}

function show(text, kind, ms) {
  const z = zone();
  if (!z) return;
  const close = () => { el.classList.add('is-leaving'); setTimeout(() => el.remove(), 180); };
  const el = h('div', { class: ['flash', `flash-${kind}`], role: kind === 'error' ? 'alert' : null },
    icon(kind === 'error' ? 'warning' : kind === 'info' ? 'info' : 'check', 'icon flash-icon'),
    h('p', { class: 'flash-text' }, text),
    h('button', { type: 'button', class: 'flash-close', onclick: close, 'aria-label': 'Close this note' }, icon('close')));
  z.append(el);
  while (z.children.length > 3) z.firstElementChild.remove();
  const t = ms ?? TIMES[kind] ?? 5000;
  if (t > 0) setTimeout(close, t);
}

/** Show the notes kept for this page (called by the router after a page mounts). */
export function showPending() {
  let list = [];
  try { list = JSON.parse(sessionStorage.getItem(KEY) || '[]'); sessionStorage.removeItem(KEY); } catch { /* none */ }
  for (const f of Array.isArray(list) ? list : []) if (f && f.text) show(f.text, f.kind || 'success', null);
}

/** Remove the notes on screen (a new page starts clean, except errors the person has not closed). */
export function clearFlash({ keepErrors = true } = {}) {
  const z = zone();
  if (!z) return;
  for (const el of [...z.children]) if (!(keepErrors && el.classList.contains('flash-error'))) el.remove();
}
