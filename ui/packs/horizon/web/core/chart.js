// Charts on uPlot (vendor/uplot, loaded on first use), drawn like an oscilloscope screen: neon traces on
// a dark well with a faint grid. A chart fills a box that CSS sizes in rem and follows it (ResizeObserver).
// uPlot draws in device pixels and takes its sizes once, so a chart is built again when the pixel ratio,
// the root size or the theme changes (onRedraw, onTheme). Sizes handed to uPlot go through rpx(), so the
// axis text keeps the 10 px floor and grows with the Interface size. Values are converted to the person's
// units here; null stays null, so a trace breaks where there was no data.

import { h, cssVar } from '../shared/dom.js';
import { convert } from '../shared/units.js';
import { formatTime, formatDate } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { rpx, onRedraw, onTheme } from './display.js';

let loading = null;
/** Load uPlot and its stylesheet once; resolves to window.uPlot. */
export function loadUPlot() {
  loading ??= new Promise((resolve, reject) => {
    if (!document.querySelector('link[data-uplot]')) document.head.append(h('link', { rel: 'stylesheet', href: '/vendor/uplot/uPlot.min.css', dataset: { uplot: '' } }));
    if (window.uPlot) { resolve(window.uPlot); return; }
    const s = document.createElement('script');
    s.src = '/vendor/uplot/uPlot.iife.min.js';
    s.onload = () => resolve(window.uPlot);
    s.onerror = () => { loading = null; reject(new Error('The chart library did not load.')); };
    document.head.append(s);
  });
  return loading;
}

/** Axis text: whole pixels, never under 10 px. */
export const chartFont = (size = 11) => `500 ${Math.max(10, Math.round(rpx(size)))}px "JetBrains Mono", ui-monospace, monospace`;

