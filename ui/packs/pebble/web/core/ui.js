// Sentry Mate, "Pebble" UI pack: the small parts every page and the frame are built from.
//
// Icons (drawn on a 24 px grid), buttons, pills, chips, tiles, the SOC ring, numbers with their unit on one
// baseline, state words with their dot, and times that keep saying how long ago they were. Everything is
// built with h() from shared/dom.js, so text from the API is always text. Styles are in app.css.

import { h, svg } from '../shared/dom.js';
import { parts } from '../shared/units.js';
import { formatRelative } from '../shared/time.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';

/* ------------------------------------------------------------------ icons: 24 x 24, 2 px strokes, round ends */

const C = (cx, cy, r, fill = false) => ['circle', { cx, cy, r, class: fill ? 'ic-fill' : null }];
const P = (d) => ['path', { d }];
const R = (x, y, width, height, rx) => ['rect', { x, y, width, height, rx }];

const ICONS = {
  fleet: [R(4, 4, 7, 7, 2), R(13, 4, 7, 7, 2), R(4, 13, 7, 7, 2), R(13, 13, 7, 7, 2)],
  car: [P('M4 16.5V12l2-5a2 2 0 0 1 1.9-1.3h8.2A2 2 0 0 1 18 7l2 5v4.5'), P('M4 12h16'), C(8, 16.5, 1.8), C(16, 16.5, 1.8)],
  route: [C(6, 18, 2), C(18, 6, 2), P('M8 18h6.5a3 3 0 0 0 0-6h-5a3 3 0 0 1 0-6H16')],
  bolt: [P('M13 3 5.5 13.5H11L10 21l7.5-10.5H12L13 3Z')],
  battery: [R(3, 7, 15, 10, 3), P('M21 10.5v3'), P('M7 10.5v3M10.5 10.5v3')],
  more: [C(6, 12, 1.6, true), C(12, 12, 1.6, true), C(18, 12, 1.6, true)],
  timeline: [P('M9 6h11M9 12h11M9 18h11'), C(4.5, 6, 1.5, true), C(4.5, 12, 1.5, true), C(4.5, 18, 1.5, true)],
  trip: [P('M5.5 21V4h11l-2.5 4 2.5 4h-11')],
  book: [P('M5 5.5A1.5 1.5 0 0 1 6.5 4H18v13H7.5A2.5 2.5 0 0 0 5 19.5v-14Z'), P('M5 19.5A2.5 2.5 0 0 0 7.5 22H18v-5')],
  leaf: [P('M5 19C5 11 10 5 19 5c0 9-6 14-14 14Z'), P('M5 19l7.5-7.5')],
  bars: [P('M4 20h16'), P('M7 16v-5M12 16V6M17 16V9')],
  globe: [C(12, 12, 8.5), P('M3.5 12h17'), P('M12 3.5c2.4 2.3 3.6 5.1 3.6 8.5s-1.2 6.2-3.6 8.5c-2.4-2.3-3.6-5.1-3.6-8.5S9.6 5.8 12 3.5Z')],
  chip: [R(7, 7, 10, 10, 2), P('M10 3.5V7M14 3.5V7M10 17v3.5M14 17v3.5M3.5 10H7M3.5 14H7M17 10h3.5M17 14h3.5')],
  pin: [P('M12 21s-6.5-5.6-6.5-11a6.5 6.5 0 0 1 13 0c0 5.4-6.5 11-6.5 11Z'), C(12, 10, 2.3)],
  sun: [C(12, 12, 3.5), P('M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6 7 7M17 17l1.4 1.4M5.6 18.4 7 17M17 7l1.4-1.4')],
  moon: [P('M19.5 14.5A8 8 0 0 1 9.5 4.5a8 8 0 1 0 10 10Z')],
  plug: [P('M9 3v4M15 3v4M6.5 7h11v3.5a5.5 5.5 0 0 1-11 0V7ZM12 16v5')],
  server: [R(4, 4, 16, 7, 2), R(4, 13, 16, 7, 2), P('M8 7.5h.01M8 16.5h.01')],
  upload: [P('M12 15V4M7.5 8.5 12 4l4.5 4.5M5 20h14')],
  download: [P('M12 4v11M7.5 10.5 12 15l4.5-4.5M5 20h14')],
  pulse: [P('M3 12h4l2.5-6 5 12 2.5-6h4')],
  help: [C(12, 12, 8.5), P('M9.6 9.4a2.5 2.5 0 0 1 4.9.7c0 1.7-2.5 2.1-2.5 3.6'), P('M12 17h.01')],
  person: [C(12, 8.5, 3.5), P('M5 20c1-3.6 3.8-5.5 7-5.5s6 1.9 7 5.5')],
  spark: [P('M12 3v4M12 17v4M3 12h4M17 12h4M6 6l2.5 2.5M15.5 15.5 18 18M6 18l2.5-2.5M15.5 8.5 18 6')],
  calendar: [R(4, 5.5, 16, 14.5, 3), P('M4 10h16M8.5 3.5v4M15.5 3.5v4')],
  chevronRight: [P('m9.5 6 6 6-6 6')],
  chevronLeft: [P('m14.5 6-6 6 6 6')],
  chevronDown: [P('m6 9.5 6 6 6-6')],
  close: [P('M6.5 6.5l11 11M17.5 6.5l-11 11')],
  check: [P('m5.5 12.5 4 4 9-9.5')],
  warning: [P('M12 4 2.8 19.5h18.4L12 4Z'), P('M12 10v4.2M12 17h.01')],
  info: [C(12, 12, 8.5), P('M12 11v5.5M12 7.8h.01')],
  search: [C(10.5, 10.5, 6), P('m15 15 5 5')],
  copy: [R(8, 8, 12, 12, 2.5), P('M16 8V6.5A2.5 2.5 0 0 0 13.5 4h-7A2.5 2.5 0 0 0 4 6.5v7A2.5 2.5 0 0 0 6.5 16H8')],
  arrowRight: [P('M4.5 12h15M13.5 6l6 6-6 6')],
  reset: [P('M4.5 12a7.5 7.5 0 1 0 2.2-5.3'), P('M4.5 4v4.2h4.2')],
  signout: [P('M10 4.5H6.5a2 2 0 0 0-2 2v11a2 2 0 0 0 2 2H10'), P('M14.5 8l4 4-4 4M18.5 12H9.5')],
  offline: [P('M3.5 3.5l17 17'), P('M8.6 16.2a5 5 0 0 1 6.8 0M5.2 12.7a10 10 0 0 1 4.2-2.4M18.8 12.7a10 10 0 0 0-3-2.1M2 9.2a14.5 14.5 0 0 1 4-2.6'), P('M12 20h.01')],
  lock: [R(5, 10.5, 14, 10, 2.5), P('M8 10.5V8a4 4 0 0 1 8 0v2.5')],
  key: [C(8, 15, 4), P('M11 12l8.5-8.5M16 7l2.5 2.5M13.5 9.5 16 12')],
  file: [P('M7 3.5h7l4.5 4.5v12.5H7a1.5 1.5 0 0 1-1.5-1.5V5A1.5 1.5 0 0 1 7 3.5Z'), P('M14 3.5V8h4.5')],
  move: [P('M4 8h13M13.5 4.5 17 8l-3.5 3.5'), P('M20 16H7M10.5 12.5 7 16l3.5 3.5')],
  minus: [P('M6 12h12')],
  plus: [P('M12 6v12M6 12h12')],
};

