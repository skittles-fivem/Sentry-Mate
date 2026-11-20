// Dialogs on the native <dialog> element: focus moves in and back, Esc closes, the page behind is inert.

import { h } from '../shared/dom.js';
import { icon } from './icons.js';
import { passwordAdvice } from '../shared/password.js';

/**
 * Open a dialog and wait for it to close. Resolves with the value of the action pressed, or null (Esc,
 * the close button). body: elements or text. actions: [{ label, value, kind: 'primary' | 'danger' |
 * undefined, onClick?, focus? }]; an onClick that returns false keeps the dialog open (a form with errors).
 * The action with focus: true has the focus when the dialog opens (else the browser's first focusable).
 *   const ok = await openDialog({ title: 'Delete place?', body: 'Drives keep their addresses.',
 *     actions: [{ label: 'Cancel', value: false }, { label: 'Delete', value: true, kind: 'danger' }] });
 */
export function openDialog({ title, body = null, actions = [{ label: 'Close', value: null }], wide = false, cls = null } = {}) {
  return new Promise((resolve) => {
    const back = document.activeElement;
    let result = null;
    const titleId = `dlg-${Math.random().toString(36).slice(2, 8)}`;
    let first = null;
    const dlg = h('dialog', { class: ['dialog', wide && 'dialog--wide', cls], 'aria-labelledby': titleId },
      h('div', { class: 'dialog-head' }, h('h2', { id: titleId }, title),
        h('button', { class: 'btn btn--ghost btn--icon btn--sm', type: 'button', 'aria-label': 'Close', onclick: () => dlg.close() }, icon('x'))),
      h('div', { class: 'dialog-body' }, body),
      actions.length ? h('div', { class: 'dialog-actions' }, actions.map((a) => {
        const b = h('button', {
          class: ['btn', a.kind === 'primary' && 'btn--primary', a.kind === 'danger' && 'btn--danger'], type: 'button',
          onclick: async () => {
            if (a.onClick && (await a.onClick()) === false) return;
            result = a.value ?? null;
            dlg.close();
          },
        }, a.label);
        if (a.focus) first = b;
        return b;
      })) : null);
    dlg.addEventListener('close', () => {
      dlg.remove();
      if (back && typeof back.focus === 'function') back.focus();
      resolve(result);
    });
    document.body.append(dlg);
    dlg.showModal();
    if (first) first.focus();
  });
}

/** Ask before something that cannot be undone. Resolves true or false. */
export async function confirmDialog(text, { title = 'Are you sure?', ok = 'Continue', danger = false } = {}) {
  const r = await openDialog({ title, body: h('p', null, text), actions: [{ label: 'Cancel', value: false }, { label: ok, value: true, kind: danger ? 'danger' : 'primary' }] });
  return r === true;
}

/**
 * Before a new password is sent: when passwordAdvice has reasons, say why it is easy to guess and let the person
 * choose. Resolves true to send it (no advice, or "Use it anyway"); false for "Change it" or Esc, with the focus
 * back in the password field. The server takes any password that is not empty.
 */
export async function passwordOk(name, password, input = null) {
  const why = passwordAdvice(name, password);
  if (!why.length) return true;
  const r = await openDialog({
    title: 'This password is easy to guess', cls: 'dialog--advice',
    body: [h('ul', { class: 'dialog-list' }, why.map((t) => h('li', null, t))), h('p', null, 'You can still use it.')],
    actions: [{ label: 'Use it anyway', value: true }, { label: 'Change it', value: false, kind: 'primary', focus: true }],
  });
  if (r === true) return true;
  if (input && typeof input.focus === 'function') { input.focus(); input.select?.(); }
  return false;
}
