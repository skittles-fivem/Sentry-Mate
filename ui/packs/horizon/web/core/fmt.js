// Formatting helpers on shared/units.js and shared/time.js. The API is metric with UTC times; these show
// values in the person's units and times in the car's zone, against the server's clock (api.now()).

import * as api from '../shared/api.js';
import { h, quantity } from '../shared/dom.js';
import { format, formatMoneyList } from '../shared/units.js';
import { formatTime, formatDate, formatDateTime, formatWhen, formatDuration, formatRelative, zoneName } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';

export const DASH = '—';
export const SEP = ' · ';

/** A value with its unit as an element (never split at a line end). */
export const q = (value, kind, opts) => quantity(value, kind, opts);
/** The same as text. */
export const qt = (value, kind, opts) => format(value, kind, opts);
/** A plain number in the person's locale. */
export const num = (v, digits = 0) => (v == null || Number.isNaN(v) ? DASH : formatNumber(v, { minimumFractionDigits: digits, maximumFractionDigits: digits }));
/** Money in each currency it is in (never added across currencies). */
export const money = (list) => (Array.isArray(list) && list.length ? formatMoneyList(list) : DASH);
/** The value of a live { value, at_ms } (or a plain value). */
export const val = (x) => (x && typeof x === 'object' && 'value' in x ? x.value : x ?? null);

export const time = (ms, tz) => (ms == null ? DASH : formatTime(ms, tz));
export const date = (ms, tz, opts) => (ms == null ? DASH : formatDate(ms, tz, { now: api.now(), ...opts }));
export const dateTime = (ms, tz, opts) => (ms == null ? DASH : formatDateTime(ms, tz, { now: api.now(), ...opts }));
export const when = (ms, tz) => (ms == null ? DASH : formatWhen(ms, tz, api.now()));
export const dur = (ms) => (ms == null ? DASH : formatDuration(ms));
export const ago = (ms) => (ms == null ? DASH : formatRelative(ms, api.now()));
export const zone = (tz) => zoneName(api.now(), tz);

/** '5 min ago' that the frame refreshes every 15 s (elements with data-ago). */
export const agoEl = (ms) => h('time', { class: 'ago', dataset: { ago: ms ?? '' }, datetime: ms ? new Date(ms).toISOString() : null }, ago(ms));
export function refreshAgo(root = document) {
  for (const el of root.querySelectorAll('[data-ago]')) { const ms = Number(el.dataset.ago); if (ms) el.textContent = ago(ms); }
}

/** Car states, with a word for each (a colour is never the only sign). */
export const STATES = Object.freeze({
  driving: { word: 'Driving', icon: 'route' },
  charging: { word: 'Charging', icon: 'bolt' },
  parked: { word: 'Parked', icon: 'pin' },
  asleep: { word: 'Asleep', icon: 'moon' },
  unknown: { word: 'No data', icon: 'signal' },
});
export const stateWord = (s) => (STATES[s] || STATES.unknown).word;

/** 'Model Y · 2025' */
export const carLine = (c) => [c?.model, c?.year].filter(Boolean).join(SEP);
/** 'Driving · 64 %' or 'Last known: parked' for a car list item. */
export function carStatus(c) {
  if (c?.sold) return c.sold_date ? `Sold on ${c.sold_date}` : 'Sold';
  const n = c?.now || {};
  const known = n.state && n.state !== 'unknown';
  const s = n.stale && known ? `Last known: ${stateWord(n.state).toLowerCase()}` : stateWord(n.state);
  return n.soc_pct != null ? `${s}${SEP}${format(n.soc_pct, 'percent')}` : s;
}

/** A place's name, or the street address, or a plain 'Unnamed spot'. */
export function placeName(p) {
  if (!p) return DASH;
  return p.name || p.place?.name || p.address || 'Unnamed spot';
}

export { formatNumber };
