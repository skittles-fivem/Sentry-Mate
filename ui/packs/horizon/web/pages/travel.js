// Travel (/travel): the world tour. Where the cars went and how long they stayed, for one car, several or
// all of them together. The tour counters (places, cities, regions, countries, days out, first and last
// visit, distance, energy, efficiency, cost), the map with every drive and a dot per city or place, the
// cities and regions ranked like a chart show, the countries, the cities (with their own column filters),
// the days in one city, the places visited and the recent stops (GET /visits/summary, /visits/countries,
// /visits/regions, /visits/cities, /visits/places, /visits/days, /visits, /tracks).

import * as state from '../shared/state.js';
import { h, replace, cssVar } from '../shared/dom.js';
import { formatTime, formatDate, formatDateTime, formatDuration, dayRange } from '../shared/time.js';
import { formatNumber, compareText } from '../shared/i18n.js';
import { column } from '../shared/csv.js';
import { icon, hud, plate, btn, seg } from '../core/ui.js';
import { controls, slot, select, search, periodBtn, csvBtn, stat, rack, plural, band } from '../core/kit.js';
import { srcChip, sourceLabel } from '../core/sources.js';
import { tracks } from '../core/tracks.js';
import { createMap, addTrace, boundsOf, pad, mapFallback } from '../core/map.js';
import { rpx } from '../core/display.js';
import { emptyState } from '../core/states.js';
import { money } from '../core/fmt.js';

export const id = 'travel';
export const css = true;

const RANKS = [['days', 'Days'], ['visits', 'Stops'], ['places', 'Places'], ['dwell_ms', 'Time']];
const CAR_TONES = ['--mag', '--cyan', '--org', '--grn', '--yel', '--violet'];
const iso = (ms) => (ms == null ? '' : new Date(ms).toISOString());

