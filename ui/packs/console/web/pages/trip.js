// Trip (/cars/{car}/trip?from=&to=): any stretch of time as numbered legs. A range line (the period
// picker plus shortcuts and a second car to set beside this one), the trip figures (GET /cars/{car}/trip),
// time spent and energy as text bars, the legs (every drive and charge that started in the range, in time
// order, numbered, with totals), the route on one map (GET /tracks), the state strip and the battery and
// elevation charts (drag to zoom both).

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { format, getUnits, convert, unitLabel } from '../shared/units.js';
import { formatTime, formatDuration, formatDateRange, dayKey, dayRange, addDays, startOf, DAY } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { head, section, kv, note } from '../core/ui.js';
import { carOptions } from '../core/carpick.js';
import { num, tag, date } from '../core/fmt.js';
import { table, cellBar } from '../core/table.js';
import { safeMap } from '../core/map.js';
import { loadUPlot, plot, line, timeAxis, valueAxis, cursor, readout } from '../core/chart.js';
import { stateStrip } from '../core/strip.js';
import { src, srcText, est as estTag } from '../core/source.js';
import { empty } from '../core/states.js';
import { rpx } from '../core/display.js';
import { placeLink } from './timeline.js';

export const id = 'trip';

const SPLIT = [['driving', 'driving', 'drive', '='], ['charging_dc', 'charging dc', 'charge', '+'], ['charging_ac', 'charging ac', 'charge', '+'], ['parked_awake', 'parked, awake', 'park', '-'], ['asleep', 'asleep', 'sleep', '.'], ['not_recorded', 'not recorded', 'gap', '?']];
const money = (list) => (list?.length ? list.map((x) => format(x.amount, 'money', { currency: x.currency })).join(' + ') : null);

