// Forms with field errors.
//
//   const name = field({ label: 'Name', name: 'name', required: true, hint: 'Shown in every list.', path: 'name' });
//   const f = form({ fields: [name], submit: 'Save', async onSubmit(values) { await ctx.api.patch(url, values); } });
//
// field() types: text, password, number, email, date, time, select (options: [[value, label]]), checkbox,
// textarea. The browser's own checks run first. An ApiError thrown from onSubmit puts each
// error.fields[path] under its field and the rest in the form's message line; anything else shows its
// message there too. The submit button is busy while onSubmit runs.

import { h, replace } from '../shared/dom.js';
import { icon, btn } from './ui.js';

let seq = 0;

export function field({ label, name, type = 'text', required = false, hint, value, options, attrs = {}, path } = {}) {
  const id = `f-${name}-${++seq}`;
  const hintId = hint ? `${id}-hint` : null;
  const errId = `${id}-err`;
  const described = [hintId, errId].filter(Boolean).join(' ');
  let input;
  if (type === 'select') {
    input = h('select', { class: 'input', id, name, required, 'aria-describedby': described, ...attrs },
      (options || []).map(([v, l]) => h('option', { value: String(v), selected: String(v) === String(value ?? '') }, l)));
  } else if (type === 'textarea') {
    input = h('textarea', { class: 'input', id, name, required, 'aria-describedby': described, ...attrs }, value ?? '');
  } else if (type === 'checkbox') {
    input = h('input', { type: 'checkbox', id, name, required, checked: !!value, 'aria-describedby': described, ...attrs });
  } else {
    input = h('input', { class: 'input', type, id, name, required, value: value ?? '', 'aria-describedby': described, ...attrs });
  }
  const err = h('p', { class: 'field-error', id: errId, hidden: true });
  const el = type === 'checkbox'
    ? h('div', { class: 'field' }, h('label', { class: 'check', for: id }, input, h('span', null, label)), hint ? h('p', { class: 'field-hint', id: hintId }, hint) : null, err)
    : h('div', { class: 'field' }, h('label', { class: 'field-label', for: id }, label), input, hint ? h('p', { class: 'field-hint', id: hintId }, hint) : null, err);
  const setError = (text) => {
    err.hidden = !text;
    if (text) { replace(err, icon('warn', 'i--sm'), h('span', null, text)); input.setAttribute('aria-invalid', 'true'); }
    else input.removeAttribute('aria-invalid');
  };
  input.addEventListener('input', () => setError(null));
  const read = () => (type === 'checkbox' ? input.checked : input.value);
  return { el, input, name, path: path || name, setError, read };
}

export function form({ fields = [], submit = 'Save', secondary = null, onSubmit, danger = false, extra = null } = {}) {
  const msg = h('p', { class: 'form-msg', role: 'alert', hidden: true });
  const go = btn(submit, null, { kind: danger ? 'danger' : 'go', type: 'submit' });
  const el = h('form', { class: 'form', novalidate: false }, fields.map((f) => f.el), extra, msg, h('div', { class: 'form-acts' }, go, secondary));
  const setMessage = (text) => { msg.hidden = !text; if (text) replace(msg, icon('warn', 'i--sm'), h('span', null, text)); };
  const clear = () => { setMessage(null); for (const f of fields) f.setError(null); };
  el.addEventListener('submit', async (e) => {
    e.preventDefault();
    clear();
    const values = Object.fromEntries(fields.map((f) => [f.name, f.read()]));
    go.disabled = true;
    el.setAttribute('aria-busy', 'true');
    try { await onSubmit?.(values); } catch (err) { showApiError({ fields, setMessage }, err); } finally {
      go.disabled = false;
      el.removeAttribute('aria-busy');
    }
  });
  return { el, setMessage, clear, submitButton: go, fields };
}

/** Put an ApiError's field messages under their fields and the rest in the message line. */
export function showApiError(f, err) {
  if (err?.name === 'AbortError') return;
  const left = [];
  for (const [path, text] of Object.entries(err?.fields || {})) {
    const fld = f.fields.find((x) => x.path === path || x.name === path);
    if (fld) fld.setError(String(text)); else left.push(String(text));
  }
  if (!err?.fields || left.length) f.setMessage(left.join(' ') || err?.message || 'That did not work. Try again.');
  if (!err?.status) console.warn(err);
}
