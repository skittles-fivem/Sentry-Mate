// Sentry Mate, "Logbook" UI pack: the journal parts, the activity strip and timeline rows.
//
// activityStrip() draws GET /api/v1/cars/{car}/activity segments as one ruled strip with a key: driving,
// charging, awake, awake with only the vehicle bus, asleep, unknown and updating, in fixed colours, with
// the time still to come outlined. Ticks follow the span (hours for a day, days for a week or a month).
// timelineRow() draws one row of GET /api/v1/cars/{car}/timeline as a journal entry on a rail, with links
// to the drive, the charge, the software page, the Trip for exactly its span and the place.

import { h, svg } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, dayKey, startOf, addPeriod, zonedParts, dateFormat } from '../shared/time.js';
import { formatList } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { sep } from './ui.js';
import { spanHref } from './period.js';
import { sourceMark, isImported } from './sources.js';

export const STRIP_NAMES = Object.freeze({
  driving: 'Driving', charging: 'Charging', online: 'Awake, parked', online_vehicle_bus_only: 'Awake, vehicle bus only',
  asleep: 'Asleep', unknown: 'Unknown (no data)', updating: 'Updating', future: 'Not yet',
});

/** Tick marks for a span: { at, label, major } in the zone. */
function ticks(from, to, tz) {
  const span = to - from;
  const out = [];
  if (span <= 36 * 3600e3) {
    const step = span <= 13 * 3600e3 ? 1 : 3;
    const dayFmt = dateFormat(tz, { weekday: 'short', day: 'numeric' });
    let t = startOf(from, 'hour', tz);
    while (t <= to) {
      const hr = zonedParts(t, tz).hour;
      if (t >= from && hr % step === 0) out.push({ at: t, label: hr === 0 ? dayFmt.format(t) : formatTime(t, tz).replace(':00', ''), major: hr % 6 === 0 });
      t = addPeriod(t, 'hour', 1, tz);
    }
  } else if (span <= 45 * 864e5) {
    const every = span <= 10 * 864e5 ? 1 : 7;
    let t = startOf(from, 'day', tz);
    const fmt = dateFormat(tz, every === 1 ? { weekday: 'short', day: 'numeric' } : { month: 'short', day: 'numeric' });
    while (t <= to) {
      const p = zonedParts(t, tz);
      if (t >= from && (every === 1 || p.weekday === 1)) out.push({ at: t, label: fmt.format(t), major: true });
      t = addPeriod(t, 'day', 1, tz);
    }
  } else {
    let t = startOf(from, 'month', tz);
    const fmt = dateFormat(tz, { month: 'short' });
    const fmtY = dateFormat(tz, { month: 'short', year: 'numeric' });
    while (t <= to) {
      if (t >= from) out.push({ at: t, label: zonedParts(t, tz).month === 1 ? fmtY.format(t) : fmt.format(t), major: true });
      t = addPeriod(t, 'month', 1, tz);
    }
  }
  return out;
}

/**
 * The strip: activityStrip({ items, from_ms, to_ms }, { tz, now, link(segment) -> href | null, cover }).
 * A segment's link (for example to its drive) makes it clickable; every segment has a <title> with its kind
 * and times. Returns a <figure>.
 */
export function activityStrip(act, { tz, now = api.now(), link = null, cover = null, label = 'What the car was doing, coloured by state.' } = {}) {
  const from = act.from_ms, to = Math.max(act.to_ms, from + 1);
  const span = to - from;
  const x = (t) => ((Math.max(from, Math.min(to, t)) - from) / span) * 100;
  const kinds = new Set();
  const segs = (act.items || []).map((s) => {
    const end = s.end_ms ?? now;
    kinds.add(s.kind);
    const r = svg('rect', { class: ['strip-seg', `strip-${s.kind}`], x: `${x(s.start_ms).toFixed(3)}%`, y: '18.5%', width: `${Math.max(0.1, x(end) - x(s.start_ms)).toFixed(3)}%`, height: '33.3%' },
      svg('title', null, `${STRIP_NAMES[s.kind] || s.kind}: ${formatDate(s.start_ms, tz, { now })}, ${formatTime(s.start_ms, tz)} to ${s.end_ms == null ? 'now' : formatTime(s.end_ms, tz)}`));
    const href = link ? link(s) : null;
    return href ? svg('a', { href }, r) : r;
  });
  const future = now < to - 60000;
  if (future) kinds.add('future');
  const tk = ticks(from, to, tz);
  const order = ['driving', 'charging', 'online', 'online_vehicle_bus_only', 'asleep', 'unknown', 'updating', 'future'];
  return h('figure', { class: 'strip', cover },
    svg('svg', { class: 'strip-svg', width: '100%', role: 'img', 'aria-label': label },
      segs,
      future ? svg('rect', { class: 'strip-future', x: `${x(now).toFixed(3)}%`, y: '19.4%', width: `${(100 - x(now)).toFixed(3)}%`, height: '31.5%' }) : null,
      tk.map((t, i) => [
        svg('line', { class: 'strip-tick', x1: `${x(t.at)}%`, x2: `${x(t.at)}%`, y1: '55.6%', y2: t.major ? '64.8%' : '61%' }),
        svg('text', { class: 'strip-hour', x: `${x(t.at)}%`, y: '90.7%', 'text-anchor': x(t.at) < 3 ? 'start' : x(t.at) > 97 ? 'end' : 'middle' }, i % Math.max(1, Math.ceil(tk.length / 12)) === 0 ? t.label : ''),
      ]),
      now > from && now < to ? svg('line', { class: 'strip-now', x1: `${x(now)}%`, x2: `${x(now)}%`, y1: '7.4%', y2: '59.3%' }) : null),
    h('figcaption', { class: 'strip-key' },
      order.filter((k) => kinds.has(k) || ['driving', 'charging', 'online', 'asleep'].includes(k)).map((k) =>
        h('span', { class: 'key' }, h('span', { class: ['key-swatch', `strip-${k}`], 'aria-hidden': 'true' }), STRIP_NAMES[k]))));
}

