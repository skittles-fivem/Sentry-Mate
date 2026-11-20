// Sentry Mate, "Pebble" UI pack: forms.
//
// field() is a labelled control (the label always shows, above a large rounded input) with room for a hint and
// an error. form() wires a <form> to an API call: the button is busy while it saves, each field error from the
// server (ApiError.fields, keyed by path such as 'units.distance') goes under its field and the rest on top.
// toggle() is an on / off switch in a list row; segmented() a row of choices in one rounded track.

import { h } from '../shared/dom.js';
import { errorWords } from './states.js';
import { icon } from './ui.js';

let seq = 0;
const nextId = (p) => `${p}-${++seq}`;

/**
 * field({ label, name, hint, type: 'text' | 'password' | 'number' | 'email' | 'date' | 'file' | 'select' |
 * 'textarea', value, options: [[value, text]], attrs, cover, cls }) -> the .field element; its control is
 * el.control.
 */
export function field({ label, name, hint = null, type = 'text', value = null, options = null, attrs = {}, cover = null, cls = null } = {}) {
  const id = nextId('f');
  const hintId = hint ? `${id}-hint` : null;
  const errId = `${id}-err`;
  const described = [hintId, errId].filter(Boolean).join(' ');
  const extra = attrs.class ? ` ${attrs.class}` : '';
  const a = { ...attrs };
  delete a.class;
  let control;
  if (type === 'select') {
    control = h('select', { id, name, class: `input select${extra}`, 'aria-describedby': described, ...a },
      (options || []).map(([v, t]) => h('option', { value: v, selected: String(v) === String(value) }, t)));
  } else if (type === 'textarea') {
    control = h('textarea', { id, name, class: `input textarea${extra}`, 'aria-describedby': described, ...a });
    if (value != null) control.value = String(value);
  } else {
    control = h('input', { id, name, type, class: `input${type === 'file' ? ' input-file' : ''}${extra}`, 'aria-describedby': described, ...a });
    if (value != null && type !== 'file') control.value = String(value);
  }
  const el = h('div', { class: ['field', cls], cover, dataset: { field: name } },
    h('label', { class: 'label', for: id }, label),
    control,
    hint ? h('p', { class: 'hint', id: hintId }, hint) : null,
    h('p', { class: 'field-error', id: errId, hidden: true }));
  el.control = control;
  return el;
}

/** Show (or clear, with null) the error under a field. */
export function setFieldError(fieldEl, text) {
  const err = fieldEl.querySelector('.field-error');
  if (!err) return;
  err.textContent = text || '';
  err.hidden = !text;
  fieldEl.classList.toggle('has-error', !!text);
  fieldEl.control?.setAttribute('aria-invalid', text ? 'true' : 'false');
}

/**
 * form({ fields, submitLabel, busyLabel, onsubmit(values, formEl), extra, cover, cls, submitIcon }) -> <form>.
 * values by control name (numbers for type number, booleans for checkboxes). onsubmit may throw an ApiError or
 * an Error with .fields: those go under their fields, the rest to the line on top.
 */
export function form({ fields = [], submitLabel = 'Save', busyLabel = 'Saving…', onsubmit, extra = null, cover = null, cls = null, submitIcon = null, secondary = null } = {}) {
  const top = h('p', { class: 'form-error', role: 'alert', hidden: true });
  const label = h('span', { class: 'btn-text' }, submitLabel);
  const submit = h('button', { type: 'submit', class: 'btn btn-primary btn-wide' }, submitIcon ? icon(submitIcon) : null, label);
  const el = h('form', { class: ['form', cls], cover, novalidate: true }, top, fields, extra, h('div', { class: 'form-actions' }, submit, secondary));
  const byName = () => new Map(fields.filter((f) => f && f.dataset && f.dataset.field).map((f) => [f.dataset.field, f]));
  const clearErrors = () => { top.hidden = true; top.textContent = ''; for (const f of fields) if (f && f.dataset?.field) setFieldError(f, null); };
  el.addEventListener('submit', async (e) => {
    e.preventDefault();
    if (submit.disabled) return;
    clearErrors();
    const values = {};
    for (const c of el.elements) {
      if (!c.name) continue;
      if (c.type === 'checkbox') values[c.name] = c.checked;
      else if (c.type === 'radio') { if (c.checked) values[c.name] = c.value; }
      else if (c.type === 'number') values[c.name] = c.value === '' ? null : Number(c.value);
      else if (c.type !== 'file') values[c.name] = c.value;
    }
    submit.disabled = true;
    el.setAttribute('aria-busy', 'true');
    label.textContent = busyLabel;
    try {
      await onsubmit(values, el);
    } catch (err) {
      const map = byName();
      const unplaced = [];
      if (err && err.fields && typeof err.fields === 'object') {
        for (const [path, msg] of Object.entries(err.fields)) {
          const f = map.get(path) || map.get(path.split('.')[0]);
          if (f) setFieldError(f, String(msg)); else unplaced.push(String(msg));
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
      el.removeAttribute('aria-busy');
      label.textContent = submitLabel;
    }
  });
  el.clearErrors = clearErrors;
  el.submitButton = submit;
  return el;
}

/** An on / off switch row: toggle({ label, sub, checked, onchange(on) }). el.set(on), el.input. */
export function toggle({ label, sub = null, checked = false, onchange = null, cover = null, disabled = false } = {}) {
  const id = nextId('t');
  const input = h('input', { type: 'checkbox', role: 'switch', id, class: 'switch-input', checked, disabled, onchange: () => onchange?.(input.checked) });
  const el = h('label', { class: 'row row-toggle', for: id, cover },
    h('span', { class: 'row-text' }, h('span', { class: 'row-name' }, label), sub ? h('span', { class: 'row-sub' }, sub) : null),
    h('span', { class: 'switch' }, input, h('span', { class: 'switch-track', 'aria-hidden': 'true' }, h('span', { class: 'switch-knob' }))));
  el.set = (v) => { input.checked = !!v; };
  el.input = input;
  return el;
}

/**
 * Choices in one rounded track (radios): segmented({ label, name, value, options: [[value, text]], onchange }).
 * Arrow keys move and pick. el.set(value).
 */
export function segmented({ label = null, name, value, options, onchange = null, cover = null, cls = null } = {}) {
  const id = nextId('g');
  const group = h('div', { class: ['seg', cls], role: 'radiogroup', 'aria-labelledby': label ? `${id}-l` : null, 'aria-label': label ? null : name, cover });
  const buttons = options.map(([v, t]) => h('button', {
    type: 'button', role: 'radio', class: 'seg-btn', dataset: { value: String(v) },
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
  const wrap = label ? h('div', { class: 'seg-field' }, h('span', { class: 'label', id: `${id}-l` }, label), group) : group;
  wrap.set = (v) => pick(v, false);
  return wrap;
}
