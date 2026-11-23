// The activity strip: what the car did over a span, in fixed colours with a legend (GET
// /api/v1/cars/{car}/activity). Asleep is hatched, unknown (no log, a paused recording, a failed check)
// is grey and hatched the other way, so the two never look alike. Positions are CSS custom properties
// set through CSSOM. Used by Car overview, Timeline and Trip.

import { h, setVars } from '../shared/dom.js';
import { formatTime, formatDate, formatDuration, buckets, zonedParts, HOUR, DAY } from '../shared/time.js';

/** API kind -> the state word used for colours, and its label. */
export const KINDS = Object.freeze({
  driving: { state: 'driving', label: 'Driving' },
  charging: { state: 'charging', label: 'Charging' },
  online: { state: 'parked', label: 'Parked, awake' },
  online_vehicle_bus_only: { state: 'vbus', label: 'Awake, vehicle bus only' },
  asleep: { state: 'asleep', label: 'Asleep' },
  updating: { state: 'updating', label: 'Updating' },
  unknown: { state: 'unknown', label: 'Unknown (no data)' },
});

const pct = (v, a, b) => Math.max(0, Math.min(100, ((v - a) / Math.max(1, b - a)) * 100));

/** Axis labels for a span: hours within a day or two, dates for longer spans. */
export function axisTicks(from, to, tz, n = 5) {
  const span = to - from;
  const out = [];
  if (span <= 2 * DAY) {
    const step = span <= 13 * HOUR ? 3 : span <= 26 * HOUR ? 6 : 12;
    for (const b of buckets(from, to, 'hour', tz)) {
      if (b.start_ms < from) continue;
      if (zonedParts(b.start_ms, tz).hour % step === 0) out.push({ x: pct(b.start_ms, from, to), text: formatTime(b.start_ms, tz) });
    }
  } else {
    const days = buckets(from, to, 'day', tz).filter((b) => b.start_ms >= from);
    const every = Math.max(1, Math.ceil(days.length / n));
    days.forEach((b, i) => { if (i % every === 0) out.push({ x: pct(b.start_ms, from, to), text: formatDate(b.start_ms, tz, { weekday: false, year: false }) }); });
  }
  return out;
}

/**
 * The strip: activityStrip(items, from_ms, to_ms, { tz, now, size: 'lg' | 'sm', axis: true, legend: true }).
 * items: the /activity segments. Time after now is left empty (not yet happened).
 */
export function activityStrip(items, from, to, { tz, now = null, size = 'lg', axis = true, legend = true, kinds = null } = {}) {
  const lg = size === 'lg';
  const end = now != null ? Math.min(to, now) : to;
  const segs = items.filter((s) => (s.end_ms ?? end) > from && s.start_ms < end).map((s) => {
    const k = KINDS[s.kind] || KINDS.unknown;
    const a = Math.max(from, s.start_ms), b = Math.min(end, s.end_ms ?? end);
    const el = h('i', { dataset: { state: k.state }, title: `${k.label}: ${formatTime(a, tz)} – ${formatTime(b, tz)} (${formatDuration(b - a)})` });
    setVars(el, { '--x': `${pct(a, from, to).toFixed(3)}%`, '--w': `${Math.max(lg ? 0.15 : 0.3, pct(b, from, to) - pct(a, from, to)).toFixed(3)}%` });
    if (lg) el.classList.add('tl-seg');
    return el;
  });
  const used = new Set(segs.map((s) => s.dataset.state));
  const band = h('div', { class: lg ? 'tl-band' : 'daystrip', role: 'img', 'aria-label': `What the car did from ${formatDate(from, tz)} ${formatTime(from, tz)} to ${formatDate(end, tz)} ${formatTime(end, tz)}` }, segs);
  const wrap = h('div', { class: lg ? 'strip-band-wrap' : 'daystrip-wrap' }, band);
  if (now != null && now > from && now < to) {
    const mark = h('i', { class: lg ? 'tl-now' : 'daystrip-now', title: 'Now' });
    setVars(mark, { '--x': `${pct(now, from, to).toFixed(3)}%` });
    wrap.append(mark);
  }
  const ticks = axis ? axisTicks(from, to, tz) : [];
  const axisEl = axis ? h('div', { class: lg ? 'tl-axis' : 'tl-axis strip-axis--sm', 'aria-hidden': 'true' }, ticks.map((t) => {
    const s = h('span', null, t.text);
    setVars(s, { '--x': `${t.x.toFixed(2)}%` });
    return s;
  })) : null;
  const show = (kinds || Object.keys(KINDS)).map((k) => KINDS[k]).filter((k) => !kinds ? used.has(k.state) || ['driving', 'charging', 'parked', 'asleep', 'unknown'].includes(k.state) : true);
  const legendEl = legend ? h('div', { class: lg ? 'tl-legend' : 'mini-legend' }, show.map((k) => h('span', null, h('i', { dataset: { state: k.state } }), k.label))) : null;
  return h('div', { class: ['strip', `strip--${size}`] }, wrap, axisEl, legendEl);
}
