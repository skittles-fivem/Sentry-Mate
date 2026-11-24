// Forms as console lines: 'label: [input]' rows, a message line and buttons. An ApiError thrown from onSubmit
// puts each fields[path] message under its field and the rest in the message line.
//
//   const name = field({ label: 'user', name: 'name', required: true });
//   const f = form({ fields: [name], submit: 'sign in', async onSubmit(values) { ... } });
//   view.append(f.el);

import { h, replace } from '../shared/dom.js';
import { errorWords } from './states.js';

let uid = 0;

/**
 * field({ label, name, type, required, hint, path, options, attrs, value }) -> { el, input, setError, name, path }
 * type: text | password | number | email | date | select | checkbox | textarea | file
 */
export function field({ label, name, type = 'text', required = false, hint = null, path = null, options = null, attrs = {}, value = null }) {
  const id = `f-${name}-${++uid}`;
  const err = h('p', { class: 'fld-err', id: `${id}-err`, hidden: true, role: 'alert' });
  const hintEl = hint ? h('p', { class: 'fld-hint', id: `${id}-hint` }, hint) : null;
  const describedby = [hintEl && `${id}-hint`, `${id}-err`].filter(Boolean).join(' ');
  let input;
  if (type === 'select') {
    input = h('select', { class: 'in', id, name, required, 'aria-describedby': describedby, ...attrs },
      (options || []).map(([v, l]) => h('option', { value: v, selected: String(v) === String(value) }, l)));
  } else if (type === 'textarea') {
    input = h('textarea', { class: 'in in--area', id, name, required, 'aria-describedby': describedby, ...attrs }, value ?? '');
  } else if (type === 'checkbox') {
    input = h('input', { class: 'chk', id, name, type, required, checked: !!value, 'aria-describedby': describedby, ...attrs });
  } else {
    input = h('input', { class: 'in', id, name, type, required, value: value ?? undefined, 'aria-describedby': describedby, ...attrs });
  }
  const el = type === 'checkbox'
    ? h('div', { class: 'fld fld--check' }, h('label', { class: 'fld-checkline', for: id }, input, h('span', null, label)), hintEl, err)
    : h('div', { class: 'fld' }, h('label', { class: 'fld-label', for: id }, `${label}:`), h('div', { class: 'fld-box' }, input, hintEl, err));
  const setError = (text) => {
    err.hidden = !text;
    replace(err, text || '');
    input.setAttribute('aria-invalid', text ? 'true' : 'false');
  };
  input.addEventListener('input', () => setError(null));
  return { el, input, setError, name, path: path || name, type };
}

function values(fields) {
  const v = {};
  for (const f of fields) v[f.name] = f.type === 'checkbox' ? f.input.checked : f.type === 'file' ? f.input.files?.[0] || null : f.input.value;
  return v;
}

/**
 * form({ fields, submit, secondary, onSubmit, cls }) -> { el, setMessage, busy(on), fields }
 * secondary: more buttons next to submit. onSubmit(values) may throw an ApiError.
 */
export function form({ fields, submit = 'save', secondary = null, onSubmit, cls = null, danger = false }) {
  const msg = h('p', { class: 'frm-msg', hidden: true, role: 'alert' });
  const go = h('button', { class: ['btn', danger ? 'btn--danger' : 'btn--primary'], type: 'submit' }, submit);
  const el = h('form', { class: ['frm', cls], novalidate: false }, fields.map((f) => f.el), msg, h('div', { class: 'acts' }, go, secondary));
  const setMessage = (text) => { msg.hidden = !text; replace(msg, text || ''); };
  let running = false;
  const busy = (on) => { running = on; go.disabled = on; go.setAttribute('aria-busy', on ? 'true' : 'false'); };
  el.addEventListener('submit', async (e) => {
    e.preventDefault();
    if (running) return;
    setMessage(null);
    for (const f of fields) f.setError(null);
    busy(true);
    try {
      await onSubmit(values(fields));
    } catch (err) {
      showApiError({ fields, setMessage }, err);
    } finally {
      busy(false);
    }
  });
  return { el, setMessage, busy, fields, submitButton: go };
}

/** Put an ApiError's field messages under their fields, the rest in the message line. */
export function showApiError(f, err) {
  const left = [];
  for (const [path, text] of Object.entries(err?.fields || {})) {
    const fl = f.fields.find((x) => x.path === path || x.name === path);
    if (fl) fl.setError(text); else left.push(text);
  }
  if (left.length || !err?.fields) f.setMessage(left.length ? left.join(' ') : errorWords(err).text);
}
