// Sentry Mate, "Pebble" UI pack: bottom sheets and dialogs.
//
// A sheet is a native modal <dialog>: while it is open the page behind is inert, so the focus stays inside;
// Esc, the close button and a tap on the dim backdrop close it, and the focus goes back to what opened it.
// It slides up from the bottom in 220 ms (no motion with prefers-reduced-motion). On a phone it fills the
// width and up to 90 % of the height; on a bigger screen it floats above the dock, centred.
//
//   const s = createSheet({ title: 'More', cls: 'more-sheet' }); s.body.append(...); s.open(button);
//   const v = await openSheet({ title, body, actions: [{ label, value, kind }] });   // a one-off sheet
//   const ok = await confirmSheet({ title, text, yes: 'Give up', danger: true });

import { h } from '../shared/dom.js';
import { icon, btn } from './ui.js';

let seq = 0;
const reduced = () => matchMedia('(prefers-reduced-motion: reduce)').matches;

/**
 * A sheet kept for reuse: createSheet({ title, cls, cover, onOpen, onClose, center }) ->
 * { el, body, head, titleEl, open(opener), close(), isOpen }. center: a dialog in the middle of the screen
 * instead of a sheet from the bottom (confirmations on a desktop).
 */
export function createSheet({ title = '', cls = null, cover = null, onOpen = null, onClose = null, label = null, center = false } = {}) {
  const id = `sheet-${++seq}`;
  const titleEl = h('h2', { class: 'sheet-title', id: `${id}-t` }, title);
  const closeBtn = btn('Close', { kind: 'ghost', icon: 'close', iconOnly: true, cls: 'sheet-close', onclick: () => api.close() });
  const head = h('div', { class: 'sheet-head' }, titleEl, closeBtn);
  const body = h('div', { class: 'sheet-body' });
  const el = h('dialog', { class: ['sheet', { 'is-center': center }, cls], id, 'aria-labelledby': label ? null : `${id}-t`, 'aria-label': label, cover },
    h('div', { class: 'sheet-panel' }, h('span', { class: 'sheet-grip', 'aria-hidden': 'true' }), head, body));
  document.body.append(el);
  let opener = null;
  let closing = 0;

  function finish() {
    closing = 0;
    el.classList.remove('is-closing');
    if (el.open) el.close();
  }
  const api = {
    el, body, head, titleEl,
    get isOpen() { return el.open && !closing; },
    open(from = document.activeElement) {
      if (el.open && !closing) return;
      if (closing) { clearTimeout(closing); finish(); }
      opener = from;
      el.showModal();
      el.scrollTop = 0;
      body.scrollTop = 0;
      onOpen?.(api);
      const first = el.querySelector('[data-autofocus]') || body.querySelector('[aria-current="page"], [aria-checked="true"], input, button, a[href]');
      if (first) first.focus({ preventScroll: true });
    },
    close() {
      if (!el.open || closing) return;
      onClose?.(api);
      const back = opener;
      opener = null;
      if (back && back.isConnected) setTimeout(() => back.focus({ preventScroll: true }), 0);
      if (reduced()) { finish(); return; }
      el.classList.add('is-closing');
      closing = setTimeout(finish, 200);
    },
  };
  el.addEventListener('cancel', (e) => { e.preventDefault(); api.close(); });
  el.addEventListener('click', (e) => { if (e.target === el) api.close(); });
  return api;
}

/**
 * A one-off sheet: openSheet({ title, body, actions: [{ label, value, kind, onclick }] }) resolves with the
 * value of the action that closed it, or null. body may be (close) => nodes. onclick(close) may return false
 * to keep the sheet open.
 */
export function openSheet({ title, body = null, actions = [], cls = null, center = false } = {}) {
  return new Promise((resolve) => {
    let value = null;
    const s = createSheet({ title, cls, center, onClose: () => setTimeout(() => { s.el.remove(); resolve(value); }, 220) });
    const close = (v = null) => { value = v; s.close(); };
    s.body.append(...[].concat(typeof body === 'function' ? body(close) : body).filter(Boolean));
    if (actions.length) {
      s.body.append(h('div', { class: 'sheet-actions' }, actions.map((a) => btn(a.label, {
        kind: a.kind || 'soft',
        onclick: async () => { if (a.onclick) { const keep = await a.onclick(close); if (keep === false) return; } close(a.value ?? null); },
      }))));
    }
    s.open();
  });
}

/** Ask yes or no: resolves true for yes. */
export function confirmSheet({ title, text = null, yes = 'Yes', no = 'Cancel', danger = false } = {}) {
  return openSheet({
    title, center: true, cls: 'confirm-sheet',
    body: text ? h('p', { class: 'sheet-text' }, text) : null,
    actions: [{ label: no, value: false, kind: 'soft' }, { label: yes, value: true, kind: danger ? 'danger' : 'primary' }],
  }).then((v) => v === true);
}

/** A row of a sheet list: sheetRow({ href | onclick, icon, name, sub, current, cover, tone }). */
export function sheetRow({ href = null, onclick = null, icon: ic = null, name, sub = null, current = false, cover = null, tone = null, side = null, role = null, checked = null } = {}) {
  const tag = href ? 'a' : 'button';
  return h(tag, {
    class: ['row', { 'is-current': current }], href, type: href ? null : 'button', onclick, cover, role,
    'aria-current': href && current ? 'page' : null, 'aria-checked': checked == null ? null : String(checked),
  },
  ic ? h('span', { class: ['row-ic', tone && `tone-${tone}`], 'aria-hidden': 'true' }, icon(ic)) : null,
  h('span', { class: 'row-text' }, h('span', { class: 'row-name' }, name), sub ? h('span', { class: 'row-sub' }, sub) : null),
  side ? h('span', { class: 'row-side' }, side) : null,
  checked != null ? h('span', { class: 'row-check', 'aria-hidden': 'true' }, icon('check')) : href ? icon('chevronRight', 'row-go') : null);
}

/** A group of rows with a small heading. */
export const sheetGroup = (name, rows) => h('section', { class: 'rows' }, name ? h('h3', { class: 'rows-name' }, name) : null, h('div', { class: 'rows-list' }, rows));
