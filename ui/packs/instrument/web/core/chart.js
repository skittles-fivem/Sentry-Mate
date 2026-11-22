// Charts on uPlot (vendor/uplot, loaded on first use). Canvases draw in device pixels, so:
// - a chart fills a box that CSS sizes in rem; it follows the box (ResizeObserver);
// - it is built again when the pixel ratio or the Interface size changes (uPlot draws text, lines and
//   gaps in pixels and takes its pixel ratio when it is built), and after other root-size changes once
//   they settle; a rebuild keeps the cursor where it was;
// - sizes handed to uPlot come from rpx() (px at the 16 px design size -> px now), so chart text keeps
//   the 10 px floor and grows with the interface;
// - colours come from the theme's CSS custom properties, read again on every build (a theme change
//   rebuilds the chart).
// Values are converted to the person's units here (shared/units.js convert), and null stays null, so a
// line breaks where there was no data instead of carrying a value across unknown time.

import { convert, unitLabel, format } from '../shared/units.js';
import { formatTime, formatDate } from '../shared/time.js';
import { cssVar } from '../shared/dom.js';
import { formatNumber } from '../shared/i18n.js';
import { rpx, onChange, onTheme, uplotRatio } from './display.js';

let load = null;
/** Load uPlot once; resolves to window.uPlot. */
export function loadUPlot() {
  load ??= window.uPlot ? Promise.resolve(window.uPlot) : new Promise((resolve, reject) => {
    const s = document.createElement('script');
    s.src = '/vendor/uplot/uPlot.iife.min.js';
    s.onload = () => { uplotRatio.release(); resolve(window.uPlot); };
    s.onerror = () => { uplotRatio.release(); load = null; reject(new Error('The chart library did not load.')); };
    uplotRatio.capture(); // uPlot asks for its pixel-ratio media query while its script runs
    document.head.append(s);
  });
  return load;
}

/** The font for axis text: whole pixels, never under 10 px. */
export const chartFont = (size = 11, weight = 500) => `${weight} ${Math.max(10, Math.round(rpx(size)))}px "SM Inter"`;

