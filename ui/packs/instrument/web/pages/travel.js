// Travel (/travel?cars=all|ids): where the cars have been, for one car, several or all of them.
// One definition of a stop (a visit: from arriving to leaving) feeds every section, so the counts, the
// map's markers and the lists can never disagree:
// - filters: the shared period, the cars, text, a saved place, a country, a region, and the city whose
//   days are listed (all in the address, so a link or a reload shows the same page);
// - summary (GET /visits/summary): places, cities, regions, countries, days out, first and last visit,
//   unnamed stops, and the driving and charging of the same cars in the same period;
// - map: every drive's route from GET /tracks (one line per drive, coloured per car) and one marker per
//   city or per place (GET /visits/cities, /visits/places) with its details on hover;
// - countries, cities and regions (GET /visits/countries, /visits/cities, /visits/regions): time spent,
//   days, first and last visit, rankings with their counts, and a city picked for its days;
// - days in a city (GET /visits/days?city=): first and last time seen, drives, charges and places;
// - places visited (GET /visits/places) with links to save or edit a place, and the recent stops
//   (GET /visits). Days are counted in each car's own time zone.
// Live: a drive, charge or stop that starts or ends ("rows" events) builds the page again.

import * as state from '../shared/state.js';
import { h, replace, cssVar } from '../shared/dom.js';
import { downloadCsv, csvFilename, column, timeColumn } from '../shared/csv.js';
import { format, formatMoney } from '../shared/units.js';
import { formatNumber, compareText } from '../shared/i18n.js';
import { startOf, addPeriod } from '../shared/time.js';
import { icon } from '../core/icons.js';
import { pageHead, panel, metric, csvButton, tag } from '../core/page.js';
import { dataTable } from '../core/table.js';
import { emptyState } from '../core/states.js';
import { date, dateTime, time, dur, tzName, DASH, spotText } from '../core/fmt.js';
import { createMap, mapFallback, boundsOf, padClear } from '../core/map.js';
import { openPeriodPicker } from '../core/period.js';
import { placePopover } from '../core/popover.js';
import { rpx } from '../core/display.js';

export const id = 'travel';
export const css = true;

// line colours for cars, in order (the legend names each car)
// (never --warm or --c-charge: the city and place markers are warm, and a car line must not look like them)
const CAR_COLORS = ['--live', '--c-power', '--c-soc', '--c-regen', '--c-elev'];
const CAP = 5000; // grouped rows loaded for the map, the rankings and the local tables
const RANKS = [
  ['days', 'Days', 'Calendar days with at least one stop there (a day counts once per car)'],
  ['visits', 'Stops', 'Stops there: from arriving to leaving'],
  ['places', 'Places', 'Different places there: saved places, or spots about 150 m apart'],
  ['dwell_ms', 'Time', 'Time spent parked there, inside the period'],
];
const iso = (ms) => (ms == null ? '' : new Date(ms).toISOString());
const plural = (n, one, many = `${one}s`) => `${formatNumber(n)} ${n === 1 ? one : many}`;
// the text search had focus when it reloaded the page: the new box takes the focus back
let refocusSearch = false;

