// Charts on uPlot (vendor/uplot, loaded on first use): flat lines, 1 px grid, mono axis text.
//
// - A chart fills a box CSS sizes (.chart, .chart--spark); it follows the box (ResizeObserver) and is
//   built again when the root size, the pixel ratio or the theme changes (uPlot reads sizes, colours and
//   the pixel ratio when it is built). Sizes handed to uPlot are rpx() of design px, text never under 10 px.
// - Series colours are the chart tokens in order: --c1 solid, --c2 dashed, --c3 dashed. The second and
//   third series always differ by dash too, so a chart reads without colour.
// - x is time in ms (uPlot ms: 1), shown in the car's zone. Values are converted to the person's units
//   before they reach a chart (shared/units.js convert); null stays null, so a line breaks over gaps.
// - No legend of uPlot's own: a readout line under the chart shows the values at the cursor.
// - A chart below the fold is built when it comes within a screen of view (shared/lazy.js); its box has its
//   height from CSS already, so nothing moves. Until then u is null and setData keeps the newest data.

import { h, cssVar, replace, styleSheet } from '../shared/dom.js';
import { formatTime, formatDate } from '../shared/time.js';
import { rpx, onChange, onTheme } from './display.js';
import { whenNear } from '../shared/lazy.js';

let load = null;
/**
 * Load uPlot once (its script and its style sheet, both relative to this file); resolves to window.uPlot once the
 * style sheet applies too, so no chart is laid out without it. Pages without a chart never load either.
 */
export function loadUPlot() {
  load ??= Promise.all([
    window.uPlot ? window.uPlot : new Promise((resolve, reject) => {
      const s = document.createElement('script');
      s.src = new URL('../vendor/uplot/uPlot.iife.min.js', import.meta.url).href;
      s.onload = () => resolve(window.uPlot);
      s.onerror = () => { load = null; reject(new Error('The chart library did not load.')); };
      document.head.append(s);
    }),
    styleSheet(new URL('../vendor/uplot/uPlot.min.css', import.meta.url).href, { first: true }),
  ]).then(([u]) => u);
  return load;
}

/** Theme colours, read now. */
export const colors = () => {
  const v = (n) => cssVar(n);
  return { text: v('--text'), muted: v('--muted'), rule: v('--rule'), rule2: v('--rule-2'), bg: v('--bg'), panel: v('--panel'),
    c1: v('--c1'), c2: v('--c2'), c3: v('--c3'), accent: v('--accent'), warn: v('--warn'), bad: v('--bad'), good: v('--good') };
};
/** The mono font for axis text, whole px, at least 10. */
export const font = (size = 12) => `400 ${Math.max(10, rpx(size))}px "JetBrains Mono", monospace`;
/** Dash patterns for series 2 and 3. */
export const DASHES = [null, [6, 4], [2, 3]];
const dashOf = (i) => (DASHES[i] ? DASHES[i].map((x) => rpx(x)) : undefined);

/** A series: n is its place (0 solid c1, 1 dashed c2, 2 dashed c3). */
export function line(label, n = 0, { scale = 'y', width = 1.5, fill = false, token = null, dash = undefined, spanGaps = false } = {}) {
  const c = colors();
  const stroke = token ? cssVar(token) : [c.c1, c.c2, c.c3][n] || c.text;
  return {
    label, scale, stroke, width: rpx(width), spanGaps, points: { show: false },
    dash: dash === undefined ? dashOf(n) : dash,
    fill: fill ? hexA(stroke, 0.12) : undefined,
  };
}

