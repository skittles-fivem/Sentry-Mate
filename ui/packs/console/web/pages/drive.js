// Drive (/cars/{car}/drives/{drive}): one drive. A header line of stats, the route on a flat grey map
// (accent line, A and B pins, a square that follows the chart cursor), then stacked uPlot panes on one
// x-axis and one cursor (speed, power, battery, elevation, climate, battery heating, tyres). Drag across a
// pane to zoom every pane to that stretch and see its figures; [all] zooms out. Time at each speed as text
// bars, GPX / GeoJSON export, and the table of samples on demand.

import { h, replace } from '../shared/dom.js';
import { format, getUnits, convert, unitLabel, KM_PER_MI } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, zoneName, zonedParts } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { head, section, note, link } from '../core/ui.js';
import { num, tag } from '../core/fmt.js';
import { safeMap } from '../core/map.js';
import { loadUPlot, plot, line, timeAxis, valueAxis, cursor, readout } from '../core/chart.js';
import { table, cellBar } from '../core/table.js';
import { empty } from '../core/states.js';
import { rpx } from '../core/display.js';
import { placeLink } from './timeline.js';

export const id = 'drive';
export const title = () => 'Drive';

const exportName = (carName, ms, tz) => {
  const p = zonedParts(ms, tz);
  const z = (n) => String(n).padStart(2, '0');
  return `${String(carName).replace(/[^A-Za-z0-9._-]+/g, '-').replace(/^-+|-+$/g, '') || 'car'}-drive-${p.year}-${z(p.month)}-${z(p.day)}-${z(p.hour)}${z(p.minute)}`;
};

