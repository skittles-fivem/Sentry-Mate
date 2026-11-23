// Sentry Mate, "Logbook" UI pack: Charge detail (/cars/{car}/charges/{charge}).
//
// One charging session in full, with its cost editor on the same page. Data:
// GET /api/v1/cars/{car}/charges/{charge} (the charge, previous and next, the cost breakdown, gaps),
// /charges/{charge}/curve (1 Hz columns: power, battery, range, temperatures, heating, AC line or DC
// charger volts and amps, phases, pilot current, which limit applies), /charging-stats/dc-curve (this
// car's typical fast-charging curve), /drives (the drive before and after) and PUT or DELETE
// /charges/{charge}/cost (admin). Zooming a chart narrows the figures under "Selected part"; the charge
// and supply charts share one time axis and one readout.

import { h, svg, replace, cover } from '../shared/dom.js';
import { format, unitLabel, rangeField, rangeLabel, formatMoney } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, dayKey, zoneName, MINUTE, DAY } from '../shared/time.js';
import { formatList, formatNumber } from '../shared/i18n.js';
import { column, timeColumn, csvFilename, downloadCsv } from '../shared/csv.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { pageHead, q, figure, icon, sep, empty, errorWords, flag, chip } from '../core/ui.js';
import { makeMap, coordsText } from '../core/map.js';
import { stackedCharts, niceStep } from '../core/chart.js';
import { placeLink } from '../core/journal.js';
import { sourceMark, sourceLabel, isImported, isEstimated, ESTIMATED_WHY } from '../core/sources.js';
import { dataTable } from '../core/table.js';
import { field, form, choice, setFieldError } from '../core/form.js';
import { confirmDialog } from '../core/dialog.js';
import { remount } from '../core/router.js';
import { browserZone } from '../core/format.js';

export const id = 'charge';
export const css = true;
export const title = () => 'Charge';

// Fields asked of the curve. The AC line fields came later: a server without them answers 400 on
// `fields`, and the page asks again with the older set (the AC chart then uses what is there).
const CURVE_BASE = ['power_kw', 'volts_v', 'amps_a', 'soc_pct', 'usable_soc_pct', 'battery_c', 'range_km', 'outside_c',
  'phases', 'pilot_a', 'ac_line_kw', 'heater_kw', 'dc_volts_v', 'dc_amps_a'];
const CURVE_FIELDS = [...CURVE_BASE, 'ac_volts_v', 'ac_amps_a', 'limit_source'];

const CURRENCIES = ['USD', 'CAD', 'EUR', 'GBP', 'NOK', 'SEK', 'DKK', 'CHF', 'AUD', 'NZD', 'JPY', 'CNY'];
const MAX_RATE_DECIMALS = 5;

// What limits the current the car draws, in words.
const LIMIT = Object.freeze({
  station: 'Station (its pilot signal)', cable: 'Cable rating', car: 'Car’s own maximum', setting: 'Current set on the screen',
});
// How the cost was made, in words.
const COST_SOURCE = Object.freeze({
  tariff: 'Priced with the place’s price', manual: 'Entered by hand; kept through rebuilds',
  free_supercharging: 'Free Supercharging on this car', teslamate: 'Imported from TeslaMate',
});
const MODE_WORDS = Object.freeze({ total: 'a total', per_kwh: 'a price per kWh', per_minute: 'a price per minute' });

// A rate as entered: every decimal it has (up to MAX_RATE_DECIMALS), never fewer than the currency's own.
const currencyDigits = (cur) => { try { return new Intl.NumberFormat('en', { style: 'currency', currency: cur }).resolvedOptions().minimumFractionDigits; } catch { return 2; } };
function rateText(v, cur, kind) {
  const dec = (String(Number(v.toFixed(MAX_RATE_DECIMALS))).split('.')[1] || '').length;
  return format({ amount: v, currency: cur }, kind, { digits: Math.max(dec, currencyDigits(cur)) });
}

