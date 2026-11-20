// Small pictures that sit inside a text line: a sparkline (an SVG polyline sized by CSS to a run of
// characters and one text line high) and a grid of one-character cells (heat maps as text).

import { h, svg } from '../shared/dom.js';

/**
 * A sparkline of values (null breaks the line) in a box CSS sizes (.spark: 12 characters by one line).
 * The stroke is a token in CSS (.spark-l: --c1), 1.5 px whatever the box size.
 */
export function spark(values, { label = null, min = 0, max = null, cls = null } = {}) {
  const W = 120, H = 24;
  const vals = (values || []).map((v) => (v == null || !Number.isFinite(v) ? null : v));
  const known = vals.filter((v) => v != null);
  const hi = max ?? Math.max(min + 1e-9, ...known);
  const n = Math.max(1, vals.length - 1);
  const runs = [];
  let cur = [];
  vals.forEach((v, i) => {
    if (v == null) { if (cur.length) runs.push(cur); cur = []; return; }
    const y = H - 1 - ((Math.max(min, Math.min(hi, v)) - min) / (hi - min || 1)) * (H - 2);
    cur.push(`${((i / n) * W).toFixed(2)},${y.toFixed(2)}`);
  });
  if (cur.length) runs.push(cur);
  return svg('svg', { class: ['spark', cls], viewBox: `0 0 ${W} ${H}`, preserveAspectRatio: 'none', role: label ? 'img' : null, 'aria-label': label, 'aria-hidden': label ? null : 'true', focusable: 'false' },
    runs.map((r) => svg('polyline', { class: 'spark-l', points: r.length === 1 ? `${r[0]} ${r[0]}` : r.join(' '), 'vector-effect': 'non-scaling-stroke' })));
}

/** The character for a share 0..1 of a text heat map: ' ' none, then . : + # by how much. */
export const SHADES = [' ', '.', ':', '+', '#'];
export const shade = (frac) => (!frac ? SHADES[0] : SHADES[Math.min(4, 1 + Math.floor(Math.max(0, Math.min(0.9999, frac)) * 4))]);

/**
 * A text heat map: rows of one-character cells with a label column and an axis line under it.
 *   grid({ rows: [{ label, cells: [n, ...] }], axis: ['mon', ...] | null, max, tone, title(r, c, n), onCol(c) })
 * Each cell is a span with data-col, so hovering a column can point other charts at it.
 */
export function textGrid({ rows, axis = null, max = null, tone = 'accent', title = null, onCol = null, labelW = 8, cls = null }) {
  const top = max ?? Math.max(1, ...rows.flatMap((r) => r.cells));
  const pad = (s) => String(s).padEnd(labelW, ' ').slice(0, labelW);
  const body = rows.map((r, ri) => h('div', { class: 'tg-row' }, h('span', { class: 'tg-k' }, pad(r.label)),
    h('span', { class: ['tg-cells', `tg--${tone}`] }, r.cells.map((n, ci) => h('span', { class: ['tg-c', n ? `tg-c${Math.min(4, 1 + Math.floor(Math.min(0.9999, n / top) * 4))}` : 'tg-c0'], dataset: { col: ci }, title: title ? title(ri, ci, n) : null }, shade(n / top))))));
  const ax = axis ? h('div', { class: 'tg-row tg-axis' }, h('span', { class: 'tg-k' }, pad('')), h('span', { class: 'tg-cells' }, axis.join(''))) : null;
  const el = h('div', { class: ['tg', cls], role: 'img' }, h('div', { class: 'tg-in' }, body, ax));
  if (onCol) {
    el.addEventListener('mousemove', (e) => { const c = e.target instanceof Element ? e.target.closest('.tg-c') : null; if (c) onCol(+c.dataset.col); });
    el.addEventListener('mouseleave', () => onCol(null));
  }
  return el;
}

/** An axis line for n columns: a label every `step` columns, each label padded to the step. */
export function gridAxis(n, step, labelOf) {
  const out = [];
  for (let i = 0; i < n; i += step) {
    const w = Math.min(step, n - i);
    out.push(String(labelOf(i)).slice(0, Math.max(0, w - 1)).padEnd(w, ' '));
  }
  return out;
}
