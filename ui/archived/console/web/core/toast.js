// Toasts: short notes after an action, bottom right ('[ok] Saved.'). No motion: they appear and go.
// toast(text, { kind: 'ok' | 'info' | 'error', next: true }) - next: show it on the page you go to.

import { h } from '../shared/dom.js';

let box = null;
let pending = [];
const TAGS = { ok: 'ok', info: 'info', error: 'bad' };

export function initToasts(el) { box = el; }

export function toast(text, { kind = 'ok', next = false, ms = null } = {}) {
  if (next) { pending.push([text, kind]); return; }
  if (!box) return;
  const t = h('div', { class: ['toast', `toast--${kind}`], role: kind === 'error' ? 'alert' : 'status' },
    h('span', { class: `tag tag--${TAGS[kind] || 'ok'}` }, `[${TAGS[kind] || kind}]`), h('span', { class: 'toast-text' }, text),
    h('button', { class: 'toast-x', type: 'button', 'aria-label': 'Close', onclick: () => t.remove() }, 'x'));
  box.append(t);
  while (box.children.length > 3) box.firstElementChild.remove();
  setTimeout(() => t.remove(), ms ?? (kind === 'error' ? 9000 : 4500));
}

/** The router calls this after a page mounts. */
export function showPending() {
  const list = pending;
  pending = [];
  for (const [text, kind] of list) toast(text, { kind });
}
