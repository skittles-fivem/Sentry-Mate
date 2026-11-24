// Dialogs on the native <dialog> element: focus moves in and back, Esc closes, the page behind is inert.

import { h } from '../shared/dom.js';
import { icon } from './icons.js';
import { passwordAdvice } from '../shared/password.js';

// A button that disables itself while its request runs loses the focus before its dialog opens (the browser
// takes the focus off a disabled control, and document.activeElement is then <body>). It is remembered here, so
// the dialog still gives the focus back to it. Any focus or press after that forgets it.
let dropped = null;
document.addEventListener('focusout', (e) => { dropped = !e.relatedTarget && e.target instanceof HTMLElement && e.target.matches(':disabled') ? e.target : null; }, true);
document.addEventListener('focusin', () => { dropped = null; }, true);
document.addEventListener('pointerdown', () => { dropped = null; }, true);

/** The control a dialog opening now gives the focus back to when it closes. */
function opener() {
  const a = document.activeElement;
  if (a && a !== document.body) return a;
  return dropped && dropped.isConnected ? dropped : null;
}

/** Focus `el`; one that is still disabled (its request has not ended) gets the focus as soon as it is not. */
function giveBack(el) {
  if (!el || !el.isConnected || typeof el.focus !== 'function') return;
  el.focus();
  if (document.activeElement === el) return;
  setTimeout(() => { if (el.isConnected && (!document.activeElement || document.activeElement === document.body)) el.focus(); }, 0);
}

/**
 * A dialog whose request is running cannot be closed: Esc, the close button and any Cancel (a button in the
 * dialog's head or marked [data-cancel], or the secondary button of a form) wait until it ends, or its answer
 * (a key shown once, an account added) would be lost with it. Pass any element inside the dialog.
 */
export function dialogBusy(inside, on) {
  const dlg = inside?.closest?.('dialog');
  if (!dlg) return;
  dlg.toggleAttribute('data-busy', !!on);
  for (const b of dlg.querySelectorAll('.dialog-head button, .form-actions button:not([type="submit"]), [data-cancel]')) {
    if (on) { if (!b.disabled) { b.disabled = true; b.dataset.busyOff = ''; } } else if ('busyOff' in b.dataset) { b.disabled = false; delete b.dataset.busyOff; }
  }
}

/**
 * Open a dialog and wait for it to close. Resolves with the value of the action pressed, or null (Esc,
 * the close button). body: elements or text. actions: [{ label, value, kind: 'primary' | 'danger' |
 * undefined, onClick?, focus? }]; an onClick that returns false keeps the dialog open (a form with errors).
 * The action with focus: true has the focus when the dialog opens (else the browser's first focusable).
 * When it closes the focus goes back to the control that had it (back: another element to give it to).
 *   const ok = await openDialog({ title: 'Delete place?', body: 'Drives keep their addresses.',
 *     actions: [{ label: 'Cancel', value: false }, { label: 'Delete', value: true, kind: 'danger' }] });
 */
export function openDialog({ title, body = null, actions = [{ label: 'Close', value: null }], wide = false, cls = null, back: backTo = null } = {}) {
  return new Promise((resolve) => {
    const back = backTo || opener();
    let result = null;
    const titleId = `dlg-${Math.random().toString(36).slice(2, 8)}`;
    let first = null;
    const dlg = h('dialog', { class: ['dialog', wide && 'dialog--wide', cls], 'aria-labelledby': titleId },
      h('div', { class: 'dialog-head' }, h('h2', { id: titleId }, title),
        h('button', { class: 'btn btn--ghost btn--icon btn--sm', type: 'button', 'aria-label': 'Close', onclick: () => { if (!dlg.hasAttribute('data-busy')) dlg.close(); } }, icon('x'))),
      h('div', { class: 'dialog-body' }, body),
      actions.length ? h('div', { class: 'dialog-actions' }, actions.map((a) => {
        const b = h('button', {
          class: ['btn', a.kind === 'primary' && 'btn--primary', a.kind === 'danger' && 'btn--danger'], type: 'button',
          onclick: async () => {
            if (dlg.hasAttribute('data-busy')) return;
            if (a.onClick) {
              // the dialog stays and takes no second press while the action runs
              dialogBusy(dlg.firstChild, true);
              for (const x of dlg.querySelectorAll('.dialog-actions button')) x.setAttribute('aria-disabled', 'true');
              let keep;
              try { keep = await a.onClick(); } finally {
                dialogBusy(dlg.firstChild, false);
                for (const x of dlg.querySelectorAll('.dialog-actions button')) x.removeAttribute('aria-disabled');
              }
              if (keep === false) return;
            }
            result = a.value ?? null;
            dlg.close();
          },
        }, a.label);
        if (a.focus) first = b;
        return b;
      })) : null);
    dlg.addEventListener('cancel', (e) => { if (dlg.hasAttribute('data-busy')) e.preventDefault(); });
    // a browser lets a cancel be held back only once between two gestures: Escape itself is held back while busy,
    // and a dialog closed while its request runs opens again, so its answer (a key shown once) is not lost
    dlg.addEventListener('keydown', (e) => { if (e.key === 'Escape' && dlg.hasAttribute('data-busy')) e.preventDefault(); });
    dlg.addEventListener('close', () => {
      if (dlg.hasAttribute('data-busy') && dlg.isConnected) { dlg.showModal(); return; }
      dlg.remove();
      resolve(result);
      giveBack(back);
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
