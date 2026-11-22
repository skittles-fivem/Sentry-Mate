// Sentry Mate, "Logbook" UI pack: charts on uPlot.
//
// stackedCharts() draws one or more time charts under each other on one time axis (the drive chart, the
// battery chart), with one hover readout, drag to zoom, double-click to zoom out, gaps where there was no
// sample, and asleep / unknown spans shaded apart. Values arrive metric from the API and are drawn in the
// person's units (shared/units.js), with the unit beside each chart.
//
// Size and sharpness: each chart fills a box app.css sizes in rem and follows it (ResizeObserver). uPlot
// draws in device pixels and follows the pixel ratio itself. Fonts, axis room and line widths are given
// at the standard size and scaled with the interface size; a new root size or theme builds the charts
// again, keeping the zoom. uPlot is loaded on first use (vendor/uplot), so pages without charts never load it.

import { h, cssVar } from '../shared/dom.js';
import { convert, unitLabel } from '../shared/units.js';
import { formatNumber } from '../shared/i18n.js';
import { dateFormat } from '../shared/time.js';
import { display } from './display.js';
import { onTheme } from './prefs.js';

let loading = null;
/** Load uPlot (script and style sheet) once: resolves with the uPlot constructor. */
export function loadUplot() {
  if (window.uPlot) return Promise.resolve(window.uPlot);
  if (!loading) {
    loading = new Promise((resolve, reject) => {
      if (!document.querySelector('link[data-uplot]')) document.head.append(h('link', { rel: 'stylesheet', href: '/vendor/uplot/uPlot.min.css', dataset: { uplot: '' } }));
      const s = h('script', { src: '/vendor/uplot/uPlot.iife.min.js' });
      s.addEventListener('load', () => resolve(window.uPlot));
      s.addEventListener('error', () => { loading = null; reject(new Error('The chart library did not load.')); });
      document.head.append(s);
    });
  }
  return loading;
}

/** A step that reads well: 1, 2, 2.5 or 5 times a power of ten, about span / count. */
export function niceStep(span, count) {
  if (!(span > 0)) return 1;
  const raw = span / count;
  const mag = 10 ** Math.floor(Math.log10(raw));
  return [1, 2, 2.5, 5, 10].map((m) => m * mag).find((s) => s >= raw - 1e-12) || raw;
}
/** The decimals the step between ticks needs, so a 2.5 step never prints 517.5 as 518. */
export function stepDigits(splits) {
  if (!splits || splits.length < 2) return 0;
  const step = Math.abs(splits[1] - splits[0]);
  for (let d = 0; d < 4; d++) if (Math.abs(Math.round(step * 10 ** d) - step * 10 ** d) < 1e-6) return d;
  return 4;
}
/** Tick values from min to max on nice steps. */
export function niceSplits(min, max, count) {
  const span = max - min;
  if (!(span > 0)) return [min];
  const step = niceStep(span, count);
  const out = [];
  for (let v = Math.ceil(min / step - 1e-9) * step; v <= max + 1e-9; v += step) out.push(Math.round(v / step) * step);
  return out;
}

/** The theme's colours for charts, read from the CSS tokens now. */
export function chartColours() {
  const c = (v) => cssVar(v);
  return {
    ink: c('--ink'), ink2: c('--ink-2'), ink3: c('--ink-3'), rule: c('--rule'), strong: c('--rule-strong'),
    accent: c('--accent'), ochre: c('--ochre'), dusk: c('--dusk'), slate: c('--slate'), rust: c('--rust'),
    wash: c('--chart-wash'), ochreWash: c('--chart-ochre-wash'), sunk: c('--chart-sunk'),
    asleep: c('--chart-asleep'), unknown: c('--chart-unknown'),
  };
}

/**
 * Charts under each other on one time axis:
 *
 *   stackedCharts(ctx, container, {
 *     tz,                       // the car's time zone, for the time axis
 *     t_ms: [...],              // the times (ms)
 *     rows: [{ key, name, kind, values: [...], colour: '--accent', fill: '--chart-wash', note, ticks: 4,
 *              floor: 0, signed: false (power: use above zero, regen below), digits, unit (instead of kind's),
 *              extra: [{ values, colour, dash }] (more lines in the same chart, same unit) }],
 *     spans: [{ start_ms, end_ms, state: 'asleep' | 'unknown' }],  // shaded apart
 *     states: [...],            // the state per time (from /series); values are held across asleep, never unknown
 *     onHover(index | null), onZoom(from_ms, to_ms),
 *   }) -> Promise<{ resetZoom(), plots, destroy() }>
 *
 * Each row is { label column, chart box }; the last row carries the time axis. A null value is a gap.
 */
