// Sentry Mate, "Pebble" UI pack: time charts on uPlot.
//
// timeChart(ctx, tileBody, opts) draws one rounded plot with one or two lines (a left and a right axis) and a
// legend of round keys above it. Each key shows its line's value under the cursor (or the last one) and hides or
// shows the line when pressed. Charts given the same `sync` key share one cursor and one zoom: hover one and the
// others follow; drag to zoom, double-click to zoom out. Values arrive metric and are drawn in the person's units;
// a null is a gap, asleep and unknown spans are shaded. Series colours come from the tokens: --c1 coral, --c2 sky
// (dashed, so it reads without colour), --c3 teal.
//
// The plot fills a box app.css sizes (15 rem, 200 px on a phone) and follows it; fonts and line widths follow the
// Interface size; a new theme or root size builds the chart again with the same zoom. uPlot loads on first use.

import { h, cssVar } from '../shared/dom.js';
import { convert, unitLabel, format } from '../shared/units.js';
import { formatNumber } from '../shared/i18n.js';
import { dateFormat } from '../shared/time.js';
import { display } from './display.js';
import { onTheme } from './prefs.js';

let loading = null;
/** Load uPlot (script and style sheet) once. */
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

function niceStep(span, count) {
  if (!(span > 0)) return 1;
  const raw = span / count;
  const mag = 10 ** Math.floor(Math.log10(raw));
  return [1, 2, 2.5, 5, 10].map((m) => m * mag).find((s) => s >= raw - 1e-12) || raw;
}
function niceRange(vals, { floor = null, signed = false, ticks = 4 } = {}) {
  const nums = vals.filter((v) => v != null && Number.isFinite(v));
  let lo = nums.length ? Math.min(...nums) : 0, hi = nums.length ? Math.max(...nums) : 1;
  if (signed) lo = Math.min(lo, 0);
  else if (floor != null && lo >= floor) lo = floor;
  if (hi === lo) hi = lo + 1;
  const step = niceStep(hi - lo, ticks);
  return { min: Math.floor(lo / step + 1e-9) * step, max: Math.ceil(hi / step - 1e-9) * step, step };
}
function splitsOf(r) {
  const out = [];
  for (let v = r.min; v <= r.max + r.step / 1000; v += r.step) out.push(Math.round(v / r.step) * r.step);
  return out;
}
const digitsOf = (step) => { for (let d = 0; d < 4; d++) if (Math.abs(Math.round(step * 10 ** d) - step * 10 ** d) < 1e-6) return d; return 3; };

/** Hold a value across asleep buckets without samples (nothing changes while the car sleeps); unknown stays a gap. */
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
/** Asleep and unknown spans of a /series answer, joined. */
export function spansOf(t_ms, states, bucketMs = 0) {
  const out = [];
  (states || []).forEach((s, i) => {
    if (s !== 'asleep' && s !== 'unknown') return;
    const start = t_ms[i], end = t_ms[i + 1] ?? t_ms[i] + bucketMs;
    const last = out[out.length - 1];
    if (last && last.state === s && last.end_ms >= start) last.end_ms = end;
    else out.push({ state: s, start_ms: start, end_ms: end });
  });
  return out;
}

const syncs = new Map();
const peers = new Map(); // sync key -> Set of plots (their zoom follows)
const zooming = new Set();

/**
 * A chart in `body`: timeChart(ctx, body, { tz, t_ms, series: [{ key, name, kind, values, tone: 'c1' | 'c2' | 'c3',
 * axis: 'left' | 'right', fill, dashed, points (dots only), floor, signed, digits, unit, cover }], states, spans, sync, onCursor(i),
 * onZoom(from_ms, to_ms), cover, label }) -> Promise<{ plot, reset(), destroy() } | null>.
 */
