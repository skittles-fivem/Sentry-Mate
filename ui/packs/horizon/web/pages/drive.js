// Drive (/cars/{car}/drives/{drive}): the replay. The route on the night map (GET .../track, 1 Hz), the
// trip computer (figures, battery, odometer), then the traces on one time axis (GET /cars/{car}/series for
// the drive's span): speed and power, battery and range, elevation, climate with its set points and mode,
// battery heating and the tyres. One cursor runs through every trace and shows the car on the map; drag
// across a trace to zoom, and the selection's own figures follow. Export as GPX or GeoJSON.

import * as state from '../shared/state.js';
import { h, replace, cssVar } from '../shared/dom.js';
import { format, convert, getUnits, unitLabel, KM_PER_MI } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, zoneName, dayKey, dayRange, zonedParts } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { column, timeColumn, csvFilename } from '../shared/csv.js';
import { icon, hud, plate, btn, linkBtn, tag } from '../core/ui.js';
import { stat, rack, counters, eq, csvBtn, placeLabel, placeLink } from '../core/kit.js';
import { srcChip, estTag, kindsOf } from '../core/sources.js';
import { scope, loadUPlot, well } from '../core/chart.js';
import { createMap, addTrace, pin, boundsOf, pad, mapFallback } from '../core/map.js';

export const id = 'drive';
export const css = true;
export const title = () => 'Drive';

const CLIMATE = { off: 'Climate off', on: 'Climate on', precondition: 'Preconditioning', overheat_protect: 'Overheat protection' };
const TYRES = [['fl', 'Front left', '--cyan'], ['fr', 'Front right', '--org'], ['rl', 'Rear left', '--mag'], ['rr', 'Rear right', '--yel']];
const STEP_M = 2;
function climb(el) {
  let up = 0, down = 0, ref = el[0];
  for (let i = 1; i < el.length; i++) { const d = el[i] - ref; if (d >= STEP_M) { up += d; ref = el[i]; } else if (d <= -STEP_M) { down -= d; ref = el[i]; } }
  return [up, down];
}
const fileName = (name, ms, tz) => {
  const p = zonedParts(ms, tz); const z = (n) => String(n).padStart(2, '0');
  return `${String(name).replace(/[^A-Za-z0-9._-]+/g, '-').replace(/^-+|-+$/g, '') || 'car'}-drive-${p.year}-${z(p.month)}-${z(p.day)}-${z(p.hour)}${z(p.minute)}`;
};

