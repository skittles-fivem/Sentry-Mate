// Drive detail (/cars/{car}/drives/{drive}): one drive in full. The route (GET .../track, GeoJSON, 1 Hz,
// coloured by speed or power), the charts on one time axis (GET /cars/{car}/series for the drive's span:
// speed, power, battery, range, elevation, climate, tyres) with one hover readout and zoom by dragging
// (the figures for the selection follow it), the drive's own figures, and the export as GPX or GeoJSON.

import * as state from '../shared/state.js';
import { h, replace, setVars, cssVar, steady } from '../shared/dom.js';
import { format, convert, unitLabel, getUnits, KM_PER_MI } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, zoneName, dayRange, dayKey, zonedParts } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { downloadCsv, csvFilename, column, timeColumn } from '../shared/csv.js';
import { trackBounds } from '../shared/tracks.js';
import { icon } from '../core/icons.js';
import { pageHead, panel, metric, tag, csvButton } from '../core/page.js';
import { createMap, routePin, boundsOf, padClear, mapFallback } from '../core/map.js';
import { loadUPlot, mountPlot, timeAxis, valueAxis, colors, areaFill, hexA } from '../core/chart.js';
import { rpx } from '../core/display.js';
import { TYRES, val, spotText } from '../core/fmt.js';
import { srcMark, isImported, apiOnly } from '../core/source.js';

export const id = 'drive';
export const css = true;
export const title = () => 'Drive';

const placeLabel = (p) => p?.place?.name || p?.label || p?.address?.short || spotText(p) || '—';
const placeHref = (p) => (p?.place?.id ? state.buildUrl('places', { query: { place: p.place.id } })
  : p?.lat != null ? state.buildUrl('places', { query: { lat: p.lat.toFixed(5), lon: p.lon.toFixed(5), new: 1 } }) : null);
const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
// the climate system's modes, in words, with the token its band is drawn in
const CLIMATE = { off: ['Climate off', null], on: ['Climate on', '--live'], precondition: ['Preconditioning', '--warm'], overheat_protect: ['Overheat protection', '--danger'] };
const setWord = (v) => (v === 'LO' ? 'LO (full cooling)' : v === 'HI' ? 'HI (full heat)' : null);
// climb and descent the way the drive's own figures count them: only steps of 2 m or more
const ELEV_STEP_M = 2;
function climbDescent(el) {
  let up = 0, down = 0, ref = el[0];
  for (let i = 1; i < el.length; i++) {
    const dlt = el[i] - ref;
    if (dlt >= ELEV_STEP_M) { up += dlt; ref = el[i]; } else if (dlt <= -ELEV_STEP_M) { down -= dlt; ref = el[i]; }
  }
  return [up, down];
}
const AVG_HOW = 'distance ÷ time, stops included';
// export files: <car>-drive-<YYYY-MM-DD-HHMM>, the start in the car's zone (the server names them the same way)
const exportName = (carName, ms, tz) => {
  const p = zonedParts(ms, tz);
  const z = (n) => String(n).padStart(2, '0');
  return `${String(carName).replace(/[^A-Za-z0-9._-]+/g, '-').replace(/^-+|-+$/g, '') || 'car'}-drive-${p.year}-${z(p.month)}-${z(p.day)}-${z(p.hour)}${z(p.minute)}`;
};

// The page's reads; prefetch() warms the same ones when a link here is under the pointer.
const driveBase = (ctx) => `cars/${encodeURIComponent(ctx.car.id)}/drives/${encodeURIComponent(ctx.state.drive)}`;
// speed bands of 10 in the viewer's own unit: 10 km/h, or 10 mph (16.09344 km/h)
const driveRead = (ctx) => ctx.api.get(driveBase(ctx), { speed_band_kph: getUnits().distance === 'mi' ? 10 * KM_PER_MI : 10 });
const trackRead = (ctx) => ctx.api.get(`${driveBase(ctx)}/track`, { extensions: 'speed,power' });
const seriesFields = (rk) => ['speed_kph', 'power_kw', 'soc_pct', 'usable_soc_pct', rk, 'energy_remaining_kwh', 'elevation_m', 'outside_c', 'cabin_c', 'setpoint_left_c', 'setpoint_right_c', 'odometer_km', 'tyre_fl_kpa', 'tyre_fr_kpa', 'tyre_rl_kpa', 'tyre_rr_kpa', 'lat', 'lon', 'accuracy_m', 'battery_heat_kw', 'climate', 'fan_level'];
const seriesRead = (ctx, t0, t1, fields) => ctx.api.get(`cars/${encodeURIComponent(ctx.car.id)}/series`, { fields: fields.join(','), from_ms: t0, to_ms: t1 + 1000, max_points: 3600 });
export async function prefetch(ctx) {
  const [d] = await Promise.all([driveRead(ctx), trackRead(ctx), Promise.resolve().then(loadUPlot).catch(() => null)]);
  if (!d) return;
  const rk = getUnits().range === 'ideal' ? 'ideal_range_km' : 'range_km';
  await seriesRead(ctx, d.start_ms, d.end_ms ?? ctx.api.now(), seriesFields(rk));
}

