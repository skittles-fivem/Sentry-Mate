// Sentry Mate, "Logbook" UI pack: Travel (/travel?cars=all|ids).
//
// Where the cars have been, for one car, several or all of them. One definition of a stop (a visit: from
// arriving to leaving) feeds every part, so the figures, the map's markers and the lists never disagree.
// Data:
//   GET /cars                                  the car pick list
//   GET /places                                the saved places for the place filter
//   GET /visits/summary                        the figures
//   GET /visits/countries, /cities, /regions   the country, city and region lists, the rankings, the markers
//   GET /visits/places                         the places visited, the place markers
//   GET /visits/days?city=                     the days in the chosen city
//   GET /visits                                the recent stops (paged, CSV from the server)
//   GET /tracks                                every drive's route (one line per drive, a colour per car)
//
// The filters (cars, text, place, country, region and the chosen city) live in the address; the period is
// the frame's. Days are counted in each car's own time zone. A drive, charge or stop that starts or ends
// ("rows" events) loads the page's figures and lists again.

import { h, replace } from '../shared/dom.js';
import { format, formatMoney } from '../shared/units.js';
import { formatDate, formatDateTime, formatTime, formatDuration, formatDayKey, dayStart, addDays, periodLabel, toIsoZoned } from '../shared/time.js';
import { formatNumber, compareText, tn } from '../shared/i18n.js';
import { downloadCsv, csvFilename, column, timeColumn } from '../shared/csv.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { trackBounds, dropJumps } from '../shared/tracks.js';
import { pageHead, q, figure, icon, empty, errorView, button, sep, loadingBlock, cover } from '../core/ui.js';
import { rowTitle } from '../core/sources.js';
import { dataTable } from '../core/table.js';
import { popover } from '../core/dialog.js';
import { choice, soldPickRows } from '../core/form.js';
import { makeMap, boundsOf, coordsText } from '../core/map.js';
import { zoneLabel, zoneShort } from '../core/format.js';
import { placeLink } from '../core/journal.js';

export const id = 'travel';
export const css = true;
export const title = () => 'Travel';

const CAP = 5000;              // grouped rows loaded for the map, the rankings and the local tables
const MAX_CAR_ROWS = 24;       // cars given their own row in "By car"
const MAX_MARKS = 400;         // markers drawn on the map
const CAR_TOKENS = ['--accent', '--ochre', '--dusk', '--slate', '--rust', '--ink-2'];
const RANK_BY = [
  ['places', 'Places', 'different places there: saved places, or spots about 150 m apart'],
  ['days', 'Days', 'calendar days with a stop there (a day counts once per car)'],
  ['visits', 'Stops', 'stops there, from arriving to leaving'],
  ['dwell_ms', 'Time', 'time parked there, inside the period'],
];
const idList = (v) => (v ? String(v).split(',').map((x) => x.trim()).filter(Boolean) : []);
const csvButton = (label, onclick, cover = null, text = 'Download CSV') => h('button', { type: 'button', class: 'button button-quiet button-small', onclick, cover, 'aria-label': `Download CSV: ${label}` }, icon('download'), text);
const mixed = (list) => new Set(list.map((x) => x.currency)).size > 1;
const moneyText = (list) => { const l = (list || []).filter((x) => x && x.amount != null); return l.map((x) => formatMoney(x.amount, x.currency) + (mixed(l) ? ` ${x.currency}` : '')).join(' + '); };
const cityOfKey = (key) => String(key || '').split('/').pop();
// a city group's name: its label, else the city part of its key; a stop with no town known (out at sea, or no
// address list for that area) groups under a key ending in "-", which is no name to show
const cityName = (row, key) => (row && row.label) || (cityOfKey(key ?? (row && row.key)).replace(/^-$/, '') || 'Unknown town');