export async function stackedCharts(ctx, container, { tz, t_ms, rows, spans = [], states = null, onHover = null, onZoom = null }) {
  const uPlot = await loadUplot();
  await document.fonts.ready;
  if (ctx.signal.aborted) return null;
  const x = t_ms.map((t) => t / 1000);
  const timeFmt = dateFormat(tz, { hour: 'numeric', minute: '2-digit' });
  const timeFmtS = dateFormat(tz, { hour: 'numeric', minute: '2-digit', second: '2-digit' });
  const dayFmt = dateFormat(tz, { month: 'short', day: 'numeric' });
  const span = x.length > 1 ? x[x.length - 1] - x[0] : 0;
  const boxes = rows.map((r) => {
    const box = h('div', { class: 'chart-box' });
    const unit = r.unit ?? (r.kind ? unitLabel(r.kind) : '');
    container.append(h('div', { class: ['chart-row', `chart-${r.key}`] },
      h('div', { class: 'chart-label' }, h('span', { class: 'chart-name' }, r.name), unit ? h('span', { class: 'chart-unit' }, unit) : null, r.note ? h('span', { class: 'chart-note' }, r.note) : null),
      h('div', { class: 'chart-plot' }, box)));
    const conv = (list) => holdAsleep(list, states).map((v) => (v == null ? null : r.kind ? convert(v, r.kind) : v));
    const vals = conv(r.values);
    const extra = (r.extra || []).map((e) => ({ ...e, vals: conv(e.values) }));
    return { r, box, vals, extra, ...range(r, [vals, ...extra.map((e) => e.vals)].flat()) };
  });
  const sync = uPlot.sync('sync-' + Math.random().toString(36).slice(2));
  let plots = [];
  let syncing = false;

  function range(r, vals) {
    const nums = vals.filter((v) => v != null && Number.isFinite(v));
    let lo = nums.length ? Math.min(...nums) : 0, hi = nums.length ? Math.max(...nums) : 1;
    if (r.signed) lo = Math.min(lo, -0.01);
    else if (r.floor != null && lo >= r.floor) lo = r.floor;
    if (hi === lo) hi = lo + 1;
    const step = niceStep(hi - lo, r.ticks || 4);
    return { yRange: [Math.floor(lo / step) * step, Math.ceil(hi / step) * step], lo, hi };
  }

  function timeTicks(splits) {
    const withSeconds = splits.length > 1 && splits[1] - splits[0] < 60;
    const days = span > 36 * 3600;
    let prevDay = null;
    return splits.map((v) => {
      const ms = v * 1000;
      if (days) {
        const d = dayFmt.format(ms);
        const out = d !== prevDay ? d : timeFmt.format(ms);
        prevDay = d;
        return out;
      }
      return (withSeconds ? timeFmtS : timeFmt).format(ms);
    });
  }

  function options(b, last, C, k) {
    const px = (v) => Math.round(v * k * 10) / 10;
    // uPlot scales the first number of the font by the pixel ratio; never under 10 px, like the page's small print.
    const font = `${Math.max(10, px(12))}px "Logbook Sans", Inter, system-ui, sans-serif`;
    const stroke = cssVar(b.r.colour || '--ink');
    const fill = b.r.fill ? cssVar(b.r.fill) : undefined;
    const series = [{}];
    const data = [x];
    if (b.r.signed) {
      series.push({ stroke: 'rgba(0,0,0,0)', width: 0, fill: C.sunk, points: { show: false } },
        { stroke: 'rgba(0,0,0,0)', width: 0, fill: C.wash, points: { show: false } },
        { stroke, width: 1.25 * k, points: { show: false } });
      data.push(b.vals.map((v) => (v == null ? null : Math.max(0, v))), b.vals.map((v) => (v == null ? null : Math.min(0, v))), b.vals);
    } else {
      series.push({ stroke, width: 1.6 * k, fill, points: { show: false } });
      data.push(b.vals);
      for (const e of b.extra) {
        series.push({ stroke: cssVar(e.colour || '--ink-3'), width: 1.25 * k, dash: e.dash ? [4 * k, 3 * k] : undefined, points: { show: false } });
        data.push(e.vals);
      }
    }
    const digits = b.r.digits ?? (b.hi - b.lo < 4 ? 1 : 0);
    return {
      opts: {
        width: Math.max(1, b.box.clientWidth), height: Math.max(1, b.box.clientHeight),
        padding: [px(10), px(28), last ? 0 : px(8), px(2)],
        legend: { show: false },
        tzDate: (ts) => uPlot.tzDate(new Date(ts * 1000), tz),
        cursor: { sync: { key: sync.key, setSeries: false }, drag: { x: true, y: false, setScale: true }, points: { show: false }, y: false },
        select: { show: true },
        scales: { x: { time: true }, y: { range: () => b.yRange } },
        axes: [
          {
            stroke: C.ink3, font, size: last ? px(30) : px(2), gap: px(6), space: px(80),
            ticks: { show: last, stroke: C.strong, width: 1, size: px(4) },
            grid: { stroke: C.rule, width: 1 },
            values: last ? (u, splits) => timeTicks(splits) : (u, splits) => splits.map(() => ''),
          },
          {
            stroke: C.ink3, font, size: px(48), gap: px(8), ticks: { show: false }, grid: { stroke: C.rule, width: 1 },
            splits: (u, ax, min, max) => niceSplits(min, max, b.r.ticks || 4),
            values: (u, splits) => { const d = Math.max(digits, stepDigits(splits)); return splits.map((v) => formatNumber(v, { maximumFractionDigits: d, minimumFractionDigits: 0 })); },
          },
        ],
        series,
        hooks: {
          drawClear: [(u) => {
            if (!spans.length) return;
            const c = u.ctx;
            c.save();
            for (const s of spans) {
              const a = u.valToPos(s.start_ms / 1000, 'x', true), z = u.valToPos((s.end_ms ?? t_ms[t_ms.length - 1]) / 1000, 'x', true);
              const l = Math.max(u.bbox.left, a), r = Math.min(u.bbox.left + u.bbox.width, z);
              if (r <= l) continue;
              c.fillStyle = s.state === 'asleep' ? C.asleep : C.unknown;
              c.fillRect(l, u.bbox.top, r - l, u.bbox.height);
            }
            c.restore();
          }],
          setCursor: b === boxes[0] && onHover ? [(u) => onHover(u.cursor.idx == null || u.cursor.left < 0 ? null : u.cursor.idx)] : [],
          setScale: [(u, key) => {
            if (key !== 'x' || syncing) return;
            syncing = true;
            const { min, max } = u.scales.x;
            for (const p of plots) if (p !== u) p.setScale('x', { min, max });
            syncing = false;
            onZoom?.(min * 1000, max * 1000);
          }],
          draw: b.r.signed ? [(u) => {
            const lw = Math.max(1, Math.round(uPlot.pxRatio));
            const y0 = Math.round(u.valToPos(0, 'y', true)) + (lw % 2) / 2;
            const c = u.ctx;
            c.save(); c.strokeStyle = C.ink3; c.lineWidth = lw; c.beginPath();
            c.moveTo(u.bbox.left, y0); c.lineTo(u.bbox.left + u.bbox.width, y0); c.stroke(); c.restore();
          }] : [],
        },
      },
      data,
    };
  }

  function build() {
    const shown = plots.length ? { min: plots[0].scales.x.min, max: plots[0].scales.x.max } : null;
    for (const p of plots) p.destroy();
    plots = [];
    const C = chartColours();
    const k = display.scale;
    boxes.forEach((b, i) => {
      const { opts, data } = options(b, i === boxes.length - 1, C, k);
      b.plot = new uPlot(opts, data, b.box);
      plots.push(b.plot);
    });
    if (shown && plots[0]) plots[0].setScale('x', shown);
  }
  build();

  const ro = new ResizeObserver(() => {
    for (const b of boxes) {
      const w = b.box.clientWidth, hh = b.box.clientHeight;
      if (b.plot && w && hh && (Math.abs(w - b.plot.width) > 1 || Math.abs(hh - b.plot.height) > 1)) b.plot.setSize({ width: w, height: hh });
    }
  });
  boxes.forEach((b) => ro.observe(b.box));
  const stopSize = display.onChange((change) => { if (change.rem) build(); });
  const stopTheme = onTheme(() => build());
  const destroy = () => { stopSize(); stopTheme(); ro.disconnect(); plots.forEach((p) => p.destroy()); plots = []; };
  ctx.onCleanup(destroy);
  return {
    get plots() { return plots; },
    resetZoom() { if (x.length) for (const p of plots) p.setScale('x', { min: x[0], max: x[x.length - 1] }); },
    destroy,
  };
}

/**
 * Asleep is not unknown: while the car sleeps nothing changes, so a value is held across asleep buckets
 * that have no sample. Unknown buckets stay empty, so a line never crosses time with no data.
 */
export function holdAsleep(values, states) {
  if (!states) return values;
  let last = null;
  return values.map((v, i) => {
    if (v != null) { last = v; return v; }
    if (states[i] === 'asleep' && last != null) return last;
    if (states[i] !== 'asleep') last = null;
    return null;
  });
}

/**
 * The asleep and unknown spans of a /series answer (its parallel state column), for stackedCharts.
 * Neighbouring buckets of the same state are joined.
 */
export function spansFromStates(t_ms, states, bucketMs = 0) {
  const out = [];
  (states || []).forEach((s, i) => {
    if (s !== 'asleep' && s !== 'unknown') return;
    const start = t_ms[i], end = (t_ms[i + 1] ?? t_ms[i] + bucketMs);
    const last = out[out.length - 1];
    if (last && last.state === s && last.end_ms >= start) last.end_ms = end;
    else out.push({ state: s, start_ms: start, end_ms: end });
  });
  return out;
}