const has = (list) => (list || []).some((v) => v != null);
const numOnly = (list) => (list || []).map((v) => (typeof v === 'number' && Number.isFinite(v) ? v : null));

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz;
  const base = api.apiPath`cars/${car.id}/charges/${ctx.chargeId}`;
  const listUrl = state.buildUrl('charges', { car: car.id, period: ctx.state.period });
  let c;
  try { c = await ctx.api.get(base); } catch (err) {
    if (err && err.status === 404) {
      // An unknown charge, or one of a car this account may not see: the same answer, with the way back.
      ctx.main.append(pageHead({ eyebrow: crumbs(car, listUrl, null), title: 'No such charge' }),
        empty({
          title: 'There is no charge with this number for this car.',
          text: 'It may have been rebuilt from the logs under a new number, or the link is wrong. Its cost cannot be entered here.',
          action: h('a', { class: 'link-arrow', href: listUrl, cover: 'web.charge-cost.back' }, 'Find it on Charges', icon('arrowRight')),
          cover: 'web.charge-cost.not-found',
        }));
      ctx.ready();
      return {};
    }
    throw err;
  }
  const going = c.end_ms == null || (c.flags && c.flags.in_progress);
  const end = c.end_ms ?? api.now();
  const dc = c.kind === 'dc';

  const curveReq = (fields) => ctx.api.get(`${base}/curve`, { fields: fields.join(',') });
  const curveP = curveReq(CURVE_FIELDS).catch((err) => (err && err.status === 400 ? curveReq(CURVE_BASE) : Promise.reject(err))).catch((err) => ({ error: err }));
  const [curve, typical, before, after, place] = await Promise.all([
    curveP,
    dc ? ctx.api.get(api.apiPath`cars/${car.id}/charging-stats/dc-curve`, { from_ms: c.start_ms - 365 * DAY, to_ms: end + 1 }).catch(() => null) : null,
    ctx.api.get(api.apiPath`cars/${car.id}/drives`, { to_ms: c.start_ms, limit: 1 }).catch(() => null),
    going ? null : ctx.api.get(api.apiPath`cars/${car.id}/drives`, { from_ms: c.end_ms, sort: 'start', dir: 'asc', limit: 1 }).catch(() => null),
    !c.cost && c.place && c.place.id ? ctx.api.get(api.apiPath`places/${c.place.id}`).catch(() => null) : null,
  ]);
  if (!ctx.alive()) return {};
  const s = curve && !curve.error && curve.t_ms && curve.t_ms.length ? curve : null;
  const where = placeName(c);
  ctx.title(`${where}, ${formatDate(c.start_ms, tz, { weekday: false, now: api.now() })}`);

  /* ------------------------------------------------------------ head: breadcrumb, steps, kind */
  const link = (cid) => (cid ? state.buildUrl('charge', { car: car.id, charge: cid, period: ctx.state.period }) : null);
  const step = (href, text, dir) => (href
    ? h('a', { class: 'button button-quiet', href, rel: dir < 0 ? 'prev' : 'next' }, dir < 0 ? icon('chevronLeft') : null, text, dir > 0 ? icon('chevronRight') : null)
    : h('span', { class: 'button button-quiet is-disabled', 'aria-disabled': 'true', title: dir < 0 ? 'This is the first charge recorded' : 'This is the newest charge' }, dir < 0 ? icon('chevronLeft') : null, text, dir > 0 ? icon('chevronRight') : null));
  const asTrip = state.buildUrl('trip', { car: car.id, period: { from: c.start_ms, to: end } });
  const head = pageHead({
    eyebrow: crumbs(car, listUrl, formatDate(c.start_ms, tz, { weekday: 'long', month: 'long', year: true }), ['cd.context.session', 'cd.filter.car', 'web.charge-cost.breadcrumb']),
    title: where,
    deck: [`${formatTime(c.start_ms, tz)} – ${going ? 'now' : formatTime(c.end_ms, tz)}`, sep(), formatDuration(c.charging_ms), ' charging', sep(),
      kindChip(c),
      isImported(c) ? [' ', flag('Imported', { kind: 'estimated', title: `${sourceLabel(c)}: totals and a coarse curve` })] : null,
      !isImported(c) && isEstimated(c) ? [' ', flag('Estimated', { kind: 'estimated', title: ESTIMATED_WHY })] : null,
      going ? [' ', flag('In progress', { title: 'Figures up to now' })] : null,
      c.has_gaps ? [' ', flag('Data missing', { kind: 'warn', title: 'Part of this charge has no data; see the notes under the charts' })] : null,
      sourceMark(c, { block: true, cls: 'charge-head-src' })],
    aside: h('nav', { class: 'step-nav', 'aria-label': 'Other charges' },
      step(link(c.prev), 'Earlier', -1), step(link(c.next), 'Later', 1),
      h('a', { class: 'button button-quiet', href: asTrip, title: 'This charge’s span on the Trip page' }, 'As a trip')),
    cls: 'charge-head',
  });

  // A charge still going: say so when it ends, and offer the new figures (pages never poll).
  const liveNote = h('p', { class: 'charge-live', hidden: true, role: 'status' });
  if (going) {
    ctx.events(['rows'], (ev) => {
      const e = ev.data || {};
      if (e.car !== car.id || !(e.kinds || []).includes('charges')) return;
      replace(liveNote, 'This charge has new data. ', h('button', { type: 'button', class: 'button button-quiet button-small', onclick: () => remount() }, icon('reset'), 'Show it'));
      liveNote.hidden = false;
    });
  }

  /* ------------------------------------------------------------ figures */
  const stats = h('dl', { class: 'charge-stats' });
  const drawStats = () => replace(stats, chargeFigures(c, { tz, going, end }));
  drawStats();

  /* ------------------------------------------------------------ place and map */
  const pt = { place: c.place, lat: c.position && c.position.lat, lon: c.position && c.position.lon };
  const pl = placeLink(pt);
  // the note under the map follows the cost, so it is drawn again when a cost is saved or removed
  const mapFoot = h('p', { class: 'note map-foot' });
  const drawMapFoot = () => replace(mapFoot, c.position && c.position.lat != null
    ? [coordsText([c.position.lon, c.position.lat]), c.position.accuracy_m != null ? [', GPS within ', q(c.position.accuracy_m, 'length')] : null, mapCostNote(c)]
    : ['No position was recorded for this charge', mapCostNote(c)]);
  drawMapFoot();
  const mapBox = h('div', { class: 'map map-charge', role: 'region', 'aria-label': `Map of where the car charged: ${where}` });
  const placeEl = h('section', { class: 'charge-place', 'aria-labelledby': 'place-title', cover: ['cd.stat.location', 'cd.map.location', 'web.charge-cost.map', 'web.charge-cost.location'] },
    h('div', { class: 'side-head' }, h('h2', { class: 'label', id: 'place-title' }, 'Where'),
      pl ? h('a', { class: 'link-arrow', href: pl }, c.place ? 'Edit this place' : 'Name this place', icon('arrowRight')) : null),
    h('p', { class: 'place-name' }, where, c.place && c.place.kind ? h('span', { class: 'place-kind' }, kindWord(c.place.kind)) : null),
    h('p', { class: 'place-addr' }, addressText(c) || 'No address: the position has no address in the offline lookup.'),
    mapBox,
    mapFoot);

  /* ------------------------------------------------------------ the cost, with the editor */
  const costEl = h('section', { class: 'charge-cost', id: 'cost', 'aria-labelledby': 'cost-title' });
  const drawCost = () => replace(costEl, costView(ctx, c, { tz, car, listUrl, place, onSaved }));
  function onSaved(next) { c = { ...c, ...next }; drawCost(); drawStats(); drawMapFoot(); }
  drawCost();

  /* ------------------------------------------------------------ the drive before and after */
  const driveItem = (d, word) => {
    if (!d) return h('li', { class: 'near-row is-none' }, h('span', { class: 'near-word' }, word), h('span', { class: 'near-text' }, word === 'After' && going ? 'the charge is still going' : 'no drive recorded'));
    return h('li', { class: 'near-row' }, h('span', { class: 'near-word' }, word),
      h('a', { class: 'near-text', href: state.buildUrl('drive', { car: car.id, drive: d.id, period: ctx.state.period }) },
        `${d.start && d.start.label ? d.start.label : 'unnamed spot'} to ${d.end && d.end.label ? d.end.label : 'unnamed spot'}, `, q(d.distance_km, 'distance'),
        h('span', { class: 'near-when' }, `${formatDate(d.start_ms, tz, { now: api.now() })}, ${formatTime(d.start_ms, tz)}${d.end_ms ? ` – ${formatTime(d.end_ms, tz)}` : ''}`)));
  };
  const nearEl = h('section', { class: 'charge-near', 'aria-labelledby': 'near-title' },
    h('h2', { class: 'label', id: 'near-title' }, 'Drives either side'),
    h('ul', { class: 'near-list' }, driveItem(before && before.items && before.items[0], 'Before'), driveItem(after && after.items && after.items[0], 'After')));

  /* ------------------------------------------------------------ during the charge: readout, zoom, charts */
  const readout = h('div', { class: 'charge-readout', 'aria-live': 'off' });
  const selected = h('div', { class: 'selected', cover: 'cd.filter.time_range' });
  const resetBtn = h('button', { type: 'button', class: 'button button-quiet button-small', disabled: true }, icon('reset'), 'Show the whole charge');
  const csvBtn = h('button', { type: 'button', class: 'button button-quiet button-small', disabled: !s }, icon('download'), 'Download CSV');
  const chargeCharts = h('div', { class: 'charts', cover: ['cd.chart.timeline', 'cd.chart.soc', 'cd.chart.power', 'cd.chart.range', 'cd.chart.outdoor_temp', 'cd.chart.battery_heater'] });
  const supplyCharts = h('div', { class: 'charts' });
  const supplyNote = h('p', { class: 'note supply-note' });
  const notes = h('ul', { class: 'along-notes' });
  const curveEl = h('section', { class: 'block charge-curve', 'aria-labelledby': 'curve-title' });

  ctx.main.append(head, liveNote, h('div', { class: 'charge-body', cover: 'shots.charge-details' },
    h('div', { class: 'charge-col charge-col-a' },
      h('div', { class: 'charge-top' },
        h('section', { class: 'charge-figs', 'aria-label': 'Charge figures' }, stats),
        h('div', { class: 'charge-side' }, placeEl, nearEl)),
      costEl),
    h('div', { class: 'charge-col charge-col-b' },
    h('section', { class: 'along', 'aria-labelledby': 'along-title' },
      h('div', { class: 'along-head' }, h('h2', { class: 'label', id: 'along-title' }, 'During the charge', h('span', { class: 'label-note' }, s ? [s.step_ms > 1000 ? `one reading per ${formatDuration(s.step_ms)}` : 'one reading a second', (s.sources && s.sources.length) || s.source_label ? `; ${sourceLabel(s)}` : ''] : null)),
        h('div', { class: 'along-actions' }, resetBtn, csvBtn)),
      readout, selected,
      h('h3', { class: 'chart-group' }, 'Battery, power and temperatures'), chargeCharts,
      h('h3', { class: 'chart-group' }, dc ? 'Supply: the fast charger' : 'Supply: the AC line'), supplyCharts, supplyNote,
      notes,
      h('p', { class: 'along-hint' }, 'Point at a chart to read every value at that moment. Drag across a chart to zoom in on part of the charge; the figures under “Selected part” follow. Double-click a chart to zoom out.')),
    curveEl)));

  // The map: one pin where the car charged.
  let m = null;
  if (pt.lat != null && pt.lon != null) {
    m = await makeMap(ctx, mapBox, { center: [pt.lon, pt.lat], zoom: 15, maxZoom: 17, fullscreen: true });
    if (m) {
      await m.loaded;
      if (!ctx.alive()) return {};
      m.addPin([pt.lon, pt.lat], where, 'pin-charge');
    }
  } else {
    replace(mapBox, h('p', { class: 'map-note' }, 'No position was recorded, so there is nothing to show on a map.'));
    mapBox.classList.add('is-empty');
  }

  /* ------------------------------------------------------------ the charts */
  const V = s ? s.values : {};
  const val = (f, i) => (i != null && V[f] ? V[f][i] : null);
  const gaps = (c.gaps || []).map((g) => ({ start_ms: g.start_ms, end_ms: g.end_ms ?? end, kind: g.kind }));
  const states = s ? s.t_ms.map((t) => (gaps.some((g) => t >= g.start_ms && t < g.end_ms) ? 'unknown' : 'charging')) : null;
  const spans = gaps.map((g) => ({ start_ms: g.start_ms, end_ms: g.end_ms, state: 'unknown' }));
  const acVolts = has(V.ac_volts_v) ? V.ac_volts_v : null;
  const acAmps = has(V.ac_amps_a) ? V.ac_amps_a : null;
  const hasHeat = has(V.heater_kw), hasUsable = has(V.usable_soc_pct), hasLine = has(V.ac_line_kw);
  const hasBatt = has(V.battery_c), hasPhases = has(V.phases), hasPilot = has(V.pilot_a), hasLimit = has(V.limit_source);
  const hasDcV = has(V.dc_volts_v), hasDcA = has(V.dc_amps_a);
  const rangeName = 'Rated range'; // the curve keeps rated range only

  const reading = (name, value, cls = null) => h('div', { class: ['reading', cls] }, h('dt', null, name), h('dd', null, value));
  const curveErr = curve && curve.error ? curve.error : null;
  const showReadout = (i) => {
    if (!s) {
      replace(readout, curveErr
        ? h('p', { class: 'readout-idle readout-error', role: 'alert' }, 'The readings could not be loaded, so there is nothing to point at. ', h('button', { type: 'button', class: 'button button-quiet button-small', onclick: () => remount() }, icon('reset'), 'Try again'))
        : h('p', { class: 'readout-idle' }, 'No readings during this charge.'));
      return;
    }
    const idle = i == null;
    replace(readout,
      idle
        ? h('p', { class: 'reading-time readout-idle' }, `${formatNumber(s.t_ms.length)} readings, ${formatTime(s.t_ms[0], tz)} to ${going ? 'now' : formatTime(s.t_ms[s.t_ms.length - 1], tz)}. Point at a chart for every value at that moment.`)
        : h('p', { class: 'reading-time' }, h('time', null, formatTime(s.t_ms[i], tz, { seconds: true })), states[i] === 'unknown' ? h('span', { class: 'reading-state' }, 'no data') : null),
      h('dl', { class: 'readings' },
        reading('Power', q(val('power_kw', i), 'power')),
        hasLine ? reading('AC line power', q(val('ac_line_kw', i), 'power')) : null,
        reading('Battery', q(val('soc_pct', i), 'percent', { digits: 1 })),
        hasUsable ? reading('Usable', q(val('usable_soc_pct', i), 'percent', { digits: 1 })) : null,
        reading(rangeName, q(val('range_km', i), 'range')),
        hasBatt ? reading('Battery temp.', q(val('battery_c', i), 'temperature')) : null,
        reading('Outside', q(val('outside_c', i), 'temperature')),
        hasHeat ? reading('Battery heating', idle || val('heater_kw', i) == null ? q(null, 'power') : val('heater_kw', i) > 0 ? q(val('heater_kw', i), 'power') : 'off') : null,
        dc ? reading('Charger', idle ? '—' : [q(val('dc_volts_v', i), 'voltage'), ' · ', q(val('dc_amps_a', i), 'current')])
          : reading('AC line', idle ? '—' : [q(acVolts ? acVolts[i] : null, 'voltage'), ' · ', q(acAmps ? acAmps[i] : null, 'current')]),
        !dc && hasPilot ? reading('Pilot', q(val('pilot_a', i), 'current')) : null,
        !dc && hasPhases ? reading('Phases', val('phases', i) == null ? '—' : format(val('phases', i), 'count')) : null,
        !dc && hasLimit ? reading('Limited by', idle || val('limit_source', i) == null ? '—' : LIMIT[val('limit_source', i)] || val('limit_source', i)) : null));
  };
  showReadout(null);

  // Zoom: the selected part's figures, worked out from the readings in it.
  const onZoom = (from, to) => {
    const full = !s || (from <= s.t_ms[0] + 500 && to >= s.t_ms[s.t_ms.length - 1] - 500);
    resetBtn.disabled = full;
    if (s && hasLimit) drawLimit(full ? s.t_ms[0] : from, full ? s.t_ms[s.t_ms.length - 1] + s.step_ms : to);
    const idx = s ? s.t_ms.map((t, i) => (t >= from && t <= to ? i : -1)).filter((i) => i >= 0) : [];
    if (full || idx.length < 2) {
      replace(selected, h('p', { class: 'selected-idle' }, 'Selected part: drag across a chart to pick a stretch of the charge; its energy, power, battery and range appear here.'));
      return;
    }
    const f = selFigures(s, idx);
    replace(selected, h('p', { class: 'label' }, `Selected part, ${formatTime(f.from, tz, { seconds: true })} to ${formatTime(f.to, tz, { seconds: true })}`),
      h('dl', { class: 'figs selected-figs' },
        figure('Duration', formatDuration(f.to - f.from), `of ${formatDuration(c.charging_ms)} charging`),
        figure('Energy added', q(f.kwh, 'energy', { why: 'no power readings here' }), 'power × time, into the pack'),
        figure('Average power', q(f.avg, 'power', { why: 'no power readings here' }), f.peak != null ? ['peak ', format(f.peak, 'power')] : null),
        figure('Battery', f.soc0 != null && f.soc1 != null ? `${format(f.soc0, 'percent')} → ${format(f.soc1, 'percent')}` : q(null, 'percent', { why: 'not recorded here' }), f.soc0 != null && f.soc1 != null ? `${format(f.soc1 - f.soc0, 'number', { digits: 1 })} points` : null),
        figure(rangeName, f.r0 != null && f.r1 != null ? ['+', format(f.r1 - f.r0, 'range')] : q(null, 'range', { why: 'not recorded here' }), 'range added in this part')));
  };

  // Which limit applies, as a strip under the currents (AC only).
  const limitStrip = h('div', { class: 'limit-track', role: 'img' });
  const drawLimit = (from, to) => {
    const segs = [];
    const span = to - from || 1;
    (V.limit_source || []).forEach((v, i) => {
      const a = s.t_ms[i], z = s.t_ms[i + 1] ?? a + s.step_ms;
      if (z <= from || a >= to || v == null) return;
      const last = segs[segs.length - 1];
      if (last && last.v === v && Math.abs(last.z - a) < s.step_ms * 1.5) last.z = z;
      else segs.push({ v, a, z });
    });
    replace(limitStrip, segs.map((g) => h('span', {
      class: ['limit-seg', `lim-${g.v}`], title: `${LIMIT[g.v] || g.v}: ${formatTime(g.a, tz)} to ${formatTime(g.z, tz)}`,
      vars: { '--l': `${(Math.max(0, (g.a - from) / span) * 100).toFixed(3)}%`, '--w': `${(((Math.min(to, g.z) - Math.max(from, g.a)) / span) * 100).toFixed(3)}%` },
    })));
    const time = {};
    segs.forEach((g) => { time[g.v] = (time[g.v] || 0) + (Math.min(to, g.z) - Math.max(from, g.a)); });
    limitStrip.setAttribute('aria-label', `Current limited by: ${formatList(Object.entries(time).map(([k, ms]) => `${(LIMIT[k] || k).toLowerCase()} ${formatDuration(ms)}`))}`);
  };

  let plotA = null, plotB = null, syncing = false;
  const both = (src, from, to) => {
    if (!syncing) {
      syncing = true;
      const other = src === 'a' ? plotB : plotA;
      if (other) for (const p of other.plots) p.setScale('x', { min: from / 1000, max: to / 1000 });
      syncing = false;
    }
    onZoom(from, to);
  };
  if (s && s.t_ms.length > 1) {
    const rows = [
      { key: 'soc', name: 'Battery', kind: 'percent', note: hasUsable ? 'charge level solid, usable dashed (a cold pack locks part away)' : 'this car does not send usable %', values: V.soc_pct, extra: [{ values: V.usable_soc_pct, colour: '--dusk', dash: true }], colour: '--ink', ticks: 4 },
      { key: 'power', name: 'Charging power', kind: 'power', note: hasLine ? 'into the pack solid, drawn from the AC line dashed' : 'into the pack', values: V.power_kw, extra: hasLine ? [{ values: V.ac_line_kw, colour: '--ink-3', dash: true }] : [], colour: '--ochre', fill: '--chart-ochre-wash', floor: 0, ticks: 4 },
      { key: 'range', name: rangeName, kind: 'range', values: V.range_km, colour: '--ink-2', ticks: 4 },
      { key: 'temp', name: 'Temperatures', kind: 'temperature', note: hasBatt ? 'battery ochre, outside blue' : 'outside; this car sends no battery temperature', values: V.outside_c, colour: '--slate', extra: hasBatt ? [{ values: V.battery_c, colour: '--ochre' }] : [], ticks: 4 },
      hasHeat ? { key: 'heat', name: 'Battery heating', kind: 'power', note: 'power the pack spends warming itself; zero when not heating', values: V.heater_kw, colour: '--rust', floor: 0, ticks: 2 } : null,
    ].filter(Boolean);
    plotA = await stackedCharts(ctx, chargeCharts, { tz, t_ms: s.t_ms, states, spans, rows, onHover: showReadout, onZoom: (a, b) => both('a', a, b) });
    if (!ctx.alive()) return {};

    const supply = dc
      ? [
        hasDcV ? { key: 'volts', name: 'Charger voltage', kind: 'voltage', note: 'the fast charger’s own output (DC)', values: V.dc_volts_v, colour: '--ink-2', ticks: 3, cover: 'cd.chart.ac_voltage' } : null,
        hasDcA ? { key: 'amps', name: 'Charger current', kind: 'current', note: 'the fast charger’s own output (DC)', values: V.dc_amps_a, colour: '--ochre', floor: 0, ticks: 3, cover: 'cd.chart.ac_current' } : null,
      ]
      : [
        acVolts ? { key: 'volts', name: 'Line voltage', kind: 'voltage', note: 'AC at the inlet, one phase', values: acVolts, colour: '--ink-2', ticks: 3, cover: 'cd.chart.ac_voltage' } : null,
        acAmps || hasPilot ? { key: 'amps', name: 'Line current', kind: 'current', note: hasPilot ? 'drawn per phase solid; the station’s pilot (most it allows) dashed' : 'drawn per phase', values: acAmps || numOnly(V.pilot_a).map(() => null), extra: hasPilot ? [{ values: V.pilot_a, colour: '--rust', dash: true }] : [], colour: '--ochre', floor: 0, ticks: 3, cover: ['cd.chart.ac_current', 'cd.chart.pilot_current'] } : null,
        hasPhases ? { key: 'phases', name: 'Phases in use', unit: 'of 3', note: 'counted from the charger’s enabled phases each second', values: V.phases, colour: '--ink-2', floor: 0, ticks: 3, digits: 0, cover: 'cd.chart.phases' } : null,
      ];
    const srows = supply.filter(Boolean);
    if (srows.length) {
      plotB = await stackedCharts(ctx, supplyCharts, { tz, t_ms: s.t_ms, states, spans, rows: srows, onHover: showReadout, onZoom: (a, b) => both('b', a, b) });
      if (!ctx.alive()) return {};
      srows.forEach((r) => cover(supplyCharts.querySelector(`.chart-${r.key}`), r.cover));
      if (!dc && hasLimit) {
        const row = h('div', { class: 'chart-row chart-limit', cover: 'cd.chart.pilot_current' },
          h('div', { class: 'chart-label' }, h('span', { class: 'chart-name' }, 'Limited by'), h('span', { class: 'chart-note' }, 'what set the most the car could draw')),
          h('div', { class: 'chart-plot' }, limitStrip, h('ul', { class: 'limit-key' }, Object.entries(LIMIT).map(([k, t]) => h('li', null, h('span', { class: ['limit-swatch', `lim-${k}`], 'aria-hidden': 'true' }), t)))));
        const anchor = supplyCharts.querySelector('.chart-amps');
        if (anchor) anchor.after(row); else supplyCharts.append(row);
      }
    } else {
      replace(supplyCharts, h('p', { class: 'note supply-empty' }, dc ? 'The fast charger sent no voltage or current for this charge.' : 'No AC line readings were recorded for this charge.'));
    }
    if (dc) {
      replace(supplyNote,
        h('span', { cover: ['cd.chart.phases', 'cd.chart.pilot_current'] }, 'AC phases in use: none, and no pilot current: a fast charger feeds the pack with direct current, so there is no AC line to count phases on and no station pilot signal. '),
        c.fast_charger && c.fast_charger.billed_kwh != null ? ['The charger reports ', format(c.fast_charger.billed_kwh, 'energy'), ' delivered at the car’s inlet.'] : null);
    } else {
      replace(supplyNote, hasPhases ? null : h('span', { cover: 'cd.chart.phases' }, 'This car does not say how many phases it uses. '),
        hasPilot ? null : h('span', null, 'No station pilot current was recorded. '),
        acVolts ? null : h('span', null, 'Line voltage is not recorded for this charge; the charger’s pack-side volts are in the CSV. '));
    }

    resetBtn.addEventListener('click', () => { plotA?.resetZoom(); plotB?.resetZoom(); });
    onZoom(s.t_ms[0], s.t_ms[s.t_ms.length - 1]);
    csvBtn.addEventListener('click', () => downloadCsv(csvFilename(car.name, 'charge', where, dayKey(c.start_ms, tz)), s.t_ms.map((t, i) => ({ t, i })), [
      timeColumn('Time', 't', tz),
      { label: 'Data', value: (r) => (states[r.i] === 'unknown' ? 'missing' : 'recorded') },
      column('Charging power', (r) => val('power_kw', r.i), 'power', { digits: 2 }),
      column('AC line power', (r) => val('ac_line_kw', r.i), 'power', { digits: 2 }),
      column('Battery', (r) => val('soc_pct', r.i), 'percent', { digits: 2 }),
      column('Usable battery', (r) => val('usable_soc_pct', r.i), 'percent', { digits: 2 }),
      column(rangeName, (r) => val('range_km', r.i), 'range', { digits: 2 }),
      column('Battery temperature', (r) => val('battery_c', r.i), 'temperature', { digits: 1 }),
      column('Outside', (r) => val('outside_c', r.i), 'temperature', { digits: 1 }),
      column('Battery heating', (r) => val('heater_kw', r.i), 'power', { digits: 2 }),
      column('Pack voltage', (r) => val('volts_v', r.i), 'voltage', { digits: 1 }),
      column('Pack current', (r) => val('amps_a', r.i), 'current', { digits: 1 }),
      column('AC line voltage', (r) => (acVolts ? acVolts[r.i] : null), 'voltage', { digits: 1 }),
      column('AC line current', (r) => (acAmps ? acAmps[r.i] : null), 'current', { digits: 1 }),
      column('Pilot current', (r) => val('pilot_a', r.i), 'current', { digits: 1 }),
      { label: 'Phases', value: (r) => val('phases', r.i) },
      { label: 'Limited by', value: (r) => val('limit_source', r.i) },
      column('DC charger voltage', (r) => val('dc_volts_v', r.i), 'voltage', { digits: 1 }),
      column('DC charger current', (r) => val('dc_amps_a', r.i), 'current', { digits: 1 }),
    ]));
  } else if (curve && curve.error) {
    replace(chargeCharts, h('div', { class: 'empty empty-error', role: 'alert' },
      h('p', { class: 'eyebrow' }, 'Readings'),
      h('p', { class: 'empty-title' }, 'The readings could not be loaded.'),
      h('p', { class: 'empty-text' }, `The figures above are fine; only this charge’s second-by-second readings failed (${curveWhy(curve.error)}). The charts, the CSV and the power curve need them.`),
      h('p', { class: 'empty-action' }, h('button', { type: 'button', class: 'button button-quiet', onclick: () => remount() }, icon('reset'), 'Try again'))));
    replace(supplyCharts, h('p', { class: 'note supply-empty' }, 'Supply readings could not be loaded either; they come with the readings above.'));
    csvBtn.title = 'Nothing to download: the readings could not be loaded';
    replace(selected);
  } else {
    replace(chargeCharts, empty({ title: 'No readings during this charge.', text: isImported(c) ? 'Imported charges keep only their totals; the figures above are what there is.' : 'The log for this charge holds no samples. The Data health page shows whether its log arrived and passed its check.' }));
    replace(supplyCharts);
    replace(selected);
  }

  // What is missing, and why, under the charts.
  const missing = [];
  if (s && !hasHeat) missing.push('This car does not send battery heating power, so there is no heating chart.');
  if (gaps.length) missing.push(`No data for ${formatList(gaps.map((g) => `${formatTime(g.start_ms, tz)} to ${formatTime(g.end_ms, tz)} (${g.kind === 'dropped' ? 'frames dropped' : g.kind === 'chain' ? 'a log is missing' : 'no log, or one that failed its check'})`))}; shaded red, the lines break there and nothing is carried across.`);
  if (s && rangeLabel() !== rangeName) missing.push('You prefer ideal range; the charge curve keeps rated range only, so the range chart shows rated range.');
  replace(notes, missing.map((t) => h('li', null, t)));

  /* ------------------------------------------------------------ the power curve */
  replace(curveEl, powerCurve(ctx, c, s, typical, { car, where, error: curveErr }));

  if (m) await Promise.race([m.ready, new Promise((r) => setTimeout(r, 5000))]);
  ctx.ready();
  if (ctx.query && ctx.query.edit === 'cost') {
    costEl.scrollIntoView({ block: 'start' });
    const amt = costEl.querySelector('input[name="amount"]');
    if (amt) amt.focus({ preventScroll: true });
  }
  return {};
}

