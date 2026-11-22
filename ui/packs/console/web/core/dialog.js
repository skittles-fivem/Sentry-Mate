// Dialogs on the native <dialog>: focus moves in and back, Esc closes. Placed on whole pixels by app.css.

import { h } from '../shared/dom.js';
import { passwordAdvice } from '../shared/password.js';

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

/**
 * Before a new password is sent: when passwordAdvice has reasons, say why it is easy to guess and let the person
 * choose. Resolves true to send it (no advice, or "use it anyway"); false for "change it" or Esc, with the focus
 * back in the password field. The server takes any password that is not empty.
 */
export function passwordOk(name, password, input = null) {
  const why = passwordAdvice(name, password);
  if (!why.length) return Promise.resolve(true);
  return new Promise((resolve) => {
    let answer = false;
    const yes = h('button', { class: 'btn', type: 'button', onclick: () => { answer = true; d.close(); } }, 'use it anyway');
    const no = h('button', { class: ['btn', 'btn--primary'], type: 'button', onclick: () => d.close() }, 'change it');
    const d = openDialog({
      title: 'This password is easy to guess', cls: 'dlg--advice',
      body: [h('ul', { class: 'dlg-list' }, why.map((t) => h('li', null, t))), h('p', { class: 'dlg-text' }, 'You can still use it.')],
      actions: [no, yes],
      onClose: () => {
        resolve(answer);
        if (!answer && input && typeof input.focus === 'function') setTimeout(() => { input.focus(); input.select?.(); }, 0);
      },
    });
    no.focus();
  });
}
