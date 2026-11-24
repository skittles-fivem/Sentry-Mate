// Drives (/cars/{car}/drives): find and compare drives, and see driving patterns. One filter bar holds
// both: the period feeds everything; place, text, distance, speed, source, flags and the efficiency basis
// feed the list (GET /cars/{car}/drives, sorted and paged by the server, CSV) and its summary; the places
// left out feed only the patterns (GET /cars/{car}/drive-stats). Each change reloads only what it feeds.
// The place pick lists hold this car's places, counted by this car (GET /places?car=), every page.
// Energy is measured at the pack; drives with Tesla API data alone and imported ones are marked ≈ (estimated). Where
// a drive's data came from (sources, source_label) is the row's hover title, never shown in the row; the drive's own
// page shows it. Ticked drives open as one trip.
// Live: a drive that starts or ends ("rows" events) loads the lists and patterns again.

import * as state from '../shared/state.js';
import { h, replace, setVars, quantity } from '../shared/dom.js';
import { downloadCsv, csvFilename, column } from '../shared/csv.js';
import { format, toMetric, unitLabel, convert } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, formatDayKey, isoWeek } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon } from '../core/icons.js';
import { spotText } from '../core/fmt.js';
import { pageHead, metric, tag, csvButton } from '../core/page.js';
import { dataTable } from '../core/table.js';
import { emptyState } from '../core/states.js';
import { barChart } from '../core/chart.js';
import { openCarSwitcher } from '../core/carswitch.js';
import { openPeriodPicker } from '../core/period.js';
import { placePopover } from '../core/popover.js';
import { isAdmin } from '../core/prefs.js';
import { flash } from '../core/flash.js';
import { srcTitle, sourceOptions, filterWord, FILTER_WORDS } from '../core/source.js';

export const id = 'drives';
export const css = true;

const placeLabel = (p) => p?.place?.name || p?.label || p?.address?.short || spotText(p) || '—';
const placeHref = (p) => (p?.place?.id ? state.buildUrl('places', { query: { place: p.place.id } })
  : p?.lat != null ? state.buildUrl('places', { query: { lat: p.lat.toFixed(5), lon: p.lon.toFixed(5), new: 1 } }) : null);
const placeCell = (p, cover) => {
  const href = placeHref(p);
  return href ? h('a', { class: 'tl-place-link', href, title: p.place ? `Edit the place ${p.place.name}` : 'Make this spot a place' }, placeLabel(p)) : h('span', null, placeLabel(p));
};

// The list filters of the address, and the API query they make.
// a number from the address, or nothing: a word or a number past what a computer holds is not a filter
const numberOf = (v) => { const n = v === '' || v == null ? NaN : Number(v); return Number.isFinite(n) && n >= 0 ? String(v) : ''; };
const filtersOf = (q) => ({
  places: q.places || '', place_side: q.place_side || 'any', q: q.q || '', min_distance: numberOf(q.min_distance), min_speed: numberOf(q.min_speed),
  source: filterWord(q.source), flags: q.flags || '', efficiency: q.efficiency || 'plain', exclude: q.exclude || '',
});
const listParams = (ctx, f) => ({
  ...ctx.params,
  places: f.places || null, place_side: f.places ? f.place_side : null, q: f.q || null,
  // (the server refuses more than 1,000 km/h (and a distance of 100,000 km is no drive): a larger number asks for the most it takes, an empty list)
  min_distance_km: f.min_distance ? Math.min(1e5, +toMetric(+f.min_distance, 'distance').toFixed(3)) : null,
  min_avg_kph: f.min_speed ? Math.min(1e3, +toMetric(+f.min_speed, 'speed').toFixed(2)) : null,
  source: f.source || null, flags: f.flags || null, efficiency: f.efficiency,
});
const placesRead = (ctx) => ctx.api.list('places', { car: ctx.car.id, sort: 'drives' }, { limit: 200 }).all();
const patternsRead = (ctx, f) => ctx.api.get(`cars/${encodeURIComponent(ctx.car.id)}/drive-stats`, { ...ctx.params, exclude_places: f.exclude || null });
// the first reads, warmed ahead when a link here is under the pointer: the places, and the list's first page
// as the table asks for it (newest first: the start column's sort)
export function prefetch(ctx) {
  const path = `cars/${encodeURIComponent(ctx.car.id)}/drives`;
  const f = filtersOf(ctx.query);
  return Promise.all([
    placesRead(ctx), ctx.api.pager(path, { ...listParams(ctx, f), sort: 'start', dir: 'desc' }, { limit: 25 }).more(),
    ctx.api.pager(path, { ...ctx.params, flags: 'gap,in_progress' }, { limit: 20 }).more(), patternsRead(ctx, f), // drives with missing data, the patterns
  ]);
}

