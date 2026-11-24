// Sentry Mate shared module: time.js (time zones, formatting, buckets, period presets).
// Canonical source: ui/shared/time.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// The API sends UTC milliseconds; people read them in the car's own time zone. Every function here takes
// that zone (an IANA name such as 'America/Chicago') and works through daylight saving: a day is the
// wall-clock day in that zone, so it can be 23 or 25 hours long. Weeks are ISO weeks (Monday first,
// week 1 holds the year's first Thursday). Period presets resolve against a "now" you pass in (use
// api.now(), the server's clock), never against the browser's clock directly.

import { getLocale, t, tn, formatNumber } from './i18n.js';

export const SECOND = 1000;
export const MINUTE = 60 * SECOND;
export const HOUR = 60 * MINUTE;
export const DAY = 24 * HOUR;
const NBSP = '\u00a0';
const DASH = '\u2014';
const pad2 = (n) => String(n).padStart(2, '0');

/* ------------------------------------------------------------------ zones and wall-clock parts */

const zoneOk = new Map();
const warned = new Set();
/** Is this an IANA time zone the browser knows? */
export function isValidZone(tz) {
  if (typeof tz !== 'string' || !tz) return false;
  let ok = zoneOk.get(tz);
  if (ok === undefined) {
    try { new Intl.DateTimeFormat('en-US', { timeZone: tz }); ok = true; } catch { ok = false; }
    zoneOk.set(tz, ok);
  }
  return ok;
}
// An unknown zone falls back to UTC (with one console warning) rather than breaking the page.
function zone(tz) {
  if (isValidZone(tz)) return tz;
  if (!warned.has(String(tz))) { warned.add(String(tz)); console.warn(`time.js: unknown time zone ${JSON.stringify(tz)}, using UTC`); }
  return 'UTC';
}

const partsFormats = new Map();
function partsFormat(tz) {
  let f = partsFormats.get(tz);
  if (!f) {
    f = new Intl.DateTimeFormat('en-US', {
      timeZone: tz, hourCycle: 'h23', year: 'numeric', month: 'numeric', day: 'numeric',
      hour: 'numeric', minute: 'numeric', second: 'numeric',
    });
    partsFormats.set(tz, f);
  }
  return f;
}

// Zone offsets only change on a quarter hour (UTC), so one lookup per quarter hour is enough.
const QUARTER = 15 * MINUTE;
const offsets = new Map();
/** The zone's offset from UTC at that instant, in ms (Chicago in winter: -21600000). */
export function zoneOffset(ms, tz) {
  tz = zone(tz);
  const q = Math.floor(ms / QUARTER);
  const key = tz + '|' + q;
  let o = offsets.get(key);
  if (o === undefined) {
    const at = q * QUARTER;
    const p = {};
    for (const x of partsFormat(tz).formatToParts(at)) p[x.type] = x.value;
    o = Date.UTC(+p.year, +p.month - 1, +p.day, +p.hour % 24, +p.minute, +p.second) - at;
    if (offsets.size > 50000) offsets.clear();
    offsets.set(key, o);
  }
  return o;
}

/**
 * The wall clock in the zone: { year, month (1-12), day, hour, minute, second, millisecond,
 * weekday (1 = Monday ... 7 = Sunday), offset_ms }.
 */
export function zonedParts(ms, tz) {
  const o = zoneOffset(ms, tz);
  const d = new Date(ms + o);
  return {
    year: d.getUTCFullYear(), month: d.getUTCMonth() + 1, day: d.getUTCDate(),
    hour: d.getUTCHours(), minute: d.getUTCMinutes(), second: d.getUTCSeconds(), millisecond: d.getUTCMilliseconds(),
    weekday: ((d.getUTCDay() + 6) % 7) + 1, offset_ms: o,
  };
}

/**
 * Wall-clock time in the zone -> UTC ms. A time that happens twice (clocks go back) gives the first
 * one ('compatible' and 'earlier') or the second ('later'). A time that never happens (clocks go
 * forward) moves forward by the size of the jump ('compatible', 'later': 2:30 -> 3:30) or uses the
 * time just before it ('earlier': 2:30 -> 1:30).
 */
