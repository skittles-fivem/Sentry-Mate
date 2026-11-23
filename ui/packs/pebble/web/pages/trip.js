// Sentry Mate, "Pebble" UI pack: Trip (/cars/{car}/trip), any stretch of time as one trip.
//
// The date-range pill (and a "Last 3 drives" pill) picks the stretch; the car pill picks the car. Then the route
// map with its stops, a totals tile (distance, speeds, use, cost), where the time went, where the energy went, the
// activity strip, a battery and elevation chart, a tile per leg (each drive) and per charge, and the totals again
// at the end. Data: GET /api/v1/cars/{car}/trip, /drives, /charges, /tracks, /series, /activity over the stretch.

import { h } from '../shared/dom.js';
import { format, formatMoneyList, rangeLabel } from '../shared/units.js';
import { formatTime, formatDate } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { tile, num, chip, icon, note } from '../core/ui.js';
import { emptyTile } from '../core/states.js';
import { mapTile, boundsOf } from '../core/map.js';
import { timeChart, spansOf } from '../core/chart.js';
import { placeLink, byDay, dayHead, socBar, filterBar, strip, statTile, dur, facts } from '../core/bits.js';
import { rowTitle } from '../core/source.js';

export const id = 'trip';
export const css = true;
export const shape = [['full', 'h1'], ['full', 'h3'], [2, 'h2'], [2, 'h2']];
export const title = () => 'Trip';

const TIME = [['driving', 'Driving', 'c1'], ['charging_ac', 'Charging AC', 'c3'], ['charging_dc', 'Charging DC', 'c3'], ['parked_awake', 'Parked, awake', 'c2'], ['asleep', 'Asleep', 'c2'], ['not_recorded', 'No data', 'c2']];

