// Formatting for the console, on shared/units.js and shared/time.js. Numbers carry their unit in muted
// text after them; dates and times are in the zone the page passes (ctx.tz: the car's).

import * as api from '../shared/api.js';
import { h } from '../shared/dom.js';
import { parts, format, DASH } from '../shared/units.js';
import { formatDate, formatTime, formatDateTime, formatWhen, formatDuration, formatRelative, formatDateRange } from '../shared/time.js';

export { DASH };

/** A number with its unit: <span class="n">312.4<span class="u"> km</span></span>. Null: a dash and the reason. */
export function num(value, kind, opts = {}) {
  const p = parts(value, kind, opts);
  if (p.missing) return h('span', { class: 'n is-missing', title: p.why || null }, p.number);
  return h('span', { class: 'n' }, p.number, p.unit ? h('span', { class: 'u' }, p.sep, p.unit) : null);
}
/** The same as text: '312.4 km'. */
export const numText = (value, kind, opts) => format(value, kind, opts);

export const time = (ms, tz) => (ms == null ? DASH : formatTime(ms, tz));
export const date = (ms, tz, opts) => (ms == null ? DASH : formatDate(ms, tz, { weekday: false, ...opts }));
export const dateTime = (ms, tz, opts) => (ms == null ? DASH : formatDateTime(ms, tz, { weekday: false, ...opts }));
export const when = (ms, tz) => (ms == null ? DASH : formatWhen(ms, tz, api.now()));
export const dur = (ms) => (ms == null ? DASH : formatDuration(ms));
export const ago = (ms) => (ms == null ? DASH : formatRelative(ms, api.now()));
export const range = (from, to, tz) => formatDateRange(from, to, tz);

/** A lowercase path segment for a name: 'Model Y' -> 'model-y'. */
export const slug = (s) => String(s || '').toLowerCase().normalize('NFKD').replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '') || 'car';

/** Car states and their words; the tone is the CSS class of the word. */
export const STATES = Object.freeze({
  driving: 'driving', charging: 'charging', parked: 'parked', asleep: 'asleep', offline: 'offline', unknown: 'unknown',
});
/** [driving] as an element in the state's colour. */
export function stateWord(st, { stale = false } = {}) {
  const k = STATES[st] ? st : 'unknown';
  return h('span', { class: ['st', `st--${k}`], title: stale ? 'last known state; the car has not sent anything new' : null }, `[${STATES[k]}${stale ? '?' : ''}]`);
}
/** A tag like [ok] [warn] [bad] [todo] [running], or any word. */
export function tag(word, tone = null) {
  return h('span', { class: ['tag', tone && `tag--${tone}`] }, `[${word}]`);
}
