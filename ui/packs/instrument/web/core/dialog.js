// Dialogs on the native <dialog> element: focus moves in and back, Esc closes, the page behind is inert.

import { h } from '../shared/dom.js';
import { icon } from './icons.js';

/**
 * Open a dialog and wait for it to close. Resolves with the value of the action pressed, or null (Esc,
 * the close button). body: elements or text. actions: [{ label, value, kind: 'primary' | 'danger' |
 * undefined, onClick? }]; an onClick that returns false keeps the dialog open (a form with errors).
 *   const ok = await openDialog({ title: 'Delete place?', body: 'Drives keep their addresses.',
 *     actions: [{ label: 'Cancel', value: false }, { label: 'Delete', value: true, kind: 'danger' }] });
 */
export function openDialog({ title, body = null, actions = [{ label: 'Close', value: null }], wide = false } = {}) {
  return new Promise((resolve) => {
    const back = document.activeElement;
    let result = null;
    const titleId = `dlg-${Math.random().toString(36).slice(2, 8)}`;
    const dlg = h('dialog', { class: ['dialog', wide && 'dialog--wide'], 'aria-labelledby': titleId },
      h('div', { class: 'dialog-head' }, h('h2', { id: titleId }, title),
        h('button', { class: 'btn btn--ghost btn--icon btn--sm', type: 'button', 'aria-label': 'Close', onclick: () => dlg.close() }, icon('x'))),
      h('div', { class: 'dialog-body' }, body),
      actions.length ? h('div', { class: 'dialog-actions' }, actions.map((a) => h('button', {
        class: ['btn', a.kind === 'primary' && 'btn--primary', a.kind === 'danger' && 'btn--danger'], type: 'button',
        onclick: async () => {
          if (a.onClick && (await a.onClick()) === false) return;
          result = a.value ?? null;
          dlg.close();
        },
      }, a.label))) : null);
    dlg.addEventListener('close', () => {
      dlg.remove();
      if (back && typeof back.focus === 'function') back.focus();
      resolve(result);
    });
    document.body.append(dlg);
    dlg.showModal();
  });
}

/** Ask before something that cannot be undone. Resolves true or false. */
export async function confirmDialog(text, { title = 'Are you sure?', ok = 'Continue', danger = false } = {}) {
  const r = await openDialog({ title, body: h('p', null, text), actions: [{ label: 'Cancel', value: false }, { label: ok, value: true, kind: danger ? 'danger' : 'primary' }] });
  return r === true;
}
