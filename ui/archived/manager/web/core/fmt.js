// Formatting in the person's units and the car's time zone, as text or as elements. Thin wrappers over
// shared/units.js and shared/time.js, so every page writes numbers, dates and states the same way.

import * as api from '../shared/api.js';
import { h, quantity } from '../shared/dom.js';
import { format, parts, formatMoneyList, unitLabel } from '../shared/units.js';
import { formatNumber } from '../shared/i18n.js';
import {
  formatTime, formatDate, formatDateTime, formatWhen, formatRelative, formatDuration, zoneName,
} from '../shared/time.js';
import { icon } from './icons.js';

export const SEP = ' · ';
export const DASH = '—';

/** A value with its unit, as an element (number and unit never part at a line break). */
export const q = (value, kind, opts) => quantity(value, kind, opts);
/** The same as text: q(7.6, 'distance') -> '7.6 mi'. */
export const qt = (value, kind, opts) => format(value, kind, opts);
/** The unit of a kind in the person's units: unit('speed') -> 'mph'. */
export const unit = (kind) => unitLabel(kind);
/** A plain number in the locale (counts): num(1234) -> '1,234'. */
export const num = (v, digits = 0) => (v == null || Number.isNaN(v) ? DASH : formatNumber(v, { minimumFractionDigits: digits, maximumFractionDigits: digits }));
/** Money per currency, never added across currencies: money([{ currency, amount }]) -> '$12.40 + CA$3.10'. */
export const money = (list) => (Array.isArray(list) && list.length ? formatMoneyList(list) : DASH);
/** The value of a live { value, at_ms } pair, or the plain value. */
export const val = (x) => (x && typeof x === 'object' && 'value' in x ? x.value : x ?? null);

/** '5:30 PM' in the zone. */
export const time = (ms, tz) => (ms == null ? DASH : formatTime(ms, tz));
/** 'Mon, Nov 16' in the zone. */
export const date = (ms, tz, opts) => (ms == null ? DASH : formatDate(ms, tz, { now: api.now(), ...opts }));
/** 'Mon, Nov 16, 5:30 PM' in the zone. */
export const dateTime = (ms, tz, opts) => (ms == null ? DASH : formatDateTime(ms, tz, { now: api.now(), ...opts }));
/** 'Today, 5:30 PM', 'Yesterday, 9:02 AM', or the date and time. */
export const when = (ms, tz) => (ms == null ? DASH : formatWhen(ms, tz, api.now()));
/** '2 h 05 min' from milliseconds. */
export const dur = (ms) => (ms == null ? DASH : formatDuration(ms));
/** '5 min ago' against the server's clock. */
export const ago = (ms) => (ms == null ? DASH : formatRelative(ms, api.now()));
/** The zone's short name now: 'PST'. */
export const tzName = (tz) => zoneName(api.now(), tz);

/**
 * '5 min ago' as an element that stays right: the frame refreshes every [data-ago] element every 15 s.
 * opts.title: a tooltip with the exact time in the zone.
 */
export function agoEl(ms, tz = null) {
  const el = h('span', { class: 'ago', dataset: { ago: ms ?? '' }, title: ms != null && tz ? dateTime(ms, tz) : null }, ago(ms));
  return el;
}
/** Refresh every [data-ago] element under root (the frame calls this every 15 s). */
export function refreshAgo(root = document) {
  for (const el of root.querySelectorAll('[data-ago]')) {
    const ms = Number(el.dataset.ago);
    if (el.dataset.ago === '' || !Number.isFinite(ms)) continue;
    // only when the words changed: a text written again, even the same, counts as a change in a table, and the
    // whole-pixel snap would measure every list on the page again each time (a long signal list, every 15 s)
    const t = ago(ms);
    if (el.textContent !== t) el.textContent = t;
  }
}

/** Parts joined with ' · ', each kept on one line when short: joinParts(['1.3 mi', '5 min']). */
export function joinParts(list) {
  const out = [];
  for (const p of list.filter((x) => x != null && x !== '' && x !== false)) {
    if (out.length) out.push(SEP);
    out.push(typeof p === 'string' && p.length <= 28 ? h('span', { class: 'nowrap' }, p) : p);
  }
  return out;
}

/* ------------------------------------------------------------------ car states */

/** The five car states of the API, with their word and icon. */
export const STATES = Object.freeze({
  driving: { label: 'Driving', icon: 'steer' },
  charging: { label: 'Charging', icon: 'bolt' },
  parked: { label: 'Parked', icon: 'park' },
  asleep: { label: 'Asleep', icon: 'moon' },
  unknown: { label: 'Unknown', icon: 'question' },
});
export const stateLabel = (s) => (STATES[s] || STATES.unknown).label;

/** A state pill: statePill('driving', { stale, lg, text }). Stale draws it dashed and says so. */
export function statePill(state, { stale = false, lg = false, text = null } = {}) {
  return h('span', { class: ['pill', lg && 'pill--lg', stale && 'is-stale'], dataset: { state: STATES[state] ? state : 'unknown' } },
    h('span', { class: 'dot', 'aria-hidden': 'true' }), text || stateLabel(state),
    stale ? h('span', { class: 'sr-only' }, ' (last known, no upload for over an hour)') : null);
}

/** Tyre names, in the API's order: front left, front right, rear left, rear right. */
export const TYRES = Object.freeze([['fl', 'Front left'], ['fr', 'Front right'], ['rl', 'Rear left'], ['rr', 'Rear right']]);

/** What a place point shows: the place name, else the label or short address. */
export function placeName(p) {
  if (!p) return null;
  if (typeof p === 'string') return p;
  return p.place?.name || p.label || (typeof p.address === 'string' ? p.address : p.address?.short) || null;
}
/** Where a point is when it has no place and no address (address lookup is off): its position, '0.3000, 0.2180'. */
export function spotText(p) {
  if (!p || typeof p !== 'object') return null;
  const lat = p.lat ?? p.position?.lat, lon = p.lon ?? p.position?.lon;
  return Number.isFinite(lat) && Number.isFinite(lon) ? `${lat.toFixed(4)}, ${lon.toFixed(4)}` : null;
}
/** The address line under a place name ('Sanchez St, Noe Valley'), or null when it would repeat the name. */
export function placeDetail(p) {
  if (!p || typeof p !== 'object') return null;
  const a = typeof p.address === 'string' ? p.address : p.address?.short;
  return a && a !== placeName(p) ? a : null;
}

/** 'Model Y · 2025' */
export const carMeta = (c) => [c.model, c.year].filter(Boolean).join(SEP);

/** An icon and text in one inline element: withIcon('warn', 'Low'). */
export const withIcon = (id, text, cls) => h('span', { class: ['with-icon', cls] }, icon(id, 'ico--sm'), text);

/** The pieces of a formatted value, for pages that place number and unit themselves. */
export { parts };
