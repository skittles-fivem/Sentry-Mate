// Parts the data pages share: the control deck (filters), the period and car switches, CSV buttons, place
// links, trip counters, equaliser bars and the stat rack. Each one only builds elements with h(); the look is
// in kit.css.

import * as state from '../shared/state.js';
import { h, svg, quantity } from '../shared/dom.js';
import { format, parts } from '../shared/units.js';
import { downloadCsv, csvFilename } from '../shared/csv.js';
import { formatNumber } from '../shared/i18n.js';
import { icon } from './ui.js';
import { openPeriod } from './period.js';
import { openCarPicker } from './pickers.js';

/** A plural: plural(3, 'drive') -> '3 drives'. */
export const plural = (n, one, many = `${one}s`) => `${formatNumber(n ?? 0)} ${n === 1 ? one : many}`;

/**
 * The control deck under the plate: a row of labelled slots, like the knobs of a car radio.
 * deck({ cover, note }, slot('Period', el), slot('Place', el, { cover }), ...)
 */
export function controls({ cover, note, label = 'Filters' } = {}, ...slots) {
  return h('section', { class: 'hud hud--plain ctl', 'aria-label': label, cover },
    h('div', { class: 'ctl-row' }, slots),
    note ? h('p', { class: 'ctl-note' }, icon('info', 'i--sm'), h('span', null, note)) : null);
}

/** One slot of the control deck: a small caption over its control. */
export function slot(caption, control, { cover, wide = false, cls } = {}) {
  return h('div', { class: ['ctl-slot', wide && 'ctl-slot--wide', cls], cover },
    h('span', { class: 'ctl-cap' }, caption), h('div', { class: 'ctl-in' }, control));
}

/** The button that opens the period picker from a page (the console row has the same dial). */
export function periodBtn(ctx, cover) {
  const p = ctx.period;
  return h('button', { class: 'btn ctl-btn', type: 'button', cover, 'aria-haspopup': 'dialog', onclick: (e) => openPeriod(e.currentTarget) },
    icon('calendar', 'i--sm'), h('span', { class: 'ell' }, p?.label || 'Period'), icon('chev-d', 'i--sm'));
}

/** The button that opens the car picker (keeps the page). */
export function carBtn(ctx, cover) {
  return h('button', { class: 'btn ctl-btn', type: 'button', cover, 'aria-haspopup': 'dialog', onclick: () => openCarPicker() },
    icon('car', 'i--sm'), h('span', { class: 'ell' }, ctx.car?.name || 'Choose a car'), icon('chev-d', 'i--sm'));
}

/** A select: select([[value, text], ...], value, onChange, { label }). */
export function select(options, value, onChange, { label, cls } = {}) {
  const el = h('select', { class: ['input', 'ctl-select', cls], 'aria-label': label || null },
    options.map((o) => (o.group ? h('optgroup', { label: o.group }, o.items.map(([v, t]) => h('option', { value: v, selected: String(v) === String(value) }, t)))
      : h('option', { value: o[0], selected: String(o[0]) === String(value) }, o[1]))));
  if (onChange) el.addEventListener('change', () => onChange(el.value));
  return el;
}

/** A search box that tells after a short pause: search(value, onText, { placeholder, label }). */
export function search(value, onText, { placeholder = 'Find', label, ctx } = {}) {
  const input = h('input', { class: 'input', type: 'search', value: value || '', placeholder, 'aria-label': label || placeholder, autocomplete: 'off' });
  let t = 0;
  input.addEventListener('input', () => { clearTimeout(t); t = setTimeout(() => onText(input.value.trim()), 320); });
  ctx?.onCleanup(() => clearTimeout(t));
  return h('label', { class: 'ctl-search' }, icon('search', 'i--sm'), input);
}

/** A Download CSV button: a server CSV (href) or rows built here (rows(), columns). */
export function csvBtn({ href, filename, rows, columns, name, label = 'CSV', cover } = {}) {
  if (href) return h('a', { class: 'btn btn--sm btn--ghost csv', href, download: filename || '', cover, title: 'Download this as a CSV file' }, icon('download', 'i--sm'), h('span', null, label));
  return h('button', { class: 'btn btn--sm btn--ghost csv', type: 'button', cover, title: 'Download this as a CSV file',
    onclick: () => downloadCsv(csvFilename(...[].concat(name || 'sentry-mate')), typeof rows === 'function' ? rows() : rows, columns) }, icon('download', 'i--sm'), h('span', null, label));
}

/* ------------------------------------------------------------------ places */

/** What a place, a spot or an address is called: the saved name, else the label, else the street. */
export function placeLabel(p) {
  if (!p) return '—';
  return p.place?.name || p.label || (typeof p.address === 'string' ? p.address : p.address?.short) || (p.lat != null ? `${p.lat.toFixed(4)}, ${p.lon.toFixed(4)}` : '—');
}
/** The place editor for a point: a saved place opens for editing, a bare spot starts a new place there. */
export function placeHref(p) {
  const id = p?.place?.id || p?.place_id;
  if (id) return state.buildUrl('places', { query: { place: id } });
  if (p?.lat != null && p?.lon != null) return state.buildUrl('places', { query: { lat: p.lat.toFixed(5), lon: p.lon.toFixed(5), new: 1 } });
  return null;
}
/** A place name that opens the place editor. */
export function placeLink(p, { text, cover } = {}) {
  const href = placeHref(p);
  const t = text ?? placeLabel(p);
  return href ? h('a', { class: 'plc', href, cover, title: p?.place ? `Edit the place ${p.place.name}` : 'Save this spot as a place' }, t) : h('span', { class: 'plc plc--bare', cover }, t);
}
/** 'Home → Work' with both ends linked. */
export function route(a, b, { open = 'driving now' } = {}) {
  return h('span', { class: 'route' }, placeLink(a), h('span', { class: 'route-to', 'aria-hidden': 'true' }, icon('arrow-r', 'i--sm')), h('span', { class: 'sr-only' }, ' to '), b ? placeLink(b) : h('span', { class: 'plc plc--bare' }, open));
}