export function zonedToMs({ year, month = 1, day = 1, hour = 0, minute = 0, second = 0, millisecond = 0 }, tz, { disambiguation = 'compatible' } = {}) {
  const wall = Date.UTC(year, month - 1, day, hour, minute, second, millisecond);
  const before = zoneOffset(wall - DAY, tz), after = zoneOffset(wall + DAY, tz);
  const a = wall - before, b = wall - after;
  const okA = a + zoneOffset(a, tz) === wall, okB = b + zoneOffset(b, tz) === wall;
  if (okA && okB) return a === b ? a : disambiguation === 'later' ? Math.max(a, b) : Math.min(a, b);
  if (okA) return a;
  if (okB) return b;
  return disambiguation === 'earlier' ? wall - after : wall - before;
}

/* ------------------------------------------------------------------ calendar days and ISO weeks */

/** 'YYYY-MM-DD' for the day the instant falls on in the zone. */
export function dayKey(ms, tz) {
  const p = zonedParts(ms, tz);
  return `${p.year}-${pad2(p.month)}-${pad2(p.day)}`;
}
function keyParts(key) {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(String(key));
  if (!m) throw new RangeError(`not a day key: ${key}`);
  return { year: +m[1], month: +m[2], day: +m[3] };
}
/** 'YYYY-MM-DD' plus n calendar days. */
export function addDays(key, n) {
  const { year, month, day } = keyParts(key);
  return new Date(Date.UTC(year, month - 1, day + n)).toISOString().slice(0, 10);
}
/** The first instant of a day (normally midnight) in the zone, as UTC ms. */
export function dayStart(key, tz) {
  return zonedToMs(keyParts(key), tz);
}
/** Whole calendar days from key a to key b (b - a). */
export function daysBetween(a, b) {
  const pa = keyParts(a), pb = keyParts(b);
  return Math.round((Date.UTC(pb.year, pb.month - 1, pb.day) - Date.UTC(pa.year, pa.month - 1, pa.day)) / DAY);
}

/** The ISO week of a calendar date: { year, week }. The ISO year can differ from the calendar year. */
export function isoWeek(year, month, day) {
  const d = Date.UTC(year, month - 1, day);
  const wd = (new Date(d).getUTCDay() + 6) % 7; // Monday 0
  const thursday = d + (3 - wd) * DAY;
  const y = new Date(thursday).getUTCFullYear();
  return { year: y, week: 1 + Math.floor((thursday - Date.UTC(y, 0, 1)) / (7 * DAY)) };
}
/** The ISO week the instant falls in, in the zone. */
export function isoWeekOf(ms, tz) {
  const p = zonedParts(ms, tz);
  return isoWeek(p.year, p.month, p.day);
}
/** How many ISO weeks the ISO year has (52 or 53). */
export const isoWeeksInYear = (year) => isoWeek(year, 12, 28).week;

/* ------------------------------------------------------------------ periods of the calendar */

export const UNITS = ['hour', 'day', 'week', 'month', 'year'];
function checkUnit(unit) {
  if (!UNITS.includes(unit)) throw new RangeError(`unknown unit: ${unit}`);
}
const daysInMonth = (y, m) => new Date(Date.UTC(y, m, 0)).getUTCDate();

// The start of the wall-clock hour the instant is in. Where the clocks change by a part of an hour the hour
// they change in is shorter (it starts at the change) or longer (it started before the change).
function hourStart(ms, tz) {
  const p = zonedParts(ms, tz);
  const s = ms - (p.minute * MINUTE + p.second * SECOND + p.millisecond);
  if (zoneOffset(s, tz) === p.offset_ms) return s;
  let change = Math.floor(ms / QUARTER) * QUARTER; // the clocks changed after s: find when
  while (change > s && zoneOffset(change - QUARTER, tz) === p.offset_ms) change -= QUARTER;
  const b = zonedParts(change - 1, tz);
  if (b.hour !== p.hour || b.day !== p.day) return change;
  return change - 1 - (b.minute * MINUTE + b.second * SECOND + b.millisecond);
}
// The end of the wall-clock hour that starts at `start` (the start of the next one).
function hourEnd(start, tz) {
  for (let k = 1; k <= 2; k++) {
    const e = hourStart(start + k * HOUR, tz);
    if (e > start) return e;
  }
  return start + HOUR;
}
// Does the wall-clock hour the instant is in come twice (the clocks go back by a whole hour)?
function hourTwice(ms, p, tz) {
  if (zoneOffset(ms - 2 * HOUR, tz) === zoneOffset(ms + 2 * HOUR, tz)) return false;
  const start = hourStart(ms, tz);
  const same = (q) => q.hour === p.hour && q.day === p.day;
  return same(zonedParts(start - 1, tz)) || same(zonedParts(hourEnd(start, tz), tz));
}