/* ------------------------------------------------------------------ timeline rows */

export const AWAKE_WORDS = Object.freeze({
  sentry: 'Sentry mode', keep_climate_on: 'keep-climate on', keep_climate_dog: 'Dog mode', keep_climate_camp: 'Camp mode',
  user_present: 'someone in the car', preconditioning_cabin: 'cabin preconditioning', preconditioning_battery: 'battery preconditioning',
  charging: 'charging', keep_warm: 'battery kept warm', door_open: 'a door open', accessory_power: 'accessory power on',
});
export const GAP_WORDS = Object.freeze({
  no_log: 'no log arrived for this time', not_recorded: 'recording was paused', failed_check: 'the log failed its check', dropped_frames: 'frames were dropped',
});

// a drive's start or end as words: its label, else its saved place or short address (a spot with no address known,
// out of the offline address list's reach, has none of them)
export const endLabel = (e) => (e && (e.label || (e.place && e.place.name) || (e.address && e.address.short))) || 'Unnamed spot';
/** How a fingerprint update got its version, when not typed or imported: "named from local TeslaMate". */
const NAMED_FROM = {
  build_match: 'named from an earlier update with the same fingerprints', teslafi_api: 'named from TeslaFi',
  teslamate_local: 'named from local TeslaMate', tesla_api: 'named from the Tesla API',
};
const placeOf = (p) => (p ? (p.place && p.place.name) || p.label || (typeof p.address === 'string' ? p.address : p.address && p.address.short) || 'an unnamed spot' : 'an unknown place');
const placeEl = (p) => h('span', { class: 'tl-place' }, placeOf(p));
const seg = (parts) => parts.filter(Boolean).flatMap((m, i) => { const s = h('span', { class: 'tl-seg' }, m); return i ? [sep(), s] : [s]; });

/** The link to create or edit the place at a timeline point (prefilled from its position). */
export function placeLink(p) {
  if (!p) return null;
  if (p.place && p.place.id) return state.buildUrl('places', { query: { place: p.place.id } });
  if (p.lat != null && p.lon != null) return state.buildUrl('places', { query: { new: `${p.lat.toFixed(5)},${p.lon.toFixed(5)}` } });
  return null;
}

/**
 * One journal entry: timelineRow(row, { car, tz, now, full }). full adds the end time, more figures and the
 * row's links (Trip for its span, the place). Returns an <li>.
 */