/** An icon: icon('bolt'). Decorative (aria-hidden): say what it means in text next to it. */
export function icon(name, cls = null) {
  return svg('svg', { class: ['ic', cls].filter(Boolean).join(' '), viewBox: '0 0 24 24', 'aria-hidden': 'true', focusable: 'false' },
    (ICONS[name] || ICONS.info).map(([tag, attrs]) => svg(tag, attrs)));
}
export const ICON_NAMES = Object.freeze(Object.keys(ICONS));

/* ------------------------------------------------------------------ buttons, pills, chips */

/**
 * A button: btn('Save', { kind: 'primary' | 'soft' | 'ghost' | 'danger', icon, onclick, small, iconOnly }).
 * iconOnly: a round 44 px button whose name is the text (said by screen readers, shown as a tooltip).
 */
export function btn(text, { kind = 'soft', icon: ic = null, onclick = null, type = 'button', disabled = false, cover = null, cls = null, iconOnly = false, title = null, attrs = {} } = {}) {
  return h('button', {
    type, class: ['btn', `btn-${kind}`, { 'btn-icon': iconOnly }, cls], onclick, disabled, cover,
    'aria-label': iconOnly ? text : null, title: iconOnly ? (title || text) : title, ...attrs,
  }, ic ? icon(ic) : null, iconOnly ? null : h('span', { class: 'btn-text' }, text));
}
/** A link that looks like a button. */
export function linkBtn(text, href, { kind = 'soft', icon: ic = null, cover = null, cls = null } = {}) {
  return h('a', { class: ['btn', `btn-${kind}`, cls], href, cover }, ic ? icon(ic) : null, h('span', { class: 'btn-text' }, text));
}
/** A small rounded label: chip('Plugged in', { tone: 'teal' }). */
export const chip = (text, { tone = null, icon: ic = null, cover = null, title = null } = {}) =>
  h('span', { class: ['chip', tone && `tone-${tone}`], cover, title }, ic ? icon(ic) : null, h('span', null, text));

/* ------------------------------------------------------------------ car states */

export const STATES = Object.freeze({ driving: 'Driving', charging: 'Charging', parked: 'Parked', asleep: 'Asleep', online: 'Awake', updating: 'Updating', offline: 'Offline', unknown: 'Unknown' });
/** 'Parked', or 'Parked, no news' when no upload came for a long time. */
export const stateWord = (s, stale = false) => (STATES[s] || 'Unknown') + (stale && s !== 'unknown' && s !== 'offline' ? ', no news' : '');
/** A state word with its coloured dot: stateTag('charging'). */
export function stateTag(s, { stale = false, extra = null } = {}) {
  return h('span', { class: ['state', `st-${STATES[s] ? s : 'unknown'}`, { 'is-stale': stale }] },
    h('span', { class: 'dot', 'aria-hidden': 'true' }), h('span', { class: 'state-word' }, stateWord(s, stale)), extra);
}