export async function mount(ctx) {
  const { car, tz } = ctx;
  const cid = car.id, did = ctx.state.drive;
  const base = `cars/${encodeURIComponent(cid)}/drives/${encodeURIComponent(did)}`;
  const mph = getUnits().distance === 'mi';
  let d;
  try { d = await ctx.api.get(base, { speed_band_kph: mph ? 10 * KM_PER_MI : 10 }); } catch (e) {
    if (e.status === 404) e.message = 'There is no drive with this number for this car. It may have been rebuilt under a new number; find it on drives.';
    ctx.fail(e, { retry: null }); return;
  }
  if (!ctx.alive()) return;
  const live = !!d.flags?.in_progress;
  const t0 = d.start_ms, t1 = d.end_ms ?? ctx.api.now();
  const ideal = getUnits().range === 'ideal';
  const rk = ideal ? 'ideal_range_km' : 'range_km';
  const fields = ['speed_kph', 'power_kw', 'soc_pct', rk, 'elevation_m', 'outside_c', 'cabin_c', 'setpoint_left_c', 'odometer_km', 'tyre_fl_kpa', 'tyre_fr_kpa', 'tyre_rl_kpa', 'tyre_rr_kpa', 'lat', 'lon', 'battery_heat_kw', 'climate', 'fan_level'];
  let series, track;
  try {
    [series, track] = await Promise.all([
      ctx.api.get(`cars/${encodeURIComponent(cid)}/series`, { fields: fields.join(','), from_ms: t0, to_ms: t1 + 1000, max_points: 3600 }),
      ctx.api.get(`${base}/track`, { extensions: 'speed,power' }).catch(() => null),
    ]);
  } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  ctx.setTitle(`Drive ${formatDate(t0, tz, { weekday: false, year: false })} ${formatTime(t0, tz)}`);

  const V = (k, kind) => (series.values[k] || []).map((v) => (v == null || typeof v !== 'number' ? null : kind ? convert(v, kind) : v));
  const xs = series.t_ms;
  const est = d.energy_basis === 'estimated';
  const why = live ? 'still driving' : 'not recorded';
  const r0 = ideal ? d.start_ideal_range_km : d.start_range_km, r1 = ideal ? d.end_ideal_range_km : d.end_range_km;

  /* ---------- head and the stat line */
  const hd = head(ctx, { title: 'Drive', period: false, extra: h('span', { class: 'ph-extra muted', cover: 'dd.context.drive' },
    `${formatDate(t0, tz, { weekday: false })} ${formatTime(t0, tz)} - ${live ? 'now' : formatTime(t1, tz)} ${zoneName(t0, tz)}`) });
  const carBtn = hd.querySelector('.ph-btn');
  if (carBtn) carBtn.dataset.cover = 'dd.filter.car';
  const st = (k, v, cover = null) => h('span', { class: 'stat', cover }, h('span', { class: 'stat-k' }, k), v);
  const m = (v, kind, w = why) => (v == null ? h('span', { class: 'muted' }, w) : num(v, kind));
  const statsEl = h('div', { class: 'stats' });
  const paintStats = () => {
    const dur = live ? ctx.api.now() - t0 : d.duration_ms;
    const avg = live ? (dur > 0 ? d.distance_km / (dur / 3.6e6) : null) : d.speed_avg_kph;
    replace(statsEl,
      st('distance', m(d.distance_km, 'distance'), 'dd.stat.distance'),
      st('time', formatDuration(dur), 'dd.stat.drive_duration'),
      st('avg', m(avg, 'speed'), 'dd.stat.speed_avg'),
      st('top', m(d.speed_max_kph, 'speed')),
      st('energy', [m(d.energy_kwh, 'energy'), est ? h('span', { class: 'tone--warn' }, ' est') : null], 'dd.stat.energy_net'),
      st('regen', m(d.regen_kwh, 'energy', est ? 'not in imported history' : why), 'dd.stat.energy_recovered'),
      st('use', [m(d.wh_per_km, 'efficiency', d.distance_km < 0.5 ? 'too short' : why), d.efficiency_pct != null ? h('span', { class: 'muted' }, ` ${format(d.efficiency_pct, 'percent')} of rated`) : null], 'dd.stat.consumption_net'),
      st('peak', [m(d.power_max_kw, 'power'), d.power_min_kw != null ? h('span', { class: 'muted' }, ` regen ${format(Math.abs(d.power_min_kw), 'power')}`) : null]),
      st('climb', m(d.climb_m, 'length'), 'dd.stat.elevation_up'),
      st('descent', m(d.descent_m, 'length'), 'dd.stat.elevation_down'),
      st('out', m(d.outside_avg_c, 'temperature')),
      st('in', m(d.cabin_avg_c, 'temperature', 'not sent')),
      st('soc', h('span', null, format(d.start_soc_pct, 'percent'), h('span', { class: 'arrow' }, ' -> '), format(d.end_soc_pct, 'percent'))),
      st(ideal ? 'ideal' : 'rated', h('span', null, format(r0, 'range'), h('span', { class: 'arrow' }, ' -> '), format(r1, 'range'))),
      st('odo', h('span', null, format(d.start_odometer_km, 'odometer'), h('span', { class: 'arrow' }, ' -> '), format(d.end_odometer_km, 'odometer')), 'dd.stat.odometer'),
      st('climate', m(d.hvac_kwh, 'energy', 'not sent')));
  };
  paintStats();
  if (live) {
    ctx.events('rows', (ev) => { if (ev.data?.car === cid && (ev.data.kinds || []).includes('drives')) ctx.reload(); });
    ctx.events('now', (ev) => {
      const dr = ev.data?.car === cid ? ev.data.now?.drive : null;
      if (!dr || (dr.drive_id != null && dr.drive_id !== did)) return;
      if (dr.distance_km != null) d.distance_km = dr.distance_km;
      if (dr.energy_kwh != null) d.energy_kwh = dr.energy_kwh;
      paintStats();
    });
  }

  /* ---------- route and export */
  const exp = { speed: true, power: false, soc: false, accuracy: true };
  const expUrl = (fmt) => ctx.api.url(`${base}/track`, { format: fmt, extensions: Object.keys(exp).filter((k) => exp[k]).join(',') || null });
  const fileBase = exportName(car.name, t0, tz);
  const gpxA = h('a', { class: 'lnk', download: `${fileBase}.gpx`, href: expUrl('gpx'), cover: ['dd.export.gpx', 'web.drive-gpx.entry', 'web.drive-gpx.file', 'web.drive-gpx.point-coordinates', 'web.drive-gpx.point-elevation', 'web.drive-gpx.point-time', 'web.drive-gpx.not-found'] }, 'gpx');
  const geoA = h('a', { class: 'lnk', download: `${fileBase}.geojson`, href: expUrl('geojson') }, 'geojson');
  const expKeys = h('span', { class: 'keys' }, Object.keys(exp).map((k) => h('button', { class: 'key', type: 'button', 'aria-pressed': String(exp[k]), title: `put ${k} in the file`,
    onclick: (e) => { exp[k] = !exp[k]; e.currentTarget.setAttribute('aria-pressed', String(exp[k])); gpxA.href = expUrl('gpx'); geoA.href = expUrl('geojson'); } }, k)));
  const mapEl = h('div', { class: 'map map--lg', role: 'img', 'aria-label': 'Map of the route' });
  const nav = h('span', { class: 'drive-nav' },
    d.prev ? link(ctx.link('drive', { drive: d.prev }), '< previous') : h('span', { class: 'muted' }, '< previous'), '  ',
    d.next ? link(ctx.link('drive', { drive: d.next }), 'next >') : h('span', { class: 'muted' }, 'next >'));

  /* ---------- the panes */
  const fan = V('fan_level');
  const climOn = (series.values.climate || []).map((v) => (v == null ? null : v === 'off' ? 0 : 1));
  const tyres = ['fl', 'fr', 'rl', 'rr'].map((k) => V(`tyre_${k}_kpa`, 'pressure'));
  const has = (a) => a.some((v) => v != null);
  const PANES = [
    { key: 'speed', name: 'speed', unit: unitLabel('speed'), cover: 'dd.chart.drive.speed', s: [['speed', V('speed_kph', 'speed'), 'speed']] },
    { key: 'power', name: 'power', unit: 'kW', cover: 'dd.chart.drive.power', note: 'below 0 is regen', s: [['power', V('power_kw'), 'power']] },
    { key: 'soc', name: 'battery', unit: `% / ${unitLabel('range')}`, cover: ['dd.chart.drive.soc', 'dd.chart.drive.range'], two: true, s: [['soc', V('soc_pct'), 'percent'], [ideal ? 'ideal' : 'rated', V(rk, 'range'), 'range', true]] },
    { key: 'elev', name: 'elevation', unit: unitLabel('length'), cover: 'dd.chart.elevation', s: [['elevation', V('elevation_m', 'length'), 'length']] },
    { key: 'temps', name: 'climate', unit: unitLabel('temperature'), cover: ['dd.chart.temps.air', 'dd.chart.temps.setpoints'], s: [['outside', V('outside_c', 'temperature'), 'temperature'], ['inside', V('cabin_c', 'temperature'), 'temperature'], ['set', V('setpoint_left_c', 'temperature'), 'temperature']] },
    { key: 'hvac', name: 'climate system', unit: 'fan 0-10', min0: true, cover: 'dd.chart.temps.climate_state', s: [['fan', fan, null], ['on', climOn.map((v) => (v == null ? null : v * 10)), null]] },
    { key: 'heat', name: 'battery heating', unit: 'kW', min0: true, cover: 'dd.chart.drive.battery_heater', s: [['heater', V('battery_heat_kw'), 'power']] },
    { key: 'tyres', name: 'tyres', unit: unitLabel('pressure'), cover: 'dd.chart.tire_pressure', s: [['fl', tyres[0], 'pressure_c'], ['fr', tyres[1], 'pressure_c'], ['rl', tyres[2], 'pressure_c'], ['rr', tyres[3], 'pressure_c']] },
  ];
  const fmtV = (v, kind) => (v == null ? '—' : kind === 'pressure_c' ? `${formatNumber(v, { maximumFractionDigits: 2 })} ${unitLabel('pressure')}` : kind ? format(kind === 'speed' || kind === 'range' || kind === 'length' || kind === 'temperature' ? null : v, kind) : formatNumber(v, { maximumFractionDigits: 1 }));
  // values are converted already: show them with their unit, not converted again
  const shown = (v, kind) => {
    if (v == null) return '—';
    if (!kind) return formatNumber(v, { maximumFractionDigits: 1 });
    if (kind === 'pressure_c') return `${formatNumber(v, { maximumFractionDigits: 2 })} ${unitLabel('pressure')}`;
    if (['speed', 'range', 'length', 'temperature'].includes(kind)) return `${formatNumber(v, { maximumFractionDigits: kind === 'temperature' ? 1 : 0 })} ${unitLabel(kind)}`;
    return format(v, kind);
  };
  const read = readout('move over a pane for values; drag across it to zoom');
  const selLine = h('p', { class: 'sel-line', cover: ['dd.stat.selected_duration', 'dd.control.zoom'] });
  const panesEl = h('div', { class: 'panes' });
  const plots = [];
  let cursorPin = null;
  const lat = series.values.lat || [], lon = series.values.lon || [];
  const sync = `drive-${did}`;
  let zoom = null;
  const paintSel = () => {
    if (!zoom) { replace(selLine, h('span', { class: 'muted' }, 'the whole drive; drag across a pane to zoom')); return; }
    const [a, b] = zoom;
    const i0 = xs.findIndex((t) => t >= a);
    let i1 = xs.length - 1;
    while (i1 > 0 && xs[i1] > b) i1--;
    const odo = series.values.odometer_km || [];
    const dist = odo[i1] != null && odo[i0] != null ? odo[i1] - odo[i0] : null;
    const sp = V('speed_kph', 'speed').slice(i0, i1 + 1).filter((v) => v != null);
    replace(selLine, h('span', { class: 'muted' }, 'selected '), `${formatTime(a, tz)} - ${formatTime(b, tz)}`, h('span', { class: 'muted' }, '  time '), formatDuration(b - a),
      h('span', { class: 'muted' }, '  distance '), dist != null ? num(dist, 'distance') : '—',
      h('span', { class: 'muted' }, '  top '), sp.length ? `${formatNumber(Math.max(...sp), { maximumFractionDigits: 0 })} ${unitLabel('speed')}` : '—', '  ',
      h('button', { class: 'btn btn--line tone--accent', type: 'button', onclick: () => setZoom(null) }, '[all]'));
  };
  const setZoom = (z) => {
    zoom = z;
    for (const p of plots) p.u?.setScale('x', { min: z ? z[0] : t0, max: z ? z[1] : t1 });
    paintSel();
  };
  paintSel();

  const blocks = PANES.filter((p) => p.s.some(([, a]) => has(a))).map((p) => ({ ...p, el: h('div', { class: 'chart chart--pane' }) }));
  const missing = PANES.filter((p) => !blocks.some((b) => b.key === p.key));
  replace(panesEl, blocks.map((b) => h('div', { class: 'pane', cover: b.cover },
    h('div', { class: 'chart-head' }, h('b', null, b.name), h('span', null, b.unit), b.note ? h('span', null, b.note) : null,
      h('span', { class: 'chart-key' }, b.s.length > 1 ? b.s.map(([n], k) => h('span', { class: `chart-key-i chart-key-i--${k + 1}` }, `${['──', '┄┄', '┈┈'][k]} ${n}`)) : null)),
    b.el)),
  missing.length ? h('p', { class: 'note', cover: missing.flatMap((p) => [].concat(p.cover)) }, `not sent by this car on this drive: ${missing.map((p) => p.name).join(', ')}`) : null);

  const onCursor = (i) => {
    if (i == null) { read.show(null); cursorPin?.getElement().classList.add('is-off'); return; }
    read.show([['at', formatTime(xs[i], tz)], ...blocks.flatMap((b) => b.s.slice(0, 2).map(([n, a, kind]) => [n, shown(a[i], kind)]))]);
    if (cursorPin && lat[i] != null) { cursorPin.setLngLat([lon[i], lat[i]]); cursorPin.getElement().classList.remove('is-off'); }
  };

  /* ---------- time at each speed */
  const bands = (d.speed_bands || []).filter((b) => b.seconds > 0);
  const total = bands.reduce((s, b) => s + b.seconds, 0) || 1;
  const maxB = Math.max(1e-9, ...bands.map((b) => b.seconds));
  const bandsEl = section({ title: 'time at each speed', cover: 'dd.chart.speed_histogram' },
    bands.length ? h('div', { class: 'bars' }, bands.map((b) => h('div', { class: 'bar-row' },
      h('span', { class: 'bar-k' }, `${formatNumber(convert(b.from_kph, 'speed'), { maximumFractionDigits: 0 })}-${format(b.to_kph, 'speed', { digits: 0 })}`),
      cellBar(b.seconds / maxB, 30), h('span', { class: 'bar-v' }, `${formatNumber((b.seconds / total) * 100, { maximumFractionDigits: 0 })}%`)))) : empty('no speed recorded'));

  /* ---------- samples on demand */
  const samplesSlot = h('div');
  const samplesBtn = h('button', { class: 'btn', type: 'button', onclick: () => showSamples() }, 'show the samples');
  function showSamples() {
    const step = Math.max(1, Math.ceil(xs.length / 600));
    const rows = [];
    for (let i = 0; i < xs.length; i += step) rows.push(i);
    const col = (key, label, a, kind) => ({ key, label, num: true, ph: key === 'speed' ? 'num' : key === 'power' ? 'sub' : null, render: (i) => shown(a[i], kind) });
    const t = table(ctx, {
      bare: true, rows, label: 'Samples', columns: [
        { key: 't', label: 'time', ph: 'main', render: (i) => formatTime(xs[i], tz, { seconds: true }) },
        col('speed', 'speed', V('speed_kph', 'speed'), 'speed'), col('power', 'power kW', V('power_kw'), null), col('soc', 'soc %', V('soc_pct'), null),
        col('elev', 'elev', V('elevation_m', 'length'), 'length'), col('out', 'out', V('outside_c', 'temperature'), 'temperature'),
        { key: 'pos', label: 'position', render: (i) => (lat[i] != null ? `${lat[i].toFixed(5)}, ${lon[i].toFixed(5)}` : null) },
      ],
    });
    replace(samplesSlot, note(`${formatNumber(rows.length)} of ${formatNumber(xs.length)} samples${step > 1 ? `, one in ${step}` : ''}.`), t.el);
    samplesBtn.hidden = true;
  }

  replace(ctx.view, h('div', { class: 'page', dataset: { page: 'drive' }, cover: 'shots.drive-details' },
    hd,
    h('p', { class: 'subline' }, placeLink(d.start) || '—', h('span', { class: 'arrow' }, ' -> '), d.end && !live ? placeLink(d.end) || '—' : h('span', { class: 'tone--accent' }, 'driving now'),
      d.flags?.reduced_range ? [' ', tag('cold battery', 'warn')] : null, d.flags?.gap_ms ? [' ', tag(`gap ${formatDuration(d.flags.gap_ms)}`, 'bad')] : null, d.source !== 'can' ? [' ', tag('imported', 'muted')] : null, '   ', nav),
    section({ title: 'figures', cover: 'dd.empty_states', side: live ? h('span', { class: 'tone--accent' }, 'so far') : null }, statsEl),
    section({ title: 'route', cover: 'dd.map.route', side: [h('span', { class: 'muted' }, 'export '), gpxA, geoA, expKeys] }, mapEl),
    section({ title: 'through the drive', cover: ['dd.chart.drive', 'dd.chart.drive.tooltip'] }, selLine, panesEl, read.el),
    bandsEl,
    section({ title: 'samples' }, samplesBtn, samplesSlot)));

  // charts first (the ready signal waits for them), then the map
  try { await loadUPlot(); } catch { replace(panesEl, h('p', { class: 'chart-note' }, '-- the charts could not be drawn --')); }
  if (!ctx.alive()) return;
  if (window.uPlot) {
    const lastKey = blocks[blocks.length - 1]?.key;
    for (const b of blocks) {
      const two = b.two;
      const xAxis = b.key === lastKey ? timeAxis(tz) : { ...timeAxis(tz), size: 0, gap: 0, values: (u, sp) => sp.map(() => '') };
      const yScale = b.min0 ? { range: (u, a, c) => [0, Math.max(1, (c || 0) * 1.1)] } : { auto: true };
      const p = plot(b.el, ctx, () => ({
        padding: [rpx(6), 0, b.key === lastKey ? 0 : rpx(8), 0],
        cursor: { ...cursor(sync), drag: { x: true, y: false, setScale: false } },
        select: { show: true },
        scales: { x: { time: true, min: zoom ? zoom[0] : t0, max: zoom ? zoom[1] : t1 }, y: yScale, ...(two ? { y2: { auto: true } } : {}) },
        axes: [xAxis, valueAxis((v) => formatNumber(v), { size: 52 }), two ? valueAxis((v) => formatNumber(v), { scale: 'y2', side: 1, grid: false, size: 52 }) : { show: true, side: 1, size: rpx(52), grid: { show: false }, ticks: { show: false }, values: () => [] }],
        series: [{}, ...b.s.map(([n], k) => line(n, k, { scale: two && k === 1 ? 'y2' : 'y' }))],
        hooks: { setSelect: [(u) => { if (u.select.width > 2) { const a = u.posToVal(u.select.left, 'x'), c = u.posToVal(u.select.left + u.select.width, 'x'); u.setSelect({ width: 0, height: 0 }, false); setZoom([a, c]); } }] },
      }), [xs, ...b.s.map(([, a]) => a)], { onCursor });
      plots.push(p);
    }
  }

  // the map: the route from the track, A and B, a square at the cursor
  const coords = (track?.features || []).flatMap((f) => f.geometry.coordinates.map((c) => [c[0], c[1]]));
  if (coords.length > 1) {
    const mp = await safeMap(mapEl, ctx, { center: coords[0], zoom: 13 });
    if (mp && ctx.alive()) {
      mp.line('route', coords);
      mp.marker(coords[0], 'map-pin map-pin--a');
      if (!live) mp.marker(coords[coords.length - 1], 'map-pin map-pin--b');
      cursorPin = mp.marker(coords[0], 'map-pin map-pin--cur is-off');
      mp.fit(coords, { pad: 32 });
    }
  } else mapEl.append(h('p', { class: 'map-note' }, '-- no positions recorded for this drive --'));
  if (!ctx.alive()) return;
  ctx.ready();
}