/** The start of the hour, day, ISO week, month or year the instant is in, in the zone. */
export function startOf(ms, unit, tz) {
  checkUnit(unit);
  if (unit === 'hour') return hourStart(ms, tz);
  const p = zonedParts(ms, tz);
  if (unit === 'day') return zonedToMs({ year: p.year, month: p.month, day: p.day }, tz);
  if (unit === 'week') {
    const k = addDays(`${p.year}-${pad2(p.month)}-${pad2(p.day)}`, -(p.weekday - 1));
    return dayStart(k, tz);
  }
  if (unit === 'month') return zonedToMs({ year: p.year, month: p.month, day: 1 }, tz);
  return zonedToMs({ year: p.year, month: 1, day: 1 }, tz);
}

/**
 * n calendar units later (or earlier, n < 0) on the zone's wall clock: one day after 00:00 is the next
 * 00:00 even across a clock change. A month later from the 31st ends on the month's last day.
 */
export function addPeriod(ms, unit, n, tz) {
  checkUnit(unit);
  if (unit === 'hour') return ms + n * HOUR;
  const p = zonedParts(ms, tz);
  let { year, month, day } = p;
  if (unit === 'day' || unit === 'week') {
    const k = addDays(`${year}-${pad2(month)}-${pad2(day)}`, unit === 'week' ? 7 * n : n);
    ({ year, month, day } = keyParts(k));
  } else {
    const total = year * 12 + (month - 1) + (unit === 'year' ? 12 * n : n);
    year = Math.floor(total / 12);
    month = (total % 12) + 1;
    day = Math.min(day, daysInMonth(year, month));
  }
  return zonedToMs({ year, month, day, hour: p.hour, minute: p.minute, second: p.second, millisecond: p.millisecond }, tz);
}

/** The end of the unit the instant is in (the start of the next one, exclusive). */
export const endOf = (ms, unit, tz) => (unit === 'hour' ? hourEnd(hourStart(ms, tz), tz) : addPeriod(startOf(ms, unit, tz), unit, 1, tz));

/**
 * A stable key for the bucket: '2026-11-16T17', '2026-11-16', '2026-W47', '2026-11', '2026'. An hour that
 * comes twice when the clocks go back has its offset in the key: '2026-11-01T01-05:00', '2026-11-01T01-06:00'.
 */
export function bucketKey(ms, unit, tz) {
  checkUnit(unit);
  const p = zonedParts(ms, tz);
  if (unit === 'hour') {
    const key = `${p.year}-${pad2(p.month)}-${pad2(p.day)}T${pad2(p.hour)}`;
    if (!hourTwice(ms, p, tz)) return key;
    const oa = Math.abs(Math.round(p.offset_ms / MINUTE));
    return `${key}${p.offset_ms < 0 ? '-' : '+'}${pad2(Math.floor(oa / 60))}:${pad2(oa % 60)}`;
  }
  if (unit === 'day') return `${p.year}-${pad2(p.month)}-${pad2(p.day)}`;
  if (unit === 'week') { const w = isoWeek(p.year, p.month, p.day); return `${w.year}-W${pad2(w.week)}`; }
  if (unit === 'month') return `${p.year}-${pad2(p.month)}`;
  return String(p.year);
}

/**
 * The calendar buckets that cover [from_ms, to_ms): [{ key, start_ms, end_ms }]. The first starts at the
 * start of the unit holding from_ms; bounds are the zone's own (a day can be 23 or 25 hours, and an hour 30
 * or 90 minutes where the clocks change by half an hour).
 */