export async function mount(ctx) {
  const { car, tz } = ctx;
  const cid = car.id, did = ctx.state.drive;
  const base = `cars/${encodeURIComponent(cid)}/drives/${encodeURIComponent(did)}`;
  const mph = getUnits().distance === 'mi';
  let d;
  try { d = await ctx.api.get(base, { speed_band_kph: mph ? 10 * KM_PER_MI : 10 }); } catch (e) {
    if (e.status === 404) e.message = 'There is no drive with this number for this car. It may have been rebuilt under a new number; find it on Drives.';
    ctx.fail(e, { retry: null }); return;
  }
  if (!ctx.alive()) return;
  const live = !!d.flags?.in_progress;
  const t0 = d.start_ms, t1 = d.end_ms ?? ctx.api.now();
  const ideal = getUnits().range === 'ideal';
  const rk = ideal ? 'ideal_range_km' : 'range_km';
  const fields = ['speed_kph', 'power_kw', 'soc_pct', 'usable_soc_pct', rk, 'energy_remaining_kwh', 'elevation_m', 'outside_c', 'cabin_c', 'setpoint_left_c', 'setpoint_right_c', 'odometer_km',
    'tyre_fl_kpa', 'tyre_fr_kpa', 'tyre_rl_kpa', 'tyre_rr_kpa', 'lat', 'lon', 'accuracy_m', 'battery_heat_kw', 'climate', 'fan_level'];
  const [series, track] = await Promise.all([
    ctx.api.get(`cars/${encodeURIComponent(cid)}/series`, { fields: fields.join(','), from_ms: t0, to_ms: t1 + 1000, max_points: 3600 }),
    ctx.api.get(`${base}/track`, { extensions: 'speed,power' }).catch(() => null),
  ]);
  if (!ctx.alive()) return;
  const V = series.values;
  V.est_range_km = (V.energy_remaining_kwh || []).map((e) => (e == null || !d.est_wh_per_km ? null : (e * 1000) / d.est_wh_per_km));
  if (live) ctx.events('rows', (ev) => { if (ev.data?.car === cid && (ev.data.kinds || []).includes('drives')) ctx.reload(); });

  ctx.setTitle(`Drive ${formatDate(t0, tz, { weekday: false, year: false })}, ${formatTime(t0, tz)}`);
  const est = d.energy_basis === 'estimated';
  const why = live ? 'Still driving' : 'Not recorded';

  /* ---------- export */
  const exp = { speed: true, power: false, soc: false, accuracy: true };
  const expUrl = (fmt) => ctx.api.url(`${base}/track`, { format: fmt, extensions: Object.keys(exp).filter((k) => exp[k]).join(',') || null });
  const file = fileName(car.name, t0, tz);
  const gpx = h('a', { class: 'btn btn--sm btn--go', download: `${file}.gpx`, href: expUrl('gpx'), cover: ['dd.export.gpx', 'web.drive-gpx.entry', 'web.drive-gpx.file', 'web.drive-gpx.point-coordinates', 'web.drive-gpx.point-elevation', 'web.drive-gpx.point-time', 'web.drive-gpx.not-found'] }, icon('download', 'i--sm'), h('span', null, 'GPX'));
  const geo = h('a', { class: 'btn btn--sm', download: `${file}.geojson`, href: expUrl('geojson') }, icon('download', 'i--sm'), h('span', null, 'GeoJSON'));
  const extras = h('div', { class: 'row dd-extras' }, h('span', { class: 'ctl-cap' }, 'With'),
    [['speed', 'speed'], ['power', 'power'], ['soc', 'battery %'], ['accuracy', 'GPS accuracy']].map(([k, w]) => h('label', { class: 'check' },
      h('input', { type: 'checkbox', checked: exp[k], onchange: (e) => { exp[k] = e.target.checked; gpx.setAttribute('href', expUrl('gpx')); geo.setAttribute('href', expUrl('geojson')); } }), h('span', null, w))));

  /* ---------- layout */
  const mapEl = h('div', { class: 'map dd-map', role: 'img', 'aria-label': `Map of the route from ${placeLabel(d.start)} to ${placeLabel(d.end)}` });
  const mapCard = h('div', { class: 'map-card dd-card', hidden: true, 'aria-hidden': 'true' });
  const selBox = h('div', { class: 'dd-sel', cover: ['dd.stat.selected_duration', 'dd.control.zoom'] });
  const resetBtn = btn('Whole drive', () => zoomTo(xMin, xMax), { sm: true, icon: 'arrow-l', kind: 'ghost' });
  resetBtn.hidden = true;
  const chartsEl = h('div', { class: 'dd-traces' });
  const timeEl = h('span', { class: 'scope-read' });
  const bands = (d.speed_bands || []).filter((b) => b.seconds > 0);
  const maxBand = Math.max(1e-9, ...bands.map((b) => b.seconds));
  const day = dayKey(t0, tz);
  const r0 = ideal ? d.start_ideal_range_km : d.start_range_km, r1 = ideal ? d.end_ideal_range_km : d.end_range_km;

  const figures = hud({ title: live ? 'Trip computer · so far' : 'Trip computer', icon: 'gauge', tone: 'cyan', cover: 'dd.empty_states' },
    counters([
      { label: 'Distance', value: d.distance_km, kind: 'distance', digits: 1, cover: 'dd.stat.distance', how: 'by odometer' },
      { label: 'Time', text: formatDuration(live ? ctx.api.now() - t0 : d.duration_ms), cover: 'dd.stat.drive_duration' },
      { label: 'Odometer', value: d.end_odometer_km ?? d.start_odometer_km, kind: 'odometer', digits: 1, cover: 'dd.stat.odometer', how: `started at ${format(d.start_odometer_km, 'odometer')}` },
    ]),
    rack([
      stat('Average speed', d.speed_avg_kph, 'speed', { cover: 'dd.stat.speed_avg', how: `distance ÷ time, stops included; top ${format(d.speed_max_kph, 'speed')}`, why }),
      stat('Energy used', d.energy_kwh, 'energy', { cover: 'dd.stat.energy_net', how: est ? (kindsOf(d).includes('tesla_api') ? 'estimated from the Tesla API\'s power readings' : 'estimated (imported)') : 'at the pack, regen taken off', why }),
      stat('Regen', d.regen_kwh, 'energy', { cover: 'dd.stat.energy_recovered', how: 'won back braking', why: est ? 'Not in this drive\'s data' : why }),
      stat('Consumption', d.wh_per_km, 'efficiency', { cover: 'dd.stat.consumption_net', how: d.efficiency_pct != null ? `${format(d.efficiency_pct, 'percent')} of rated` : null, why: d.distance_km < 0.5 ? 'Too short to be reliable' : why }),
      stat('Climb', d.climb_m, 'length', { cover: 'dd.stat.elevation_up', sign: true, how: "the car's own elevation" }),
      stat('Descent', d.descent_m != null ? -d.descent_m : null, 'length', { cover: 'dd.stat.elevation_down' }),
      stat('Peak power', d.power_max_kw, 'power', { how: d.power_min_kw != null ? `regen up to ${format(Math.abs(d.power_min_kw), 'power')}` : null }),
      stat('Climate', d.hvac_kwh, 'energy', { how: 'heating and cooling', why: 'Not sent by this car' }),
    ], { cls: 'dd-rack' }),
    h('div', { class: 'dd-soc' },
      h('div', null, h('b', null, format(d.start_soc_pct, 'percent')), h('span', null, `${format(r0, 'range')} ${ideal ? 'ideal' : 'rated'}`)),
      h('span', { class: 'dd-soc-bar', 'aria-hidden': 'true' }, h('i', { vars: { '--a': `${Math.max(0, Math.min(100, d.end_soc_pct ?? 0))}%`, '--b': `${Math.max(0, Math.min(100, d.start_soc_pct ?? 0))}%` } })),
      h('div', { class: 'dd-soc-end' }, h('b', null, format(d.end_soc_pct, 'percent')), h('span', null, `${format(r1, 'range')} ${ideal ? 'ideal' : 'rated'}`))),
    h('div', { class: 'flagline dd-flags' },
      d.flags?.gap_ms ? tag(`${formatDuration(d.flags.gap_ms)} without data`, 'org') : null,
      d.flags?.reduced_range ? tag('Cold battery', 'cyan') : null, estTag(d),
      h('span', { class: 'faint small' }, `Outside ${format(d.outside_avg_c, 'temperature')} · cabin ${format(d.cabin_avg_c, 'temperature')}`)));

  replace(ctx.view, h('div', { class: 'page dd', cover: 'shots.drive-details' },
    plate({
      kicker: [h('a', { href: ctx.link('drives') }, 'Drives'), ` · ${live ? 'Drive in progress' : 'Drive'} · ${formatDate(t0, tz, { weekday: 'long', month: 'long' })}`],
      title: h('span', { class: 'dd-title' }, placeLabel(d.start), h('span', { class: 'dd-arrow' }, ' → '), d.end && !live ? placeLabel(d.end) : 'driving now'),
      tools: [
        d.prev ? h('a', { class: 'btn btn--sm btn--icon', href: state.buildUrl('drive', { car: cid, drive: d.prev }), 'aria-label': 'Previous drive', title: 'Previous drive' }, icon('seek-l', 'i--sm')) : null,
        d.next ? h('a', { class: 'btn btn--sm btn--icon', href: state.buildUrl('drive', { car: cid, drive: d.next }), 'aria-label': 'Next drive', title: 'Next drive' }, icon('seek-r', 'i--sm')) : null,
        d.prev_charge ? linkBtn('Charge before', state.buildUrl('charge', { car: cid, charge: d.prev_charge }), { icon: 'plug', sm: true, kind: 'ghost' }) : null,
        d.next_charge ? linkBtn('Charge after', state.buildUrl('charge', { car: cid, charge: d.next_charge }), { icon: 'plug', sm: true, kind: 'ghost' }) : null,
        linkBtn('That day', state.buildUrl('timeline', { car: cid, period: dayRange(day, day, tz) }), { icon: 'timeline', sm: true, kind: 'ghost' }),
        linkBtn('As a trip', state.buildUrl('trip', { car: cid, period: { from: t0, to: d.end_ms ?? null } }), { icon: 'flag', sm: true, kind: 'ghost' }),
      ],
      extra: h('p', { class: 'dd-sub' },
        h('span', { cover: 'dd.context.drive' }, `${formatTime(t0, tz)} – ${live ? 'now' : formatTime(t1, tz)} ${zoneName(t0, tz)}`),
        h('span', null, format(d.distance_km, 'distance')), h('span', null, formatDuration(d.duration_ms)),
        h('span', { cover: 'dd.filter.car' }, h('a', { href: ctx.link('car') }, car.name)),
        live ? tag('Live', 'grn') : null, srcChip(d, { full: true })),
    }),
    h('div', { class: 'cols cols--wide dd-top' },
      hud({ title: 'Route', icon: 'route', pad: false, cover: 'dd.map.route', cls: 'dd-map-panel' },
        h('div', { class: 'dd-map-wrap' }, mapEl, mapCard),
        h('div', { class: 'dd-ends' },
          h('div', { class: 'dd-end' }, h('span', { class: 'pin' }, 'A'), h('div', null, h('b', null, placeLink(d.start)), h('span', null, d.start?.address?.short || '')), h('time', null, formatTime(t0, tz))),
          h('div', { class: 'dd-end' }, h('span', { class: 'pin pin--cyan' }, 'B'), h('div', null, h('b', null, d.end && !live ? placeLink(d.end) : (d.end?.label || 'Driving now')), h('span', null, d.end?.address?.short || '')), h('time', null, live ? 'now' : formatTime(t1, tz))))),
      figures),
    hud({ title: 'Through the drive', icon: 'pulse', cover: ['dd.chart.drive', 'dd.chart.drive.tooltip'],
      side: [srcChip(series), timeEl, resetBtn, csvBtn({ name: [car.name, 'drive', day], rows: () => series.t_ms.map((t, i) => Object.fromEntries([['t', t], ...[...fields, 'est_range_km'].map((k) => [k, V[k]?.[i] ?? null])])),
        columns: [timeColumn('Time', 't', tz), column('Speed', 'speed_kph', 'speed'), column('Power', 'power_kw', 'power'), column('Battery', 'soc_pct', 'percent'), column('Usable', 'usable_soc_pct', 'percent'),
          column(ideal ? 'Ideal range' : 'Rated range', rk, 'range'), column('Estimated range', 'est_range_km', 'range'), column('Elevation', 'elevation_m', 'length'), column('Outside', 'outside_c', 'temperature'),
          column('Cabin', 'cabin_c', 'temperature'), column('Set left', 'setpoint_left_c'), column('Set right', 'setpoint_right_c'), column('Climate', 'climate'), column('Fan', 'fan_level'),
          column('Battery heating', 'battery_heat_kw', 'power'), ...TYRES.map(([k, w]) => column(w, `tyre_${k}_kpa`, 'pressure')), column('Latitude', 'lat'), column('Longitude', 'lon')] })] },
    h('p', { class: 'note dd-hint' }, icon('info', 'i--sm'), h('span', null, 'Move over a trace to read every value at that moment and see the car on the map. Drag across a trace to zoom; double-click for the whole drive.')),
    selBox, chartsEl),
    h('div', { class: 'cols' },
      hud({ title: 'Time at each speed', icon: 'gauge', cover: 'dd.chart.speed_histogram', side: csvBtn({ name: [car.name, 'drive', day, 'speeds'], rows: bands, columns: [column('From', 'from_kph', 'speed'), column('To', 'to_kph', 'speed'), column('Seconds', 'seconds')] }) },
        eq(bands.map((b) => ({ label: `${formatNumber(convert(b.from_kph, 'speed'), { maximumFractionDigits: 0 })}–${format(b.to_kph, 'speed', { digits: 0 })}`, value: b.seconds / maxBand, text: formatDuration(b.seconds * 1000) })), { empty: 'No speed recorded.' }),
        h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, `Counted second by second, in steps of 10 ${unitLabel('speed')}.`))),
      hud({ title: 'Export the route', icon: 'download' },
        h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, 'Every second of the route with its time, elevation and position, for map apps and route planners. A drive that no longer exists answers "not found".')),
        h('div', { class: 'row dd-exp' }, gpx, geo), extras))));
  ctx.ready();

  /* ---------- the map */
  const coords = [];
  for (const ft of track?.features || []) for (const p of ft.geometry?.coordinates || []) coords.push([p[0], p[1]]);
  let m = null, hover = null;
  (async () => {
    if (coords.length < 2) { mapEl.append(mapFallback('No route was recorded for this drive.')); return; }
    try {
      m = await createMap(mapEl, ctx, { bounds: boundsOf(coords) });
      if (!m) return;
      await m.ready;
      if (!ctx.alive()) return;
      addTrace(m, 'route', { type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: coords } }, { color: '--mag' });
      m.map.setPaintProperty('route-line', 'line-gradient', ['interpolate', ['linear'], ['line-progress'], 0, cssVar('--cyan'), 0.5, cssVar('--mag'), 1, cssVar('--org')]);
      pin(m, coords[0], { text: 'A' });
      pin(m, coords[coords.length - 1], { text: 'B', tone: 'cyan' });
      m.frame(() => m.map.fitBounds(boundsOf(coords), { padding: pad(36), maxZoom: 16, duration: 0 }));
    } catch (e) { console.warn(e); replace(mapEl, mapFallback()); }
  })();
  function hoverAt(i) {
    const la = V.lat?.[i], lo = V.lon?.[i];
    if (i == null || la == null || !m) { mapCard.hidden = true; hover?.remove(); hover = null; return; }
    if (!hover) hover = pin(m, [lo, la], { tone: 'car' }); else hover.setLngLat([lo, la]);
    mapCard.hidden = false;
    replace(mapCard, h('b', null, formatTime(series.t_ms[i], tz, { seconds: true })), ` · ${format(V.speed_kph?.[i], 'speed')} · ${format(V.power_kw?.[i], 'power')}`);
  }

  /* ---------- the traces */
  await loadUPlot();
  if (!ctx.alive()) return;
  const T = series.t_ms;
  const xMin = T[0] / 1000, xMax = T[T.length - 1] / 1000;
  const known = (k) => (V[k] || []).some((v) => v != null);
  const clim = V.climate || [];
  const climSeen = [...new Set(clim.filter((v) => v != null))];
  const heat = (V.battery_heat_kw || []).filter((v) => v != null);
  const defs = [
    { key: 'drive', name: 'Speed and power', cover: ['dd.chart.drive.speed', 'dd.chart.drive.power'], series: [
      { label: `Speed (${unitLabel('speed')})`, values: V.speed_kph || [], kind: 'speed', color: '--c-speed', fill: true },
      { label: 'Power (kW, regen below 0)', values: V.power_kw || [], kind: 'power', color: '--c-power', scale: 'y2', width: 1.5 }] },
    { key: 'soc', name: 'Battery and range', cover: ['dd.chart.drive.soc', 'dd.chart.drive.range'], series: [
      { label: 'Battery %', values: V.soc_pct || [], kind: 'percent', color: '--c-soc' },
      { label: 'Usable %', values: V.usable_soc_pct || [], kind: 'percent', color: '--violet', dash: [4, 3], width: 1.5 },
      { label: `${ideal ? 'Ideal' : 'Rated'} range (${unitLabel('range')})`, values: V[rk] || [], kind: 'range', color: '--cyan', scale: 'y2', width: 1.5 },
      { label: 'Sentry Mate estimate', values: V.est_range_km, kind: 'range', color: '--org', scale: 'y2', dash: [5, 3], width: 1.5 }],
    note: d.est_wh_per_km && known('energy_remaining_kwh') ? `The estimate is the energy left ÷ ${format(d.est_wh_per_km, 'efficiency')}, the car's consumption over the 30 days before.` : 'No estimate: this drive has no energy-left readings.' },
    { key: 'elev', name: `Elevation (${unitLabel('length')})`, cover: 'dd.chart.elevation', series: [{ label: 'Elevation', values: V.elevation_m || [], kind: 'length', color: '--c-elev', fill: true }] },
    { key: 'temps', name: `Climate (${unitLabel('temperature')})`, cover: ['dd.chart.temps.air', 'dd.chart.temps.setpoints', 'dd.chart.temps.climate_state'], bands: clim, series: [
      { label: 'Outside', values: V.outside_c || [], kind: 'temperature', color: '--org' },
      { label: 'Cabin', values: V.cabin_c || [], kind: 'temperature', color: '--yel' },
      { label: 'Set left', values: V.setpoint_left_c || [], kind: 'temperature', color: '--mag', dash: [4, 3], width: 1.5 },
      { label: 'Set right', values: V.setpoint_right_c || [], kind: 'temperature', color: '--violet', dash: [4, 3], width: 1.5 },
      { label: 'Fan step', values: V.fan_level || [], color: '--cyan', scale: 'y2', width: 1 }],
    note: !known('climate') ? 'This car does not send the climate mode.' : `Shaded while the climate was on${climSeen.includes('precondition') ? ', orange while preconditioning' : ''}. LO and HI set points leave a gap in the line.` },
    { key: 'heat', name: 'Battery heating (kW)', cover: 'dd.chart.drive.battery_heater', series: [{ label: 'Heating', values: V.battery_heat_kw || [], kind: 'power', color: '--org', fill: true }],
      note: !heat.length ? 'This car does not send battery heating power.' : heat.some((v) => v > 0) ? 'Power the pack spent warming itself.' : 'The pack did not heat itself on this drive.' },
    { key: 'tyres', name: `Tyres (${unitLabel('pressure')})`, cover: 'dd.chart.tire_pressure', axes: { y: { digits: 2 } }, series: TYRES.map(([k, w, c]) => ({ label: w, values: V[`tyre_${k}_kpa`] || [], kind: 'pressure', color: c, width: 1.5 })),
      note: TYRES.some(([k]) => known(`tyre_${k}_kpa`)) ? null : 'This drive has no tyre pressures.' },
  ];
  const plots = [];
  let zoom = [xMin, xMax];
  const zoomTo = (a, b) => { zoom = [a, b]; for (const p of plots) p?.u?.setScale('x', { min: a, max: b }); paintSel(); };
  function paintSel() {
    const [a, b] = zoom;
    const z = a > xMin + 1 || b < xMax - 1;
    resetBtn.hidden = !z;
    const idx = []; T.forEach((t, i) => { if (t / 1000 >= a && t / 1000 <= b) idx.push(i); });
    const pick = (k) => idx.map((i) => V[k]?.[i]).filter((v) => v != null);
    const odo = pick('odometer_km'), el = pick('elevation_m');
    const [up, down] = el.length > 1 ? climb(el) : [null, null];
    const dur = z ? (b - a) * 1000 : (live ? ctx.api.now() - t0 : d.duration_ms);
    const dist = z ? (odo.length > 1 ? odo[odo.length - 1] - odo[0] : null) : d.distance_km;
    replace(selBox, h('span', { class: 'dd-sel-title' }, z ? `Selection ${formatTime(a * 1000, tz)} – ${formatTime(b * 1000, tz)}` : 'The whole drive'),
      rack([
        stat('Duration', dur, 'duration'),
        stat('Distance', dist, 'distance', { why: 'No odometer here' }),
        stat('Average speed', dist != null && dur > 0 ? dist / (dur / 3.6e6) : null, 'speed', { why: 'No odometer here' }),
        stat('Climb', z ? up : d.climb_m, 'length', { sign: true }),
        stat('Descent', z ? (down != null ? -down : null) : (d.descent_m != null ? -d.descent_m : null), 'length'),
      ], { cls: 'dd-sel-rack' }));
  }
  paintSel();
  const read = (i) => {
    if (i == null) { timeEl.textContent = ''; for (const b of blocks) b.val.textContent = ''; hoverAt(null); return; }
    timeEl.textContent = formatTime(T[i], tz, { seconds: true });
    for (const b of blocks) {
      const parts = b.df.series.map((s) => { const v = s.values[i]; return typeof v === 'string' ? v : v == null ? '—' : format(v, s.kind || 'number'); });
      const mode = b.df.bands ? CLIMATE[clim[i]] || 'Climate unknown' : null;
      b.val.textContent = (mode ? `${mode} · ` : '') + parts.join(' · ');
    }
    hoverAt(i);
  };
  const drawBands = (u) => {
    const c = u.ctx; const { top, height } = u.bbox;
    for (let i = 0; i < clim.length; i++) {
      const mode = clim[i];
      if (mode !== 'on' && mode !== 'precondition' && mode !== 'overheat_protect') continue;
      const xa = u.valToPos(T[i] / 1000, 'x', true), xb = u.valToPos((T[i + 1] ?? T[i] + 1000) / 1000, 'x', true);
      c.fillStyle = mode === 'on' ? 'rgba(61, 240, 255, 0.10)' : mode === 'precondition' ? 'rgba(255, 138, 61, 0.16)' : 'rgba(255, 90, 120, 0.18)';
      c.fillRect(xa, top, Math.max(1, xb - xa), height);
    }
  };
  const blocks = defs.map((df) => {
    const target = well('', df.name);
    const val = h('span', { class: 'scope-read' });
    const el = h('div', { class: 'dd-trace', cover: df.cover },
      h('div', { class: 'scope-head' }, h('span', { class: 'scope-title' }, df.name),
        h('span', { class: 'keys' }, df.series.map((s) => h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': `var(${s.color})` } }), s.label))), val),
      target, df.note ? h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, df.note)) : null);
    return { df, el, target, val };
  });
  replace(chartsEl, blocks.map((b) => b.el));
  for (const [k, b] of blocks.entries()) {
    const p = await scope(b.target, ctx, {
      tz, t_ms: T, series: b.df.series, sync: `dd-${did}`, axes: b.df.axes || {}, showX: true,
      zoom: (a, z) => zoomTo(a / 1000, z / 1000),
      onCursor: k === 0 ? read : null,
      hooks: b.df.bands ? { drawClear: [drawBands] } : {},
      onBuild: (u) => { u.over.addEventListener('dblclick', () => zoomTo(xMin, xMax)); if (zoom[0] > xMin || zoom[1] < xMax) u.setScale('x', { min: zoom[0], max: zoom[1] }); },
    });
    if (!ctx.alive()) return;
    plots.push(p);
  }
}