/* ================================================================== parts */

function crumbs(car, listUrl, last, cover = null) {
  return h('nav', { class: 'crumbs', 'aria-label': 'Breadcrumb', cover },
    h('a', { href: state.buildUrl('car', { car: car.id }) }, car.name), h('span', { class: 'crumb-sep', 'aria-hidden': 'true' }, '/'),
    h('a', { href: listUrl }, 'Charges'),
    last ? [h('span', { class: 'crumb-sep', 'aria-hidden': 'true' }, '/'), h('span', { 'aria-current': 'page' }, last)] : null);
}

// Why the readings failed, in a few words for one section (not the whole-page wording).
function curveWhy(err) {
  const w = errorWords(err);
  if (w.title !== 'This page could not be loaded.') return w.title.replace(/\.$/, '').toLowerCase();
  return err && err.status ? `the server answered ${err.status}` : 'an error on the way';
}

// What the place's price does for this charge's cost, from how the cost was made.
function mapCostNote(c) {
  const src = c.cost && c.cost.source;
  if (src === 'manual') return '. A cost was entered by hand, so the place’s price is not used.';
  if (src === 'teslamate') return '. The cost was imported from TeslaMate, so the place’s price is not used.';
  if (src === 'free_supercharging') return '. Supercharging was free for this car, so the place’s price is not used.';
  if (src === 'tariff') return '. The place’s price sets the cost unless one is entered below.';
  return c.place ? '. The place’s price would set the cost, but it has none: enter a cost below or give the place a price.' : '. No place here, so no price: enter a cost below or name the place with a price.';
}