function rankList(rows, total, { cover = null } = {}) {
  return h('ol', { class: 'rank', cover }, rows.map(([name, val, text, tone]) => {
    const fill = h('span', { class: 'rank-fill' });
    fill.style.setProperty('--v', total > 0 ? Math.max(0, Math.min(1, val / total)).toFixed(4) : '0');
    return h('li', { class: ['rank-row', `tone-${tone || 'c1'}`] }, h('span', { class: 'rank-name' }, name), h('span', { class: 'rank-n' }, text), h('span', { class: 'rank-bar', 'aria-hidden': 'true' }, fill));
  }));
}

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz, P = ctx.params;
  const keep = (p) => p.catch((err) => ({ error: err }));
  const [trip, drives, charges, tracks, series, act, last3] = await Promise.all([
    ctx.api.get(api.apiPath`cars/${car.id}/trip`, P),
    keep(ctx.api.list(api.apiPath`cars/${car.id}/drives`, { ...P, sort: 'start', dir: 'asc' }).all({ max: 200 })),
    keep(ctx.api.list(api.apiPath`cars/${car.id}/charges`, { ...P, sort: 'start', dir: 'asc' }).all({ max: 100 })),
    keep(ctx.api.get('tracks', { cars: car.id, ...P, max_points: 4000 })),
    keep(ctx.api.get(api.apiPath`cars/${car.id}/series`, { ...P, fields: 'soc_pct,range_km,ideal_range_km,elevation_m', max_points: 1200 })),
    keep(ctx.api.get(api.apiPath`cars/${car.id}/activity`, P)),
    keep(ctx.api.page(api.apiPath`cars/${car.id}/drives`, {}, { limit: 3 })),
  ]);
  if (!ctx.alive()) return {};
  const dl = drives.error ? [] : drives;
  const cl = charges.error ? [] : charges;

  const third = last3.error ? null : (last3.items || [])[2] || (last3.items || []).slice(-1)[0];
  const lastBtn = third ? h('button', { type: 'button', class: 'fpill fpill-ic', cover: 'trip.link.last_three_drives', title: `From ${formatDate(third.start_ms, tz, { now: api.now() })}, ${formatTime(third.start_ms, tz)} up to now`,
    onclick: () => state.setPeriod({ from: third.start_ms, to: api.now() }, { push: true }) }, icon('route'), 'Last 3 drives')
    : h('span', { class: 'fpill', cover: 'trip.link.last_three_drives' }, 'No drive yet');
  const bar = filterBar(ctx, { carCover: 'trip.control.per_car', periodCover: 'trip.filter.time_range' }, lastBtn);

  if (!dl.length && !cl.length && !(trip.distance?.odometer_km > 0)) {
    ctx.view.replaceChildren(bar, emptyTile({ icon: 'trip', title: 'Nothing happened in this stretch', text: 'Pick a longer stretch with the date pill, or start from the last three drives.', cover: ['trip.empty_states', 'trip.map.route', 'trip.control.zoom', 'trip.stat.mileage', 'trip.chart.time_spent', 'trip.stat.speed_moving', 'trip.stat.speed_incl_dc', 'trip.stat.consumption_net', 'trip.stat.consumption_gross', 'trip.stat.charging_cost', 'trip.stat.cost_per_100', 'trip.chart.energy', 'trip.chart.states', 'shots.trip', 'trip.drives.table', 'trip.charges.table', 'trip.chart.battery.soc', 'trip.chart.battery.range', 'trip.chart.elevation'] }));
    ctx.ready();
    return {};
  }

  /* ------------------------------------------------------------ map */
  const mapBox = h('div', { class: 'map map-tall' });
  const mapT = tile({ w: 'full', title: 'Where it went', icon: 'map', cls: 'map-tile', cover: ['trip.map.route', 'trip.control.zoom', 'shots.trip'] }, mapBox,
    h('div', { class: 'map-foot' }, h('span', null, `${formatNumber(dl.length)} drives, ${formatNumber(cl.length)} charges`), h('span', null, 'Teal dots are charges.')));

  /* ------------------------------------------------------------ totals */
  const cost = trip.cost || {};
  const totals = (cls) => tile({ w: 2, title: 'The trip in numbers', icon: 'trip', cls: ['totals-tile', cls] },
    h('div', { class: 'tot-nums' },
      h('div', { cover: 'trip.stat.mileage' }, h('span', { class: 'hero-k' }, 'Distance'), num(trip.distance?.odometer_km ?? trip.distance?.drives_km, 'distance', { size: 'l' })),
      h('div', { cover: 'trip.stat.charging_cost' }, h('span', { class: 'hero-k' }, 'Charging cost'), h('span', { class: 'tot-money' }, cost.by_currency?.length ? formatMoneyList(cost.by_currency) : 'none'))),
    facts([
      ['Moving speed', format(trip.speed_kph?.moving, 'speed'), 'trip.stat.speed_moving'],
      ['Speed with fast charging', format(trip.speed_kph?.incl_dc, 'speed'), 'trip.stat.speed_incl_dc'],
      ['Driving use', format(trip.wh_per_km?.net, 'efficiency'), 'trip.stat.consumption_net'],
      ['Use with parked time', format(trip.wh_per_km?.gross, 'efficiency'), 'trip.stat.consumption_gross'],
      ['Cost per 100 km', cost.per_100km?.length ? formatMoneyList(cost.per_100km) : 'no priced charge', 'trip.stat.cost_per_100'],
      ['Charges without a price', cost.unpriced ? formatNumber(cost.unpriced) : 'none'],
    ]));
  const tm = trip.time_ms || {};
  const totalMs = Object.values(tm).reduce((a, b) => a + (b || 0), 0);
  const timeT = tile({ w: 2, title: 'Where the time went', icon: 'clock', cover: 'trip.chart.time_spent' },
    rankList(TIME.filter(([k]) => tm[k]).map(([k, name, tone]) => [name, tm[k], dur(tm[k]), tone]), totalMs));
  const en = trip.energy_kwh || {};
  const enMax = Math.max(en.gross || 0, (en.added_ac || 0) + (en.added_dc || 0), 0.001);
  const energyT = tile({ w: 2, title: 'Where the energy went', icon: 'bolt', cover: 'trip.chart.energy' },
    rankList([
      ['Used driving', en.drive_net || 0, format(en.drive_net, 'energy'), 'c1'],
      ['Used in all', en.gross || 0, format(en.gross, 'energy'), 'c2'],
      ['Added AC', en.added_ac || 0, format(en.added_ac, 'energy'), 'c3'],
      ['Added DC', en.added_dc || 0, format(en.added_dc, 'energy'), 'c3'],
    ], enMax), note('Used in all adds what the car used while parked.'));
  const stripT = tile({ w: 2, title: 'What it did', icon: 'timeline', cls: 'strip-tile', cover: 'trip.chart.states' },
    act.error ? note('The activity did not load.') : strip(act, { tz, link: (s) => (s.ref?.drive ? state.buildUrl('drive', { car: car.id, drive: s.ref.drive }) : s.ref?.charge ? state.buildUrl('charge', { car: car.id, charge: s.ref.charge }) : null) }));
  const chartT = tile({ w: 'full', title: 'Battery and elevation', icon: 'mountain', cls: 'chart-tile', cover: ['trip.chart.battery.soc', 'trip.chart.battery.range', 'trip.chart.elevation'] });

  /* ------------------------------------------------------------ legs and charges */
  // where a leg's data came from is its card's hover title (the drive and charge pages show it in full)
  const titled = (t, r) => { const why = rowTitle(r); if (why) t.title = why; return t; };
  const legTile = (d) => titled(tile({ w: 2, cls: ['item', 'drive-tile'], cover: 'trip.drives.table' },
    h('div', { class: 'item-top' },
      h('span', { class: 'item-ic tone-coral', 'aria-hidden': 'true' }, icon('route')),
      h('div', { class: 'item-text' },
        h('p', { class: 'item-title route-words' }, h('span', { cover: ['trip.drives.start', 'trip.drives.link.geofence_start'] }, placeLink(d.start)), icon('arrowRight', 'route-arrow'),
          h('span', { cover: ['trip.drives.destination', 'trip.drives.link.geofence_end'] }, d.end_ms ? placeLink(d.end) : 'now')),
        h('p', { class: 'item-sub', cover: 'trip.drives.date' }, `${formatTime(d.start_ms, tz)} – ${d.end_ms ? formatTime(d.end_ms, tz) : 'now'}`))),
    h('div', { class: 'item-nums' },
      h('span', { cover: 'trip.drives.distance' }, num(d.distance_km, 'distance', { size: 's' })),
      h('span', { cover: ['trip.drives.duration', 'trip.drives.link.duration_hint'], title: `${formatNumber(Math.round((d.duration_ms || 0) / 60000))} minutes` }, num(d.duration_ms, 'duration', { size: 's' })),
      h('span', { cover: 'trip.drives.consumption' }, num(d.wh_per_km, 'efficiency', { size: 's' }))),
    socBar(d.start_soc_pct, d.end_soc_pct, { cover: ['trip.drives.soc_start', 'trip.drives.soc_end'] }),
    h('p', { class: 'item-links tile-foot' }, h('a', { href: state.buildUrl('drive', { car: car.id, drive: d.id }), cover: 'trip.drives.link.details' }, 'Details'))), d);
  const chargeTile = (c) => titled(tile({ w: 2, cls: ['item', 'charge-item'], cover: 'trip.charges.table' },
    h('div', { class: 'item-top' },
      h('span', { class: 'item-ic tone-teal', 'aria-hidden': 'true' }, icon('bolt')),
      h('div', { class: 'item-text' },
        h('p', { class: 'item-title', cover: ['trip.charges.location', 'trip.charges.link.geofence'] }, placeLink({ place: c.place, address: c.address, lat: c.position?.lat, lon: c.position?.lon })),
        h('p', { class: 'item-sub', cover: 'trip.charges.date' }, `${formatTime(c.start_ms, tz)} – ${c.end_ms ? formatTime(c.end_ms, tz) : 'now'}`)),
      c.kind ? chip(c.kind.toUpperCase(), { tone: c.kind === 'dc' ? 'sky' : 'teal' }) : null),
    h('div', { class: 'item-nums' },
      h('span', { cover: 'trip.charges.energy_added' }, num(c.energy_added_kwh, 'energy', { size: 's' })),
      h('span', { cover: 'trip.charges.duration' }, num(c.charging_ms, 'duration', { size: 's' })),
      h('span', { cover: 'trip.charges.power_avg' }, num(c.power_avg_kw, 'power', { size: 's' })),
      h('span', { cover: 'trip.charges.range_added' }, `+${format(c.range_added_km, 'range')}`)),
    socBar(c.start_soc_pct, c.end_soc_pct, { tone: 'teal', cover: ['trip.charges.soc_start', 'trip.charges.soc_end'] }),
    h('p', { class: 'item-sub' },
      h('span', { cover: 'trip.charges.energy_used' }, `Drawn ${c.energy_grid_kwh != null ? format(c.energy_grid_kwh, 'energy') : 'not known'}`), ' · ',
      h('span', { cover: 'trip.charges.cost', title: c.cost ? null : 'No price' }, c.cost ? format(c.cost.amount, 'money', { currency: c.cost.currency }) : '—')),
    h('p', { class: 'item-links tile-foot' },
      h('a', { href: state.buildUrl('charge', { car: car.id, charge: c.id }), cover: 'trip.charges.link.details' }, 'Details'),
      h('a', { class: 'row-act', href: state.buildUrl('charge', { car: car.id, charge: c.id, query: { edit: 'cost' } }), cover: 'trip.charges.link.cost' }, c.cost ? 'Change the cost' : 'Add a cost'))), c);

  // legs and charges in time order, grouped by day
  const items = [...dl.map((d) => ({ k: 'd', t: d.start_ms, x: d })), ...cl.map((c) => ({ k: 'c', t: c.start_ms, x: c }))].sort((a, b) => a.t - b.t);
  const legs = [h('div', { class: 'section-head w-full' }, h('h2', { class: 'section-title' }, 'Legs and charges'),
    h('p', { class: 'note', cover: 'trip.empty_states' }, `${formatNumber(dl.length)} drives and ${formatNumber(cl.length)} charges, oldest first.`))];
  if (!dl.length) legs.push(h('p', { class: 'note w-full', cover: ['trip.drives.table', 'trip.empty_states'] }, 'No drive in this stretch.'));
  if (!cl.length) legs.push(h('p', { class: 'note w-full', cover: ['trip.charges.table', 'trip.empty_states'] }, 'No charge in this stretch.'));
  for (const g of byDay(items, tz, (i) => i.t)) {
    legs.push(dayHead(g.key, tz));
    for (const i of g.items) legs.push(i.k === 'd' ? legTile(i.x) : chargeTile(i.x));
  }

  ctx.view.replaceChildren(bar, mapT, totals(), timeT, energyT, stripT, chartT, ...legs, totals('is-end'));

  /* map */
  const lines = tracks.error ? [] : (tracks.drives || []).map((t) => t.lon.map((x, i) => [x, t.lat[i]]));
  const all = lines.flat();
  let m = null;
  if (all.length > 1) {
    m = await mapTile(ctx, mapBox, { bounds: boundsOf(all), padding: 40 });
    if (!m) return {};
    if (m.map) {
      m.routes('legs', lines, 'c1');
      for (const mk of trip.markers || []) {
        if (mk.lat == null) continue;
        m.pin([mk.lon, mk.lat], { tone: mk.kind === 'charge' ? 'teal' : mk.kind === 'start' ? 'sky' : mk.kind === 'end' ? 'coral' : 'muted', title: `${mk.kind === 'overnight' ? 'Night stop' : mk.kind[0].toUpperCase() + mk.kind.slice(1)}, ${formatDate(mk.t_ms, tz, { now: api.now() })} ${formatTime(mk.t_ms, tz)}` });
      }
    }
  } else mapBox.append(h('p', { class: 'map-note' }, 'No route was recorded in this stretch.'));

  /* chart */
  let chart = null;
  if (series.error || !series.t_ms?.length) chartT.append(note('No battery values in this stretch.'));
  else {
    const V = series.values;
    chart = timeChart(ctx, chartT, { tz, t_ms: series.t_ms, states: series.state, spans: spansOf(series.t_ms, series.state, series.bucket_ms), label: 'Battery and elevation', series: [
      { key: 'soc', name: 'Battery', kind: 'percent', values: V.soc_pct, tone: 'c1', fill: true, floor: 0 },
      { key: 'elev', name: 'Elevation', kind: 'length', values: V.elevation_m, tone: 'c2', dashed: true },
    ] });
    const rv = rangeLabel() === 'Ideal range' ? V.ideal_range_km : V.range_km;
    const first = (rv || []).find((x) => x != null), last = [...(rv || [])].reverse().find((x) => x != null);
    chartT.append(note(`${rangeLabel()} went from ${format(first, 'range')} to ${format(last, 'range')}.`));
  }
  await Promise.all([chart, m?.ready]);
  if (!ctx.alive()) return {};
  ctx.ready();
  return {};
}