export function buckets(fromMs, toMs, unit, tz, { max = 100000 } = {}) {
  checkUnit(unit);
  const out = [];
  if (!(toMs > fromMs)) return out;
  let start = startOf(fromMs, unit, tz);
  while (start < toMs) {
    const end = unit === 'hour' ? hourEnd(start, tz) : addPeriod(start, unit, 1, tz);
    out.push({ key: bucketKey(start, unit, tz), start_ms: start, end_ms: end });
    if (out.length >= max) throw new RangeError(`more than ${max} buckets`);
    start = end;
  }
  return out;
}

/** Group items by bucket: Map key -> items, in the order the items came. at(item) gives its ms. */
export function groupByBucket(items, at, unit, tz) {
  const m = new Map();
  for (const it of items) {
    const ms = at(it);
    if (ms == null) continue;
    const k = bucketKey(ms, unit, tz);
    let list = m.get(k);
    if (!list) { list = []; m.set(k, list); }
    list.push(it);
  }
  return m;
}

/* ------------------------------------------------------------------ formatting */

const dateFormats = new Map();
/** A cached Intl.DateTimeFormat for the current locale and the zone. */
export function dateFormat(tz, opts) {
  tz = zone(tz);
  const key = getLocale() + '|' + tz + '|' + JSON.stringify(opts);
  let f = dateFormats.get(key);
  if (!f) {
    f = new Intl.DateTimeFormat(getLocale(), { timeZone: tz, ...opts });
    if (dateFormats.size > 500) dateFormats.clear();
    dateFormats.set(key, f);
  }
  return f;
}

const sameYear = (ms, now, tz) => now != null && zonedParts(ms, tz).year === zonedParts(now, tz).year;
function dateOpts({ weekday = true, month = 'short', year = 'auto', ms, now, tz }) {
  const o = { month, day: 'numeric' };
  if (weekday) o.weekday = weekday === true ? 'short' : weekday;
  if (year === true || (year === 'auto' && !sameYear(ms, now, tz))) o.year = 'numeric';
  return o;
}

/**
 * The date: 'Mon, Nov 16' (the year is shown when it is not the year of opts.now, or always when no now
 * is given). opts: weekday (true | false | 'long'), month ('short' | 'long' | 'numeric'), year (true |
 * false | 'auto'), now.
 */
export function formatDate(ms, tz, { weekday = true, month = 'short', year = 'auto', now = null } = {}) {
  if (ms == null) return DASH;
  return dateFormat(tz, dateOpts({ weekday, month, year, ms, now, tz })).format(ms);
}

/** The time of day: '5:30 PM' or '17:30', as the locale writes it. opts.seconds adds seconds. */
export function formatTime(ms, tz, { seconds = false } = {}) {
  if (ms == null) return DASH;
  return dateFormat(tz, seconds ? { hour: 'numeric', minute: '2-digit', second: '2-digit' } : { hour: 'numeric', minute: '2-digit' }).format(ms);
}

/** Date and time: 'Mon, Nov 16, 5:30 PM'. Same options as formatDate, plus seconds. */
export function formatDateTime(ms, tz, { weekday = true, month = 'short', year = 'auto', now = null, seconds = false } = {}) {
  if (ms == null) return DASH;
  const o = dateOpts({ weekday, month, year, ms, now, tz });
  o.hour = 'numeric';
  o.minute = '2-digit';
  if (seconds) o.second = '2-digit';
  return dateFormat(tz, o).format(ms);
}

/** 'Today, 5:30 PM', 'Yesterday, 9:12 AM', 'Tomorrow, 7:00 AM' or 'Mon, Nov 9, 5:30 PM'. */
export function formatWhen(ms, tz, now, { seconds = false } = {}) {
  if (ms == null) return DASH;
  const k = dayKey(ms, tz), today = dayKey(now, tz);
  const time = formatTime(ms, tz, { seconds });
  if (k === today) return t('Today, {time}', { time });
  if (k === addDays(today, -1)) return t('Yesterday, {time}', { time });
  if (k === addDays(today, 1)) return t('Tomorrow, {time}', { time });
  return formatDateTime(ms, tz, { now, seconds });
}

