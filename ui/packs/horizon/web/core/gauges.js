// Cluster parts for car pages: a speedometer-style dial, a fuel-style gauge, odometer counters and warning
// lights. Values come in API units; the dial's readout shows the person's units (shared/units.js).

import { h, svg, quantity } from '../shared/dom.js';
import { icon } from './ui.js';

const ARC = 240; // degrees the dial sweeps, from lower left to lower right

function point(cx, cy, r, deg) {
  const a = ((deg - 90) * Math.PI) / 180;
  return [cx + r * Math.cos(a), cy + r * Math.sin(a)];
}
function arcPath(cx, cy, r, from, to) {
  const [x0, y0] = point(cx, cy, r, from), [x1, y1] = point(cx, cy, r, to);
  return `M${x0.toFixed(2)} ${y0.toFixed(2)}A${r} ${r} 0 ${to - from > 180 ? 1 : 0} 1 ${x1.toFixed(2)} ${y1.toFixed(2)}`;
}

/**
 * A speedometer-style dial: dial({ value, max, kind, label, ticks, warm, why }). value and max in API units;
 * null value shows a dash and `why`. The needle and arc stop at max.
 */
export function dial({ value, max = 100, kind = 'percent', label, ticks = 10, warm = false, why, cover } = {}) {
  const start = -ARC / 2;
  const f = value == null ? 0 : Math.max(0, Math.min(1, value / max));
  const at = start + ARC * f;
  const marks = [];
  for (let i = 0; i <= ticks; i++) {
    const d = start + (ARC * i) / ticks;
    const major = i % 2 === 0;
    const [x0, y0] = point(100, 100, major ? 74 : 78, d), [x1, y1] = point(100, 100, 84, d);
    marks.push(svg('line', { class: ['gauge-tick', major && 'gauge-tick--major'], x1: x0.toFixed(2), y1: y0.toFixed(2), x2: x1.toFixed(2), y2: y1.toFixed(2), 'stroke-width': major ? 2 : 1 }));
  }
  const [nx, ny] = point(100, 100, 66, at);
  return h('div', { class: 'gauge', role: 'img', 'aria-label': `${label}: ${value == null ? 'not known' : quantity(value, kind).textContent}`, cover },
    svg('svg', { viewBox: '0 0 200 170', 'aria-hidden': 'true' },
      svg('path', { class: 'gauge-track', d: arcPath(100, 100, 90, start, start + ARC), 'stroke-width': 8 }),
      f > 0 ? svg('path', { class: ['gauge-arc', warm && 'gauge-arc--warm'], d: arcPath(100, 100, 90, start, at), 'stroke-width': 8 }) : null,
      marks,
      value == null ? null : svg('line', { class: 'gauge-needle', x1: 100, y1: 100, x2: nx.toFixed(2), y2: ny.toFixed(2), 'stroke-width': 3 }),
      svg('circle', { class: 'gauge-hub', cx: 100, cy: 100, r: 6, 'stroke-width': 2 })),
    h('div', { class: 'gauge-read' }, h('span', { class: 'gauge-value' }, quantity(value, kind, { why })), h('span', { class: 'gauge-label' }, label)));
}

/** A fuel-style gauge: fuel({ fraction (0..1), cells, low, label: ['E', 'F'] }). */
export function fuel({ fraction, cells = 10, low = 0.2, ends = ['E', 'F'], label = 'Charge', cover } = {}) {
  const on = fraction == null ? 0 : Math.round(Math.max(0, Math.min(1, fraction)) * cells);
  return h('div', { class: 'fuel', role: 'img', 'aria-label': `${label}: ${fraction == null ? 'not known' : `${Math.round(fraction * 100)} %`}`, cover },
    h('div', { class: 'fuel-scale', vars: { '--cells': cells } }, Array.from({ length: cells }, (_, i) => h('span', {
      class: ['fuel-cell', i < on && 'is-on', i < on && fraction <= low && 'is-low', i < on && fraction <= low / 2 && 'is-empty'],
    }))),
    h('div', { class: 'fuel-ends', 'aria-hidden': 'true' }, h('span', null, ends[0]), h('span', null, ends[1])));
}

/** Odometer digits: odo('12345.6') -> rolling-counter boxes; the part after the point is shown in magenta. */
export function odo(text) {
  const [whole, frac] = String(text ?? '—').split(/[.,]/);
  return h('span', { class: 'odo', 'aria-label': String(text ?? 'not known') },
    [...whole].map((d) => h('span', { class: 'odo-d', 'aria-hidden': 'true' }, d)),
    frac ? [...frac].map((d) => h('span', { class: 'odo-d odo-d--frac', 'aria-hidden': 'true' }, d)) : null);
}

/** Warning lights: lights([{ icon, text, on, tone: 'red' | 'cyan' | 'grn' | undefined (orange), title, cover }]). */
export function lights(list) {
  return h('div', { class: 'lights', role: 'list' }, list.map((l) => h('span', {
    class: ['light', l.on && 'is-on'], role: 'listitem', dataset: { tone: l.tone || null }, title: l.title || null, cover: l.cover,
  }, icon(l.icon, 'i--sm'), h('span', null, l.text), h('span', { class: 'sr-only' }, l.on ? ' (on)' : ' (off)'))));
}
