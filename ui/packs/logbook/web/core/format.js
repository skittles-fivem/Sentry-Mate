// Sentry Mate, "Logbook" UI pack: text for times, dates, durations and values.
//
// Thin glue over shared/time.js and shared/units.js, in the forms this pack prints: every time in the
// car's time zone (pass ctx.tz), every value with its unit in the person's units. Elements for the same
// things (a figure with a quieter unit, an age that keeps itself up to date) are in ui.js.

import * as api from '../shared/api.js';
import { format } from '../shared/units.js';
import {
  formatTime, formatDate, formatDateTime, formatWhen, formatRelative, formatDuration, formatDayKey,
  dayKey, addDays, dateFormat, zonedParts,
} from '../shared/time.js';

/** The browser's own time zone, for pages that show no car. */
export const browserZone = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC'; } catch { return 'UTC'; } })();

/** A value with its unit in the person's units: T(12.3, 'distance') -> '7.6 mi'; null -> '—'. */
export const T = (value, kind, opts) => format(value, kind, opts);
/** A length of time in ms: '12 min', '2 h 05 min', '3 d 4 h'. */
export const dur = (ms, opts) => formatDuration(ms, opts);
/** How long ago, from the server's clock: '5 min ago'. */
export const ago = (ms) => formatRelative(ms, api.now());
/** '5:30 PM' in the zone. */
export const time = (ms, tz, opts) => formatTime(ms, tz, opts);
/** 'Mon, Nov 16' (the year when it is not this year). */
export const date = (ms, tz, opts) => formatDate(ms, tz, { now: api.now(), ...opts });
/** 'Mon, Nov 16, 5:30 PM'. */
export const dateTime = (ms, tz, opts) => formatDateTime(ms, tz, { now: api.now(), ...opts });
/** 'Today, 5:30 PM', 'Yesterday, 9:12 AM' or 'Mon, Nov 9, 5:30 PM'. */
export const when = (ms, tz) => formatWhen(ms, tz, api.now());
/** The day key ('2026-11-16') of an instant in the zone. */
export const dayOf = (ms, tz) => dayKey(ms, tz);
/** Today's day key in the zone, from the server's clock. */
export const today = (tz) => dayKey(api.now(), tz);
export { addDays };

/** 'Today', 'Yesterday' or 'Monday, November 9' (with the year when it is not this year). */
export function dayName(key, tz) {
  const t = today(tz);
  if (key === t) return 'Today';
  if (key === addDays(t, -1)) return 'Yesterday';
  return formatDayKey(key, { weekday: 'long', month: 'long', year: key.slice(0, 4) !== t.slice(0, 4) });
}
/** 'Monday, November 9, 2026'. */
export const dayLong = (key) => formatDayKey(key, { weekday: 'long', month: 'long', year: true });
/** 'Nov 9'. */
export const dayShort = (key) => formatDayKey(key, { weekday: false, month: 'short', year: false });

/** '8:10 – 8:24 AM' (the day period once when both share it), or '8:10 AM – now' while still going. */
export function timeRange(a, b, tz) {
  const pa = dateFormat(tz, { hour: 'numeric', minute: '2-digit' }).formatToParts(a);
  const ta = formatTime(a, tz);
  if (b == null) return `${ta} – now`;
  const tb = formatTime(b, tz);
  const pb = dateFormat(tz, { hour: 'numeric', minute: '2-digit' }).formatToParts(b);
  const ap = (ps) => (ps.find((p) => p.type === 'dayPeriod') || {}).value || '';
  if (ap(pa) && ap(pa) === ap(pb) && dayKey(a, tz) === dayKey(b, tz)) {
    const hm = pa.filter((p) => p.type !== 'dayPeriod').map((p) => p.value).join('').trim();
    return `${hm} – ${tb}`;
  }
  return dayKey(a, tz) === dayKey(b, tz) ? `${ta} – ${tb}` : `${ta} – ${formatDate(b, tz, { weekday: false, now: api.now() })}, ${tb}`;
}

/** The zone as people say it: 'Pacific Time'; falls back to the IANA name. */
export function zoneLabel(tz, ms = api.now()) {
  try {
    const p = dateFormat(tz, { timeZoneName: 'longGeneric' }).formatToParts(ms).find((x) => x.type === 'timeZoneName');
    if (p && p.value) return p.value;
  } catch { /* old browser */ }
  return String(tz).replace(/_/g, ' ');
}
/** The zone's short name now: 'PST'. */
export function zoneShort(tz, ms = api.now()) {
  const p = dateFormat(tz, { timeZoneName: 'short' }).formatToParts(ms).find((x) => x.type === 'timeZoneName');
  return p ? p.value : tz;
}
/** The hour of an instant in the zone (0 to 23). */
export const hourOf = (ms, tz) => zonedParts(ms, tz).hour;

/** A count with its word: count(3, 'drive') -> '3 drives'. */
export const count = (n, one, many = one + 's') => `${format(n, 'count')} ${n === 1 ? one : many}`;

/** The place of a drive, charge or timeline end: the place name, else its label, else the short address. */
export function placeText(p) {
  if (!p) return '—';
  if (p.place && p.place.name) return p.place.name;
  if (p.label) return p.label;
  if (typeof p.address === 'string') return p.address;
  if (p.address && p.address.short) return p.address.short;
  return '—';
}
/** The area under a place: 'Noe Valley, San Francisco' style, from whatever address parts there are. */
export function areaText(p) {
  if (!p) return '';
  const a = p.address;
  if (!a) return '';
  if (typeof a === 'string') return p.place && p.place.name ? a : '';
  const bits = [p.place && p.place.name ? a.short : null, a.locality, a.region].filter(Boolean);
  return [...new Set(bits)].join(', ');
}