/** A day key ('2026-11-16') as a date: 'Mon, Nov 16' (same options as formatDate). */
export function formatDayKey(key, { weekday = true, month = 'short', year = true } = {}) {
  const { year: y, month: m, day } = keyParts(key);
  const o = { month, day: 'numeric' };
  if (weekday) o.weekday = weekday === true ? 'short' : weekday;
  if (year) o.year = 'numeric';
  return dateFormat('UTC', o).format(Date.UTC(y, m - 1, day, 12));
}

/**
 * A span of dates: 'Nov 1 – 16, 2026'. Both ends are inclusive instants (pass to_ms - 1 for an
 * exclusive end). With opts.time the times are shown too.
 */
export function formatDateRange(fromMs, toMs, tz, { time = false, weekday = false, year = true } = {}) {
  if (fromMs == null || toMs == null) return DASH;
  if (toMs < fromMs) [fromMs, toMs] = [toMs, fromMs];
  const o = { month: 'short', day: 'numeric' };
  if (year) o.year = 'numeric';
  if (weekday) o.weekday = 'short';
  if (time) { o.hour = 'numeric'; o.minute = '2-digit'; }
  const f = dateFormat(tz, o);
  return typeof f.formatRange === 'function' ? f.formatRange(fromMs, toMs) : `${f.format(fromMs)} \u2013 ${f.format(toMs)}`;
}

/**
 * Where a car's zone comes from (the `time_zone_source` of /cars), in words: 'set for this car' (owner), 'from the
 * car's position' (position: worked out on the server, offline), "the server's default" (default: no zone set and
 * no position known yet). Null for anything else.
 */
export function zoneSourceText(source) {
  switch (source) {
    case 'owner': return t('set for this car');
    case 'position': return t('from the car’s position');
    case 'default': return t('the server’s default');
    default: return null;
  }
}

/** The zone's short name at that instant ('CST', 'GMT+1'). */
export function zoneName(ms, tz, { style = 'short' } = {}) {
  const p = dateFormat(tz, { timeZoneName: style }).formatToParts(ms).find((x) => x.type === 'timeZoneName');
  return p ? p.value : zone(tz);
}

/** ISO 8601 with the zone's offset, for files: '2026-11-16T11:30:00-06:00'. */
export function toIsoZoned(ms, tz) {
  if (ms == null) return '';
  const p = zonedParts(ms, tz);
  const o = Math.round(p.offset_ms / MINUTE);
  const sign = o < 0 ? '-' : '+';
  const oa = Math.abs(o);
  const frac = p.millisecond ? '.' + String(p.millisecond).padStart(3, '0') : '';
  return `${p.year}-${pad2(p.month)}-${pad2(p.day)}T${pad2(p.hour)}:${pad2(p.minute)}:${pad2(p.second)}${frac}${sign}${pad2(Math.floor(oa / 60))}:${pad2(oa % 60)}`;
}

/**
 * How long ago (or how long until): 'just now', '5 min ago', '2 h 5 min ago', '3 h ago', '4 days ago',
 * 'in 10 min'. Minutes up to an hour, hours and minutes up to 6 hours, hours up to 2 days, then days.
 */
export function formatRelative(ms, now) {
  if (ms == null || now == null) return DASH;
  const diff = now - ms;
  const s = Math.abs(diff) / 1000;
  const future = diff < 0;
  if (s < 45) return t('just now');
  let text;
  if (s < 3600) {
    const m = Math.max(1, Math.round(s / 60));
    text = m === 60 ? t('{h} h', { h: 1 }) : t('{m} min', { m });
  } else if (s < 6 * 3600) {
    const total = Math.round(s / 60);
    const h = Math.floor(total / 60), m = total % 60;
    text = m ? t('{h} h {m} min', { h, m }) : t('{h} h', { h });
  } else if (s < 2 * 86400) {
    text = t('{h} h', { h: Math.round(s / 3600) });
  } else {
    text = tn(Math.round(s / 86400), '{n} day', '{n} days');
  }
  text = text.replace(/ (h|min|day|days)\b/g, NBSP + '$1');
  return future ? t('in {text}', { text }) : t('{text} ago', { text });
}

/**
 * A length of time in ms: style 'short' (default) '45 s', '12 min', '2 h 05 min', '3 d 4 h';
 * 'clock' '2:05:13'; 'long' '2 hours 5 minutes'. opts.seconds: true shows seconds under an hour
 * ('12 min 05 s'), 'auto' (default) only under a minute. null -> an em dash.
 */