/** '#9CCC65' -> 'rgba(156, 204, 101, a)'. Flat fills only (no gradients). */
export function hexA(color, a) {
  const m = /^#([0-9a-f]{6})$/i.exec(String(color).trim());
  if (!m) return color;
  const n = parseInt(m[1], 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${a})`;
}

/** The time axis in the zone tz: times for a short span, dates for a long one. */
export function timeAxis(tz, { show = true } = {}) {
  const c = colors();
  return {
    show, stroke: c.muted, font: font(12), size: rpx(28), gap: rpx(4), space: rpx(104),
    grid: { stroke: c.rule, width: 1 }, ticks: { stroke: c.rule, width: 1, size: rpx(4) },
    values: (u, splits) => {
      const span = (u.scales.x.max ?? 0) - (u.scales.x.min ?? 0);
      return splits.map((v) => (span > 2 * 86400000 ? formatDate(v, tz, { weekday: false, year: false }) : formatTime(v, tz)));
    },
  };
}

/** A value axis: fmt(v) -> text; side 1 is the right. */
export function valueAxis(fmt, { scale = 'y', side = 3, show = true, grid = true, size = 56 } = {}) {
  const c = colors();
  return {
    scale, side, show, stroke: c.muted, font: font(12), size: rpx(size), gap: rpx(4), space: rpx(32),
    grid: { show: grid, stroke: c.rule, width: 1 }, ticks: { show: false },
    values: (u, splits) => splits.map((v) => (v == null ? '' : fmt(v))),
  };
}

/**
 * A chart in el: make({ width, height, colors }) returns uPlot options without width and height;
 * data is uPlot's [xs, ...ys]. Built again after size, ratio and theme changes. onCursor(i) for the
 * readout. Returns { get u(), setData(d) } or null when the page was left.
 */
export function plot(el, ctx, make, data, { onCursor = null, onBuild = null } = {}) {
  const uPlot = window.uPlot;
  let u = null, cur = data;
  const box = () => ({ width: Math.max(10, Math.floor(el.clientWidth)), height: Math.max(10, Math.floor(el.clientHeight)) });
  const build = () => {
    if (!el) return; // taken down
    u?.destroy();
    const b = box();
    const o = make({ ...b, colors: colors() });
    o.width = b.width; o.height = b.height; o.ms = 1;
    o.legend = { show: false };
    o.hooks = { ...(o.hooks || {}) };
    if (onCursor) o.hooks.setCursor = [...(o.hooks.setCursor || []), (uu) => onCursor(uu.cursor.idx == null || uu.cursor.left < 0 ? null : uu.cursor.idx, uu)];
    u = new uPlot(o, cur, el);
    onBuild?.(u);
  };
  const stopNear = whenNear(el, build);
  const ro = new ResizeObserver(() => {
    if (!u) return;
    const b = box();
    if (b.width !== u.width || b.height !== u.height) u.setSize(b);
  });
  ro.observe(el);
  const off = onChange(() => { if (u) build(); });
  const offTheme = onTheme(() => { if (u) build(); });
  // taken down with the page, or before it when the part of the page it is in is drawn again (core/stage.js): then
  // nothing of the chart is held on to, though the page's clean-up list still names this function
  const destroy = () => {
    if (!el) return;
    stopNear(); ro.disconnect(); off(); offTheme(); u?.destroy(); u = null;
    if (el.__smPlot === handle) delete el.__smPlot;
    el = null; make = null; cur = null; data = null; onCursor = null; onBuild = null;
  };
  ctx.onCleanup(destroy);
  const handle = {
    get u() { return u; },
    setData(d) { cur = d; u?.setData(d); },
    destroy,
  };
  el.__smPlot = handle;
  return handle;
}

/** A cursor that follows the mouse with a 1 px line, no points; sync: a key shared by stacked panes. */
export function cursor(sync = null, { y = false } = {}) {
  const c = colors();
  return {
    x: true, y, points: { show: false },
    drag: { x: !!sync, y: false, setScale: true },
    sync: sync ? { key: sync, setSeries: false, scales: ['x', null] } : undefined,
    dataIdx: undefined, stroke: c.muted,
  };
}

/** A sparkline: one series, no axes, no cursor. */
export function sparkOpts(tz, { token = '--c1', min = null, max = null } = {}) {
  return () => ({
    axes: [{ show: false }, { show: false }],
    scales: { x: { time: true }, y: { range: (u, lo, hi) => [min ?? lo, max ?? hi] } },
    cursor: { show: false }, select: { show: false },
    series: [{}, line('', 0, { token, width: 1.5 })],
    padding: [rpx(2), 0, rpx(2), 0],
  });
}

/** The readout line under a chart: 'at 14:32  speed 94 km/h  power 12 kW'. */
export function readout(idleText = '') {
  const el = h('p', { class: 'chart-read', 'aria-live': 'off' });
  const idle = () => replace(el, h('span', { class: 'muted' }, idleText || ' '));
  idle();
  return {
    el,
    show(parts) {
      if (!parts) { idle(); return; }
      replace(el, parts.flatMap(([k, v], i) => [i ? '  ' : null, h('span', { class: 'muted' }, `${k} `), v]).filter((x) => x != null));
    },
  };
}

/** Draw ys over xs once uPlot is in; on failure the box says so. */
export async function chartOrNote(el, ctx, make, data, opts) {
  try {
    await loadUPlot();
  } catch (e) {
    replace(el, h('p', { class: 'chart-note' }, '-- the chart could not be drawn --'));
    return null;
  }
  if (!ctx.alive()) return null;
  return plot(el, ctx, make, data, opts);
}