export async function mount(ctx) {
  const now = api.now();
  const query = ctx.query || {};
  const f = { q: query.q || '', place: query.place || '', country: (query.country || '').toUpperCase(), region: query.region || '' };
  let city = query.city || null;
  let cityPicked = !!city;

  /* ---------------------------------------------------------------- which cars */

  let fleet = [];
  fleet = await ctx.api.list('cars', { sort: 'name' }, { limit: 200 }).all({ max: 5000 });
  if (!ctx.alive()) return {};
  const byId = new Map(fleet.map((c) => [c.id, c]));
  const asked = idList(query.cars).filter((x) => byId.has(x));
  const all = !asked.length || query.cars === 'all';
  const viewCars = all ? fleet.map((c) => c.id) : asked;
  const multi = viewCars.length > 1;
  const carsParam = all ? 'all' : asked.join(',');
  const carName = (cid) => (byId.get(cid) && byId.get(cid).name) || cid;
  const carTz = (cid) => (byId.get(cid) && byId.get(cid).time_zone) || ctx.tz;
  const rowTz = (r) => r.time_zone || (r.car ? carTz(r.car) : r.cars && r.cars.length === 1 ? carTz(r.cars[0]) : viewCars.length === 1 ? carTz(viewCars[0]) : ctx.tz);
  const tz = viewCars.length === 1 ? carTz(viewCars[0]) : ctx.tz;
  const zones = [...new Set(viewCars.map(carTz))];
  const scopeName = all ? (fleet.length === 1 ? fleet[0].name : `All ${formatNumber(fleet.length)} cars`) : asked.length === 1 ? carName(asked[0]) : `${formatNumber(asked.length)} cars`;
  const fileBase = all ? 'all-cars' : viewCars.map(carName).join('-');
  const colourOf = new Map(viewCars.map((cid, i) => [cid, i < CAR_TOKENS.length ? i : 'x']));
  const swatch = (cid) => h('span', { class: ['tv-swatch', `tv-c${colourOf.get(cid) ?? 'x'}`], 'aria-hidden': 'true' });
  const carsCell = (list) => (list || []).length ? h('span', { class: 'tv-cars' }, list.map((cid) => h('span', { class: 'tv-car' }, swatch(cid), carName(cid)))) : '—';
  const fileName = (...parts) => csvFilename(fileBase, 'travel', ...parts);
  const day = (ms, rz = tz) => (ms == null ? '—' : formatDate(ms, rz, { now, weekday: false, year: true }));
  const dayTime = (ms, rz = tz) => (ms == null ? '—' : formatDateTime(ms, rz, { now, weekday: false }));
  // A group's first and last moments belong to one car each (first_car, last_car): shown in that car's zone.
  const firstTz = (r) => (r && r.first_car ? carTz(r.first_car) : rowTz(r || {}));
  const lastTz = (r) => (r && r.last_car ? carTz(r.last_car) : rowTz(r || {}));
  // A date in a car's zone; when the cars' zones differ, the zone is named under it.
  const dayIn = (ms, rz) => (ms == null ? '—' : zones.length > 1 ? h('span', { class: 'tv-dated' }, day(ms, rz), h('span', { class: 'tv-sub tv-zone' }, zoneShort(rz, ms))) : day(ms, rz));
  const zonedColumn = (label, key, zoneOf) => ({ label, value: (r) => toIsoZoned(r[key], zoneOf(r)) });

  /* ---------------------------------------------------------------- head */

  const reloadBtn = button('Load again', { iconName: 'reset', cls: 'button-small', onclick: () => loadAll().catch(() => {}),
    cover: ['locations.control.refresh', 'visited.control.refresh'],
    title: 'Load every figure, the map markers and the lists again now.' });
  const asOf = h('p', { class: 'tv-asof' });
  const drawAsOf = () => replace(asOf, 'Figures as of ', formatTime(api.now(), tz), ' · they also load again by themselves when a drive, a charge or a stop starts or ends.');
  drawAsOf();
  ctx.main.append(pageHead({
    eyebrow: h('span', { class: 'tv-scope', cover: multi ? null : 'visited.section.car_header' }, 'Reports', sep(), scopeName),
    title: 'Travel',
    deck: 'Where the cars have been: every stop from arriving to leaving, grouped into places, cities, regions and countries, with the time spent there.',
    aside: h('div', { class: 'tv-head-aside' }, reloadBtn, asOf),
    cover: ['shots.locations', 'shots.visited'],
  }));

  /* ---------------------------------------------------------------- filters */

  const goCars = (ids) => {
    const every = !ids.length || (fleet.length > 1 && ids.length === fleet.length);
    ctx.go(state.buildUrl('travel', { period: ctx.state.period, query: { ...queryNow(), city: null, cars: every ? null : ids.join(',') } }));
  };
  const carField = carPicker(ctx, { fleet, chosen: all ? [] : asked, onapply: goCars });
  const queryNow = () => ({ cars: all ? null : asked.join(','), q: f.q || null, place: f.place || null, country: f.country || null, region: f.region || null, city: cityPicked ? city : null });
  const applyFilters = () => { ctx.setQuery(queryNow()); loadAll().catch(() => {}); };

  // The period's edges are in the viewer's zone (ctx.tz); every date and time below is in the zone of the car it
  // belongs to. Say so whenever the two can differ, so a first stop on the day before the period is no surprise.
  function zoneWhy() {
    const own = zoneLabel(ctx.tz);
    if (zones.length > 1) return `the period runs in ${own}; dates and times are in each car’s own zone, named next to each date`;
    const z = zones[0] || ctx.tz;
    if (z === ctx.tz) return `days and times in ${own}`;
    return `the period runs in ${own}; dates and times are in ${viewCars.length === 1 ? `${carName(viewCars[0])}’s` : 'the cars’'} zone, ${zoneLabel(z)}, so the first stop can fall on the day before`;
  }
  const periodLine = h('p', { class: 'tv-period', cover: ['locations.filter.time_range', 'visited.filter.time_range'] },
    h('span', { class: 'field-label' }, 'Period'),
    h('span', { class: 'tv-period-text' }, periodLabel(ctx.period, ctx.tz, now)), sep(),
    h('span', { class: 'tv-period-why' }, zoneWhy(), sep(),
      'a stop belongs to the period it began in; time spent is cut at the edges'),
    button('Change the period', { cls: 'button-small', iconName: 'calendar', onclick: () => document.querySelector('.period-trigger')?.click() }));

  const textIn = h('input', { class: 'field-input', id: 'tv-q', type: 'search', value: f.q, placeholder: 'Place, street or town', autocomplete: 'off' });
  let textTimer = 0;
  textIn.addEventListener('input', () => { clearTimeout(textTimer); textTimer = setTimeout(() => { const v = textIn.value.trim(); if (v !== f.q) { f.q = v; applyFilters(); } }, 450); });
  ctx.onCleanup(() => clearTimeout(textTimer));
  const placeSel = h('select', { class: 'field-select', id: 'tv-place' }, h('option', { value: '' }, 'Any place'));
  const countrySel = h('select', { class: 'field-select', id: 'tv-country' }, h('option', { value: '' }, 'Every country'));
  const regionSel = h('select', { class: 'field-select', id: 'tv-region' }, h('option', { value: '' }, 'Every region'));
  placeSel.addEventListener('change', () => { f.place = placeSel.value; applyFilters(); });
  countrySel.addEventListener('change', () => { f.country = countrySel.value; f.region = ''; applyFilters(); });
  regionSel.addEventListener('change', () => {
    const [cc, ...rest] = regionSel.value.split('/');
    f.region = rest.join('/'); if (cc && cc !== '-' && f.region) f.country = cc;
    applyFilters();
  });
  const clearBtn = button('Clear the filters', { cls: 'button-small', iconName: 'close', onclick: () => {
    f.q = ''; f.place = ''; f.country = ''; f.region = ''; textIn.value = ''; applyFilters();
  } });
  const cityLine = h('p', { class: 'tv-city-line', cover: 'visited.state.selected_city', 'aria-live': 'polite' });

  const fieldBox = (label, forId, control, hint = null, cover = null, cls = null) => h('div', { class: ['field', cls], cover },
    h('label', { class: 'field-label', for: forId }, label), control, hint ? h('p', { class: 'field-hint' }, hint) : null);
  ctx.main.append(h('section', { class: 'tv-filters', 'aria-label': 'What the page covers' },
    periodLine,
    h('div', { class: 'tv-filter-row' },
      fieldBox('Cars', 'travel-cars', carField, fleet.length > 1 ? 'One, several or every car: one page with a car column, not a page per car.' : 'The one car on this server.', ['locations.filter.car', 'visited.filter.car'], 'tv-f-cars'),
      fieldBox('Text', 'tv-q', h('div', { class: 'field-control' }, h('span', { class: 'field-icon' }, icon('search')), textIn), 'In the place name or any part of the address.', 'locations.filter.address', 'field-search tv-f-text'),
      fieldBox('Saved place', 'tv-place', placeSel, null, null, 'tv-f-place'),
      fieldBox('Country', 'tv-country', countrySel, null, null, 'tv-f-country'),
      fieldBox('Region', 'tv-region', regionSel, null, null, 'tv-f-region'),
      h('div', { class: 'tv-f-clear' }, clearBtn)),
    cityLine));

  /* ---------------------------------------------------------------- the parts */

  const figsBox = h('div', { class: 'tv-figs-box' }, loadingBlock(3));
  const byCarBox = h('div', { class: 'tv-bycar-box' });
  const mapBox = h('div', { class: 'tv-map' });
  const mapNote = h('p', { class: 'note tv-map-note' });
  const readout = h('div', { class: 'readout tv-readout', 'aria-live': 'polite', cover: 'visited.map.marker_details' });
  const legend = h('div', { class: 'tv-legend', cover: 'visited.text.map_help' });
  let markMode = 'cities';
  const markChoice = choice({ label: 'Markers', name: 'travel-marks', value: markMode, options: [['cities', 'One per city'], ['places', 'One per place']],
    onchange: (v) => { markMode = v; drawMarks(); } });
  const marksCsv = csvButton('map markers', () => {
    const rows = markRows();
    downloadCsv(fileName(markMode === 'cities' ? 'city markers' : 'place markers'), rows, [
      { label: markMode === 'cities' ? 'City' : 'Place', value: (r) => r.label }, { label: 'Region', value: (r) => r.region }, { label: 'Country', value: (r) => r.country },
      { label: 'Latitude', value: (r) => r.lat }, { label: 'Longitude', value: (r) => r.lon },
      { label: 'Stops', value: (r) => r.visits }, { label: 'Days', value: (r) => r.days }, { label: 'Places', value: (r) => r.places },
      { label: 'Time there (h)', value: (r) => Math.round(r.dwell_ms / 36000) / 100 }, zonedColumn('First there', 'first_ms', firstTz), zonedColumn('Last there', 'last_ms', lastTz),
    ]);
  }, null, 'Markers CSV');
  const tracksCsv = csvButton('routes', () => downloadCsv(fileName('routes'), trackRows(), [
    { label: 'Car', value: (r) => carName(r.car) }, { label: 'Drive', value: (r) => r.drive }, timeColumn('Time', 't', tz),
    { label: 'Latitude', value: (r) => r.lat }, { label: 'Longitude', value: (r) => r.lon }, column('Accuracy', 'acc', 'length', { digits: 1 }),
  ]), null, 'Routes CSV');

  const mapSection = h('section', { class: 'block tv-map-block', id: 'tv-map', 'aria-labelledby': 'tv-map-title', cover: 'visited.map' },
    h('div', { class: 'block-head' },
      h('h2', { class: 'label', id: 'tv-map-title' }, 'Map', h('span', { class: 'label-note' }, 'the routes driven and where the cars stopped')),
      h('div', { class: 'block-actions' }, markChoice, marksCsv, tracksCsv)),
    h('div', { class: 'tv-map-grid' },
      h('div', { class: 'tv-map-main' }, mapBox, mapNote),
      h('aside', { class: 'tv-map-side' }, legend, readout)));

  const countriesBox = h('div', { class: 'tv-countries-box' });
  const ranksBox = h('div', { class: 'tv-ranks-box' });
  const citiesBox = h('div', { class: 'tv-cities-box' });
  const daysBox = h('div', { class: 'tv-days-box' });
  const placesBox = h('div', { class: 'tv-places-box' });
  const stopsBox = h('div', { class: 'tv-stops-box' });
  const daysTitle = h('span', { class: 'label-note' });

  ctx.main.append(
    h('section', { class: 'block tv-summary', 'aria-labelledby': 'tv-sum-title' },
      h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'tv-sum-title' }, 'Summary', h('span', { class: 'label-note' }, 'for the cars, period and filters above'))),
      figsBox,
      h('p', { class: 'note tv-shared', cover: 'visited.text.shared_visits_help' }, icon('info'),
        'One list of stops feeds everything on this page: the stop, place, city, region, country and day counts, the map’s markers and every list below. A stop is the time from arriving to leaving; places are saved places, else spots about 150 m apart, so one spot never counts twice.'),
      byCarBox),
    mapSection,
    h('div', { class: 'tv-pair' },
      h('section', { class: 'block tv-countries', id: 'tv-countries', 'aria-labelledby': 'tv-countries-title', cover: 'visited.table.countries' },
        h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'tv-countries-title' }, 'Countries', h('span', { class: 'label-note' }, 'most days first'))),
        countriesBox),
      h('section', { class: 'block tv-ranks', id: 'tv-ranks', 'aria-labelledby': 'tv-ranks-title' },
        h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'tv-ranks-title' }, 'Top cities and regions', h('span', { class: 'label-note' }, 'the ten with the most'))),
        ranksBox)),
    h('section', { class: 'block tv-cities', id: 'tv-cities', 'aria-labelledby': 'tv-cities-title', cover: 'visited.table.cities' },
      h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'tv-cities-title' }, 'Cities and regions', h('span', { class: 'label-note' }, 'pick a city to list its days'))),
      citiesBox),
    h('section', { class: 'block tv-days', id: 'tv-days', 'aria-labelledby': 'tv-days-title', cover: 'visited.table.city_days' },
      h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'tv-days-title' }, 'Days in a city', daysTitle)),
      h('p', { class: 'note tv-days-help', cover: 'visited.text.city_days_help' },
        'One row per car and day it was in the city, newest first. First and last seen are the first arrival and the last moment there that day, in the car’s own time zone. Each date opens that car’s drives of the day; the counts open its drives and charges. Pick another city in the list above or on the map.'),
      daysBox),
    h('section', { class: 'block tv-places', id: 'tv-places', 'aria-labelledby': 'tv-places-title', cover: 'locations.table.addresses' },
      h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'tv-places-title' }, 'Places visited', h('span', { class: 'label-note' }, 'stops grouped by saved place or by spot'))),
      placesBox),
    h('section', { class: 'block tv-stops', id: 'tv-stops', 'aria-labelledby': 'tv-stops-title', cover: 'locations.table.last_visited' },
      h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'tv-stops-title' }, 'Recent stops', h('span', { class: 'label-note' }, 'newest first, with arrival and departure'))),
      stopsBox));

  if (!viewCars.length) {
    replace(figsBox, empty({ title: 'No car to show.', text: fleet.length ? 'The cars in this address are unknown or hidden from your account. Pick other cars above.' : 'Add a car in Settings: Cars; its stops show up here once it has sent logs.' }));
    ctx.ready();
    return {};
  }

  /* ---------------------------------------------------------------- loading */

  const params = (patch = {}) => {
    const p = { cars: carsParam, ...ctx.params, q: f.q || null, place: f.place || null, country: f.country || null, region: f.region || null, ...patch };
    for (const k of Object.keys(p)) if (p[k] == null || p[k] === '') delete p[k];
    return p;
  };
  const allRows = async (path, p) => {
    const items = await ctx.api.list(path, p, { limit: 500 }).all({ max: CAP });
    return { items, capped: items.length >= CAP };
  };
  let savedPlaces = null;
  let data = { summary: null, countries: { items: [] }, cities: { items: [] }, regions: { items: [] }, places: { items: [] } };

  // Every drive's route, fetched with the figures so a drive that ends while the page is open is drawn too.
  let tracks = [];
  let tracksNote = null;
  async function loadTracks() {
    try {
      let r;
      try { r = await ctx.api.get('tracks', { cars: carsParam, ...ctx.params, max_points: 30000 }); } catch (err) {
        if (!(err && err.status === 400)) throw err;
        r = await ctx.api.get('tracks', { cars: carsParam, ...ctx.params, max_points: 200000 });
      }
      const list = (r.drives || []).map((d) => ({ car: d.car, drive: d.drive, source: d.source, t: d.t_ms, coords: d.lon.map((x, i) => [x, d.lat[i]]), acc: d.accuracy_m }))
        .map((d) => ({ ...d, coords: d.coords.filter((c) => c[0] != null && c[1] != null) })).filter((d) => d.coords.length > 1);
      return { list, note: `${tn(list.length, '{n} drive', '{n} drives')}, simplified on the server to ${formatNumber(r.points || 0)} points` };
    } catch (err) {
      return { list: null, note: err && err.status === 400 ? 'Too many drives in this period to draw their routes; choose a shorter period. The markers still show every stop.' : 'The routes could not be loaded; the markers still show every stop.' };
    }
  }

  let gen = 0;
  async function loadAll(first = false) {
    const my = ++gen;
    const narrowed = f.country || f.region;
    const [sumR, coR, ciR, reR, plR, savedR, coOptR, reOptR, perCarR, trR] = await Promise.allSettled([
      ctx.api.get('visits/summary', params()),
      allRows('visits/countries', params()),
      allRows('visits/cities', params()),
      allRows('visits/regions', params()),
      allRows('visits/places', params()),
      savedPlaces ? Promise.resolve(savedPlaces) : ctx.api.list('places', { sort: 'name' }, { limit: 200 }).all({ max: 5000 }),
      narrowed ? allRows('visits/countries', params({ country: null, region: null })) : null,
      f.region ? allRows('visits/regions', params({ region: null })) : null,
      multi ? Promise.allSettled(viewCars.slice(0, MAX_CAR_ROWS).map((cid) => ctx.api.get('visits/summary', params({ cars: cid })))) : null,
      loadTracks(),
    ]);
    if (my !== gen || !ctx.alive()) return;
    if (sumR.status === 'rejected') {
      if (first) throw sumR.reason;
      replace(figsBox, errorView(sumR.reason, { retry: () => loadAll(), eyebrow: 'Could not load the figures' }));
      return;
    }
    if (savedR.status === 'fulfilled') savedPlaces = savedR.value;
    if (trR.status === 'fulfilled') {
      // A failed reload keeps the routes already drawn; the note says they could not be loaded again.
      if (trR.value.list) tracks = trR.value.list;
      tracksNote = trR.value.note;
      drawRoutes();
    }
    const ok = (r) => (r.status === 'fulfilled' ? r.value : null);
    data = {
      summary: sumR.value,
      countries: ok(coR) || { items: [], error: coR.reason },
      cities: ok(ciR) || { items: [], error: ciR.reason },
      regions: ok(reR) || { items: [], error: reR.reason },
      places: ok(plR) || { items: [], error: plR.reason },
    };
    const countryOpts = ok(coOptR) || data.countries;
    const regionOpts = ok(reOptR) || data.regions;
    const byDays = (a, b) => (b.days - a.days) || (b.visits - a.visits) || compareText(a.label || '', b.label || '');
    data.cities.items.sort(byDays);
    data.countries.items.sort(byDays);
    data.regions.items.sort(byDays);
    // The address's city stays even when the filters leave it no stop; else the city with the most days.
    if (!cityPicked || !city) city = data.cities.items.length ? data.cities.items[0].key : null;
    drawSelects(countryOpts.items, regionOpts.items);
    drawFigures(data.summary);
    drawByCar(perCarR && perCarR.status === 'fulfilled' ? perCarR.value : null);
    drawCountries();
    drawRanks();
    drawCities();
    drawCityLine();
    drawMarks(true);
    drawAsOf();
    await Promise.all([drawDays(), drawPlaces(), drawStops()]);
  }

  /* ---------------------------------------------------------------- filter controls */

  function drawSelects(countries, regions) {
    const pl = savedPlaces || [];
    replace(placeSel, h('option', { value: '' }, 'Any place'),
      pl.map((p) => h('option', { value: p.id, selected: f.place === p.id }, p.name || p.id)),
      f.place && !pl.some((p) => p.id === f.place) ? h('option', { value: f.place, selected: true }, `Place ${f.place} (removed)`) : null);
    replace(countrySel, h('option', { value: '' }, 'Every country'),
      countries.filter((c) => c.country_code).map((c) => h('option', { value: c.country_code, selected: f.country === c.country_code }, `${c.label || c.country_code} (${tn(c.days, '{n} day', '{n} days')})`)),
      f.country && !countries.some((c) => c.country_code === f.country) ? h('option', { value: f.country, selected: true }, f.country) : null);
    const twice = new Map();
    for (const r of regions) if (r.region) twice.set(r.region, (twice.get(r.region) || 0) + 1);
    const regionKey = (r) => `${r.country_code || '-'}/${r.region}`;
    replace(regionSel, h('option', { value: '' }, 'Every region'),
      regions.filter((r) => r.region).map((r) => h('option', { value: regionKey(r), selected: !!f.region && f.region.toLowerCase() === r.region.toLowerCase() && (!f.country || f.country === r.country_code) },
        twice.get(r.region) > 1 ? `${r.region}, ${r.country_code}` : r.region)));
    clearBtn.disabled = !(f.q || f.place || f.country || f.region);
  }

  function drawCityLine() {
    const row = data.cities.items.find((c) => c.key === city);
    if (data.cities.error && !city) {
      replace(cityLine, h('span', { class: 'field-label' }, 'City for the day list'), ' ', h('span', { class: 'tv-lost-text' }, 'not loaded: the city list could not be loaded.'), ' ',
        button('Try again', { cls: 'button-small', iconName: 'reset', onclick: () => loadAll().catch(() => {}) }));
      return;
    }
    if (!city) { replace(cityLine, h('span', { class: 'field-label' }, 'City for the day list'), ' No city in the period yet.'); return; }
    replace(cityLine, h('span', { class: 'field-label' }, 'City for the day list'), ' ',
      h('b', null, cityName(row, city)), row ? [' ', h('span', { class: 'tv-city-sub' }, [row.region, row.country].filter(Boolean).join(', '))] : null,
      cityPicked ? null : h('span', { class: 'tv-city-sub' }, ' · the city with the most days; pick another in the list or on the map'),
      cityPicked ? [' ', button('Back to the top city', { cls: 'button-small', iconName: 'close', onclick: () => { cityPicked = false; city = data.cities.items[0]?.key || null; ctx.setQuery({ city: null }); drawCityLine(); drawCities(); drawDays().catch(() => {}); } })] : null);
  }

  function pickCity(key, { scroll = true } = {}) {
    city = key; cityPicked = true;
    ctx.setQuery({ city: key });
    drawCityLine(); drawCities();
    drawDays().catch(() => {});
    if (scroll) document.getElementById('tv-days')?.scrollIntoView({ behavior: 'smooth', block: 'start' });
  }

  /* ---------------------------------------------------------------- summary */

  function drawFigures(s) {
    const narrowed = !!(f.q || f.place || f.country || f.region);
    const none = !s.visits;
    const why = 'no stop in the period';
    // stops there, but none with a known address: the server cannot count cities, regions or countries
    const whyArea = none ? why : 'no stop has a known address';
    const cost = moneyText(s.cost_by_currency);
    const eff = s.efficiency || {};
    const notNarrowed = narrowed ? ' (the place filters narrow stops only, not charges)' : '';
    const driveNote = narrowed ? 'every drive of these cars in the period (the place filters narrow stops only)' : 'every drive of these cars in the period';
    const fz = firstTz(s), lz = lastTz(s);
    const whenNote = (ms, rz, cid) => [formatTime(ms, rz), ' ', zoneShort(rz, ms), multi && cid ? [sep(), carName(cid)] : null];
    const unpriced = s.charges_without_cost ? tn(s.charges_without_cost, '{n} charge', '{n} charges') : '';
    const costNote = cost
      ? [mixed(s.cost_by_currency || []) ? 'each currency kept apart' : 'as charged', unpriced ? `; ${unpriced} without a cost left out` : '', notNarrowed]
      : unpriced ? [`${unpriced}, none with a cost`, notNarrowed] : null;
    replace(figsBox,
      h('div', { class: 'figs tv-figs' },
        figure('Stops', q(s.visits ?? 0, 'count'), 'from arriving to leaving'),
        figure('Places', q(none ? null : s.places, 'count', { why }), none ? null : s.unnamed_stops ? `${tn(s.unnamed_stops, '{n} stop', '{n} stops')} at no saved place` : 'every stop was at a saved place', { cover: 'locations.stat.addresses' }),
        figure('Cities', q(none ? null : s.cities, 'count', { why: whyArea }), 'country, region and city together, so namesakes stay apart', { cover: ['locations.stat.cities', 'visited.stat.cities'] }),
        figure('Regions', q(none ? null : s.regions, 'count', { why: whyArea }), 'states, provinces or the like', { cover: 'locations.stat.states' }),
        figure('Countries', q(none ? null : s.countries, 'count', { why: whyArea }), 'by country code', { cover: ['locations.stat.countries', 'visited.stat.countries'] }),
        figure('Days out', q(none ? null : s.days, 'count', { why }), multi ? 'calendar days with a stop, counted per car in its own zone' : 'calendar days with a stop', { cover: 'visited.stat.days' }),
        figure('First stop', none ? q(null, 'count', { why }) : day(s.first_ms, fz), none ? null : whenNote(s.first_ms, fz, s.first_car), { cover: 'visited.stat.first_visit' }),
        figure('Last stop', none ? q(null, 'count', { why }) : day(s.last_ms, lz), none ? null : s.last_ms >= now - 60000 ? ['still there now', multi && s.last_car ? [sep(), carName(s.last_car)] : null] : whenNote(s.last_ms, lz, s.last_car), { cover: 'visited.stat.last_visit' })),
      h('div', { class: 'figs tv-figs tv-figs-2' },
        figure('Distance', q(s.distance_km, 'distance', { why: 'nothing driven' }), driveNote, { cover: 'visited.stat.distance' }),
        figure('Energy added', q(s.energy_added_kwh, 'energy', { why: 'no charge' }), `into the pack, over every charge in the period${notNarrowed}`, { cover: 'visited.stat.energy_added' }),
        figure('Energy drawn', q(s.energy_grid_kwh || null, 'energy', { why: 'no charge with a meter reading' }), (eff.charges_counted ? `from the grid, over ${tn(eff.charges_counted, '{n} charge', '{n} charges')} with a meter reading` : 'from the grid; fast chargers send no meter reading') + notNarrowed, { cover: 'visited.stat.energy_drawn' }),
        figure('Charging efficiency', q(eff.value, 'percent', { why: 'needs a charge with a meter reading' }), eff.value != null ? `added ÷ drawn, over those same charges only${notNarrowed}` : null, { cover: 'visited.stat.charging_efficiency' }),
        figure('Charging cost', cost ? h('span', { class: 'tv-money' }, cost) : q(null, 'money', { why: 'no priced charge' }),
          costNote, { cover: 'visited.stat.charging_cost' })));
  }

  function drawByCar(list) {
    if (!multi) { replace(byCarBox); return; }
    const rows = (list || []).map((r, i) => ({ car: viewCars[i], s: r.status === 'fulfilled' ? r.value : null, err: r.status === 'rejected' })).filter((r) => r.car);
    const t = dataTable(ctx, {
      rows, label: 'Per car', itemName: ['car', 'cars'], sort: { key: 'visits', dir: 'desc' },
      caption: viewCars.length > MAX_CAR_ROWS ? `the first ${formatNumber(MAX_CAR_ROWS)} of ${formatNumber(viewCars.length)} cars; pick fewer cars to see the others` : 'each car on its own, with the same filters',
      columns: [
        { key: 'car', label: 'Car', th: true, sort: true, value: (r) => carName(r.car), cell: (r) => h('span', { class: 'tv-car tv-car-head' }, swatch(r.car), carName(r.car)) },
        { key: 'visits', label: 'Stops', num: true, sort: true, value: (r) => r.s?.visits, cell: (r) => (r.s ? q(r.s.visits, 'count') : r.err ? 'not loaded' : '—') },
        { key: 'places', label: 'Places', num: true, sort: true, value: (r) => r.s?.places, cell: (r) => (r.s ? q(r.s.places, 'count') : '—') },
        { key: 'cities', label: 'Cities', num: true, sort: true, value: (r) => r.s?.cities, cell: (r) => (r.s ? q(r.s.cities, 'count') : '—') },
        { key: 'countries', label: 'Countries', num: true, sort: true, value: (r) => r.s?.countries, cell: (r) => (r.s ? q(r.s.countries, 'count') : '—') },
        { key: 'days', label: 'Days out', num: true, sort: true, value: (r) => r.s?.days, cell: (r) => (r.s ? q(r.s.days, 'count') : '—') },
        { key: 'first', label: 'First stop', sort: true, value: (r) => r.s?.first_ms, cell: (r) => (r.s?.first_ms ? dayIn(r.s.first_ms, carTz(r.car)) : '—') },
        { key: 'last', label: 'Last stop', sort: true, value: (r) => r.s?.last_ms, cell: (r) => (r.s?.last_ms ? dayIn(r.s.last_ms, carTz(r.car)) : '—') },
        { key: 'dist', label: 'Distance', num: true, sort: true, value: (r) => r.s?.distance_km, cell: (r) => (r.s ? q(r.s.distance_km, 'distance') : '—') },
      ],
      csv: { filename: fileName('per car'), columns: [
        { label: 'Car', value: (r) => carName(r.car) }, { label: 'Stops', value: (r) => r.s?.visits }, { label: 'Places', value: (r) => r.s?.places },
        { label: 'Cities', value: (r) => r.s?.cities }, { label: 'Regions', value: (r) => r.s?.regions }, { label: 'Countries', value: (r) => r.s?.countries },
        { label: 'Days out', value: (r) => r.s?.days }, { label: 'First stop', value: (r) => toIsoZoned(r.s?.first_ms, carTz(r.car)) }, { label: 'Last stop', value: (r) => toIsoZoned(r.s?.last_ms, carTz(r.car)) },
        column('Distance', (r) => r.s?.distance_km, 'distance', { digits: 1 }),
      ] },
      cover: 'visited.section.car_header', cls: 'tv-bycar',
    });
    replace(byCarBox, h('h3', { class: 'tv-sub-title' }, 'By car'), t.el);
    t.load();
  }

  /* ---------------------------------------------------------------- countries */

  function drawCountries() {
    const rows = data.countries.items;
    if (data.countries.error) { replace(countriesBox, errorView(data.countries.error, { retry: () => loadAll(), eyebrow: 'Could not load the countries' })); return; }
    const citiesIn = new Map();
    const citiesLost = !!data.cities.error;
    const cityCount = (r) => (citiesLost ? null : citiesIn.get(r.country_code || '-') || 0);
    for (const c of data.cities.items) citiesIn.set(c.country_code || '-', (citiesIn.get(c.country_code || '-') || 0) + 1);
    const t = dataTable(ctx, {
      rows, label: 'Countries', itemName: ['country', 'countries'], sort: { key: 'days', dir: 'desc' },
      empty: () => emptyStops(),
      columns: [
        { key: 'country', label: 'Country', th: true, sort: true, value: (r) => r.label, cover: 'visited.countries.country',
          cell: (r) => h('span', { class: 'cell-main' }, r.label || r.country_code || 'Unknown country', r.country_code ? h('span', { class: 'tv-code' }, r.country_code) : null) },
        { key: 'cities', label: 'Cities', num: true, sort: true, value: cityCount, cover: 'visited.countries.cities', cell: (r) => (citiesLost ? h('span', { class: 'tv-sub', title: 'The city list could not be loaded; see the note above the table.' }, 'not loaded') : q(cityCount(r), 'count')) },
        { key: 'visits', label: 'Stops', num: true, sort: true, cell: (r) => q(r.visits, 'count') },
        { key: 'days', label: 'Days', num: true, sort: true, cover: 'visited.countries.days', cell: (r) => q(r.days, 'count') },
        { key: 'dwell_ms', label: 'Time there', num: true, sort: true, cell: (r) => formatDuration(r.dwell_ms) },
        { key: 'first_ms', label: 'First', sort: true, cover: 'visited.countries.first_visit', cell: (r) => dayIn(r.first_ms, firstTz(r)) },
        { key: 'last_ms', label: 'Last', sort: true, cover: 'visited.countries.last_visit', cell: (r) => dayIn(r.last_ms, lastTz(r)) },
        { key: 'only', label: 'Narrow', cell: (r) => (r.country_code && f.country !== r.country_code ? h('button', { type: 'button', class: 'tv-row-link', onclick: () => { f.country = r.country_code; f.region = ''; applyFilters(); } }, 'Only here') : f.country === r.country_code ? h('span', { class: 'tv-sub' }, 'filtered') : null) },
      ],
      csv: { filename: fileName('countries'), columns: [
        { label: 'Country', value: (r) => r.label }, { label: 'Country code', value: (r) => r.country_code }, { label: 'Cities', value: cityCount },
        { label: 'Stops', value: (r) => r.visits }, { label: 'Places', value: (r) => r.places }, { label: 'Days', value: (r) => r.days },
        { label: 'Time there (h)', value: (r) => Math.round(r.dwell_ms / 36000) / 100 }, zonedColumn('First', 'first_ms', firstTz), zonedColumn('Last', 'last_ms', lastTz),
        { label: 'Cars', value: (r) => (r.cars || []).map(carName).join('; ') },
      ] },
      cls: 'tv-countries-table',
    });
    replace(countriesBox, citiesLost ? h('p', { class: 'note tv-lost' }, icon('info'), 'The city list could not be loaded, so the Cities column is left blank.', button('Try again', { cls: 'button-small', iconName: 'reset', onclick: () => loadAll().catch(() => {}) })) : null, t.el);
    t.load();
  }

  const emptyStops = () => empty({
    title: f.q || f.place || f.country || f.region ? 'No stop matches these filters.' : 'No stop in this period.',
    text: f.q || f.place || f.country || f.region ? 'Clear the text, place, country or region above, or choose a longer period.' : 'Choose a longer period, or other cars. A stop is recorded once a car parks after a drive.',
  });

  /* ---------------------------------------------------------------- rankings */

  let rankBy = 'places';
  function drawRanks() {
    const pick = choice({ label: 'Rank by', name: 'travel-rank', value: rankBy, options: RANK_BY.map(([k, t]) => [k, t]), onchange: (v) => { rankBy = v; drawRanks(); } });
    const why = RANK_BY.find((x) => x[0] === rankBy)[2];
    const top = (list) => [...list].sort((a, b) => (b[rankBy] - a[rankBy]) || compareText(a.label || '', b.label || '')).slice(0, 10);
    const chart = (name, src, cls, coverIds, sub) => {
      if (src.error) {
        return h('figure', { class: ['tv-rank', cls], cover: coverIds },
          h('figcaption', { class: 'tv-rank-head' }, h('span', { class: 'tv-rank-name' }, name)),
          errorView(src.error, { retry: () => loadAll().catch(() => {}), eyebrow: `Could not load the ${name.toLowerCase()}`, title: `The ${name === 'Cities' ? 'city' : 'region'} list could not be loaded.`, home: false, cls: 'tv-rank-error' }));
      }
      const list = src.items;
      const rows = top(list).map((r, i) => ({ ...r, rank: i + 1 }));
      const max = Math.max(1, ...rows.map((r) => r[rankBy] || 0));
      const valueText = (r) => (rankBy === 'dwell_ms' ? formatDuration(r.dwell_ms) : formatNumber(r[rankBy]));
      const csv = csvButton(`top ${name.toLowerCase()}`, () => downloadCsv(fileName(`top ${name.toLowerCase()}`, rankBy === 'dwell_ms' ? 'time' : rankBy), rows, [
        { label: 'Rank', value: (r) => r.rank }, { label: name === 'Cities' ? 'City' : 'Region', value: (r) => r.label },
        name === 'Cities' ? { label: 'Region', value: (r) => r.region } : null, { label: 'Country', value: (r) => r.country },
        { label: 'Places', value: (r) => r.places }, { label: 'Days', value: (r) => r.days }, { label: 'Stops', value: (r) => r.visits },
        { label: 'Time there (h)', value: (r) => Math.round(r.dwell_ms / 36000) / 100 },
      ].filter(Boolean)));
      return h('figure', { class: ['tv-rank', cls], cover: coverIds },
        h('figcaption', { class: 'tv-rank-head' }, h('span', { class: 'tv-rank-name' }, name), csv),
        rows.length ? h('ol', { class: 'tv-bars' }, rows.map((r, i) => {
          const share = (r[rankBy] || 0) / max;
          const tier = share >= 0.75 ? 1 : share >= 0.4 ? 2 : share >= 0.15 ? 3 : 4;
          const label = name === 'Cities'
            ? h('button', { type: 'button', class: 'tv-bar-name tv-row-link', onclick: () => pickCity(r.key), title: `List the days in ${cityName(r)}` }, cityName(r))
            : h('span', { class: 'tv-bar-name' }, r.label || 'Unknown region');
          return h('li', { class: ['tv-bar', `tv-tier-${tier}`] },
            h('span', { class: 'tv-bar-rank' }, String(i + 1)), label,
            h('span', { class: 'tv-bar-track' }, h('span', { class: 'tv-bar-fill', vars: { '--w': `${(share * 100).toFixed(2)}%` } })),
            h('span', { class: 'tv-bar-value' }, valueText(r)), h('span', { class: 'tv-bar-sub' }, sub(r)));
        })) : h('p', { class: 'note' }, 'None in the period.'));
    };
    replace(ranksBox,
      h('div', { class: 'tv-rank-tools' }, pick, h('p', { class: 'note tv-rank-why' }, `Ranked by ${why}.`)),
      h('div', { class: 'tv-rank-pair' },
        chart('Cities', data.cities, 'tv-rank-cities', 'locations.chart.top_cities', (r) => r.country_code || ''),
        chart('Regions', data.regions, 'tv-rank-regions', 'locations.chart.top_regions', (r) => r.country_code || '')),
      h('p', { class: 'note tv-rank-key', cover: 'locations.chart.rank_shading' },
        [['tv-key-cities', 'Cities'], ['tv-key-regions', 'Regions']].map(([c, n]) => h('span', { class: ['tv-key-set', c] },
          [1, 2, 3, 4].map((i) => h('span', { class: ['tv-key-chip', `tv-tier-${i}`], 'aria-hidden': 'true' })), ` ${n}`)),
        ' Shade follows the bar: darkest for three quarters of the top count or more, then 40 %, 15 % and below. Each bar runs from zero to the top count and prints its value.'));
  }

  /* ---------------------------------------------------------------- cities and regions */

  let level = 'cities';
  const tableFilter = { text: '', country: '', region: '' };
  function drawCities() {
    const src = level === 'cities' ? data.cities : data.regions;
    if (src.error) { replace(citiesBox, errorView(src.error, { retry: () => loadAll(), eyebrow: 'Could not load the list', title: `The ${level === 'cities' ? 'city' : 'region'} list could not be loaded.`, home: false })); return; }
    const levelChoice = choice({ label: 'Show', name: 'travel-level', value: level, options: [['cities', 'Cities'], ['regions', 'Regions']], onchange: (v) => { level = v; drawCities(); } });
    const findIn = h('input', { class: 'field-input', type: 'search', id: 'tv-find', placeholder: 'Find in this list', value: tableFilter.text, autocomplete: 'off' });
    const countries = [...new Set(src.items.map((r) => r.country).filter(Boolean))].sort(compareText);
    const regions = [...new Set(src.items.map((r) => r.region).filter(Boolean))].sort(compareText);
    const cSel = h('select', { class: 'field-select', id: 'tv-tc', 'aria-label': 'Country in this list' }, h('option', { value: '' }, 'Any country'), countries.map((c) => h('option', { value: c, selected: tableFilter.country === c }, c)));
    const rSel = level === 'cities' ? h('select', { class: 'field-select', id: 'tv-tr', 'aria-label': 'Region in this list' }, h('option', { value: '' }, 'Any region'), regions.map((c) => h('option', { value: c, selected: tableFilter.region === c }, c))) : null;
    const shown = () => src.items.filter((r) => {
      const s = tableFilter.text.toLowerCase();
      if (s && ![r.label, r.region, r.country, r.country_code].some((x) => String(x || '').toLowerCase().includes(s))) return false;
      if (tableFilter.country && r.country !== tableFilter.country) return false;
      if (level === 'cities' && tableFilter.region && r.region !== tableFilter.region) return false;
      return true;
    });
    const cityCols = [
      { key: 'label', label: level === 'cities' ? 'City' : 'Region', th: true, sort: true, cover: level === 'cities' ? ['visited.cities.city', 'visited.link.city_days'] : null,
        cell: (r) => (level === 'cities'
          ? h('button', { type: 'button', class: ['tv-row-link', 'tv-city-pick', { 'is-on': r.key === city }], 'aria-pressed': String(r.key === city), onclick: () => pickCity(r.key), title: `List the days in ${cityName(r)}` }, cityName(r), r.key === city ? h('span', { class: 'tv-sub' }, ' · its days are listed below') : null)
          : h('span', { class: 'cell-main' }, r.label || 'Unknown region')) },
      level === 'cities' ? { key: 'region', label: 'Region', sort: true, cover: 'visited.cities.region', cell: (r) => r.region || '—' } : null,
      { key: 'country', label: 'Country', sort: true, cover: level === 'cities' ? 'visited.cities.country' : null, cell: (r) => r.country || r.country_code || '—' },
      { key: 'visits', label: 'Stops', num: true, sort: true, cell: (r) => q(r.visits, 'count') },
      { key: 'places', label: 'Places', num: true, sort: true, cell: (r) => q(r.places, 'count') },
      { key: 'days', label: 'Days', num: true, sort: true, cover: level === 'cities' ? 'visited.cities.days' : null, cell: (r) => q(r.days, 'count') },
      { key: 'dwell_ms', label: 'Time there', num: true, sort: true, cell: (r) => formatDuration(r.dwell_ms) },
      { key: 'first_ms', label: 'First', sort: true, cover: level === 'cities' ? 'visited.cities.first_visit' : null, cell: (r) => dayIn(r.first_ms, firstTz(r)) },
      { key: 'last_ms', label: 'Last', sort: true, cover: level === 'cities' ? 'visited.cities.last_visit' : null, cell: (r) => dayIn(r.last_ms, lastTz(r)) },
      multi ? { key: 'cars', label: 'Cars', value: (r) => (r.cars || []).length, cell: (r) => carsCell(r.cars) } : null,
    ].filter(Boolean);
    const t = dataTable(ctx, {
      rows: shown(), label: level === 'cities' ? 'Cities' : 'Regions', itemName: level === 'cities' ? ['city', 'cities'] : ['region', 'regions'], sort: { key: 'days', dir: 'desc' },
      caption: src.capped ? `the first ${formatNumber(CAP)}; narrow the filters to see the rest` : 'days count once per car and calendar day',
      empty: () => (src.items.length ? empty({ title: `No ${level === 'cities' ? 'city' : 'region'} matches “${tableFilter.text || tableFilter.country || tableFilter.region}”.`, text: 'Clear the boxes above this list.' }) : emptyStops()),
      columns: cityCols,
      csv: { filename: fileName(level), columns: [
        { label: level === 'cities' ? 'City' : 'Region', value: (r) => r.label }, level === 'cities' ? { label: 'Region', value: (r) => r.region } : null,
        { label: 'Country', value: (r) => r.country }, { label: 'Country code', value: (r) => r.country_code }, { label: 'Key', value: (r) => r.key },
        { label: 'Stops', value: (r) => r.visits }, { label: 'Places', value: (r) => r.places }, { label: 'Days', value: (r) => r.days },
        { label: 'Time there (h)', value: (r) => Math.round(r.dwell_ms / 36000) / 100 }, zonedColumn('First', 'first_ms', firstTz), zonedColumn('Last', 'last_ms', lastTz),
        { label: 'Latitude', value: (r) => r.lat }, { label: 'Longitude', value: (r) => r.lon }, { label: 'Cars', value: (r) => (r.cars || []).map(carName).join('; ') },
      ].filter(Boolean) },
      cls: 'tv-cities-table',
    });
    const refilter = () => t.setRows(shown());
    let ft = 0;
    findIn.addEventListener('input', () => { clearTimeout(ft); ft = setTimeout(() => { tableFilter.text = findIn.value.trim(); refilter(); }, 200); });
    cSel.addEventListener('change', () => { tableFilter.country = cSel.value; refilter(); });
    rSel?.addEventListener('change', () => { tableFilter.region = rSel.value; refilter(); });
    replace(citiesBox,
      h('div', { class: 'tv-table-tools', cover: 'visited.cities.column_filter' },
        levelChoice,
        h('div', { class: 'field field-search tv-find' }, h('label', { class: 'field-label', for: 'tv-find' }, 'Narrow this list'), h('div', { class: 'field-control' }, h('span', { class: 'field-icon' }, icon('search')), findIn)),
        h('div', { class: 'field' }, h('label', { class: 'field-label', for: 'tv-tc' }, 'Country'), cSel),
        rSel ? h('div', { class: 'field' }, h('label', { class: 'field-label', for: 'tv-tr' }, 'Region'), rSel) : null,
        h('p', { class: 'field-hint tv-tools-hint' }, 'These narrow only this list; the filters at the top narrow the whole page.')),
      t.el);
    t.load();
  }

  /* ---------------------------------------------------------------- days in a city */

  async function drawDays() {
    const row = data.cities.items.find((c) => c.key === city);
    replace(daysTitle, city ? `${cityName(row, city)}${row && row.region ? `, ${row.region}` : ''}` : data.cities.error ? 'the city list is not loaded' : 'pick a city');
    if (!city && data.cities.error) { replace(daysBox, errorView(data.cities.error, { retry: () => loadAll(), eyebrow: 'Could not load the cities', title: 'The city list could not be loaded, so no city is chosen.', home: false })); return; }
    if (!city) { replace(daysBox, emptyStops()); return; }
    if (await again(daysBox, 'days', params({ city }))) return;
    const dayLink = (r, page) => {
      const rz = rowTz(r);
      const from = dayStart(r.date, rz), to = dayStart(addDays(r.date, 1), rz);
      return state.buildUrl(page, { car: r.car, period: { from, to } });
    };
    const t = dataTable(ctx, {
      path: 'visits/days', params: params({ city }), limit: 20, itemName: ['day', 'days'],
      label: 'Days in the city',
      empty: () => empty({ title: 'No day in this city in the period.', text: 'Pick another city above, or choose a longer period.' }),
      columns: [
        { key: 'date', label: 'Day', th: true, cover: ['visited.city_days.date', 'visited.link.day_drives'], cell: (r) => h('a', { href: dayLink(r, 'drives'), title: `${carName(r.car)}’s drives that day` }, formatDayKey(r.date, { weekday: true, month: 'short', year: true })) },
        multi ? { key: 'car', label: 'Car', cell: (r) => carsCell([r.car]) } : null,
        { key: 'first', label: 'First seen', cover: 'visited.city_days.first_seen', cell: (r) => (r.first_ms === dayStart(r.date, rowTz(r)) ? h('span', null, 'from midnight', h('span', { class: 'tv-sub' }, ' · there overnight')) : formatTime(r.first_ms, rowTz(r))) },
        { key: 'last', label: 'Last seen', cover: 'visited.city_days.last_seen', cell: (r) => h('span', null, r.last_ms >= now - 60000 ? 'still there' : r.last_ms === dayStart(addDays(r.date, 1), rowTz(r)) ? 'until midnight' : formatTime(r.last_ms, rowTz(r))) },
        { key: 'drives', label: 'Drives', num: true, cover: 'visited.city_days.drives', cell: (r) => (r.drives.length ? h('a', { href: dayLink(r, 'drives') }, formatNumber(r.drives.length)) : '0') },
        { key: 'charges', label: 'Charges', num: true, cover: 'visited.city_days.charges', cell: (r) => (r.charges.length ? h('a', { href: dayLink(r, 'charges') }, formatNumber(r.charges.length)) : '0') },
        { key: 'places', label: 'Places', cover: 'visited.city_days.places', cls: 'tv-wrap', cell: (r) => (r.places || []).join(', ') || '—' },
        { key: 'zone', label: 'Zone', cell: (r) => h('span', { class: 'tv-sub' }, zoneLabel(rowTz(r), dayStart(r.date, rowTz(r)))) },
      ].filter(Boolean),
      cls: 'tv-days-table',
    });
    await swapIn(daysBox, t, 'days', params({ city }));
  }

  // A list drawn again (a live refresh, another city) loads its rows before it takes the old one's place: put in
  // first, it was empty until they came, and everything under it moved up and back down. The first one goes in at
  // once (its loading view shows).
  // The same list with the same filters (a live refresh) loads its rows again in place instead, as many as it shows.
  const lists = {};
  async function swapIn(box, t, key, p) {
    lists[key] = { t, p: JSON.stringify(p) };
    if (!box.hasChildNodes()) { replace(box, t.el); return t.load(); }
    try { await t.load(); } finally { if (ctx.alive()) replace(box, t.el); }
  }
  async function again(box, key, p) {
    const l = lists[key];
    if (!l || l.p !== JSON.stringify(p) || !box.contains(l.t.el)) return false;
    await l.t.reload();
    return true;
  }

  /* ---------------------------------------------------------------- places visited */

  let placeKind = 'all';
  function drawPlaces() {
    if (data.places.error) { replace(placesBox, errorView(data.places.error, { retry: () => loadAll(), eyebrow: 'Could not load the places' })); return Promise.resolve(); }
    const kindChoice = choice({ label: 'Show', name: 'travel-place-kind', value: placeKind, options: [['all', 'Every place'], ['saved', 'Saved places'], ['spots', 'Other spots']],
      onchange: (v) => { placeKind = v; t.setRows(shown()); } });
    const shown = () => data.places.items.filter((r) => placeKind === 'all' || (placeKind === 'saved' ? !!r.place : !r.place));
    const isAdmin = ctx.me && ctx.me.role === 'admin';
    const t = dataTable(ctx, {
      rows: shown(), label: 'Places visited', itemName: ['place', 'places'], sort: { key: 'visits', dir: 'desc' },
      caption: data.places.capped ? `the first ${formatNumber(CAP)}; narrow the filters to see the rest` : 'addresses come from the offline address list, looked up from each stop’s position',
      empty: () => (data.places.items.length ? empty({ title: placeKind === 'saved' ? 'No saved place was visited.' : 'Every stop was at a saved place.', text: 'Choose “Every place” above.' }) : emptyStops()),
      columns: [
        { key: 'label', label: 'Place', th: true, sort: true, cover: ['locations.addresses.name', 'locations.geofences.name', 'locations.link.new_geofence', 'locations.link.edit_geofence'],
          cell: (r) => {
            const href = placeLink({ place: r.place, lat: r.lat, lon: r.lon });
            const act = !href ? null : r.place ? h('a', { class: 'tv-row-link', href }, isAdmin ? 'Edit the place' : 'See the place') : isAdmin ? h('a', { class: 'tv-row-link', href }, 'Save as a place') : null;
            return h('span', { class: r.place ? 'cell-main tv-has-pin' : 'cell-main' }, r.place ? h('span', { class: 'tv-saved', title: 'A saved place' }, icon('pin')) : null, r.label || 'Unnamed spot',
              h('span', { class: 'cell-sub' }, r.place ? 'saved place' : coordsText([r.lon, r.lat]), act ? [sep(), act] : null));
          } },
        { key: 'street', label: 'Street and neighbourhood', sort: true, cover: 'locations.addresses.neighbourhood', cls: 'tv-wrap',
          cell: (r) => h('span', null, r.street || '—', h('span', { class: 'cell-sub' }, r.neighbourhood || 'no neighbourhood named')) },
        { key: 'locality', label: 'City, region, country', sort: true, cover: ['locations.addresses.city', 'locations.addresses.state', 'locations.addresses.country'], cls: 'tv-wrap',
          cell: (r) => h('span', null, r.locality || '—', h('span', { class: 'cell-sub' }, [r.region, r.country || r.country_code].filter(Boolean).join(', ') || '—')) },
        { key: 'visits', label: 'Stops', num: true, sort: true, cell: (r) => q(r.visits, 'count') },
        { key: 'days', label: 'Days', num: true, sort: true, cell: (r) => q(r.days, 'count') },
        { key: 'dwell_ms', label: 'Time there', num: true, sort: true, cover: 'locations.geofences.time', cell: (r) => formatDuration(r.dwell_ms) },
        { key: 'last_ms', label: 'Last there', sort: true, cover: 'locations.addresses.updated_at', hint: 'Addresses are looked up live from the position, so they are always current; this is when a car was last there.', cell: (r) => dayIn(r.last_ms, lastTz(r)) },
        multi ? { key: 'cars', label: 'Cars', value: (r) => (r.cars || []).length, cell: (r) => carsCell(r.cars) } : null,
      ].filter(Boolean),
      csv: { filename: fileName('places'), columns: [
        { label: 'Place', value: (r) => r.label }, { label: 'Saved place id', value: (r) => r.place?.id }, { label: 'Street', value: (r) => r.street },
        { label: 'Neighbourhood', value: (r) => r.neighbourhood }, { label: 'City', value: (r) => r.locality }, { label: 'Region', value: (r) => r.region },
        { label: 'Country', value: (r) => r.country }, { label: 'Latitude', value: (r) => r.lat }, { label: 'Longitude', value: (r) => r.lon },
        { label: 'Stops', value: (r) => r.visits }, { label: 'Days', value: (r) => r.days }, { label: 'Time there (h)', value: (r) => Math.round(r.dwell_ms / 36000) / 100 },
        zonedColumn('First there', 'first_ms', firstTz), zonedColumn('Last there', 'last_ms', lastTz), { label: 'Cars', value: (r) => (r.cars || []).map(carName).join('; ') },
      ] },
      cls: 'tv-places-table',
    });
    replace(placesBox, h('div', { class: 'tv-table-tools', cover: 'locations.table.geofences' }, kindChoice,
      h('p', { class: 'field-hint tv-tools-hint' }, 'Saved places are the ones set up on the Places page; other spots are stops about 150 m apart that no saved place holds. Save one from its row.')), t.el);
    return t.load();
  }

  /* ---------------------------------------------------------------- recent stops */

  async function drawStops() {
    if (await again(stopsBox, 'stops', params({ sort: 'arrived_desc' }))) return;
    const t = dataTable(ctx, {
      path: 'visits', params: params({ sort: 'arrived_desc' }), limit: 25, itemName: ['stop', 'stops'], label: 'Recent stops',
      empty: () => emptyStops(),
      rowTitle: (r) => rowTitle(r, { energy: false }),
      columns: [
        { key: 'arrived', label: 'Arrived', th: true, cover: 'locations.last_visited.date', cell: (r) => dayTime(r.arrived_ms, rowTz(r)) },
        { key: 'left', label: 'Left', cell: (r) => (r.left_ms == null ? h('span', { class: 'tv-now' }, 'still there') : dayTime(r.left_ms, rowTz(r))) },
        { key: 'dwell', label: 'Time there', num: true, cell: (r) => h('span', null, formatDuration((r.left_ms ?? now) - r.arrived_ms),
          r.unknown_ms ? h('span', { class: 'cell-sub tv-unknown' }, `${formatDuration(r.unknown_ms)} not recorded`) : null) },
        { key: 'place', label: 'Place', cover: 'locations.last_visited.address', cls: 'tv-wrap',
          cell: (r) => { const href = placeLink(r); return h('span', { class: 'cell-main' }, href ? h('a', { href, title: r.place ? 'Open the saved place' : 'Save this spot as a place' }, r.label || 'Unnamed spot') : (r.label || 'Unnamed spot'),
            r.place && r.address?.short ? h('span', { class: 'cell-sub' }, r.address.short) : null); } },
        { key: 'city', label: 'City', cover: 'locations.last_visited.city', cell: (r) => (r.address?.locality || r.address?.neighbourhood || '—') },
        { key: 'country', label: 'Country', cell: (r) => r.address?.country_code || '—' },
        multi ? { key: 'car', label: 'Car', cell: (r) => carsCell([r.car]) } : null,
        { key: 'charges', label: 'Charged', num: true, cell: (r) => ((r.charges || []).length ? tn(r.charges.length, '{n} charge', '{n} charges') : '—') },
      ].filter(Boolean),
      cls: 'tv-stops-table',
    });
    return swapIn(stopsBox, t, 'stops', params({ sort: 'arrived_desc' }));
  }

  /* ---------------------------------------------------------------- the map */

  const trackRows = () => tracks.flatMap((d) => d.coords.map((c, i) => ({ car: d.car, drive: d.drive, t: d.t[i], lat: c[1], lon: c[0], acc: d.acc ? d.acc[i] : null })));

  // One GeoJSON source per car colour; a reload replaces the lines with setData.
  let mapLoaded = false;
  const routeSources = new Set();
  function drawRoutes() {
    if (!m || !mapLoaded) return;
    const byCar = new Map();
    for (const d of tracks) { const k = colourOf.get(d.car) ?? 'x'; if (!byCar.has(k)) byCar.set(k, []); byCar.get(k).push(d); }
    const fc = (list) => ({ type: 'FeatureCollection', features: list.map((d) => ({ type: 'Feature', properties: { drive: d.drive }, geometry: { type: 'LineString', coordinates: d.coords } })) });
    for (const [k, list] of byCar) {
      const sid = `routes-${k}`;
      if (routeSources.has(sid)) { m.map.getSource(sid).setData(fc(list)); continue; }
      routeSources.add(sid);
      m.map.addSource(sid, { type: 'geojson', data: fc(list) });
      m.addLine({ id: `${sid}-casing`, source: sid, paint: { 'line-opacity': 0.85 } }, 5, '--paper');
      m.addLine({ id: `${sid}-line`, source: sid, paint: { 'line-opacity': 0.9 } }, 2.4, k === 'x' ? '--ink-3' : CAR_TOKENS[k]);
    }
    const used = new Set([...byCar.keys()].map((k) => `routes-${k}`));
    for (const sid of routeSources) if (!used.has(sid)) m.map.getSource(sid).setData(fc([]));
  }

  const markRows = () => (markMode === 'cities' ? data.cities.items : data.places.items).filter((r) => r.lat != null && r.lon != null);
  let m = null;
  let marks = [];
  let framed = false;
  function drawLegend() {
    const cars = viewCars.filter((cid) => tracks.some((d) => d.car === cid));
    replace(legend,
      h('h3', { class: 'tv-sub-title' }, 'How to read the map'),
      h('ul', { class: 'tv-key' },
        h('li', { cover: 'visited.map.route' }, h('span', { class: 'tv-key-line' }), h('span', null, 'Lines: every drive’s route in the period, one line per drive', cars.length > 1 ? ', in each car’s colour' : '', '.')),
        cars.length > 1 ? cars.slice(0, CAR_TOKENS.length).map((cid) => h('li', { class: 'tv-key-car' }, swatch(cid), carName(cid))) : null,
        cars.length > CAR_TOKENS.length ? h('li', { class: 'tv-key-car' }, swatch('x'), `${formatNumber(cars.length - CAR_TOKENS.length)} more cars`) : null,
        h('li', { cover: 'visited.map.city_markers' }, h('span', { class: 'tv-key-dot' }), h('span', null, markMode === 'cities' ? 'Rings: one per city with a stop; bigger means more days there.' : 'Rings: one per place; filled ones are saved places.')),
        h('li', null, h('span', { class: 'tv-key-hint' }, icon('info')), h('span', null, markMode === 'cities' ? 'Point at a ring, or move to it with Tab, to read its figures; choose it to list the days in that city.' : 'Point at a ring to read its figures; choose it to open or save the place.'))));
  }
  function idleReadout() {
    const s = data.summary;
    replace(readout, h('span', { class: 'readout-idle' }, s ? `${tn(markRows().length, markMode === 'cities' ? '{n} city' : '{n} place', markMode === 'cities' ? '{n} cities' : '{n} places')} on the map. Point at one for its figures.` : ''));
  }
  function showMark(r) {
    replace(readout, h('div', { class: 'tv-detail' },
      h('p', { class: 'tv-detail-name' }, r.label || 'Unnamed spot'),
      h('p', { class: 'tv-detail-where' }, [markMode === 'places' ? r.locality : null, r.region, r.country].filter(Boolean).join(', ') || coordsText([r.lon, r.lat])),
      h('dl', { class: 'tv-detail-list' },
        h('dt', null, 'Days'), h('dd', null, formatNumber(r.days)),
        h('dt', null, 'Stops'), h('dd', null, formatNumber(r.visits)),
        markMode === 'cities' ? [h('dt', null, 'Places'), h('dd', null, formatNumber(r.places))] : null,
        h('dt', null, 'Time there'), h('dd', null, formatDuration(r.dwell_ms)),
        h('dt', null, 'First'), h('dd', null, dayIn(r.first_ms, firstTz(r))),
        h('dt', null, 'Last'), h('dd', null, dayIn(r.last_ms, lastTz(r))),
        multi ? [h('dt', null, 'Cars'), h('dd', null, (r.cars || []).map(carName).join(', '))] : null,
        h('dt', null, 'Position'), h('dd', null, coordsText([r.lon, r.lat]))),
      h('p', { class: 'tv-detail-act' }, markMode === 'cities' ? 'Choose the ring to list its days.' : r.place ? 'Choose the ring to open the place.' : 'Choose the ring to save this spot as a place.')));
  }
  function drawMarks(refit = false) {
    drawLegend();
    if (!m) return;
    for (const mk of marks) mk.remove();
    marks = [];
    const rows = markRows().slice(0, MAX_MARKS);
    const max = Math.max(1, ...rows.map((r) => r.days || 0));
    for (const r of rows) {
      const size = 0.75 + 1.25 * Math.sqrt((r.days || 0) / max);
      const el = h('button', { type: 'button', class: ['tv-mark', { 'is-saved': markMode === 'places' && !!r.place, 'is-on': markMode === 'cities' && r.key === city }], vars: { '--d': `${size.toFixed(3)}rem` },
        'aria-label': `${r.label || 'Unnamed spot'}: ${tn(r.days, '{n} day', '{n} days')}, ${tn(r.visits, '{n} stop', '{n} stops')}` });
      el.addEventListener('mouseenter', () => showMark(r));
      el.addEventListener('focus', () => showMark(r));
      el.addEventListener('mouseleave', () => idleReadout());
      el.addEventListener('blur', () => idleReadout());
      el.addEventListener('click', () => {
        if (markMode === 'cities') { pickCity(r.key); drawMarks(); return; }
        const href = placeLink({ place: r.place, lat: r.lat, lon: r.lon });
        if (href) ctx.go(href);
      });
      marks.push(new m.ml.Marker({ element: el, anchor: 'center' }).setLngLat([r.lon, r.lat]).addTo(m.map));
    }
    idleReadout();
    const capped = markRows().length > MAX_MARKS;
    const lost = (markMode === 'cities' ? data.cities : data.places).error;
    replace(mapNote, tracksNote ? `${tracksNote}. ` : null, lost ? `The ${markMode} could not be loaded, so they are not marked; use Load again. ` : null, capped ? `The ${formatNumber(MAX_MARKS)} ${markMode} with the most stops are marked. ` : null,
      m.source ? (m.source.offline ? 'Map tiles from this server (offline).' : 'Map tiles from the online tile server the owner turned on.') : null);
    if (refit || !framed) {
      const pts = [...rows.map((r) => [r.lon, r.lat]), ...(f.q || f.place || f.country || f.region ? [] : tracks.flatMap((d) => { const c = dropJumps(d.coords, d.t); return c.length ? [c[0], c[c.length - 1]] : []; }))]; // each drive's ends, without stray fixes
      if (pts.length) { framed = true; m.frame(pts.length === 1 ? [[pts[0][0] - 0.02, pts[0][1] - 0.02], [pts[0][0] + 0.02, pts[0][1] + 0.02]] : boundsOf(pts)); }
    }
  }

  let rowsTimer = 0;
  ctx.events(['rows'], (ev) => {
    const d = ev.data || {};
    if (d.car && !viewCars.includes(d.car)) return;
    if (d.kinds && !d.kinds.some((k) => ['visits', 'drives', 'charges'].includes(k))) return;
    clearTimeout(rowsTimer);
    rowsTimer = setTimeout(() => { if (ctx.alive()) loadAll().catch(() => {}); }, 2000);
  });
  ctx.onCleanup(() => clearTimeout(rowsTimer));

  await loadAll(true);
  if (!ctx.alive()) return {};

  const allPts = [...tracks.flatMap((d) => d.coords), ...markRows().map((r) => [r.lon, r.lat])];
  if (allPts.length) {
    m = await makeMap(ctx, mapBox, { bounds: trackBounds(tracks.map((d) => ({ coords: d.coords, t_ms: d.t })), markRows().map((r) => [r.lon, r.lat])) || boundsOf(allPts), padding: 40, maxZoom: 14, fullscreen: true });
    if (!ctx.alive()) return {};
    if (m) {
      await m.loaded;
      if (!ctx.alive()) return {};
      cover(mapBox, 'visited.map.base_map');
      mapLoaded = true;
      drawRoutes();
      framed = true;
      drawMarks();
    }
  } else {
    mapBox.classList.add('is-empty');
    cover(mapBox, 'visited.map.base_map');
    replace(mapBox, h('p', { class: 'map-note' }, 'No drive and no stop in this period, so there is nothing to draw.'));
    replace(mapNote, tracksNote);
    drawLegend();
    idleReadout();
  }
  if (m) await Promise.race([m.ready, new Promise((r) => setTimeout(r, 5000))]);
  if (!ctx.alive()) return {};
  ctx.ready();
  return {};
}