/* ------------------------------------------------------------------ figures */

/**
 * The stat rack: a grid of figures, each a label, a value in the person's units and a line on how it is
 * worked out. rack([stat(...), ...], { cols }).
 */
export const rack = (items, { cls, cover } = {}) => h('div', { class: ['rack', cls], cover }, items);

/** One figure: stat('Distance', 12.3, 'distance', { how, why, cover, hot }). kind null: value as it is. */
export function stat(label, value, kind, o = {}) {
  const v = kind ? quantity(value, kind, { why: o.why, digits: o.digits, sign: o.sign }) : (value == null || value === '' ? quantity(null, 'number', { why: o.why }) : value);
  return h('div', { class: ['stat', o.hot && 'stat--hot', o.cls], cover: o.cover, title: o.title || null },
    h('span', { class: 'stat-label' }, o.icon ? icon(o.icon, 'i--sm') : null, h('span', null, label)),
    h('span', { class: 'stat-value' }, v),
    o.how ? h('span', { class: 'stat-how' }, o.how) : null);
}

/**
 * Trip counters: rolling digits with a caption, like the trip meters of a dashboard.
 * counters([{ label, value, kind, digits, how, cover }])
 */
export function counters(list, { cover } = {}) {
  return h('div', { class: 'ctrs', cover }, list.map((c) => {
    const p = c.kind ? parts(c.value, c.kind, { digits: c.digits }) : null;
    const text = c.text ?? (p ? p.number : c.value ?? '—');
    const unit = c.unit ?? (p && !p.missing ? p.unit : '');
    return h('div', { class: 'ctr', cover: c.cover },
      h('span', { class: 'ctr-label' }, c.label),
      h('span', { class: 'ctr-face' }, h('span', { class: 'ctr-digits' }, text), unit ? h('span', { class: 'ctr-unit' }, unit) : null),
      c.how ? h('span', { class: 'ctr-how' }, c.how) : null);
  }));
}

/**
 * Equaliser bars: eq([{ label, value: 0..1, text, href, title, tone }], { cover, label }). Rows of a label, a
 * bar and its figure, aligned in three columns.
 */
export function eq(rows, { cover, label, empty = 'Nothing to show.' } = {}) {
  if (!rows.length) return h('p', { class: 'note', cover }, icon('info', 'i--sm'), h('span', null, empty));
  return h('ul', { class: 'eq', cover, 'aria-label': label || null }, rows.map((r) => h('li', { class: 'eq-row', title: r.title || null },
    r.href ? h('a', { class: 'eq-label ell', href: r.href }, r.label) : h('span', { class: 'eq-label ell' }, r.label),
    h('span', { class: ['eq-bar', r.tone && `eq-bar--${r.tone}`], 'aria-hidden': 'true' }, h('i', { vars: { '--w': `${(Math.max(0, Math.min(1, r.value || 0)) * 100).toFixed(2)}%` } })),
    h('span', { class: 'eq-val' }, r.text))));
}

/** Small columns per bucket: spark(values) (heights relative to the largest). */
export function spark(values, { label } = {}) {
  const max = Math.max(1e-9, ...values.map((v) => v || 0));
  const n = Math.max(1, values.length);
  // one SVG stretched to the box: thin columns stay crisp at any pixel ratio
  return h('span', { class: 'spark', role: label ? 'img' : null, 'aria-label': label || null, 'aria-hidden': label ? null : 'true' },
    svg('svg', { class: 'spark-svg', viewBox: `0 0 ${n * 10} 100`, preserveAspectRatio: 'none', 'shape-rendering': 'crispEdges' },
      values.map((v, i) => { const hh = Math.max(1, ((v || 0) / max) * 100); return svg('rect', { x: i * 10 + 1, y: (100 - hh).toFixed(2), width: 8, height: hh.toFixed(2) }); })));
}

/** A section head inside a page (outside a panel): an index number and a title, like a band on the dial. */
export function band(no, title, side) {
  return h('div', { class: 'band' }, h('span', { class: 'band-no' }, String(no).padStart(2, '0')), h('h2', { class: 'band-title' }, title), h('span', { class: 'band-rule', 'aria-hidden': 'true' }), side ? h('div', { class: 'band-side' }, side) : null);
}

/** A source tag for rows that did not come from the CAN Bridge. */
export const sourceTag = (s) => (s && s !== 'can' ? h('span', { class: 'tag tag--org', title: 'Imported from TeslaMate: energy is estimated and points are sparser' }, 'Imported') : null);

/** A share of the period for a state strip piece. */
export const pct = (v) => format(v, 'percent');