export async function mount(ctx) {
  const { car, tz } = ctx;
  const cid = car.id;
  const enc = encodeURIComponent;
  const now = ctx.api.now();
  const rk = getUnits().range === 'ideal' ? 'ideal_range_km' : 'range_km';
  const vsId = ctx.query.vs && ctx.query.vs !== cid ? String(ctx.query.vs) : null;

  let from = ctx.period?.from_ms ?? null;
  const to = Math.min(ctx.period?.to_ms ?? now, now);
  if (from == null) {
    const d0 = await ctx.api.page(`cars/${enc(cid)}/drives`, { sort: 'start', dir: 'asc' }, { limit: 1 }).catch(() => ({ items: [] }));
    if (!ctx.alive()) return;
    from = d0.items?.[0]?.start_ms ?? to - 7 * DAY;
  }
  const win = { from_ms: from, to_ms: to };

  let trip, series, act, last3, fleet, drives, charges, vsTrip = null, vsCar = null;
  try {
    [trip, series, act, last3, fleet, drives, charges] = await Promise.all([
      ctx.api.get(`cars/${enc(cid)}/trip`, win),
      ctx.api.get(`cars/${enc(cid)}/series`, { fields: `soc_pct,${rk},elevation_m`, ...win, max_points: 1500 }).catch(() => null),
      ctx.api.get(`cars/${enc(cid)}/activity`, win).catch(() => ({ items: [] })),
      ctx.api.page(`cars/${enc(cid)}/drives`, { sort: 'start', dir: 'desc' }, { limit: 3 }).catch(() => ({ items: [] })),
      ctx.api.page('cars', { sort: 'name' }, { limit: 100 }).catch(() => ({ items: [car] })),
      ctx.api.list(`cars/${enc(cid)}/drives`, { ...win, sort: 'start', dir: 'asc' }, { limit: 200 }).all({ max: 1000 }),
      ctx.api.list(`cars/${enc(cid)}/charges`, { ...win, sort: 'start', dir: 'asc' }, { limit: 200 }).all({ max: 1000 }),
    ]);
    if (vsId) [vsCar, vsTrip] = await Promise.all([ctx.api.get(`cars/${enc(vsId)}`).catch(() => null), ctx.api.get(`cars/${enc(vsId)}/trip`, win).catch(() => null)]);
  } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  ctx.setTitle(`Trip ${formatDateRange(from, Math.max(from, to - 1), tz, { year: false })}`);

  /* ---------- the range line */
  const periodLink = (period) => state.buildUrl('trip', { car: cid, period, query: vsId ? { vs: vsId } : null });
  const l3 = last3.items || [];
  const l3From = l3.length ? l3[l3.length - 1].start_ms : null;
  const today = dayKey(now, tz);
  const vsSel = h('select', { class: 'in', 'aria-label': 'Set a second car beside this one' }, h('option', { value: '' }, 'vs: no second car'),
    carOptions((fleet.items || []).filter((c) => c.id !== cid), (c) => h('option', { value: c.id, selected: c.id === vsId }, `vs: ${c.name}`)));
  vsSel.addEventListener('change', () => ctx.setQuery({ vs: vsSel.value || null }) && ctx.reload());
  const rangeLine = h('div', { class: 'optline' },
    h('span', { class: 'optline-k' }, 'range>'),
    h('span', { class: 'keys' },
      l3From != null ? h('a', { class: 'key', href: periodLink({ from: l3From, to: null }), cover: 'trip.link.last_three_drives' }, l3.length === 3 ? 'last 3 drives' : `last ${l3.length}`)
        : h('button', { class: 'key', type: 'button', disabled: true, cover: 'trip.link.last_three_drives' }, 'last 3 drives'),
      h('a', { class: 'key', href: periodLink({ from: dayRange(today, today, tz).from, to: null }) }, 'today'),
      h('a', { class: 'key', href: periodLink(dayRange(addDays(today, -1), addDays(today, -1), tz)) }, 'yesterday'),
      h('a', { class: 'key', href: periodLink({ from: startOf(now, 'month', tz), to: null }) }, 'this month')),
    h('span', { class: 'filt-sel', cover: 'trip.control.per_car' }, vsSel));

  /* ---------- figures */
  const nD = drives.length, nC = charges.length;
  const tm = trip.time_ms || {}, dist = trip.distance || {}, en = trip.energy_kwh || {}, cost = trip.cost || {};
  const noDrive = !nD;
  const blank = (w) => h('span', { class: 'muted' }, w);
  const figs = (t, label) => kv([
    ['distance', noDrive && !t.distance?.odometer_km ? blank('no drive') : [num(t.distance?.odometer_km, 'distance'), h('span', { class: 'muted' }, ` ${formatNumber(t.drives?.length ?? 0)} drives`)], { cover: label ? null : 'trip.stat.mileage' }],
    ['avg, moving', t.speed_kph?.moving != null ? num(t.speed_kph.moving, 'speed') : blank('no drive'), { cover: label ? null : 'trip.stat.speed_moving' }],
    ['avg, with dc', t.speed_kph?.incl_dc != null ? num(t.speed_kph.incl_dc, 'speed') : blank('no drive'), { cover: label ? null : 'trip.stat.speed_incl_dc' }],
    ['net use', t.wh_per_km?.net != null ? num(t.wh_per_km.net, 'efficiency') : blank('too little distance'), { cover: label ? null : 'trip.stat.consumption_net' }],
    ['gross use', t.wh_per_km?.gross != null ? num(t.wh_per_km.gross, 'efficiency') : blank('too little distance'), { cover: label ? null : 'trip.stat.consumption_gross' }],
    ['charging cost', money(t.cost?.by_currency) || blank(t.charges?.length ? 'no price yet' : 'no charge'), { cover: label ? null : 'trip.stat.charging_cost' }],
    ['cost per 100', money(t.cost?.per_100km) ? [money(t.cost.per_100km), h('span', { class: 'muted' }, ` per 100 ${unitLabel('distance')}`)] : blank('no price yet'), { cover: label ? null : 'trip.stat.cost_per_100' }],
    ['charges', `${formatNumber(t.charges?.length ?? 0)}${t.cost?.unpriced ? ` (${formatNumber(t.cost.unpriced)} not priced)` : ''}`],
  ], { cols: label ? 1 : 2 });

  const splitTotal = SPLIT.reduce((s, [k]) => s + (tm[k] || 0), 0) || 1;
  // one character per cell, as many cells as fit (filled once the page is laid out)
  const timeStrip = h('div', { class: 'strip-cells', role: 'img', 'aria-label': SPLIT.map(([k, l]) => `${l} ${formatDuration(tm[k] || 0)}`).join(', ') });
  const fillTimeStrip = () => {
    const adv = parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--a2')) || 8;
    const cells = Math.max(8, Math.floor(timeStrip.clientWidth / adv));
    let used = 0;
    replace(timeStrip, SPLIT.map(([k, , cl, c], i) => { const n = Math.max(0, i === SPLIT.length - 1 ? cells - used : Math.min(cells - used, Math.round(((tm[k] || 0) / splitTotal) * cells))); used += n; return h('span', { class: `strip-${cl}` }, c.repeat(n)); }));
  };
  const timeEl = section({ title: 'time spent', cover: 'trip.chart.time_spent' },
    timeStrip,
    h('div', { class: 'bars' }, SPLIT.map(([k, l, cl, c]) => h('div', { class: 'bar-row bar-row--2' }, h('span', { class: 'bar-k' }, h('span', { class: `strip-${cl}` }, c), ` ${l}`),
      h('span', { class: 'bar-v' }, formatDuration(tm[k] || 0), h('span', { class: 'pct' }, `${formatNumber(((tm[k] || 0) / splitTotal) * 100, { maximumFractionDigits: 0 })}%`))))),
    note('Parked time is split into awake and asleep; time with no log is "not recorded", never counted as sleep.'));
  const eRows = [['used in all', en.gross, 'parked time included'], ['used driving', en.drive_net, 'regen taken off'], ['added ac', en.added_ac, 'home and public ac'], ['added dc', en.added_dc, 'fast chargers']];
  const eMax = Math.max(1e-6, ...eRows.map((r) => r[1] || 0));
  const energyEl = section({ title: 'energy', cover: 'trip.chart.energy' },
    h('div', { class: 'bars' }, eRows.map(([l, v, ex]) => h('div', { class: 'bar-row' }, h('span', { class: 'bar-k' }, l, h('span', { class: 'muted' }, ` ${ex}`)), cellBar((v || 0) / eMax, 24), h('span', { class: 'bar-v bar-v--w' }, num(v, 'energy'))))),
    note('Measured at the pack; energy added counts charges that started in the range.'));

  /* ---------- the legs */
  const legs = [...drives.map((d) => ({ kind: 'drive', t: d.start_ms, d })), ...charges.map((c) => ({ kind: 'charge', t: c.start_ms, c }))].sort((a, b) => a.t - b.t);
  legs.forEach((l, i) => { l.n = i + 1; });
  const sumOf = (f) => legs.reduce((s, l) => s + (f(l) || 0), 0);
  const legHref = (l) => (l.kind === 'drive' ? ctx.link('drive', { drive: l.d.id }) : ctx.link('charge', { charge: l.c.id }));
  const legsT = table(ctx, {
    title: 'legs', rows: legs, csv: false, sort: null,
    cover: ['trip.drives.table', 'trip.charges.table'],
    rowHref: legHref,
    columns: [
      { key: 'n', label: '#', num: true, render: (l) => h('span', { class: 'muted' }, String(l.n)), total: () => 'total' },
      { key: 'kind', label: 'leg', ph: 'main', render: (l) => (l.kind === 'drive' ? tag('drive', 'drive') : tag('charge', 'charge')) },
      { key: 'start', label: 'start', ph: 'main', cover: ['trip.drives.date', 'trip.drives.link.details', 'trip.charges.date', 'trip.charges.link.details'], render: (l) => h('a', { class: 'lnk-plain', href: legHref(l) }, `${date(l.t, tz, { year: false })} ${formatTime(l.t, tz)}`) },
      { key: 'where', label: 'from / at', cls: 'wrap-cell', ph: 'sub', cover: ['trip.drives.start', 'trip.drives.link.geofence_start', 'trip.charges.location', 'trip.charges.link.geofence'],
        render: (l) => (l.kind === 'drive' ? placeLink(l.d.start) : placeLink({ place: l.c.place, address: l.c.address, lat: l.c.position?.lat, lon: l.c.position?.lon })) },
      { key: 'to', label: 'to', cls: 'wrap-cell', ph: 'sub', cover: ['trip.drives.destination', 'trip.drives.link.geofence_end'],
        render: (l) => (l.kind === 'drive' ? (l.d.flags?.in_progress ? h('span', { class: 'tone--accent' }, 'driving now') : placeLink(l.d.end)) : false) },
      { key: 'dur', label: 'time', num: true, cover: ['trip.drives.duration', 'trip.drives.link.duration_hint', 'trip.charges.duration'], title: 'drive time; for a charge, the time charging',
        render: (l) => formatDuration(l.kind === 'drive' ? l.d.duration_ms : l.c.charging_ms), total: () => formatDuration(sumOf((l) => (l.kind === 'drive' ? l.d.duration_ms : l.c.charging_ms))) },
      { key: 'dist', label: 'dist', num: true, ph: 'num', cover: 'trip.drives.distance', render: (l) => (l.kind === 'drive' ? num(l.d.distance_km, 'distance') : false), total: () => num(sumOf((l) => l.d?.distance_km), 'distance') },
      { key: 'soc', label: 'soc', num: true, cover: ['trip.drives.soc_start', 'trip.drives.soc_end', 'trip.charges.soc_start', 'trip.charges.soc_end'],
        render: (l) => { const x = l.d || l.c; return h('span', null, format(x.start_soc_pct, 'percent'), h('span', { class: 'arrow' }, '->'), format(x.end_soc_pct, 'percent')); } },
      { key: 'use', label: 'use', num: true, cover: 'trip.drives.consumption', render: (l) => (l.kind === 'drive' ? num(l.d.wh_per_km, 'efficiency') : false) },
      { key: 'added', label: 'added', num: true, ph: 'num', cover: 'trip.charges.energy_added', render: (l) => (l.kind === 'charge' ? [num(l.c.energy_added_kwh, 'energy'), estTag(l.c)] : false), total: () => num(sumOf((l) => l.c?.energy_added_kwh), 'energy') },
      { key: 'grid', label: 'grid', num: true, cover: 'trip.charges.energy_used', title: 'energy drawn, losses included (ac only)', render: (l) => (l.kind === 'charge' ? num(l.c.energy_grid_kwh, 'energy') : false) },
      { key: 'power', label: 'avg kW', num: true, cover: 'trip.charges.power_avg', render: (l) => (l.kind === 'charge' ? num(l.c.power_avg_kw, 'power') : false) },
      { key: 'range', label: 'range+', num: true, cover: 'trip.charges.range_added', render: (l) => (l.kind === 'charge' ? num(l.c.range_added_km, 'range') : false) },
      { key: 'cost', label: 'cost', num: true, cover: ['trip.charges.cost', 'trip.charges.link.cost'],
        render: (l) => (l.kind === 'charge' ? (l.c.cost ? format(l.c.cost.amount, 'money', { currency: l.c.cost.currency }) : h('a', { class: 'lnk', href: legHref(l), title: 'set a price for this charge' }, 'set')) : false),
        total: () => money(cost.by_currency) },
      { key: 'from', label: 'data', title: 'where the data came from: smo, api (the Tesla API), imp (imported)', render: (l) => src(l.d || l.c), value: (l) => srcText(l.d || l.c) },
    ],
    empty: () => empty('no drives or charges in this range', 'pick a wider range or a shortcut above'),
  });

  /* ---------- map, strip, charts */
  const mapEl = h('div', { class: 'map', role: 'img', 'aria-label': 'Map of the trip' });
  const battEl = h('div', { class: 'chart chart--pane' });
  const elevEl = h('div', { class: 'chart chart--pane' });
  const read = readout('move over a chart for values; drag to zoom both');
  const sync = `trip-${cid}`;

  replace(ctx.view, h('div', { class: 'page', dataset: { page: 'trip' }, cover: 'shots.trip' },
    (() => { const hd = head(ctx, { title: 'Trip' }); const b = hd.querySelector('.ph-btn'); if (b) b.dataset.cover = 'trip.filter.time_range'; return hd; })(),
    rangeLine,
    section({ title: 'figures', cover: 'trip.empty_states', side: h('span', { class: 'muted' }, formatDateRange(from, Math.max(from, to - 1), tz, { time: true })) },
      noDrive ? empty('nothing was driven in this range', 'pick a wider range or a shortcut above') : null,
      vsId ? h('div', { class: 'cols2' }, h('div', null, h('h3', { class: 'sub-title' }, car.name), figs({ ...trip, drives: trip.drives, charges: trip.charges })),
        h('div', null, h('h3', { class: 'sub-title' }, vsCar?.name || 'second car'), vsTrip ? figs(vsTrip, true) : empty('the second car could not be loaded')))
        : figs(trip)),
    h('div', { class: 'cols2' }, timeEl, energyEl),
    legsT.el,
    section({ title: 'route', cover: 'trip.map.route', side: h('span', { class: 'muted' }, `${formatNumber(nD)} drives, ${formatNumber(nC)} charges; square: a charge`) }, mapEl),
    section({ title: 'states', cover: 'trip.chart.states' }, stateStrip(ctx, act.items || [], from, to, tz)),
    section({ title: 'battery and elevation', cover: 'trip.control.zoom', side: series ? src(series) : null },
      h('div', { class: 'pane', cover: ['trip.chart.battery.soc', 'trip.chart.battery.range'] }, h('div', { class: 'chart-head' }, h('b', null, 'battery'), h('span', null, `% / ${unitLabel('range')}`),
        h('span', { class: 'chart-key' }, h('span', { class: 'chart-key-i chart-key-i--1' }, '── soc'), h('span', { class: 'chart-key-i chart-key-i--2' }, `┄┄ ${rk === 'ideal_range_km' ? 'ideal' : 'rated'}`))), battEl),
      h('div', { class: 'pane', cover: 'trip.chart.elevation' }, h('div', { class: 'chart-head' }, h('b', null, 'elevation'), h('span', null, unitLabel('length'))), elevEl),
      read.el)));

  fillTimeStrip();
  // charts
  const xs = series?.t_ms || [from, to];
  // asleep keeps the line going (nothing changes while the car sleeps); no data breaks it
  const vals = (k, kind) => { let last = null; return (series?.values?.[k] || [null, null]).map((v, i) => { if (v != null) { last = kind ? convert(v, kind) : v; return last; } return series?.state?.[i] === 'asleep' || series?.state?.[i] === 'parked' ? last : (last = null); }); };
  const soc = vals('soc_pct'), range = vals(rk, 'range'), elev = vals('elevation_m', 'length');
  const plots = [];
  const setZoom = (a, b) => { for (const p of plots) p.u?.setScale('x', { min: a, max: b }); };
  const onCursor = (i) => read.show(i == null ? null : [['at', `${date(xs[i], tz, { year: false })} ${formatTime(xs[i], tz)}`], ['soc', soc[i] == null ? '—' : `${formatNumber(soc[i], { maximumFractionDigits: 0 })}%`],
    ['range', range[i] == null ? '—' : `${formatNumber(range[i], { maximumFractionDigits: 0 })} ${unitLabel('range')}`], ['elevation', elev[i] == null ? '—' : `${formatNumber(elev[i], { maximumFractionDigits: 0 })} ${unitLabel('length')}`]]);
  try { await loadUPlot(); } catch { /* the boxes stay empty */ }
  if (!ctx.alive()) return;
  if (window.uPlot) {
    const mk = (last, scales, axes, ser) => () => ({
      padding: [rpx(6), 0, last ? 0 : rpx(8), 0], cursor: { ...cursor(sync), drag: { x: true, y: false, setScale: false } }, select: { show: true },
      scales: { x: { time: true, min: from, max: to }, ...scales },
      axes: [last ? timeAxis(tz) : { ...timeAxis(tz), size: 0, gap: 0, values: (u, sp) => sp.map(() => '') }, ...axes],
      series: [{}, ...ser],
      hooks: { setSelect: [(u) => { if (u.select.width > 2) { const a = u.posToVal(u.select.left, 'x'), b = u.posToVal(u.select.left + u.select.width, 'x'); u.setSelect({ width: 0, height: 0 }, false); setZoom(a, b); } }] },
    });
    const yR = { show: true, side: 1, size: rpx(52), grid: { show: false }, ticks: { show: false }, values: () => [] };
    plots.push(plot(battEl, ctx, mk(false, { pct: { range: [0, 100] }, dist: { auto: true } },
      [valueAxis((v) => `${formatNumber(v)}%`, { scale: 'pct', size: 52 }), valueAxis((v) => formatNumber(v), { scale: 'dist', side: 1, grid: false, size: 52 })],
      [line('soc', 0, { scale: 'pct' }), line('range', 1, { scale: 'dist' })]), [xs, soc, range], { onCursor }));
    plots.push(plot(elevEl, ctx, mk(true, { y: { auto: true } }, [valueAxis((v) => formatNumber(v), { size: 52 }), yR], [line('elevation', 0)]), [xs, elev], { onCursor }));
    elevEl.addEventListener('dblclick', () => setZoom(from, to));
    battEl.addEventListener('dblclick', () => setZoom(from, to));
  }

  // map
  let tracks = null;
  const cars = [cid, vsCar && vsTrip ? vsId : null].filter(Boolean).join(',');
  try { tracks = await ctx.api.get('tracks', { cars, ...win, max_points: Math.min(200000, Math.max(12000, nD * 6)) }); } catch { tracks = null; }
  if (!ctx.alive()) return;
  const lines = (car2) => (tracks?.drives || []).filter((d) => (d.car === cid) !== car2).map((d) => d.lon.map((lo, i) => [lo, d.lat[i]]).filter((p) => p[0] != null && p[1] != null)).filter((c) => c.length > 1);
  const mine = lines(false), theirs = lines(true);
  const marks = (trip.markers || []).filter((k) => k.lat != null && k.lon != null);
  const all = [...mine.flat(), ...theirs.flat(), ...marks.map((k) => [k.lon, k.lat])];
  if (!all.length) mapEl.append(h('p', { class: 'map-note' }, nD ? '-- no positions recorded for these drives --' : '-- nothing was driven in this range --'));
  else {
    const m = await safeMap(mapEl, ctx, { center: all[0], zoom: 11 });
    if (m && ctx.alive()) {
      if (theirs.length) m.line('vs', theirs, { token: '--c2', width: 2, dash: [2, 1] });
      if (mine.length) m.line('route', mine);
      for (const c of charges) if (c.position?.lat != null) m.marker([c.position.lon, c.position.lat], 'map-pin map-pin--warn');
      if (mine.length) { m.marker(mine[0][0], 'map-pin map-pin--a'); const lst = mine[mine.length - 1]; m.marker(lst[lst.length - 1], 'map-pin map-pin--b'); }
      m.fit(all, { pad: 32 });
    }
  }
  if (!ctx.alive()) return;
  ctx.ready();
}
