// Things that sit above the page: dialogs (native <dialog>: focus moves in and back, Esc closes),
// popovers (the popover attribute: a click outside or Esc closes) and toasts (short notes after an action).

import { h, replace } from '../shared/dom.js';
import { icon, btn, place } from './ui.js';

/* ------------------------------------------------------------------ dialogs */

/**
 * Open a dialog: openDialog({ title, body, actions, cls, label, onClose }). body and actions are elements
 * (or lists). Returns the <dialog>; it is removed from the page when it closes.
 */
export function openDialog({ title, body, actions, cls, label, onClose } = {}) {
  const back = document.activeElement;
  const d = h('dialog', { class: ['dlg', cls], 'aria-label': label || title || null });
  const head = title ? h('div', { class: 'dlg-head' }, h('h2', { class: 'dlg-title' }, title), btn(null, () => d.close(), { kind: 'ghost', icon: 'x', label: 'Close', sm: true })) : null;
  replace(d, head, h('div', { class: 'dlg-body' }, body), actions ? h('div', { class: 'dlg-acts' }, actions) : null);
  d.addEventListener('click', (e) => { if (e.target === d) d.close(); });
  d.addEventListener('close', () => { d.remove(); onClose?.(d.returnValue); if (back && document.contains(back)) back.focus?.({ preventScroll: true }); });
  document.body.append(d);
  d.showModal();
  return d;
}

/** Ask yes or no: resolves true when the person agrees. confirmDialog(text, { title, ok, danger }). */
export function confirmDialog(text, { title = 'Are you sure?', ok = 'Go on', cancel = 'Cancel', danger = false } = {}) {
  return new Promise((resolve) => {
    let yes = false;
    const d = openDialog({
      title, body: h('p', null, text),
      actions: [btn(cancel, () => d.close(), { kind: 'ghost' }), btn(ok, () => { yes = true; d.close(); }, { kind: danger ? 'danger' : 'go' })],
      onClose: () => resolve(yes),
    });
  });
}

/* ------------------------------------------------------------------ popovers */

/**
 * A popover tied to an anchor button: const p = popover({ cls, label, fill(el) }); p.toggle(button).
 * fill(el) builds its content each time it opens. aria-expanded on the anchor follows it.
 */
export function popover({ cls, label, fill, side = 'below', align = 'start' }) {
  const el = h('div', { class: ['pop', cls], popover: 'auto', role: 'dialog', 'aria-label': label });
  let anchor = null;
  let opening = false;
  el.addEventListener('toggle', (e) => {
    anchor?.setAttribute('aria-expanded', String(e.newState === 'open'));
    if (e.newState === 'closed') el.dispatchEvent(new Event('hz-closed'));
  });
  document.body.append(el);
  const api = {
    el,
    get open() { return el.matches(':popover-open'); },
    show(a, o = {}) {
      anchor = a || anchor;
      if (opening) return;
      opening = true;
      try {
        fill?.(el, api);
        if (!api.open) el.showPopover();
        if (anchor) place(el, anchor, { side: o.side || side, align: o.align || align });
      } finally { opening = false; }
    },
    hide() { if (api.open) el.hidePopover(); },
    toggle(a, o) { if (api.open && (!a || a === anchor)) api.hide(); else api.show(a, o); },
    reposition() { if (api.open && anchor) place(el, anchor, { side, align }); },
  };
  window.addEventListener('resize', () => api.reposition());
  return api;
}

/* ------------------------------------------------------------------ toasts */

const KEY = 'sentrymate.horizon.toast';
let box = null;

/** The frame calls this once with the #toasts element. */
export function initToasts(el) { box = el; }

/**
 * A short note after an action: toast('Saved.', { kind: 'ok' | 'info' | 'error', next }). With next: true it is
 * shown on the page the person goes to next (kept for this tab only).
 */
export function toast(text, { kind = 'ok', next = false, ms } = {}) {
  if (next) {
    try { sessionStorage.setItem(KEY, JSON.stringify({ text, kind })); return; } catch { /* show it now instead */ }
  }
  if (!box) return;
  const t = h('div', { class: 'toast', dataset: { kind }, role: kind === 'error' ? 'alert' : null },
    icon(kind === 'error' ? 'warn' : kind === 'info' ? 'info' : 'check'),
    h('span', { class: 'toast-text' }, text),
    btn(null, () => t.remove(), { kind: 'ghost', icon: 'x', label: 'Dismiss', sm: true }));
  box.append(t);
  while (box.children.length > 3) box.firstElementChild.remove();
  setTimeout(() => t.remove(), ms ?? (kind === 'error' ? 9000 : 5000));
}

/** Show a toast kept with next: true (the router calls this after each page mounts). */
export function showWaitingToast() {
  let v = null;
  try { v = JSON.parse(sessionStorage.getItem(KEY) || 'null'); sessionStorage.removeItem(KEY); } catch { /* none */ }
  if (v?.text) toast(v.text, { kind: v.kind || 'ok' });
}
