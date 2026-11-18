// Sentry Mate, "Logbook" UI pack: forms with field errors.
//
// field() builds a labelled control with room for a hint and an error; form() wires a <form> to an API
// call: it disables the submit button while saving, puts each field error from the server (ApiError
// .fields, keyed by path such as 'units.distance') under its field, and shows other errors at the top.
// choice() is a segmented radio group (the pack's toggle), switchField() a labelled on/off switch.

import { h } from '../shared/dom.js';
import { errorWords } from './ui.js';

let seq = 0;
const nextId = (p) => `${p}-${++seq}`;

/**
 * A labelled field: field({ label: 'Name', name: 'name', hint, type: 'text' | 'password' | 'number' | 'date'
 * | 'select' | 'textarea', value, options: [[value, text]], attrs: { required, min, max, autocomplete } }).
 * The error line is filled by form() (or by setFieldError). Returns the .field element; its control is
 * el.control.
 */
export function field({ label, name, hint = null, type = 'text', value = null, options = null, attrs = {}, cover = null, cls = null } = {}) {
  const id = nextId('f');
  const hintId = hint ? `${id}-hint` : null;
  const errId = `${id}-err`;
  const described = [hintId, errId].filter(Boolean).join(' ');
  let control;
  if (type === 'select') {
    control = h('select', { id, name, class: 'field-select', 'aria-describedby': described, ...attrs },
      (options || []).map(([v, t]) => h('option', { value: v, selected: String(v) === String(value) }, t)));
  } else if (type === 'textarea') {
    control = h('textarea', { id, name, class: 'field-input field-textarea', 'aria-describedby': described, ...attrs });
    if (value != null) control.value = String(value);
  } else {
    control = h('input', { id, name, type, class: 'field-input', 'aria-describedby': described, ...attrs });
    if (value != null) control.value = String(value);
  }
  const el = h('div', { class: ['field', cls], cover, dataset: { field: name } },
    h('label', { class: 'field-label', for: id }, label),
    h('div', { class: 'field-control' }, control),
    hint ? h('p', { class: 'field-hint', id: hintId }, hint) : null,
    h('p', { class: 'field-error', id: errId, hidden: true }));
  el.control = control;
  return el;
}

/** Show (or clear, with null) the error under a field element. */
export function setFieldError(fieldEl, text) {
  const err = fieldEl.querySelector('.field-error');
  if (!err) return;
  err.textContent = text || '';
  err.hidden = !text;
  fieldEl.classList.toggle('has-error', !!text);
  fieldEl.control?.setAttribute('aria-invalid', text ? 'true' : 'false');
}

/**
 * Wire a form: form({ fields: [field elements], submitLabel, onsubmit(values, formEl) -> Promise, cover }).
 * values are the controls' values by name (numbers for type number, booleans for checkboxes). The promise
 * may reject with an ApiError: its fields go under their fields (a path that names no field goes to the
 * top line), other errors to the top line. Returns the <form>; el.reset() clears the errors.
 */
export function form({ fields = [], submitLabel = 'Save', onsubmit, extra = null, cover = null, cls = null, busyLabel = 'Saving…' } = {}) {
  const top = h('p', { class: 'form-error', role: 'alert', hidden: true });
  const submit = h('button', { type: 'submit', class: 'button' }, submitLabel);
  const el = h('form', { class: ['form', cls], cover, novalidate: true }, top, fields, extra, h('div', { class: 'form-actions' }, submit));
  const byName = () => new Map(fields.filter((f) => f.dataset && f.dataset.field).map((f) => [f.dataset.field, f]));
  const clearErrors = () => { top.hidden = true; top.textContent = ''; for (const f of fields) if (f.dataset?.field) setFieldError(f, null); };
  el.addEventListener('submit', async (e) => {
    e.preventDefault();
    clearErrors();
    const values = {};
    for (const c of el.elements) {
      if (!c.name) continue;
      if (c.type === 'checkbox') values[c.name] = c.checked;
      else if (c.type === 'radio') { if (c.checked) values[c.name] = c.value; }
      else if (c.type === 'number') values[c.name] = c.value === '' ? null : Number(c.value);
      else values[c.name] = c.value;
    }
    submit.disabled = true;
    const label = submit.textContent;
    submit.textContent = busyLabel;
    try {
      await onsubmit(values, el);
    } catch (err) {
      const map = byName();
      const unplaced = [];
      if (err && err.fields && typeof err.fields === 'object') {
        for (const [path, msg] of Object.entries(err.fields)) {
          const f = map.get(path) || map.get(path.split('.')[0]);
          if (f) setFieldError(f, String(msg)); else unplaced.push(`${path}: ${msg}`);
        }
      }
      if (!err || !err.fields || unplaced.length) {
        top.textContent = unplaced.length ? unplaced.join(' ') : (err && err.message) || errorWords(err).title;
        top.hidden = false;
      }
      const firstBad = el.querySelector('[aria-invalid="true"]');
      if (firstBad) firstBad.focus();
    } finally {
      submit.disabled = false;
      submit.textContent = label;
    }
  });
  el.clearErrors = clearErrors;
  return el;
}

/**
 * A segmented choice (radio buttons that look like the pack's toggles): choice({ label, name, value,
 * options: [[value, text]], onchange(value) }). Arrow keys move and pick, as radios do.
 */
export function choice({ label = null, name, value, options, onchange, cover = null, cls = null, labelHidden = false } = {}) {
  const id = nextId('c');
  const group = h('div', { class: ['segmented', cls], role: 'radiogroup', 'aria-labelledby': label ? `${id}-label` : null, 'aria-label': label ? null : name, cover });
  const buttons = options.map(([v, t]) => h('button', {
    type: 'button', role: 'radio', class: 'segmented-option', dataset: { value: String(v) },
    'aria-checked': String(String(v) === String(value)), tabindex: String(v) === String(value) ? '0' : '-1',
    onclick: () => pick(v, true),
  }, t));
  group.append(...buttons);
  function pick(v, fire) {
    buttons.forEach((b) => { const on = b.dataset.value === String(v); b.setAttribute('aria-checked', String(on)); b.tabIndex = on ? 0 : -1; });
    if (fire) onchange?.(v);
  }
  group.addEventListener('keydown', (e) => {
    const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
    if (!step) return;
    e.preventDefault();
    const i = buttons.indexOf(document.activeElement);
    const j = (Math.max(0, i) + step + buttons.length) % buttons.length;
    buttons[j].focus();
    buttons[j].click();
  });
  if (!buttons.some((b) => b.tabIndex === 0) && buttons[0]) buttons[0].tabIndex = 0;
  const wrap = label
    ? h('div', { class: 'choice' }, h('span', { class: ['field-label', { 'sr-only': labelHidden }], id: `${id}-label` }, label), group)
    : group;
  wrap.set = (v) => pick(v, false);
  return wrap;
}

/** A labelled on / off switch: switchField({ label, checked, onchange(checked), hint }). */
export function switchField({ label, checked = false, onchange, hint = null, cover = null } = {}) {
  const id = nextId('s');
  const input = h('input', { type: 'checkbox', role: 'switch', id, class: 'switch-input', checked, onchange: () => onchange?.(input.checked) });
  const el = h('div', { class: 'switch-field', cover },
    h('label', { class: 'switch', for: id }, input, h('span', { class: 'switch-track', 'aria-hidden': 'true' }), h('span', { class: 'switch-label' }, label)),
    hint ? h('p', { class: 'field-hint' }, hint) : null);
  el.set = (v) => { input.checked = !!v; };
  el.input = input;
  return el;
}
