// Forms with field errors. A field has a label, a hint, the control and an error line tied to it
// (aria-describedby, aria-invalid). The API names wrong fields in error.fields, keyed by path
// ('units.distance', 'free_supercharging.0.from_date'); showApiError() puts each message under its
// field and the rest in the form's message line.

import { h, replace } from '../shared/dom.js';
import { icon } from './icons.js';
import { dialogBusy } from './dialog.js';

// field ids: numbered from 0 again for each page build (the router calls fieldIdsFromStart), so a page built
// again on a live refresh gives its fields the same ids, and the page swapped in looks the same as the one it
// takes the place of (a form whose ids changed would come in as new controls: a flicker, a lost screen reader place)
let uid = 0;
export function fieldIdsFromStart() { uid = 0; }

/**
 * field({ label, name, type, value, hint, required, options: [[value, label]], attrs, path })
 *   -> { el, input, name, path, setError(text | null), error(), reset(), value() }
 * type: text | password | number | email | date | time | select | checkbox | textarea. path: the API
 * field path for error messages (defaults to name).
 */
export function field({ label, name, type = 'text', value = null, hint = null, required = false, options = null, attrs = {}, path = null }) {
  const id = `f-${name}-${++uid}`;
  const hintId = hint ? `${id}-hint` : null;
  const errId = `${id}-err`;
  let input;
  if (type === 'select') {
    input = h('select', { class: 'select', id, name, required }, (options || []).map(([v, l]) => h('option', { value: v, selected: String(v) === String(value) }, l)));
  } else if (type === 'textarea') {
    input = h('textarea', { class: 'input input--area', id, name, required, ...attrs }, value ?? '');
  } else if (type === 'checkbox') {
    input = h('input', { type: 'checkbox', id, name, checked: !!value, ...attrs });
  } else {
    input = h('input', { class: 'input', type, id, name, required, value: value ?? '', ...attrs });
  }
  input.setAttribute('aria-describedby', [hintId, errId].filter(Boolean).join(' '));
  const err = h('p', { class: 'form-error', id: errId, hidden: true });
  const el = type === 'checkbox'
    ? h('div', { class: 'form-row form-row--check' }, h('label', { class: 'check' }, input, h('span', null, label)), hint ? h('p', { class: 'form-hint', id: hintId }, hint) : null, err)
    : h('div', { class: 'form-row' }, h('label', { class: 'form-label', for: id }, label), input, hint ? h('p', { class: 'form-hint', id: hintId }, hint) : null, err);
  const setError = (text) => {
    err.hidden = !text;
    if (text) { replace(err, icon('warn'), text); input.setAttribute('aria-invalid', 'true'); } else { err.replaceChildren(); input.removeAttribute('aria-invalid'); }
  };
  input.addEventListener('input', () => setError(null));
  const read = () => (type === 'checkbox' ? input.checked : type === 'number' ? (input.value === '' ? null : Number(input.value)) : input.value);
  // the value the field was built with (the saved one): a save that failed puts it back
  const first = type === 'checkbox' ? !!value : String(value ?? '');
  const reset = () => { if (type === 'checkbox') input.checked = first; else input.value = first; };
  return { el, input, name, path: path || name, setError, error: () => (err.hidden ? null : err.textContent.trim() || null), reset, value: read };
}

/**
 * form({ fields: [field(...)], submit: 'Save', onSubmit: async (values, f) => {}, extra, cls })
 *   -> { el, values(), setErrors({ path: text }), setMessage(text | null), busy(on) }
 * onSubmit gets { name: value }. Throwing an ApiError from it shows the error (showApiError).
 */
export function form({ fields, submit = 'Save', onSubmit, extra = null, cls = null, secondary = null }) {
  const message = h('p', { class: 'form-message', role: 'alert', hidden: true });
  const btn = h('button', { class: 'btn btn--primary', type: 'submit' }, submit);
  const values = () => Object.fromEntries(fields.map((f) => [f.name, f.value()]));
  let held = false;   // the submit button had the focus when it was disabled
  const f = {
    fields, values,
    setErrors(map) { for (const x of fields) x.setError(map?.[x.path] || map?.[x.name] || null); },
    setMessage(text) { message.hidden = !text; message.textContent = text || ''; },
    // A button that is disabled while it has the focus loses it to <body>. When the request is over and nothing
    // else took the focus (a wrong field, a next step), it goes to the first field marked wrong, else back to
    // the button.
    busy(on) {
      if (on) held = document.activeElement === btn;
      btn.disabled = !!on; btn.setAttribute('aria-busy', on ? 'true' : 'false');
      dialogBusy(btn, on);   // in a dialog: it cannot be closed while the request runs
      if (on || !held) return;
      held = false;
      const a = document.activeElement;
      if (!btn.isConnected || (a && a !== document.body && a !== btn)) return;
      const wrong = fields.find((x) => x.input.getAttribute('aria-invalid') === 'true' && !x.input.disabled);
      (wrong ? wrong.input : btn).focus();
    },
  };
  const el = h('form', {
    class: ['form', cls], novalidate: true,
    onsubmit: async (e) => {
      e.preventDefault();
      f.setErrors(null); f.setMessage(null);
      // the browser's own checks (required, min, max) first, worded by the browser
      const bad = fields.find((x) => x.input.checkValidity && !x.input.checkValidity());
      if (bad) { bad.setError(bad.input.validationMessage); bad.input.focus(); return; }
      f.busy(true);
      try { await onSubmit(values(), f); } catch (err) { showApiError(f, err); } finally { f.busy(false); }
    },
  }, fields.map((x) => x.el), extra, message, h('div', { class: 'form-actions' }, btn, secondary));
  f.el = el;
  return f;
}

/** Put an ApiError's field messages under their fields; anything else goes to the message line. */
export function showApiError(f, err) {
  if (err?.name === 'AbortError') return;
  const fields = err?.fields || null;
  let placed = false;
  if (fields) {
    for (const x of f.fields) {
      const msg = fields[x.path] ?? fields[x.name];
      if (msg) { x.setError(String(msg)); placed = true; }
    }
  }
  const rest = fields ? Object.entries(fields).filter(([k]) => !f.fields.some((x) => x.path === k || x.name === k)) : [];
  if (!placed || rest.length) f.setMessage(rest.length ? rest.map(([k, v]) => `${k}: ${v}`).join(' ') : (err?.message || 'That did not work. Try again.'));
  const first = f.fields.find((x) => x.input.getAttribute('aria-invalid') === 'true');
  first?.input.focus();
}

/**
 * An onclick for a button that starts one thing at a time: a press while the last one is still running does
 * nothing (a double click makes one token, sends one save). The button shows it with aria-disabled (it keeps the
 * focus; a disabled button would lose it).
 *   h('button', { onclick: once(async () => { await api.post(...); }) }, 'Make')
 */
export function once(fn) {
  let running = false;
  return async (e) => {
    if (running) return undefined;
    running = true;
    const b = e?.currentTarget instanceof HTMLElement ? e.currentTarget : null;
    b?.setAttribute('aria-disabled', 'true');
    try { return await fn(e); } finally { running = false; b?.removeAttribute('aria-disabled'); }
  };
}