export function timelineRow(r, { car, tz, now = api.now(), full = false, period = null } = {}) {
  const going = r.end_ms == null;
  const end = r.end_ms ?? now;
  const len = r.duration_ms ?? end - r.start_ms;
  let title, meta = [], cls = r.kind, href = null;
  const range = full && r.range_end_km != null ? `${format(r.range_end_km, 'range')} range${Math.abs(r.range_delta_km || 0) >= 0.5 ? ` (${r.range_delta_km > 0 ? '+' : '−'}${format(Math.abs(r.range_delta_km), 'range')})` : ''}` : null;
  const odo = full && r.odometer_km != null ? `odometer ${format(r.odometer_km, 'odometer')}` : null;
  const soc = r.soc_end_pct != null ? `${format(r.soc_end_pct, 'percent')}${Math.abs(r.soc_delta_pct || 0) >= 0.5 ? ` (${r.soc_delta_pct > 0 ? '+' : '−'}${format(Math.abs(r.soc_delta_pct), 'percent')})` : ''}` : null;
  if (r.kind === 'drive') {
    if (r.ref && r.ref.drive && !going) href = state.buildUrl('drive', { car, drive: r.ref.drive, period });
    title = going ? ['Driving from ', placeEl(r.start)] : ['Drove from ', placeEl(r.start), ' to ', placeEl(r.end)];
    meta = [format(r.distance_km, 'distance') + (going ? ' so far' : ''), formatDuration(len), r.energy_delta_kwh != null ? `${format(Math.abs(r.energy_delta_kwh), 'energy')}${r.energy_basis === 'estimated' ? ' (estimated)' : ''}` : null, full ? soc : null, range, full && r.outside_avg_c != null ? `${format(r.outside_avg_c, 'temperature')} outside` : null, odo];
  } else if (r.kind === 'charge') {
    if (r.ref && r.ref.charge) href = state.buildUrl('charge', { car, charge: r.ref.charge, period });
    title = [going ? 'Charging at ' : 'Charged at ', placeEl(r.start)];
    meta = [soc, r.energy_delta_kwh != null ? `+${format(r.energy_delta_kwh, 'energy')}` : null, formatDuration(len), r.cost ? format(r.cost.amount, 'money', { currency: r.cost.currency }) : null, range, full && r.outside_avg_c != null ? `${format(r.outside_avg_c, 'temperature')} outside` : null];
  } else if (r.kind === 'parked') {
    const p = r.parked_ms || {};
    const asleep = p.asleep || 0;
    cls = asleep > len * 0.6 ? 'asleep' : 'parked';
    title = [going ? 'Parked at ' : 'Parked at ', placeEl(r.start), r.plugged ? ', plugged in' : ''];
    meta = [going ? `since ${formatTime(r.start_ms, tz)}` : formatDuration(len),
      asleep > 60000 ? `asleep ${formatDuration(asleep)} of it` : null,
      p.unknown > 60000 ? `${formatDuration(p.unknown)} unknown` : null,
      full && r.energy_delta_kwh ? `${format(Math.abs(r.energy_delta_kwh), 'energy')} used${r.energy_basis === 'estimated' ? ' (estimated)' : ''}` : null,
      full ? soc : null, range,
      full && r.outside_avg_c != null ? `${format(r.outside_avg_c, 'temperature')} average` : null];
  } else if (r.kind === 'gap') {
    title = ['No data', r.start ? [' near ', placeEl(r.start)] : ''];
    meta = [formatDuration(len), r.gap_reason ? GAP_WORDS[r.gap_reason] || r.gap_reason.replace(/_/g, ' ') : null, r.distance_km ? `the car moved ${format(r.distance_km, 'distance')}` : null];
  } else if (r.kind === 'update') {
    href = `${state.buildUrl('software', { car, period })}${r.ref && r.ref.update ? `#update-${r.ref.update}` : ''}`;
    const fp = r.detected_by === 'fingerprint';
    title = [fp ? 'Tesla update installed' : 'Software changed', r.version ? `: ${r.version}` : ''];
    meta = [fp ? (r.version ? (NAMED_FROM[r.version_source] || null) : 'version not named yet') : formatDuration(len),
      fp && r.changed_modules && r.changed_modules.length ? `${r.changed_modules.length} ${r.changed_modules.length === 1 ? 'module' : 'modules'} changed` : null];
  }
  const awake = r.kind === 'parked' && r.awake_reasons && r.awake_reasons.length
    ? h('p', { class: 'tl-why' }, 'Stayed awake for ', formatList(r.awake_reasons.map((x) => AWAKE_WORDS[x] || x.replace(/_/g, ' '))), '.') : null;
  const links = full ? h('p', { class: 'tl-links' },
    r.kind !== 'update' ? h('a', { href: spanHref('trip', { car, from: r.start_ms, to: Math.max(end, r.start_ms + 60000) }) }, 'Trip for this span') : null,
    placeLink(r.start) && r.kind !== 'gap' ? h('a', { href: placeLink(r.start) }, r.start.place ? `Edit ${r.start.place.name}` : 'Name this place') : null,
    r.kind === 'drive' && !going && placeLink(r.end) ? h('a', { href: placeLink(r.end) }, r.end.place ? `Edit ${r.end.place.name}` : 'Name the destination') : null,
    isImported(r) ? h('span', { class: 'flag flag-estimated' }, 'Imported') : null) : null;
  return h('li', { class: ['tl-item', `tl-${cls}`, { 'is-now': going }], dataset: { id: r.id } },
    h('div', { class: 'tl-time' },
      h('time', { datetime: new Date(r.start_ms).toISOString() }, formatTime(r.start_ms, tz)),
      full ? h('span', { class: 'tl-end' }, going ? 'now' : dayKey(r.end_ms, tz) === dayKey(r.start_ms, tz) ? formatTime(r.end_ms, tz) : `${formatDate(r.end_ms, tz, { weekday: false, now })}, ${formatTime(r.end_ms, tz)}`) : null),
    h('div', { class: 'tl-mark', 'aria-hidden': 'true' }),
    h('div', { class: 'tl-body' },
      h('p', { class: 'tl-title' }, href ? h('a', { href }, title) : title),
      h('p', { class: 'tl-meta' }, seg(meta)),
      awake, sourceMark(r, { block: true, short: !full, cls: 'tl-src' }), links));
}
