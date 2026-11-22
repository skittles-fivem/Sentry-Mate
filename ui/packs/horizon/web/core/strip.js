// The activity strip: what the car did over a span, as one band of coloured pieces (driving, charging,
// awake, vehicle bus only, asleep, updating, unknown), with hour or day marks under it and a key. Every
// piece has its words in a title and in the list a screen reader reads; a colour is never the only sign.

import { h, svg } from '../shared/dom.js';
import { formatTime, formatDate, formatDuration } from '../shared/time.js';
import { format } from '../shared/units.js';

export const KINDS = Object.freeze({
  driving: { word: 'Driving', tone: 'driving' },
  charging: { word: 'Charging', tone: 'charging' },
  online: { word: 'Parked, awake', tone: 'parked' },
  online_vehicle_bus_only: { word: 'Awake, vehicle bus only', tone: 'bus' },
  asleep: { word: 'Asleep', tone: 'asleep' },
  updating: { word: 'Updating', tone: 'updating' },
  unknown: { word: 'Unknown: no log', tone: 'unknown' },
});
const kindOf = (k) => KINDS[k] || KINDS.unknown;

/**
 * strip(items, from, to, { tz, now, cover, key: true }) where items are /activity pieces
 * ({ kind, start_ms, end_ms }). The time before the first piece and after the last (up to now) shows as
 * unknown. Returns the element.
 */
export function strip(items, from, to, { tz, now = Date.now(), cover, key = true, tall = false } = {}) {
  const end = Math.min(to ?? now, now);
  const span = Math.max(1, end - from);
  const pieces = [];
  let at = from;
  for (const it of items || []) {
    const s = Math.max(from, it.start_ms), e = Math.min(end, it.end_ms ?? end);
    if (e <= s) continue;
    if (s > at) pieces.push({ kind: 'unknown', start_ms: at, end_ms: s, filled: true });
    pieces.push({ ...it, start_ms: s, end_ms: e });
    at = Math.max(at, e);
  }
  if (at < end) pieces.push({ kind: 'unknown', start_ms: at, end_ms: end, filled: true });
  const long = span > 2 * 86400000;
  // marks: about six evenly spaced times
  const marks = [];
  for (let i = 0; i <= 6; i++) {
    const t = from + (span * i) / 6;
    marks.push(h('span', { class: 'strip-mark', vars: { '--x': `${((i / 6) * 100).toFixed(3)}%` }, dataset: { edge: i === 0 ? 'start' : i === 6 ? 'end' : null } },
      long ? formatDate(t, tz, { weekday: false, year: false }) : formatTime(t, tz)));
  }
  const used = [...new Set(pieces.map((p) => (KINDS[p.kind] ? p.kind : 'unknown')))];
  const total = {};
  for (const p of pieces) { const k = KINDS[p.kind] ? p.kind : 'unknown'; total[k] = (total[k] || 0) + (p.end_ms - p.start_ms); }
  return h('div', { class: ['strip', tall && 'strip--tall'], cover },
    h('div', { class: 'strip-band', role: 'img', 'aria-label': `What the car did from ${formatDate(from, tz, { year: false })}, ${formatTime(from, tz)} to ${formatTime(end, tz)}: ${used.map((k) => `${kindOf(k).word} ${formatDuration(total[k])}`).join(', ')}` },
      // one SVG, stretched to the band: thin pieces stay crisp and never land between device pixels
      svg('svg', { class: 'strip-svg', viewBox: '0 0 1000 10', preserveAspectRatio: 'none', 'shape-rendering': 'crispEdges', 'aria-hidden': 'true' },
        pieces.map((p) => svg('rect', {
          class: 'strip-rect', dataset: { tone: kindOf(p.kind).tone },
          x: (((p.start_ms - from) / span) * 1000).toFixed(3), y: 0, height: 10,
          width: Math.max(0.6, ((p.end_ms - p.start_ms) / span) * 1000).toFixed(3),
        }, svg('title', null, `${kindOf(p.kind).word}: ${formatTime(p.start_ms, tz)} to ${formatTime(p.end_ms, tz)} (${formatDuration(p.end_ms - p.start_ms)})`))))),
    h('div', { class: 'strip-marks', 'aria-hidden': 'true' }, marks),
    key ? h('ul', { class: 'strip-key' }, used.map((k) => h('li', { class: 'strip-key-item' },
      h('i', { class: 'strip-sw', dataset: { tone: kindOf(k).tone }, 'aria-hidden': 'true' }), h('span', null, kindOf(k).word),
      h('span', { class: 'strip-key-v' }, `${formatDuration(total[k])} · ${format((total[k] / span) * 100, 'percent')}`)))) : null);
}