export function formatDuration(ms, { style = 'short', seconds = 'auto' } = {}) {
  if (ms == null || !Number.isFinite(ms)) return DASH;
  const sign = ms < 0 ? '\u2212' : '';
  let s = Math.round(Math.abs(ms) / 1000);
  if (style === 'clock') {
    const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), sec = s % 60;
    return sign + (h ? `${h}:${pad2(m)}:${pad2(sec)}` : `${m}:${pad2(sec)}`);
  }
  const long = style === 'long';
  // one number with its unit; the second part of '2 h 05 min' is padded to two digits
  const word = (n, short, one, other, padded = false) => (long
    ? tn(n, `{n} ${one}`, `{n} ${other}`)
    : `${formatNumber(n, padded ? { minimumIntegerDigits: 2 } : {})}${NBSP}${t(short)}`);
  let out;
  if (s < 60) {
    out = word(s, 's', 'second', 'seconds');
  } else if (s < 3600 && seconds === true) {
    out = `${word(Math.floor(s / 60), 'min', 'minute', 'minutes')} ${word(s % 60, 's', 'second', 'seconds', true)}`;
  } else {
    const mins = Math.round(s / 60);
    if (mins < 60) out = word(mins, 'min', 'minute', 'minutes');
    else if (mins < 24 * 60) {
      const h = Math.floor(mins / 60), m = mins % 60;
      out = word(h, 'h', 'hour', 'hours') + (m ? ' ' + word(m, 'min', 'minute', 'minutes', true) : '');
    } else {
      const hours = Math.round(mins / 60);
      const d = Math.floor(hours / 24), h = hours % 24;
      out = word(d, 'd', 'day', 'days') + (h ? ' ' + word(h, 'h', 'hour', 'hours') : '');
    }
  }
  return sign + out;
}

/* ------------------------------------------------------------------ period presets */

/** The shared period presets, in menu order. */
export const PRESETS = ['24h', '7d', '30d', 'year', 'all'];
export const PRESET_LABELS = { '24h': 'Last 24 hours', '7d': 'Last 7 days', '30d': 'Last 30 days', year: 'This year', all: 'All time', custom: 'Custom' };
export const presetLabel = (p) => t(PRESET_LABELS[p] || PRESET_LABELS.custom);

const isMs = (v) => typeof v === 'number' && Number.isFinite(v);
/** The last time a period can name: 1 January 2100 UTC. The API takes `from_ms` and `to_ms` from 0 up to it. */
export const PERIOD_MAX_MS = 4102444800000;
const inPeriodRange = (v) => isMs(v) && v >= 0 && v <= PERIOD_MAX_MS;
/**
 * A period in any of its shapes -> { preset } or { from, to } (ms; to may be null for "until now"),
 * or null when it is not a period. Accepts '7d', { preset: '7d' }, { from, to }, { from_ms, to_ms }.
 */
export function normalizePeriod(p) {
  if (p == null) return null;
  if (typeof p === 'string') return PRESETS.includes(p) ? { preset: p } : null;
  if (typeof p !== 'object') return null;
  if (p.preset && p.preset !== 'custom') return PRESETS.includes(p.preset) ? { preset: p.preset } : null;
  let from = p.from ?? p.from_ms ?? null, to = p.to ?? p.to_ms ?? null;
  // a time before 1970 or after 2100 (a year mistyped as 0206) is no period: the API answers it with an error
  if (from != null && !inPeriodRange(from)) return null;
  if (to != null && !inPeriodRange(to)) return null;
  if (from == null && to == null) return null;
  if (from != null && to != null && from > to) [from, to] = [to, from];
  return { from, to };
}

/**
 * Resolve a period against now in the car's zone -> { preset, from_ms, to_ms, label }.
 *   24h   the last 24 hours, to now
 *   7d    today and the 6 days before (from the start of that day, in the zone), to now
 *   30d   today and the 29 days before, to now
 *   year  from 1 January this year, to now
 *   all   from first_ms (the oldest data, when known; otherwise null = no lower limit), to now
 *   custom { from, to }; to null means now
 * A period that is not valid gives the fallback preset (default '30d').
 */