export async function mount(ctx) {
  const { car, tz } = ctx;
  const cid = car.id, did = ctx.state.drive;
  const base = `cars/${encodeURIComponent(cid)}/drives/${encodeURIComponent(did)}`;
  // speed bands of 10 in the viewer's own unit: 10 km/h, or 10 mph (16.09344 km/h)
  const mph = getUnits().distance === 'mi';
  let d;
  try { d = await driveRead(ctx); } catch (e) {
    if (e.status === 404) e.message = 'There is no drive with this number for this car. It may have been rebuilt under a new number; find it on Drives.';
    ctx.fail(e, { retry: null }); return;
  }
  if (!ctx.alive()) return;
  const live = !!d.flags?.in_progress;
  const t0 = d.start_ms, t1 = d.end_ms ?? ctx.api.now();
  const ideal = getUnits().range === 'ideal';
  const rk = ideal ? 'ideal_range_km' : 'range_km';
  const fields = seriesFields(rk);
  // uPlot loads with the data, so the charts are drawn with the page (their space never opens up later)
  let trackErr = null;   // the route's read failed: said as that, not as "no route was recorded"
  const [series, track] = await Promise.all([
    seriesRead(ctx, t0, t1, fields),
    trackRead(ctx).catch((e) => { trackErr = e; return null; }),
    loadUPlot(),
  ]);
  if (!ctx.alive()) return;
  // Sentry Mate's own range estimate: the energy left divided by the consumption of the 30 days before the drive
  const estWh = d.est_wh_per_km;
  series.values.est_range_km = (series.values.energy_remaining_kwh || []).map((e) => (e == null || !estWh ? null : (e * 1000) / estWh));
  let repaintSel = null;
  // a drive still going: build again when it ends, and keep the 'so far' figures moving meanwhile
  if (live) {
    ctx.events('rows', (ev) => { if (ev.data?.car === cid && (ev.data.kinds || []).includes('drives')) ctx.refresh(); });
    ctx.events('now', (ev) => {
      const dr = ev.data?.car === cid ? ev.data.now?.drive : null;
      if (!dr || (dr.drive_id != null && dr.drive_id !== did)) return;
      if (dr.distance_km != null) d.distance_km = dr.distance_km;
      if (dr.energy_kwh != null) d.energy_kwh = dr.energy_kwh;
      paintFigures();
      repaintSel?.();
    });
  }

  ctx.setTitle(`Drive ${formatDate(t0, tz, { weekday: false, year: false })}, ${formatTime(t0, tz)}`);
  ctx.setCrumbs([{ label: 'All cars', href: '/' }, { label: car.name, href: ctx.link('car') }, { label: 'Drives', href: ctx.link('drives') }, { label: `${formatDate(t0, tz, { year: false })}, ${formatTime(t0, tz)}` }]);

  const day = dayKey(t0, tz);
  const dayPeriod = dayRange(day, day, tz);
  const exp = { speed: true, power: false, soc: false, accuracy: true };
  const expUrl = (fmt) => ctx.api.url(`${base}/track`, { format: fmt, extensions: Object.keys(exp).filter((k) => exp[k]).join(',') || null });
  const fileBase = exportName(car.name, t0, tz);
  const gpxA = h('a', { class: 'btn btn--sm', download: `${fileBase}.gpx`, href: expUrl('gpx'), cover: ['dd.export.gpx', 'web.drive-gpx.entry', 'web.drive-gpx.file', 'web.drive-gpx.point-coordinates', 'web.drive-gpx.point-elevation', 'web.drive-gpx.point-time', 'web.drive-gpx.not-found'] }, icon('download', 'ico--sm'), 'GPX');
  const geoA = h('a', { class: 'btn btn--sm', download: `${fileBase}.geojson`, href: expUrl('geojson') }, icon('download', 'ico--sm'), 'GeoJSON');
  const extras = h('span', { class: 'export-extras' }, h('span', { class: 'faint' }, 'With'),
    ['speed', 'power', 'soc', 'accuracy'].map((k) => h('label', { class: 'check' }, h('input', { type: 'checkbox', checked: exp[k], onchange: (e) => { exp[k] = e.target.checked; gpxA.setAttribute('href', expUrl('gpx')); geoA.setAttribute('href', expUrl('geojson')); } }), { speed: 'speed', power: 'power', soc: 'battery %', accuracy: 'accuracy' }[k])));

  /* ---------- the layout */
  const mapEl = h('div', { class: 'map', role: 'img', 'aria-label': `Map of the route from ${placeLabel(d.start)} to ${placeLabel(d.end)}` });
  const readout = h('div', { class: 'glass-card hover-readout', hidden: true, 'aria-hidden': 'true' });
  const colourSeg = h('div', { class: 'seg route-colour', role: 'group', 'aria-label': 'Colour the route by' },
    [['speed', 'Speed'], ['power', 'Power'], ['plain', 'Plain']].map(([v, l]) => h('button', { class: 'seg-btn', type: 'button', 'aria-pressed': String(v === 'speed'), dataset: { colour: v }, onclick: () => colourRoute(v) }, l)));
  const placeA = (p) => { const href = placeHref(p); return href ? h('a', { class: 'tl-place-link', href }, placeLabel(p)) : placeLabel(p); };
  const legend = h('div', { class: 'glass-card route-legend' },
    h('div', { class: 'route-end' }, h('span', { class: 'route-pin route-pin--a', 'aria-hidden': 'true' }, 'A'), h('div', null, h('b', null, placeA(d.start)), h('span', null, d.start?.address?.short || '')), h('time', null, formatTime(t0, tz))),
    h('div', { class: 'route-line', 'aria-hidden': 'true' }),
    h('div', { class: 'route-end' }, h('span', { class: 'route-pin route-pin--b', 'aria-hidden': 'true' }, 'B'), h('div', null, h('b', null, d.end && !live ? placeA(d.end) : (d.end?.label || 'Driving now')), h('span', null, d.end?.address?.short || '')), h('time', null, live ? 'now' : formatTime(t1, tz))));
  const mapPanel = h('section', { class: 'panel map-panel', 'aria-label': 'Route map', cover: 'dd.map.route' },
    mapEl, h('div', { class: 'map-overlay map-overlay--tl' }, legend), h('div', { class: 'map-overlay map-overlay--bl' }, readout), h('div', { class: 'map-overlay map-overlay--br' }, colourSeg));

  const why = live ? 'Still driving' : 'Not recorded';
  const est = d.energy_basis === 'estimated';
  const figuresEl = h('div', { class: 'drive-figures' });
  const paintFigures = () => {
    const dur = live ? ctx.api.now() - t0 : d.duration_ms;
    const avg = live ? (dur > 0 ? d.distance_km / (dur / 3.6e6) : null) : d.speed_avg_kph;
    replace(figuresEl,
      metric('Distance', d.distance_km, 'distance', { cover: 'dd.stat.distance', explain: 'by odometer, 1 m steps' }),
      metric('Duration', dur, 'duration', { cover: 'dd.stat.drive_duration' }),
      metric('Average speed', avg, 'speed', { cover: 'dd.stat.speed_avg', explain: `${AVG_HOW}; top ${format(d.speed_max_kph, 'speed')}` }),
      metric('Energy used', d.energy_kwh, 'energy', { cover: 'dd.stat.energy_net', explain: est ? (apiOnly(d) ? 'estimated: the Tesla API\'s power readings, summed' : 'estimated (imported)') : 'measured at the pack, regen taken off', why }),
      metric('Regen', d.regen_kwh, 'energy', { cover: 'dd.stat.energy_recovered', explain: 'won back braking', why: est ? (apiOnly(d) ? 'Not from the Tesla API' : 'Not in imported history') : why }),
      metric('Consumption', d.wh_per_km, 'efficiency', { cover: 'dd.stat.consumption_net', explain: d.efficiency_pct != null ? `${format(d.efficiency_pct, 'percent')} of rated` : null, why: d.distance_km < 0.5 ? 'Too short to be reliable' : why }),
      metric('Climb', d.climb_m, 'length', { cover: 'dd.stat.elevation_up', explain: "the car's own elevation", sign: true }),
      metric('Descent', d.descent_m != null ? -d.descent_m : null, 'length', { cover: 'dd.stat.elevation_down' }),
      metric('Peak power', d.power_max_kw, 'power', { explain: d.power_min_kw != null ? `regen up to ${format(Math.abs(d.power_min_kw), 'power')}` : null }),
      metric('Climate', d.hvac_kwh, 'energy', { explain: 'heating and cooling', why: 'Not sent by this car' }));
  };
  paintFigures();
  // the battery strip names its range: rated, or ideal when the viewer chose ideal
  const rangeWord = ideal ? 'ideal' : 'rated';
  const r0 = ideal ? d.start_ideal_range_km : d.start_range_km;
  const r1 = ideal ? d.end_ideal_range_km : d.end_range_km;
  const stats = panel({ title: 'Figures', icon: 'stats', cls: 'stats-panel', side: live ? h('span', { class: 'faint' }, 'so far') : null, cover: 'dd.empty_states' },
    figuresEl,
    h('div', { class: 'drive-soc' },
      h('div', null, h('b', null, format(d.start_soc_pct, 'percent')), h('span', null, `${format(r0, 'range')} ${rangeWord}`)),
      h('span', { class: 'bar' }, h('i', { vars: { '--w': `${clamp(d.end_soc_pct ?? 0, 0, 100)}%` } })),
      h('div', { class: 'r' }, h('b', null, format(d.end_soc_pct, 'percent')), h('span', null, `${format(r1, 'range')} ${rangeWord}`))),
    h('dl', { class: 'drive-odo', cover: 'dd.stat.odometer' },
      h('dt', null, 'Odometer'), h('dd', null, `${format(d.start_odometer_km, 'odometer')} → ${format(d.end_odometer_km, 'odometer')}`),
      h('dt', null, 'Outside'), h('dd', null, format(d.outside_avg_c, 'temperature')),
      h('dt', null, 'Cabin'), h('dd', null, format(d.cabin_avg_c, 'temperature', { why: 'Not sent by this car' })),
      d.flags?.gap_ms ? [h('dt', null, 'Missing'), h('dd', { class: 'warn' }, `${formatDuration(d.flags.gap_ms)} without data`)] : null,
      d.flags?.reduced_range ? [h('dt', null, 'Cold battery'), h('dd', { class: 'warn' }, 'part of the charge was locked')] : null,
      [h('dt', null, 'Data from'), h('dd', { cover: null }, srcMark(d, { estimated: true }))]));

  // a set point column: LO and HI stay words, numbers go to the viewer's unit
  const setColumn = (label, key) => {
    const c = column(label, key, 'temperature', { digits: 1 });
    return { label: c.label, value: (row, i) => (typeof row[key] === 'string' ? row[key] : c.value(row, i)) };
  };
  const selBox = h('div', { class: 'selection', cover: ['dd.stat.selected_duration', 'dd.control.zoom'] });
  const timeEl = h('span', { class: 'chart-time' });
  const resetBtn = h('button', { class: 'btn btn--sm', type: 'button', hidden: true }, icon('reset', 'ico--sm'), 'Whole drive');
  const chartsHost = h('div', { class: 'drive-charts' });
  const bands = (d.speed_bands || []).filter((b) => b.seconds > 0);
  const maxBand = Math.max(1e-9, ...bands.map((b) => b.seconds));
  const bandsEl = panel({ title: 'Time at each speed', icon: 'gauge', cover: 'dd.chart.speed_histogram', side: csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'drive', day, 'speeds'), bands, [column('From', 'from_kph', 'speed'), column('To', 'to_kph', 'speed'), column('Seconds', 'seconds')]) }) },
    bands.length ? h('ul', { class: 'hbars' }, bands.map((b) => h('li', null,
      h('span', { class: 'hbar-label' }, `${formatNumber(convert(b.from_kph, 'speed'), { maximumFractionDigits: 0 })}–${format(b.to_kph, 'speed', { digits: 0 })}`),
      h('span', { class: 'hbar' }, h('i', { vars: { '--w': `${(b.seconds / maxBand) * 100}%` } })),
      h('span', { class: 'hbar-val' }, formatDuration(b.seconds * 1000))))) : h('p', { class: 'note' }, 'No speed recorded.'),
    h('p', { class: 'note' }, `Counted second by second, in bands of 10 ${unitLabel('speed')}.`));

  replace(ctx.view, h('div', { class: 'drive-page', cover: 'shots.drive-details' },
    pageHead({
      back: { href: ctx.link('drives'), label: 'Drives' },
      eyebrow: `${live ? 'Drive in progress' : 'Drive'} · ${formatDate(t0, tz, { weekday: 'long', month: 'long' })}`,
      title: [placeLabel(d.start), ' → ', d.end ? placeLabel(d.end) : 'driving now'],
      titleExtra: live ? h('span', { class: 'pill pill--lg pill--live' }, icon('live'), 'Live') : null,
      sub: [h('span', { cover: 'dd.context.drive' }, `${formatTime(t0, tz)} – ${live ? 'now' : formatTime(t1, tz)} ${zoneName(t0, tz)}`), format(d.distance_km, 'distance'), formatDuration(d.duration_ms), h('span', { cover: 'dd.filter.car' }, car.name), srcMark(d, { estimated: true })],
      actions: [
        d.prev ? h('a', { class: 'btn btn--icon', href: state.buildUrl('drive', { car: cid, drive: d.prev }), 'aria-label': 'Previous drive', title: 'Previous drive' }, icon('chev-l')) : null,
        d.next ? h('a', { class: 'btn btn--icon', href: state.buildUrl('drive', { car: cid, drive: d.next }), 'aria-label': 'Next drive', title: 'Next drive' }, icon('chev-r')) : null,
        d.prev_charge ? h('a', { class: 'btn', href: state.buildUrl('charge', { car: cid, charge: d.prev_charge }), title: 'The last charge before this drive' }, icon('plug'), 'Charge before') : null,
        d.next_charge ? h('a', { class: 'btn', href: state.buildUrl('charge', { car: cid, charge: d.next_charge }), title: 'The first charge after this drive' }, icon('plug'), 'Charge after') : null,
        h('a', { class: 'btn', href: state.buildUrl('timeline', { car: cid, period: dayPeriod }) }, icon('timeline'), 'That day'),
        h('a', { class: 'btn', href: state.buildUrl('trip', { car: cid, period: { from: t0, to: d.end_ms ?? null } }) }, icon('flag'), 'As a trip'),
      ],
    }),
    h('div', { class: 'drive-layout' }, mapPanel, stats),
    h('section', { class: 'panel charts-panel', 'aria-label': 'Through the drive', cover: ['dd.chart.drive', 'dd.chart.drive.tooltip'] },
      h('div', { class: 'panel-head' }, h('h2', { class: 'panel-title' }, icon('signals'), 'Through the drive'),
        h('span', { class: 'row' }, steady(timeEl, (series?.t_ms || []).map((t) => formatTime(t, tz, { seconds: true }))), srcMark(series, { short: true }), h('span', { class: 'faint' }, 'Drag across a chart to zoom; double-click for the whole drive'), resetBtn,
          csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'drive', day), series.t_ms.map((t, i) => Object.fromEntries([['t', t], ...[...fields, 'est_range_km'].map((f) => [f, series.values[f]?.[i] ?? null])])),
            [timeColumn('Time', 't', tz), column('Speed', 'speed_kph', 'speed'), column('Power', 'power_kw', 'power'), column('Battery', 'soc_pct', 'percent'), column('Usable', 'usable_soc_pct', 'percent'), column(ideal ? 'Ideal range' : 'Rated range', rk, 'range'), column('Estimated range (Sentry Mate)', 'est_range_km', 'range'), column('Energy left', 'energy_remaining_kwh', 'energy'), column('Elevation', 'elevation_m', 'length'), column('Outside', 'outside_c', 'temperature'), column('Cabin', 'cabin_c', 'temperature'), setColumn('Set left', 'setpoint_left_c'), setColumn('Set right', 'setpoint_right_c'), column('Climate', 'climate'), column('Fan step (0-10)', 'fan_level'), column('Battery heating', 'battery_heat_kw', 'power'), column('Odometer', 'odometer_km', 'odometer'), column('Latitude', 'lat'), column('Longitude', 'lon'), column('Accuracy', 'accuracy_m', 'length')]) }))),
      selBox, chartsHost),
    h('div', { class: 'drive-lower' }, bandsEl,
      panel({ title: 'Export the route', icon: 'download' },
        h('p', { class: 'note' }, 'Every second of the route with its time, elevation and GPS accuracy, for map apps and route planners.'),
        h('div', { class: 'row' }, gpxA, geoA), extras))));

  /* ---------- the map */
  const coords = [];
  const props = { speed: [], power: [] };
  for (const f of track?.features || []) {
    const c = f.geometry?.coordinates || [];
    c.forEach((p, i) => { coords.push([p[0], p[1]]); props.speed.push(f.properties?.speed_kph?.[i] ?? null); props.power.push(f.properties?.power_kw?.[i] ?? null); });
  }
  // the map frames the route without stray fixes (a jump to another state and back)
  const routeBounds = trackBounds(track?.features || []) || boundsOf(coords);
  let m = null, hoverMarker = null;
  // a gradient along the route by speed or power, from stops at the share of the distance travelled
  const gradient = (kind) => {
    if (kind === 'plain' || coords.length < 2) return ['interpolate', ['linear'], ['line-progress'], 0, cssVar('--live-2'), 1, cssVar('--live')];
    const vals = props[kind];
    const lo = kind === 'power' ? Math.min(0, ...vals.filter((v) => v != null)) : 0;
    const hi = Math.max(1, ...vals.filter((v) => v != null));
    const dist = [0];
    for (let i = 1; i < coords.length; i++) dist.push(dist[i - 1] + Math.hypot(coords[i][0] - coords[i - 1][0], (coords[i][1] - coords[i - 1][1]) * 1.3));
    const total = dist[dist.length - 1] || 1;
    const ramp = kind === 'power' ? [cssVar('--c-regen'), cssVar('--c-power'), cssVar('--warm')] : [cssVar('--st-parked'), cssVar('--live'), cssVar('--warm')];
    const step = Math.max(1, Math.floor(coords.length / 120));
    const stops = [];
    let lastP = -1;
    for (let i = 0; i < coords.length; i += step) {
      const p = dist[i] / total;
      if (p <= lastP) continue;
      lastP = p;
      const v = vals[i] ?? 0;
      const t = kind === 'power' ? (v < 0 ? 0 : 0.5 + 0.5 * Math.min(1, v / hi)) : Math.min(1, (v - lo) / (hi - lo));
      stops.push(p, t < 0.5 ? ramp[0] : t < 0.8 ? ramp[1] : ramp[2]);
    }
    if (stops.length < 4) return ['interpolate', ['linear'], ['line-progress'], 0, ramp[1], 1, ramp[1]];
    return ['interpolate', ['linear'], ['line-progress'], ...stops];
  };
  function colourRoute(kind) {
    for (const b of colourSeg.querySelectorAll('[data-colour]')) b.setAttribute('aria-pressed', String(b.dataset.colour === kind));
    if (m && m.map.getLayer('route-line')) m.map.setPaintProperty('route-line', 'line-gradient', gradient(kind));
  }
  // the page is ready once the map's foot is laid out (its credit and style switch, before MapLibre has arrived): the
  // overlays above them stay where they are, and the map fills in its place after
  let mapMade = null;
  const mapUp = new Promise((res) => { mapMade = res; });
  (async () => {
    if (coords.length < 2) { mapEl.append(mapFallback(trackErr ? `The route could not be loaded: ${trackErr.message}` : 'No route was recorded for this drive.')); mapMade(); return; }
    try {
      m = await createMap(mapEl, ctx, { bounds: routeBounds, onShell: mapMade });
      mapMade();
      if (!m) return;
      await m.ready;
      if (!ctx.alive()) return;
      m.map.addSource('route', { type: 'geojson', lineMetrics: true, data: { type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: coords } } });
      const line = { type: 'line', source: 'route', layout: { 'line-cap': 'round', 'line-join': 'round' } };
      m.addLayer({ ...line, id: 'route-casing', paint: { 'line-color': cssVar('--map-casing'), 'line-opacity': 0.85 } }, { 'line-width': () => rpx(8) });
      m.addLayer({ ...line, id: 'route-line', paint: { 'line-gradient': gradient('speed') } }, { 'line-width': () => rpx(4.2) });
      routePin(m, coords[0], 'A');
      routePin(m, coords[coords.length - 1], 'B');
      m.frame(() => m.map.fitBounds(routeBounds, { padding: padClear(mapEl, legend, { left: 40, top: 24, right: 60, bottom: 50 }), maxZoom: 16, duration: 0 }));
    } catch (e) { console.warn(e); replace(mapEl, mapFallback()); mapMade(); }
  })();
  function hoverAt(i) {
    const lat = series.values.lat?.[i], lon = series.values.lon?.[i];
    if (i == null || lat == null || !m) { readout.hidden = true; hoverMarker?.remove(); hoverMarker = null; return; }
    if (!hoverMarker) hoverMarker = new m.ml.Marker({ element: h('div', { class: 'car-marker car-marker--live hover-marker', 'aria-hidden': 'true' }) }).setLngLat([lon, lat]).addTo(m.map);
    else hoverMarker.setLngLat([lon, lat]);
    readout.hidden = false;
    replace(readout, h('b', null, formatTime(series.t_ms[i], tz, { seconds: true })), h('span', null, format(series.values.speed_kph?.[i], 'speed')),
      h('span', { class: 'faint' }, `GPS ±${format(series.values.accuracy_m?.[i], 'length')}`));
  }

  /* ---------- the charts */
  const x = series.t_ms.map((t) => t / 1000);
  const V = (f, kind) => (series.values[f] || []).map((v) => (v == null || typeof v !== 'number' ? null : kind ? convert(v, kind) : v));
  const raw = (f) => series.values[f] || [];
  const known = (f) => raw(f).some((v) => v != null);
  const heatVals = raw('battery_heat_kw').filter((v) => v != null);
  const heatKwh = heatVals.length ? series.t_ms.reduce((sum, t, i) => {
    const v = series.values.battery_heat_kw[i]; const dt = i ? (t - series.t_ms[i - 1]) / 3.6e6 : 0;
    return v == null || dt > 0.05 ? sum : sum + v * dt; }, 0) : null;
  const clim = raw('climate');
  const climSeen = [...new Set(clim.filter((v) => v != null))];
  const setLo = ['setpoint_left_c', 'setpoint_right_c'].some((f) => raw(f).some((v) => typeof v === 'string'));
  const tyreSeries = TYRES.map(([k, l]) => ({ label: l, data: V(`tyre_${k}_kpa`, 'pressure'), color: { fl: '--c-speed', fr: '--c-power', rl: '--c-soc', rr: '--warm' }[k] }));
  const defs = [
    { key: 'speed', name: 'Speed', unit: unitLabel('speed'), cover: 'dd.chart.drive.speed', series: [{ label: 'Speed', data: V('speed_kph', 'speed'), color: '--c-speed', area: true }], kind: 'speed' },
    { key: 'power', name: 'Battery power', unit: 'kW', cover: 'dd.chart.drive.power', split: true, series: [{ label: 'Power', data: V('power_kw'), color: '--c-power' }], kind: 'power', key2: 'regen below 0' },
    { key: 'soc', name: 'Battery', unit: '%', cover: 'dd.chart.drive.soc', series: [{ label: 'Battery', data: V('soc_pct'), color: '--c-soc' }, { label: 'Usable', data: V('usable_soc_pct'), color: '--c-power', dash: true }], kind: 'percent', digits: 1 },
    { key: 'range', name: ideal ? 'Ideal range' : 'Rated range', unit: unitLabel('range'), cover: 'dd.chart.drive.range', kind: 'range',
      series: [{ label: ideal ? 'Ideal' : 'Rated', data: V(rk, 'range'), color: '--live' }, { label: 'Estimate', data: V('est_range_km', 'range'), color: '--warm', dash: true }],
      note: estWh && known('energy_remaining_kwh') ? `The estimate is Sentry Mate's own: the energy left in the pack ÷ ${format(estWh, 'efficiency')}, the car's consumption over the 30 days before this drive.`
        : isImported(d) && !d.sources?.some((x) => x.kind === 'smo') ? 'No estimate: imported history has no energy left in the pack.' : 'No estimate: this car does not send the energy left in the pack.' },
    { key: 'elev', name: 'Elevation', unit: unitLabel('length'), cover: 'dd.chart.elevation', series: [{ label: 'Elevation', data: V('elevation_m', 'length'), color: '--c-elev', area: true }], kind: 'length' },
    { key: 'temps', name: 'Climate', unit: unitLabel('temperature'), cover: ['dd.chart.temps.air', 'dd.chart.temps.setpoints'], kind: 'temperature', digits: 1,
      series: [{ label: 'Outside', data: V('outside_c', 'temperature'), color: '--c-power' }, { label: 'Cabin', data: V('cabin_c', 'temperature'), color: '--warm' },
        { label: 'Set left', data: V('setpoint_left_c', 'temperature'), raw: raw('setpoint_left_c'), color: '--c-soc', dash: true }, { label: 'Set right', data: V('setpoint_right_c', 'temperature'), raw: raw('setpoint_right_c'), color: '--c-elev', dash: true }],
      note: known('setpoint_left_c') ? `Set points are the driver's and passenger's choices${setLo ? '; LO and HI (full cooling, full heat) are named in the readout and the CSV and leave a gap in the line' : ''}.` : 'This car does not send its set points, only the air temperatures.' },
    { key: 'hvac', name: 'Climate system', unit: 'fan step 0–10', cover: 'dd.chart.temps.climate_state', kind: null, digits: 0, bands: clim,
      series: [{ label: 'Fan', data: V('fan_level'), color: '--live', step: true }],
      keys: Object.entries(CLIMATE).filter(([k, [, c]]) => c && climSeen.includes(k)).map(([k, [l]]) => h('span', { class: 'key key--band', dataset: { climate: k } }, h('i'), l)),
      note: !known('climate') ? 'This car does not send the climate mode or fan step.'
        : `Shaded while the climate system was on${climSeen.includes('precondition') ? ', warm while preconditioning' : ''}${climSeen.includes('overheat_protect') ? ', red for overheat protection' : ''}; the line is the fan step (0 is off).` },
    { key: 'heat', name: 'Battery heating', unit: 'kW', cover: 'dd.chart.drive.battery_heater', kind: 'power', digits: 1,
      series: [{ label: 'Heating', data: V('battery_heat_kw'), color: '--warm', area: true }],
      note: !heatVals.length ? 'This car does not send battery heating power.'
        : heatVals.some((v) => v > 0) ? `Power the pack spent warming itself: about ${format(heatKwh, 'energy')} on this drive, part of the energy used.`
          : 'The pack did not heat itself on this drive (0 kW all the way).' },
    { key: 'tyres', name: 'Tyres', unit: unitLabel('pressure'), cover: 'dd.chart.tire_pressure', kind: 'pressure', digits: 2, series: tyreSeries, showX: true,
      note: TYRES.some(([k]) => known(`tyre_${k}_kpa`)) ? null : isImported(d) ? 'Not in imported history: these drives came without tyre pressures.' : apiOnly(d) ? 'Not read from the Tesla API for this drive.' : 'This car does not send tyre pressures.' },
  ];
  const xMin = x[0], xMax = x[x.length - 1];
  const plots = [];
  let zoom = [xMin, xMax];
  const vals = [];
  const blocks = defs.map((df) => {
    const valEl = h('span', { class: 'val' });
    vals.push(valEl);
    const target = h('div', { class: 'chart' });
    const block = h('div', { class: 'chart-block', dataset: { series: df.key }, cover: df.cover },
      h('div', { class: 'chart-label' }, h('span', { class: 'name' }, df.name, h('span', { class: 'chart-unit' }, ` · ${df.unit}`)),
        df.series.map((s) => h('span', { class: 'key' }, h('i', { vars: { '--c': `var(${s.color})` } }), s.label)), df.keys || null, valEl),
      target, df.note ? h('p', { class: 'chart-note' }, df.note) : null);
    return { df, block, target, valEl };
  });
  replace(chartsHost, blocks.map((b) => b.block));
  mapUp.then(() => ctx.ready());
  const idle = () => blocks.forEach(({ df, valEl }) => { valEl.textContent = ''; });
  idle();

  // the selection's figures, from the samples inside the zoom
  function paintSelection() {
    const [a, b] = zoom;
    const idx = x.map((t, i) => (t >= a && t <= b ? i : -1)).filter((i) => i >= 0);
    const sp = idx.map((i) => series.values.speed_kph?.[i]).filter((v) => v != null);
    const odo = idx.map((i) => series.values.odometer_km?.[i]).filter((v) => v != null);
    const el = idx.map((i) => series.values.elevation_m?.[i]).filter((v) => v != null);
    const zoomed = a > xMin + 1 || b < xMax - 1;
    resetBtn.hidden = !zoomed;
    // not zoomed: the drive's own figures, so both boxes always agree
    const [up, down] = el.length > 1 ? climbDescent(el) : [null, null];
    const dur = zoomed ? (b - a) * 1000 : (live ? ctx.api.now() - t0 : d.duration_ms);
    const dist = zoomed ? (odo.length > 1 ? odo[odo.length - 1] - odo[0] : null) : d.distance_km;
    const avg = zoomed ? (dist != null && dur > 0 ? dist / (dur / 3.6e6) : null) : (live ? (dur > 0 ? d.distance_km / (dur / 3.6e6) : null) : d.speed_avg_kph);
    replace(selBox, h('span', { class: 'selection-title' }, zoomed ? `Selection ${formatTime(a * 1000, tz)} – ${formatTime(b * 1000, tz)}` : 'The whole drive'),
      metric('Duration', dur, 'duration', { size: 'sm' }),
      metric('Distance', dist, 'distance', { size: 'sm', why: 'No odometer here' }),
      metric('Average speed', avg, 'speed', { size: 'sm', explain: AVG_HOW, why: sp.length ? 'No odometer here' : 'No speed here' }),
      metric('Climb', zoomed ? up : d.climb_m, 'length', { size: 'sm', sign: true, explain: zoomed ? `steps of ${format(ELEV_STEP_M, 'length')} or more` : null }),
      metric('Descent', zoomed ? (down != null ? -down : null) : (d.descent_m != null ? -d.descent_m : null), 'length', { size: 'sm' }));
  }
  const zoomTo = (a, b) => { zoom = [a, b]; for (const p of plots) p.u?.setScale('x', { min: a, max: b }); paintSelection(); };
  resetBtn.addEventListener('click', () => zoomTo(xMin, xMax));
  paintSelection();
  repaintSel = paintSelection;

  const splitGrad = (u, c1, c2, a) => {
    const { top, height } = u.bbox;
    const f = clamp((u.valToPos(0, 'y', true) - top) / height, 0, 1);
    const g = u.ctx.createLinearGradient(0, top, 0, top + height);
    g.addColorStop(0, hexA(c1, a)); g.addColorStop(f, hexA(c1, a)); g.addColorStop(f, hexA(c2, a)); g.addColorStop(1, hexA(c2, a));
    return g;
  };
  // the climate mode as shaded bands behind the fan line, one run of the same mode at a time
  function drawBands(u, modes) {
    const { left, top, width, height } = u.bbox;
    const cx = u.ctx;
    cx.save();
    cx.beginPath(); cx.rect(left, top, width, height); cx.clip();
    let i = 0;
    while (i < modes.length) {
      let j = i;
      while (j + 1 < modes.length && modes[j + 1] === modes[i]) j++;
      const tok = CLIMATE[modes[i]]?.[1];
      if (tok) {
        const a = u.valToPos(x[i], 'x', true);
        const b = u.valToPos(x[Math.min(j + 1, x.length - 1)], 'x', true);
        cx.fillStyle = hexA(cssVar(tok), 0.16);
        cx.fillRect(a, top, Math.max(1, b - a), height);
      }
      i = j + 1;
    }
    cx.restore();
  }
  blocks.forEach(({ df, target }) => {
    const make = (width, height) => {
      const c = colors();
      return {
        width, height, padding: [rpx(10), rpx(14), df.showX ? 0 : rpx(6), 0], legend: { show: false },
        cursor: { sync: { key: `drive-${did}`, setSeries: false }, drag: { x: true, y: false, setScale: false }, points: { size: rpx(8), width: rpx(2), stroke: c.cursor } },
        select: { show: true },
        scales: { x: { time: true }, y: { auto: true, range: df.key === 'speed' ? (u, a, b) => [0, Math.max(10, (b || 0) * 1.15)] : df.key === 'hvac' ? () => [0, 10] : df.key === 'heat' ? (u, a, b) => [0, Math.max(1, (b || 0) * 1.15)] : undefined } },
        axes: [timeAxis(tz, { show: !!df.showX }), valueAxis({ size: 56, digits: df.digits ?? 0 })],
        series: [{}, ...df.series.map((s) => {
          const col = cssVar(s.color);
          if (df.split) return { label: s.label, stroke: (u) => splitGrad(u, col, cssVar('--c-regen'), 1), fill: (u) => splitGrad(u, col, cssVar('--c-regen'), 0.16), fillTo: () => 0, width: rpx(2), points: { show: false } };
          return { label: s.label, stroke: col, width: rpx(s.dash ? 1.5 : 2), dash: s.dash ? [rpx(4), rpx(3)] : undefined, fill: s.area ? areaFill(col) : undefined, paths: s.step ? window.uPlot.paths.stepped({ align: 1 }) : undefined, points: { show: false } };
        })],
        hooks: {
          drawClear: df.bands ? [(u) => drawBands(u, df.bands)] : [],
          setSelect: [(u) => {
            if (u.select.width < rpx(6)) return;
            const a = u.posToVal(u.select.left, 'x'), b = u.posToVal(u.select.left + u.select.width, 'x');
            u.setSelect({ left: 0, top: 0, width: 0, height: 0 }, false);
            zoomTo(a, b);
          }],
          setCursor: df.key === 'speed' ? [(u) => {
            const i = u.cursor.idx;
            if (i == null || u.cursor.left < 0) { timeEl.textContent = ''; idle(); hoverAt(null); return; }
            timeEl.textContent = formatTime(series.t_ms[i], tz, { seconds: true });
            blocks.forEach(({ df: g, valEl }) => {
              const parts = g.series.map((s) => setWord(s.raw?.[i]) || (s.data[i] == null ? '—' : formatNumber(s.data[i], { maximumFractionDigits: g.digits ?? 0 })));
              const mode = g.bands ? (CLIMATE[g.bands[i]]?.[0] || 'Climate unknown') : null;
              valEl.textContent = (mode ? `${mode} · fan ` : '') + parts.join(' / ') + (g.bands ? '' : ` ${g.unit}`);
            });
            hoverAt(i);
          }] : [],
        },
      };
    };
    plots.push(mountPlot(target, make, [x, ...df.series.map((s) => s.data)], ctx, (u) => {
      u.over.addEventListener('dblclick', () => zoomTo(xMin, xMax));
      if (zoom[0] > xMin || zoom[1] < xMax) u.setScale('x', { min: zoom[0], max: zoom[1] });
    }));
  });
}
