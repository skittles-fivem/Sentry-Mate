// Sentry Mate, "Logbook" UI pack: Drive detail (/cars/{car}/drives/{drive}).
//
// One drive in full: the route, the charts along it, its figures and the export. Data:
// GET /api/v1/cars/{car}/drives/{drive} (the drive, previous and next, speed bands, gaps),
// /drives/{drive}/track?format=geojson (the route at 1 Hz, with speed, power and GPS accuracy per point),
// /series for the drive's span (speed, power, battery, range, battery heating, temperatures, set points,
// climate mode and fan, tyres, elevation, position for the hover marker) and /charges for the charge
// before and after. Zooming a chart narrows the figures under "Selected part" and highlights that part of
// the route; the climate strip follows the zoom.

import { h, replace } from '../shared/dom.js';
import { format, convert, unitLabel, rangeField, rangeValue, rangeLabel } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, dayKey } from '../shared/time.js';
import { formatList } from '../shared/i18n.js';
import { column, timeColumn, csvFilename, downloadCsv } from '../shared/csv.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { trackBounds } from '../shared/tracks.js';
import { pageHead, q, figure, icon, sep, empty, errorWords, flag, button } from '../core/ui.js';
import { makeMap, boundsOf } from '../core/map.js';
import { stackedCharts } from '../core/chart.js';
import { placeLink, endLabel } from '../core/journal.js';
import { sourceMark, sourceLabel, isImported, isEstimated, estimatedWhy, ESTIMATED_WHY } from '../core/sources.js';
import { choice } from '../core/form.js';
import { remount } from '../core/router.js';

export const id = 'drive';
export const css = true;
export const title = () => 'Drive';

const FIELDS = ['speed_kph', 'power_kw', 'soc_pct', 'usable_soc_pct', 'range_km', 'ideal_range_km', 'energy_remaining_kwh', 'battery_heat_kw',
  'elevation_m', 'outside_c', 'cabin_c', 'setpoint_left_c', 'setpoint_right_c', 'climate', 'fan_level',
  'tyre_fl_kpa', 'tyre_fr_kpa', 'tyre_rl_kpa', 'tyre_rr_kpa', 'lat', 'lon', 'accuracy_m', 'odometer_km'].join(',');

// The climate system's modes, in words.
const CLIMATE = Object.freeze({
  off: 'Off', on: 'On', precondition: 'Preconditioning', overheat_protect: 'Overheat protection',
});
// Set points: the car sends LO and HI as words, not temperatures.
const SETPOINT_WORDS = Object.freeze({ LO: 'LO (coldest: full cooling)', HI: 'HI (warmest: full heating)' });
const setText = (v) => (typeof v === 'string' ? SETPOINT_WORDS[v.toUpperCase()] || v : format(v, 'temperature'));
const numOnly = (list) => (list || []).map((v) => (typeof v === 'number' && Number.isFinite(v) ? v : null));
const has = (list) => (list || []).some((v) => v != null);
const TYRES = [['tyre_fl_kpa', 'Front left', 'FL'], ['tyre_fr_kpa', 'Front right', 'FR'], ['tyre_rl_kpa', 'Rear left', 'RL'], ['tyre_rr_kpa', 'Rear right', 'RR']];
const DAY_MS = 86400000;
// The speed bands' width: 10 of the person's unit (16.09344 km/h is 10 mph).
const BAND_KPH = { 'km/h': 10, mph: 16.09344 };