const placeName = (c) => (c.place && c.place.name ? c.place.name : c.address && c.address.short ? c.address.short : 'Unnamed spot');
function addressText(c) {
  const a = c.address;
  if (!a) return '';
  const parts = (a.short || '').split(',').map((x) => x.trim()).filter(Boolean);
  for (const x of [a.locality, a.region]) if (x && !parts.includes(x)) parts.push(x);
  return parts.join(', ');
}
const kindWord = (k) => ({ home: 'home', work: 'work', charger: 'charger', service: 'service centre', other: 'place' }[k] || k);

/** 'DC · Supercharger V3' or 'AC · 1 phase, 32 A' as a chip. */
function kindChip(c) {
  if (c.kind === 'dc') {
    const fc = c.fast_charger;
    const what = fc ? [fc.type === 'supercharger' ? 'Supercharger' : fc.type ? fc.type.toUpperCase() : 'fast charger', fc.class].filter(Boolean).join(' ') : 'fast charger';
    return chip(`DC · ${what}`, { kind: 'plug', iconName: 'plug' });
  }
  if (c.kind === 'ac') return chip('AC', { kind: 'plug', iconName: 'plug' });
  return chip('Kind not known', { title: 'The car did not say whether this was AC or DC' });
}

/** The figures of the charge; each blank one says why. */
function chargeFigures(c, { tz, going, end }) {
  const dc = c.kind === 'dc';
  const imported = isImported(c);
  // rated or ideal: the charge keeps rated range only, so say which one is shown
  const rk = (f) => { const k = rangeField(f); return c[k] != null ? k : f; };
  const rLabel = rangeField('start_range_km') !== 'start_range_km' && c[rangeField('start_range_km')] == null ? 'Rated range' : rangeLabel();
  const r0 = c[rk('start_range_km')], r1 = c[rk('end_range_km')];
  const rAdd = c.range_added_km ?? (r0 != null && r1 != null ? r1 - r0 : null);
  const soc0 = c.start_soc_pct, soc1 = c.end_soc_pct;
  const socAdd = soc0 != null && soc1 != null ? soc1 - soc0 : null;
  const perPct = socAdd > 0.5 && c.energy_added_kwh != null ? c.energy_added_kwh / socAdd : null;
  const eff = c.efficiency_pct;
  // while it charges, the second value is the latest one, not the end
  const latestWord = going ? 'now ' : 'end ';
  const latestNote = going ? h('span', { class: 'trio-word' }, ` now, ${formatTime(end, tz)}`) : null;
  const toFull = c.to_full_kwh ?? c.energy_to_complete_kwh ?? null;
  return [
    figure('Charging time', formatDuration(c.charging_ms), [going ? `so far, up to ${formatTime(end, tz)}; ` : '', c.plugged_ms != null ? ['plugged in ', formatDuration(c.plugged_ms), ', first to last second of charging'] : 'first to last second of charging'], { cover: 'cd.stat.duration' }),
    figure('Energy added', q(c.energy_added_kwh, 'energy', { why: 'not recorded' }), imported ? 'into the pack; imported, estimated from range' : isEstimated(c) ? 'into the pack; estimated from the Tesla API’s power readings' : 'into the pack, measured at the pack (volts × amps)', { cover: 'cd.stat.energy_added' }),
    dc
      ? figure('Energy drawn', q(null, 'energy', { why: 'unknown on DC' }), c.dc_inlet_kwh != null ? ['a fast charger’s grid side is out of sight; ', format(c.dc_inlet_kwh, 'energy'), ' reached the car’s inlet', c.fast_charger && c.fast_charger.billed_kwh != null ? [', billed ', format(c.fast_charger.billed_kwh, 'energy')] : ''] : 'a fast charger’s grid side is out of sight', { cover: 'cd.stat.energy_used' })
      : figure('Energy drawn', q(c.energy_grid_kwh, 'energy', { why: imported ? 'not in the imported row' : 'the car did not report it' }), c.losses_kwh != null ? ['from the AC line; ', format(c.losses_kwh, 'energy'), ' lost on the way in'] : 'from the AC line, by the car’s charger', { cover: 'cd.stat.energy_used' }),
    figure('Efficiency', dc ? q(null, 'percent', { why: 'unknown on DC' }) : q(eff, 'percent', { why: c.efficiency_note || 'needs the energy drawn' }),
      dc ? 'needs the grid energy, which a fast charger does not send' : eff != null && eff > 100 ? [flag('Check', { kind: 'warn' }), ' above 100 %: a reading is off (often a three-phase line counted as one)'] : 'energy added ÷ energy drawn', { cover: 'cd.stat.efficiency' }),
    figure('Battery', h('span', { class: 'trio' },
      h('span', { cover: 'cd.stat.soc_start' }, h('span', { class: 'sr-only' }, 'start '), q(soc0, 'percent', { why: 'not recorded' })), h('span', { class: 'arrow', 'aria-hidden': 'true' }, ' → '),
      h('span', { cover: 'cd.stat.soc_end' }, h('span', { class: 'sr-only' }, latestWord), q(soc1, 'percent', { why: 'not recorded' })), latestNote),
    h('span', { cover: 'cd.stat.soc_added' }, socAdd != null ? [`+${format(socAdd, 'number', { digits: 1 })} points added${going ? ' so far' : ''}`, perPct != null ? ['; ', format(perPct, 'energy', { digits: 2 }), ' per point'] : null] : 'points added: not known'), { cls: 'fig-trio' }),
    figure(rLabel, h('span', { class: 'trio' },
      h('span', { cover: 'cd.stat.range_start' }, h('span', { class: 'sr-only' }, 'start '), q(r0, 'range', { why: 'not recorded' })), h('span', { class: 'arrow', 'aria-hidden': 'true' }, ' → '),
      h('span', { cover: 'cd.stat.range_end' }, h('span', { class: 'sr-only' }, latestWord), q(r1, 'range', { why: 'not recorded' })), latestNote && latestNote.cloneNode(true)),
    h('span', { cover: 'cd.stat.range_added' }, rAdd != null ? ['+', format(rAdd, 'range'), going ? ' added so far' : ' added', c.range_per_hour_km != null ? ['; ', format(c.range_per_hour_km, 'range_rate'), ' of charging'] : null] : 'range added: not known'), { cls: 'fig-trio' }),
    figure('Average power', q(c.power_avg_kw, 'power', { why: 'not recorded' }), c.power_max_kw != null ? ['energy added ÷ charging time; peak ', format(c.power_max_kw, 'power')] : 'energy added ÷ charging time', { cover: 'cd.stat.avg_power' }),
    figure('Temperatures', h('span', { class: 'trio' }, q(c.outside_avg_c, 'temperature', { why: 'not recorded' }), h('span', { class: 'trio-word' }, ' outside')),
      [imported ? 'from the imported row' : 'average of every second', c.battery_avg_c != null ? ['; battery ', format(c.battery_avg_c, 'temperature')] : ['; battery: ', imported ? 'not in imported history' : 'not sent']], { cover: 'cd.stat.avg_temp' }),
    figure(going ? 'Still to add' : 'Still to add when stopped', q(toFull, 'energy', { why: 'not sent yet' }),
      toFull != null ? (going ? 'energy the battery still needs to reach its limit' : 'energy the battery still needed to reach its limit when charging stopped') : `what the battery ${going ? 'still needs' : 'still needed when charging stopped'} to reach its limit; the server does not send it for charges yet`),
    figure('Odometer', q(c.odometer_km, 'odometer', { why: 'not read' }), 'when it plugged in'),
  ];
}