export async function mount(ctx) {
  const { period } = ctx;
  const q = ctx.query;
  const f = { q: q.q || '', place: q.place || '', country: (q.country || '').toUpperCase(), region: q.region || '' };
  let city = q.city || null;
  let cityPicked = !!q.city; // picked by the viewer (kept in the address), else the city with the most days

  /* ---------- which cars */
  let fleet = [];
  try { fleet = (await ctx.api.page('cars', { sort: 'name' }, { limit: 200 })).items || []; } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  const carById = new Map(fleet.map((c) => [c.id, c]));
  const chosen = !q.cars || q.cars === 'all' ? 'all' : q.cars.split(',').filter(Boolean);
  const viewCars = chosen === 'all' ? fleet.map((c) => c.id) : chosen.filter((c) => carById.has(c));
  const multi = viewCars.length > 1;
  const carsParam = chosen === 'all' ? 'all' : viewCars.join(',');
  const carName = (cid) => carById.get(cid)?.name || cid;
  const carTz = (cid) => carById.get(cid)?.time_zone || ctx.tz;
  const colorOf = new Map(viewCars.map((cid, i) => [cid, CAR_COLORS[i % CAR_COLORS.length]]));
  const rowTz = (r) => r.time_zone || (r.car ? carTz(r.car) : r.cars?.length === 1 ? carTz(r.cars[0]) : ctx.tz);
  const scopeName = chosen === 'all' ? (fleet.length === 1 ? fleet[0].name : `All ${formatNumber(fleet.length)} cars`)
    : viewCars.length === 1 ? carName(viewCars[0]) : `${formatNumber(viewCars.length)} cars`;
  const fileBase = chosen === 'all' ? 'all-cars' : viewCars.map(carName).join('-');
  const csvOf = (name, rows, cols, cv) => csvButton({ cover: cv, onClick: () => downloadCsv(csvFilename(fileBase, name), typeof rows === 'function' ? rows() : rows, cols) });

  const root = h('div', { class: 'tv-page', cover: ['shots.locations', 'shots.visited'] });
  replace(ctx.view, root);
  if (!viewCars.length) {
    root.append(pageHead({ title: 'Travel', sub: [period.label] }), panel({}, emptyState({ icon: 'globe', title: fleet.length ? 'None of these cars is yours to see' : 'There is no car yet',
      text: fleet.length ? 'The cars in this address are unknown or hidden from your account. Pick other cars.' : 'Travel lists where the cars have been once a CAN Bridge has sent its first logs.',
      action: fleet.length ? h('a', { class: 'btn', href: state.buildUrl('travel', { period: state.current().period }) }, 'Every car') : null })));
    ctx.ready();
    return;
  }

  // the filters every list takes (the day list adds the city)
  const params = (patch = {}) => {
    const p = { cars: carsParam, from_ms: ctx.params.from_ms, to_ms: ctx.params.to_ms, q: f.q || null, place: f.place || null, country: f.country || null, region: f.region || null, ...patch };
    for (const k of Object.keys(p)) if (p[k] == null || p[k] === '') delete p[k];
    return p;
  };
  const queryOf = () => ({ cars: chosen === 'all' ? null : viewCars.join(','), q: f.q || null, place: f.place || null, country: f.country || null, region: f.region || null, city: cityPicked ? city : null });
  const apply = () => { refocusSearch = document.activeElement === textIn; ctx.setQuery(queryOf()); ctx.reload(); };

  async function all(path, p) {
    const out = [];
    let after = null;
    do {
      const pg = await ctx.api.page(path, p, { limit: 500, after: after ?? undefined });
      out.push(...(pg.items || []));
      after = pg.next;
    } while (after && out.length < CAP);
    return { items: out, more: !!after };
  }

  /* ---------- load (everything at once) */
  const narrowed = f.country || f.region;
  let summary, countries, regions, cities, placesG, savedPlaces, countryOpts, regionOpts;
  try {
    [summary, countries, regions, cities, placesG, savedPlaces, countryOpts, regionOpts] = await Promise.all([
      ctx.api.get('visits/summary', params()),
      all('visits/countries', params()),
      all('visits/regions', params()),
      all('visits/cities', params()),
      all('visits/places', params()),
      ctx.api.page('places', { sort: 'name' }, { limit: 200 }).then((x) => x.items || []).catch(() => []),
      narrowed ? all('visits/countries', params({ country: null, region: null })) : null,
      f.region ? all('visits/regions', params({ region: null })) : null,
    ]);
  } catch (e) { if (ctx.alive()) ctx.fail(e); return; }
  if (!ctx.alive()) return;
  countryOpts ||= countries;
  regionOpts ||= regions;
  const byDays = (a, b) => (b.days - a.days) || (b.visits - a.visits) || compareText(a.label || '', b.label || '');
  cities.items.sort(byDays);
  const topCity = () => cities.items[0]?.key || null;
  if (city && !cities.items.some((c) => c.key === city)) {
    // the filters no longer reach the picked city: back to the one with the most days
    city = null; cityPicked = false; ctx.setQuery({ city: null });
  }
  if (!city) city = topCity();
  const cityRow = () => cities.items.find((c) => c.key === city) || null;
  const cityName = (key) => { const c = cities.items.find((x) => x.key === key); const last = (key || '').split('/').pop(); return (c ? c.label || c.locality : null) || (last && last !== '-' ? last : null) || 'no known town'; };
  const citiesPer = new Map();
  for (const c of cities.items) citiesPer.set(c.country_code || '-', (citiesPer.get(c.country_code || '-') || 0) + 1);

  /* ---------- head */
  const refreshBtn = h('button', { class: 'btn', type: 'button', onclick: () => ctx.reload(), cover: ['locations.control.refresh', 'visited.control.refresh'],
    title: 'Load every figure, the map and the lists again. The page also does this by itself when a drive, a charge or a stop starts or ends.' },
  icon('reset', 'ico--sm'), 'Load again');
  root.append(pageHead({
    eyebrow: scopeName, title: 'Travel',
    sub: [period.label, multi ? `days in each car’s own time zone, dates in ${tzName(ctx.tz)}` : `days in ${tzName(ctx.tz)}`, plural(summary.visits ?? 0, 'stop')],
    actions: [h('span', { class: 'tv-live faint' }, icon('live', 'ico--sm'), 'Updates by itself'), refreshBtn],
  }));

  /* ---------- filters */
  const pops = [];
  ctx.onCleanup(() => { for (const p of pops) { if (p.matches(':popover-open')) p.hidePopover(); p.remove(); } });
  function carPicker() {
    const sel = new Set(chosen === 'all' ? [] : viewCars);
    const text = h('span', { class: 'tv-pick-text' });
    const btn = h('button', { class: 'btn tv-pick-btn', type: 'button', 'aria-haspopup': 'dialog', 'aria-expanded': 'false', title: 'Which cars this page covers',
      cover: ['locations.filter.car', 'visited.filter.car'] }, icon('car', 'ico--sm'), text, icon('chev-d', 'ico--sm'));
    const find = h('input', { class: 'input', type: 'search', placeholder: 'Find a car', 'aria-label': 'Find a car' });
    const list = h('div', { class: 'tv-pick-list', role: 'group', 'aria-label': 'Cars' });
    const pop = h('div', { class: 'popover tv-pick-pop', popover: 'auto', role: 'dialog', 'aria-label': 'Cars' },
      h('p', { class: 'tv-pick-hint' }, 'Tick one car, several, or none for every car. The figures, the map and every list then cover the cars you picked together, with a car column, instead of one copy of the page per car.'),
      fleet.length > 8 ? find : null, list,
      h('div', { class: 'tv-pick-actions' },
        h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: () => { sel.clear(); paint(); } }, 'Every car'),
        h('button', { class: 'btn btn--sm btn--primary', type: 'button', onclick: () => {
          pop.hidePopover();
          const ids = [...sel];
          const everyCar = !ids.length || ids.length === fleet.length;
          ctx.navigate(state.buildUrl('travel', { period: state.current().period, query: { ...queryOf(), city: null, cars: everyCar ? null : ids.join(',') } }));
        } }, 'Show')));
    document.body.append(pop); // a popover must be in the document to open; the cleanup removes it
    const paintText = () => {
      const names = fleet.filter((c) => sel.has(c.id)).map((c) => c.name);
      text.textContent = names.length ? (names.length > 2 ? `${formatNumber(names.length)} cars` : names.join(', ')) : fleet.length === 1 ? fleet[0].name : 'Every car';
      btn.classList.toggle('is-on', names.length > 0);
    };
    const paint = () => {
      const needle = find.value.trim().toLowerCase();
      const shown = fleet.filter((c) => !needle || c.name.toLowerCase().includes(needle));
      replace(list, shown.length ? shown.map((c) => h('label', { class: 'check' },
        h('input', { type: 'checkbox', checked: sel.has(c.id), onchange: (e) => { if (e.target.checked) sel.add(c.id); else sel.delete(c.id); paintText(); } }),
        h('span', null, c.name), h('span', { class: 'faint tv-pick-sub' }, [c.model, c.year].filter(Boolean).join(' · '))))
        : h('p', { class: 'note' }, 'No car is called that.'));
      paintText();
    };
    find.addEventListener('input', paint);
    btn.addEventListener('click', () => {
      if (pop.matches(':popover-open')) { pop.hidePopover(); return; }
      pop.showPopover(); placePopover(pop, btn, { align: 'start' });
    });
    pop.addEventListener('toggle', (e) => btn.setAttribute('aria-expanded', String(e.newState === 'open')));
    paint();
    pops.push(pop);
    return btn;
  }

  const periodBtn = h('button', { class: 'btn', type: 'button', onclick: (e) => openPeriodPicker(e.currentTarget), cover: ['locations.filter.time_range', 'visited.filter.time_range'],
    title: 'The period of the whole page (the same picker as in the top bar). A stop belongs to it by its arrival; time spent is cut at the edges.' },
  icon('calendar', 'ico--sm'), period.label, icon('chev-d', 'ico--sm'));
  const textIn = h('input', { class: 'input', type: 'search', value: f.q, placeholder: 'Place, street, town', 'aria-label': 'Text in the place or address' });
  // applied on Enter, on leaving the box or on clearing it, never mid-typing, so no keystroke is lost to a reload
  const searchNow = () => { const v = textIn.value.trim(); if (v !== f.q) { f.q = v; apply(); } };
  textIn.addEventListener('change', searchNow);
  textIn.addEventListener('search', searchNow);
  // places that share a name are told apart by where they are (else by their id)
  const sameName = (p) => savedPlaces.filter((x) => x.name === p.name);
  const whereOf = (p) => [p.address?.neighbourhood, p.address?.locality || p.address?.short].filter(Boolean).join(', ');
  const placeOptText = (p) => {
    const twins = sameName(p);
    if (twins.length < 2) return p.name;
    const w = whereOf(p);
    return twins.some((x) => x !== p && whereOf(x) === w) || !w ? `${p.name} (${w ? `${w}, ` : ''}${p.id})` : `${p.name} (${w})`;
  };
  const placeSel = h('select', { class: 'select', 'aria-label': 'Saved place', onchange: (e) => { f.place = e.target.value; apply(); } },
    h('option', { value: '' }, 'Any place'),
    savedPlaces.map((p) => h('option', { value: p.id, selected: f.place === p.id }, placeOptText(p))),
    f.place && !savedPlaces.some((p) => p.id === f.place) ? h('option', { value: f.place, selected: true }, `Place ${f.place}`) : null);
  const countrySel = h('select', { class: 'select', 'aria-label': 'Country', onchange: (e) => { f.country = e.target.value; f.region = ''; apply(); } },
    h('option', { value: '' }, 'Every country'),
    countryOpts.items.filter((c) => c.country_code).map((c) => h('option', { value: c.country_code, selected: f.country === c.country_code }, c.label || c.country_code)));
  const regionSel = h('select', { class: 'select', 'aria-label': 'Region', onchange: (e) => {
    const r = regionOpts.items.find((x) => x.key === e.target.value);
    f.region = r ? r.region || '' : ''; if (r?.country_code) f.country = r.country_code; apply();
  } },
  h('option', { value: '' }, 'Every region'),
  regionOpts.items.filter((r) => r.region).map((r) => h('option', { value: r.key, selected: !!f.region && f.region.toLowerCase() === r.region.toLowerCase() && (!f.country || f.country === r.country_code) },
    regionOpts.items.some((x) => x !== r && x.region === r.region) ? `${r.region} (${r.country_code})` : r.region)));
  const cityChip = h('span', { class: 'tv-city-chip', cover: 'visited.state.selected_city', title: 'The city whose days are listed under “Days in a city”. Pick another on the map or in the city list; it stays in the address.' });
  const paintChip = () => replace(cityChip,
    icon('pin', 'ico--sm'), h('span', { class: 'faint' }, 'Days of'), h('b', null, city ? cityName(city) : 'no city'),
    cityPicked ? h('button', { class: 'btn btn--sm btn--ghost btn--icon', type: 'button', 'aria-label': 'Back to the city with the most days', title: 'Back to the city with the most days',
      onclick: () => setCity(topCity(), false) }, icon('x', 'ico--sm')) : city ? h('span', { class: 'faint' }, '(most days)') : null);
  paintChip();
  const anyFilter = f.q || f.place || f.country || f.region;
  root.append(h('section', { class: 'panel panel-pad tv-filters', 'aria-label': 'What the page covers' },
    h('div', { class: 'filter-bar' },
      periodBtn, carPicker(),
      h('label', { class: 'field field--search', cover: 'locations.filter.address', title: 'Keeps stops whose saved place name, street, neighbourhood, town, region or country contains this text. Press Enter (or leave the box) to apply it.' }, icon('search'), textIn),
      h('span', { class: 'filter' }, icon('pin', 'ico--sm'), placeSel),
      h('span', { class: 'filter' }, icon('globe', 'ico--sm'), countrySel, regionSel),
      cityChip,
      anyFilter ? h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: () => { f.q = f.place = f.country = f.region = ''; apply(); } }, icon('x', 'ico--sm'), 'Clear filters') : null),
    h('p', { class: 'note' }, icon('info', 'ico--sm'),
      'Text, place, country and region narrow every figure, marker and list below. The route lines on the map follow only the cars and the period.')));
  if (refocusSearch) { refocusSearch = false; textIn.focus(); const n = textIn.value.length; textIn.setSelectionRange(n, n); }

  /* ---------- the cars (one colour each, as on the map) */
  const carStrip = h('div', { class: 'tv-cars', cover: 'visited.section.car_header', 'aria-label': 'Cars on this page' },
    h('span', { class: 'tv-cars-label' }, multi ? 'Cars together on this page' : 'Car'),
    viewCars.slice(0, 12).map((cid) => h('a', { class: 'tv-car', href: state.buildUrl('car', { car: cid, period: state.current().period }), vars: { '--cc': cssVar(colorOf.get(cid)) }, title: `${carName(cid)}: days counted in ${carTz(cid).replace(/_/g, ' ')}` },
      h('i', { 'aria-hidden': 'true' }), h('span', null, carName(cid)), h('span', { class: 'faint' }, tzName(carTz(cid))))),
    viewCars.length > 12 ? h('span', { class: 'faint' }, `and ${formatNumber(viewCars.length - 12)} more`) : null);
  root.append(carStrip);

  /* ---------- summary and map, side by side */
  const s = summary;
  const money = (s.cost_by_currency || []).filter((x) => x.amount != null);
  const moneyEl = money.length ? h('span', { class: 'tv-money' }, money.map((x) => h('span', null, formatMoney(x.amount, x.currency), money.length > 1 ? h('small', null, ` ${x.currency}`) : null))) : null;
  const group = (title, items) => h('div', { class: 'tv-group' }, h('h3', { class: 'tv-group-title' }, title), h('div', { class: 'tv-fig-grid' }, items));
  const sumTz = multi ? ctx.tz : carTz(viewCars[0]);
  const summaryPanel = panel({ title: 'Summary', icon: 'stats', cls: 'tv-summary',
    side: csvOf('travel-summary', [s], [column('Stops', 'visits'), column('Places', 'places'), column('Cities', 'cities'), column('Regions', 'regions'), column('Countries', 'countries'),
      column('Days out', 'days'), timeColumn('First visit', 'first_ms', sumTz), timeColumn('Last visit', 'last_ms', sumTz), column('Unnamed stops', 'unnamed_stops'),
      column('Distance', 'distance_km', 'distance'), column('Energy added', 'energy_added_kwh', 'energy'), column('Energy drawn', 'energy_grid_kwh', 'energy'),
      column('Charging efficiency (%)', (r) => r.efficiency?.value), column('Cost', (r) => (r.cost_by_currency || []).map((x) => `${x.amount} ${x.currency}`).join('; '))]) },
  group('Where', [
    metric('Places', s.places, 'count', { explain: 'Saved places, plus spots about 150 m apart: one spot never counts twice', cover: 'locations.stat.addresses' }),
    metric('Cities', s.cities, 'count', { explain: 'Towns by name, region and country, so two towns of one name stay apart', cover: ['locations.stat.cities', 'visited.stat.cities'] }),
    metric('Regions', s.regions, 'count', { explain: 'States, provinces or regions', cover: 'locations.stat.states' }),
    metric('Countries', s.countries, 'count', { explain: 'From the offline address of each stop', cover: ['locations.stat.countries', 'visited.stat.countries'] }),
  ]),
  group('When', [
    metric('Days out', s.days, 'count', { explain: 'Calendar days with a stop, in each car’s own time zone; a day counts once per car', cover: 'visited.stat.days' }),
    metric('First visit', s.first_ms != null ? date(s.first_ms, sumTz, { year: true }) : null, null, { explain: s.first_ms != null ? `arrived ${time(s.first_ms, sumTz)}` : null, why: 'no stop in this period', cover: 'visited.stat.first_visit' }),
    metric('Last visit', s.last_ms != null ? date(s.last_ms, sumTz, { year: true }) : null, null, { explain: s.last_ms != null ? `left ${time(s.last_ms, sumTz)}, or still there` : null, why: 'no stop in this period', cover: 'visited.stat.last_visit' }),
    metric('Unnamed stops', s.unnamed_stops, 'count', { explain: s.unnamed_stops ? 'At no saved place: save one from “Places visited”' : 'Every stop was at a saved place' }),
  ]),
  group('Driving and charging', [
    metric('Distance', s.distance_km, 'distance', { explain: 'Drives that started in the period', cover: 'visited.stat.distance' }),
    metric('Energy added', s.energy_added_kwh, 'energy', { explain: 'Into the battery, measured at the pack', cover: 'visited.stat.energy_added' }),
    metric('Energy drawn', s.energy_grid_kwh, 'energy', { explain: s.efficiency?.charges_counted ? `From the charger, over ${plural(s.efficiency.charges_counted, 'charge')} that measured it` : 'No charge measured the charger side', cover: 'visited.stat.energy_drawn' }),
    metric('Charging efficiency', s.efficiency?.value ?? null, 'percent', { digits: 1, explain: 'Energy added ÷ energy drawn, over the charges that measured both', why: 'no charge measured both sides', cover: 'visited.stat.charging_efficiency' }),
    metric('Charging cost', moneyEl, null, { explain: `Per currency, never added across currencies${s.charges_without_cost ? `; ${plural(s.charges_without_cost, 'charge')} without a price` : ''}`, why: 'no charge with a price', cover: 'visited.stat.charging_cost' }),
  ]),
  h('p', { class: 'note tv-shared', cover: 'visited.text.shared_visits_help' }, icon('info', 'ico--sm'),
    'Every figure here, the markers on the map and the country, city and place lists count the same stops: a stop is the time from arriving to leaving, grouped by saved place, else by position. Distance, energy and cost are the drives and charges of the same cars in the same period.'));

  // the map
  const mapEl = h('div', { class: 'map tv-map' });
  const hoverCard = h('div', { class: 'glass-card tv-hover', hidden: true, cover: 'visited.map.marker_details', 'aria-live': 'polite' });
  let markMode = q.marks === 'places' ? 'places' : 'cities';
  const markSeg = h('span', { class: 'seg', role: 'group', 'aria-label': 'Markers' },
    [['cities', 'Cities'], ['places', 'Places']].map(([v, l]) => h('button', { class: 'seg-btn', type: 'button', 'aria-pressed': String(markMode === v), dataset: { v },
      onclick: () => { if (markMode === v) return; markMode = v; for (const b of markSeg.children) b.setAttribute('aria-pressed', String(b.dataset.v === v)); ctx.setQuery({ marks: v === 'cities' ? null : v }); paintMarks(); } }, l)));
  const legend = h('div', { class: 'glass-card tv-legend' },
    h('b', null, 'On the map'),
    viewCars.slice(0, 6).map((cid) => h('span', { vars: { '--cc': cssVar(colorOf.get(cid)) } }, h('i', { class: 'tv-line', 'aria-hidden': 'true' }), multi ? `${carName(cid)}’s drives` : 'Every drive')),
    h('span', null, h('i', { class: 'tv-dot', 'aria-hidden': 'true' }), h('span', { class: 'tv-mark-word' }, 'a city: bigger means more days')),
    markSeg);
  let tracks = null;
  const routeRead = h('span', { class: 'faint tv-route-read' });
  const mapPanel = h('section', { class: 'panel tv-map-panel', 'aria-label': 'Map', cover: ['visited.map', 'visited.map.base_map'] },
    h('div', { class: 'tv-map-head' }, h('h2', { class: 'panel-title' }, icon('globe'), 'Map'), routeRead,
      csvOf('travel-markers', () => (markMode === 'cities' ? cities.items : placesG.items), [column('Marker', 'label'), column('Key', 'key'), column('Region', 'region'), column('Country', 'country'),
        column('Latitude', 'lat'), column('Longitude', 'lon'), column('Stops', 'visits'), column('Days', 'days'), column('Time spent (h)', (r) => Math.round(r.dwell_ms / 36e3) / 100),
        timeColumn('First visit', 'first_ms', ctx.tz), timeColumn('Last visit', 'last_ms', ctx.tz)]),
      csvButton({ label: 'Route CSV', onClick: () => downloadCsv(csvFilename(fileBase, 'travel-route'), (tracks?.drives || []).flatMap((d) => d.t_ms.map((t, i) => ({ car: d.car, drive: d.drive, t, lat: d.lat[i], lon: d.lon[i], acc: d.accuracy_m?.[i] }))),
        [column('Car', (r) => carName(r.car)), column('Drive', 'drive'), column('Time', (r) => iso(r.t)), column('Latitude', 'lat'), column('Longitude', 'lon'), column('Accuracy (m)', 'acc')]) })),
    h('div', { class: 'tv-map-box', cover: ['visited.map.route', 'visited.map.city_markers'] }, mapEl,
      h('div', { class: 'map-overlay map-overlay--tl' }, legend),
      h('div', { class: 'map-overlay map-overlay--tr' }, hoverCard)),
    h('p', { class: 'note tv-map-help', cover: 'visited.text.map_help' }, icon('info', 'ico--sm'),
      'Lines are every drive of the period, from 1 Hz positions, one line per drive in its car’s colour. Dots are where the cars stopped: one per city, or per place. Hover a dot for its details; click a city to list its days below, or a place to open it in the place editor. Tiles come from Sentry Mate itself unless an admin turned on an online map.'));
  root.append(h('div', { class: 'tv-top' }, mapPanel, summaryPanel));

  /* ---------- countries, rankings */
  const cityCount = (c) => citiesPer.get(c.country_code || '-') || 0;
  // the city whose days are listed: picked by the viewer (kept in the address) or the default (most days)
  function setCity(key, picked) {
    city = key;
    cityPicked = picked && !!key;
    ctx.setQuery({ city: cityPicked ? key : null });
    paintDays();
    paintChip();
    for (const b of root.querySelectorAll('.tv-city-btn')) b.classList.toggle('is-chosen', b.dataset.key === city);
  }
  const pickCity = (key) => {
    setCity(key, true);
    daysSlot.scrollIntoView({ block: 'start', behavior: 'smooth' });
  };
  const filterCountry = (cc) => { f.country = cc; f.region = ''; apply(); };
  const datesCols = (tzOf) => [
    { key: 'first_ms', label: 'First visit', num: true, sort: true, render: (r) => date(r.first_ms, tzOf(r), { year: true }), csv: (r) => iso(r.first_ms) },
    { key: 'last_ms', label: 'Last visit', num: true, sort: true, render: (r) => date(r.last_ms, tzOf(r), { year: true }), csv: (r) => iso(r.last_ms) },
  ];
  const carsCol = { key: 'cars', label: 'Cars', render: (r) => (r.cars || []).map(carName).join(', '), csv: (r) => (r.cars || []).map(carName).join('; ') };
  const dwellCol = (cv) => ({ key: 'dwell_ms', label: 'Time spent', num: true, sort: true, cover: cv, title: 'Time parked there, inside the period', render: (r) => dur(r.dwell_ms), csv: column('Time spent (h)', (r) => Math.round(r.dwell_ms / 36e3) / 100) });
  const countryTable = dataTable(ctx, {
    title: 'Countries', icon: 'flag', rows: countries.items, csvName: 'travel-countries', cover: 'visited.table.countries', cls: 'tv-tbl',
    note: 'Click a country to narrow the whole page to it.',
    columns: [
      { key: 'label', label: 'Country', sort: true, cover: 'visited.countries.country', render: (r) => (r.country_code ? h('button', { class: 'link-btn', type: 'button', title: `Show only ${r.label}`, onclick: () => filterCountry(r.country_code) }, r.label) : h('span', { class: 'faint' }, 'No address')),
        csv: (r) => r.label },
      { key: 'cities', label: 'Cities', num: true, sort: true, value: cityCount, cover: 'visited.countries.cities', render: (r) => formatNumber(cityCount(r)), csv: cityCount },
      { key: 'visits', label: 'Stops', num: true, sort: true, render: (r) => formatNumber(r.visits) },
      { key: 'days', label: 'Days', num: true, sort: true, cover: 'visited.countries.days', render: (r) => formatNumber(r.days) },
      dwellCol(null),
      ...datesCols(rowTz).map((c, i) => ({ ...c, cover: i ? 'visited.countries.last_visit' : 'visited.countries.first_visit' })),
      ...(multi ? [carsCol] : []),
    ],
    sort: { key: 'days', dir: 'desc' },
    empty: () => emptyState({ icon: 'globe', title: 'No country', text: anyFilter ? 'No stop matches these filters in this period.' : 'No stop in this period.' }),
  });

  let rankBy = RANKS.some(([k]) => k === q.rank) ? q.rank : 'days';
  const rankSlot = h('div', { class: 'tv-ranks' });
  const rankSeg = h('span', { class: 'seg', role: 'group', 'aria-label': 'Rank by' },
    RANKS.map(([k, l, help]) => h('button', { class: 'seg-btn', type: 'button', title: help, 'aria-pressed': String(rankBy === k), dataset: { k },
      onclick: () => { rankBy = k; for (const b of rankSeg.children) b.setAttribute('aria-pressed', String(b.dataset.k === k)); ctx.setQuery({ rank: k === 'days' ? null : k }); paintRanks(); } }, l)));
  const rankText = (r) => (rankBy === 'dwell_ms' ? dur(r.dwell_ms) : formatNumber(r[rankBy]));
  function ranking(title, rows, tone, cv, onPick, csvName) {
    const top = [...rows].sort((a, b) => (b[rankBy] - a[rankBy]) || compareText(a.label || '', b.label || '')).slice(0, 10);
    const max = Math.max(1, ...top.map((r) => r[rankBy] || 0));
    return h('div', { class: 'tv-rank', dataset: { tone }, cover: cv },
      h('div', { class: 'tv-rank-head' }, h('h3', { class: 'tv-group-title' }, title),
        csvOf(csvName, () => [...rows].sort((a, b) => b[rankBy] - a[rankBy]), [column('Name', 'label'), column('Region', 'region'), column('Country', 'country'), column('Stops', 'visits'), column('Places', 'places'), column('Days', 'days'), column('Time spent (h)', (r) => Math.round(r.dwell_ms / 36e3) / 100)])),
      top.length ? h('ol', { class: 'tv-bars' }, top.map((r, i) => {
        const share = (r[rankBy] || 0) / max;
        return h('li', { dataset: { tier: share >= 2 / 3 ? '1' : share >= 1 / 3 ? '2' : '3' } },
          h('span', { class: 'tv-bar-rank' }, String(i + 1)),
          onPick ? h('button', { class: 'tv-bar-label link-btn', type: 'button', title: `List the days in ${r.label}`, onclick: () => onPick(r) }, r.label || DASH) : h('span', { class: 'tv-bar-label' }, r.label || DASH, r.country_code ? h('span', { class: 'faint' }, ` ${r.country_code}`) : null),
          h('span', { class: 'tv-bar', 'aria-hidden': 'true' }, h('i', { vars: { '--w': `${Math.max(2, share * 100).toFixed(1)}%` } })),
          h('span', { class: 'tv-bar-val' }, rankText(r)));
      })) : h('p', { class: 'note' }, 'Nothing to rank in this period.'));
  }
  function paintRanks() {
    replace(rankSlot,
      ranking('Top cities', cities.items, 'city', ['locations.chart.top_cities', 'locations.chart.rank_shading'], (r) => pickCity(r.key), 'travel-city-ranking'),
      ranking('Top regions', regions.items, 'region', 'locations.chart.top_regions', (r) => { f.region = r.region || ''; f.country = r.country_code || f.country; apply(); }, 'travel-region-ranking'));
  }
  paintRanks();
  const rankPanel = panel({ title: 'Cities and regions', icon: 'stats', cls: 'tv-rank-panel', side: rankSeg }, rankSlot,
    h('p', { class: 'note' }, 'The ten with the most, with the count printed on each bar. A bar is full colour from two thirds of the top one, lighter from a third, and faint below that, so the gap to the top shows at a glance. Click a city to list its days, a region to narrow the page to it.'));
  root.append(h('div', { class: 'tv-pair' }, countryTable.el, rankPanel));

  /* ---------- cities table, with its own column filters */
  const tf = { country: q.c_country || '', region: q.c_region || '', find: '' };
  const cityRows = () => cities.items.filter((c) => (!tf.country || c.country_code === tf.country) && (!tf.region || c.region === tf.region)
    && (!tf.find || (c.label || '').toLowerCase().includes(tf.find)));
  const cCountry = h('select', { class: 'select select--sm', 'aria-label': 'Cities in country' }, h('option', { value: '' }, 'Any country'),
    [...new Map(cities.items.filter((c) => c.country_code).map((c) => [c.country_code, c.country || c.country_code])).entries()].sort((a, b) => compareText(a[1], b[1]))
      .map(([cc, name]) => h('option', { value: cc, selected: tf.country === cc }, name)));
  const regionList = () => [...new Set(cities.items.filter((c) => c.region && (!tf.country || c.country_code === tf.country)).map((c) => c.region))].sort(compareText);
  const cRegion = h('select', { class: 'select select--sm', 'aria-label': 'Cities in region' });
  const paintRegionOpts = () => replace(cRegion, h('option', { value: '' }, 'Any region'), regionList().map((r) => h('option', { value: r, selected: tf.region === r }, r)));
  paintRegionOpts();
  const cFind = h('input', { class: 'input input--sm tv-find', type: 'search', placeholder: 'Find a city', 'aria-label': 'Find a city' });
  const cityTable = dataTable(ctx, {
    title: 'Cities', icon: 'pin', rows: cityRows(), csvName: 'travel-cities', cover: 'visited.table.cities', cls: 'tv-tbl',
    side: h('span', { class: 'tv-col-filters', cover: 'visited.cities.column_filter' }, cFind, cCountry, cRegion),
    note: cities.more ? `The first ${formatNumber(CAP)} cities by stops; narrow the period or the filters for the rest.` : 'A city is its name with its region and country. Click one to list its days.',
    columns: [
      { key: 'label', label: 'City', sort: true, cover: ['visited.cities.city', 'visited.link.city_days'],
        render: (r) => h('button', { class: ['link-btn', 'tv-city-btn', r.key === city && 'is-chosen'], dataset: { key: r.key }, type: 'button', title: `List the days in ${r.label}`, onclick: () => pickCity(r.key) }, r.label || DASH), csv: (r) => r.label },
      { key: 'region', label: 'Region', sort: true, cover: 'visited.cities.region', render: (r) => r.region || DASH },
      { key: 'country', label: 'Country', sort: true, cover: 'visited.cities.country', render: (r) => r.country || r.country_code || DASH },
      { key: 'visits', label: 'Stops', num: true, sort: true, render: (r) => formatNumber(r.visits) },
      { key: 'places', label: 'Places', num: true, sort: true, render: (r) => formatNumber(r.places) },
      { key: 'days', label: 'Days', num: true, sort: true, cover: 'visited.cities.days', render: (r) => formatNumber(r.days) },
      dwellCol(null),
      ...datesCols(rowTz).map((c, i) => ({ ...c, cover: i ? 'visited.cities.last_visit' : 'visited.cities.first_visit' })),
      ...(multi ? [carsCol] : []),
    ],
    sort: { key: 'days', dir: 'desc' },
    empty: () => emptyState({ icon: 'pin', title: 'No city', text: cities.items.length ? 'No city matches the column filters. Clear them to see every city.' : 'No stop in this period.' }),
  });
  const refilter = () => { ctx.setQuery({ c_country: tf.country || null, c_region: tf.region || null }); cityTable.setRows(cityRows()); };
  cCountry.addEventListener('change', () => { tf.country = cCountry.value; tf.region = ''; paintRegionOpts(); refilter(); });
  cRegion.addEventListener('change', () => { tf.region = cRegion.value; refilter(); });
  cFind.addEventListener('input', () => { tf.find = cFind.value.trim().toLowerCase(); cityTable.setRows(cityRows()); });
  root.append(cityTable.el);

  /* ---------- days in a city */
  const daysSlot = h('div', { class: 'tv-days-slot' });
  const dayOf = (r) => { const t = rowTz(r); const a = startOf(r.first_ms, 'day', t); return { from: a, to: addPeriod(a, 'day', 1, t) }; };
  const countLink = (n, page, r, what) => (n ? h('a', { class: 'tv-count', href: state.buildUrl(page, { car: r.car, period: dayOf(r) }), title: `${what} of ${carName(r.car)} that day` }, formatNumber(n)) : h('span', { class: 'faint' }, '0'));
  let daysTable = null;
  function paintDays() {
    const c = cityRow();
    const citySel = h('select', { class: 'select select--sm', 'aria-label': 'City' },
      cities.items.map((x) => h('option', { value: x.key, selected: x.key === city }, `${x.label}${x.region ? `, ${x.region}` : ''} (${plural(x.days, 'day')})`)));
    citySel.addEventListener('change', () => pickCity(citySel.value));
    if (!city) {
      replace(daysSlot, panel({ title: 'Days in a city', icon: 'calendar', cover: 'visited.table.city_days' },
        emptyState({ icon: 'pin', title: 'No city to list', text: anyFilter ? 'No stop matches these filters in this period, so there is no city to pick.' : 'No stop in this period, so there is no city to pick. Choose a longer period.' })));
      return;
    }
    daysTable = dataTable(ctx, {
      title: `Days in ${cityName(city)}`, icon: 'calendar', path: 'visits/days', params: params({ city }), limit: 20, csvName: `travel-days-${cityName(city)}`,
      cover: 'visited.table.city_days', cls: 'tv-tbl', side: cities.items.length > 1 ? citySel : null,
      note: `One row per day and car${c?.region ? ` in ${c.label}, ${c.region}` : ''}, newest first, in that car’s own time zone. The list follows the city picked on the map, in the rankings or in the city list; click a date for that day’s drives.`,
      noteCover: 'visited.text.city_days_help',
      columns: [
        { key: 'date', label: 'Date', cover: ['visited.city_days.date', 'visited.link.day_drives'],
          render: (r) => h('a', { href: state.buildUrl('drives', { car: r.car, period: dayOf(r) }), title: `The drives of ${carName(r.car)} that day` }, date(r.first_ms, rowTz(r), { year: true })), csv: (r) => r.date },
        ...(multi ? [{ key: 'car', label: 'Car', render: (r) => carName(r.car), csv: (r) => carName(r.car) }] : []),
        { key: 'first_ms', label: 'First seen', num: true, cover: 'visited.city_days.first_seen', title: 'Arrived in the city, or “start of day” when the car was already there at midnight',
          render: (r) => (r.first_ms === dayOf(r).from ? h('span', { class: 'faint' }, 'start of day') : time(r.first_ms, rowTz(r))), csv: (r) => iso(r.first_ms) },
        { key: 'last_ms', label: 'Last seen', num: true, cover: 'visited.city_days.last_seen', title: 'Left the city, or “end of day” when the car was still there at midnight',
          render: (r) => (r.last_ms != null && r.last_ms >= dayOf(r).to ? h('span', { class: 'faint' }, 'end of day') : time(r.last_ms, rowTz(r))), csv: (r) => iso(r.last_ms) },
        { key: 'drives', label: 'Drives', num: true, cover: 'visited.city_days.drives', title: 'Drives that arrived in or left the city that day', render: (r) => countLink(r.drives.length, 'drives', r, 'The drives'), csv: (r) => r.drives.length },
        { key: 'charges', label: 'Charges', num: true, cover: 'visited.city_days.charges', render: (r) => countLink(r.charges.length, 'charges', r, 'The charges'), csv: (r) => r.charges.length },
        { key: 'places', label: 'Places', cls: 'tv-wrap', cover: 'visited.city_days.places', render: (r) => r.places.join(', '), csv: (r) => r.places.join('; ') },
      ],
      empty: () => emptyState({ icon: 'calendar', title: `No day in ${cityName(city)}`, text: 'No stop in this city matches the period and filters. Pick another city.' }),
    });
    replace(daysSlot, daysTable.el);
  }
  paintDays();

  /* ---------- recent stops */
  const placeHref = (r) => (r.place?.id ? state.buildUrl('places', { query: { place: r.place.id } }) : state.buildUrl('places', { query: { lat: r.lat.toFixed(5), lon: r.lon.toFixed(5), new: 1 } }));
  const recent = dataTable(ctx, {
    title: 'Recent stops', icon: 'clock', path: 'visits', params: params({ sort: 'arrived_desc' }), limit: 15, csvName: 'travel-recent-stops', cover: 'locations.table.last_visited', cls: 'tv-tbl',
    note: 'Newest first. A stop runs from arriving to leaving; one still under way says so.',
    columns: [
      { key: 'arrived_ms', label: 'Arrived', cover: 'locations.last_visited.date', render: (r) => dateTime(r.arrived_ms, rowTz(r)), csv: (r) => iso(r.arrived_ms) },
      { key: 'left_ms', label: 'Left', render: (r) => (r.left_ms == null ? tag('still there', 'live') : dateTime(r.left_ms, rowTz(r))), csv: (r) => iso(r.left_ms) },
      { key: 'dwell', label: 'Time there', num: true, title: 'Arriving to leaving; time without a log is named, not counted as asleep',
        render: (r) => h('span', { class: 'cell-text' }, h('span', { class: 'main' }, dur((r.left_ms ?? ctx.api.now()) - r.arrived_ms)), r.unknown_ms ? h('span', { class: 'sub warn' }, `${dur(r.unknown_ms)} unknown`) : null),
        csv: column('Time there (h)', (r) => Math.round(((r.left_ms ?? ctx.api.now()) - r.arrived_ms) / 36e3) / 100) },
      { key: 'label', label: 'Place', cover: 'locations.last_visited.address',
        render: (r) => h('span', { class: 'cell-text' }, h('a', { class: 'main', href: placeHref(r), title: r.place ? 'Open this saved place' : 'Save this spot as a place' }, r.label || r.address?.short || spotText(r) || DASH),
          r.place && r.address?.short ? h('span', { class: 'sub' }, r.address.short) : null), csv: (r) => r.label },
      { key: 'city', label: 'City', cover: 'locations.last_visited.city', render: (r) => r.address?.locality || DASH, csv: (r) => r.address?.locality },
      ...(multi ? [{ key: 'car', label: 'Car', render: (r) => carName(r.car), csv: (r) => carName(r.car) }] : []),
      { key: 'source', label: 'Source', render: (r) => (r.source === 'teslamate' ? tag('Imported', 'warm') : h('span', { class: 'faint' }, 'CAN')), csv: (r) => r.source },
    ],
    empty: () => emptyState({ icon: 'clock', title: 'No stop', text: anyFilter ? 'No stop matches these filters in this period.' : 'No stop in this period.' }),
  });
  root.append(h('div', { class: 'tv-pair tv-pair--days' }, daysSlot, recent.el));

  /* ---------- places visited */
  const placeCell = (r) => h('span', { class: 'cell-text' },
    r.place ? h('a', { class: 'main', href: state.buildUrl('places', { query: { place: r.place.id } }), cover: 'locations.link.edit_geofence', title: 'Open this saved place in the place editor' }, r.place.name)
      : h('span', { class: 'main' }, r.label || spotText(r) || DASH),
    r.place ? h('span', { class: 'sub' }, [r.street, r.neighbourhood].filter(Boolean).join(', '))
      : h('a', { class: 'sub', href: state.buildUrl('places', { query: { lat: r.lat.toFixed(5), lon: r.lon.toFixed(5), new: 1 } }), cover: 'locations.link.new_geofence', title: 'Make a saved place at this spot' }, icon('plus', 'ico--sm'), 'Save as a place'));
  const placesTable = dataTable(ctx, {
    title: 'Places visited', icon: 'home', rows: placesG.items, csvName: 'travel-places', cover: ['locations.table.addresses', 'locations.table.geofences'], cls: 'tv-tbl',
    note: `${placesG.more ? `The first ${formatNumber(CAP)} places by stops. ` : ''}Stops grouped by saved place, else by position (about 150 m), never by address text, so one spot is never listed twice. Saved places link to their editor; an unnamed spot can be saved as a place.`,
    columns: [
      { key: 'label', label: 'Place', sort: true, cover: ['locations.addresses.name', 'locations.geofences.name', 'locations.link.new_geofence', 'locations.link.edit_geofence'], render: placeCell, csv: (r) => r.label },
      { key: 'kind', label: 'Kind', sort: true, value: (r) => (r.place ? 0 : 1), render: (r) => (r.place ? tag('Saved place', 'live') : tag('Spot')), csv: (r) => (r.place ? 'saved place' : 'spot') },
      { key: 'neighbourhood', label: 'Neighbourhood', sort: true, cover: 'locations.addresses.neighbourhood', render: (r) => r.neighbourhood || DASH },
      { key: 'locality', label: 'City', sort: true, cover: 'locations.addresses.city', render: (r) => r.locality || DASH },
      { key: 'region', label: 'Region', sort: true, cover: 'locations.addresses.state', render: (r) => r.region || DASH },
      { key: 'country', label: 'Country', sort: true, cover: 'locations.addresses.country', render: (r) => r.country || r.country_code || DASH },
      { key: 'visits', label: 'Stops', num: true, sort: true, render: (r) => formatNumber(r.visits) },
      { key: 'days', label: 'Days', num: true, sort: true, render: (r) => formatNumber(r.days) },
      dwellCol('locations.geofences.time'),
      { key: 'last_ms', label: 'Last there', num: true, sort: true, cover: 'locations.addresses.updated_at', title: 'When a car last left this place (or is still there)', render: (r) => date(r.last_ms, rowTz(r), { year: true }), csv: (r) => iso(r.last_ms) },
      ...(multi ? [carsCol] : []),
    ],
    sort: { key: 'visits', dir: 'desc' },
    empty: () => emptyState({ icon: 'home', title: 'No place', text: anyFilter ? 'No stop matches these filters in this period.' : 'No stop in this period.' }),
  });
  root.append(placesTable.el);

  await Promise.all([countryTable.ready, cityTable.ready, recent.ready, daysTable?.ready]);
  if (!ctx.alive()) return;
  ctx.ready();

  /* ---------- live */
  let liveTimer = 0;
  ctx.events('rows', (ev) => {
    const d = ev.data || {};
    if (d.car && !viewCars.includes(d.car)) return;
    if (d.kinds && !d.kinds.some((k) => k === 'visits' || k === 'drives' || k === 'charges')) return;
    clearTimeout(liveTimer); liveTimer = setTimeout(() => ctx.reload(), 1500);
  });
  ctx.onCleanup(() => clearTimeout(liveTimer));

  /* ---------- the map (after ready: it may take a while) */
  let m = null;
  function hoverText(r) {
    const t = rowTz(r);
    replace(hoverCard,
      h('div', { class: 'tv-hover-name' }, r.label || spotText(r) || DASH),
      h('div', { class: 'faint' }, [markMode === 'places' && r.locality, r.region, r.country].filter(Boolean).join(', ')),
      h('dl', { class: 'tv-hover-kv' },
        h('dt', null, 'Days'), h('dd', null, formatNumber(r.days)),
        h('dt', null, 'Stops'), h('dd', null, formatNumber(r.visits)),
        markMode === 'cities' ? [h('dt', null, 'Places'), h('dd', null, formatNumber(r.places))] : null,
        h('dt', null, 'Time spent'), h('dd', null, dur(r.dwell_ms)),
        h('dt', null, 'First visit'), h('dd', null, date(r.first_ms, t, { year: true })),
        h('dt', null, 'Last visit'), h('dd', null, date(r.last_ms, t, { year: true })),
        multi ? [h('dt', null, 'Cars'), h('dd', null, (r.cars || []).map(carName).join(', '))] : null),
      h('div', { class: 'faint tv-hover-pos' }, `${r.country_code || ''} ${r.lat.toFixed(4)}, ${r.lon.toFixed(4)}`.trim()),
      h('div', { class: 'tv-hover-act' }, markMode === 'cities' ? 'Click to list its days' : r.place ? 'Click to open the place' : 'Click to save it as a place'));
    hoverCard.hidden = false;
  }
  const markRows = () => (markMode === 'cities' ? cities.items : placesG.items).filter((r) => r.lat != null && r.lon != null);
  function paintMarks() {
    const word = legend.querySelector('.tv-mark-word');
    if (word) word.textContent = markMode === 'cities' ? 'a city: bigger means more days' : 'a place: bigger means more days';
    hoverCard.hidden = true;
    if (!m) return;
    const rows = markRows();
    const max = Math.max(1, ...rows.map((r) => r.days || 0));
    m.map.getSource('tv-marks')?.setData({ type: 'FeatureCollection', features: rows.map((r, i) => ({ type: 'Feature', properties: { i, r: 3.5 + 12 * Math.sqrt((r.days || 0) / max), saved: r.place ? 1 : 0 }, geometry: { type: 'Point', coordinates: [r.lon, r.lat] } })) });
    if (m.map.getLayer('tv-marks')) m.map.setPaintProperty('tv-marks', 'circle-color', markColour());
  }
  // cities are warm dots; places: saved ones warm, unnamed spots grey
  const markColour = () => (markMode === 'places' ? ['case', ['==', ['get', 'saved'], 1], cssVar('--warm'), cssVar('--text-3')] : cssVar('--warm'));
  try {
    const span = { cars: carsParam, from_ms: ctx.params.from_ms, to_ms: ctx.params.to_ms };
    for (const max of [40000, 200000]) {
      try { tracks = await ctx.api.get('tracks', { ...span, max_points: max }); break; } catch (e) { if (e.status !== 400) throw e; }
    }
  } catch { tracks = null; }
  if (!ctx.alive()) return;
  const lines = (tracks?.drives || []).filter((d) => d.lat.length > 1).map((d) => ({ type: 'Feature', properties: { car: d.car },
    geometry: { type: 'LineString', coordinates: d.lat.map((la, i) => [d.lon[i], la]) } }));
  replace(routeRead, tracks ? `${plural(lines.length, 'drive')} · ${formatNumber(tracks.points || 0)} positions` : 'The route could not be loaded: too many drives for one map. Pick a shorter period or fewer cars.');
  const pts = [...lines.flatMap((l) => [l.geometry.coordinates[0], l.geometry.coordinates[l.geometry.coordinates.length - 1]]), ...cities.items.filter((r) => r.lat != null).map((r) => [r.lon, r.lat]), ...placesG.items.filter((r) => r.lat != null).map((r) => [r.lon, r.lat])];
  if (!pts.length) { mapEl.append(h('div', { class: 'map-fallback' }, 'No drive and no stop in this period, so there is nothing to map.')); return; }
  try { m = await createMap(mapEl, ctx, { bounds: boundsOf(pts) }); } catch { replace(mapEl, mapFallback()); return; }
  if (!m) return;
  await m.ready;
  if (!ctx.alive()) return;
  const carColour = ['match', ['get', 'car'], ...viewCars.flatMap((cid) => [cid, cssVar(colorOf.get(cid))]), cssVar('--live')];
  m.map.addSource('tv-routes', { type: 'geojson', data: { type: 'FeatureCollection', features: lines } });
  const line = { type: 'line', source: 'tv-routes', layout: { 'line-cap': 'round', 'line-join': 'round' } };
  m.addLayer({ ...line, id: 'tv-casing', paint: { 'line-color': cssVar('--map-casing'), 'line-opacity': 0.6 } }, { 'line-width': () => rpx(4.5) });
  m.addLayer({ ...line, id: 'tv-line', paint: { 'line-color': carColour, 'line-opacity': 0.85 } }, { 'line-width': () => rpx(2.2) });
  m.map.addSource('tv-marks', { type: 'geojson', data: { type: 'FeatureCollection', features: [] } });
  m.addLayer({ id: 'tv-marks', type: 'circle', source: 'tv-marks', paint: { 'circle-color': markColour(), 'circle-opacity': 0.7, 'circle-stroke-color': cssVar('--map-casing') } },
    { 'circle-radius': () => ['*', ['get', 'r'], rpx(1)], 'circle-stroke-width': () => rpx(1.5) });
  paintMarks();
  m.map.on('mousemove', 'tv-marks', (e) => {
    m.map.getCanvas().classList.add('is-clickable');
    const r = markRows()[e.features?.[0]?.properties?.i];
    if (r) hoverText(r);
  });
  m.map.on('mouseleave', 'tv-marks', () => { m.map.getCanvas().classList.remove('is-clickable'); hoverCard.hidden = true; });
  m.map.on('click', 'tv-marks', (e) => {
    const r = markRows()[e.features?.[0]?.properties?.i];
    if (!r) return;
    if (markMode === 'cities') pickCity(r.key);
    else ctx.navigate(r.place ? state.buildUrl('places', { query: { place: r.place.id } }) : state.buildUrl('places', { query: { lat: r.lat.toFixed(5), lon: r.lon.toFixed(5), new: 1 } }));
  });
  m.frame(() => { if (pts.length === 1) m.map.jumpTo({ center: pts[0], zoom: 12 }); else m.map.fitBounds(boundsOf(pts), { padding: padClear(mapEl, legend, { beside: true }), duration: 0, maxZoom: 13 }); });
}