export async function mount(ctx) {
  const { car, tz, period } = ctx;
  const cid = car.id;
  const path = `cars/${encodeURIComponent(cid)}/drives`;
  const q = ctx.query;
  const f = filtersOf(q);
  const apiParams = () => listParams(ctx, f);

  const root = h('div', { class: 'drives-page', cover: ['shots.drives', 'shots.drive-stats'] });
  replace(ctx.view, root);

  // places for the filters: every page of /places, with this car's counts, the places it drove to or from
  // most first. Places it never drove to are left out (a filter on them finds nothing), unless one is chosen.
  let allPlaces = [];
  let placesErr = null;   // said under the filters: the pick lists are empty because the read failed, not because there is no place
  try { allPlaces = await placesRead(ctx); } catch (e) { placesErr = e; allPlaces = []; }
  if (!ctx.alive()) return;
  const chosen = new Set([...(f.places ? f.places.split(',') : []), ...(f.exclude ? f.exclude.split(',') : [])]);
  const places = allPlaces.filter((p) => (p.counts?.drives || 0) > 0 || chosen.has(p.id));
  const unused = allPlaces.length - places.length;
  const placeById = new Map(allPlaces.map((p) => [p.id, p]));
  // two places may share a name: the position tells them apart
  const twins = new Set(places.map((p) => p.name).filter((n, i, a) => a.indexOf(n) !== i));
  const drivesBy = (p) => { const n = p.counts?.drives || 0; return `${formatNumber(n)} ${n === 1 ? 'drive' : 'drives'} by ${car.name}`; };
  const placeSub = (p) => [drivesBy(p), twins.has(p.name) && p.lat != null ? `${p.lat.toFixed(3)}, ${p.lon.toFixed(3)}` : null].filter(Boolean).join(' · ');
  const placeOpts = (sel) => (places.length ? h('optgroup', { label: `Where ${car.name} drove, most drives first` },
    places.map((p) => h('option', { value: p.id, selected: sel.split(',').includes(p.id) }, `${p.name} (${placeSub(p)})`))) : null);

  /* ---------- filters */
  const placeSel = h('select', { class: 'select', 'aria-label': 'Place' }, h('option', { value: '' }, 'Any place'), placeOpts(f.places));
  const sideSel = h('select', { class: 'select', 'aria-label': 'Where the place is', title: 'Match the place at the start, the end, or either' },
    [['any', 'start or end'], ['start', 'start'], ['end', 'end']].map(([v, l]) => h('option', { value: v, selected: f.place_side === v }, `at the ${l}`)));
  const textIn = h('input', { class: 'input', type: 'search', value: f.q, placeholder: 'Start or destination', 'aria-label': 'Text in the start or destination (place name or address)' });
  const distIn = h('input', { class: 'input input--sm', type: 'number', min: '0', step: '0.1', value: f.min_distance, 'aria-label': `Shortest drive (${unitLabel('distance')})` });
  const speedIn = h('input', { class: 'input input--sm', type: 'number', min: '0', step: '1', value: f.min_speed, 'aria-label': `Lowest average speed (${unitLabel('speed')})` });
  const sourceSel = h('select', { class: 'select', 'aria-label': 'Source' }, sourceOptions(f.source).map(([v, l]) => h('option', { value: v, selected: f.source === v }, l)));
  const flagSel = h('select', { class: 'select', 'aria-label': 'Flags' }, [['', 'Every drive'], ['gap', 'With a gap'], ['in_progress', 'In progress'], ['reduced_range', 'Cold battery']].map(([v, l]) => h('option', { value: v, selected: f.flags === v }, l)));
  const effSel = h('select', { class: 'select', 'aria-label': 'Basis for consumption and efficiency' },
    [['plain', 'Efficiency: plain'], ['hill_corrected', 'Efficiency: hill corrected']].map(([v, l]) => h('option', { value: v, selected: f.efficiency === v }, l)));
  const hill = () => f.efficiency === 'hill_corrected';
  // the visible line under the filter bar: what the text box matches and what the basis means
  const effHint = h('span');
  const paintEffHint = () => replace(effHint, hill()
    ? 'Consumption, efficiency and the summary are hill corrected: the energy for the climb (less what the descent gives back) is taken off, with a typical mass for the model.'
    : 'Consumption, efficiency and the summary are plain: all the energy the drive took. Efficiency is the rated consumption against it (100 % = as rated).');
  paintEffHint();
  const filterHint = h('p', { class: 'note filters-hint', cover: ['drives.filter.location.hint'] },
    h('span', null, 'The text box keeps drives whose start or destination contains the text, in the place name or the address. '), effHint,
    placesErr ? h('span', { class: 'warn', role: 'status' }, ` The places could not be loaded (${placesErr.message}), so the place filters are empty: load the page again to use them.`) : null);
  // the basis named on the columns and the tile it changes
  const basisTag = () => (hill() ? ' (hill corrected)' : '');
  const whHead = h('span'); const effHead = h('span');
  const paintHeads = () => { whHead.textContent = `Consumption${basisTag()}`; effHead.textContent = `Efficiency${basisTag()}`; };
  paintHeads();
  // places to leave out of the patterns: a pick list of the known places (home, work, ...)
  const excluded = new Set(f.exclude ? f.exclude.split(',') : []);
  const exclText = h('span', { class: 'excl-text' });
  const exclBtn = h('button', { class: 'btn excl-btn', type: 'button', 'aria-haspopup': 'dialog', 'aria-expanded': 'false',
    title: 'Leave places such as home or work out of the patterns and the top destinations. The list of drives is not changed.' },
  icon('pin', 'ico--sm'), exclText, icon('chev-d', 'ico--sm'));
  const exclList = h('div', { class: 'excl-list', role: 'group', 'aria-label': 'Places to leave out' });
  const exclFind = h('input', { class: 'input', type: 'search', placeholder: 'Find a place', 'aria-label': 'Find a place to leave out' });
  const exclPop = h('div', { class: 'popover excl-pop', popover: 'auto', role: 'dialog', 'aria-label': 'Leave places out of the patterns' },
    h('p', { class: 'excl-hint', cover: 'ds.filter.exclude_locations.hint' }, 'Drives that start or end at a ticked place are left out of the driving patterns below, so home or work does not top the destinations. The list of drives keeps them.'),
    places.length > 8 ? exclFind : null, exclList,
    unused ? h('p', { class: 'note excl-unused' }, `${formatNumber(unused)} other ${unused === 1 ? 'place is' : 'places are'} not listed: ${car.name} never drove to or from ${unused === 1 ? 'it' : 'them'}.`) : null,
    h('div', { class: 'excl-actions' }, h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: () => { excluded.clear(); paintExcl(); applyExclude(); } }, 'Leave nothing out'),
      h('button', { class: 'btn btn--sm', type: 'button', onclick: () => exclPop.hidePopover() }, 'Done')));
  const paintExclText = () => {
    const names = [...excluded].map((id) => placeById.get(id)?.name).filter(Boolean);
    exclText.textContent = names.length ? `Patterns without ${names.length > 2 ? `${names.length} places` : names.join(', ')}` : 'Patterns: every place';
    exclBtn.classList.toggle('is-on', names.length > 0);
  };
  const paintExcl = () => {
    const needle = exclFind.value.trim().toLowerCase();
    const shown = places.filter((p) => !needle || p.name.toLowerCase().includes(needle));
    replace(exclList, shown.length ? shown.map((p) => h('label', { class: 'check' },
      h('input', { type: 'checkbox', checked: excluded.has(p.id), onchange: (e) => { if (e.target.checked) excluded.add(p.id); else excluded.delete(p.id); paintExclText(); applyExclude(); } }),
      h('span', null, p.name), h('span', { class: 'faint excl-sub' }, placeSub(p)))) : h('p', { class: 'note' }, places.length ? 'No place has that name.' : `${car.name} has not driven to or from a saved place yet. Add places on the Places page to leave them out here.`));
    paintExclText();
  };
  exclFind.addEventListener('input', paintExcl);
  exclBtn.addEventListener('click', () => {
    if (exclPop.matches(':popover-open')) { exclPop.hidePopover(); return; }
    exclPop.showPopover(); placePopover(exclPop, exclBtn, { align: 'start' });
  });
  exclPop.addEventListener('toggle', (e) => exclBtn.setAttribute('aria-expanded', String(e.newState === 'open')));
  paintExcl();

  const syncQuery = () => ctx.setQuery({ places: f.places || null, place_side: f.places && f.place_side !== 'any' ? f.place_side : null, q: f.q || null, min_distance: f.min_distance || null, min_speed: f.min_speed || null, source: f.source || null, flags: f.flags || null, efficiency: f.efficiency === 'plain' ? null : f.efficiency, exclude: f.exclude || null });
  // the list filters reload the list and its summary (not the patterns); the picks are of the old list
  let timer = 0;
  const apply = (now = false) => {
    clearTimeout(timer);
    timer = setTimeout(() => {
      const next = { places: placeSel.value, place_side: sideSel.value, q: textIn.value.trim(), min_distance: distIn.value, min_speed: speedIn.value, source: sourceSel.value, flags: flagSel.value, efficiency: effSel.value };
      if (Object.keys(next).every((k) => next[k] === f[k])) return;
      Object.assign(f, next);
      syncQuery(); paintMoreCount();
      paintEffHint(); paintHeads();
      reloadList();
    }, now ? 0 : 350);
  };
  // places left out change only the patterns: the list and its picks stay
  const applyExclude = () => {
    f.exclude = [...excluded].join(',');
    syncQuery(); paintMoreCount();
    loadPatterns();
  };
  function reloadList() {
    clearPicks();
    paintSummary(null, 'loading');
    return table.reload(apiParams());
  }
  function clearFilters() {
    placeSel.value = ''; sideSel.value = 'any'; textIn.value = ''; distIn.value = ''; speedIn.value = ''; sourceSel.value = ''; flagSel.value = '';
    apply(true);
  }
  ctx.onCleanup(() => { clearTimeout(timer); if (exclPop.matches(':popover-open')) exclPop.hidePopover(); exclPop.remove(); });
  for (const el of [placeSel, sideSel, sourceSel, flagSel, effSel]) el.addEventListener('change', () => apply(true));
  for (const el of [textIn, distIn, speedIn]) el.addEventListener('input', () => apply());

  // the filters used less often: in the bar on a wide screen, folded into "More filters" on a phone
  const moreFilters = [
    h('label', { class: 'filter filter--num', cover: 'drives.filter.min_distance' }, h('span', null, 'At least'), distIn, h('span', { class: 'faint' }, unitLabel('distance'))),
    h('label', { class: 'filter filter--num', cover: 'drives.filter.min_speed' }, h('span', null, 'Average at least'), speedIn, h('span', { class: 'faint' }, unitLabel('speed'))),
    sourceSel, flagSel,
    h('span', { class: 'filter-eff', cover: 'drives.setting.efficiency_method' }, effSel),
    h('span', { class: 'filter filter-excl', cover: ['ds.filter.exclude_locations', 'ds.filter.exclude_locations.hint'] }, exclBtn),
  ];
  const moreCount = h('span', { class: 'drv-more-count' });
  const moreBody = h('div', { class: 'drv-more-body' });
  const moreBox = h('details', { class: 'drv-more' }, h('summary', { class: 'btn drv-more-sum' }, 'More filters', moreCount, icon('chev-d', 'ico--sm drv-more-chev')), moreBody);
  const filters = h('div', { class: 'filter-bar drives-filters' },
    h('button', { class: 'btn drv-period', type: 'button', onclick: (e) => openPeriodPicker(e.currentTarget), cover: ['drives.filter.time_range', 'ds.filter.time_range'] }, icon('calendar', 'ico--sm'), period.preset === 'custom' ? 'Custom period' : period.label, icon('chev-d', 'ico--sm')),
    h('span', { class: 'filter filter-place', cover: 'drives.filter.geofence' }, icon('pin', 'ico--sm'), placeSel, sideSel),
    h('label', { class: 'field field--search', cover: 'drives.filter.location' }, icon('search'), textIn),
    moreFilters, filterHint);
  // how many of the folded filters are on, shown on the closed "More filters"
  const paintMoreCount = () => {
    const n = [f.min_distance, f.min_speed, f.source, f.flags, hill(), f.exclude].filter(Boolean).length;
    moreCount.textContent = n ? `${formatNumber(n)} on` : '';
  };
  const layoutFilters = () => {
    if (document.documentElement.dataset.phone) { moreBody.append(...moreFilters, filterHint); filters.append(moreBox); } else { moreBox.remove(); filters.append(...moreFilters, filterHint); }
  };
  layoutFilters(); paintMoreCount();
  const phoneWatch = new MutationObserver(layoutFilters);
  phoneWatch.observe(document.documentElement, { attributes: true, attributeFilter: ['data-phone'] });
  ctx.onCleanup(() => phoneWatch.disconnect());

  /* ---------- summary */
  const summary = h('section', { class: 'panel drives-summary', 'aria-label': 'Summary of the listed drives' });
  const dailyEl = h('div', { class: 'daily-chart-plot' });
  const dailyRead = h('span');
  let perDay = [];
  const dailyCsv = csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'drives-per-day'), perDay,
    [column('Day', 'date'), column('Drives', 'count'), column('Distance', 'distance_km', 'distance'), column('Energy', 'energy_kwh', 'energy')]) });
  // the summary follows the list: loading while it loads, a dash and "could not be loaded" when it fails
  const paintSummary = (s, st = null) => {
    s = s || {};
    const none = !s.count;
    const why = st === 'loading' ? 'Loading' : st === 'error' ? 'Could not be loaded' : 'No drive matches';
    const blank = st != null || none;
    const measured = (s.count ?? 0) - (s.without_energy ?? 0);
    const hillS = s.efficiency ? s.efficiency === 'hill_corrected' : hill();
    replace(summary,
      h('div', { class: 'drives-metrics' },
        metric('Drives', st ? null : s.count ?? 0, 'count', { why, explain: st === 'error' ? 'the list could not be loaded' : st ? 'loading the list' : none ? 'match the filters' : s.without_energy ? `${formatNumber(measured)} with energy, ${formatNumber(s.without_energy)} without` : 'all with energy' }),
        metric('Distance', blank ? null : s.distance_km, 'distance', { cover: 'drives.summary.distance', why, explain: 'sum of the listed drives' }),
        metric('Driving time', blank ? null : s.duration_ms, 'duration', { cover: 'drives.summary.duration', why, explain: 'from start to end of each drive' }),
        metric('Energy', blank ? null : s.energy_kwh, 'energy', { cover: 'drives.summary.energy', why, explain: s.energy_estimated_kwh ? `${format(s.energy_measured_kwh, 'energy')} measured at the pack, ${format(s.energy_estimated_kwh, 'energy')} estimated (Tesla API alone or imported)` : 'measured at the pack, regen taken off' }),
        metric(`Consumption${hillS ? ' (hill corrected)' : ''}`, blank ? null : s.wh_per_km, 'efficiency', { cover: 'drives.summary.consumption', why,
          explain: `${hillS ? 'energy less the climb, over distance' : 'energy over distance'}${s.without_energy ? ', drives with energy only' : ''}; same basis as the list` })));
  };

  /* ---------- the list */
  const effBar = (d) => {
    const v = d.efficiency_pct;
    if (v == null) return h('span', { class: 'faint' }, '—');
    const bar = h('span', { class: ['eff-bar', v >= 90 && 'is-better'] }, h('i'));
    setVars(bar, { '--w': `${Math.max(4, Math.min(100, v)).toFixed(1)}%` });
    return h('span', { class: 'eff' }, bar, h('span', { class: 'eff-val' }, format(v, 'percent')));
  };
  // pick drives to see them together as a trip (the period from the first start to the last end)
  const picked = new Map();
  const pickText = h('span', { class: 'pick-text', 'aria-live': 'polite' });
  const pickTrip = h('a', { class: 'btn btn--sm btn--primary', href: '#' }, icon('flag', 'ico--sm'), 'Open as a trip');
  const pickBar = h('span', { class: 'pick-bar', hidden: true }, pickText, pickTrip,
    h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: () => { clearPicks(); } }, 'Clear'));
  function paintPicks() {
    const ds = [...picked.values()];
    pickBar.hidden = !ds.length;
    if (!ds.length) return;
    const from = Math.min(...ds.map((d) => d.start_ms));
    const ends = ds.map((d) => d.end_ms);
    const to = ends.some((e) => e == null) ? null : Math.ceil(Math.max(...ends) / 60000) * 60000;
    const km = ds.reduce((a, d) => a + (d.distance_km || 0), 0);
    pickText.textContent = `${formatNumber(ds.length)} picked · ${format(km, 'distance')}`;
    pickTrip.setAttribute('href', state.buildUrl('trip', { car: cid, period: { from, to } }));
    pickTrip.title = `The trip page for ${formatDate(from, tz, { year: false })}, ${formatTime(from, tz)} to ${to ? `${formatDate(to, tz, { year: false })}, ${formatTime(to, tz)}` : 'now'}, with everything in between`;
  }
  function clearPicks() {
    picked.clear();
    for (const b of table?.el.querySelectorAll('.pick input') || []) b.checked = false;
    paintPicks();
  }
  const columns = [
    { key: 'pick', label: '', cls: 'col-pick', csv: false, title: 'Pick drives to open them together as one trip', render: (d) => h('label', { class: 'pick', title: 'Pick this drive for a trip' },
      h('input', { type: 'checkbox', checked: picked.has(d.id), 'aria-label': `Pick the drive of ${formatDate(d.start_ms, tz, { year: false })}, ${formatTime(d.start_ms, tz)}`,
        onchange: (e) => { if (e.target.checked) picked.set(d.id, d); else picked.delete(d.id); paintPicks(); } })) },
    // the end under the start, with the flags: a cold battery, missing data, still driving
    { key: 'start', label: 'Start', sort: 'start', firstDir: 'desc', cover: ['drives.table.date', 'drives.table.reduced_range'], title: 'Start, then end. A thermometer marks a cold battery, a warning sign missing data.', render: (d) => h('span', { class: 'cell-text' },
      h('a', { class: 'main', href: state.buildUrl('drive', { car: cid, drive: d.id }) }, `${formatDate(d.start_ms, tz, { year: false })}, ${formatTime(d.start_ms, tz)}`),
      h('span', { class: 'sub start-sub' }, d.end_ms && !d.flags?.in_progress ? `to ${formatTime(d.end_ms, tz)}` : h('span', { class: 'tag tag--live' }, 'driving now'),
        d.flags?.reduced_range ? h('span', { class: 'flag-ico', title: 'Cold battery: part of the charge was locked, so range and consumption read high' }, icon('thermo', 'ico--sm')) : null,
        d.flags?.gap_ms ? h('span', { class: 'flag-ico', title: `Data missing for ${formatDuration(d.flags.gap_ms)}` }, icon('warn', 'ico--sm')) : null)),
      csv: (d) => new Date(d.start_ms).toISOString() },
    { key: 'route', label: 'Route', cls: 'col-route', cover: ['drives.table.start', 'drives.table.destination', 'drives.table.link.geofence_start', 'drives.table.link.geofence_end'], render: (d) => h('span', { class: 'cell-text' },
      h('span', { class: 'route' }, placeCell(d.start), h('span', { class: 'tl-arrow', 'aria-hidden': 'true' }, ' → '), d.end && d.end_ms != null ? placeCell(d.end) : h('span', null, d.end_ms == null ? 'driving now' : '…')),
      h('span', { class: 'sub' }, [d.start?.address?.short, d.end?.address?.short].filter(Boolean).join(' → '))), csv: (d) => `${placeLabel(d.start)} → ${placeLabel(d.end)}` },
    { key: 'duration', label: 'Time', cls: 'col-time', num: true, sort: 'duration', cover: 'drives.table.duration', render: (d) => formatDuration(d.duration_ms) },
    // a narrow table has no Time column: the time goes under the distance
    { key: 'distance', label: 'Distance', num: true, sort: 'distance', cover: 'drives.table.distance', render: (d) => h('span', { class: 'cell-text cell-num' },
      h('span', { class: 'main' }, format(d.distance_km, 'distance')), h('span', { class: 'sub dist-time' }, formatDuration(d.duration_ms))) },
    { key: 'soc', label: 'Battery', num: true, cover: ['drives.table.soc_start', 'drives.table.soc_end'], render: (d) => h('span', { class: 'nowrap', title: d.outside_avg_c != null ? `Outside ${format(d.outside_avg_c, 'temperature')}` : null }, `${format(d.start_soc_pct, 'percent')} → ${format(d.end_soc_pct, 'percent')}`) },
    { key: 'outside', label: 'Outside', cls: 'col-outside', num: true, cover: 'drives.table.outside_temp', render: (d) => h('span', { class: ['temp-cell', d.outside_avg_c != null && (d.outside_avg_c < 5 ? 'is-cold' : d.outside_avg_c > 25 ? 'is-warm' : null)] }, format(d.outside_avg_c, 'temperature')) },
    { key: 'speed_max', label: 'Speed', cls: 'col-speed', num: true, sort: 'speed_max', cover: ['drives.table.speed_avg', 'drives.table.speed_max'], title: 'Average speed, then the top speed (the order sorts by the top speed)', render: (d) => h('span', { class: 'cell-text cell-num' },
      h('span', { class: 'main' }, format(d.speed_avg_kph, 'speed')), h('span', { class: 'sub' }, d.speed_max_kph != null ? `top ${format(d.speed_max_kph, 'speed')}` : '')) },
    { key: 'energy', label: 'Energy', num: true, sort: 'energy', cover: ['drives.table.energy', 'drives.table.power_max'], title: 'Energy from the pack, regen taken off; the peak power in each value’s title', render: (d) => h('span', { class: 'nowrap', title: d.power_max_kw != null ? `Peak power ${format(d.power_max_kw, 'power')}` : null }, d.energy_basis === 'estimated' ? h('span', { class: 'est-mark', title: 'Estimated: from the Tesla API\'s power readings (or an import), not measured at the pack' }, '≈ ') : null, format(d.energy_kwh, 'energy')) },
    { key: 'wh', label: whHead, num: true, sort: 'wh_per_km', cover: 'drives.table.consumption', render: (d) => format(d.wh_per_km, 'efficiency') },
    { key: 'eff', label: effHead, cover: 'drives.table.efficiency', title: 'Rated consumption against this drive\'s consumption, on the basis chosen above (100 % = as rated)', render: effBar, csv: (d) => d.efficiency_pct },
  ];
  const table = dataTable(ctx, {
    title: 'Drives', icon: 'route', path, params: apiParams(), limit: 25, columns, sort: { key: 'start', dir: 'desc' }, csvName: 'drives', cls: 'drives-table',
    cover: ['drives.table', 'drives.table.link.details', 'drives.empty_states'], side: pickBar,
    note: 'Energy is measured at the pack, regen taken off (≈ marks an estimate). Tick drives to open them together as a trip.',
    noteMore: 'Drives whose energy is an estimate (Tesla API data alone, or imported) are marked ≈; hover a row to see where its data came from (SMO, the Tesla API, an import), and open it for the details. Efficiency is the rated consumption against the drive\'s own. A thermometer by the end time marks a cold battery (part of the charge was locked, so range and consumption read high), a warning sign missing data. The CSV has every figure.',
    rowTitle: srcTitle,
    rowHref: (d) => state.buildUrl('drive', { car: cid, drive: d.id }),
    onPage: (pg) => { if (pg.summary) paintSummary(pg.summary); },
    onError: () => paintSummary(null, 'error'),
    empty: () => {
      const active = activeFilters();
      return emptyState({ icon: 'route', title: 'No drives',
        text: active.length ? `No drive in the chosen period (${period.label}) passes every filter that is on: ${active.join('; ')}.` : `${car.name} did not drive in the chosen period (${period.label}). Pick a longer period.`,
        action: active.length ? h('button', { class: 'btn btn--sm', type: 'button', onclick: clearFilters }, icon('reset', 'ico--sm'), 'Clear filters') : null });
    },
  });

  // every list filter that is on, in words
  function activeFilters() {
    const side = { start: 'at the start', end: 'at the end', any: 'at the start or end' }[f.place_side] || 'at the start or end';
    const names = f.places ? f.places.split(',').map((id) => placeById.get(id)?.name || id).join(', ') : '';
    return [
      f.places && `place ${names} ${side}`,
      f.q && `text "${f.q}"`,
      f.min_distance && `at least ${format(toMetric(+f.min_distance, 'distance'), 'distance')}`,
      f.min_speed && `average at least ${format(toMetric(+f.min_speed, 'speed'), 'speed')}`,
      f.source && (FILTER_WORDS[f.source] || f.source),
      f.flags && ({ gap: 'with a gap', in_progress: 'in progress', reduced_range: 'cold battery' }[f.flags] || f.flags),
    ].filter(Boolean);
  }

  /* ---------- drives with missing data */
  const incomplete = dataTable(ctx, {
    title: 'Drives with missing data', icon: 'warn', path, params: { ...ctx.params, flags: 'gap,in_progress' }, limit: 20, csvName: 'drives-missing-data', cls: 'missing-table',
    note: 'Drives that overlap a gap in the logs, or are still under way. Late logs fill them in by themselves; an admin can also rebuild the period from the stored logs. Nothing needs repairing by hand.',
    noteCover: 'drives.incomplete.note',
    cover: ['drives.incomplete.table', 'drives.incomplete.id', 'drives.incomplete.start', 'drives.incomplete.end', 'drives.incomplete.distance', 'drives.incomplete.duration'],
    side: isAdmin() ? h('button', { class: 'btn btn--sm', type: 'button', onclick: rebuild, title: 'Read the stored logs again for this period and rebuild drives, charges, states and stops' }, icon('reset', 'ico--sm'), 'Rebuild the period') : null,
    columns: [
      { key: 'id', label: 'Drive', render: (d) => h('a', { class: 'main mono', href: state.buildUrl('drive', { car: cid, drive: d.id }) }, d.id), csv: (d) => d.id },
      { key: 'start', label: 'Start', render: (d) => `${formatDate(d.start_ms, tz, { year: false })}, ${formatTime(d.start_ms, tz)}`, csv: (d) => new Date(d.start_ms).toISOString() },
      { key: 'end', label: 'Last recorded', render: (d) => (d.end_ms ? formatTime(d.end_ms, tz) : 'still going'), csv: (d) => (d.end_ms ? new Date(d.end_ms).toISOString() : '') },
      { key: 'why', label: 'What is missing', render: (d) => (d.flags?.in_progress ? 'Under way; it ends when the next logs arrive' : `No data for ${formatDuration(d.flags?.gap_ms)} inside it${d.flags?.lost_frames ? ', frames lost' : ''}`) },
      { key: 'distance', label: 'Distance', num: true, render: (d) => format(d.distance_km, 'distance') },
      { key: 'duration', label: 'Time', num: true, render: (d) => formatDuration(d.duration_ms) },
    ],
    empty: () => emptyState({ icon: 'check', title: 'No drive is missing data', text: 'Every drive in this period has all its data.' }),
  });
  async function rebuild() {
    try {
      const r = await ctx.api.post(`cars/${encodeURIComponent(cid)}/rebuild`, ctx.params);
      flash(`Rebuilt from ${formatNumber(r.logs_read ?? 0)} stored logs.`);
      ctx.reload();
    } catch (e) { flash(`Could not rebuild: ${e.message}`, { kind: 'error' }); }
  }

  /* ---------- driving patterns */
  const patterns = h('section', { class: 'panel panel-pad patterns', 'aria-label': 'Driving patterns', cover: 'ds.empty_states' });
  async function loadPatterns() {
    let s;
    try { s = await patternsRead(ctx, f); } catch (e) { replace(patterns, h('p', { class: 'note' }, `Patterns could not be loaded: ${e.message}`)); return; }
    if (!ctx.alive()) return;
    // per_day is every day of the car from the later of the period start and its first data to the end (or now)
    const days = s.per_day || [];
    const nDays = Math.max(1, s.days ?? days.length);
    // the small bars: per day up to 62 days, per ISO week up to 434 days, else per month
    const unit = days.length > 434 ? 'month' : days.length > 62 ? 'week' : 'day';
    const groups = [];
    for (const d of days) {
      const [y, m, dd] = d.date.split('-').map(Number);
      const wk = unit === 'week' ? isoWeek(y, m, dd) : null;
      const key = unit === 'day' ? d.date : unit === 'month' ? d.date.slice(0, 7) : `${wk.year}-W${wk.week}`;
      let g = groups[groups.length - 1];
      if (!g || g.key !== key) { g = { key, count: 0, distance_km: 0, energy_kwh: 0 }; groups.push(g); }
      g.count += d.count || 0; g.distance_km += d.distance_km || 0; g.energy_kwh += d.energy_kwh || 0;
    }
    const sparkSpan = days.length ? `${formatDayKey(days[0].date, { weekday: false })} to ${formatDayKey(days[days.length - 1].date, { weekday: false })}` : '';
    const spark = (key) => {
      const max = Math.max(1e-9, ...groups.map((d) => d[key] || 0));
      return h('span', { class: 'spark', 'aria-hidden': 'true' }, groups.map((d) => { const i = h('i'); setVars(i, { '--h': `${((d[key] || 0) / max) * 100}%` }); return i; }));
    };
    const sparkWhat = (what) => (days.length ? `bars: ${what} per ${unit}, ${sparkSpan}` : 'no days in the period');
    const endMs = Math.min(ctx.params.to_ms ?? ctx.api.now(), ctx.api.now());
    const endDay = formatDate(endMs - 1, tz, { weekday: false });
    const top = (x) => (x?.kph != null ? h('a', { class: 'link', href: state.buildUrl('drive', { car: cid, drive: x.drive }) }, format(x.kph, 'speed')) : null);
    const bands = (s.speed_bands || []).filter((b) => b.to_kph > 0 && b.seconds > 0);
    const maxBand = Math.max(1e-9, ...bands.map((b) => b.share_pct || 0));
    // a drive still going has no destination: never a row here
    const dests = (s.top_destinations || []).filter((d) => d.place || !['Driving now', 'Data stopped here'].includes(d.label));
    const maxDest = Math.max(1, ...dests.map((d) => d.count));
    const left = [...excluded].map((id) => placeById.get(id)?.name).filter(Boolean);
    const bandLabel = (b) => (b.to_kph == null ? `${format(b.from_kph, 'speed', { digits: 0 })} and over`
      : `${formatNumber(convert(b.from_kph, 'speed'), { maximumFractionDigits: 0 })}–${format(b.to_kph, 'speed', { digits: 0 })}`);
    perDay = days;
    const csvSmall = (name, rows, cols) => csvButton({ onClick: () => downloadCsv(csvFilename(car.name, name), rows, cols) });
    // the parts kept from the last drawing (the chart, its reading, its CSV) go in after the swap: put in the new
    // drawing while it is built they would leave the panel on screen first, shorter (the chart's height) when
    // replace() looks at where things are, and Chromium would scroll the page by that much
    const kept = [];
    const keep = (el) => { const c = document.createComment(''); kept.push([c, el]); return c; };
    replace(patterns,
      h('div', { class: 'panel-head' }, h('h2', { class: 'panel-title' }, icon('stats'), 'Driving patterns'),
        h('span', { class: 'faint patterns-scope' }, left.length ? `${period.label}, without drives to or from ${left.join(', ')} (the 30 and 7 day top speeds count every drive; the list filters do not apply here)` : `${period.label}, every drive (the list filters do not apply here)`),
        csvSmall('drive-patterns', [{ ...s, ...{ distance_per_day_km: s.count ? (s.distance_km || 0) / nDays : null, energy_per_day_kwh: s.count ? (s.energy_kwh || 0) / nDays : null } }], [
          column('Drives', 'count'), column('Distance', 'distance_km', 'distance'), column('Energy', 'energy_kwh', 'energy'), column('Median drive', 'median_distance_km', 'distance'),
          column('Distance per day', 'distance_per_day_km', 'distance'), column('Energy per day', 'energy_per_day_kwh', 'energy'),
          column('Top speed in the period', (r) => r.max_speed?.period?.kph, 'speed'), column('Top speed, last 30 days', (r) => r.max_speed?.last_30d?.kph, 'speed'), column('Top speed, last 7 days', (r) => r.max_speed?.last_7d?.kph, 'speed'),
          column('Projected per month', (r) => r.projection?.per_month_km, 'distance'), column('Projected per year', (r) => r.projection?.per_year_km, 'distance'), column('Projection basis (days)', (r) => r.projection?.basis_days)])),
      s.count ? null : h('p', { class: 'note warn patterns-empty' }, left.length ? `No drive in the chosen period (${period.label}) once ${left.join(', ')} ${left.length === 1 ? 'is' : 'are'} left out. Leave fewer places out to see patterns.` : `${car.name} did not drive in the chosen period (${period.label}), so there are no patterns. Pick a longer period.`,
        left.length ? [' ', h('button', { class: 'btn btn--sm', type: 'button', onclick: () => { excluded.clear(); paintExcl(); applyExclude(); } }, 'Leave nothing out')] : null),
      h('div', { class: 'pattern-grid' },
        h('div', { class: 'metric', cover: 'ds.stat.drive_count' }, h('span', { class: 'metric-label' }, 'Drives'), h('span', { class: 'metric-value' }, formatNumber(s.count || 0)), spark('count'), h('span', { class: 'explain' }, sparkWhat('drives'))),
        h('div', { class: 'metric', cover: 'ds.stat.distance_total' }, h('span', { class: 'metric-label' }, 'Distance'), h('span', { class: 'metric-value' }, s.count ? quantity(s.distance_km, 'distance') : '—'), spark('distance_km'), h('span', { class: 'explain' }, sparkWhat('distance'))),
        h('div', { class: 'metric', cover: 'ds.stat.energy_total' }, h('span', { class: 'metric-label' }, 'Energy'), h('span', { class: 'metric-value' }, s.count ? quantity(s.energy_kwh, 'energy') : '—'), spark('energy_kwh'), h('span', { class: 'explain' }, sparkWhat('energy at the pack'))),
        metric('Typical drive', s.median_distance_km, 'distance', { cover: 'ds.stat.median_distance', explain: 'median: half are shorter', why: 'No drive' }),
        metric('Per day', s.count ? (s.distance_km || 0) / nDays : null, 'distance', { cover: 'ds.stat.distance_per_day', explain: days.length ? `over ${formatNumber(nDays)} ${nDays === 1 ? 'day' : 'days'} (from the period start or the first data); days without driving count as 0` : 'no recorded days in the period', why: 'No drive' }),
        metric('Energy per day', s.count ? (s.energy_kwh || 0) / nDays : null, 'energy', { cover: 'ds.stat.energy_per_day', explain: 'driving energy over the same days', why: 'No drive' }),
        metric('Top speed', top(s.max_speed?.period), null, { cover: 'ds.stat.max_speed_period', explain: 'in the period; opens that drive', why: 'No drive' }),
        metric('Last 30 days', top(s.max_speed?.last_30d), null, { cover: 'ds.stat.max_speed_30d', explain: `top speed in the 30 days to ${endDay}, whatever the period; every drive counts`, why: 'No drive in those days' }),
        metric('Last 7 days', top(s.max_speed?.last_7d), null, { cover: 'ds.stat.max_speed_7d', explain: `top speed in the 7 days to ${endDay}, whatever the period; every drive counts`, why: 'No drive in those days' }),
        metric('Per month', s.projection?.per_month_km, 'distance', { cover: 'ds.stat.monthly_mileage', explain: s.projection?.basis_days ? `projected from the same ${formatNumber(Math.round(s.projection.basis_days))} days` : 'projected', why: 'Not enough driving to project' }),
        metric('Per year', s.projection?.per_year_km, 'distance', { cover: 'ds.stat.annual_mileage', explain: 'projected at the same rate', why: 'Not enough driving to project' })),
      h('div', { class: 'daily-chart' }, h('div', { class: 'daily-chart-head' }, h('span', null, "Distance per day, in the car's days"), h('span', { class: 'daily-chart-side' }, keep(dailyRead), keep(dailyCsv))), keep(dailyEl)),
      h('div', { class: 'pattern-lists' },
        h('div', { cover: 'ds.chart.speed_histogram' }, h('div', { class: 'list-head' }, h('h3', { class: 'list-title' }, 'Time at each speed'),
          csvSmall('drive-speed-bands', bands, [column('From', 'from_kph', 'speed'), column('To', 'to_kph', 'speed'), column('Time (s)', 'seconds'), column('Share', 'share_pct', 'percent')])),
          bands.length ? h('ul', { class: 'hbars' }, bands.map((b) => {
            const bar = h('i'); setVars(bar, { '--w': `${((b.share_pct || 0) / maxBand) * 100}%` });
            return h('li', null, h('span', { class: 'hbar-label' }, bandLabel(b)), h('span', { class: 'hbar' }, bar), h('span', { class: 'hbar-val' }, `${format(b.share_pct, 'percent')} · ${formatDuration(b.seconds * 1000)}`));
          })) : h('p', { class: 'note' }, 'No driving in the period.'),
          h('p', { class: 'note' }, 'Counted second by second while driving, from 1 Hz speed.')),
        h('div', { cover: 'ds.chart.top_destinations' }, h('div', { class: 'list-head' }, h('h3', { class: 'list-title' }, 'Most frequent destinations'),
          csvSmall('drive-destinations', dests, [column('Destination', 'label'), column('Place id', 'place'), column('Drives', 'count')])),
          dests.length ? h('ul', { class: 'hbars' }, dests.map((d) => {
            const bar = h('i'); setVars(bar, { '--w': `${(d.count / maxDest) * 100}%` });
            return h('li', null, d.place ? h('a', { class: 'hbar-label link', href: state.buildUrl('drives', { car: cid, period: state.current().period, query: { places: d.place, place_side: 'end' } }), title: 'Show the drives to this place' }, d.label) : h('span', { class: 'hbar-label' }, d.label),
              h('span', { class: 'hbar' }, bar), h('span', { class: 'hbar-val' }, `${formatNumber(d.count)} ${d.count === 1 ? 'drive' : 'drives'}`));
          })) : h('p', { class: 'note' }, 'No destination in the period.'),
          h('p', { class: 'note' }, 'Where each drive ended. A saved place opens its drives; an address is not a place yet.'))));
    for (const [c, el] of kept) c.replaceWith(el);

    // distance per day
    const xs = days.map((d) => Date.parse(`${d.date}T12:00:00Z`));
    const best = days.reduce((a, d) => (d.distance_km > (a?.distance_km ?? -1) ? d : a), null);
    const idle = () => replace(dailyRead, best ? ['busiest day ', h('b', null, format(best.distance_km, 'distance'))] : 'no driving');
    idle();
    dailyEl.hidden = !days.length;
    if (days.length) barChart(dailyEl, ctx, { t_ms: xs, values: days.map((d) => d.distance_km), kind: 'distance', tz: 'UTC', label: 'Distance driven on each day of the period, as bars', onCursor: (i) => { if (i == null) { idle(); return; } const d = days[i]; replace(dailyRead, `${formatDate(xs[i], 'UTC', { year: false })} · `, h('b', null, format(d.distance_km, 'distance')), ` · ${d.count} ${d.count === 1 ? 'drive' : 'drives'}`); } });
  }

  // a drive started or ended: load the lists and patterns again, a moment after the first word of it (so a burst
  // loads once, and words that keep coming, every second during a rebuild, never put the load off)
  let rowsTimer = 0;
  ctx.events('rows', (ev) => {
    if (ev.data?.car && ev.data.car !== cid) return;
    if (rowsTimer) return;
    rowsTimer = setTimeout(() => { rowsTimer = 0; if (!ctx.alive()) return; table.reload(apiParams()); incomplete.reload(); loadPatterns(); }, 2000);
  });
  ctx.onCleanup(() => clearTimeout(rowsTimer));

  replace(root,
    pageHead({ eyebrow: car.name, title: 'Drives', sub: [period.label, `times in ${tz.replace(/_/g, ' ')}`],
      actions: [h('button', { class: 'btn', type: 'button', onclick: openCarSwitcher, cover: ['drives.filter.car', 'ds.filter.car'] }, icon('car'), 'Switch car'), h('a', { class: 'btn', href: ctx.link('trip') }, icon('flag'), 'This period as a trip')] }),
    filters, summary, table.el, patterns, incomplete.el, exclPop);
  paintSummary(null, 'loading');

  try { await Promise.all([table.ready, loadPatterns(), incomplete.ready]); } catch (e) { ctx.fail(e); return; }
  ctx.ready();
}