/** The cost: what it is and how it was made, the breakdown per rate, and the editor (admins). */
function costView(ctx, c, { tz, car, listUrl, place, onSaved }) {
  const cost = c.cost;
  const bd = c.cost_breakdown || { parts: [], session_fee: null, total: null };
  const admin = ctx.me && ctx.me.role === 'admin';
  const ppk = c.price_per_kwh;
  const cur = cost ? cost.currency : null;

  const current = h('div', { class: 'cost-now', cover: ['cd.stat.cost', 'web.charge-cost.current-cost'] },
    h('p', { class: 'fig-name' }, 'Cost'),
    h('p', { class: 'cost-amount' }, cost ? formatMoney(cost.amount, cost.currency) : q(null, 'money', { why: 'no price' })),
    h('p', { class: 'fig-note' }, cost ? COST_SOURCE[cost.source] || cost.source : c.place ? `${c.place.name} has no price, and none was entered.` : 'No place with a price here, and none was entered.'),
    ppk && ppk.value != null && cur ? h('p', { class: 'fig-note' }, rateText(ppk.value, cur, 'price_energy'), ppk.basis === 'grid' ? ' over the energy drawn from the grid' : c.kind === 'dc' ? ' over the energy added to the pack (the grid side of a fast charger is not known)' : ' over the energy added to the pack, as entered') : null,
    bd.gap_ms > 0 ? h('p', { class: 'fig-note' }, `${formatDuration(bd.gap_ms)} without data is priced from the energy at its ends.`) : null);

  const rows = (bd.parts || []).map((p) => ({ ...p }));
  if (bd.session_fee != null) rows.push({ rate: 'Session fee', per_kwh: null, per_minute: null, kwh: null, minutes: null, cost: bd.session_fee, fee: true });
  const money = (v) => (v == null || !cur ? '—' : formatMoney(v, cur));
  const rate = (v, kind) => (v == null || !cur ? '—' : rateText(v, cur, kind));
  const table = rows.length ? dataTable(ctx, {
    rows, itemName: ['line', 'lines'], label: 'Cost breakdown',
    caption: ['Total ', money(bd.total ?? (cost && cost.amount)), bd.mode === 'tariff' ? '; each rate over the time it applied' : ''],
    columns: [
      { key: 'rate', label: 'Rate', th: true, cell: (r) => r.rate, value: (r) => r.rate },
      { key: 'kwh', label: `Energy (${unitLabel('energy')})`, num: true, cell: (r) => (r.kwh == null ? '—' : format(r.kwh, 'energy', { unit: false })), value: (r) => r.kwh },
      { key: 'minutes', label: 'Time (min)', num: true, cell: (r) => (r.minutes == null ? '—' : format(r.minutes, 'number', { digits: 1 })), value: (r) => r.minutes },
      { key: 'per_kwh', label: 'Per kWh', num: true, cell: (r) => rate(r.per_kwh, 'price_energy'), value: (r) => r.per_kwh },
      { key: 'per_minute', label: 'Per minute', num: true, cell: (r) => rate(r.per_minute, 'price_time'), value: (r) => r.per_minute },
      { key: 'cost', label: cur ? `Cost (${cur})` : 'Cost', num: true, cell: (r) => money(r.cost), value: (r) => r.cost },
    ],
    csv: { filename: csvFilename(car.name, 'charge', dayKey(c.start_ms, tz), 'cost'), columns: [
      { label: 'Rate', value: (r) => r.rate }, { label: 'Energy (kWh)', value: (r) => r.kwh }, { label: 'Time (min)', value: (r) => r.minutes },
      { label: `Per kWh (${cur || ''})`, value: (r) => r.per_kwh }, { label: `Per minute (${cur || ''})`, value: (r) => r.per_minute }, { label: `Cost (${cur || ''})`, value: (r) => r.cost }] },
    cls: 'cost-table',
  }) : null;
  if (table) table.load();

  const left = h('div', { class: 'cost-left' }, current,
    table ? table.el : h('p', { class: 'note' }, cost && cost.source === 'free_supercharging' ? 'Nothing to break down: Supercharging was free for this car on this day (Settings: Cars).' : 'No rates to break down.'));

  const right = admin ? costEditor(ctx, c, { tz, car, listUrl, place, onSaved })
    : h('div', { class: 'cost-edit' }, h('p', { class: 'note' }, 'Only an admin account can enter or remove a cost. ', h('a', { href: listUrl }, 'Back to Charges'), '.'));

  return [h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'cost-title' }, 'Cost', h('span', { class: 'label-note' }, cur ? `in ${cur}` : null))),
    h('div', { class: 'cost-grid' }, left, right)];
}