// Route colours: speed classes (in the person's unit) and power classes. Each class is a token and an opacity.
const SPEED_STEPS = { 'km/h': [30, 60, 90, 120], mph: [20, 40, 60, 80] };
const POWER_STEPS = [0, 25, 60];

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz;
  const base = api.apiPath`cars/${car.id}/drives/${ctx.driveId}`;
  let d;
  const bandKph = BAND_KPH[unitLabel('speed')] || 10;
  try { d = await ctx.api.get(base, { speed_band_kph: bandKph }); } catch (err) {
    if (err && err.status === 404) {
      // An unknown drive, or one of a car this account may not see: the same answer, with the way back.
      ctx.main.append(pageHead({ eyebrow: h('nav', { class: 'crumbs', 'aria-label': 'Breadcrumb' }, h('a', { href: state.buildUrl('car', { car: car.id }) }, car.name)), title: 'No such drive' }),
        empty({
          title: 'There is no drive with this number for this car.',
          text: 'It may have been rebuilt from the logs under a new number, or the link is wrong. Its route export answers “not found” too.',
          action: h('a', { class: 'link-arrow', href: state.buildUrl('drives', { car: car.id, period: ctx.state.period }) }, 'Find it on Drives', icon('arrowRight')),
          cover: ['dd.empty_states', 'web.drive-gpx.not-found'],
        }));
      ctx.ready();
      return {};
    }
    throw err;
  }
  const going = d.end_ms == null;
  const end = d.end_ms ?? api.now();
  // A failed request is kept as { error }, never turned into "nothing recorded".
  const kept = (p) => p.catch((err) => ({ error: err }));
  const loadNear = () => Promise.all([
    kept(ctx.api.get(api.apiPath`cars/${car.id}/charges`, { to_ms: d.start_ms, limit: 1 })),
    going ? null : kept(ctx.api.get(api.apiPath`cars/${car.id}/charges`, { from_ms: d.end_ms, sort: 'start', dir: 'asc', limit: 1 })),
  ]);
  // The estimate's consumption: the server's (est_wh_per_km), else the 30 days of drives before this one.
  const recent = d.est_wh_per_km != null ? null
    : kept(ctx.api.get(api.apiPath`cars/${car.id}/drives`, { from_ms: d.start_ms - 30 * DAY_MS, to_ms: d.start_ms, limit: 1 }));
  const [track, series, near, recentDrives] = await Promise.all([
    kept(ctx.api.get(`${base}/track`, { format: 'geojson', extensions: 'speed,power,accuracy' })),
    kept(ctx.api.get(api.apiPath`cars/${car.id}/series`, { fields: FIELDS, from_ms: d.start_ms, to_ms: end + 1, max_points: 3000 })),
    loadNear(), recent,
  ]);
  if (!ctx.alive()) return {};
  const s = series && !series.error ? series : null;
  const trackErr = track && track.error ? track.error : null;
  const estWhKm = d.est_wh_per_km ?? (recentDrives && !recentDrives.error && recentDrives.summary ? recentDrives.summary.wh_per_km : null) ?? null;
  ctx.title(`${endLabel(d.start)} to ${endLabel(d.end)}`);

  /* ------------------------------------------------------------ head: breadcrumb, steps, places */
  const link = (did) => (did ? state.buildUrl('drive', { car: car.id, drive: did, period: ctx.state.period }) : null);
  const step = (href, text, dir) => (href
    ? h('a', { class: 'button button-quiet', href, rel: dir < 0 ? 'prev' : 'next' }, dir < 0 ? icon('chevronLeft') : null, text, dir > 0 ? icon('chevronRight') : null)
    : h('span', { class: 'button button-quiet is-disabled', 'aria-disabled': 'true', title: dir < 0 ? 'This is the first drive recorded' : 'This is the newest drive' }, dir < 0 ? icon('chevronLeft') : null, text, dir > 0 ? icon('chevronRight') : null));
  // the end of a drive still going is where the car is now, not a place to name
  const placeA = (p, still = false) => (!still && placeLink(p) ? h('a', { href: placeLink(p), title: p.place ? 'Edit this place' : 'Name this place' }, p.label) : p.label);
  const asTrip = state.buildUrl('trip', { car: car.id, period: { from: d.start_ms, to: end } });
  const head = pageHead({
    eyebrow: h('nav', { class: 'crumbs', 'aria-label': 'Breadcrumb', cover: ['dd.context.drive', 'dd.filter.car'] },
      h('a', { href: state.buildUrl('car', { car: car.id }) }, car.name), h('span', { class: 'crumb-sep', 'aria-hidden': 'true' }, '/'),
      h('a', { href: state.buildUrl('drives', { car: car.id, period: ctx.state.period }) }, 'Drives'), h('span', { class: 'crumb-sep', 'aria-hidden': 'true' }, '/'),
      h('span', { 'aria-current': 'page' }, formatDate(d.start_ms, tz, { weekday: 'long', month: 'long', year: true }))),
    title: [endLabel(d.start), h('span', { class: 'to' }, ' to '), endLabel(d.end)],
    deck: [`${formatTime(d.start_ms, tz)} – ${going ? 'now' : formatTime(d.end_ms, tz)}`, sep(), formatDuration(d.duration_ms), sep(),
      placeA(d.start), ' to ', placeA(d.end, going),
      isImported(d) ? [' ', flag('Imported', { kind: 'estimated', title: `${sourceLabel(d)}: totals only, energy estimated from range` })] : null,
      !isImported(d) && isEstimated(d) ? [' ', flag('Estimated', { kind: 'estimated', title: ESTIMATED_WHY })] : null,
      going ? [' ', flag('In progress', { title: 'Figures up to now' })] : null,
      d.flags && d.flags.reduced_range ? [' ', flag('Reduced range', { kind: 'warn', title: 'The car limited its range during this drive' })] : null,
      sourceMark(d, { block: true, cls: 'drive-head-src' })],
    aside: h('nav', { class: 'step-nav', 'aria-label': 'Other drives' },
      step(link(d.prev), 'Earlier', -1), step(link(d.next), 'Later', 1),
      h('a', { class: 'button button-quiet', href: asTrip, title: 'This drive’s span on the Trip page' }, 'As a trip')),
    cls: 'drive-head',
  });

  // A drive still going: say so when it ends, and offer the finished figures (pages never poll).
  // The note holds its place from the start (a slot, not a block that pushes the page down when news arrives).
  const liveText = h('span', null, 'This drive is still going. ');
  const liveBtn = h('button', { type: 'button', class: 'button button-quiet button-small', disabled: true, title: 'Nothing new yet', onclick: () => remount() }, icon('reset'), 'Show new data');
  const liveNote = h('p', { class: 'drive-live', hidden: !going, role: 'status' }, liveText, liveBtn);
  if (going) {
    ctx.events(['rows'], (ev) => {
      const e = ev.data || {};
      if (e.car !== car.id || !(e.kinds || []).includes('drives')) return;
      liveText.textContent = 'This drive has new data. ';
      liveBtn.disabled = false;
      liveBtn.removeAttribute('title');
    });
  }

  /* ------------------------------------------------------------ figures: each blank one says why */
  const why = (cond, text) => (cond ? text : null);
  // Range at the start and end in the person's choice. An answer without the ideal twins: the first and
  // last ideal readings of the drive, else the rated figures named as rated (never rated called ideal).
  const wantIdeal = rangeField('range_km') !== 'range_km';
  let startRange = rangeValue(d, 'start_range_km'), endRange = rangeValue(d, 'end_range_km');
  let rangeName = rangeLabel(), rangeHow = null;
  if (wantIdeal && !('start_ideal_range_km' in d)) {
    const iv = s && s.values.ideal_range_km ? s.values.ideal_range_km.filter((v) => v != null) : [];
    if (iv.length) { startRange = iv[0]; endRange = iv[iv.length - 1]; rangeHow = 'first and last ideal readings'; }
    else { rangeName = 'Rated range'; startRange = d.start_range_km; endRange = d.end_range_km; rangeHow = 'rated: no ideal range was sent for this drive'; }
  }
  const stats = h('dl', { class: 'drive-stats', cover: ['dd.stat.odometer', 'dd.stat.drive_duration', 'dd.stat.distance', 'dd.stat.elevation_up', 'dd.stat.elevation_down', 'dd.stat.energy_net', 'dd.stat.energy_recovered', 'dd.stat.consumption_net', 'dd.stat.speed_avg', 'dd.empty_states'] },
    figure('Distance', q(d.distance_km, 'distance', { why: why(d.distance_km == null, 'no odometer readings') }),
      d.start_odometer_km != null ? `odometer ${format(d.start_odometer_km, 'odometer')} → ${going ? 'now ' : ''}${format(d.end_odometer_km, 'odometer', { why: 'not read at the end' })}` : 'odometer not read'),
    figure('Duration', formatDuration(d.duration_ms), going ? `so far, up to ${formatTime(end, tz)}` : 'start to stop'),
    figure('Average speed', q(d.speed_avg_kph, 'speed', { why: why(d.speed_avg_kph == null, 'no distance recorded') }), d.speed_max_kph != null ? `distance ÷ time; top ${format(d.speed_max_kph, 'speed')}` : 'distance ÷ time'),
    figure('Climb and descent', h('span', null, h('span', { class: 'updown' }, h('span', { 'aria-hidden': 'true' }, '↑ '), h('span', { class: 'sr-only' }, 'climb '), q(d.climb_m, 'length', { why: why(d.climb_m == null, 'no elevation recorded') })), ' ',
      h('span', { class: 'updown' }, h('span', { 'aria-hidden': 'true' }, '↓ '), h('span', { class: 'sr-only' }, 'descent '), q(d.descent_m, 'length', { why: why(d.descent_m == null, 'no elevation recorded') }))), 'the car’s own elevation'),
    figure('Energy used', q(d.energy_kwh, 'energy', { why: why(d.energy_kwh == null, 'no pack readings for this drive') }), isEstimated(d) ? `estimated: ${estimatedWhy(d)}` : 'net: measured at the pack, regen taken off'),
    figure('Regenerated', q(d.regen_kwh, 'energy', { why: why(d.regen_kwh == null, isImported(d) ? 'not in imported history' : 'not recorded') }), d.regen_kwh != null ? 'measured flowing back into the pack' : null),
    figure('Consumption', q(d.wh_per_km, 'efficiency', { why: why(d.wh_per_km == null, d.distance_km != null && d.distance_km < 1 ? 'too short to be reliable' : 'no energy figure') }), d.rated_wh_per_km && d.efficiency_pct != null ? `net; ${format(d.efficiency_pct, 'percent')} of the rated ${format(d.rated_wh_per_km, 'efficiency')}` : 'net energy ÷ distance'),
    figure('Climate', q(d.hvac_kwh, 'energy', { why: why(d.hvac_kwh == null, 'not sent by this car') }), 'heating and cooling, part of the energy used'),
    figure('Battery', d.start_soc_pct != null || d.end_soc_pct != null ? `${format(d.start_soc_pct, 'percent')} → ${format(d.end_soc_pct, 'percent')}` : q(null, 'percent', { why: 'not recorded' }),
      d.start_soc_pct != null && d.end_soc_pct != null ? `${format(d.start_soc_pct - d.end_soc_pct, 'number', { digits: 1 })} points used` : null),
    figure(rangeName, startRange != null || endRange != null ? `${format(startRange, 'range')} → ${format(endRange, 'range')}` : q(null, 'range', { why: 'not recorded' }),
      [startRange != null && endRange != null ? `${format(startRange - endRange, 'range')} of range used` : null, rangeHow].filter(Boolean).join('; ') || null),
    figure('Peak power', q(d.power_max_kw, 'power', { why: why(d.power_max_kw == null, 'no power readings') }),
      d.power_min_kw != null ? ['strongest regen ', q(Math.abs(d.power_min_kw), 'power'), ' back into the pack'] : isImported(d) ? 'regen not in imported history' : 'no regen recorded'),
    figure('Outside', q(d.outside_avg_c, 'temperature', { why: why(d.outside_avg_c == null, 'not recorded') }), d.cabin_avg_c != null ? `average; cabin ${format(d.cabin_avg_c, 'temperature')}` : 'average'));

  /* ------------------------------------------------------------ time in each speed band, with CSV */
  // Bands of 10 of the person's unit: the server's when it made them that wide (speed_band_kph), else
  // worked out here from the speed readings, each counting for its bucket's time.
  const bandSet = speedBands(d, s, bandKph);
  const bands = bandSet.list, speedUnit = bandSet.unit;
  const bandTotal = bands.reduce((a, b) => a + b.seconds, 0);
  const num0 = (v) => format(v, 'number', { digits: 0 });
  const bandName = (b) => (b.to == null ? `${num0(b.from)} ${speedUnit} and over` : `${num0(b.from)} to ${num0(b.to)} ${speedUnit}`);
  const bandsCsv = h('button', { type: 'button', class: 'button button-quiet button-small', onclick: () => downloadCsv(csvFilename(car.name, 'drive', dayKey(d.start_ms, tz), 'speed bands'), bands, [
    { label: `From (${speedUnit})`, value: (b) => b.from }, { label: `To (${speedUnit})`, value: (b) => b.to },
    { label: 'Seconds', value: (b) => Math.round(b.seconds * 10) / 10 }, { label: 'Share (%)', value: (b) => Math.round((b.seconds / bandTotal) * 1000) / 10 },
  ]) }, icon('download'), 'CSV');
  const bandsEl = h('section', { class: 'drive-bands', 'aria-labelledby': 'bands-title', cover: 'dd.chart.speed_histogram' },
    h('div', { class: 'side-head' }, h('h2', { class: 'label', id: 'bands-title' }, 'Time in each speed band'), bands.length ? bandsCsv : null),
    bands.length
      ? h('ul', { class: 'band-list' }, bands.map((b) => h('li', { class: 'band-row' },
        h('span', { class: 'band-name' }, bandName(b)),
        h('span', { class: 'band-bar', 'aria-hidden': 'true' }, h('span', { class: 'band-fill', vars: { '--w': `${((b.seconds / bandTotal) * 100).toFixed(2)}%` } })),
        h('span', { class: 'band-value' }, formatDuration(b.seconds * 1000), h('span', { class: 'band-share' }, format((b.seconds / bandTotal) * 100, 'percent', { digits: 0 }))))))
      : h('p', { class: 'note' }, isImported(d) ? 'Imported drives keep no speed readings, so there are no bands.' : 'No speed readings were recorded for this drive.'),
    bands.length ? h('p', { class: 'note' }, `Every reading counts, standstill included; bands are ${num0(bandSet.width)} ${speedUnit} wide${bandSet.local ? ', counted from the speed readings' : ''}.`) : null);

  /* ------------------------------------------------------------ charges before and after */
  const chargeItem = (c, word) => {
    if (!c) return h('li', { class: 'near-row is-none' }, h('span', { class: 'near-word' }, word), h('span', { class: 'near-text' }, word === 'After' && going ? 'the drive is still going' : 'no charge recorded'));
    const where = c.place && c.place.name ? c.place.name : c.address && c.address.short ? c.address.short : 'unnamed spot';
    return h('li', { class: 'near-row' }, h('span', { class: 'near-word' }, word),
      h('a', { class: 'near-text', href: state.buildUrl('charge', { car: car.id, charge: c.id, period: ctx.state.period }) },
        `${where}, ${(c.kind || '').toUpperCase() || 'kind not known'}, `, q(c.energy_added_kwh, 'energy'),
        h('span', { class: 'near-when' }, `${formatDate(c.start_ms, tz, { now: api.now() })}, ${formatTime(c.start_ms, tz)}`)));
  };
  const nearList = h('ul', { class: 'near-list' });
  const drawNear = ([before, after]) => {
    const failed = [before, after].find((x) => x && x.error);
    if (failed) {
      replace(nearList, h('li', { class: 'near-row is-error' }, partError(failed.error, 'Could not load the charges either side.', async (ev) => {
        ev.currentTarget.disabled = true;
        const again = await loadNear();
        if (ctx.alive()) drawNear(again);
      })));
      return;
    }
    replace(nearList, chargeItem(before && before.items && before.items[0], 'Before'), chargeItem(after && after.items && after.items[0], 'After'));
  };
  drawNear(near);
  const nearEl = h('section', { class: 'drive-near', 'aria-labelledby': 'near-title' },
    h('h2', { class: 'label', id: 'near-title' }, 'Charges either side'), nearList);

  /* ------------------------------------------------------------ export: GPX or GeoJSON, with extras */
  const extras = [['speed', 'Speed'], ['power', 'Power'], ['soc', 'Battery %']];
  const checks = extras.map(([k, t]) => h('label', { class: 'check' }, h('input', { type: 'checkbox', value: k }), t));
  // Named by the car and the drive's start in the car's zone: comet-drive-2026-11-15-1217.gpx
  const fileStem = csvFilename(car.name, 'drive', dayKey(d.start_ms, tz), hhmm(d.start_ms, tz)).replace(/\.csv$/, '');
  const gpx = h('a', { class: 'button button-small', download: `${fileStem}.gpx` }, icon('download'), 'GPX');
  const geo = h('a', { class: 'button button-quiet button-small', download: `${fileStem}.geojson` }, icon('download'), 'GeoJSON');
  const setLinks = () => {
    const ext = ['accuracy', ...checks.map((c) => c.querySelector('input')).filter((i) => i.checked).map((i) => i.value)].join(',');
    gpx.setAttribute('href', ctx.api.url(`${base}/track`, { format: 'gpx', extensions: ext }));
    geo.setAttribute('href', ctx.api.url(`${base}/track`, { format: 'geojson', extensions: ext }));
  };
  checks.forEach((c) => c.addEventListener('change', setLinks));
  setLinks();
  const exportEl = h('section', { class: 'drive-export', 'aria-labelledby': 'export-title', cover: ['dd.export.gpx', 'web.drive-gpx.entry', 'web.drive-gpx.file', 'web.drive-gpx.point-coordinates', 'web.drive-gpx.point-elevation', 'web.drive-gpx.point-time', 'web.drive-gpx.not-found'] },
    h('h2', { class: 'label', id: 'export-title' }, 'Export the route'),
    h('p', { class: 'note' }, `Every point once a second with its time (UTC), position, elevation and GPS accuracy, one track segment per stretch without dropped data. Tick extras to add them. The file is named ${fileStem}, after the start in ${car.name}’s time zone.`),
    h('div', { class: 'checks' }, checks), h('div', { class: 'export-buttons' }, gpx, geo));

  /* ------------------------------------------------------------ the route map, coloured by speed or power */
  const lines = track && track.features ? track.features.filter((f) => f.geometry && f.geometry.coordinates.length) : [];
  const all = lines.flatMap((f) => f.geometry.coordinates.map((c) => [c[0], c[1]]));
  const acc = lines.flatMap((f) => (f.properties && f.properties.accuracy_m) || []).filter((v) => v != null).sort((a, b) => a - b);
  const mapBox = h('div', { class: 'map map-drive', role: 'region', 'aria-label': `Map of the route from ${endLabel(d.start)} to ${endLabel(d.end)}` });
  let colourBy = (ctx.query && ctx.query.colour) || 'plain';
  if (!['plain', 'speed', 'power'].includes(colourBy)) colourBy = 'plain';
  const legend = h('div', { class: 'route-legend', 'aria-live': 'polite' });
  const canColour = lines.some((f) => f.properties && (f.properties.speed_kph || f.properties.power_kw));
  const colourChoice = canColour ? choice({
    label: 'Colour the route by', name: 'route-colour', value: colourBy,
    options: [['plain', 'One colour'], ['speed', 'Speed'], ['power', 'Power']],
    onchange: (v) => { colourBy = v; ctx.setQuery({ colour: v === 'plain' ? null : v }); paintRoute(); },
  }) : null;
  const mapNote = h('p', { class: 'note map-foot' },
    all.length > 1 ? `${format(all.length, 'count')} points, one a second. ` : null,
    acc.length ? ['GPS accuracy: typically within ', q(acc[Math.floor(acc.length / 2)], 'length'), ', worst ', q(acc[acc.length - 1], 'length'), '; the readout gives it for each moment.'] : 'No GPS accuracy was recorded.');
  const mapCol = h('div', { class: 'drive-map', cover: 'dd.map.route' }, mapBox, all.length > 1 ? h('div', { class: 'map-bar' }, colourChoice, legend) : null, all.length > 1 ? mapNote : null);

  /* ------------------------------------------------------------ along the drive: readout, selection, charts */
  const readout = h('div', { class: 'drive-readout', 'aria-live': 'off', cover: 'dd.chart.drive.tooltip' });
  const selected = h('div', { class: 'selected', cover: ['dd.stat.selected_duration', 'dd.control.zoom'] });
  const resetBtn = h('button', { type: 'button', class: 'button button-quiet button-small', disabled: true }, icon('reset'), 'Show the whole drive');
  const csvBtn = h('button', { type: 'button', class: 'button button-quiet button-small', disabled: !s || s.t_ms.length < 1 }, icon('download'), 'Download CSV');
  const charts = h('div', { class: 'charts', cover: ['dd.chart.drive', 'dd.chart.drive.speed', 'dd.chart.drive.power', 'dd.chart.drive.range', 'dd.chart.drive.soc', 'dd.chart.drive.battery_heater', 'dd.chart.elevation', 'dd.chart.temps.air', 'dd.chart.temps.setpoints', 'dd.chart.temps.climate_state', 'dd.chart.tire_pressure'] });
  const notes = h('ul', { class: 'along-notes' });
  ctx.main.append(head, liveNote, h('div', { class: 'drive-body', cover: 'shots.drive-details' },
    h('div', { class: 'drive-top' },
      h('div', { class: 'drive-main' }, mapCol, h('div', { class: 'drive-under' }, bandsEl, h('div', { class: 'drive-under-side' }, nearEl, exportEl))),
      h('section', { class: 'drive-side', 'aria-label': 'Drive figures' }, stats)),
    h('section', { class: 'along', 'aria-labelledby': 'along-title' },
      h('div', { class: 'along-head' }, h('h2', { class: 'label', id: 'along-title' }, 'Along the drive', s && s.t_ms && s.t_ms.length ? h('span', { class: 'label-note' }, sourceLabel(s)) : null), h('div', { class: 'along-actions' }, resetBtn, csvBtn)),
      readout, selected, charts, notes,
      h('p', { class: 'along-hint' }, 'Point at a chart to read every value at that moment and see where the car was on the map. Drag across a chart to zoom in; the figures under “Selected part” and the route highlight follow. Double-click a chart to zoom out.'))));

  // The map: the route (one line per stretch without dropped data), start and end pins, the hover marker.
  let marker = null, markerEl = null, m = null;
  let paintRoute = () => {};
  if (all.length > 1) {
    m = await makeMap(ctx, mapBox, { bounds: trackBounds(lines) || boundsOf(all), padding: 56, maxZoom: 17, fullscreen: true });
    if (m) {
      await m.loaded;
      if (!ctx.alive()) return {};
      await Promise.all(lines.map((f, i) => m.addRoute(`route-${i}`, f.geometry.coordinates.map((c) => [c[0], c[1]]))));
      if (!ctx.alive()) return {};
      paintRoute = colourLayers(m, lines, legend, () => colourBy);
      m.map.addSource('sel', { type: 'geojson', data: { type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: [] } } });
      m.addLine({ id: 'sel-casing', source: 'sel' }, 10, '--paper');
      m.addLine({ id: 'sel-line', source: 'sel' }, 6, '--accent-strong');
      m.addPin(all[0], endLabel(d.start), 'pin-start');
      if (!going) m.addPin(all[all.length - 1], endLabel(d.end), 'pin-end');
      else m.addPin(all[all.length - 1], 'Now', 'pin-here');
      markerEl = h('div', { class: 'car-dot is-hidden' });
      marker = new m.ml.Marker({ element: markerEl, anchor: 'center' }).setLngLat(all[0]).addTo(m.map);
      paintRoute();
    }
  } else if (trackErr) {
    replace(mapBox, partError(trackErr, 'Could not load the route.', () => remount()));
    mapBox.classList.add('is-empty');
  } else {
    replace(mapBox, h('p', { class: 'map-note' }, isImported(d) ? 'Imported drives keep no route.' : 'No route was recorded for this drive: the car sent no positions while it drove.'));
    mapBox.classList.add('is-empty');
  }

  /* ------------------------------------------------------------ charts on one time axis, one readout */
  const V = s ? s.values : {};
  const val = (f, i) => (i != null && V[f] ? V[f][i] : null);
  const rf = rangeField('range_km');
  // The range line is the person's choice when the readings have it; otherwise rated, and named so.
  const rangeVals = V[rf] && has(V[rf]) ? V[rf] : V.range_km;
  const rangeLine = V[rf] && has(V[rf]) ? rangeLabel() : 'Rated range';
  // Sentry Mate's own estimate: the energy left in the pack ÷ the consumption of the 30 days of drives before.
  const estVals = estWhKm > 0 && has(V.energy_remaining_kwh) ? V.energy_remaining_kwh.map((e) => (e == null ? null : (e * 1000) / estWhKm)) : null;
  const estNote = estVals ? ['energy left ÷ ', format(estWhKm, 'efficiency'), ' (the 30 days of drives before)'] : null;
  const climateVals = V.climate || [];
  const hasHeat = has(V.battery_heat_kw), hasClimate = has(climateVals), hasFan = has(V.fan_level);
  const hasSet = has(V.setpoint_left_c) || has(V.setpoint_right_c);
  const hasTyres = TYRES.some(([f]) => has(V[f]));
  const setWords = [...(V.setpoint_left_c || []), ...(V.setpoint_right_c || [])].some((v) => typeof v === 'string');

  // The readout: every value at the moment under the pointer, each with its unit.
  const reading = (name, value, cls = null) => h('div', { class: ['reading', cls] }, h('dt', null, name), h('dd', null, value));
  const showReadout = (i) => {
    // Without readings there is nothing to read out; the charts' place says why (failed or empty).
    if (!s) { replace(readout); readout.hidden = true; return; }
    // Idle, the same readings are shown blank, so the readout keeps its height and the charts never move.
    const idle = i == null;
    const tyre = TYRES.map(([f]) => (idle ? null : val(f, i)));
    replace(readout,
      idle
        ? h('p', { class: 'reading-time readout-idle' }, `${format(s.t_ms.length, 'count')} readings, ${formatTime(d.start_ms, tz)} to ${going ? 'now' : formatTime(d.end_ms, tz)}${s.bucket_ms > 1000 ? `, one per ${formatDuration(s.bucket_ms)} (averaged; lowest and highest kept)` : ', one a second'}. Point at a chart for every value at that moment.`)
        : h('p', { class: 'reading-time' }, h('time', null, formatTime(s.t_ms[i], tz, { seconds: true })), s.state && s.state[i] && s.state[i] !== 'driving' ? h('span', { class: 'reading-state' }, s.state[i]) : null),
      h('dl', { class: 'readings' },
        reading('Speed', q(val('speed_kph', i), 'speed')),
        reading('Power', q(val('power_kw', i), 'power'), val('power_kw', i) < 0 ? 'is-regen' : null),
        reading('Battery', q(val('soc_pct', i), 'percent', { digits: 1 })),
        reading('Usable', q(val('usable_soc_pct', i), 'percent', { digits: 1 })),
        reading(rangeLine, q(rangeVals && i != null ? rangeVals[i] : null, 'range')),
        estVals ? reading('Estimated range (from recent driving)', q(i != null ? estVals[i] : null, 'range')) : null,
        hasHeat ? reading('Battery heating', q(val('battery_heat_kw', i), 'power')) : null,
        reading('Outside', q(val('outside_c', i), 'temperature')),
        reading('Cabin', q(val('cabin_c', i), 'temperature')),
        hasSet ? reading('Set, driver', setText(val('setpoint_left_c', i))) : null,
        hasSet ? reading('Set, passenger', setText(val('setpoint_right_c', i))) : null,
        hasClimate ? reading('Climate', CLIMATE[val('climate', i)] || (val('climate', i) == null ? '—' : String(val('climate', i)))) : null,
        hasFan ? reading('Fan', val('fan_level', i) == null ? '—' : val('fan_level', i) === 0 ? 'off' : `step ${format(val('fan_level', i), 'number', { digits: 0 })} of 10`) : null,
        hasTyres ? reading('Tyres', tyre.every((v) => v == null) ? '—' : [TYRES.map(([, , ab], k) => [h('abbr', { class: 'tyre-ab', title: TYRES[k][1] }, ab), ' ', format(tyre[k], 'pressure', { unit: false })]).flatMap((x, k) => (k ? [' · ', x] : [x])), ` ${unitLabel('pressure')}`], 'is-wide') : null,
        reading('Elevation', q(val('elevation_m', i), 'length')),
        reading('GPS', val('accuracy_m', i) == null ? '—' : ['within ', q(val('accuracy_m', i), 'length')])));
  };
  showReadout(null);
  const onHover = (i) => {
    showReadout(i);
    if (!marker) return;
    const lat = i == null ? null : val('lat', i), lon = i == null ? null : val('lon', i);
    if (lat == null || lon == null) { markerEl.classList.add('is-hidden'); return; }
    markerEl.classList.remove('is-hidden');
    marker.setLngLat([lon, lat]);
  };

  // The climate strip: the mode through the drive, drawn under the temperatures in the zoomed window.
  const strip = h('div', { class: 'climate-track', role: 'img' });
  const drawStrip = (from, to) => {
    if (!s || !hasClimate) return;
    const segs = [];
    const span = to - from || 1;
    climateVals.forEach((v, i) => {
      const a = s.t_ms[i], z = s.t_ms[i + 1] ?? a + s.bucket_ms;
      if (z <= from || a >= to || v == null) return;
      const last = segs[segs.length - 1];
      if (last && last.v === v && Math.abs(last.z - a) < s.bucket_ms * 1.5) last.z = z;
      else segs.push({ v, a, z });
    });
    replace(strip, segs.map((g) => h('span', {
      class: ['climate-seg', `cl-${g.v}`], title: `${CLIMATE[g.v] || g.v}: ${formatTime(g.a, tz)} to ${formatTime(g.z, tz)}`,
      vars: { '--l': `${(Math.max(0, (g.a - from) / span) * 100).toFixed(3)}%`, '--w': `${(((Math.min(to, g.z) - Math.max(from, g.a)) / span) * 100).toFixed(3)}%` },
    })));
    const time = {};
    segs.forEach((g) => { time[g.v] = (time[g.v] || 0) + (Math.min(to, g.z) - Math.max(from, g.a)); });
    strip.setAttribute('aria-label', `Climate: ${formatList(Object.entries(time).map(([k, ms]) => `${(CLIMATE[k] || k).toLowerCase()} ${formatDuration(ms)}`))}`);
  };
  const climateKey = h('ul', { class: 'climate-key' }, Object.entries(CLIMATE).map(([k, t]) => h('li', null, h('span', { class: ['climate-swatch', `cl-${k}`], 'aria-hidden': 'true' }), t)));

  // Under each chart's name: mean, lowest and highest in the window shown (the whole drive, or the zoom).
  // A downsampled bucket's lowest and highest come from its .min and .max columns.
  const pick = (f) => (typeof f === 'string' ? { mean: V[f], min: V[`${f}.min`] || V[f], max: V[`${f}.max`] || V[f] } : { mean: f, min: f, max: f });
  const STATS = {
    speed: [['', ['speed_kph'], 'speed']],
    power: [['', ['power_kw'], 'power']],
    soc: [['charge ', ['soc_pct'], 'percent'], has(V.usable_soc_pct) ? ['usable ', ['usable_soc_pct'], 'percent'] : null],
    range: [[estVals ? `${rangeLine.toLowerCase()} ` : '', [V[rf] && has(V[rf]) ? rf : 'range_km'], 'range'], estVals ? ['estimated ', [estVals], 'range'] : null],
    heat: [['', ['battery_heat_kw'], 'power']],
    temp: [['outside ', ['outside_c'], 'temperature'], ['cabin ', ['cabin_c'], 'temperature']],
    fan: [['', ['fan_level'], 'fan']],
    tyres: [['all four ', TYRES.map(([f]) => f), 'pressure']],
    elev: [['', ['elevation_m'], 'length']],
  };
  const statText = (v, kind) => (kind === 'fan' ? `step ${format(v, 'number', { digits: 1 })}` : format(v, kind));
  const drawStats = (idx) => {
    for (const [key, lines] of Object.entries(STATS)) {
      const label = charts.querySelector(`.chart-${key} .chart-label`);
      if (!label) continue;
      let el = label.querySelector('.chart-stats');
      if (!el) { el = h('span', { class: 'chart-stats' }); label.append(el); }
      const out = lines.filter(Boolean).map(([name, fields, kind]) => {
        let sum = 0, n = 0, lo = null, hi = null;
        for (const f of fields) {
          const c = pick(f);
          for (const i of idx) {
            const m = c.mean && c.mean[i], a = c.min && c.min[i], b = c.max && c.max[i];
            if (typeof m === 'number') { sum += m; n++; }
            if (typeof a === 'number' && (lo == null || a < lo)) lo = a;
            if (typeof b === 'number' && (hi == null || b > hi)) hi = b;
          }
        }
        const piece = (t) => h('span', { class: 'stat-piece' }, t);
        return n ? h('span', { class: 'chart-stat' }, name ? h('span', { class: 'stat-name' }, name.trim()) : null,
          piece(`mean ${statText(sum / n, kind)}`), ' · ', piece(`min ${statText(lo, kind)}`), ' · ', piece(`max ${statText(hi, kind)}`)) : null;
      }).filter(Boolean);
      replace(el, ...(out.length ? out : [h('span', { class: 'chart-stat' }, 'no readings in this window')]));
    }
  };

  // Zoom: the selected part's figures, worked out from the readings in it, and its stretch on the map.
  const onZoom = (from, to) => {
    const full = !s || (from <= s.t_ms[0] + 500 && to >= s.t_ms[s.t_ms.length - 1] - 500);
    resetBtn.disabled = full;
    if (s) drawStrip(full ? s.t_ms[0] : from, full ? s.t_ms[s.t_ms.length - 1] + s.bucket_ms : to);
    const idx = s ? s.t_ms.map((t, i) => (t >= from && t <= to ? i : -1)).filter((i) => i >= 0) : [];
    if (s) drawStats(full ? s.t_ms.map((t, i) => i) : idx);
    const sel = m && m.map.getSource('sel');
    if (full || idx.length < 2) {
      replace(selected, h('p', { class: 'selected-idle' }, 'Selected part: drag across a chart to pick a stretch of the drive; its duration, distance, speed, energy and climb appear here.'));
      if (sel) sel.setData({ type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: [] } });
      return;
    }
    const f = selFigures(s, idx);
    replace(selected, h('p', { class: 'label' }, `Selected part, ${formatTime(f.from, tz, { seconds: true })} to ${formatTime(f.to, tz, { seconds: true })}`),
      h('dl', { class: 'figs selected-figs' },
        figure('Selected duration', formatDuration(f.to - f.from), `of ${formatDuration(d.duration_ms)} for the whole drive`),
        figure('Distance', q(f.dist, 'distance', { why: 'no odometer readings here' }), 'odometer at the ends'),
        figure('Average speed', q(f.avg, 'speed', { why: 'no distance here' }), f.top != null ? `distance ÷ time; top ${format(f.top, 'speed')}` : 'distance ÷ time'),
        figure('Energy', q(f.net, 'energy', { why: 'no power readings here' }), f.regen != null ? ['net; ', format(f.regen, 'energy'), ' regenerated'] : 'net, from the power readings'),
        figure('Consumption', q(f.dist > 0.2 && f.net != null ? (f.net * 1000) / f.dist : null, 'efficiency', { why: 'too short to be reliable' }), 'net energy ÷ distance'),
        figure('Climb and descent', h('span', null, '↑ ', q(f.up, 'length', { why: 'no elevation here' }), ' ↓ ', q(f.down, 'length')), 'from the elevation readings')));
    const coords = idx.map((i) => [val('lon', i), val('lat', i)]).filter((c) => c[0] != null && c[1] != null);
    if (sel) sel.setData({ type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: coords } });
  };

  if (s && s.t_ms.length > 1) {
    const rows = [
      { key: 'speed', name: 'Speed', kind: 'speed', values: V.speed_kph, colour: '--accent', fill: '--chart-wash', floor: 0, ticks: 4 },
      { key: 'power', name: 'Power', kind: 'power', note: 'use above zero, regen below', values: V.power_kw, colour: '--ink-2', signed: true, ticks: 4 },
      { key: 'soc', name: 'Battery', kind: 'percent', note: has(V.usable_soc_pct) ? 'charge solid, usable dashed (a cold pack locks part away)' : 'this car does not send usable %', values: V.soc_pct, extra: [{ values: V.usable_soc_pct, colour: '--dusk', dash: true }], colour: '--ink', ticks: 4 },
      { key: 'range', name: estVals ? 'Range' : rangeLine, kind: 'range', note: estVals ? [`${rangeLine.toLowerCase()} solid; estimated dashed: `, ...estNote] : null, values: rangeVals, extra: estVals ? [{ values: estVals, colour: '--ochre', dash: true }] : [], colour: '--ink-2', ticks: 4 },
      hasHeat ? { key: 'heat', name: 'Battery heating', kind: 'power', note: 'power the pack spends warming itself; zero when not heating', values: V.battery_heat_kw, colour: '--rust', floor: 0, ticks: 2 } : null,
      { key: 'temp', name: 'Temperatures', kind: 'temperature', note: hasSet ? `outside blue, cabin ochre; set points dashed (driver ochre, passenger red)${setWords ? '; LO and HI leave a gap and are named in the readout' : ''}` : 'outside blue, cabin ochre',
        values: V.outside_c, colour: '--slate', extra: [{ values: V.cabin_c, colour: '--ochre' }, { values: numOnly(V.setpoint_left_c), colour: '--ochre', dash: true }, { values: numOnly(V.setpoint_right_c), colour: '--rust', dash: true }], ticks: 4 },
      hasFan ? { key: 'fan', name: 'Fan', unit: 'step, 0 to 10', note: '0 is off', values: V.fan_level, colour: '--ink-3', floor: 0, ticks: 2, digits: 0 } : null,
      hasTyres ? { key: 'tyres', name: 'Tyres', kind: 'pressure', note: 'left grey, right ochre; front solid, rear dashed (FL, FR, RL, RR in the readout)', values: V.tyre_fl_kpa, extra: [{ values: V.tyre_fr_kpa, colour: '--ochre' }, { values: V.tyre_rl_kpa, colour: '--ink-2', dash: true }, { values: V.tyre_rr_kpa, colour: '--ochre', dash: true }], colour: '--ink-2', ticks: 3 } : null,
      { key: 'elev', name: 'Elevation', kind: 'length', note: 'the car’s own reading', values: V.elevation_m, colour: '--ink-3', fill: '--chart-sunk', ticks: 4 },
    ].filter(Boolean);
    const plotted = await stackedCharts(ctx, charts, { tz, t_ms: s.t_ms, states: s.state, rows, onHover, onZoom });
    if (!ctx.alive()) return {};
    if (plotted) resetBtn.addEventListener('click', () => plotted.resetZoom());
    // The climate strip sits between the temperatures and the fan.
    if (hasClimate) {
      const stripRow = h('div', { class: 'chart-row chart-climate' },
        h('div', { class: 'chart-label' }, h('span', { class: 'chart-name' }, 'Climate'), h('span', { class: 'chart-note' }, 'mode through the drive')),
        h('div', { class: 'chart-plot' }, strip, climateKey));
      const anchor = charts.querySelector('.chart-temp');
      if (anchor) anchor.after(stripRow); else charts.append(stripRow);
    }
    onZoom(s.t_ms[0], s.t_ms[s.t_ms.length - 1]);
    csvBtn.addEventListener('click', () => downloadCsv(csvFilename(car.name, 'drive', endLabel(d.start), 'to', endLabel(d.end), dayKey(d.start_ms, tz)), s.t_ms.map((t, i) => ({ t, i })), [
      timeColumn('Time', 't', tz),
      { label: 'State', value: (r) => (s.state ? s.state[r.i] : null) },
      column('Speed', (r) => val('speed_kph', r.i), 'speed', { digits: 2 }),
      column('Power', (r) => val('power_kw', r.i), 'power', { digits: 2 }),
      column('Battery', (r) => val('soc_pct', r.i), 'percent', { digits: 2 }),
      column('Usable battery', (r) => val('usable_soc_pct', r.i), 'percent', { digits: 2 }),
      column(rangeLine, (r) => (rangeVals ? rangeVals[r.i] : null), 'range', { digits: 2 }),
      column('Estimated range (from recent driving)', (r) => (estVals ? estVals[r.i] : null), 'range', { digits: 2 }),
      column('Energy left', (r) => val('energy_remaining_kwh', r.i), 'energy', { digits: 3 }),
      column('Battery heating', (r) => val('battery_heat_kw', r.i), 'power', { digits: 2 }),
      column('Outside', (r) => val('outside_c', r.i), 'temperature', { digits: 1 }),
      column('Cabin', (r) => val('cabin_c', r.i), 'temperature', { digits: 1 }),
      setColumn('Set point, driver', (r) => val('setpoint_left_c', r.i)),
      setColumn('Set point, passenger', (r) => val('setpoint_right_c', r.i)),
      { label: 'Climate', value: (r) => val('climate', r.i) },
      { label: 'Fan (step 0 to 10)', value: (r) => val('fan_level', r.i) },
      ...TYRES.map(([f, n]) => column(`Tyre ${n.toLowerCase()}`, (r) => val(f, r.i), 'pressure', { digits: 2 })),
      column('Elevation', (r) => val('elevation_m', r.i), 'length', { digits: 1 }),
      column('Odometer', (r) => val('odometer_km', r.i), 'odometer', { digits: 3 }),
      { label: 'Latitude', value: (r) => val('lat', r.i) }, { label: 'Longitude', value: (r) => val('lon', r.i) },
      column('GPS accuracy', (r) => val('accuracy_m', r.i), 'length', { digits: 1 }),
    ]));
  } else if (series && series.error) {
    replace(charts, partError(series.error, 'The readings for this drive could not be loaded, so there are no charts.', () => remount()));
    replace(selected);
    selected.hidden = true;
  } else {
    replace(charts, empty({ title: 'No readings along this drive.', text: isImported(d) ? 'Imported drives keep only their totals; the figures above are what there is.' : 'The log for this drive holds no samples. The Data health page shows whether its log arrived and passed its check.' }));
    replace(selected);
    selected.hidden = true;
  }

  // What is missing, and why, under the charts.
  const missing = [];
  if (s && !hasHeat) missing.push('This car does not send battery heating power, so there is no heating chart.');
  if (s && !hasClimate && !hasFan) missing.push('This car does not send the climate mode or fan level.');
  if (s && !hasSet) missing.push('This car does not send its climate set points, only the air temperatures.');
  if (s && !hasTyres) missing.push('No tyre pressures were sent during this drive.');
  if (d.gaps && d.gaps.length) missing.push(`No data for ${formatList(d.gaps.map((g) => `${formatTime(g.start_ms, tz)} to ${formatTime(g.end_ms, tz)}`))}; the lines break there and nothing is carried across.`);
  if (s && s.state && s.state.includes('unknown')) missing.push('Shaded red: time with no log; no value is drawn across it.');
  replace(notes, missing.map((t) => h('li', null, t)));

  if (m) await Promise.race([m.ready, new Promise((r) => setTimeout(r, 5000))]);
  ctx.ready();
  return {};
}