export async function timeChart(ctx, body, { tz, t_ms, series, states = null, spans = [], sync = null, onCursor = null, onZoom = null, cover = null, label = 'Chart', yBand = null, short = false }) {
  const legend = h('div', { class: 'legend', role: 'group', 'aria-label': `${label}: lines`, cover });
  const box = h('div', { class: ['plot', { 'plot-short': short }], role: 'img', 'aria-label': `${label}. ${series.map((s) => s.name).join(', ')} over time.` });
  body.append(legend, box);
  const uPlot = await loadUplot();
  await document.fonts.ready;
  if (ctx.signal.aborted) return null;
  const x = t_ms.map((t) => t / 1000);
  const span = x.length > 1 ? x[x.length - 1] - x[0] : 0;
  const tFmt = dateFormat(tz, { hour: 'numeric', minute: '2-digit' });
  const dFmt = dateFormat(tz, { month: 'short', day: 'numeric' });
  const lines = series.map((s, n) => {
    const vals = holdAsleep(s.values || [], states).map((v) => (v == null || typeof v !== 'number' ? null : s.kind ? convert(v, s.kind) : v));
    return { ...s, n, vals, unitText: s.unit ?? (s.kind ? unitLabel(s.kind) : ''), shown: true, axis: s.axis || (n === 0 ? 'left' : 'right') };
  });
  // the legend keys: swatch, name, value at the cursor, unit; pressing hides or shows the line
  const keys = lines.map((l) => {
    const val = h('span', { class: 'key-val' }, '—');
    const b = h('button', { type: 'button', class: ['key', `tone-${l.tone || 'c1'}`, { 'is-dashed': l.dashed }], 'aria-pressed': 'true', cover: l.cover,
      title: `Show or hide ${l.name}`, onclick: () => toggle(l, b) },
    h('span', { class: 'key-sw', 'aria-hidden': 'true' }), h('span', { class: 'key-name' }, l.name), val);
    l.valEl = val;
    return b;
  });
  legend.append(...keys);
  const lastIdx = (vals) => { for (let i = vals.length - 1; i >= 0; i--) if (vals[i] != null) return i; return -1; };
  const show = (i) => {
    for (const l of lines) {
      const j = i == null ? lastIdx(l.vals) : i;
      const v = j >= 0 ? l.vals[j] : null;
      l.valEl.textContent = v == null ? '—' : `${formatNumber(v, { maximumFractionDigits: l.digits ?? (Math.abs(v) < 10 ? 1 : 0) })}${l.unitText ? ` ${l.unitText}` : ''}`;
    }
  };
  show(null);
  let plot = null;
  let keySync = null;
  if (sync) { if (!syncs.has(sync)) syncs.set(sync, uPlot.sync(sync)); keySync = syncs.get(sync); }

  function toggle(l, b) {
    l.shown = !l.shown;
    b.setAttribute('aria-pressed', String(l.shown));
    if (plot) plot.setSeries(l.n + 1, { show: l.shown });
  }

  function opts() {
    const k = display.scale;
    const px = (v) => Math.round(v * k * 10) / 10;
    const font = `500 ${Math.max(10, px(12))}px Manrope, system-ui, sans-serif`;
    const muted = cssVar('--muted'), grid = cssVar('--chart-grid'), ground = cssVar('--chart-ground');
    const scales = { x: { time: true } };
    const axes = [{
      stroke: muted, font, size: px(30), gap: px(6), space: px(84), ticks: { show: false }, grid: { stroke: grid, width: 1 },
      values: (u, splits) => {
        const days = span > 36 * 3600;
        let prev = null;
        return splits.map((v) => { const ms = v * 1000; const d = dFmt.format(ms); const out = days && d !== prev ? d : tFmt.format(ms); prev = d; return out; });
      },
    }];
    const sides = [...new Set(lines.map((l) => l.axis))];
    for (const side of sides) {
      const mine = lines.filter((l) => l.axis === side);
      const r = niceRange(mine.flatMap((l) => l.vals), { floor: mine[0].floor ?? null, signed: mine.some((l) => l.signed), ticks: 4 });
      scales[side] = { range: () => [r.min, r.max] };
      const d = digitsOf(r.step);
      axes.push({
        scale: side, side: side === 'left' ? 3 : 1, stroke: muted, font, size: px(46), gap: px(6), ticks: { show: false },
        grid: { show: side === sides[0], stroke: grid, width: 1 },
        splits: () => splitsOf(r),
        values: (u, splits) => splits.map((v) => formatNumber(v, { maximumFractionDigits: d, minimumFractionDigits: 0 })),
      });
    }
    const ser = [{}, ...lines.map((l) => {
      const c = cssVar(`--${l.tone || 'c1'}`);
      return {
        scale: l.axis, show: l.shown, stroke: c, width: l.points ? 0 : 2 * k, dash: l.dashed ? [5 * k, 4 * k] : undefined,
        points: l.points ? { show: true, size: 7 * k, stroke: c, fill: c, width: 0 } : { show: false }, paths: l.points ? () => null : undefined,
        fill: l.fill ? cssVar(`--${l.tone || 'c1'}-wash`) : undefined, spanGaps: false,
      };
    })];
    return {
      width: Math.max(1, Math.floor(box.clientWidth)), height: Math.max(1, Math.floor(box.clientHeight)),
      padding: [px(14), sides.includes('right') ? px(4) : px(16), px(4), sides.includes('left') ? px(4) : px(12)],
      legend: { show: false }, scales, axes, series: ser,
      tzDate: (ts) => uPlot.tzDate(new Date(ts * 1000), tz),
      cursor: { sync: keySync ? { key: keySync.key, setSeries: false } : undefined, drag: { x: true, y: false, setScale: true }, points: { show: false }, y: false },
      select: { show: true },
      hooks: {
        drawClear: [(u) => {
          const c = u.ctx;
          c.save();
          c.fillStyle = ground;
          c.fillRect(u.bbox.left, u.bbox.top, u.bbox.width, u.bbox.height);
          c.fillStyle = cssVar('--chart-asleep');
          for (const s of spans) {
            const a = u.valToPos(s.start_ms / 1000, 'x', true), z = u.valToPos((s.end_ms ?? t_ms[t_ms.length - 1]) / 1000, 'x', true);
            const l = Math.max(u.bbox.left, a), r = Math.min(u.bbox.left + u.bbox.width, z);
            if (r > l) c.fillRect(l, u.bbox.top, r - l, u.bbox.height);
          }
          if (yBand) {
            const y1 = u.valToPos(yBand.to, yBand.axis || 'left', true), y0 = u.valToPos(yBand.from, yBand.axis || 'left', true);
            const top = Math.max(u.bbox.top, Math.min(y0, y1)), bot = Math.min(u.bbox.top + u.bbox.height, Math.max(y0, y1));
            c.fillStyle = cssVar('--c3-wash');
            if (bot > top) c.fillRect(u.bbox.left, Math.round(top), u.bbox.width, Math.round(bot - top));
          }
          c.restore();
        }],
        setCursor: [(u) => { const i = u.cursor.idx == null || u.cursor.left < 0 ? null : u.cursor.idx; show(i); onCursor?.(i); }],
        setScale: [(u, key) => {
          if (key !== 'x' || u.scales.x.min == null) return;
          if (sync && !zooming.has(sync)) {
            zooming.add(sync);
            try { for (const p of peers.get(sync) || []) if (p !== u) p.setScale('x', { min: u.scales.x.min, max: u.scales.x.max }); } finally { zooming.delete(sync); }
          }
          onZoom?.(u.scales.x.min * 1000, u.scales.x.max * 1000);
        }],
      },
    };
  }
  const data = () => [x, ...lines.map((l) => l.vals)];
  function build() {
    const zoom = plot ? { min: plot.scales.x.min, max: plot.scales.x.max } : null;
    if (plot) { keySync?.unsub(plot); peers.get(sync)?.delete(plot); plot.destroy(); }
    plot = new uPlot(opts(), data(), box);
    keySync?.sub(plot);
    if (sync) { if (!peers.has(sync)) peers.set(sync, new Set()); peers.get(sync).add(plot); }
    if (zoom && zoom.min != null) plot.setScale('x', zoom);
    // zooms follow across charts of one sync key
    plot.over.addEventListener('dblclick', () => reset());
  }
  build();
  const ro = new ResizeObserver(() => {
    const w = Math.floor(box.clientWidth), hh = Math.floor(box.clientHeight);
    if (plot && w && hh && (w !== plot.width || hh !== plot.height)) plot.setSize({ width: w, height: hh });
  });
  ro.observe(box);
  const stopSize = display.onChange((c) => { if (c.rem) build(); });
  const stopTheme = onTheme(() => build());
  function reset() { if (plot && x.length) plot.setScale('x', { min: x[0], max: x[x.length - 1] }); }
  const destroy = () => { stopSize(); stopTheme(); ro.disconnect(); if (plot) { keySync?.unsub(plot); peers.get(sync)?.delete(plot); plot.destroy(); plot = null; } };
  ctx.onCleanup(destroy);
  return { get plot() { return plot; }, reset, destroy, zoomTo(a, b) { plot?.setScale('x', { min: a / 1000, max: b / 1000 }); } };
}