export function resolvePeriod(period, { now, tz, first_ms = null, fallback = '30d' } = {}) {
  if (!isMs(now)) throw new TypeError('resolvePeriod needs now (ms)');
  const p = normalizePeriod(period) || { preset: fallback };
  let from, to = now;
  const preset = p.preset || 'custom';
  switch (preset) {
    case '24h': from = now - DAY; break;
    case '7d': from = addPeriod(startOf(now, 'day', tz), 'day', -6, tz); break;
    case '30d': from = addPeriod(startOf(now, 'day', tz), 'day', -29, tz); break;
    case 'year': from = startOf(now, 'year', tz); break;
    case 'all': from = isMs(first_ms) ? Math.min(first_ms, now) : null; break;
    default: from = p.from ?? null; to = p.to ?? now;
  }
  const r = { preset, from_ms: from, to_ms: to };
  r.label = periodLabel(r, tz, now);
  return r;
}

/** The API query for a resolved period: { from_ms, to_ms }, leaving out a missing lower limit. */
export function periodParams(r) {
  const out = {};
  if (r && isMs(r.from_ms)) out.from_ms = r.from_ms;
  if (r && isMs(r.to_ms)) out.to_ms = r.to_ms;
  return out;
}

const isDayStart = (ms, tz) => isMs(ms) && startOf(ms, 'day', tz) === ms;

/** 'Last 7 days', 'This year', or for a custom period its dates ('Nov 1 – 16, 2026') or date-times. */
export function periodLabel(r, tz, now = null) {
  if (!r) return '';
  if (r.preset && r.preset !== 'custom') return presetLabel(r.preset);
  const from = r.from_ms, to = r.to_ms ?? now;
  if (from == null) return t('Until {date}', { date: formatDate(to, tz, { weekday: false, year: true }) });
  if (isDayStart(from, tz) && isDayStart(to, tz)) return formatDateRange(from, to - 1, tz);
  return formatDateRange(from, to, tz, { time: true });
}

/**
 * The period n lengths later (n = 1) or earlier (n = -1), as a custom period. Periods that start at the
 * start of a day move by whole calendar days, so they stay aligned across clock changes; 'year' moves to
 * the whole year before or after. 'all' and periods with no lower limit do not move (null).
 */
export function shiftPeriod(r, n, { tz, now = null } = {}) {
  if (!r || !isMs(r.from_ms) || !n) return null;
  const to = r.to_ms ?? now;
  if (!isMs(to)) return null;
  let from2, to2;
  if (r.preset === 'year') {
    from2 = addPeriod(r.from_ms, 'year', n, tz);
    to2 = addPeriod(from2, 'year', 1, tz);
  } else if (isDayStart(r.from_ms, tz)) {
    const k0 = dayKey(r.from_ms, tz);
    const endKey = isDayStart(to, tz) ? dayKey(to, tz) : addDays(dayKey(to, tz), 1);
    const len = Math.max(1, daysBetween(k0, endKey));
    const k1 = addDays(k0, n * len);
    from2 = dayStart(k1, tz);
    to2 = dayStart(addDays(k1, len), tz);
  } else {
    const len = to - r.from_ms;
    from2 = r.from_ms + n * len;
    to2 = to + n * len;
  }
  const out = { preset: 'custom', from_ms: from2, to_ms: to2 };
  out.label = periodLabel(out, tz, now);
  return out;
}

/** A custom period from whole days: fromKey 00:00 up to the end of toKey (exclusive), in the zone. */
export function dayRange(fromKey, toKey, tz) {
  if (fromKey > toKey) [fromKey, toKey] = [toKey, fromKey];
  return { from: dayStart(fromKey, tz), to: dayStart(addDays(toKey, 1), tz) };
}

/** The first and last day of a resolved period, for date inputs: { from: 'YYYY-MM-DD', to }. */
export function periodDays(r, tz) {
  return {
    from: isMs(r?.from_ms) ? dayKey(r.from_ms, tz) : null,
    to: isMs(r?.to_ms) ? dayKey(Math.max(r.from_ms ?? -Infinity, r.to_ms - 1), tz) : null,
  };
}