/** The cost editor: a total, a price per kWh or per minute, the currency, and the energy it multiplies. */
function costEditor(ctx, c, { tz, car, listUrl, place, onSaved }) {
  const manual = c.cost && c.cost.source === 'manual';
  const entered = c.manual_cost || (manual ? { mode: (c.cost_breakdown && c.cost_breakdown.mode) || 'total', amount: c.cost.amount, currency: c.cost.currency } : null);
  let mode = entered && MODE_WORDS[entered.mode] ? entered.mode : 'total';
  const gridKnown = c.energy_grid_kwh != null;
  let basis = (entered && entered.energy_basis) || (gridKnown ? 'grid' : 'pack');
  if (!gridKnown) basis = 'pack';
  const minutes = c.charging_ms / MINUTE;
  const kwhOf = (b) => (b === 'grid' ? c.energy_grid_kwh : c.energy_added_kwh);
  const currency0 = (entered && entered.currency) || (c.cost && c.cost.currency) || (place && place.price && place.price.currency) || 'USD';

  // the session the cost is for, in the car's zone (and the viewer's, when it differs)
  const zoneDiffers = browserZone && browserZone !== tz;
  const span = `${formatDate(c.start_ms, tz, { now: api.now() })}, ${formatTime(c.start_ms, tz)} – ${c.end_ms ? formatTime(c.end_ms, tz) : 'now'} ${zoneName(c.start_ms, tz)}`;
  const facts = h('dl', { class: 'cost-facts' },
    h('div', { cover: 'web.charge-cost.dates' }, h('dt', null, 'When'), h('dd', null, span,
      zoneDiffers ? h('span', { class: 'fact-sub' }, `your time: ${formatDate(c.start_ms, browserZone, { now: api.now() })}, ${formatTime(c.start_ms, browserZone)} ${zoneName(c.start_ms, browserZone)}`) : null)),
    h('div', { cover: 'web.charge-cost.car' }, h('dt', null, 'Car'), h('dd', null, car.name)),
    h('div', { cover: 'web.charge-cost.energy' }, h('dt', null, 'Energy'), h('dd', null, 'added ', q(c.energy_added_kwh, 'energy'), gridKnown ? ['; drawn ', q(c.energy_grid_kwh, 'energy'), c.losses_kwh != null ? [' (', format(c.losses_kwh, 'energy'), ' lost)'] : null] : '; drawn: not known on DC')),
    h('div', { cover: 'web.charge-cost.duration' }, h('dt', null, 'Charging'), h('dd', null, formatDuration(c.charging_ms), ` (${formatNumber(Math.round(minutes * 10) / 10)} min)`)),
    h('div', null, h('dt', null, 'Place'), h('dd', null, placeName(c))));

  const amount = field({ label: 'Amount', name: 'amount', type: 'number', value: entered ? entered.amount : null,
    hint: `Rates take up to ${MAX_RATE_DECIMALS} decimals (0.34567); only the total is rounded.`,
    attrs: { step: 'any', min: '0', inputmode: 'decimal', autocomplete: 'off', placeholder: c.cost ? String(c.cost.amount) : '0.00' }, cover: ['web.charge-cost.amount', 'web.charge-cost.rate-precision'] });
  const currency = field({ label: 'Currency', name: 'currency', type: 'select', value: currency0, options: [...new Set([currency0, ...CURRENCIES])].map((x) => [x, x]) });
  const modeChoice = choice({ label: 'The amount is', name: 'cost-mode', value: mode, cover: 'web.charge-cost.mode',
    options: [['total', 'The total'], ['per_kwh', 'Per kWh'], ['per_minute', 'Per minute']], onchange: (v) => { mode = v; sync(); } });
  const basisChoice = choice({ label: 'Multiply by', name: 'cost-basis', value: basis,
    options: gridKnown ? [['grid', `Energy drawn, ${format(c.energy_grid_kwh, 'energy')}`], ['pack', `Energy added, ${format(c.energy_added_kwh, 'energy')}`]] : [['pack', `Energy added, ${format(c.energy_added_kwh, 'energy')}`]],
    onchange: (v) => { basis = v; sync(); } });
  const basisWrap = h('div', { class: 'cost-basis' }, basisChoice, gridKnown ? null : h('p', { class: 'field-hint' }, 'A fast charger does not send the grid energy, so the energy added to the pack is used.'));
  const preview = h('p', { class: 'cost-preview', 'aria-live': 'polite' });

  const totalFor = (v) => (v == null || !Number.isFinite(v) ? null : mode === 'total' ? v : mode === 'per_kwh' ? (kwhOf(basis) == null ? null : v * kwhOf(basis)) : v * minutes);
  function sync() {
    amount.querySelector('.field-label').textContent = mode === 'total' ? 'Total' : mode === 'per_kwh' ? 'Price per kWh' : 'Price per minute';
    basisWrap.hidden = mode !== 'per_kwh';
    const raw = amount.control.value;
    const v = raw === '' ? null : Number(raw);
    const cur = currency.control.value;
    if (v == null) { replace(preview, 'Type an amount to see the total before saving.'); return; }
    if (!Number.isFinite(v) || v < 0) { replace(preview, 'Not a cost: enter zero or more.'); return; }
    const total = totalFor(v);
    replace(preview, 'Saves ', h('b', null, formatMoney(Math.round(total * 100) / 100, cur)),
      mode === 'total' ? ' as the total.' : mode === 'per_kwh' ? [' = ', rateText(v, cur, 'price_energy'), ' × ', format(kwhOf(basis), 'energy'), basis === 'grid' ? ' drawn.' : ' added.'] : [' = ', rateText(v, cur, 'price_time'), ' × ', `${formatNumber(Math.round(minutes * 10) / 10)} min charging.`]);
  }
  amount.control.addEventListener('input', sync);
  currency.control.addEventListener('change', sync);

  const goneText = 'This charge no longer exists: it was rebuilt from the logs, or removed. Open it again from Charges.';
  const f = form({
    fields: [modeChoice, amount, currency], submitLabel: manual ? 'Save the new cost' : 'Save the cost', cls: 'cost-form', cover: ['cd.link.set_cost', 'web.charge-cost.entry', 'web.charge-cost.save', 'web.charge-cost.error', 'web.charge-cost.not-found'],
    extra: [basisWrap, preview],
    onsubmit: async (v) => {
      const raw = amount.control.value.trim();
      if (raw === '' || v.amount == null) throw Object.assign(new Error('Enter an amount.'), { fields: { amount: manual ? 'Enter an amount, or remove the entered cost below to go back to the place price.' : 'Enter an amount (0 for a free charge).' } });
      if (!Number.isFinite(v.amount)) throw Object.assign(new Error('Not a number.'), { fields: { amount: 'Not a number.' } });
      if (v.amount < 0) throw Object.assign(new Error('Below zero.'), { fields: { amount: 'A cost cannot be below zero. Enter 0 for a free charge.' } });
      const dec = (raw.split(/[.,]/)[1] || '').replace(/e.*$/i, '').length;
      if (dec > (mode === 'total' ? 2 : MAX_RATE_DECIMALS)) throw Object.assign(new Error('Too many decimals.'), { fields: { amount: mode === 'total' ? 'A total takes at most 2 decimals.' : `A rate takes at most ${MAX_RATE_DECIMALS} decimals.` } });
      if (mode === 'per_kwh' && kwhOf(basis) == null) throw Object.assign(new Error('No energy.'), { fields: { amount: 'This charge has no energy figure to multiply; enter a total.' } });
      let next;
      try {
        next = await ctx.api.put(api.apiPath`cars/${car.id}/charges/${c.id}/cost`, { mode, amount: v.amount, currency: v.currency, ...(mode === 'per_kwh' ? { energy_basis: basis } : {}) });
      } catch (err) {
        if (err && err.status === 404) throw Object.assign(new Error(goneText), { fields: null });
        throw err;
      }
      ctx.flash(`Cost saved: ${formatMoney(next.cost ? next.cost.amount : totalFor(v.amount), v.currency)}.`);
      onSaved(next);
    },
  });

  const removeBtn = manual ? h('button', { type: 'button', class: 'button button-quiet', cover: 'web.charge-cost.clear-and-negative', onclick: async () => {
    const ok = await confirmDialog({ title: 'Remove the entered cost?', text: c.place ? `The charge goes back to the price of ${c.place.name}, or to no cost when that place has no price.` : 'The charge goes back to no cost: there is no place with a price here.', yes: 'Remove it', danger: true });
    if (!ok || !ctx.alive()) return;
    try {
      const next = await ctx.api.del(api.apiPath`cars/${car.id}/charges/${c.id}/cost`);
      ctx.flash(next.cost ? `Entered cost removed; the place price gives ${formatMoney(next.cost.amount, next.cost.currency)}.` : 'Entered cost removed; this charge has no cost now.');
      onSaved(next);
    } catch (err) {
      ctx.flash(`Not removed: ${err && err.status === 404 ? goneText : (err && err.message) || 'the server said no'}`, { kind: 'error' });
    }
  } }, 'Remove the entered cost') : h('p', { class: 'field-hint', cover: 'web.charge-cost.clear-and-negative' }, 'Costs are zero or more. An entered cost can be removed later to go back to the place price.');
  if (manual) f.querySelector('.form-actions').append(removeBtn); else f.append(removeBtn);
  sync();
  setFieldError(amount, null);

  return h('div', { class: 'cost-edit' },
    h('h3', { class: 'cost-edit-title' }, manual ? 'Change the entered cost' : 'Enter a cost'),
    h('p', { class: 'note' }, manual ? `Entered as ${MODE_WORDS[mode]}. It is kept when the charge is rebuilt from the logs, and place prices never change it.` : 'An entered cost replaces the place price for this charge only, and survives rebuilds.'),
    facts, f,
    h('a', { class: 'link-arrow cost-back', href: listUrl, cover: 'web.charge-cost.back' }, 'Back to Charges', icon('arrowRight')));
}

