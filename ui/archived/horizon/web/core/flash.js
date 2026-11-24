// Flash notes: a short success, information or error note after an action ("Place saved"). Shown under
// the top bar for a few seconds; errors stay until closed. flash(text, { next: true }) keeps the note for
// the next page instead (after an action that navigates), shown once there.

import { h } from '../shared/dom.js';
import { icon } from './icons.js';

const KEY = 'sentrymate.flash';
const ICONS = { ok: 'check', info: 'info', error: 'warn' };
let region = null;

/** The frame calls this once with the #flash element. */
export function initFlash(el) {
  region = el;
  showPending();
}

/**
 * Show a note: flash('Place saved'), flash('Could not save: ...', { kind: 'error' }).
 * opts: kind 'ok' (default) | 'info' | 'error'; ms (how long; errors stay); next (show on the next page).
 */
export function flash(text, { kind = 'ok', ms = null, next = false } = {}) {
  if (next) {
    try {
      const list = JSON.parse(sessionStorage.getItem(KEY) || '[]');
      list.push({ text: String(text), kind });
      sessionStorage.setItem(KEY, JSON.stringify(list.slice(-3)));
      return;
    } catch { /* storage blocked: show it now */ }
  }
  if (!region) return;
  const close = () => note.remove();
  const note = h('div', { class: 'flash-note', dataset: { kind }, role: kind === 'error' ? 'alert' : null },
    icon(ICONS[kind] || 'info'), h('span', null, String(text)),
    h('button', { class: 'btn btn--ghost btn--icon btn--sm', type: 'button', 'aria-label': 'Close', onclick: close }, icon('x', 'ico--sm')));
  region.append(note);
  while (region.children.length > 3) region.firstElementChild.remove();
  const life = ms ?? (kind === 'error' ? 0 : 6000);
  if (life) setTimeout(close, life);
}

/** Show the notes left for this page (the router calls this after each navigation). */
export function showPending() {
  let list = [];
  try { list = JSON.parse(sessionStorage.getItem(KEY) || '[]'); sessionStorage.removeItem(KEY); } catch { list = []; }
  for (const n of list) flash(n.text, { kind: n.kind });
}