export async function mount(ctx) {
  const q = ctx.query;
  const f = { q: q.q || '', country: (q.country || '').toUpperCase(), region: q.region || '' };
  let city = q.city || null;

  let fleet = [];
  try { fleet = (await ctx.api.page('cars', { sort: 'name' }, { limit: 200 })).items || []; } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  const carById = new Map(fleet.map((c) => [c.id, c]));
  const chosen = !q.cars || q.cars === 'all' ? 'all' : q.cars.split(',').filter((c) => carById.has(c));
  const viewCars = chosen === 'all' ? fleet.map((c) => c.id) : chosen;
  const carsParam = chosen === 'all' ? 'all' : viewCars.join(',');
  const carName = (cid) => carById.get(cid)?.name || cid;
  const tzOf = (r) => r.time_zone || carById.get(r.car || r.cars?.[0])?.time_zone || ctx.tz;
  const toneOf = new Map(viewCars.map((cid, i) => [cid, CAR_TONES[i % CAR_TONES.length]]));
  const fileBase = chosen === 'all' ? 'all-cars' : viewCars.map(carName).join('-');
  const params = (patch = {}) => {
    const p = { cars: carsParam, from_ms: ctx.params.from_ms, to_ms: ctx.params.to_ms, q: f.q || null, country: f.country || null, region: f.region || null, ...patch };
    for (const k of Object.keys(p)) if (p[k] == null || p[k] === '') delete p[k];
    return p;
  };
  const go = (patch) => { ctx.setQuery(patch); ctx.reload(); };

  /* ---------- which cars */
  const carChips = h('div', { class: 'chips', role: 'group', 'aria-label': 'Cars on this page', cover: ['locations.filter.car', 'visited.filter.car'] },
    h('button', { class: 'chip', type: 'button', 'aria-pressed': String(chosen === 'all'), onclick: () => go({ cars: null }) }, `Every car (${fleet.length})`),
    fleet.map((c) => {
      const on = chosen !== 'all' && chosen.includes(c.id);
      return h('button', { class: 'chip', type: 'button', 'aria-pressed': String(on), title: on ? `Leave ${c.name} out` : `Add ${c.name}`,
        onclick: () => { const next = on ? chosen.filter((x) => x !== c.id) : [...(chosen === 'all' ? [] : chosen), c.id]; go({ cars: next.length ? next.join(',') : null }); } },
      h('i', { class: 'chip-dot', vars: { '--c': `var(${toneOf.get(c.id) || '--ink-3'})` } }), c.name || c.id);
    }));

  if (!viewCars.length) {
    replace(ctx.view, h('div', { class: 'page tv' }, plate({ kicker: 'Reports', title: 'Travel', aim: ctx.info.aim }),
      hud({ pad: false }, emptyState({ icon: 'globe', title: 'There is no car yet', text: 'Travel lists where the cars have been once a CAN Bridge has sent its first logs.' }))));
    ctx.ready();
    return;
  }

  async function all(path, p) {
    const out = []; let after = null;
    do { const pg = await ctx.api.page(path, p, { limit: 500, after: after ?? undefined }); out.push(...(pg.items || [])); after = pg.next; } while (after && out.length < 5000);
    return out;
  }
  let s, countries, regions, cities, placesG;
  try {
    [s, countries, regions, cities, placesG] = await Promise.all([
      ctx.api.get('visits/summary', params()), all('visits/countries', params()), all('visits/regions', params()), all('visits/cities', params()), all('visits/places', params()),
    ]);
  } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  const byDays = (a, b) => (b.days - a.days) || (b.visits - a.visits) || compareText(a.label || '', b.label || '');
  cities.sort(byDays);
  if (city && !cities.some((c) => c.key === city)) city = null;
  const picked = city || cities[0]?.key || null;
  const pickedCity = cities.find((c) => c.key === picked);
  const sumTz = viewCars.length === 1 ? (carById.get(viewCars[0])?.time_zone || ctx.tz) : ctx.tz;
  const pickCity = (key) => go({ city: key });

  /* ---------- the tour counters */
  const summary = hud({ title: 'The tour', icon: 'globe', tone: 'cyan', side: csvBtn({ name: [fileBase, 'travel-summary'], rows: () => [s],
    columns: [column('Places', 'places'), column('Cities', 'cities'), column('Regions', 'regions'), column('Countries', 'countries'), column('Days', 'days'), column('Distance', 'distance_km', 'distance'), column('Energy added', 'energy_added_kwh', 'energy')] }) },
  rack([
    stat('Places', formatNumber(s.places ?? 0), null, { cover: 'locations.stat.addresses', how: 'saved places, plus spots about 150 m apart' }),
    stat('Cities', formatNumber(s.cities ?? 0), null, { cover: ['locations.stat.cities', 'visited.stat.cities'], how: 'by name, region and country' }),
    stat('Regions', formatNumber(s.regions ?? 0), null, { cover: 'locations.stat.states', how: 'states, provinces or regions' }),
    stat('Countries', formatNumber(s.countries ?? 0), null, { cover: ['locations.stat.countries', 'visited.stat.countries'], how: 'from the offline address of each stop' }),
    stat('Days out', formatNumber(s.days ?? 0), null, { cover: 'visited.stat.days', how: "days with a stop, in each car's zone; once per car" }),
    stat('First visit', s.first_ms != null ? formatDate(s.first_ms, sumTz, { year: true }) : null, null, { cover: 'visited.stat.first_visit', why: 'no stop in this period', how: s.first_ms != null ? `arrived ${formatTime(s.first_ms, sumTz)}${viewCars.length > 1 && s.first_car ? `, ${carName(s.first_car)}` : ''}` : null }),
    stat('Last visit', s.last_ms != null ? formatDate(s.last_ms, sumTz, { year: true }) : null, null, { cover: 'visited.stat.last_visit', why: 'no stop in this period', how: s.last_ms != null ? `${formatTime(s.last_ms, sumTz)}, or still there` : null }),
    stat('Distance', s.distance_km, 'distance', { cover: 'visited.stat.distance', how: 'drives that started in the period' }),
    stat('Energy added', s.energy_added_kwh, 'energy', { cover: 'visited.stat.energy_added', how: 'into the battery' }),
    stat('Energy drawn', s.energy_grid_kwh, 'energy', { cover: 'visited.stat.energy_drawn', how: 'from the chargers that measured it' }),
    stat('Charging efficiency', s.efficiency?.value ?? null, 'percent', { digits: 1, cover: 'visited.stat.charging_efficiency', why: 'no charge measured both sides', how: s.efficiency?.charges_counted ? `added ÷ drawn, over ${plural(s.efficiency.charges_counted, 'charge')}` : null }),
    stat('Charging cost', (s.cost_by_currency || []).length ? money(s.cost_by_currency) : null, null, { cover: 'visited.stat.charging_cost', hot: true, why: 'no priced charge', how: `per currency, never added together${s.charges_without_cost ? `; ${plural(s.charges_without_cost, 'charge')} without a price` : ''}` }),
  ]),
  h('p', { class: 'note', cover: 'visited.text.shared_visits_help' }, icon('info', 'i--sm'), h('span', null, viewCars.length > 1
    ? 'With several cars a place visited by two cars counts once among the places, but each car\'s days and stops count on their own.'
    : 'Stops are from arriving to leaving; a stop that runs past the end of the period is cut there.')));

  /* ---------- the map */
  let markMode = q.marks === 'places' ? 'places' : 'cities';
  const mapEl = h('div', { class: 'map map--lg tv-map', role: 'img', 'aria-label': 'Map of the drives and the cities visited' });
  const card = h('div', { class: 'map-card tv-card', hidden: true, cover: 'visited.map.marker_details', 'aria-live': 'polite' });
  const marks = seg([['cities', 'Cities'], ['places', 'Places']], markMode, (v) => { markMode = v; ctx.setQuery({ marks: v === 'cities' ? null : v }); paintMarks(); }, { label: 'Dots for' });
  const routeRead = h('span', { class: 'trk-count' });

  /* ---------- rankings */
  let rankBy = RANKS.some(([k]) => k === q.rank) ? q.rank : 'days';
  const rankSlot = h('div', { class: 'cols' });
  const rankText = (r) => (rankBy === 'dwell_ms' ? formatDuration(r.dwell_ms) : formatNumber(r[rankBy]));
  const ranking = (title, rows, cv, onPick) => {
    const top = [...rows].sort((a, b) => (b[rankBy] - a[rankBy]) || compareText(a.label || '', b.label || '')).slice(0, 10);
    const max = Math.max(1, ...top.map((r) => r[rankBy] || 0));
    return h('div', { class: 'tv-rank', cover: cv }, h('p', { class: 'scope-title' }, title),
      top.length ? h('ol', { class: 'tv-chart' }, top.map((r, i) => {
        const share = (r[rankBy] || 0) / max;
        return h('li', { class: 'tv-pos', dataset: { tier: share >= 2 / 3 ? '1' : share >= 1 / 3 ? '2' : '3' } },
          h('span', { class: 'tv-pos-no' }, String(i + 1).padStart(2, '0')),
          h('button', { class: 'tv-pos-name', type: 'button', onclick: () => onPick(r), title: `Pick ${r.label}` }, r.label || '—', r.country_code ? h('span', { class: 'faint' }, ` ${r.country_code}`) : null),
          h('span', { class: 'eq-bar', 'aria-hidden': 'true' }, h('i', { vars: { '--w': `${Math.max(2, share * 100).toFixed(1)}%` } })),
          h('span', { class: 'eq-val' }, rankText(r)));
      })) : h('p', { class: 'empty-line' }, icon('info', 'i--sm'), h('span', null, 'Nothing to rank in this period.')));
  };
  const paintRanks = () => replace(rankSlot,
    ranking('Top cities', cities, ['locations.chart.top_cities', 'locations.chart.rank_shading', 'visited.link.city_days'], (r) => pickCity(r.key)),
    ranking('Top regions', regions, 'locations.chart.top_regions', (r) => go({ region: r.region || null, country: r.country_code || null })));
  paintRanks();
  const rankSeg = seg(RANKS.map(([k, l]) => [k, l]), rankBy, (v) => { rankBy = v; ctx.setQuery({ rank: v === 'days' ? null : v }); paintRanks(); }, { label: 'Rank by' });

  /* ---------- tables */
  const dateCols = (a, b) => [
    { key: 'first_ms', label: 'First', num: true, sort: true, cover: a, render: (r) => formatDate(r.first_ms, tzOf(r), { year: true, weekday: false }), csv: (r) => iso(r.first_ms) },
    { key: 'last_ms', label: 'Last', num: true, sort: true, cover: b, render: (r) => formatDate(r.last_ms, tzOf(r), { year: true, weekday: false }), csv: (r) => iso(r.last_ms) },
  ];
  const countryT = tracks(ctx, {
    title: 'Countries', icon: 'flag', rows: countries, csvName: 'travel-countries', cover: 'visited.table.countries', dense: true, sort: { key: 'days', dir: 'desc' },
    columns: [
      { key: 'label', label: 'Country', sort: true, lead: true, cover: 'visited.countries.country', render: (r) => h('button', { class: 'tv-link', type: 'button', onclick: () => go({ country: r.country_code, region: null }), title: `Show only ${r.label}` }, r.label) },
      { key: 'cities', label: 'Cities', num: true, sort: true, value: (r) => cities.filter((c) => c.country_code === r.country_code).length, cover: 'visited.countries.cities', render: (r) => formatNumber(cities.filter((c) => c.country_code === r.country_code).length) },
      { key: 'days', label: 'Days', num: true, sort: true, cover: 'visited.countries.days', render: (r) => formatNumber(r.days) },
      ...dateCols('visited.countries.first_visit', 'visited.countries.last_visit'),
    ],
    empty: () => emptyState({ icon: 'flag', title: 'No country', text: 'No stop in this period.' }),
  });
  const tf = { country: q.c_country || '', region: q.c_region || '', find: '' };
  const cityRows = () => cities.filter((c) => (!tf.country || c.country_code === tf.country) && (!tf.region || c.region === tf.region) && (!tf.find || (c.label || '').toLowerCase().includes(tf.find)));
  const cCountry = select([['', 'Any country'], ...[...new Map(cities.filter((c) => c.country_code).map((c) => [c.country_code, c.country || c.country_code])).entries()]], tf.country,
    (v) => { tf.country = v; tf.region = ''; cRegion.value = ''; cityT.setRows(cityRows()); }, { label: 'Cities in country' });
  const cRegion = select([['', 'Any region'], ...[...new Set(cities.map((c) => c.region).filter(Boolean))].sort(compareText).map((r) => [r, r])], tf.region,
    (v) => { tf.region = v; cityT.setRows(cityRows()); }, { label: 'Cities in region' });
  const cFind = search('', (v) => { tf.find = v.toLowerCase(); cityT.setRows(cityRows()); }, { placeholder: 'Find a city', ctx });
  const cityT = tracks(ctx, {
    title: 'Cities', icon: 'pin', rows: cityRows(), csvName: 'travel-cities', cover: 'visited.table.cities', dense: true, sort: { key: 'days', dir: 'desc' },
    side: h('span', { class: 'tv-colf', cover: 'visited.cities.column_filter' }, cFind, cCountry, cRegion),
    rowCls: (r) => (r.key === picked ? 'is-picked' : null),
    columns: [
      { key: 'label', label: 'City', sort: true, lead: true, cover: 'visited.cities.city', render: (r) => h('button', { class: 'tv-link', type: 'button', onclick: () => pickCity(r.key), title: `List the days in ${r.label}` }, r.label) },
      { key: 'region', label: 'Region', sort: true, cover: 'visited.cities.region', render: (r) => r.region || '—' },
      { key: 'country', label: 'Country', sort: true, cover: 'visited.cities.country', render: (r) => r.country || r.country_code || '—' },
      { key: 'days', label: 'Days', num: true, sort: true, cover: 'visited.cities.days', render: (r) => formatNumber(r.days) },
      ...dateCols('visited.cities.first_visit', 'visited.cities.last_visit'),
    ],
    empty: () => emptyState({ icon: 'pin', title: 'No city matches', text: 'Change the column filters.' }),
  });

  const daysT = picked ? tracks(ctx, {
    title: `Days in ${pickedCity?.label || 'a city'}`, icon: 'calendar', path: 'visits/days', params: params({ city: picked }), limit: 15, csvName: 'travel-city-days', cover: 'visited.table.city_days', dense: true,
    side: h('span', { class: 'tag tag--mag', cover: 'visited.state.selected_city' }, pickedCity?.label || picked),
    note: 'Each day a car was in the city: when it arrived and left, its drives, its charges and the places it stopped at. Pick another city in the rankings or the city list.', noteCover: 'visited.text.city_days_help',
    columns: [
      { key: 'date', label: 'Date', lead: true, cover: ['visited.city_days.date', 'visited.link.day_drives'], csv: (r) => r.date,
        render: (r) => h('span', { class: 'cell' }, h('a', { class: 'cell-main', href: state.buildUrl('timeline', { car: r.car, period: dayRange(r.date, r.date, tzOf(r)) }), title: 'That day on the timeline' }, formatDate(Date.parse(`${r.date}T12:00:00Z`), 'UTC', { year: true })), viewCars.length > 1 ? h('span', { class: 'cell-sub' }, carName(r.car)) : null) },
      { key: 'first_ms', label: 'First seen', num: true, cover: 'visited.city_days.first_seen', render: (r) => formatTime(r.first_ms, tzOf(r)), csv: (r) => iso(r.first_ms) },
      { key: 'last_ms', label: 'Last seen', num: true, cover: 'visited.city_days.last_seen', render: (r) => formatTime(r.last_ms, tzOf(r)), csv: (r) => iso(r.last_ms) },
      { key: 'drives', label: 'Drives', num: true, cover: 'visited.city_days.drives', csv: (r) => r.drives.length,
        render: (r) => (r.drives.length ? h('a', { href: state.buildUrl('drives', { car: r.car, period: dayRange(r.date, r.date, tzOf(r)) }) }, formatNumber(r.drives.length)) : '0') },
      { key: 'charges', label: 'Charges', num: true, cover: 'visited.city_days.charges', csv: (r) => r.charges.length,
        render: (r) => (r.charges.length ? h('a', { href: state.buildUrl('charges', { car: r.car, period: dayRange(r.date, r.date, tzOf(r)) }) }, formatNumber(r.charges.length)) : '0') },
      { key: 'places', label: 'Places', cls: 'is-wide', cover: 'visited.city_days.places', render: (r) => r.places.join(', ') || '—', csv: (r) => r.places.join('; ') },
    ],
    empty: () => emptyState({ icon: 'calendar', title: 'No day there', text: 'No car was in this city in the period.' }),
  }) : null;

  const placeCell = (r) => h('span', { class: 'cell' },
    r.place ? h('a', { class: 'plc', href: state.buildUrl('places', { query: { place: r.place.id } }), cover: 'locations.link.edit_geofence', title: 'Open this place in the place editor' }, r.place.name)
      : h('span', { class: 'plc plc--bare' }, r.label || 'Unnamed spot'),
    !r.place && r.lat != null ? h('a', { class: 'cell-sub', href: state.buildUrl('places', { query: { lat: r.lat.toFixed(5), lon: r.lon.toFixed(5), new: 1 } }), cover: 'locations.link.new_geofence' }, '+ Save as a place') : (r.street ? h('span', { class: 'cell-sub' }, r.street) : null));
  const placeT = tracks(ctx, {
    title: 'Places visited', icon: 'home', rows: placesG, csvName: 'travel-places', cover: ['locations.table.addresses', 'locations.table.geofences'], dense: true, sort: { key: 'days', dir: 'desc' },
    columns: [
      { key: 'label', label: 'Place', sort: true, lead: true, cover: ['locations.addresses.name', 'locations.geofences.name', 'locations.link.new_geofence', 'locations.link.edit_geofence'], render: placeCell, csv: (r) => r.label },
      { key: 'neighbourhood', label: 'Neighbourhood', sort: true, cover: 'locations.addresses.neighbourhood', render: (r) => r.neighbourhood || '—' },
      { key: 'locality', label: 'City', sort: true, cover: 'locations.addresses.city', render: (r) => r.locality || '—' },
      { key: 'region', label: 'Region', sort: true, cover: 'locations.addresses.state', render: (r) => r.region || '—' },
      { key: 'country', label: 'Country', sort: true, cover: 'locations.addresses.country', render: (r) => r.country || r.country_code || '—' },
      { key: 'days', label: 'Days', num: true, sort: true, render: (r) => formatNumber(r.days) },
      { key: 'dwell_ms', label: 'Time there', num: true, sort: true, cover: 'locations.geofences.time', render: (r) => formatDuration(r.dwell_ms), csv: (r) => Math.round(r.dwell_ms / 60000) },
      { key: 'last_ms', label: 'Last there', num: true, sort: true, cover: 'locations.addresses.updated_at', render: (r) => formatDate(r.last_ms, tzOf(r), { year: true, weekday: false }), csv: (r) => iso(r.last_ms) },
    ],
    empty: () => emptyState({ icon: 'home', title: 'No place', text: 'No stop in this period.' }),
  });
  const stopsT = tracks(ctx, {
    title: 'Recent stops', icon: 'clock', path: 'visits', params: params({ sort: 'arrived_desc' }), limit: 15, csvName: 'travel-recent-stops', cover: 'locations.table.last_visited', dense: true,
    columns: [
      { key: 'arrived_ms', label: 'Arrived', lead: true, cover: 'locations.last_visited.date', render: (r) => h('span', { class: 'cell' }, h('span', { class: 'cell-main' }, formatDateTime(r.arrived_ms, tzOf(r))), viewCars.length > 1 ? h('span', { class: 'cell-sub' }, carName(r.car)) : null), csv: (r) => iso(r.arrived_ms) },
      { key: 'label', label: 'Place', cover: 'locations.last_visited.address', render: (r) => placeCell({ ...r, street: r.address?.short }), csv: (r) => r.label },
      { key: 'city', label: 'City', cover: 'locations.last_visited.city', render: (r) => r.address?.locality || '—', csv: (r) => r.address?.locality },
      { key: 'stay', label: 'Stayed', num: true, render: (r) => (r.left_ms ? formatDuration(r.left_ms - r.arrived_ms) : 'still there'), csv: (r) => (r.left_ms ? Math.round((r.left_ms - r.arrived_ms) / 60000) : '') },
      { key: 'src', label: 'Source', render: (r) => srcChip(r), csv: (r) => sourceLabel(r) },
    ],
    empty: () => emptyState({ icon: 'clock', title: 'No stop', text: 'No stop in this period.' }),
  });

  replace(ctx.view, h('div', { class: 'page tv', cover: ['shots.locations', 'shots.visited'] },
    plate({ kicker: 'Reports · the world tour', title: 'Travel', aim: ctx.info.aim,
      tools: btn('Refresh', () => ctx.reload(), { icon: 'arrow-r', sm: true, kind: 'ghost', cover: ['locations.control.refresh', 'visited.control.refresh'] }) }),
    controls({ label: 'What the page covers', note: 'The text keeps stops whose saved name, street, neighbourhood, town, region or country contains it.' },
      slot('Period', periodBtn(ctx, ['locations.filter.time_range', 'visited.filter.time_range'])),
      slot('Cars', carChips, { cover: 'visited.section.car_header' }),
      slot('Country', select([['', 'Every country'], ...countries.map((c) => [c.country_code, c.label])], f.country, (v) => go({ country: v || null, region: null }), { label: 'Country' })),
      slot('Text', search(f.q, (v) => go({ q: v || null }), { placeholder: 'Place, street or town', ctx }), { wide: true, cover: 'locations.filter.address' }),
      f.region ? slot('Region', btn(`${f.region} ×`, () => go({ region: null }), { sm: true, kind: 'ghost' })) : null),
    summary,
    hud({ title: 'Map', icon: 'globe', pad: false, cover: ['visited.map', 'visited.map.base_map'], side: [routeRead, marks.el] },
      h('div', { class: 'tv-map-box', cover: ['visited.map.route', 'visited.map.city_markers'] }, mapEl, card),
      h('p', { class: 'note tv-map-help', cover: 'visited.text.map_help' }, icon('info', 'i--sm'), h('span', null, 'Lines are the drives, a colour per car. A dot is a city or a place: the bigger, the more days there. Point at a dot for its figures; click a city to list its days.'))),
    band(2, 'Cities and regions', rankSeg.el),
    hud({ title: 'Ranked', icon: 'stats', side: csvBtn({ name: [fileBase, 'travel-ranking'], rows: () => [...cities].sort((a, b) => b[rankBy] - a[rankBy]), columns: [column('City', 'label'), column('Region', 'region'), column('Country', 'country'), column('Stops', 'visits'), column('Places', 'places'), column('Days', 'days')] }) },
      rankSlot, h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, 'The top ten, the figure printed on each bar. A bar is bright from two thirds of the top one, softer from a third and faint below that.'))),
    countryT.el, h('div', null, daysT ? daysT.el : hud({ title: 'Days in a city', icon: 'calendar', cover: ['visited.table.city_days', 'visited.text.city_days_help', 'visited.state.selected_city'] }, emptyState({ icon: 'calendar', title: 'No city yet', text: 'There is no stop in this period.' }))),
    cityT.el, placeT.el, stopsT.el));

  // the drives for the map are asked for with the lists: the map head's count is in before the page is
  // ready, so the head does not grow (and push the page down) a moment later
  const tracksIn = (async () => {
    for (const max of [40000, 200000]) {
      try { return await ctx.api.get('tracks', { cars: carsParam, from_ms: ctx.params.from_ms, to_ms: ctx.params.to_ms, max_points: max }); } catch (e) { if (e.status !== 400) break; }
    }
    return null;
  })();
  let tr = null;
  try { [, , tr] = await Promise.all([stopsT.ready, daysT?.ready, tracksIn]); } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  const lines = (tr?.drives || []).filter((d) => d.lat.length > 1).map((d) => ({ type: 'Feature', properties: { car: d.car }, geometry: { type: 'LineString', coordinates: d.lat.map((la, i) => [d.lon[i], la]) } }));
  const trSrc = [...new Map((tr?.drives || []).flatMap((d) => d.sources || []).map((x) => [`${x.kind}:${x.site || ''}`, x])).values()];
  replace(routeRead, tr ? `${plural(lines.length, 'drive')} · ${formatNumber(tr.points || 0)} positions ` : 'Too many drives for one map: pick a shorter period or fewer cars.', trSrc.length ? srcChip({ sources: trSrc }) : null);
  ctx.ready();

  /* ---------- map, after the first paint */
  let m = null;
  const markRows = () => (markMode === 'cities' ? cities : placesG).filter((r) => r.lat != null && r.lon != null);
  function paintMarks() {
    card.hidden = true;
    if (!m) return;
    const rows = markRows();
    const max = Math.max(1, ...rows.map((r) => r.days || 0));
    m.map.getSource('tv-marks')?.setData({ type: 'FeatureCollection', features: rows.map((r, i) => ({ type: 'Feature', properties: { i, r: 4 + 12 * Math.sqrt((r.days || 0) / max), picked: r.key === picked ? 1 : 0 }, geometry: { type: 'Point', coordinates: [r.lon, r.lat] } })) });
  }
  const pts = [...lines.flatMap((l) => [l.geometry.coordinates[0], l.geometry.coordinates[l.geometry.coordinates.length - 1]]), ...cities.filter((r) => r.lat != null).map((r) => [r.lon, r.lat])];
  if (!pts.length) { mapEl.append(mapFallback('No drive and no stop in this period, so there is nothing to map.')); return; }
  try { m = await createMap(mapEl, ctx, { bounds: boundsOf(pts) }); } catch { replace(mapEl, mapFallback()); return; }
  if (!m) return;
  await m.ready;
  if (!ctx.alive()) return;
  for (const cid of viewCars) {
    const fs = lines.filter((l) => l.properties.car === cid);
    if (fs.length) addTrace(m, `tv-${cid}`, { type: 'FeatureCollection', features: fs }, { color: toneOf.get(cid), width: 2.2, glow: viewCars.length < 4 });
  }
  m.map.addSource('tv-marks', { type: 'geojson', data: { type: 'FeatureCollection', features: [] } });
  m.addLayer({ id: 'tv-marks', type: 'circle', source: 'tv-marks', paint: { 'circle-color': ['case', ['==', ['get', 'picked'], 1], cssVar('--mag'), cssVar('--org')], 'circle-opacity': 0.75, 'circle-stroke-color': '#ffffff' } },
    { 'circle-radius': () => ['*', ['get', 'r'], rpx(1)], 'circle-stroke-width': () => rpx(1.5) });
  paintMarks();
  m.map.on('mousemove', 'tv-marks', (e) => {
    const r = markRows()[e.features?.[0]?.properties?.i];
    if (!r) return;
    card.hidden = false;
    replace(card, h('b', null, r.label || 'Unnamed spot'), h('span', { class: 'faint' }, ` · ${[r.region, r.country_code].filter(Boolean).join(', ')}`),
      h('br'), `${plural(r.days, 'day')} · ${plural(r.visits, 'stop')} · ${formatDuration(r.dwell_ms)} there`);
  });
  m.map.on('mouseleave', 'tv-marks', () => { card.hidden = true; });
  m.map.on('click', 'tv-marks', (e) => {
    const r = markRows()[e.features?.[0]?.properties?.i];
    if (!r) return;
    if (markMode === 'cities') pickCity(r.key);
    else ctx.navigate(r.place ? state.buildUrl('places', { query: { place: r.place.id } }) : state.buildUrl('places', { query: { lat: r.lat.toFixed(5), lon: r.lon.toFixed(5), new: 1 } }));
  });
  m.frame(() => { if (pts.length === 1) m.map.jumpTo({ center: pts[0], zoom: 12 }); else m.map.fitBounds(boundsOf(pts), { padding: pad(40), duration: 0, maxZoom: 13 }); });
}