/** A colour with alpha: '#3FE0D0' or 'rgb(...)' -> rgba. */
export function hexA(color, a) {
  const c = String(color).trim();
  const m = /^#([0-9a-f]{6})$/i.exec(c);
  if (m) { const n = parseInt(m[1], 16); return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${a})`; }
  const r = /^rgba?\(([^)]+)\)$/i.exec(c);
  if (r) { const p = r[1].split(',').map((x) => x.trim()); return `rgba(${p[0]}, ${p[1]}, ${p[2]}, ${a})`; }
  return c;
}

/** Theme colours for charts, read now. */
export const colors = () => ({
  axis: cssVar('--text-3'), grid: cssVar('--chart-grid'), zero: cssVar('--chart-zero'), cursor: cssVar('--bg-1'),
  text: cssVar('--text'), live: cssVar('--live'), warm: cssVar('--warm'),
});

/**
 * A uPlot chart that fills its box: make(width, height) returns the uPlot options (called again on
 * every build), data is uPlot's [x, ...ys]. onBuild(u) runs after each build. Cleaned up with the page.
 * Returns { get u(), setData(data) }.
 */
export function mountPlot(target, make, data, ctx, onBuild = null) {
  const uPlot = window.uPlot;
  // a chart drawn again into the same box (a live refresh) takes the old one's place, never a second one under it
  target.__smPlot?.destroy();
  let u = null, later = 0, cur = data;
  const box = () => ({ width: Math.max(10, Math.floor(target.clientWidth)), height: Math.max(10, Math.floor(target.clientHeight)) });
  const cursorAt = () => (u && u.cursor.idx != null && u.cursor.left >= 0 ? { x: u.posToVal(u.cursor.left, 'x'), fy: u.cursor.top / Math.max(1, u.over.clientHeight) } : null);
  const putCursor = (c) => { if (c) requestAnimationFrame(() => { if (u) u.setCursor({ left: u.valToPos(c.x, 'x'), top: c.fy * u.over.clientHeight }); }); };
  const build = () => {
    clearTimeout(later);
    const c = cursorAt();
    u?.destroy();
    const { width, height } = box();
    u = new uPlot(make(width, height), cur, target);
    onBuild?.(u);
    putCursor(c);
  };
  build();
  const ro = new ResizeObserver(() => {
    if (!u) return;
    const { width, height } = box();
    if (width !== u.width || height !== u.height) { const c = cursorAt(); u.setSize({ width, height }); putCursor(c); }
  });
  ro.observe(target);
  const off = onChange((ev) => {
    if (ev.dprChanged || ev.sizeChanged) build();
    else { clearTimeout(later); later = setTimeout(build, 150); }
  });
  const offTheme = onTheme(() => build());
  const destroy = () => { clearTimeout(later); ro.disconnect(); off(); offTheme(); u?.destroy(); u = null; if (target.__smPlot === plot) delete target.__smPlot; };
  ctx.onCleanup(destroy);
  const plot = { get u() { return u; }, setData(d) { cur = d; u?.setData(d); }, rebuild: build, destroy };
  target.__smPlot = plot;
  return plot;
}

/** Time ticks that read as whole minutes and hours (seconds). */
export const TIME_INCRS = [60, 300, 600, 900, 1800, 3600, 7200, 10800, 21600, 43200, 86400, 172800, 604800, 2592000];

/**
 * An x axis of times in the car's zone: labels '5:30 PM' within a day, dates for longer spans.
 * opts.show false draws the grid without labels (charts stacked over one that shows them).
 */
export function timeAxis(tz, { show = true, spanMs = null } = {}) {
  const c = colors();
  const long = spanMs != null && spanMs > 2 * 86400000;
  return {
    stroke: c.axis, font: chartFont(), grid: { stroke: c.grid, width: 1 }, ticks: { show: false },
    gap: rpx(6), space: rpx(long ? 96 : 84), size: show ? rpx(26) : rpx(2), incrs: TIME_INCRS,
    values: (u, v) => (show ? v.map((t) => (long ? formatDate(t * 1000, tz, { weekday: false, year: false }) : formatTime(t * 1000, tz))) : v.map(() => '')),
  };
}

/** A y axis with plain numbers (the unit is in the chart's label). */
export function valueAxis({ digits = 0, size = 52 } = {}) {
  const c = colors();
  return {
    stroke: c.axis, font: chartFont(), grid: { stroke: c.grid, width: 1 }, ticks: { show: false },
    gap: rpx(8), size: rpx(size), space: rpx(30),
    values: (u, v) => v.map((n) => formatNumber(n, { maximumFractionDigits: digits })),
  };
}

/** A vertical gradient fill for an area under a line. */
export function areaFill(color, a0 = 0.26, a1 = 0.01) {
  return (u) => {
    const { top, height } = u.bbox;
    const g = u.ctx.createLinearGradient(0, top, 0, top + height);
    g.addColorStop(0, hexA(color, a0)); g.addColorStop(1, hexA(color, a1));
    return g;
  };
}

/** Convert a column of API values to the person's units (null stays null). */
export const toUnits = (values, kind) => (kind ? values.map((v) => (v == null ? null : convert(v, kind))) : values.slice());

/**
 * One line chart on a time axis, filling target (sized by CSS):
 *   await timeChart(el, ctx, { tz, t_ms, series: [{ label, values, kind, color: '--c-soc', area: true }],
 *     range: [0, 100], showX: true, sync: 'drive-1', onCursor(i) })
 * kind converts values to the person's units. Several series share the y axis. Returns the plot handle.
 */
export async function timeChart(target, ctx, opts) {
  await loadUPlot();
  if (!ctx.alive()) return null;
  const { tz, t_ms, series, showX = true, range = null, sync = null, onCursor = null, digits = 0, onBuild = null } = opts;
  const x = t_ms.map((t) => t / 1000);
  const ys = series.map((s) => toUnits(s.values, s.kind));
  const spanMs = t_ms.length ? t_ms[t_ms.length - 1] - t_ms[0] : 0;
  const make = (width, height) => {
    const c = colors();
    return {
      width, height,
      padding: [rpx(10), rpx(14), showX ? 0 : rpx(6), 0],
      legend: { show: false },
      cursor: {
        ...(sync ? { sync: { key: sync, setSeries: false } } : {}),
        drag: { x: !!opts.zoom, y: false, setScale: false },
        points: { size: rpx(8), width: rpx(2), stroke: c.cursor },
      },
      select: { show: !!opts.zoom },
      scales: { x: { time: true }, y: range ? { range: () => range } : { auto: true } },
      axes: [timeAxis(tz, { show: showX, spanMs }), valueAxis({ digits })],
      series: [{}, ...series.map((s) => {
        const color = cssVar(s.color || '--live');
        return { label: s.label, stroke: color, width: rpx(s.width ?? 2), points: { show: false }, fill: s.area ? areaFill(color) : undefined, spanGaps: false };
      })],
      hooks: {
        setCursor: onCursor ? [(u) => onCursor(u.cursor.idx == null || u.cursor.left < 0 ? null : u.cursor.idx, u)] : [],
        ...(opts.zoom ? { setSelect: [(u) => {
          if (u.select.width < rpx(6)) return;
          const a = u.posToVal(u.select.left, 'x'), b = u.posToVal(u.select.left + u.select.width, 'x');
          u.setSelect({ left: 0, top: 0, width: 0, height: 0 }, false);
          opts.zoom(a * 1000, b * 1000);
        }] } : {}),
      },
    };
  };
  return mountPlot(target, make, [x, ...ys], ctx, onBuild);
}

/**
 * Bars per day (or any bucket): await barChart(el, ctx, { t_ms, values, kind, color, tz, onCursor }).
 * One bar per value at its time; labels are dates in the zone.
 */
export async function barChart(target, ctx, { t_ms, values, kind = null, color = '--live', tz, onCursor = null }) {
  const uPlot = await loadUPlot();
  if (!ctx.alive()) return null;
  const xs = t_ms.map((t) => t / 1000);
  const ys = toUnits(values, kind);
  const make = (width, height) => {
    const c = colors();
    const col = cssVar(color);
    return {
      width, height,
      padding: [rpx(8), rpx(16), 0, rpx(8)], legend: { show: false },
      cursor: { y: false, points: { show: false }, drag: { x: false, y: false } },
      scales: { x: { time: false, range: (_, a, b) => [a - 43200, b + 43200] }, y: { range: (_, a, b) => [0, Math.max(1, b * 1.12)] } },
      axes: [
        {
          show: true, size: rpx(20), gap: rpx(2), stroke: c.axis, font: chartFont(), grid: { show: false }, ticks: { show: false },
          splits: (u) => { const n = xs.length; const step = Math.max(1, Math.ceil(n / Math.max(4, u.width / rpx(110)))); return xs.filter((_, i) => (n - 1 - i) % step === 0); },
          values: (_, sp) => sp.map((v) => formatDate(v * 1000, tz, { weekday: false, year: false })),
        },
        { show: false },
      ],
      series: [{}, { stroke: col, fill: hexA(col, 0.75), width: 0, paths: uPlot.paths.bars({ size: [0.62, rpx(18), rpx(2)], radius: [0.3, 0], gap: 1 }), points: { show: false } }],
      hooks: { setCursor: onCursor ? [(u) => onCursor(u.cursor.idx)] : [] },
    };
  };
  return mountPlot(target, make, [xs, ys], ctx);
}

/** '12.4 mi' for a chart readout. */
export const readout = (v, kind, digits) => format(v, kind, digits != null ? { digits } : {});
export { unitLabel };
