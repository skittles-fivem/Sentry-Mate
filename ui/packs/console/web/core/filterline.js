// The filter line: one text field with a prompt, 'filter> from:2026-10 min:10km place:home'.
// Words with a colon are keys (key:value, the value may be "quoted"); the other words are free text.
// The page says which keys it knows; the line under the field lists them and shows what is on.
//
//   const f = filterLine({ keys: { min: 'shortest drive, e.g. min:10km', place: 'a place name' }, value, onApply(parsed) });
//   f.el; f.set(text)
//   parsed: { keys: { min: '10km', place: 'home' }, text: 'free words', bad: ['xyz:1'] }

import { h, replace } from '../shared/dom.js';
import { zonedToMs, addPeriod } from '../shared/time.js';

/** Split a filter line into keys and free text. */
export function parseFilter(s, known) {
  const keys = {}, words = [], bad = [];
  const re = /(\w+):("([^"]*)"|\S+)|"([^"]*)"|(\S+)/g;
  let m;
  while ((m = re.exec(String(s || '')))) {
    if (m[1]) {
      const k = m[1].toLowerCase();
      const v = m[3] ?? m[2];
      if (known[k]) keys[k] = v; else bad.push(m[0]);
    } else words.push(m[4] ?? m[5]);
  }
  return { keys, text: words.join(' ').trim(), bad };
}

/** 'from:2026-10' / '2026-10-03' / '2026' -> [start, end) in ms in the zone, or null. */
export function dateSpan(v, tz) {
  const m = /^(\d{4})(?:-(\d{1,2})(?:-(\d{1,2}))?)?$/.exec(String(v || ''));
  if (!m) return null;
  const year = +m[1], month = m[2] ? +m[2] : null, day = m[3] ? +m[3] : null;
  if ((month && (month < 1 || month > 12)) || (day && (day < 1 || day > 31))) return null;
  const start = zonedToMs({ year, month: month || 1, day: day || 1 }, tz);
  const end = addPeriod(start, day ? 'day' : month ? 'month' : 'year', 1, tz);
  return [start, end];
}

/** A number with an optional unit: '10km' -> 10, '12.5' -> 12.5; null when it is not one. */
export const numberOf = (v) => { const m = /^(-?\d+(?:[.,]\d+)?)/.exec(String(v || '')); return m ? parseFloat(m[1].replace(',', '.')) : null; };

export function filterLine({ keys, value = '', placeholder = '', onApply, label = 'Filter', cover = null }) {
  const input = h('input', { class: 'in', type: 'search', value, placeholder, 'aria-label': label, spellcheck: 'false', autocomplete: 'off' });
  const on = h('div', { class: 'filt-on' });
  const help = h('p', { class: 'filt-help' }, Object.entries(keys).map(([k, what], i) => [i ? '   ' : null, h('span', { class: 'tone--accent' }, `${k}:`), ` ${what}`]));
  const show = (p) => replace(on,
    Object.entries(p.keys).map(([k, v]) => h('span', { class: 'tag tag--accent' }, `[${k}:${v}]`)),
    p.text ? h('span', { class: 'tag' }, `["${p.text}"]`) : null,
    p.bad.map((b) => h('span', { class: 'tag tag--bad', title: 'not a key this page knows' }, `[? ${b}]`)));
  let timer = 0;
  const apply = () => { const p = parseFilter(input.value, keys); show(p); onApply(p, input.value.trim()); };
  input.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(apply, 400); });
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') { e.preventDefault(); clearTimeout(timer); apply(); }
    else if (e.key === 'Escape' && input.value) { e.preventDefault(); e.stopPropagation(); input.value = ''; clearTimeout(timer); apply(); }
  });
  show(parseFilter(value, keys));
  const el = h('div', { class: 'filt-block', cover }, h('label', { class: 'filt' }, h('span', { class: 'filt-p' }, 'filter>'), input), on, help);
  return { el, input, set(v) { input.value = v; apply(); }, parsed: () => parseFilter(input.value, keys) };
}