/* ------------------------------------------------------------------ numbers */

/**
 * A number with its unit on one baseline, the unit smaller and lighter: num(72, 'percent'). size: 's' | 'm' |
 * 'l' | 'xl' (16, 28, 44, 72 px at the standard size). A missing value shows a dash and, when given, why.
 */
export function num(value, kind, { size = 'm', why = null, digits, cls = null, cover = null } = {}) {
  const p = parts(value, kind, { why, digits });
  if (p.missing) {
    return h('span', { class: ['num', `num-${size}`, 'is-missing', cls], cover },
      h('span', { class: 'num-n' }, p.number), p.why ? h('span', { class: 'num-why' }, p.why) : null);
  }
  return h('span', { class: ['num', `num-${size}`, cls], cover },
    h('span', { class: 'num-n' }, p.number), p.unit ? h('span', { class: 'num-u' }, p.unit) : null);
}

/**
 * A round gauge with the number in its middle (centred by the grid, not by nudging): ring(72, { label:
 * 'Battery' }). pct 0 to 100 fills the arc; it fills once on first show unless motion is reduced.
 */
export function ring(pct, { label = null, size = 'm', tone = 'coral', text = null, unit = '%', cover = null, cls = null } = {}) {
  const v = Number.isFinite(pct) ? Math.max(0, Math.min(100, pct)) : null;
  const R0 = 42, L = 2 * Math.PI * R0;
  const arc = svg('circle', { class: 'ring-arc', cx: 50, cy: 50, r: R0, 'stroke-dasharray': `${L.toFixed(2)} ${L.toFixed(2)}`, pathLength: null });
  arc.style.setProperty('--ring-len', L.toFixed(2));
  arc.style.setProperty('--ring-off', v == null ? L.toFixed(2) : (L * (1 - v / 100)).toFixed(2));
  return h('div', { class: ['ring', `ring-${size}`, `tone-${tone}`, cls], cover, role: 'img', 'aria-label': `${label ? label + ': ' : ''}${v == null ? 'not known' : `${Math.round(v)} ${unit === '%' ? 'percent' : unit}`}` },
    svg('svg', { class: 'ring-svg', viewBox: '0 0 100 100', 'aria-hidden': 'true', focusable: 'false' },
      svg('circle', { class: 'ring-track', cx: 50, cy: 50, r: R0 }), arc),
    h('span', { class: 'ring-mid', 'aria-hidden': 'true' },
      h('span', { class: 'ring-n' }, text != null ? text : v == null ? '—' : String(Math.round(v)), v != null && text == null && unit ? h('span', { class: 'ring-u' }, unit) : null),
      label ? h('span', { class: 'ring-label' }, label) : null));
}

/* ------------------------------------------------------------------ tiles */

/**
 * A rounded tile of the bento grid: tile({ w: 1 | 2 | 3 | 4 | 'full', tall, title, icon, side, href, cover, cls,
 * id, tone }, ...children). w is columns (clamped to the grid); href makes the whole tile a link that presses in.
 */
export function tile({ w = 1, tall = false, title = null, icon: ic = null, side = null, href = null, cover = null, cls = null, id = null, label = null } = {}, ...children) {
  const head = title || side ? h('div', { class: 'tile-head' },
    title ? h('h2', { class: 'tile-title', id: id ? `${id}-t` : null }, ic ? icon(ic, 'tile-ic') : null, h('span', null, title)) : h('span'),
    side ? h('div', { class: 'tile-side' }, side) : null) : null;
  const tag = href ? 'a' : 'section';
  return h(tag, {
    class: ['tile', `w-${w}`, { 'is-tall': tall, 'is-link': !!href }, cls], href, id, cover,
    'aria-labelledby': !href && title && id ? `${id}-t` : null, 'aria-label': label,
  }, head, children);
}

/** A line of plain help text inside a tile. */
export const note = (...text) => h('p', { class: 'note' }, ...text);

/* ------------------------------------------------------------------ times */

/** A time that keeps saying how long ago it was; the frame refreshes every [data-ago] each 20 s. */
export function ago(ms) {
  if (ms == null) return h('span', { class: 'ago is-missing' }, '—');
  return h('time', { class: 'ago', datetime: new Date(ms).toISOString(), dataset: { ago: String(ms) } }, formatRelative(ms, api.now()));
}
export function refreshAges(root = document) {
  const now = api.now();
  for (const el of root.querySelectorAll('[data-ago]')) el.textContent = formatRelative(Number(el.dataset.ago), now);
}

/** A link to a page that carries the car and the period (or '/' when the page needs a car and none is known). */
export function safeLink(page, params) {
  try { return state.linkTo(page, params); } catch { return '/'; }
}

/** The car's initial for its round avatar ('C' for Comet). */
export const initial = (name) => (String(name || '?').trim().match(/\p{L}|\p{N}/u) || ['?'])[0].toUpperCase();
