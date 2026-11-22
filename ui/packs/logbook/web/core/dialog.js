// Sentry Mate, "Logbook" UI pack: dialogs.
//
// openDialog({ title, body, actions }) shows a native modal <dialog> (focus is kept inside, Esc closes,
// the page behind is inert) and resolves with the value of the action that closed it. confirmDialog()
// is the yes / no case. popover(trigger, panel) wires a small menu that opens under its button and
// closes on Esc, on a click outside and when the focus leaves it.

import { h } from '../shared/dom.js';
import { icon } from './ui.js';
import { passwordAdvice } from '../shared/password.js';

/**
 * A modal dialog. body: nodes (or a function (close) -> nodes). actions: [{ label, value, kind, onclick }]
 * where onclick(close) may return false to keep it open (a form that failed). Resolves with the chosen
 * value, or null when closed with Esc, the close button or a click on the backdrop.
 * opts: cls, wide (a wider dialog), cover (item ids for the dialog element), signal (closes it).
 */
export function openDialog({ title, body = null, actions = [{ label: 'Close', value: null }], cls = null, wide = false, cover = null, signal = null, initialFocus = null } = {}) {
  return new Promise((resolve) => {
    let value = null;
    // a unique title id: a dialog may open over another (the password advice over the account form)
    const titleId = `dialog-title-${Math.random().toString(36).slice(2, 8)}`;
    const dlg = h('dialog', { class: ['dialog', cls, { 'dialog-wide': wide }], 'aria-labelledby': titleId, cover });
    const close = (v = null) => { value = v; if (dlg.open) dlg.close(); };
    const content = typeof body === 'function' ? body(close) : body;
    dlg.append(
      h('div', { class: 'dialog-head' },
        h('h2', { class: 'dialog-title', id: titleId }, title),
        h('button', { type: 'button', class: 'dialog-close', 'aria-label': 'Close', onclick: () => close(null) }, icon('close'))),
      h('div', { class: 'dialog-body' }, content),
      ...(actions && actions.length ? [h('div', { class: 'dialog-actions' }, actions.map((a) => h('button', {
        type: 'button', class: ['button', a.kind && a.kind !== 'primary' ? `button-${a.kind}` : a.kind === 'primary' ? null : 'button-quiet'],
        onclick: async () => {
          if (a.onclick) { const keep = await a.onclick(close); if (keep === false) return; }
          close(a.value);
        },
      }, a.label)))] : []));
    dlg.addEventListener('click', (e) => { if (e.target === dlg) close(null); });
    dlg.addEventListener('close', () => { dlg.remove(); resolve(value); });
    if (signal) signal.addEventListener('abort', () => close(null), { once: true });
    document.body.append(dlg);
    dlg.showModal();
    const first = initialFocus ? dlg.querySelector(initialFocus) : null;
    if (first) first.focus();
  });
}

/** Ask yes or no: resolves true for yes. confirmDialog({ title, text, yes: 'Delete', danger: true }). */
export function confirmDialog({ title, text = null, yes = 'Yes', no = 'Cancel', danger = false } = {}) {
  return openDialog({
    title,
    body: text ? h('p', { class: 'dialog-text' }, text) : null,
    actions: [{ label: no, value: false, kind: 'quiet' }, { label: yes, value: true, kind: danger ? 'danger' : 'primary' }],
  }).then((v) => v === true);
}

/**
 * Before a new password is sent: when passwordAdvice has reasons, say why it is easy to guess and let the person
 * choose. Resolves true to send it (no advice, or "Use it anyway"); false for "Change it" or Esc, with the focus
 * back in the password field. The server takes any password that is not empty.
 */
export async function passwordOk(name, password, input = null) {
  const why = passwordAdvice(name, password);
  if (!why.length) return true;
  const v = await openDialog({
    title: 'This password is easy to guess', cls: 'dialog-advice',
    body: [h('ul', { class: 'dialog-list' }, why.map((t) => h('li', null, t))), h('p', { class: 'dialog-text' }, 'You can still use it.')],
    actions: [{ label: 'Use it anyway', value: true, kind: 'quiet' }, { label: 'Change it', value: false, kind: 'primary' }],
    initialFocus: '.dialog-actions button:last-child',
  });
  if (v === true) return true;
  if (input && typeof input.focus === 'function') { input.focus(); input.select?.(); }
  return false;
}

/**
 * A small menu under a button: popover(trigger, panel, { onOpen }). The panel must be in the DOM right
 * after the trigger (or anywhere; it is positioned by CSS). Returns { open(), close(), toggle(), isOpen }.
 */
export function popover(trigger, panel, { onOpen = null, onClose = null, focus = null } = {}) {
  const wrapper = trigger.parentElement;
  panel.hidden = true;
  trigger.setAttribute('aria-expanded', 'false');
  if (panel.id) trigger.setAttribute('aria-controls', panel.id);
  const api = {
    get isOpen() { return !panel.hidden; },
    open() {
      if (!panel.hidden) return;
      panel.hidden = false;
      trigger.setAttribute('aria-expanded', 'true');
      onOpen?.();
      const f = focus ? panel.querySelector(focus) : null;
      if (f) f.focus({ preventScroll: true });
    },
    close({ refocus = false } = {}) {
      if (panel.hidden) return;
      panel.hidden = true;
      trigger.setAttribute('aria-expanded', 'false');
      onClose?.();
      if (refocus) trigger.focus({ preventScroll: true });
    },
    toggle() { if (panel.hidden) api.open(); else api.close(); },
  };
  trigger.addEventListener('click', () => api.toggle());
  document.addEventListener('pointerdown', (e) => { if (!panel.hidden && !wrapper.contains(e.target) && !panel.contains(e.target)) api.close(); });
  document.addEventListener('keydown', (e) => { if (e.key === 'Escape' && !panel.hidden) { api.close({ refocus: true }); } });
  wrapper.addEventListener('focusout', (e) => { if (!panel.hidden && e.relatedTarget && !wrapper.contains(e.relatedTarget) && !panel.contains(e.relatedTarget)) api.close(); });
  return api;
}