/** A set-point CSV column: temperatures in the person's unit, LO and HI kept as words. */
function setColumn(label, get) {
  const num = column(label, get, 'temperature', { digits: 1 });
  return { label: num.label, value: (r, i) => { const v = get(r, i); return typeof v === 'string' ? v : num.value(r, i); } };
}

/** The selected part's figures from the readings in it (indexes into the series). */
function selFigures(s, idx) {
  const V = s.values;
  const t = s.t_ms;
  const from = t[idx[0]], to = t[idx[idx.length - 1]] + (s.bucket_ms > 1000 ? s.bucket_ms : 0);
  const odo = idx.map((i) => (V.odometer_km || [])[i]).filter((x) => x != null);
  const dist = odo.length > 1 ? odo[odo.length - 1] - odo[0] : null;
  const hours = (to - from) / 3600000;
  let net = null, regen = null;
  for (const i of idx) {
    const p = (V.power_kw || [])[i];
    if (p == null) continue;
    const e = p * (s.bucket_ms / 3600000);
    net = (net || 0) + e;
    if (p < 0) regen = (regen || 0) - e;
  }
  let up = null, down = null, ref = null;
  for (const i of idx) {
    const e = (V.elevation_m || [])[i];
    if (e == null) continue;
    if (ref == null) { ref = e; up = 0; down = 0; continue; }
    if (e - ref >= 2) { up += e - ref; ref = e; } else if (ref - e >= 2) { down += ref - e; ref = e; }
  }
  const speeds = idx.map((i) => (V['speed_kph.max'] || V.speed_kph || [])[i]).filter((x) => x != null);
  return {
    from, to, dist, net, regen: regen == null ? (net == null ? null : 0) : regen, up, down,
    avg: dist != null && hours > 0 ? dist / hours : null,
    top: speeds.length ? Math.max(...speeds) : null,
  };
}