/** A colour with alpha: '#3df0ff' or 'rgb(...)' -> rgba(). */
export function alpha(color, a) {
  const c = String(color).trim();
  const m = /^#([0-9a-f]{6})$/i.exec(c);
  if (m) { const n = parseInt(m[1], 16); return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${a})`; }
  const r = /^rgba?\(([^)]+)\)$/i.exec(c);
  if (r) { const p = r[1].split(',').map((x) => x.trim()); return `rgba(${p[0]}, ${p[1]}, ${p[2]}, ${a})`; }
  return c;
}

/** Theme colours, read now. */
export const tone = () => ({ axis: cssVar('--ink-3'), grid: cssVar('--chart-grid'), zero: cssVar('--chart-zero'), hub: cssVar('--glass-solid'), ink: cssVar('--ink') });

/**
 * A uPlot chart that fills target: make(width, height) returns uPlot's options (called on every build);
 * data is uPlot's [x, ...ys]. Returns { get u(), setData(d), rebuild() }; cleaned up with the page.
 */
export function mountPlot(target, make, data, ctx, onBuild = null) {
  const uPlot = window.uPlot;
  let u = null, cur = data, later = 0;
  const box = () => ({ width: Math.max(10, Math.floor(target.clientWidth)), height: Math.max(10, Math.floor(target.clientHeight)) });
  const build = () => {
    clearTimeout(later);
    const scale = u ? { min: u.scales.x.min, max: u.scales.x.max } : null;
    u?.destroy();
    const { width, height } = box();
    u = new uPlot(make(width, height), cur, target);
    onBuild?.(u, scale);
  };
  build();
  const ro = new ResizeObserver(() => {
    if (!u) return;
    const { width, height } = box();
    if (width !== u.width || height !== u.height) u.setSize({ width, height });
  });
  ro.observe(target);
  // uPlot learns the new pixel ratio from its own media query: build once that has fired
  const offRedraw = onRedraw(() => { clearTimeout(later); later = setTimeout(build, 120); });
  const offTheme = onTheme(() => build());
  ctx.onCleanup(() => { clearTimeout(later); ro.disconnect(); offRedraw(); offTheme(); u?.destroy(); u = null; });
  return { get u() { return u; }, setData(d) { cur = d; u?.setData(d); }, rebuild: build };
}

/** Time steps that read as whole minutes, hours and days (seconds). */
export const TIME_INCRS = [60, 300, 600, 900, 1800, 3600, 7200, 10800, 21600, 43200, 86400, 172800, 604800, 2592000];

/** An x axis of times in the car's zone; dates for spans over two days. show false: grid only. */
export function timeAxis(tz, { show = true, spanMs = 0 } = {}) {
  const c = tone();
  const long = spanMs > 2 * 86400000;
  return {
    stroke: c.axis, font: chartFont(), grid: { stroke: c.grid, width: 1 }, ticks: { show: false },
    gap: rpx(6), space: rpx(long ? 96 : 80), size: show ? rpx(26) : rpx(4), incrs: TIME_INCRS,
    values: (u, v) => v.map((t) => (show ? (long ? formatDate(t * 1000, tz, { weekday: false, year: false }) : formatTime(t * 1000, tz)) : '')),
  };
}

/** A y axis of plain numbers (the unit is in the chart's caption). */
export function valueAxis({ digits = 0, size = 48, side = 3, show = true } = {}) {
  const c = tone();
  return {
    show, side, stroke: c.axis, font: chartFont(), grid: { show: side === 3, stroke: c.grid, width: 1 }, ticks: { show: false },
    gap: rpx(6), size: rpx(size), space: rpx(28),
    values: (u, v) => v.map((n) => formatNumber(n, { maximumFractionDigits: digits })),
  };
}

/** A fade under a trace, from the trace colour to nothing. */
export const glowFill = (color, a0 = 0.32, a1 = 0) => (u) => {
  const { top, height } = u.bbox;
  const g = u.ctx.createLinearGradient(0, top, 0, top + height);
  g.addColorStop(0, alpha(color, a0)); g.addColorStop(1, alpha(color, a1));
  return g;
};

/** Convert a column of API values to the person's units (null stays null). */
export const toUnits = (values, kind) => (kind ? values.map((v) => (v == null || typeof v !== 'number' ? null : convert(v, kind))) : values.map((v) => (typeof v === 'number' ? v : null)));

/**
 * Traces on a time axis: await scope(el, ctx, { tz, t_ms, series: [{ label, values, kind, color: '--c-soc',
 * fill, scale: 'y' | 'y2', width, dash }], ranges: { y: [0, 100] }, axes: { y: { digits }, y2: { digits } },
 * showX, sync, onCursor(i, u), zoom(fromMs, toMs), hooks }). Returns the plot handle.
 */
export async function scope(target, ctx, opts) {
  await loadUPlot();
  if (!ctx.alive()) return null;
  const { tz, t_ms, series, showX = true, ranges = {}, axes = {}, sync = null, onCursor = null, zoom = null, hooks = {}, onBuild = null } = opts;
  const x = t_ms.map((t) => t / 1000);
  const ys = series.map((s) => toUnits(s.values, s.kind));
  const spanMs = t_ms.length > 1 ? t_ms[t_ms.length - 1] - t_ms[0] : 0;
  const scales = [...new Set(series.map((s) => s.scale || 'y'))];
  const make = (width, height) => {
    const c = tone();
    const sc = { x: { time: true } };
    for (const k of scales) sc[k] = ranges[k] ? { range: typeof ranges[k] === 'function' ? ranges[k] : () => ranges[k] } : { auto: true };
    return {
      width, height,
      padding: [rpx(10), scales.length > 1 ? rpx(4) : rpx(28), showX ? 0 : rpx(4), 0],
      legend: { show: false },
      cursor: {
        ...(sync ? { sync: { key: sync, setSeries: false } } : {}),
        drag: { x: !!zoom, y: false, setScale: false },
        points: { size: rpx(9), width: rpx(2), stroke: c.hub },
      },
      select: { show: !!zoom },
      scales: sc,
      axes: [timeAxis(tz, { show: showX, spanMs }), ...scales.map((k, i) => ({ ...valueAxis({ digits: axes[k]?.digits ?? 0, side: i === 0 ? 3 : 1, size: axes[k]?.size ?? 48 }), scale: k }))],
      series: [{}, ...series.map((s) => {
        const col = cssVar(s.color || '--cyan');
        return { label: s.label, scale: s.scale || 'y', stroke: col, width: rpx(s.width ?? 2), dash: s.dash ? s.dash.map(rpx) : undefined, points: { show: false }, fill: s.fill ? glowFill(col) : undefined, spanGaps: false };
      })],
      hooks: {
        ...hooks,
        setCursor: [...(hooks.setCursor || []), ...(onCursor ? [(u) => onCursor(u.cursor.idx == null || u.cursor.left < 0 ? null : u.cursor.idx, u)] : [])],
        setSelect: zoom ? [(u) => {
          if (u.select.width < rpx(6)) return;
          const a = u.posToVal(u.select.left, 'x'), b = u.posToVal(u.select.left + u.select.width, 'x');
          u.setSelect({ left: 0, top: 0, width: 0, height: 0 }, false);
          zoom(a * 1000, b * 1000);
        }] : [],
      },
    };
  };
  return mountPlot(target, make, [x, ...ys], ctx, onBuild);
}

/**
 * Columns per bucket (days, weeks): await columns(el, ctx, { labels, values, kind, color, onCursor }).
 * labels are the texts under the columns (some are left out when they would crowd).
 */
export async function columns(target, ctx, { labels, values, kind = null, color = '--mag', onCursor = null }) {
  const uPlot = await loadUPlot();
  if (!ctx.alive()) return null;
  const xs = values.map((_, i) => i);
  const ys = toUnits(values, kind);
  const make = (width, height) => {
    const c = tone();
    const col = cssVar(color);
    return {
      width, height, padding: [rpx(8), rpx(10), 0, rpx(4)], legend: { show: false },
      cursor: { y: false, points: { show: false }, drag: { x: false, y: false } },
      scales: { x: { time: false, range: (_, a, b) => [a - 0.6, b + 0.6] }, y: { range: (_, a, b) => [0, Math.max(1, b * 1.1)] } },
      axes: [
        { size: rpx(22), gap: rpx(4), stroke: c.axis, font: chartFont(), grid: { show: false }, ticks: { show: false },
          splits: (u) => { const n = xs.length; const step = Math.max(1, Math.ceil(n / Math.max(3, u.width / rpx(90)))); return xs.filter((i) => (n - 1 - i) % step === 0); },
          values: (_, sp) => sp.map((i) => labels[i] ?? '') },
        { ...valueAxis({ size: 40 }) },
      ],
      series: [{}, { stroke: col, fill: alpha(col, 0.7), width: 0, paths: uPlot.paths.bars({ size: [0.66, rpx(22), rpx(2)], gap: 1 }), points: { show: false } }],
      hooks: { setCursor: onCursor ? [(u) => onCursor(u.cursor.idx)] : [] },
    };
  };
  return mountPlot(target, make, [xs, ys], ctx);
}

/** The chart box: a sized well that a chart fills. */
export const well = (cls, label) => h('div', { class: ['well', cls], role: label ? 'img' : null, 'aria-label': label || null });
