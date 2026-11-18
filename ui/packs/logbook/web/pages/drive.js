// Sentry Mate, "Logbook" UI pack: Drive detail (/cars/{car}/drives/{drive}).
//
// One drive in full: the route, the charts along it, its figures and the export. Data:
// GET /api/v1/cars/{car}/drives/{drive} (the drive, previous and next, speed bands, gaps),
// /drives/{drive}/track?format=geojson (the route at 1 Hz) and /series for the drive's span (speed, power,
// battery, range, elevation, temperatures, tyres, position for the hover marker). Zooming a chart narrows
// the figures in "Selected" and highlights that part of the route.

import { h, replace } from '../shared/dom.js';
import { format, parts, rangeField, rangeLabel } from '../shared/units.js';
import { formatTime, formatDate, formatDuration } from '../shared/time.js';
import { formatList } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { pageHead, q, figure, icon, sep, empty, errorView, flag } from '../core/ui.js';
import { makeMap, boundsOf } from '../core/map.js';
import { stackedCharts } from '../core/chart.js';
import { placeLink } from '../core/journal.js';

export const id = 'drive';
export const css = true;
export const title = () => 'Drive';

const FIELDS = 'speed_kph,power_kw,soc_pct,usable_soc_pct,range_km,ideal_range_km,elevation_m,outside_c,cabin_c,setpoint_left_c,tyre_fl_kpa,tyre_fr_kpa,tyre_rl_kpa,tyre_rr_kpa,lat,lon,odometer_km';

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz;
  const base = api.apiPath`cars/${car.id}/drives/${ctx.driveId}`;
  const d = await ctx.api.get(base);
  const end = d.end_ms ?? api.now();
  const [track, series] = await Promise.all([
    ctx.api.get(`${base}/track`, { format: 'geojson' }).catch(() => null),
    ctx.api.get(api.apiPath`cars/${car.id}/series`, { fields: FIELDS, from_ms: d.start_ms, to_ms: end + 1, max_points: 3000 }).catch(() => null),
  ]);
  ctx.title(`${d.start.label} to ${d.end.label}`);
  const going = d.end_ms == null;
  const link = (id) => (id ? state.buildUrl('drive', { car: car.id, drive: id, period: ctx.state.period }) : null);
  const step = (href, text, dir) => (href
    ? h('a', { class: 'button button-quiet', href }, dir < 0 ? icon('chevronLeft') : null, text, dir > 0 ? icon('chevronRight') : null)
    : h('span', { class: 'button button-quiet is-disabled', 'aria-disabled': 'true' }, dir < 0 ? icon('chevronLeft') : null, text, dir > 0 ? icon('chevronRight') : null));
  // the end of a drive still going is where the car is now, not a place to name
  const placeA = (p, still = false) => (!still && placeLink(p) ? h('a', { href: placeLink(p), title: p.place ? 'Edit this place' : 'Name this place' }, p.label) : p.label);

  const head = pageHead({
    eyebrow: h('nav', { class: 'crumbs', 'aria-label': 'Breadcrumb', cover: ['dd.context.drive', 'dd.filter.car'] },
      h('a', { href: state.buildUrl('car', { car: car.id }) }, car.name), h('span', { class: 'crumb-sep', 'aria-hidden': 'true' }, '/'),
      h('a', { href: state.buildUrl('drives', { car: car.id, period: ctx.state.period }) }, 'Drives'), h('span', { class: 'crumb-sep', 'aria-hidden': 'true' }, '/'),
      h('span', { 'aria-current': 'page' }, formatDate(d.start_ms, tz, { weekday: 'long', month: 'long', year: true }))),
    title: [d.start.label, h('span', { class: 'to' }, ' to '), d.end.label],
    deck: [`${formatTime(d.start_ms, tz)} – ${going ? 'now' : formatTime(d.end_ms, tz)}`, sep(), formatDuration(d.duration_ms), sep(),
      placeA(d.start), ' to ', placeA(d.end, going),
      d.source === 'teslamate' ? [' ', flag('Imported', { kind: 'estimated' })] : null, going ? [' ', flag('In progress')] : null],
    aside: h('nav', { class: 'step-nav', 'aria-label': 'Other drives' },
      step(link(d.prev), 'Earlier', -1), step(link(d.next), 'Later', 1),
      h('a', { class: 'button button-quiet', href: state.buildUrl('trip', { car: car.id, period: { from: d.start_ms, to: end } }) }, 'As a trip')),
    cls: 'drive-head',
  });

  // Figures: each blank one says why.
  const why = (cond, text) => (cond ? text : null);
  const bands = (d.speed_bands || []).filter((b) => b.seconds > 0);
  const stats = h('dl', { class: 'drive-stats', cover: ['dd.stat.odometer', 'dd.stat.drive_duration', 'dd.stat.distance', 'dd.stat.elevation_up', 'dd.stat.elevation_down', 'dd.stat.energy_net', 'dd.stat.energy_recovered', 'dd.stat.consumption_net', 'dd.stat.speed_avg', 'dd.empty_states'] },
    figure('Distance', q(d.distance_km, 'distance'), `odometer ${format(d.start_odometer_km, 'odometer')} → ${format(d.end_odometer_km, 'odometer')}`),
    figure('Duration', formatDuration(d.duration_ms), going ? 'so far' : null),
    figure('Average speed', q(d.speed_avg_kph, 'speed'), `top ${format(d.speed_max_kph, 'speed')}`),
    figure('Climb and descent', h('span', null, h('span', { class: 'updown' }, '↑ ', q(d.climb_m, 'length', { why: why(d.climb_m == null, 'no elevation recorded') })), ' ', h('span', { class: 'updown' }, '↓ ', q(d.descent_m, 'length')))),
    figure('Energy used', q(d.energy_kwh, 'energy', { why: why(d.energy_kwh == null, 'no pack readings for this drive') }), d.energy_basis === 'estimated' ? 'estimated from range (imported)' : 'measured at the pack, regen taken off'),
    figure('Regenerated', q(d.regen_kwh, 'energy', { why: why(d.regen_kwh == null, d.source === 'teslamate' ? 'not in imported history' : 'not recorded') })),
    figure('Consumption', q(d.wh_per_km, 'efficiency', { why: why(d.wh_per_km == null, 'too short to be reliable') }), d.rated_wh_per_km ? `${format(d.efficiency_pct, 'percent')} of the rated ${format(d.rated_wh_per_km, 'efficiency')}` : null),
    figure('Climate', q(d.hvac_kwh, 'energy', { why: why(d.hvac_kwh == null, 'not sent by this car') }), 'heating and cooling'),
    figure('Battery', `${format(d.start_soc_pct, 'percent')} → ${format(d.end_soc_pct, 'percent')}`, d.start_soc_pct != null && d.end_soc_pct != null ? `${format(d.start_soc_pct - d.end_soc_pct, 'number', { digits: 1 })} points used` : null),
    figure(rangeLabel(), `${format(d[rangeField('start_range_km')] ?? d.start_range_km, 'range')} → ${format(d[rangeField('end_range_km')] ?? d.end_range_km, 'range')}`),
    figure('Outside', q(d.outside_avg_c, 'temperature'), `cabin ${format(d.cabin_avg_c, 'temperature')}`));
  const bandsEl = bands.length ? h('div', { class: 'drive-bands', cover: 'dd.chart.speed_histogram' },
    h('p', { class: 'label' }, 'Time in each speed band'),
    h('ul', { class: 'band-list' }, bands.map((b) => h('li', { class: 'band-row' },
      h('span', { class: 'band-name' }, `${parts(b.from_kph, 'speed').number} to ${format(b.to_kph, 'speed')}`),
      h('span', { class: 'band-value' }, formatDuration(b.seconds * 1000)))))) : null;

  // Export: GPX or GeoJSON, with the extras chosen.
  const extras = [['speed', 'Speed'], ['power', 'Power'], ['soc', 'Battery %'], ['accuracy', 'GPS accuracy']];
  const checks = extras.map(([k, t]) => h('label', { class: 'check' }, h('input', { type: 'checkbox', value: k }), t));
  const gpx = h('a', { class: 'button button-small', download: '' }, icon('download'), 'GPX');
  const geo = h('a', { class: 'button button-quiet button-small', download: '' }, icon('download'), 'GeoJSON');
  const setLinks = () => {
    const ext = checks.map((c) => c.querySelector('input')).filter((i) => i.checked).map((i) => i.value).join(',');
    gpx.setAttribute('href', ctx.api.url(`${base}/track`, { format: 'gpx', extensions: ext || undefined }));
    geo.setAttribute('href', ctx.api.url(`${base}/track`, { format: 'geojson', extensions: ext || undefined }));
  };
  checks.forEach((c) => c.addEventListener('change', setLinks));
  setLinks();
  const exportEl = h('section', { class: 'drive-export', cover: ['dd.export.gpx', 'web.drive-gpx.entry', 'web.drive-gpx.file', 'web.drive-gpx.point-coordinates', 'web.drive-gpx.point-elevation', 'web.drive-gpx.point-time', 'web.drive-gpx.not-found'] },
    h('p', { class: 'label' }, 'Export the route'),
    h('p', { class: 'note' }, 'Every point with its time, position and elevation; tick extras to add them.'),
    h('div', { class: 'checks' }, checks), h('div', { class: 'export-buttons' }, gpx, geo));

  const lines = track && track.features ? track.features.map((f) => f.geometry.coordinates.map((c) => [c[0], c[1]])) : [];
  const all = lines.flat();
  const mapBox = h('div', { class: 'map map-drive', role: 'region', 'aria-label': `Map of the route from ${d.start.label} to ${d.end.label}`, cover: 'dd.map.route' });
  const readout = h('p', { class: 'readout', 'aria-live': 'off' });
  const selected = h('p', { class: 'selected-note', cover: 'dd.stat.selected_duration' });
  const resetBtn = h('button', { type: 'button', class: 'button button-quiet button-small', disabled: true }, icon('reset'), 'Show the whole drive');
  const charts = h('div', { class: 'charts', cover: ['dd.chart.drive', 'dd.chart.drive.tooltip', 'dd.chart.drive.speed', 'dd.chart.drive.power', 'dd.chart.drive.range', 'dd.chart.drive.soc', 'dd.chart.elevation', 'dd.chart.temps.air', 'dd.chart.temps.setpoints', 'dd.chart.tire_pressure', 'dd.control.zoom'] });
  ctx.main.append(head, h('div', { class: 'drive-body' },
    h('div', { class: 'drive-top' }, mapBox, h('section', { class: 'drive-side', 'aria-label': 'Drive figures' }, stats, bandsEl, exportEl)),
    h('section', { class: 'along', 'aria-labelledby': 'along-title' },
      h('div', { class: 'along-head' }, h('h2', { class: 'label', id: 'along-title' }, 'Along the drive'), readout, resetBtn),
      selected, charts,
      d.gaps && d.gaps.length ? h('p', { class: 'note' }, `No data for ${formatList(d.gaps.map((g) => `${formatTime(g.start_ms, tz)} to ${formatTime(g.end_ms, tz)}`))}; the lines break there.`) : null,
      h('p', { class: 'along-hint' }, 'Point at a chart to read every value at that moment and see where the car was. Drag across a chart to zoom in; the figures under “Selected” follow.'))));

  // The map: the route, start and end pins, and the hover marker.
  let marker = null, markerEl = null, m = null;
  if (all.length > 1) {
    m = await makeMap(ctx, mapBox, { bounds: boundsOf(all), padding: 56, maxZoom: 17, fullscreen: true });
    if (m) {
      await m.loaded;
      lines.forEach((c, i) => m.addRoute(`route-${i}`, c));
      m.map.addSource('sel', { type: 'geojson', data: { type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: [] } } });
      m.addLine({ id: 'sel-line', source: 'sel' }, 6.5, '--accent-strong');
      m.addPin(all[0], d.start.label, 'pin-start');
      if (!going) m.addPin(all[all.length - 1], d.end.label, 'pin-end');
      markerEl = h('div', { class: 'car-dot is-hidden' });
      marker = new m.ml.Marker({ element: markerEl, anchor: 'center' }).setLngLat(all[0]).addTo(m.map);
    }
  } else replace(mapBox, h('p', { class: 'map-note' }, 'No route was recorded for this drive.'));

  // The charts, on one time axis, with one readout.
  const s = series;
  const val = (f, i) => (s && s.values[f] ? s.values[f][i] : null);
  const showReadout = (i) => {
    if (i == null || !s) {
      replace(readout, h('span', { class: 'readout-idle' }, s ? `${s.t_ms.length} points, ${formatTime(d.start_ms, tz)} to ${going ? 'now' : formatTime(d.end_ms, tz)}${s.bucket_ms > 1000 ? `, one per ${formatDuration(s.bucket_ms)}` : ', one a second'}` : ''));
      return;
    }
    replace(readout,
      h('time', { class: 'readout-time' }, formatTime(s.t_ms[i], tz, { seconds: true })),
      h('span', null, h('b', null, format(val('speed_kph', i), 'speed'))),
      h('span', null, h('b', null, format(val('power_kw', i), 'power'))),
      h('span', null, h('b', null, format(val('soc_pct', i), 'percent', { digits: 1 }))),
      h('span', null, h('b', null, format(val('elevation_m', i), 'length'))));
  };
  showReadout(null);
  const onHover = (i) => {
    showReadout(i);
    if (!marker) return;
    const lat = val('lat', i), lon = val('lon', i);
    if (i == null || lat == null) { markerEl.classList.add('is-hidden'); return; }
    markerEl.classList.remove('is-hidden');
    marker.setLngLat([lon, lat]);
  };
  const onZoom = (from, to) => {
    const full = !s || (from <= s.t_ms[0] + 500 && to >= s.t_ms[s.t_ms.length - 1] - 500);
    resetBtn.disabled = full;
    const idx = s ? s.t_ms.map((t, i) => (t >= from && t <= to ? i : -1)).filter((i) => i >= 0) : [];
    if (full || idx.length < 2) { selected.textContent = ''; if (m && m.map.getSource('sel')) m.map.getSource('sel').setData({ type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: [] } }); return; }
    const odo = idx.map((i) => val('odometer_km', i)).filter((x) => x != null);
    const dist = odo.length > 1 ? odo[odo.length - 1] - odo[0] : null;
    selected.textContent = `Selected: ${formatTime(s.t_ms[idx[0]], tz)} to ${formatTime(s.t_ms[idx[idx.length - 1]], tz)}, ${formatDuration(s.t_ms[idx[idx.length - 1]] - s.t_ms[idx[0]])}, ${format(dist, 'distance')}${dist ? `, average ${format((dist / ((s.t_ms[idx[idx.length - 1]] - s.t_ms[idx[0]]) / 3600000)), 'speed')}` : ''}.`;
    const coords = idx.map((i) => [val('lon', i), val('lat', i)]).filter((c) => c[0] != null);
    if (m && m.map.getSource('sel')) m.map.getSource('sel').setData({ type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: coords } });
  };
  if (s && s.t_ms.length > 1) {
    const rf = rangeField('range_km');
    const tyres = ['tyre_fl_kpa', 'tyre_fr_kpa', 'tyre_rl_kpa', 'tyre_rr_kpa'];
    const hasTyres = tyres.some((t) => (s.values[t] || []).some((x) => x != null));
    const rows = [
      { key: 'speed', name: 'Speed', kind: 'speed', values: s.values.speed_kph, colour: '--accent', fill: '--chart-wash', floor: 0, ticks: 4 },
      { key: 'power', name: 'Power', kind: 'power', note: 'use above zero, regen below', values: s.values.power_kw, colour: '--ink-2', signed: true, ticks: 4 },
      { key: 'soc', name: 'Battery', kind: 'percent', note: 'usable dashed', values: s.values.soc_pct, extra: [{ values: s.values.usable_soc_pct, colour: '--ink-3', dash: true }], colour: '--ink', ticks: 4 },
      { key: 'range', name: rangeLabel(), kind: 'range', values: s.values[rf] || s.values.range_km, colour: '--ink-2', ticks: 4 },
      { key: 'temp', name: 'Temperatures', kind: 'temperature', note: 'outside, cabin, set point dashed', values: s.values.outside_c, colour: '--slate', extra: [{ values: s.values.cabin_c, colour: '--ochre' }, { values: s.values.setpoint_left_c, colour: '--ochre', dash: true }], ticks: 4 },
      hasTyres ? { key: 'tyres', name: 'Tyres', kind: 'pressure', note: 'all four', values: s.values.tyre_fl_kpa, extra: tyres.slice(1).map((t) => ({ values: s.values[t], colour: '--ink-3' })), colour: '--ink-2', ticks: 3 } : null,
      { key: 'elev', name: 'Elevation', kind: 'length', values: s.values.elevation_m, colour: '--ink-3', fill: '--chart-sunk', ticks: 4 },
    ].filter(Boolean);
    const plotted = await stackedCharts(ctx, charts, { tz, t_ms: s.t_ms, states: s.state, rows, onHover, onZoom });
    if (plotted) resetBtn.addEventListener('click', () => plotted.resetZoom());
  } else replace(charts, empty({ title: 'No readings along this drive.', text: d.source === 'teslamate' ? 'Imported drives keep only their totals.' : 'The log for this drive holds no samples.' }));
  if (m) await Promise.race([m.ready, new Promise((r) => setTimeout(r, 5000))]);
  ctx.ready();
  return {};
}