/**
 * Colour layers for the route: classes of speed or power as separate line layers over the plain route,
 * each a theme token, shown or hidden as the person picks. Returns paint(), which applies the choice.
 */
function colourLayers(m, lines, legend, mode) {
  const unit = unitLabel('speed');
  const steps = (SPEED_STEPS[unit] || SPEED_STEPS['km/h']);
  const speedClass = (kph) => { const v = convert(kph, 'speed'); let c = 0; while (c < steps.length && v >= steps[c]) c++; return c; };
  const powerClass = (kw) => (kw < POWER_STEPS[0] ? 0 : kw < POWER_STEPS[1] ? 1 : kw < POWER_STEPS[2] ? 2 : 3);
  const SPEED_LOOK = [['--accent', 0.22], ['--accent', 0.42], ['--accent', 0.62], ['--accent', 0.82], ['--accent-strong', 1]];
  const POWER_LOOK = [['--slate', 1], ['--accent', 0.45], ['--accent', 1], ['--rust', 1]];
  const runs = (key, cls) => {
    const out = [];
    lines.forEach((f) => {
      const vals = (f.properties && f.properties[key]) || [];
      const cs = f.geometry.coordinates;
      let cur = null;
      cs.forEach((c, i) => {
        const v = vals[i];
        const k = v == null ? null : cls(v);
        const p = [c[0], c[1]];
        if (cur && k === cur.k) { cur.coords.push(p); return; }
        if (cur) { cur.coords.push(p); out.push(cur); }
        cur = k == null ? null : { k, coords: [p] };
      });
      if (cur && cur.coords.length > 1) out.push(cur);
    });
    return { type: 'FeatureCollection', features: out.filter((r) => r.coords.length > 1).map((r) => ({ type: 'Feature', properties: { k: r.k }, geometry: { type: 'LineString', coordinates: r.coords } })) };
  };
  const layers = { speed: [], power: [] };
  const add = (name, data, looks) => {
    m.map.addSource(`by-${name}`, { type: 'geojson', data });
    looks.forEach(([token, opacity], k) => {
      const lid = `by-${name}-${k}`;
      m.addLine({ id: lid, source: `by-${name}`, filter: ['==', ['get', 'k'], k], layout: { 'line-cap': 'round', 'line-join': 'round', visibility: 'none' }, paint: { 'line-opacity': opacity } }, 4.2, token);
      layers[name].push(lid);
    });
  };
  add('speed', runs('speed_kph', speedClass), SPEED_LOOK);
  add('power', runs('power_kw', powerClass), POWER_LOOK);
  const plainIds = lines.map((f, i) => `route-${i}-line`);
  const fmtS = (v) => `${format(v, 'number', { digits: 0 })}`;
  const speedNames = [`under ${fmtS(steps[0])}`, ...steps.slice(1).map((v, i) => `${fmtS(steps[i])} to ${fmtS(v)}`), `${fmtS(steps[steps.length - 1])} and over`].map((t) => `${t} ${unit}`);
  const powerNames = ['regen (charging the pack)', `0 to ${POWER_STEPS[1]} kW`, `${POWER_STEPS[1]} to ${POWER_STEPS[2]} kW`, `${POWER_STEPS[2]} kW and over`];
  const key = (looks, names, cls) => h('ul', { class: 'route-key' }, names.map((n, k) => h('li', null, h('span', { class: ['route-swatch', `${cls}-${k}`], 'aria-hidden': 'true' }), n)));
  return function paint() {
    const by = mode();
    const vis = (id, on) => m.setVisible(id, on);
    plainIds.forEach((lid) => vis(lid, by === 'plain'));
    layers.speed.forEach((lid) => vis(lid, by === 'speed'));
    layers.power.forEach((lid) => vis(lid, by === 'power'));
    if (by === 'speed') replace(legend, key(SPEED_LOOK, speedNames, 'sw-speed'));
    else if (by === 'power') replace(legend, key(POWER_LOOK, powerNames, 'sw-power'));
    else replace(legend, h('p', { class: 'route-plain' }, 'The route in one colour; the highlighted stretch is the part selected on the charts.'));
  };
}

