// Sentry Mate, "Pebble" UI pack: Travel (/travel), where the cars have been.
//
// Car pills (every car, or some) and a search; count tiles (places, cities, regions, countries, days, first and
// last visit, distance, energy and cost); a map tile with every route and a dot per city (tap a dot or a city to
// pick it); tiles for countries, cities (with region pills), regions, the days spent in the picked city, the
// places visited (named places and plain addresses) and the latest stops. Data: GET /api/v1/visits/summary,
// /visits (paged), /visits/places, /visits/cities, /visits/regions, /visits/countries, /visits/days, /tracks, /cars.

import { h } from '../shared/dom.js';
import { format, formatMoneyList } from '../shared/units.js';
import { formatDate, formatDayKey, formatTime } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { tile, chip, btn, icon, note } from '../core/ui.js';
import { emptyTile } from '../core/states.js';
import { mapTile, boundsOf } from '../core/map.js';
import { filterBar, placeHref, statTile, dur } from '../core/bits.js';
import { srcPill } from '../core/source.js';

export const id = 'travel';
export const css = true;
export const shape = [['full', 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1'], ['full', 'h3']];
export const title = () => 'Travel';

/** A list row inside a tile: name and a line under it on the left, figures on the right. */
function row({ name, sub = null, side = null, href = null, onclick = null, current = false, cover = null, shade = null }) {
  const tag = href ? 'a' : onclick ? 'button' : 'div';
  const el = h(tag, { class: ['trow', { 'is-current': current, 'is-act': !!(href || onclick) }], href, type: onclick ? 'button' : null, onclick, cover, 'aria-pressed': onclick ? String(current) : null },
    h('span', { class: 'trow-text' }, h('span', { class: 'trow-name' }, name), sub ? h('span', { class: 'trow-sub' }, sub) : null),
    side ? h('span', { class: 'trow-side' }, side) : null);
  if (shade != null) { el.classList.add('is-shaded'); el.style.setProperty('--shade', shade.toFixed(4)); }
  return h('li', null, el);
}
const list = (rows, cover = null) => h('ul', { class: 'tlist', cover }, rows);

export async function mount(ctx) {
  const tz = Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
  const cars = await ctx.api.list('cars', { sort: 'custom' }).all({ max: 50 }).catch(() => []);
  const Q = ctx.query;
  const chosen = new Set(String(Q.cars || '').split(',').filter((x) => cars.some((c) => c.id === x)));
  let qtext = Q.q || '';
  let city = Q.city || null;
  let region = Q.region || '';
  const carName = (id2) => cars.find((c) => c.id === id2)?.name || id2;
  const zoneOf = (id2) => cars.find((c) => c.id === id2)?.time_zone || tz;
  const base = () => ({ ...ctx.params, cars: chosen.size ? [...chosen].join(',') : undefined, q: qtext || undefined });
  const write = () => ctx.setQuery({ cars: chosen.size ? [...chosen].join(',') : null, q: qtext || null, city, region: region || null });

  /* ------------------------------------------------------------ controls */
  const carPills = h('div', { class: 'pills', role: 'group', 'aria-label': 'Cars', cover: ['locations.filter.car', 'visited.filter.car'] },
    h('button', { type: 'button', class: 'fpill', 'aria-pressed': String(!chosen.size), onclick: () => { chosen.clear(); syncCars(); write(); load(); } }, 'Every car'),
    cars.map((c) => h('button', { type: 'button', class: 'fpill', dataset: { car: c.id }, 'aria-pressed': String(chosen.has(c.id)), onclick: () => { if (chosen.has(c.id)) chosen.delete(c.id); else chosen.add(c.id); syncCars(); write(); load(); } }, c.name)));
  function syncCars() {
    for (const b of carPills.children) b.setAttribute('aria-pressed', String(b.dataset.car ? chosen.has(b.dataset.car) : !chosen.size));
    carHead.textContent = chosen.size ? [...chosen].map(carName).join(', ') : `Every car (${formatNumber(cars.length)})`;
  }
  const search = h('input', { type: 'search', value: qtext, placeholder: 'Place, street or city', 'aria-label': 'Find a place, street or city', autocomplete: 'off' });
  let timer = 0;
  search.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(() => { qtext = search.value.trim(); write(); load(); }, 300); });
  ctx.onCleanup(() => clearTimeout(timer));
  const refresh = btn('Refresh', { kind: 'soft', icon: 'refresh', iconOnly: true, cover: ['locations.control.refresh', 'visited.control.refresh'], onclick: () => load() });
  ctx.head({ actions: [refresh] });
  const carHead = h('span', { class: 'filter-note', cover: 'visited.section.car_header' });
  const bar = filterBar(ctx, { periodCover: ['locations.filter.time_range', 'visited.filter.time_range'] }, carPills, h('label', { class: 'search-pill', cover: 'locations.filter.address' }, icon('search'), search), carHead);
  syncCars();

  /* ------------------------------------------------------------ boxes filled by load() */
  const sumBox = h('div', { class: 'contents' });
  const mapBox = h('div', { class: 'map map-tall' });
  const mapNote = h('p', { class: 'note', cover: 'visited.map.marker_details', 'aria-live': 'polite' }, 'Tap a city dot to see the days spent there.');
  const mapT = tile({ w: 'full', title: 'Where the cars went', icon: 'globe', cls: 'map-tile', cover: ['visited.map', 'visited.map.base_map', 'shots.visited', 'shots.locations'] }, mapBox,
    h('div', { class: 'map-foot' },
      h('span', { class: 'tv-key', cover: 'visited.map.route' }, h('span', { class: 'tv-line', 'aria-hidden': 'true' }), 'Drives'),
      h('span', { class: 'tv-key', cover: 'visited.map.city_markers' }, h('span', { class: 'tv-dot', 'aria-hidden': 'true' }), 'Cities'),
      h('span', { cover: 'visited.text.map_help' }, 'Drag to move, use + and − to zoom.')), h('div', { class: 'map-foot' }, mapNote));
  const countriesT = tile({ w: 2, title: 'Countries', icon: 'flag', cover: 'visited.table.countries' });
  const citiesT = tile({ w: 2, title: 'Cities', icon: 'pin', cover: ['visited.table.cities', 'locations.chart.top_cities', 'locations.chart.rank_shading'] });
  const regionsT = tile({ w: 2, title: 'Regions', icon: 'map', cover: 'locations.chart.top_regions' });
  const daysT = tile({ w: 2, title: 'Days in a city', icon: 'calendar', cover: 'visited.table.city_days' });
  const placesT = tile({ w: 2, title: 'Places visited', icon: 'pin', cover: ['locations.table.addresses', 'locations.table.geofences'] });
  const stopsT = tile({ w: 2, title: 'Latest stops', icon: 'park', cover: 'locations.table.last_visited' });
  let map = null, mapReady = null;

  async function load() {
    const p = base();
    const keep = (x) => x.catch(() => null);
    const [sum, countries, cities, regions, places, stops, tracks] = await Promise.all([
      ctx.api.get('visits/summary', p),
      keep(ctx.api.page('visits/countries', p, { limit: 30 })),
      keep(ctx.api.page('visits/cities', p, { limit: 60 })),
      keep(ctx.api.page('visits/regions', p, { limit: 12 })),
      keep(ctx.api.page('visits/places', p, { limit: 12 })),
      keep(ctx.api.page('visits', { ...p, sort: 'arrived_desc' }, { limit: 8 })),
      keep(ctx.api.get('tracks', { ...p, max_points: 6000 })),
    ]);
    if (!ctx.alive()) return;
    const money = sum.cost_by_currency?.length ? formatMoneyList(sum.cost_by_currency) : 'none';
    sumBox.replaceChildren(
      statTile({ label: 'Places', value: sum.places, kind: 'count', icon: 'pin', cover: 'locations.stat.addresses', sub: `${formatNumber(sum.visits)} stops${sum.unnamed_stops ? `, ${formatNumber(sum.unnamed_stops)} unnamed` : ''}` }),
      statTile({ label: 'Cities', value: sum.cities, kind: 'count', icon: 'pin', cover: ['locations.stat.cities', 'visited.stat.cities'], sub: `${formatNumber(sum.regions)} regions` }),
      statTile({ label: 'Countries', value: sum.countries, kind: 'count', icon: 'flag', cover: ['locations.stat.countries', 'visited.stat.countries', 'locations.stat.states'], sub: `${formatNumber(sum.regions)} states and regions` }),
      statTile({ label: 'Days away and home', value: sum.days, kind: 'count', icon: 'calendar', cover: ['visited.stat.days', 'visited.stat.first_visit', 'visited.stat.last_visit'], sub: sum.first_ms ? `${formatDate(sum.first_ms, tz, { now: api.now() })} to ${formatDate(sum.last_ms, tz, { now: api.now() })}` : 'no stops' }),
      statTile({ label: 'Distance', value: sum.distance_km, kind: 'distance', icon: 'route', cover: 'visited.stat.distance' }),
      statTile({ label: 'Energy added', value: sum.energy_added_kwh, kind: 'energy', icon: 'bolt', cover: ['visited.stat.energy_added', 'visited.stat.energy_drawn'], sub: `${format(sum.energy_grid_kwh, 'energy')} drawn from chargers` }),
      statTile({ label: 'Charging efficiency', value: sum.efficiency?.value, kind: 'percent', icon: 'leaf', cover: 'visited.stat.charging_efficiency', why: 'no charge with both figures', sub: sum.efficiency?.charges_counted ? `from ${formatNumber(sum.efficiency.charges_counted)} charges` : null }),
      tile({ w: 1, title: 'Charging cost', icon: 'coin', cls: 'tile-stat', cover: 'visited.stat.charging_cost' }, h('p', { class: 'tile-big' }, money),
        h('p', { class: 'tile-line' }, sum.charges_without_cost ? `${formatNumber(sum.charges_without_cost)} charges without a price` : 'every charge priced')),
      h('p', { class: 'note w-full tv-help', cover: 'visited.text.shared_visits_help' }, icon('info'), 'When two cars were in the same place, the place and its days count once; each car’s stops are in the lists below.'));

    // countries
    const cl = countries?.items || [];
    countriesT.replaceChildren(countriesT.firstChild, cl.length ? list(cl.map((c) => row({
      name: h('span', { cover: 'visited.countries.country' }, c.label),
      sub: [h('span', { cover: 'visited.countries.first_visit' }, `first ${formatDate(c.first_ms, tz, { now: api.now() })}`), ', ', h('span', { cover: 'visited.countries.last_visit' }, `last ${formatDate(c.last_ms, tz, { now: api.now() })}`)],
      side: [h('span', { cover: 'visited.countries.cities' }, `${formatNumber(c.places)} places`), h('span', { cover: 'visited.countries.days' }, `${formatNumber(c.days)} days`)],
    }))) : note('No country in this period.'));

    // cities, with region pills
    const allCities = cities?.items || [];
    const regionsOf = [...new Set(allCities.map((c) => c.region).filter(Boolean))];
    if (region && !regionsOf.includes(region)) region = '';
    const shown = allCities.filter((c) => !region || c.region === region);
    const top = Math.max(1, ...shown.map((c) => c.days || 0));
    const regPills = h('div', { class: 'pills', role: 'group', 'aria-label': 'Region', cover: 'visited.cities.column_filter' },
      ['', ...regionsOf].slice(0, 8).map((r) => h('button', { type: 'button', class: 'fpill fpill-sm', 'aria-pressed': String(region === r), onclick: () => { region = r; write(); load(); } }, r || 'Every region')));
    citiesT.replaceChildren(citiesT.firstChild, regPills, shown.length ? list(shown.slice(0, 12).map((c) => row({
      name: h('span', { cover: 'visited.cities.city' }, c.label),
      sub: [h('span', { cover: 'visited.cities.region' }, c.region || '—'), ', ', h('span', { cover: 'visited.cities.country' }, c.country || c.country_code), ' · ',
        h('span', { cover: ['visited.cities.first_visit', 'visited.cities.last_visit'] }, `${formatDate(c.first_ms, tz, { now: api.now() })} to ${formatDate(c.last_ms, tz, { now: api.now() })}`)],
      side: h('span', { cover: 'visited.cities.days' }, `${formatNumber(c.days)} days`),
      onclick: () => pickCity(c.key), current: city === c.key, shade: (c.days || 0) / top, cover: 'visited.link.city_days',
    }))) : note('No city matches.'));

    const rl = regions?.items || [];
    const rtop = Math.max(1, ...rl.map((r) => r.days || 0));
    regionsT.replaceChildren(regionsT.firstChild, rl.length ? list(rl.map((r) => row({ name: r.label, sub: r.country, side: `${formatNumber(r.days)} days`, shade: (r.days || 0) / rtop }))) : note('No region in this period.'));

    // places visited: named places link to edit, addresses to "name this place"
    const pl = places?.items || [];
    placesT.replaceChildren(placesT.firstChild, pl.length ? list(pl.map((p2) => row({
      name: h('span', { cover: ['locations.addresses.name', 'locations.geofences.name'] }, p2.label),
      sub: [h('span', { cover: 'locations.addresses.neighbourhood' }, p2.neighbourhood || p2.street || ''), p2.neighbourhood || p2.street ? ', ' : '',
        h('span', { cover: ['locations.addresses.city', 'locations.addresses.state', 'locations.addresses.country'] }, [p2.locality, p2.region, p2.country_code].filter(Boolean).join(', ')),
        ' · ', h('span', { cover: 'locations.addresses.updated_at' }, `last ${formatDate(p2.last_ms, tz, { now: api.now() })}`)],
      side: [h('span', { cover: 'locations.geofences.time' }, dur(p2.dwell_ms)), p2.place ? chip('Saved', { tone: 'teal' }) : chip('Address', { tone: 'muted' })],
      href: placeHref({ place: p2.place, lat: p2.lat, lon: p2.lon }), cover: p2.place ? 'locations.link.edit_geofence' : 'locations.link.new_geofence',
    }))) : note('No place in this period.'),
    h('p', { class: 'note' }, 'Tap a saved place to edit it, or an address to name it as a place.'));
    // make sure both link kinds are marked even when every row is one kind
    placesT.firstChild.dataset.cover = 'locations.link.edit_geofence locations.link.new_geofence';

    const sl = stops?.items || [];
    stopsT.replaceChildren(stopsT.firstChild, sl.length ? list(sl.map((v) => row({
      name: h('span', { cover: 'locations.last_visited.address' }, v.label || v.address?.short || 'Unnamed spot'),
      sub: [h('span', { cover: 'locations.last_visited.date' }, `${formatDate(v.arrived_ms, v.time_zone || tz, { now: api.now() })}, ${formatTime(v.arrived_ms, v.time_zone || tz)}`), ' · ',
        h('span', { cover: 'locations.last_visited.city' }, v.address?.locality || '—'),
        v.sources?.length || v.source ? h('span', { class: 'src-line' }, srcPill(v, { short: true })) : null],
      side: carName(v.car),
    }))) : note('No stop in this period.'));

    // the first time, the page goes in with its tiles filled, so nothing is pushed down when the answers arrive
    if (!bar.isConnected) ctx.view.replaceChildren(bar, sumBox, mapT, citiesT, daysT, countriesT, regionsT, placesT, stopsT);
    await drawDays();
    await drawMap(tracks, allCities);
  }

  async function pickCity(key) {
    city = city === key ? null : key;
    write();
    for (const b of citiesT.querySelectorAll('.trow[aria-pressed]')) b.setAttribute('aria-pressed', 'false');
    await drawDays();
  }
  async function drawDays() {
    const head = daysT.firstChild;
    if (!city) {
      daysT.replaceChildren(head, h('p', { class: 'lede', cover: 'visited.state.selected_city' }, 'Pick a city'),
        h('p', { class: 'note', cover: 'visited.text.city_days_help' }, 'Tap a city in the list or on the map to see each day spent there: when the cars came and left, their drives, charges and places.'),
        h('span', { class: 'sr-only', cover: ['visited.city_days.date', 'visited.city_days.first_seen', 'visited.city_days.last_seen', 'visited.city_days.drives', 'visited.city_days.charges', 'visited.city_days.places', 'visited.link.day_drives'] }, 'No city picked.'));
      return;
    }
    const res = await ctx.api.page('visits/days', { ...base(), city }, { limit: 14 }).catch(() => null);
    if (!ctx.alive()) return;
    const items = res?.items || [];
    daysT.replaceChildren(head,
      h('div', { class: 'tv-city', cover: 'visited.state.selected_city' }, h('p', { class: 'lede' }, city.split('/').pop()), btn('Clear', { kind: 'ghost', icon: 'close', iconOnly: true, onclick: () => pickCity(city) })),
      h('p', { class: 'note', cover: 'visited.text.city_days_help' }, 'Each day a car was in the city, in that car’s own time zone.'),
      items.length ? list(items.map((d) => {
        const z = d.time_zone || zoneOf(d.car);
        return row({
          name: h('span', { cover: 'visited.city_days.date' }, `${formatDayKey(d.date)} · ${carName(d.car)}`),
          sub: [h('span', { cover: 'visited.city_days.first_seen' }, formatTime(d.first_ms, z)), ' – ', h('span', { cover: 'visited.city_days.last_seen' }, formatTime(d.last_ms, z)), ' · ',
            h('span', { cover: 'visited.city_days.places' }, (d.places || []).join(', ') || 'no named place')],
          side: [h('span', { cover: 'visited.city_days.drives' }, `${formatNumber(d.drives?.length || 0)} drives`), h('span', { cover: 'visited.city_days.charges' }, `${formatNumber(d.charges?.length || 0)} charges`)],
          href: state.buildUrl('drives', { car: d.car, period: { from: d.first_ms - 60000, to: d.last_ms + 60000 } }), cover: 'visited.link.day_drives',
        });
      })) : note('No day found in this city for this period.'));
  }

  const pins = [];
  async function drawMap(tracks, allCities) {
    const lines = (tracks?.drives || []).map((t) => t.lon.map((x, i) => [x, t.lat[i]]));
    const pts = [...lines.flat(), ...allCities.map((c) => [c.lon, c.lat])];
    if (!map) {
      if (!pts.length) { mapBox.append(h('p', { class: 'map-note' }, 'No route or stop in this period.')); return; }
      map = await mapTile(ctx, mapBox, { bounds: boundsOf(pts), padding: 40, fullscreen: true });
      if (!map) return;
      mapReady = map.ready;
    }
    if (!map.map) return;
    for (const p2 of pins.splice(0)) p2.remove();
    await map.routes('all', lines, 'c1');
    for (const c of allCities.slice(0, 80)) {
      pins.push(map.pin([c.lon, c.lat], { tone: 'teal', title: `${c.label}: ${formatNumber(c.days)} days, ${formatNumber(c.visits)} stops`, onclick: () => { mapNote.textContent = `${c.label}, ${c.region || c.country}: ${formatNumber(c.days)} days, ${formatNumber(c.visits)} stops, ${dur(c.dwell_ms)} parked.`; pickCity(c.key); } }));
    }
    if (pts.length) map.frame(boundsOf(pts));
  }

  try { await load(); } catch (err) {
    ctx.view.replaceChildren(bar, emptyTile({ icon: 'globe', title: 'Travel did not load', text: err.message || '' }));
  }
  await mapReady;
  if (!ctx.alive()) return {};
  ctx.ready();
  return {};
}

