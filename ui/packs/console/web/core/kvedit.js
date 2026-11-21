// Settings as 'key = value' lines. A value looks like plain text until it has focus, then it is an input:
// Enter saves, Esc puts the saved value back. A choice is a row of keys ([km] [mi]): pressing one saves it.
// While a value is changed but not saved the line says so, with a save button for mouse and touch.
//
//   const lines = editLines([
//     { key: 'units.distance', id: 'units', type: 'choice', value: 'km', options: [['km', 'km'], ['mi', 'mi']],
//       help: 'everywhere: drives, range, speed', cover: 'web.settings.general.unit-length', save: (v) => savePrefs(...) },
//     { key: 'name', type: 'text', value: car.name, parse: (t) => (t.trim() ? { value: t.trim() } : { error: 'a car needs a name' }), save },
//   ]);
//   view.append(lines.el); lines.line('name').set('Model Y');
//
// type: text | number | password | date | choice | bool | show (read-only, value is an element or text)
// group: lines that are saved together (a broker's host, port and TLS): editGroup({ save(values, lines) }).
// Enter on any of them saves every changed one in one call; Esc puts all of them back.

import { h, replace } from '../shared/dom.js';
import { errorWords } from './states.js';
import { tag } from './fmt.js';

let uid = 0;
const BOOL = [['true', 'on'], ['false', 'off']];

/** The words of an error thrown by a save: the field message when there is one. */
export function saveError(err, path = null) {
  if (err?.fields) {
    if (path && err.fields[path]) return err.fields[path];
    const all = Object.values(err.fields);
    if (all.length) return all.join(' ');
  }
  return errorWords(err).text;
}

/** Lines saved together. save(values, lines): values is { path: value } of the changed lines. */
export function editGroup({ save, label = 'save' }) {
  const g = { lines: [], save, label, busy: false };
  const go = h('button', { class: 'btn btn--primary', type: 'button', onclick: () => commit() }, label);
  const undoBtn = h('button', { class: 'btn', type: 'button', onclick: () => undo() }, 'undo');
  const msg = h('p', { class: 'ke-msg', role: 'alert', hidden: true });
  g.bar = h('div', { class: 'ke-bar', hidden: true }, h('div', { class: 'acts' }, go, undoBtn), msg);
  g.changed = () => {
    const dirty = g.lines.some((l) => l.dirty());
    g.bar.hidden = !dirty && msg.hidden;
    go.disabled = !dirty || g.busy;
    undoBtn.disabled = !dirty || g.busy;
  };
  g.message = (text) => { msg.hidden = !text; replace(msg, text || ''); g.changed(); };
  async function commit() {
    if (g.busy) return;
    const dirty = g.lines.filter((l) => l.dirty());
    if (!dirty.length) return;
    const values = {};
    for (const l of dirty) {
      const p = l.parsed();
      if (p.error) { l.error(p.error); l.focus(); return; }
      values[l.path] = p.value;
    }
    g.busy = true; g.message(null);
    for (const l of dirty) l.state('saving');
    try {
      await g.save(values, dirty);
      for (const l of dirty) l.done();
    } catch (err) {
      let placed = false;
      for (const l of g.lines) {
        const t = err?.fields?.[l.path];
        if (t) { l.error(t); placed = true; } else if (l.dirty()) l.state('dirty');
      }
      if (!placed) g.message(saveError(err));
    } finally { g.busy = false; g.changed(); }
  }
  function undo() { for (const l of g.lines) l.undo(); g.message(null); }
  g.commit = commit;
  g.undo = undo;
  return g;
}

function stateEl() { return h('span', { class: 'ke-st', 'aria-live': 'polite' }); }

/**
 * One line. spec: { key, id, path, type, value, options, help, cover, readOnly, attrs, unit, toText(v), parse(text),
 * save(value), group, label }. Returns { el, set(value), error(text), focus(), dirty(), ... }.
 */