/**
 * Time in each speed band: { list: [{ from, to, seconds }], width, unit, local }, from and to in `unit`.
 * The drive's own bands when the server made them `bandKph` wide (10 of the person's unit); else counted
 * here from the speed readings (each reading stands for its bucket's time, `local`); with neither, the
 * server's bands as they came, in km/h.
 */
function speedBands(d, s, bandKph) {
  const unit = unitLabel('speed');
  const own = (d.speed_bands || []).filter((b) => b.seconds > 0);
  const served = d.speed_band_kph != null ? Math.abs(d.speed_band_kph - bandKph) < 0.01 : bandKph === 10;
  if (served) {
    const u = (v) => (v == null ? null : Math.round(convert(v, 'speed')));
    return { list: own.map((b) => ({ from: u(b.from_kph), to: u(b.to_kph), seconds: b.seconds })), width: Math.round(convert(bandKph, 'speed')), unit, local: false };
  }
  if (!s || !has(s.values.speed_kph)) {
    const w = d.speed_band_kph || 10;
    return { list: own.map((b) => ({ from: b.from_kph, to: b.to_kph, seconds: b.seconds })), width: w, unit: 'km/h', local: false };
  }
  const w = Math.round(convert(bandKph, 'speed'));
  const secs = {};
  const step = (s.bucket_ms || 1000) / 1000;
  s.values.speed_kph.forEach((v) => {
    if (v == null) return;
    const k = Math.floor(convert(v, 'speed') / w);
    secs[k] = (secs[k] || 0) + step;
  });
  const list = Object.keys(secs).map(Number).sort((a, b) => a - b).map((k) => ({ from: k * w, to: (k + 1) * w, seconds: secs[k] }));
  return { list, width: w, unit, local: true };
}

/** Hours and minutes in the car's zone, for a file name ("1217"). */
function hhmm(ms, tz) {
  const p = new Intl.DateTimeFormat('en-GB', { timeZone: tz, hour: '2-digit', minute: '2-digit', hourCycle: 'h23' }).formatToParts(ms);
  const get = (t) => (p.find((x) => x.type === t) || {}).value || '00';
  return `${get('hour')}${get('minute')}`;
}

/** One part of the page that could not be loaded: what is missing, why, and Try again. */
function partError(err, title, retry) {
  const w = errorWords(err);
  const st = err && err.status;
  const text = st === 0 || st === 401 || st === 403 || st === 429 || st === 404 ? `${w.title} ${w.text}`
    : `The server answered with an error${st ? ` (${st})` : ''}; the rest of the page is fine.`;
  return h('div', { class: 'part-error', role: 'alert' },
    h('p', { class: 'part-error-title' }, title), h('p', { class: 'note' }, text),
    retry ? button('Try again', { onclick: retry, iconName: 'reset', kind: 'quiet' }) : null);
}
