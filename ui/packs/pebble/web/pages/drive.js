// Sentry Mate, "Pebble" UI pack: Drive (/cars/{car}/drives/{drive}), one drive in full.
//
// A full-width map tile with the route, then number tiles (distance, duration, energy, use, top speed, temperatures,
// battery start to end, climb and descent), a speed and power chart and a battery and elevation chart that share
// one cursor and one zoom (a dot on the map follows the cursor; a zoom shows "Selected part" figures), the climate
// and tyre charts, time at each speed, and the GPX export. Earlier and Later step through the car's drives.
// Data: GET /api/v1/cars/{car}/drives/{drive}, /drives/{drive}/track, /series over the drive.

import { h } from '../shared/dom.js';
import { format, convert, unitLabel, rangeLabel, rangeValue } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, dayKey } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { tile, num, chip, linkBtn, icon, note } from '../core/ui.js';
import { emptyTile } from '../core/states.js';
import { mapTile, boundsOf } from '../core/map.js';
import { trackBounds } from '../shared/tracks.js';
import { timeChart, bars } from '../core/chart.js';
import { endLabel, placeLink, statTile, socBar, facts } from '../core/bits.js';
import { srcPill, estPill, srcText, isEstimated, apiOnly, EST_WHY } from '../core/source.js';

export const id = 'drive';
export const css = true;
export const title = () => 'Drive';

const FIELDS = 'speed_kph,power_kw,soc_pct,range_km,ideal_range_km,battery_heat_kw,elevation_m,outside_c,cabin_c,setpoint_left_c,climate,tyre_fl_kpa,tyre_fr_kpa,tyre_rl_kpa,tyre_rr_kpa,lat,lon';
const nums = (l) => (l || []).map((x) => (typeof x === 'number' && Number.isFinite(x) ? x : null));
const has = (l) => (l || []).some((x) => x != null);

