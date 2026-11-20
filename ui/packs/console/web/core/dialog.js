// Dialogs on the native <dialog>: focus moves in and back, Esc closes. Placed on whole pixels by app.css.

import { h } from '../shared/dom.js';

/**
 * openDialog({ title, body, actions, cls, onClose }) -> { el, close }. actions: elements for the bottom row.
 * The title line has a close button ('esc').
 */
export function openDialog({ title, body, actions = null, cls = null, onClose = null, label = null }) {
  const back = document.activeElement;
  const el = h('dialog', { class: ['dlg', cls], 'aria-label': label || title },
    h('div', { class: 'dlg-head' }, h('h2', { class: 'dlg-title' }, title),
      h('button', { class: 'dlg-x', type: 'button', 'aria-label': 'Close', onclick: () => el.close() }, 'esc')),
    h('div', { class: 'dlg-body' }, body),
    actions ? h('div', { class: 'acts dlg-acts' }, actions) : null);
  el.addEventListener('close', () => {
    el.remove();
    try { onClose?.(el.returnValue); } finally { if (back && back.isConnected) back.focus?.({ preventScroll: true }); }
  });
  el.addEventListener('click', (e) => { if (e.target === el) el.close(); });   // a click on the backdrop
  document.body.append(el);
  el.showModal();
  return { el, close: (v) => el.close(v) };
}

/** Ask yes or no. Resolves true for ok. */
export function confirmDialog(text, { title = 'Are you sure?', ok = 'ok', danger = false } = {}) {
  return new Promise((resolve) => {
    let answer = false;
    const yes = h('button', { class: ['btn', danger ? 'btn--danger' : 'btn--primary'], type: 'button', onclick: () => { answer = true; d.close(); } }, ok);
    const no = h('button', { class: 'btn', type: 'button', onclick: () => d.close() }, 'cancel');
    const d = openDialog({ title, body: h('p', { class: 'dlg-text' }, text), actions: [yes, no], onClose: () => resolve(answer) });
    no.focus();
  });
}
