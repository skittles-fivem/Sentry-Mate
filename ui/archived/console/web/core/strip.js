// The state strip: what the car did over a period as one line of characters, one per cell, coloured by
// state: '=' driving, '+' charging, '-' parked and awake, '.' asleep, 'u' updating, '?' no data. A time
// line under it and a key with each state's share of the period. Items: GET /cars/{car}/activity.

import { h } from '../shared/dom.js';
import { formatTime, formatDate } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import * as api from '../shared/api.js';

export const STRIP = { driving: ['=', 'drive'], charging: ['+', 'charge'], online: ['-', 'park'], online_vehicle_bus_only: ['-', 'park'], parked: ['-', 'park'], asleep: ['.', 'sleep'], updating: ['u', 'update'], unknown: ['?', 'gap'] };
const KEY = [['=', 'drive', 'driving'], ['+', 'charge', 'charging'], ['-', 'park', 'parked, awake'], ['.', 'sleep', 'asleep'], ['u', 'update', 'updating'], ['?', 'gap', 'no data']];

export function stateStrip(ctx, items, from, to, tz, { cover = null } = {}) {
  const cs = getComputedStyle(document.documentElement);
  const adv = parseFloat(cs.getPropertyValue('--a2')) || 8;
  const width = Math.max(200, ctx.view.clientWidth - 2 * (parseFloat(cs.getPropertyValue('--gut')) || 16));
  const max = Math.min(180, Math.floor(width / adv));
  const cells = Math.max(16, max);
  const step = (to - from) / cells;
  const nowMs = api.now();
  const kinds = [];
  for (let i = 0; i < cells; i++) {
    const a = from + i * step, b = a + step;
    const share = new Map();
    for (const it of items) {
      const s = Math.max(a, it.start_ms), e = Math.min(b, it.end_ms ?? to);
      if (e > s) share.set(it.kind, (share.get(it.kind) || 0) + (e - s));
    }
    const pri = ['driving', 'charging', 'updating'].find((k) => share.get(k) > step * 0.2);
    kinds.push(pri || [...share.entries()].sort((x, y) => y[1] - x[1])[0]?.[0] || (a > nowMs ? 'future' : 'unknown'));
  }
  const runs = [];
  for (const k of kinds) { const last = runs[runs.length - 1]; if (last && last.k === k) last.n++; else runs.push({ k, n: 1 }); }
  const spent = {};
  for (const it of items) spent[it.kind] = (spent[it.kind] || 0) + Math.max(0, Math.min(it.end_ms ?? to, to) - Math.max(it.start_ms, from));
  const share = (cl) => {
    const t = Object.entries(spent).filter(([k]) => STRIP[k]?.[1] === cl).reduce((s, [, v]) => s + v, 0);
    return ` ${formatNumber((t / Math.max(1, to - from)) * 100, { maximumFractionDigits: 0 })}%`;
  };
  const long = to - from > 2 * 86400000;
  const tick = (ms) => (long ? formatDate(ms, tz, { weekday: false }) : formatTime(ms, tz));
  return h('div', { class: 'strip', cover },
    h('div', { class: 'strip-cells', role: 'img', 'aria-label': 'What the car did over the period' },
      runs.map((r) => h('span', { class: `strip-${STRIP[r.k]?.[1] || 'none'}` }, (STRIP[r.k]?.[0] || ' ').repeat(r.n)))),
    h('div', { class: 'strip-axis muted' }, h('span', null, tick(from)), h('span', null, tick(to))),
    h('div', { class: 'strip-key' }, KEY.map(([c, cl, w]) => h('span', { class: 'nowrap' }, h('span', { class: `strip-${cl}` }, c), ` ${w}${share(cl)}`))));
}