export function editLine(spec) {
  const id = `ke-${++uid}`;
  const type = spec.type || 'text';
  const ro = !!spec.readOnly || type === 'show' || (!spec.save && !spec.group);
  const toText = spec.toText || ((v) => (v == null ? '' : String(v)));
  let saved = spec.value;
  const st = stateEl();
  const err = h('p', { class: 'ke-err', id: `${id}-err`, role: 'alert', hidden: true });
  const help = spec.help ? h('p', { class: 'ke-help', id: `${id}-help` }, spec.help) : null;
  const goBtn = h('button', { class: 'btn btn--quiet ke-go', type: 'button', hidden: true, onclick: () => commit() }, 'save');
  const L = { path: spec.path || spec.key, spec };
  let input = null, keys = null;

  const setState = (s, text = null) => {
    goBtn.hidden = s !== 'dirty' || !!spec.group;
    if (s === 'saving') replace(st, h('span', { class: 'muted' }, 'saving'));
    else if (s === 'saved') replace(st, tag('saved', 'ok'));
    else if (s === 'dirty') replace(st, tag('edited', 'warn'), h('span', { class: 'muted ke-hint' }, spec.group ? ' enter saves all' : ' enter saves, esc undoes'));
    else if (s === 'bad') replace(st, tag('bad', 'bad'));
    else replace(st, text || '');
    spec.group?.changed();
  };
  const showErr = (text) => {
    err.hidden = !text; replace(err, text || '');
    input?.setAttribute('aria-invalid', text ? 'true' : 'false');
  };

  // the value as text (inputs) or as the chosen key (choices)
  const current = () => (keys ? keys.dataset.v : input ? input.value : null);
  const savedText = () => (keys ? String(saved) : toText(saved));
  L.dirty = () => !ro && current() !== savedText();
  L.parsed = () => {
    const t = current();
    if (keys) return { value: spec.options?.find(([v]) => String(v) === t)?.[0] ?? (t === 'true' ? true : t === 'false' ? false : t) };
    if (spec.parse) return spec.parse(t);
    if (type === 'number') {
      if (t.trim() === '') return spec.required ? { error: 'a number is needed' } : { value: null };
      const n = Number(t);
      if (!Number.isFinite(n)) return { error: 'not a number' };
      const min = spec.attrs?.min != null ? Number(spec.attrs.min) : null, max = spec.attrs?.max != null ? Number(spec.attrs.max) : null;
      if ((min != null && n < min) || (max != null && n > max)) return { error: `a number from ${min ?? '...'} to ${max ?? '...'}` };
      return { value: n };
    }
    return { value: type === 'password' ? t : t.trim() === '' && !spec.keepEmpty ? null : t.trim() };
  };
  L.state = setState;
  L.error = (text) => { showErr(text); setState(text ? 'bad' : ''); };
  L.focus = () => (input || keys?.querySelector('[aria-pressed="true"]') || keys?.querySelector('button'))?.focus();
  L.set = (v) => {
    saved = v;
    if (keys) mark(String(v)); else if (input) input.value = type === 'password' ? '' : toText(v);
    if (ro && valueEl) replace(valueEl, show(v));
  };
  L.done = (v = undefined) => {
    const p = L.parsed();
    saved = v !== undefined ? v : p.value;
    if (input && type !== 'password') input.value = toText(saved);
    if (input && type === 'password') input.value = '';
    showErr(null); setState('saved');
  };
  L.undo = () => { L.set(saved); showErr(null); setState(''); };

  async function commit() {
    if (spec.group) { spec.group.commit(); return; }
    if (!L.dirty()) { setState(''); return; }
    const p = L.parsed();
    if (p.error) { L.error(p.error); return; }
    showErr(null); setState('saving');
    try {
      const out = await spec.save(p.value, L);
      L.done(out !== undefined && out !== null && typeof out !== 'object' ? out : undefined);
    } catch (e) {
      if (keys) mark(String(saved));
      L.error(saveError(e, L.path));
    }
  }
  L.commit = commit;

  const show = (v) => (spec.show ? spec.show(v) : v == null || v === '' ? h('span', { class: 'muted' }, spec.empty || '-') :
    spec.options ? (spec.options.find(([x]) => String(x) === String(v))?.[1] ?? String(v)) : type === 'bool' ? (v ? 'on' : 'off') : String(v));

  let valueEl = null;
  function mark(v) {
    keys.dataset.v = v;
    for (const b of keys.children) b.setAttribute('aria-pressed', String(b.dataset.v === v));
  }
  if (ro) {
    valueEl = h('span', { class: 'ke-show' }, show(saved), spec.unit ? h('span', { class: 'u' }, ` ${spec.unit}`) : null);
  } else if (type === 'choice' || type === 'bool') {
    const opts = type === 'bool' ? BOOL : spec.options;
    if (type === 'bool') saved = String(!!saved);
    keys = h('div', { class: 'keys ke-keys', role: 'group', 'aria-label': spec.label || spec.key, 'data-focus': '' },
      opts.map(([v, l]) => h('button', { class: 'key', type: 'button', dataset: { v: String(v) }, disabled: !!spec.disabledOption?.(v),
        onclick: () => { if (keys.dataset.v === String(v)) return; mark(String(v)); commit(); } }, l)));
    mark(String(saved));
    keys.addEventListener('keydown', (e) => { if (e.key === 'Escape') { L.undo(); e.currentTarget.querySelector('button')?.blur(); } });
    // bools save true / false, not text
    if (type === 'bool') {
      const parsed = L.parsed;
      L.parsed = () => ({ value: parsed().value === true || parsed().value === 'true' });
      const set = L.set;
      L.set = (v) => set(String(!!v));
      const done = L.done;
      L.done = () => { done(); saved = keys.dataset.v; };
    }
    valueEl = keys;
  } else {
    input = h('input', { class: 'ke-in', id, type: type === 'password' ? 'password' : type === 'date' ? 'date' : 'text',
      value: type === 'password' ? '' : toText(saved), spellcheck: 'false', autocomplete: 'off', 'data-focus': '',
      inputmode: type === 'number' ? 'decimal' : null, 'aria-describedby': [help && `${id}-help`, `${id}-err`].filter(Boolean).join(' '),
      ...(spec.attrs || {}) });
    if (type === 'password') input.placeholder = spec.attrs?.placeholder || '';
    input.addEventListener('input', () => { showErr(null); setState(L.dirty() ? 'dirty' : ''); });
    input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') { e.preventDefault(); commit(); }
      else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); if (spec.group) spec.group.undo(); else L.undo(); input.blur(); }
    });
    valueEl = h('span', { class: ['ke-box', spec.unit && 'ke-box--unit'] }, input, spec.unit ? h('span', { class: 'u ke-unit' }, spec.unit) : null, spec.after || null);
  }
  L.input = input;
  L.keys = keys;

  const keyEl = input ? h('label', { class: 'ke-k', for: id }, spec.key) : h('span', { class: 'ke-k' }, spec.key);
  L.el = h('div', { class: ['ke', ro && 'ke--ro', spec.cls], id: spec.id || null, cover: spec.cover || null, dataset: { row: '' } },
    keyEl, h('span', { class: 'ke-eq', 'aria-hidden': 'true' }, '='),
    h('div', { class: 'ke-v' }, valueEl), h('span', { class: 'ke-side' }, st, goBtn),
    help, err);
  if (spec.group) spec.group.lines.push(L);
  return L;
}

/** Many lines; lines.line(key) finds one. */
export function editLines(specs, { cls = null, cover = null } = {}) {
  const lines = specs.filter(Boolean).map((s) => (s.el instanceof Element ? s : editLine(s)));
  const el = h('div', { class: ['ke-list', cls], cover }, lines.map((l) => l.el));
  return { el, lines, line: (key) => lines.find((l) => l.spec?.key === key || l.path === key) };
}

/** A read-only line in the same layout: key = value. */
export const showLine = (key, value, { id = null, cover = null, help = null } = {}) =>
  editLine({ key, type: 'show', show: () => value, value: '', id, cover, help });