/**
 * Bars in plain boxes (no canvas): bars(items: [{ label, value, text, href, title }], { tone, cover, label, max }).
 * Each bar's height is its share of the largest; its value text sits above it.
 */
export function bars(items, { tone = 'c1', cover = null, label = 'Bars', max = null } = {}) {
  const top = max ?? Math.max(0, ...items.map((i) => i.value || 0));
  return h('div', { class: ['bars', `tone-${tone}`], role: 'list', 'aria-label': label, cover },
    items.map((i) => {
      const raw = top > 0 ? Math.max(0, Math.min(1, (i.value || 0) / top)) : 0;
      const share = raw > 0 ? Math.max(raw, 0.06) : 0; // never a hairline: a small share still shows as a small bar
      const fill = h('span', { class: 'bar-fill' });
      fill.style.setProperty('--v', share.toFixed(4));
      return h(i.href ? 'a' : 'div', { class: 'bar', role: 'listitem', href: i.href || null, title: i.title || null },
        h('span', { class: 'bar-val' }, i.text ?? ''), h('span', { class: 'bar-track' }, fill), h('span', { class: 'bar-label' }, i.label));
    }));
}

/**
 * A chart over a number, not time (power against battery level, use against speed): xyChart(ctx, body, { xs, xKind,
 * xUnit, xRange: [min, max], series: [{ name, kind, values, tone, dashed, points }], dots: [{ x, y, tone }], dotsName,
 * yKind, yFloor, label, cover, onDot(dot) }). xs is sorted and in API units; each series has one value per x (null
 * is a gap). dots are drawn as small round marks (a scatter) under the lines; their key shows how many there are.
 */