/** Power against battery %: this charge's readings, its average per point, and this car's typical DC curve. */
function powerCurve(ctx, c, s, typical, { car, where, error = null }) {
  const dc = c.kind === 'dc';
  const pts = [];
  if (s) {
    const P = s.values.power_kw || [], S = s.values.soc_pct || [], B = s.values.battery_c || [];
    for (let i = 0; i < s.t_ms.length; i++) if (P[i] != null && S[i] != null && P[i] > 0.05) pts.push({ soc: S[i], kw: P[i], batt: B[i] ?? null });
  }
  const byPct = new Map();
  pts.forEach((p) => { const k = Math.floor(p.soc); const g = byPct.get(k) || { n: 0, sum: 0 }; g.n++; g.sum += p.kw; byPct.set(k, g); });
  const avg = [...byPct.entries()].sort((a, b) => a[0] - b[0]).map(([k, g]) => ({ soc: k + 0.5, kw: g.sum / g.n }));
  const med = dc && typical && typical.median ? typical.median.map((p) => ({ soc: p.soc_pct, kw: p.power_kw })).filter((p) => p.kw != null) : [];
  const sessions = dc && typical && typical.sessions ? typical.sessions.length : 0;

  const csvBtn = h('button', { type: 'button', class: 'button button-quiet button-small', disabled: !avg.length, title: avg.length ? null : error ? 'Nothing to download: the readings could not be loaded' : 'Nothing to download: no curve', onclick: () => {
    const keys = [...new Set([...avg.map((p) => Math.floor(p.soc)), ...med.map((p) => Math.round(p.soc))])].sort((a, b) => a - b);
    downloadCsv(csvFilename(car.name, 'charge', where, dayKey(c.start_ms, ctx.tz), 'power curve'), keys.map((k) => ({ k })), [
      { label: 'Battery (%)', value: (r) => r.k },
      column('This charge, average power', (r) => { const g = byPct.get(r.k); return g ? g.sum / g.n : null; }, 'power', { digits: 2 }),
      { label: 'Readings', value: (r) => (byPct.get(r.k) || { n: 0 }).n },
      column('Typical for this car (median)', (r) => { const p = med.find((x) => Math.round(x.soc) === r.k); return p ? p.kw : null; }, 'power', { digits: 2 }),
    ]);
  } }, icon('download'), 'Download CSV');
  const headEl = h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'curve-title' }, 'Power curve', h('span', { class: 'label-note' }, 'charging power against battery %')), h('div', { class: 'block-actions' }, csvBtn));
  if (!pts.length) {
    return [headEl, h('p', { class: 'note curve-empty', cover: ['cd.chart.curve_points', 'cd.chart.curve_avg'] }, error ? ['The readings could not be loaded, so the curve cannot be drawn. ', h('button', { type: 'button', class: 'button button-quiet button-small', onclick: () => remount() }, icon('reset'), 'Try again')]
      : s ? 'No charging power was recorded, so there is no curve.' : 'No readings, so there is no curve.')];
  }

  // the window: the charge's own span, widened to tens and to at least 30 points of battery
  const lo0 = Math.min(...pts.map((p) => p.soc)), hi0 = Math.max(...pts.map((p) => p.soc));
  let xlo = Math.max(0, Math.floor((lo0 - 5) / 10) * 10), xhi = Math.min(100, Math.ceil((hi0 + 5) / 10) * 10);
  while (xhi - xlo < 30) { if (xlo > 0) xlo -= 10; if (xhi - xlo < 30 && xhi < 100) xhi += 10; if (xlo === 0 && xhi === 100) break; }
  const inX = (p) => p.soc >= xlo && p.soc <= xhi;
  const medIn = med.filter(inX);
  const ymax0 = Math.max(...pts.map((p) => p.kw), ...medIn.map((p) => p.kw), 1);
  const ystep = niceStep(ymax0, 4);
  const ymax = Math.ceil((ymax0 * 1.04) / ystep) * ystep;
  const X = (soc) => ((soc - xlo) / (xhi - xlo)) * 1000;
  const Y = (kw) => 600 - (kw / ymax) * 600;
  const line = (list) => list.map((p, i) => `${i ? 'L' : 'M'}${X(p.soc).toFixed(1)} ${Y(p.kw).toFixed(1)}`).join('');
  const stride = Math.max(1, Math.ceil(pts.length / 900));
  const dots = pts.filter((p, i) => i % stride === 0).map((p) => `M${X(p.soc).toFixed(1)} ${Y(p.kw).toFixed(1)}h0`).join('');
  const yTicks = []; for (let v = 0; v <= ymax + 1e-9; v += ystep) yTicks.push(v);
  const xTicks = []; for (let v = xlo; v <= xhi; v += 10) xTicks.push(v);

  const plot = svg('svg', { class: 'curve-svg', viewBox: '0 0 1000 600', preserveAspectRatio: 'none', 'aria-hidden': 'true' },
    yTicks.map((v) => svg('line', { class: 'curve-grid', x1: 0, x2: 1000, y1: Y(v), y2: Y(v) })),
    xTicks.map((v) => svg('line', { class: 'curve-grid', y1: 0, y2: 600, x1: X(v), x2: X(v) })),
    medIn.length > 1 ? svg('path', { class: 'curve-typical', d: line(medIn) }) : null,
    svg('path', { class: 'curve-dots', d: dots }),
    avg.length > 1 ? svg('path', { class: 'curve-avg', d: line(avg) }) : null,
    svg('line', { class: 'curve-cursor', x1: 0, x2: 0, y1: 0, y2: 600, visibility: 'hidden' }));
  const area = h('div', { class: 'curve-area' }, plot);
  const yAxis = h('div', { class: 'curve-y', 'aria-hidden': 'true' }, yTicks.map((v) => h('span', { vars: { '--y': `${((1 - v / ymax) * 100).toFixed(2)}%` } }, formatNumber(v))));
  const xAxis = h('div', { class: 'curve-x', 'aria-hidden': 'true' }, xTicks.map((v) => h('span', { vars: { '--x': `${(((v - xlo) / (xhi - xlo)) * 100).toFixed(2)}%` } }, `${v} %`)));
  const readout = h('p', { class: 'curve-readout', 'aria-live': 'off' });
  const idle = () => replace(readout, h('span', { class: 'readout-idle' }, `${formatNumber(pts.length)} readings from ${format(lo0, 'percent')} to ${format(hi0, 'percent')}; point at the chart to read the power at each battery %.`));
  idle();
  const cursor = plot.querySelector('.curve-cursor');
  area.addEventListener('pointermove', (e) => {
    const r = area.getBoundingClientRect();
    const soc = xlo + ((e.clientX - r.left) / r.width) * (xhi - xlo);
    const k = Math.floor(soc);
    const g = byPct.get(k);
    const t = medIn.find((p) => Math.round(p.soc) === k) || medIn.find((p) => Math.floor(p.soc) === k);
    cursor.setAttribute('x1', X(k + 0.5)); cursor.setAttribute('x2', X(k + 0.5)); cursor.setAttribute('visibility', 'visible');
    replace(readout, h('b', null, `${k} %`), sep(), 'this charge ', g ? q(g.sum / g.n, 'power') : 'did not pass here',
      dc ? [sep(), 'typical ', t ? q(t.kw, 'power') : '—'] : null);
  });
  area.addEventListener('pointerleave', () => { cursor.setAttribute('visibility', 'hidden'); idle(); });

  const key = h('ul', { class: 'curve-key' },
    h('li', null, h('span', { class: 'sw sw-dot', 'aria-hidden': 'true' }), 'each reading of this charge'),
    h('li', null, h('span', { class: 'sw sw-avg', 'aria-hidden': 'true' }), 'this charge, average at each battery %'),
    dc ? h('li', null, h('span', { class: 'sw sw-typical', 'aria-hidden': 'true' }), medIn.length > 1 ? `typical for this car: median of ${formatNumber(sessions)} fast charges in the year to this one` : 'typical for this car: not enough fast charges yet') : null);
  return [headEl,
    h('figure', { class: 'curve-fig', cover: ['cd.chart.curve_points', 'cd.chart.curve_avg'], 'aria-label': `Charging power against battery %, ${format(lo0, 'percent')} to ${format(hi0, 'percent')}` },
      h('div', { class: 'curve-frame' }, h('span', { class: 'curve-unit' }, unitLabel('power')), yAxis, area, xAxis),
      readout, key,
      h('figcaption', { class: 'note' }, dc
        ? 'A fast charger starts high and tapers as the battery fills; a cold pack charges slower. Dots are single readings, the line their average at each whole percent.'
        : 'AC charging is flat: the car draws what the station, cable or car allows until the battery nears its limit. The typical fast-charging curve is drawn for DC charges only.'))];
}

/** The selected part's figures from the readings in it (indexes into the curve). */
function selFigures(s, idx) {
  const V = s.values;
  const t = s.t_ms;
  const from = t[idx[0]], to = t[idx[idx.length - 1]] + (s.step_ms > 1000 ? s.step_ms : 0);
  let kwh = null, n = 0, peak = null;
  for (const i of idx) {
    const p = (V.power_kw || [])[i];
    if (p == null) continue;
    kwh = (kwh || 0) + p * (s.step_ms / 3600000);
    n++;
    if (peak == null || p > peak) peak = p;
  }
  const first = (f) => { for (const i of idx) if ((V[f] || [])[i] != null) return V[f][i]; return null; };
  const last = (f) => { for (let k = idx.length - 1; k >= 0; k--) if ((V[f] || [])[idx[k]] != null) return V[f][idx[k]]; return null; };
  return {
    from, to, kwh, peak, avg: n ? kwh / ((n * s.step_ms) / 3600000) : null,
    soc0: first('soc_pct'), soc1: last('soc_pct'), r0: first('range_km'), r1: last('range_km'),
  };
}