// The page's reads; prefetch() warms the same ones when a link here is under the pointer.
const driveBase = (ctx) => api.apiPath`cars/${ctx.car.id}/drives/${ctx.driveId}`;
const driveRead = (ctx) => ctx.api.get(driveBase(ctx), { speed_band_kph: unitLabel('speed') === 'mph' ? 16.09344 : 10 });
const trackRead = (ctx) => ctx.api.get(`${driveBase(ctx)}/track`, { format: 'geojson' });
const seriesRead = (ctx, d) => ctx.api.get(api.apiPath`cars/${ctx.car.id}/series`, { fields: FIELDS, from_ms: d.start_ms, to_ms: (d.end_ms ?? api.now()) + 1, max_points: 2000 });
export async function prefetch(ctx) {
  const [d] = await Promise.all([driveRead(ctx), trackRead(ctx)]);
  if (d) await seriesRead(ctx, d);
}

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz;
  const base = driveBase(ctx);
  let d;
  try { d = await driveRead(ctx); } catch (err) {
    if (err && err.status === 404) {
      ctx.head({ title: 'No such drive' });
      ctx.view.replaceChildren(emptyTile({ icon: 'route', title: 'There is no drive with this number', text: 'It may have been rebuilt under a new number, or the link is wrong. Its route export says “not found” too.',
        action: linkBtn('Find it on Drives', state.buildUrl('drives', { car: car.id, period: ctx.state.period }), { kind: 'primary', icon: 'route' }), cover: ['dd.empty_states', 'web.drive-gpx.not-found'] }));
      ctx.ready();
      return {};
    }
    throw err;
  }
  const going = d.end_ms == null;
  const end = d.end_ms ?? api.now();
  const keep = (p) => p.catch((err) => ({ error: err }));
  const [track, s] = await Promise.all([
    keep(trackRead(ctx)),
    keep(seriesRead(ctx, d)),
  ]);
  if (!ctx.alive()) return {};
  const ser = s.error ? null : s;
  const titleText = `${endLabel(d.start)} to ${going ? 'now' : endLabel(d.end)}`;
  ctx.title(titleText);
  const step = (did, text, ic, rel) => (did ? linkBtn(text, state.buildUrl('drive', { car: car.id, drive: did, period: ctx.state.period }), { kind: 'soft', icon: ic, cls: `step-${rel}` })
    : h('span', { class: 'btn btn-soft is-off', 'aria-disabled': 'true', title: rel === 'prev' ? 'The first drive recorded' : 'The newest drive' }, icon(ic), h('span', { class: 'btn-text' }, text)));
  ctx.head({ title: titleText, actions: [step(d.prev, 'Earlier', 'chevronLeft', 'prev'), step(d.next, 'Later', 'chevronRight', 'next')] });

  /* ------------------------------------------------------------ context line */
  const ctxBar = h('div', { class: 'page-bar w-full drive-ctx', cover: ['dd.context.drive', 'dd.filter.car', 'shots.drive-details'] },
    h('a', { class: 'fpill fpill-ic', href: state.buildUrl('drives', { car: car.id, period: ctx.state.period }) }, icon('chevronLeft'), `${car.name}’s drives`),
    h('span', { class: 'drive-when' }, `${formatDate(d.start_ms, tz, { weekday: 'long', month: 'long', now: api.now() })}, ${formatTime(d.start_ms, tz)} – ${going ? 'now' : formatTime(d.end_ms, tz)}`),
    going ? chip('In progress', { tone: 'coral' }) : null,
    d.flags?.reduced_range ? chip('Reduced range', { tone: 'sun' }) : null,
    srcPill(d), estPill(d),
    d.gaps?.length ? chip(`${d.gaps.length} gap${d.gaps.length > 1 ? 's' : ''} in the data`, { tone: 'bad' }) : null);

  /* ------------------------------------------------------------ map */
  const coords = track.error ? [] : (track.features || []).flatMap((f) => (f.geometry?.coordinates || []));
  const mapBox = h('div', { class: 'map map-tall' });
  const mapT = tile({ w: 'full', title: 'Route', icon: 'map', cls: 'map-tile', cover: 'dd.map.route', side: h('span', { class: 'route-words item-sub' }, placeLink(d.start), icon('arrowRight', 'route-arrow'), placeLink(d.end, { still: going })) }, mapBox);

  /* ------------------------------------------------------------ figures */
  const rangeName = rangeLabel();
  const figs = [
    statTile({ label: 'Distance', value: d.distance_km, kind: 'distance', icon: 'route', cover: 'dd.stat.distance', sub: `${format(d.start_odometer_km, 'odometer')} to ${format(d.end_odometer_km, 'odometer')}` }),
    statTile({ label: 'Duration', value: d.duration_ms, kind: 'duration', icon: 'clock', cover: 'dd.stat.drive_duration', sub: `Avg ${format(d.speed_avg_kph, 'speed')}` }),
    statTile({ label: 'Energy', value: d.energy_kwh, kind: 'energy', icon: 'bolt', cover: ['dd.stat.energy_net', 'dd.stat.energy_recovered'], sub: isEstimated(d) ? 'estimated: Tesla API readings added up' : `${format(d.regen_kwh, 'energy')} won back braking` }),
    statTile({ label: 'Use', value: d.wh_per_km, kind: 'efficiency', icon: 'leaf', cover: 'dd.stat.consumption_net', sub: d.efficiency_pct != null ? `${format(d.efficiency_pct, 'percent')} of rated` : null }),
    statTile({ label: 'Top speed', value: d.speed_max_kph, kind: 'speed', icon: 'gauge', cover: 'dd.stat.speed_avg', sub: `avg ${format(d.speed_avg_kph, 'speed')}, peak ${format(d.power_max_kw, 'power')}` }),
    statTile({ label: 'Outside', value: d.outside_avg_c, kind: 'temperature', icon: 'thermo', sub: `inside ${format(d.cabin_avg_c, 'temperature')}` }),
    tile({ w: 1, title: 'Battery', icon: 'battery', cls: 'tile-stat' }, num((d.start_soc_pct ?? 0) - (d.end_soc_pct ?? 0), 'percent', { why: 'not known' }),
      socBar(d.start_soc_pct, d.end_soc_pct), h('p', { class: 'tile-line' }, `${rangeName}: ${format(rangeValue(d, 'start_range_km'), 'range')} to ${format(rangeValue(d, 'end_range_km'), 'range')}`)),
    statTile({ label: 'Climb', value: d.climb_m, kind: 'length', icon: 'mountain', cover: ['dd.stat.elevation_up', 'dd.stat.elevation_down'], sub: `down ${format(d.descent_m, 'length')}` }),
  ];
  const selected = tile({ w: 2, title: 'Selected part', icon: 'search', cls: 'sel-tile', cover: ['dd.stat.selected_duration', 'dd.control.zoom', 'dd.empty_states'] },
    h('p', { class: 'note' }, 'Drag across a chart to pick a part of the drive; double-click to see it all again.'));
  const odo = tile({ w: 2, title: 'More figures', icon: 'info', cover: 'dd.stat.odometer' },
    facts([
      ['Odometer', `${format(d.start_odometer_km, 'odometer')} → ${format(d.end_odometer_km, 'odometer')}`],
      ['Climate used', format(d.hvac_kwh, 'energy')],
      ['Lowest power', format(d.power_min_kw, 'power')],
      ['Energy', isEstimated(d) ? (apiOnly(d) ? 'estimated: the Tesla API’s power readings added up' : 'estimated') : 'measured'],
      ['Data', srcText(d) || 'not known'],
    ]), isEstimated(d) && apiOnly(d) ? note(EST_WHY) : null);

  /* ------------------------------------------------------------ charts */
  const sync = `drive-${d.id}-${Date.now()}`;
  const cSpeed = tile({ w: 2, title: 'Speed and power', icon: 'gauge', cls: 'chart-tile', cover: ['dd.chart.drive', 'dd.chart.drive.tooltip', 'dd.chart.drive.speed', 'dd.chart.drive.power'] });
  const cSoc = tile({ w: 2, title: 'Battery and elevation', icon: 'mountain', cls: 'chart-tile', cover: ['dd.chart.drive.soc', 'dd.chart.drive.range', 'dd.chart.elevation', 'dd.chart.drive.battery_heater'] });
  const cTemp = tile({ w: 2, title: 'Climate', icon: 'thermo', cls: 'chart-tile', cover: ['dd.chart.temps.air', 'dd.chart.temps.setpoints', 'dd.chart.temps.climate_state'] });
  const cTyre = tile({ w: 2, title: 'Tyres', icon: 'tyre', cls: 'chart-tile', cover: 'dd.chart.tire_pressure' });
  const bandsT = tile({ w: 2, title: 'Time at each speed', icon: 'bars', cls: 'chart-tile', cover: 'dd.chart.speed_histogram' });
  const total = (d.speed_bands || []).reduce((a, b) => a + (b.seconds || 0), 0);
  bandsT.append((d.speed_bands || []).length
    ? bars(d.speed_bands.map((b) => ({ label: formatNumber(Math.round(convert(b.from_kph, 'speed'))), value: b.seconds, text: total ? `${Math.round((b.seconds / total) * 100)}%` : '', title: `${format(b.from_kph, 'speed')} to ${format(b.to_kph, 'speed')}: ${formatDuration(b.seconds * 1000)}` })), { label: 'Time per speed band' })
    : note('No speed was recorded.'), note(`Speed in ${unitLabel('speed')}, from the lower end of each band.`));

  /* ------------------------------------------------------------ export */
  const stem = `${(car.name || 'car').toLowerCase().replace(/[^a-z0-9]+/g, '-')}-drive-${dayKey(d.start_ms, tz)}-${formatTime(d.start_ms, tz).replace(/[^0-9]/g, '')}`;
  const exp = tile({ w: 2, title: 'Export the route', icon: 'download', cover: ['dd.export.gpx', 'web.drive-gpx.entry', 'web.drive-gpx.file', 'web.drive-gpx.point-coordinates', 'web.drive-gpx.point-elevation', 'web.drive-gpx.point-time', 'web.drive-gpx.not-found'] },
    note('A GPX file has every point of the route with its position, elevation and time, for maps and other apps.'),
    h('div', { class: 'form-actions' },
      h('a', { class: 'btn btn-primary', href: ctx.api.url(`${base}/track`, { format: 'gpx', extensions: 'speed,power' }), download: `${stem}.gpx` }, icon('download'), h('span', { class: 'btn-text' }, 'GPX file')),
      h('a', { class: 'btn btn-soft', href: ctx.api.url(`${base}/track`, { format: 'geojson' }), download: `${stem}.geojson` }, icon('download'), h('span', { class: 'btn-text' }, 'GeoJSON'))));

  ctx.view.replaceChildren(ctxBar, mapT, ...figs, cSpeed, cSoc, selected, odo, cTemp, cTyre, bandsT, exp);

  /* map */
  let marker = null;
  let m = null;
  if (coords.length > 1) {
    m = await mapTile(ctx, mapBox, { bounds: trackBounds(track.error ? [] : track.features || []) || boundsOf(coords), padding: 48 });
    if (!m) return {};
    if (m.map) {
      m.route('route', coords.map((c) => [c[0], c[1]]), { tone: 'c1' });
      m.pin(coords[0].slice(0, 2), { tone: 'teal', title: `Start: ${endLabel(d.start)}` });
      m.pin(coords[coords.length - 1].slice(0, 2), { tone: going ? 'coral' : 'sky', title: going ? 'Now' : `End: ${endLabel(d.end)}` });
    }
  } else mapBox.append(h('p', { class: 'map-note' }, track.error ? 'The route did not load.' : 'No positions were recorded for this drive.'));

  /* charts */
  const charts = [];
  if (!ser || !ser.t_ms?.length) {
    for (const t of [cSpeed, cSoc, cTemp, cTyre]) t.append(note(s.error ? 'The values along the drive did not load.' : 'No values were recorded along this drive.'));
  } else {
    const V = ser.values;
    const lat = nums(V.lat), lon = nums(V.lon);
    const onCursor = (i) => {
      if (!m || !m.map) return;
      if (i == null || lat[i] == null) { marker?.remove(); marker = null; return; }
      if (!marker) marker = m.pin([lon[i], lat[i]], { tone: 'sun', title: 'Here' }); else marker.setLngLat([lon[i], lat[i]]);
    };
    const onZoom = (a, b) => drawSelected(a, b);
    const common = { tz, t_ms: ser.t_ms, sync, onCursor, onZoom };
    charts.push(timeChart(ctx, cSpeed, { ...common, label: 'Speed and power', series: [
      { key: 'speed', name: 'Speed', kind: 'speed', values: V.speed_kph, tone: 'c1', fill: true, floor: 0 },
      { key: 'power', name: 'Power', kind: 'power', values: V.power_kw, tone: 'c2', dashed: true, signed: true },
    ] }));
    if (ser.sources?.length) cSpeed.append(h('p', { class: 'src-line' }, srcPill(ser)));
    const rv = rangeLabel() === 'Ideal range' && has(V.ideal_range_km) ? V.ideal_range_km : V.range_km;
    charts.push(timeChart(ctx, cSoc, { ...common, label: 'Battery and elevation', series: [
      { key: 'soc', name: 'Battery', kind: 'percent', values: V.soc_pct, tone: 'c1', fill: true },
      { key: 'elev', name: 'Elevation', kind: 'length', values: V.elevation_m, tone: 'c2', dashed: true },
    ] }));
    cSoc.append(note(`${rangeLabel()} went from ${format(rv.find((x) => x != null), 'range')} to ${format([...rv].reverse().find((x) => x != null), 'range')}.`,
      has(V.battery_heat_kw) ? ` The battery heater ran up to ${format(Math.max(...nums(V.battery_heat_kw).filter((x) => x != null)), 'power')}.` : ' The battery heater did not run.'));
    const sp = (V.setpoint_left_c || []).map((x) => (typeof x === 'number' ? x : null));
    charts.push(timeChart(ctx, cTemp, { ...common, label: 'Climate', series: [
      { key: 'out', name: 'Outside', kind: 'temperature', values: V.outside_c, tone: 'c2', dashed: true, axis: 'left' },
      { key: 'in', name: 'Inside', kind: 'temperature', values: V.cabin_c, tone: 'c1', axis: 'left' },
      ...(has(sp) ? [{ key: 'set', name: 'Set to', kind: 'temperature', values: sp, tone: 'c3', axis: 'left' }] : []),
    ] }));
    const modes = [...new Set((V.climate || []).filter(Boolean))];
    cTemp.append(note(modes.length ? `Climate: ${modes.map((x) => String(x).replace(/_/g, ' ')).join(', ')}.` : 'The climate state was not sent.', has(sp) ? '' : ' Set points were not sent by this car.'));
    if (['tyre_fl_kpa', 'tyre_fr_kpa', 'tyre_rl_kpa', 'tyre_rr_kpa'].some((k) => has(V[k]))) {
      charts.push(timeChart(ctx, cTyre, { ...common, label: 'Tyres', series: [
        { key: 'fl', name: 'Front left', kind: 'pressure', digits: 2, values: V.tyre_fl_kpa, tone: 'c1', axis: 'left' },
        { key: 'fr', name: 'Front right', kind: 'pressure', digits: 2, values: V.tyre_fr_kpa, tone: 'c2', dashed: true, axis: 'left' },
        { key: 'rl', name: 'Rear left', kind: 'pressure', digits: 2, values: V.tyre_rl_kpa, tone: 'c3', axis: 'left' },
      ] }));
    } else cTyre.append(note('This car sends no tyre pressures.'));

    function drawSelected(a, b) {
      const t = ser.t_ms;
      const idx = t.map((x, i) => (x >= a && x <= b ? i : -1)).filter((i) => i >= 0);
      const full = a <= t[0] + 1000 && b >= t[t.length - 1] - 1000;
      if (full || idx.length < 2) {
        selected.replaceChildren(selected.firstChild, h('p', { class: 'note' }, 'Drag across a chart to pick a part of the drive; double-click to see it all again.'));
        return;
      }
      const sp2 = idx.map((i) => V.speed_kph[i]).filter((x) => x != null);
      const so = idx.map((i) => V.soc_pct[i]).filter((x) => x != null);
      let km = 0;
      for (let k = 1; k < idx.length; k++) { const i = idx[k], j = idx[k - 1]; if (V.speed_kph[i] != null && V.speed_kph[j] != null) km += ((V.speed_kph[i] + V.speed_kph[j]) / 2) * ((t[i] - t[j]) / 3600000); }
      selected.replaceChildren(selected.firstChild,
        h('div', { class: 'sel-nums' },
          h('div', null, h('span', { class: 'hero-k' }, 'Time'), num(t[idx[idx.length - 1]] - t[idx[0]], 'duration', { size: 's' })),
          h('div', null, h('span', { class: 'hero-k' }, 'About'), num(km, 'distance', { size: 's' })),
          h('div', null, h('span', { class: 'hero-k' }, 'Top speed'), num(sp2.length ? Math.max(...sp2) : null, 'speed', { size: 's' })),
          h('div', null, h('span', { class: 'hero-k' }, 'Battery used'), num(so.length ? so[0] - so[so.length - 1] : null, 'percent', { size: 's' }))),
        h('p', { class: 'note' }, `${formatTime(t[idx[0]], tz, { seconds: true })} to ${formatTime(t[idx[idx.length - 1]], tz, { seconds: true })}. Double-click a chart to see it all.`));
    }
  }
  await Promise.all([...charts, m ? m.ready : null]);
  if (!ctx.alive()) return {};
  ctx.ready();
  return {};
}