export async function xyChart(ctx, body, { xs = [], xKind = null, xUnit = null, xRange = null, series = [], dots = [], dotsName = 'Readings', dotsTone = 'c1', yKind = null, yFloor = 0, label = 'Chart', cover = null, onDot = null, short = false }) {
  const legend = h('div', { class: 'legend', role: 'group', 'aria-label': `${label}: lines`, cover });
  const box = h('div', { class: ['plot', { 'plot-short': short }], role: 'img', 'aria-label': `${label}.` });
  body.append(legend, box);
  const uPlot = await loadUplot();
  await document.fonts.ready;
  if (ctx.signal.aborted) return null;
  const cx = (v) => (v == null ? null : xKind ? convert(v, xKind) : v);
  const cy = (v) => (v == null || typeof v !== 'number' ? null : yKind ? convert(v, yKind) : v);
  const x = xs.map(cx);
  const lines = series.map((s, n) => ({ ...s, n, vals: (s.values || []).map(cy), shown: true }));
  const pts = dots.map((d) => ({ ...d, px: cx(d.x), py: cy(d.y) })).filter((d) => d.px != null && d.py != null);
  const yUnit = yKind ? unitLabel(yKind) : '';
  const xu = xUnit ?? (xKind ? unitLabel(xKind) : '');
  let dotsShown = true;
  const keys = [];
  if (pts.length) {
    const b = h('button', { type: 'button', class: ['key', 'key-dots', `tone-${dotsTone}`], 'aria-pressed': 'true', title: `Show or hide ${dotsName}`,
      onclick: () => { dotsShown = !dotsShown; b.setAttribute('aria-pressed', String(dotsShown)); plot?.redraw(false, false); } },
    h('span', { class: 'key-sw key-dot', 'aria-hidden': 'true' }), h('span', { class: 'key-name' }, dotsName), h('span', { class: 'key-val' }, formatNumber(pts.length)));
    keys.push(b);
  }
  for (const l of lines) {
    const val = h('span', { class: 'key-val' }, '—');
    const b = h('button', { type: 'button', class: ['key', `tone-${l.tone || 'c1'}`, { 'is-dashed': l.dashed }], 'aria-pressed': 'true', cover: l.cover, title: `Show or hide ${l.name}`,
      onclick: () => { l.shown = !l.shown; b.setAttribute('aria-pressed', String(l.shown)); plot?.setSeries(l.n + 1, { show: l.shown }); } },
    h('span', { class: 'key-sw', 'aria-hidden': 'true' }), h('span', { class: 'key-name' }, l.name), val);
    l.valEl = val;
    keys.push(b);
  }
  legend.append(...keys);
  const at = h('span', { class: 'key-at' });
  legend.append(at);
  const show = (i) => {
    at.textContent = i == null || x[i] == null ? '' : `at ${formatNumber(x[i], { maximumFractionDigits: 1 })}${xu ? ` ${xu}` : ''}`;
    for (const l of lines) {
      const v = i == null ? null : l.vals[i];
      l.valEl.textContent = v == null ? '—' : `${formatNumber(v, { maximumFractionDigits: Math.abs(v) < 10 ? 1 : 0 })}${yUnit ? ` ${yUnit}` : ''}`;
    }
  };
  show(null);
  let plot = null;
  function opts() {
    const k = display.scale;
    const px = (v) => Math.round(v * k * 10) / 10;
    const font = `500 ${Math.max(10, px(12))}px Manrope, system-ui, sans-serif`;
    const muted = cssVar('--muted'), grid = cssVar('--chart-grid'), ground = cssVar('--chart-ground');
    const allX = [...x.filter((v) => v != null), ...pts.map((d) => d.px)];
    const rx = xRange ? { min: cx(xRange[0]), max: cx(xRange[1]) } : niceRange(allX, { ticks: 5 });
    if (!xRange) rx.step = niceStep(rx.max - rx.min, 5);
    const ry = niceRange([...lines.flatMap((l) => l.vals), ...pts.map((d) => d.py)], { floor: yFloor, ticks: 4 });
    const sx = { min: rx.min, max: rx.max, step: xRange ? niceStep(rx.max - rx.min, 5) : rx.step };
    const dx = digitsOf(sx.step), dy = digitsOf(ry.step);
    return {
      width: Math.max(1, Math.floor(box.clientWidth)), height: Math.max(1, Math.floor(box.clientHeight)),
      padding: [px(14), px(16), px(4), px(4)], legend: { show: false },
      scales: { x: { time: false, range: () => [sx.min, sx.max] }, y: { range: () => [ry.min, ry.max] } },
      axes: [
        { stroke: muted, font, size: px(30), gap: px(6), ticks: { show: false }, grid: { stroke: grid, width: 1 }, splits: () => splitsOf(sx),
          values: (u, v) => v.map((n) => `${formatNumber(n, { maximumFractionDigits: dx })}${xu && xu.length <= 2 ? ` ${xu}` : ''}`) },
        { stroke: muted, font, size: px(46), gap: px(6), ticks: { show: false }, grid: { stroke: grid, width: 1 }, splits: () => splitsOf(ry),
          values: (u, v) => v.map((n) => formatNumber(n, { maximumFractionDigits: dy })) },
      ],
      series: [{}, ...lines.map((l) => {
        const c = cssVar(`--${l.tone || 'c1'}`);
        return { show: l.shown, stroke: c, width: (l.points ? 0 : 2.5) * k, dash: l.dashed ? [5 * k, 4 * k] : undefined, spanGaps: true,
          points: l.points ? { show: true, size: 6 * k, stroke: c, fill: c, width: 0 } : { show: false } };
      })],
      cursor: { drag: { x: false, y: false }, points: { show: false }, y: false },
      hooks: {
        drawClear: [(u) => { const c = u.ctx; c.save(); c.fillStyle = ground; c.fillRect(u.bbox.left, u.bbox.top, u.bbox.width, u.bbox.height); c.restore(); }],
        drawAxes: [(u) => {
          if (!dotsShown || !pts.length) return;
          const c = u.ctx;
          const r = Math.max(1.5, 2.6 * k) * (u.ctx.canvas.width / Math.max(1, u.width));
          c.save();
          c.globalAlpha = 0.55;
          for (const d of pts) {
            c.fillStyle = cssVar(`--${d.tone || dotsTone}`);
            c.beginPath(); c.arc(u.valToPos(d.px, 'x', true), u.valToPos(d.py, 'y', true), r, 0, Math.PI * 2); c.fill();
          }
          c.restore();
        }],
        setCursor: [(u) => { const i = u.cursor.idx == null || u.cursor.left < 0 ? null : u.cursor.idx; show(i); }],
      },
    };
  }
  function build() {
    if (plot) plot.destroy();
    plot = new uPlot(opts(), [x.length ? x : [0], ...lines.map((l) => (l.vals.length ? l.vals : [null]))], box);
    if (onDot && pts.length) {
      plot.over.addEventListener('click', (e) => {
        const r = plot.over.getBoundingClientRect();
        const ex = e.clientX - r.left, ey = e.clientY - r.top;
        let best = null, bd = (12 * display.scale) ** 2;
        for (const d of pts) { const dd = (plot.valToPos(d.px, 'x') - ex) ** 2 + (plot.valToPos(d.py, 'y') - ey) ** 2; if (dd < bd) { bd = dd; best = d; } }
        if (best) onDot(best);
      });
    }
  }
  build();
  const ro = new ResizeObserver(() => {
    const w = Math.floor(box.clientWidth), hh = Math.floor(box.clientHeight);
    if (plot && w && hh && (w !== plot.width || hh !== plot.height)) plot.setSize({ width: w, height: hh });
  });
  ro.observe(box);
  const stopSize = display.onChange((c) => { if (c.rem) build(); });
  const stopTheme = onTheme(() => build());
  const destroy = () => { stopSize(); stopTheme(); ro.disconnect(); if (plot) { plot.destroy(); plot = null; } };
  ctx.onCleanup(destroy);
  return { get plot() { return plot; }, destroy };
}

/** A value for a readout: format(v, kind). */
export const fmt = (v, kind) => format(v, kind);