/* ------------------------------------------------------------------ the car pick list */

function carPicker(ctx, { fleet, chosen, onapply }) {
  const sel = new Set(chosen);
  let soldOpen = false;
  const trigger = h('button', { type: 'button', class: 'field-select tv-pick-trigger', id: 'travel-cars', disabled: fleet.length < 2 });
  const search = h('input', { class: 'field-input tv-pick-search', type: 'search', placeholder: 'Find a car', 'aria-label': 'Find a car', autocomplete: 'off' });
  const list = h('ul', { class: 'tv-pick-list' });
  const more = h('p', { class: 'tv-pick-more' });
  const name = (cid) => (fleet.find((c) => c.id === cid) || {}).name || cid;
  const text = () => {
    const n = [...sel];
    trigger.textContent = fleet.length < 2 ? (fleet[0] ? fleet[0].name : 'No car') : !n.length ? 'Every car' : n.length === 1 ? name(n[0]) : `${name(n[0])} and ${formatNumber(n.length - 1)} more`;
  };
  function draw() {
    const s = search.value.trim().toLowerCase();
    const found = fleet.filter((c) => !s || String(c.name || '').toLowerCase().includes(s));
    found.sort((a, b) => (sel.has(b.id) - sel.has(a.id)) || compareText(String(a.name || ''), String(b.name || '')));
    // Sold cars wait behind their own toggle unless ticked or searched for.
    const { rows, cut } = soldPickRows(found, {
      chosen: (c) => sel.has(c.id), searching: !!s, open: soldOpen, ontoggle: () => { soldOpen = !soldOpen; draw(); }, max: 60,
      row: (c) => {
        const box = h('input', { type: 'checkbox', id: `tv-car-${c.id}`, checked: sel.has(c.id) });
        box.addEventListener('change', () => { if (box.checked) sel.add(c.id); else sel.delete(c.id); text(); });
        return h('li', null, h('label', { class: 'tv-pick-item', for: `tv-car-${c.id}` }, box, h('span', { class: 'tv-pick-name' }, c.name || c.id), h('span', { class: 'tv-pick-sub' }, [[c.model, c.year].filter(Boolean).join(' '), state.isSold(c) ? 'Sold' : null].filter(Boolean).join(' · '))));
      },
    });
    replace(list, rows);
    more.textContent = !found.length ? 'No car has that name.' : cut ? `${formatNumber(cut)} more; type to narrow the list.` : '';
  }
  search.addEventListener('input', draw);
  const panel = h('div', { class: 'pop tv-pick-pop', id: 'travel-cars-menu', role: 'group', 'aria-label': 'Cars', tabindex: '-1' },
    fleet.length > 6 ? search : null, list, more,
    h('div', { class: 'tv-pick-foot' },
      button('Every car', { cls: 'button-small', onclick: () => { sel.clear(); pop.close({ refocus: true }); onapply([]); } }),
      button('Show these cars', { kind: 'primary', cls: 'button-small', onclick: () => { pop.close({ refocus: true }); onapply([...sel]); } })));
  const wrap = h('div', { class: 'pop-wrap tv-pick-wrap' }, trigger, panel);
  const pop = popover(trigger, panel, { onOpen: draw, focus: '.tv-pick-search' });
  text();
  return wrap;
}
