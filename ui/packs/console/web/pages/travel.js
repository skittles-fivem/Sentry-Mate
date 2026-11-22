// Travel (/travel): where the cars have been, for one car or many. A cars line and a filter line
// (text, country:, region:), the summary (GET /visits/summary), one flat grey map with the routes and a
// square per city (bigger with more days; a click picks the city), the top cities and regions as text
// bars, then tables: countries, cities (a click picks the city), days in the picked city, places visited
// and the newest stops. Lists come from /visits/{countries,regions,cities,places,days} and /visits.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { formatTime, formatDuration, dayRange } from '../shared/time.js';
import { formatNumber, compareText } from '../shared/i18n.js';
import { head, section, kv } from '../core/ui.js';
import { carKeys as soldAwareKeys } from '../core/carpick.js';
import { num, date, dateTime } from '../core/fmt.js';
import { table, cellBar } from '../core/table.js';
import { safeMap } from '../core/map.js';
import { empty } from '../core/states.js';
import { filterLine } from '../core/filterline.js';
import { src, srcText } from '../core/source.js';

export const id = 'travel';

const CAP = 2000;
const KEYS = { country: 'a country (name or code)', region: 'a state or region', place: 'a saved place' };

export async function mount(ctx) {
  const { period } = ctx;
  const q = ctx.query;
  let fleet = [];
  try { fleet = (await ctx.api.page('cars', { sort: 'name' }, { limit: 200 })).items || []; } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  const carById = new Map(fleet.map((c) => [c.id, c]));
  const chosen = !q.cars || q.cars === 'all' ? 'all' : q.cars.split(',').filter((c) => carById.has(c));
  const viewCars = chosen === 'all' ? fleet.map((c) => c.id) : chosen;
  const carsParam = chosen === 'all' ? 'all' : viewCars.join(',');
  const multi = viewCars.length > 1;
  const carName = (cid) => carById.get(cid)?.name || cid;
  const tzOf = (r) => r?.time_zone || carById.get(r?.car)?.time_zone || ctx.tz;
  let city = q.city || null;

  const hd = head(ctx, { title: 'Travel' });
  const pb = hd.querySelector('.ph-btn');
  if (pb) pb.dataset.cover = 'locations.filter.time_range visited.filter.time_range';
  const carKeys = h('span', { class: 'keys', role: 'group', 'aria-label': 'Cars', cover: ['locations.filter.car', 'visited.filter.car', 'visited.section.car_header'] },
    h('a', { class: 'key', href: state.buildUrl('travel', { period: state.current().period }), 'aria-pressed': String(chosen === 'all') }, 'all cars'),
    soldAwareKeys(fleet, (c) => h('a', { class: 'key', 'aria-pressed': String(chosen !== 'all' && viewCars.length === 1 && viewCars[0] === c.id),
      href: state.buildUrl('travel', { period: state.current().period, query: { cars: c.id } }) }, c.name), { keep: chosen === 'all' ? null : viewCars }));
  if (!viewCars.length) {
    replace(ctx.view, h('div', { class: 'page', cover: ['shots.locations', 'shots.visited'] }, hd, h('div', { class: 'optline' }, carKeys),
      empty(fleet.length ? 'none of these cars is yours to see' : 'no car yet', fleet.length ? 'pick other cars above' : 'travel lists where the cars have been once a CAN Bridge has sent logs')));
    ctx.ready();
    return;
  }

  const fl = filterLine({
    keys: KEYS, value: q.f || '', placeholder: 'a street or town, country:US region:California', label: 'Filter the places', cover: 'locations.filter.address',
    onApply: (p, raw) => { ctx.setQuery({ f: raw || null, city: null }); ctx.reload(); },
  });
  const fp = fl.parsed();
  const params = (patch = {}) => {
    const p = { cars: carsParam, from_ms: ctx.params.from_ms, to_ms: ctx.params.to_ms, q: fp.text || null, country: fp.keys.country || null, region: fp.keys.region || null, ...patch };
    for (const k of Object.keys(p)) if (p[k] == null || p[k] === '') delete p[k];
    return p;
  };
  async function all(path, p) {
    const out = [];
    let after = null;
    do {
      const pg = await ctx.api.page(path, p, { limit: 500, after: after ?? undefined });
      out.push(...(pg.items || []));
      after = pg.next;
    } while (after && out.length < CAP);
    return out;
  }

  let summary, countries, regions, cities, placesV, saved;
  try {
    [summary, countries, regions, cities, placesV, saved] = await Promise.all([
      ctx.api.get('visits/summary', params()),
      all('visits/countries', params()), all('visits/regions', params()), all('visits/cities', params()), all('visits/places', params()),
      ctx.api.page('places', { sort: 'name' }, { limit: 200 }).then((x) => x.items || []).catch(() => []),
    ]);
  } catch (e) { if (ctx.alive()) ctx.fail(e); return; }
  if (!ctx.alive()) return;
  if (fp.keys.place) {
    const n = fp.keys.place.toLowerCase();
    placesV = placesV.filter((p) => (p.label || '').toLowerCase().includes(n));
  }
  const byDays = (a, b) => (b.days - a.days) || (b.visits - a.visits) || compareText(a.label || '', b.label || '');
  cities.sort(byDays); regions.sort(byDays); countries.sort(byDays);
  if (city && !cities.some((c) => c.key === city)) city = null;
  city ||= cities[0]?.key || null;
  const cityRow = () => cities.find((c) => c.key === city);
  const s = summary || {};
  const span = s.first_ms ? `${date(s.first_ms, ctx.tz)} - ${date(s.last_ms, ctx.tz)}` : null;
  const money = (list) => (list?.length ? list.map((x) => format(x.amount, 'money', { currency: x.currency })).join(' + ') : null);
  const dayOf = (r) => dayRange(r.date, r.date, tzOf(r));

  /* ---------- summary */
  const summaryEl = section({ title: 'summary', side: h('button', { class: 'btn btn--line tone--accent', type: 'button', cover: ['locations.control.refresh', 'visited.control.refresh'], onclick: () => { ctx.api.invalidate('visits'); ctx.reload(); } }, '[refresh]') },
    kv([
      ['places', [formatNumber(s.places ?? 0), s.unnamed_stops ? h('span', { class: 'muted' }, ` + ${formatNumber(s.unnamed_stops)} unnamed stops`) : null], { cover: 'locations.stat.addresses' }],
      ['cities', formatNumber(s.cities ?? 0), { cover: ['locations.stat.cities', 'visited.stat.cities'] }],
      ['states', formatNumber(s.regions ?? 0), { cover: 'locations.stat.states' }],
      ['countries', formatNumber(s.countries ?? 0), { cover: ['locations.stat.countries', 'visited.stat.countries'] }],
      ['distance', num(s.distance_km, 'distance'), { cover: 'visited.stat.distance' }],
      ['charging cost', money(s.cost_by_currency) || h('span', { class: 'muted' }, 'no price yet'), { cover: 'visited.stat.charging_cost' }],
      ['energy added', num(s.energy_added_kwh, 'energy'), { cover: 'visited.stat.energy_added' }],
      ['energy drawn', num(s.energy_grid_kwh, 'energy'), { cover: 'visited.stat.energy_drawn' }],
      ['charging eff.', s.efficiency?.value != null ? [num(s.efficiency.value, 'percent'), h('span', { class: 'muted' }, ` over ${formatNumber(s.efficiency.charges_counted)} charges`)] : null, { cover: 'visited.stat.charging_efficiency' }],
      ['days', formatNumber(s.days ?? 0), { cover: 'visited.stat.days' }],
      ['first visit', s.first_ms ? [dateTime(s.first_ms, ctx.tz), multi && s.first_car ? h('span', { class: 'muted' }, ` ${carName(s.first_car)}`) : null] : null, { cover: 'visited.stat.first_visit' }],
      ['last visit', s.last_ms ? [dateTime(s.last_ms, ctx.tz), multi && s.last_car ? h('span', { class: 'muted' }, ` ${carName(s.last_car)}`) : null] : null, { cover: 'visited.stat.last_visit' }],
    ]),
    h('p', { class: 'note', cover: 'visited.text.shared_visits_help' }, multi ? 'Places, cities and countries count once however many of these cars went there; days count once per day. ' : '', 'A day counts when a car was in a place at any time of it, in that car’s own time zone.'));

  /* ---------- map */
  const mapEl = h('div', { class: 'map map--lg', role: 'img', 'aria-label': 'Map of the cities visited' });
  const mapRead = h('p', { class: 'chart-read', cover: ['visited.map.marker_details', 'visited.state.selected_city'] });
  const paintRead = () => { const c = cityRow(); replace(mapRead, c ? [h('span', { class: 'muted' }, 'city '), h('b', null, c.label), h('span', { class: 'muted' }, `  ${[c.region, c.country].filter(Boolean).join(', ')}  ${formatNumber(c.days)} days  ${formatNumber(c.visits)} visits  ${formatNumber(c.places)} places`)] : h('span', { class: 'muted' }, '-- no city in this range --')); };
  paintRead();

  /* ---------- top cities and regions */
  const topBars = (list, label, cover) => {
    const top = list.slice(0, 10);
    const max = Math.max(1, ...top.map((x) => x.days));
    return h('div', { cover }, h('h3', { class: 'sub-title' }, label),
      top.length ? h('div', { class: 'bars' }, top.map((x, i) => h('div', { class: ['bar-row', i < 3 && 'bar-row--top'] },
        label.includes('cit') ? h('button', { class: 'btn btn--line bar-k', type: 'button', onclick: () => pickCity(x.key) }, `${i + 1}. ${x.label}`) : h('span', { class: 'bar-k' }, `${i + 1}. ${x.label}`),
        cellBar(x.days / max, 20), h('span', { class: 'bar-v' }, `${formatNumber(x.days)} d`)))) : empty('nothing in this range'));
  };

  /* ---------- tables */
  const countryT = table(ctx, {
    title: 'countries', rows: countries, csv: false, sort: { key: 'days', dir: 'desc' }, cover: 'visited.table.countries',
    columns: [
      { key: 'label', label: 'country', sort: true, firstDir: 'asc', ph: 'main', cover: 'visited.countries.country', render: (r) => r.label || r.country_code },
      { key: 'cities', label: 'cities', num: true, sort: true, value: (r) => cities.filter((c) => c.country_code === r.country_code).length, cover: 'visited.countries.cities', render: (r) => formatNumber(cities.filter((c) => c.country_code === r.country_code).length) },
      { key: 'days', label: 'days', num: true, sort: true, ph: 'num', cover: 'visited.countries.days', render: (r) => formatNumber(r.days) },
      { key: 'first_ms', label: 'first', sort: true, ph: 'sub', cover: 'visited.countries.first_visit', render: (r) => date(r.first_ms, ctx.tz) },
      { key: 'last_ms', label: 'last', sort: true, ph: 'sub', cover: 'visited.countries.last_visit', render: (r) => date(r.last_ms, ctx.tz) },
    ],
    empty: () => empty('no country in this range'),
  });
  const cityFilter = h('input', { class: 'in in--cell', type: 'search', placeholder: 'find a city', 'aria-label': 'Find a city in the list', cover: 'visited.cities.column_filter' });
  const cityT = table(ctx, {
    title: 'cities', rows: cities, csv: false, sort: { key: 'days', dir: 'desc' }, cover: ['visited.table.cities', 'visited.link.city_days'], side: cityFilter,
    rowHref: null,
    columns: [
      { key: 'label', label: 'city', sort: true, firstDir: 'asc', ph: 'main', cover: 'visited.cities.city', render: (r) => h('button', { class: ['btn btn--line', r.key === city && 'tone--accent'], type: 'button', title: 'list the days in this city', onclick: () => pickCity(r.key) }, r.label) },
      { key: 'region', label: 'region', sort: true, ph: 'sub', cover: 'visited.cities.region', render: (r) => r.region },
      { key: 'country', label: 'country', sort: true, ph: 'sub', cover: 'visited.cities.country', render: (r) => r.country || r.country_code },
      { key: 'days', label: 'days', num: true, sort: true, ph: 'num', cover: 'visited.cities.days', render: (r) => formatNumber(r.days) },
      { key: 'first_ms', label: 'first', sort: true, cover: 'visited.cities.first_visit', render: (r) => date(r.first_ms, ctx.tz) },
      { key: 'last_ms', label: 'last', sort: true, cover: 'visited.cities.last_visit', render: (r) => date(r.last_ms, ctx.tz) },
    ],
    empty: () => empty('no city in this range'),
  });
  let ct = 0;
  cityFilter.addEventListener('input', () => { clearTimeout(ct); ct = setTimeout(() => { const n = cityFilter.value.trim().toLowerCase(); cityT.setRows(n ? cities.filter((c) => `${c.label} ${c.region} ${c.country}`.toLowerCase().includes(n)) : cities); }, 200); });
  ctx.onCleanup(() => clearTimeout(ct));

  const daysSlot = h('div');
  function paintDays() {
    const c = cityRow();
    if (!c) { replace(daysSlot, section({ title: 'days in a city', cover: ['visited.table.city_days', 'visited.text.city_days_help'] }, empty('no city to list in this range'))); return; }
    const t = table(ctx, {
      title: `days in ${c.label}`, path: 'visits/days', params: params({ city }), limit: 20, csvName: `travel-days-${c.label}`,
      cover: 'visited.table.city_days', note: `One row per day and car, newest first, in that car’s own time zone. Pick another city on the map or in the lists.`, noteCover: 'visited.text.city_days_help',
      columns: [
        { key: 'date', label: 'date', ph: 'main', cover: ['visited.city_days.date', 'visited.link.day_drives'], render: (r) => h('a', { class: 'lnk-plain', href: state.buildUrl('drives', { car: r.car, period: dayOf(r) }) }, date(r.first_ms, tzOf(r), { year: true })) },
        ...(multi ? [{ key: 'car', label: 'car', ph: 'sub', render: (r) => carName(r.car) }] : []),
        { key: 'first', label: 'first seen', num: true, cover: 'visited.city_days.first_seen', render: (r) => (r.first_ms === dayOf(r).from ? h('span', { class: 'muted' }, 'start of day') : formatTime(r.first_ms, tzOf(r))) },
        { key: 'last', label: 'last seen', num: true, cover: 'visited.city_days.last_seen', render: (r) => (r.last_ms != null && r.last_ms >= dayOf(r).to ? h('span', { class: 'muted' }, 'end of day') : r.last_ms == null ? 'still there' : formatTime(r.last_ms, tzOf(r))) },
        { key: 'drives', label: 'drives', num: true, ph: 'num', cover: 'visited.city_days.drives', render: (r) => (r.drives.length ? h('a', { class: 'lnk-plain', href: state.buildUrl('drives', { car: r.car, period: dayOf(r) }) }, formatNumber(r.drives.length)) : '0') },
        { key: 'charges', label: 'charges', num: true, cover: 'visited.city_days.charges', render: (r) => (r.charges.length ? h('a', { class: 'lnk-plain', href: state.buildUrl('charges', { car: r.car, period: dayOf(r) }) }, formatNumber(r.charges.length)) : '0') },
        { key: 'places', label: 'places', cls: 'wrap-cell', ph: 'sub', cover: 'visited.city_days.places', render: (r) => r.places.join(', ') },
      ],
      empty: () => empty('no day in this city in this range'),
    });
    replace(daysSlot, t.el);
    return t.ready;
  }
  function pickCity(key) {
    city = key;
    ctx.setQuery({ city: key });
    paintRead(); paintDays();
    cityT.repaint();
    pinCities?.();
  }

  const savedById = new Map(saved.map((p) => [p.id, p]));
  const placesT = table(ctx, {
    title: 'places visited', rows: placesV, csv: false, sort: { key: 'last_ms', dir: 'desc' }, cover: ['locations.table.addresses', 'locations.table.geofences'],
    columns: [
      { key: 'label', label: 'name', sort: true, firstDir: 'asc', ph: 'main', cover: ['locations.addresses.name', 'locations.geofences.name', 'locations.link.edit_geofence', 'locations.link.new_geofence'],
        render: (r) => (r.place ? h('a', { class: 'lnk-plain', href: state.buildUrl('places', { query: { place: r.place.id } }), title: 'edit the place' }, r.label)
          : [h('span', null, r.label || r.street || '—'), ' ', r.lat != null ? h('a', { class: 'lnk', href: state.buildUrl('places', { query: { lat: r.lat.toFixed(5), lon: r.lon.toFixed(5), new: 1 } }) }, '+place') : null]) },
      { key: 'neighbourhood', label: 'area', sort: true, ph: 'sub', cover: 'locations.addresses.neighbourhood', render: (r) => r.neighbourhood },
      { key: 'locality', label: 'city', sort: true, ph: 'sub', cover: 'locations.addresses.city', render: (r) => r.locality },
      { key: 'region', label: 'state', sort: true, cover: 'locations.addresses.state', render: (r) => r.region },
      { key: 'country', label: 'country', sort: true, cover: 'locations.addresses.country', render: (r) => r.country_code },
      { key: 'days', label: 'days', num: true, sort: true, ph: 'num', render: (r) => formatNumber(r.days) },
      { key: 'dwell_ms', label: 'time there', num: true, sort: true, cover: 'locations.geofences.time', render: (r) => formatDuration(r.dwell_ms) },
      { key: 'last_ms', label: 'last', sort: true, cover: 'locations.addresses.updated_at', render: (r) => date(r.last_ms, ctx.tz) },
      { key: 'saved', label: 'saved', render: (r) => (r.place && savedById.has(r.place.id) ? h('span', { class: 'tag tag--ok' }, '[place]') : h('span', { class: 'muted' }, 'address')) },
    ],
    empty: () => empty('no place in this range'),
  });
  const stopsT = table(ctx, {
    title: 'newest stops', path: 'visits', params: params(), limit: 15, csvName: 'travel-stops', cover: 'locations.table.last_visited',
    columns: [
      { key: 'arrived', label: 'date', ph: 'main', cover: 'locations.last_visited.date', render: (r) => `${date(r.arrived_ms, tzOf(r))} ${formatTime(r.arrived_ms, tzOf(r))}` },
      ...(multi ? [{ key: 'car', label: 'car', ph: 'sub', render: (r) => carName(r.car) }] : []),
      { key: 'address', label: 'address', cls: 'wrap-cell', ph: 'sub', cover: 'locations.last_visited.address', render: (r) => (r.place ? h('a', { class: 'lnk-plain', href: state.buildUrl('places', { query: { place: r.place.id } }) }, r.label) : r.address?.short || r.label) },
      { key: 'city', label: 'city', ph: 'sub', cover: 'locations.last_visited.city', render: (r) => r.address?.locality },
      { key: 'stay', label: 'stayed', num: true, ph: 'num', render: (r) => (r.left_ms ? formatDuration(r.left_ms - r.arrived_ms) : h('span', { class: 'tone--accent' }, 'still there')) },
      { key: 'from', label: 'data', title: 'where the data came from: smo, api (the Tesla API), imp (imported)', render: (r) => src(r), value: (r) => srcText(r) },
    ],
    empty: () => empty('no stop in this range'),
  });

  replace(ctx.view, h('div', { class: 'page', dataset: { page: 'travel' }, cover: ['shots.locations', 'shots.visited'] },
    hd,
    h('div', { class: 'optline' }, h('span', { class: 'optline-k' }, 'cars:'), carKeys),
    fl.el,
    summaryEl,
    section({ title: 'map', cover: ['visited.map', 'visited.map.base_map', 'visited.map.route', 'visited.map.city_markers'], side: span ? h('span', { class: 'muted' }, span) : null },
      mapEl, mapRead, h('p', { class: 'note', cover: 'visited.text.map_help' }, 'Lines: the drives in the range. Dots: cities, bigger with more days; a click on one lists its days below.')),
    h('div', { class: 'cols2', cover: 'locations.chart.rank_shading' }, topBars(cities, 'top cities by days', 'locations.chart.top_cities'), topBars(regions, 'top states and regions by days', 'locations.chart.top_regions')),
    h('div', { class: 'split' }, countryT.el, cityT.el),
    daysSlot, placesT.el, stopsT.el));

  let pinCities = null;
  try { await Promise.all([paintDays(), stopsT.ready]); } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;

  // the map: routes, then a square per city
  const pts = cities.filter((c) => c.lat != null);
  let tracks = null;
  try { tracks = await ctx.api.get('tracks', { ...params(), max_points: 20000 }); } catch { tracks = null; }
  if (!ctx.alive()) return;
  const lines = (tracks?.drives || []).map((d) => d.lon.map((lo, i) => [lo, d.lat[i]]).filter((p) => p[0] != null && p[1] != null)).filter((c) => c.length > 1);
  const every = [...pts.map((c) => [c.lon, c.lat]), ...lines.flat()];
  if (!every.length) mapEl.append(h('p', { class: 'map-note' }, '-- no position in this range --'));
  else {
    const m = await safeMap(mapEl, ctx, { center: every[0], zoom: 6 });
    if (m && ctx.alive()) {
      if (lines.length) m.line('routes', lines, { width: 2, token: '--c2' });
      const maxD = Math.max(1, ...pts.map((c) => c.days));
      m.points('cities', pts.map((c) => ({ lon: c.lon, lat: c.lat, props: { key: c.key, days: c.days } })), { size: 14, sizeBy: { prop: 'days', min: 1, max: maxD } });
      let sel = null;
      pinCities = () => { sel?.remove(); const c = cityRow(); if (c?.lat != null) sel = m.marker([c.lon, c.lat], 'map-pin map-pin--sel'); };
      pinCities();
      m.map.on('click', 'cities', (e) => { const k = e.features?.[0]?.properties?.key; if (k) pickCity(k); });
      m.map.on('mouseenter', 'cities', () => { m.map.getCanvas().style.cursor = 'pointer'; });
      m.map.on('mouseleave', 'cities', () => { m.map.getCanvas().style.cursor = ''; });
      m.fit(every, { pad: 32, maxZoom: 12 });
    }
  }
  if (!ctx.alive()) return;
  ctx.ready();
}
