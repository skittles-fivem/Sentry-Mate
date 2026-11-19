// Sentry Mate, "Logbook" UI pack: Drives (/cars/{car}/drives).
//
// Find and compare drives, and see driving patterns, for the shared period. Data:
// GET /api/v1/cars/{car}/drives (the list: filters, sort, cursor paging, summary, CSV), /tracks (the
// shapes of the listed drives, for the sketches and the map), /places (the place pickers),
// /drive-stats (patterns for the period, places left out), /drives?flags=gap,in_progress (drives with
// missing data), and POST /cars/{car}/rebuild for admins.
//
// Filters live in the address (?q=&places=&place_side=&min_distance_km=&min_avg_kph=&source=&flags=
// &sort=&efficiency=&exclude=), so a link or Back shows the same list. The period is the frame's.

import { h, svg, replace } from '../shared/dom.js';
import { format, parts, convert, toMetric, unitLabel } from '../shared/units.js';
import { formatTime, formatDuration, dayKey, formatDayKey, addDays, periodLabel } from '../shared/time.js';
import { formatNumber, tn } from '../shared/i18n.js';
import { downloadCsv, csvFilename, column } from '../shared/csv.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { pageHead, q, figure, icon, empty, errorView, errorWords, loadingBlock, sep, routeSketch, flag, button } from '../core/ui.js';
import { makeMap, boundsOf } from '../core/map.js';
import { confirmDialog, popover } from '../core/dialog.js';
import { choice } from '../core/form.js';
import { placeLink, endLabel } from '../core/journal.js';

export const id = 'drives';
export const css = true;
export const title = () => 'Drives';

const SORTS = [['start', 'Newest first'], ['distance', 'Longest first'], ['duration', 'Longest time first'], ['energy', 'Most energy first'], ['wh_per_km', 'Highest consumption first'], ['speed_max', 'Fastest first']];
const SOURCES = [['', 'All sources'], ['can', 'CAN Bridge only'], ['teslamate', 'Imported only']];
const FLAGS = [['', 'Every drive'], ['gap', 'With a gap in the data'], ['in_progress', 'Still going'], ['reduced_range', 'Cold battery (part locked)']];
const SIDES = [['any', 'Start or end'], ['start', 'Start'], ['end', 'End']];
const BASES = [['plain', 'As measured'], ['hill_corrected', 'Hill-corrected']];
const oneOf = (list, v, dflt) => (list.some(([k]) => k === v) ? v : dflt);
const idList = (v) => (v ? String(v).split(',').map((x) => x.trim()).filter(Boolean) : []);
const num = (v) => { const n = parseFloat(v); return Number.isFinite(n) && n > 0 ? n : null; };
/** A metric value shown in the person's unit in a number box (one decimal). */
const shown = (metric, kind) => (metric != null ? String(Math.round(convert(metric, kind) * 10) / 10) : '');

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz;
  const f = {
    q: ctx.query.q || '',
    places: idList(ctx.query.places),
    side: oneOf(SIDES, ctx.query.place_side, 'any'),
    min: num(ctx.query.min_distance_km),
    speed: num(ctx.query.min_avg_kph),
    source: oneOf(SOURCES, ctx.query.source, ''),
    flags: oneOf(FLAGS, ctx.query.flags, ''),
    sort: oneOf(SORTS, ctx.query.sort, 'start'),
    basis: oneOf(BASES, ctx.query.efficiency, 'plain'),
    exclude: idList(ctx.query.exclude),
  };
  const listParams = () => ({
    ...ctx.params, q: f.q || undefined, places: f.places.length ? f.places.join(',') : undefined, place_side: f.places.length ? f.side : undefined,
    min_distance_km: f.min || undefined, min_avg_kph: f.speed || undefined, source: f.source || undefined, flags: f.flags || undefined,
    efficiency: f.basis === 'plain' ? undefined : f.basis, sort: f.sort, dir: 'desc',
  });
  const filtered = () => !!(f.q || f.places.length || f.min || f.speed || f.source || f.flags);

  // The places the account may see (watch accounts: where their cars have been), for both pickers, with
  // this car's counts (car=). When they cannot be loaded, the ids from the address are still sent: the API
  // checks them itself, so a shared link never quietly shows unfiltered numbers.
  let places = [], placesFailed = false;
  try { places = await ctx.api.list('places', { sort: 'name', car: car.id }, { limit: 200 }).all({ max: 2000 }); } catch (err) { if (ctx.signal.aborted) throw err; placesFailed = true; }
  if (!placesFailed) {
    const known = new Set(places.map((p) => p.id));
    f.places = f.places.filter((x) => known.has(x));
    f.exclude = f.exclude.filter((x) => known.has(x));
  }

  /* ---------------------------------------------------------------- filters */

  const qIn = h('input', { class: 'field-input', type: 'search', id: 'drives-q', value: f.q, placeholder: 'A place or an address', autocomplete: 'off' });
  const minIn = h('input', { class: 'field-input field-num', type: 'number', id: 'drives-min', min: '0', step: '0.1', inputmode: 'decimal', value: shown(f.min, 'distance') });
  const speedIn = h('input', { class: 'field-input field-num', type: 'number', id: 'drives-speed', min: '0', step: '1', inputmode: 'decimal', value: shown(f.speed, 'speed') });
  const minLabel = h('label', { class: 'field-label', for: 'drives-min' });
  const speedLabel = h('label', { class: 'field-label', for: 'drives-speed' });
  const unitLabels = () => { minLabel.textContent = `At least (${unitLabel('distance')})`; speedLabel.textContent = `Average speed at least (${unitLabel('speed')})`; };
  unitLabels();
  const srcSel = h('select', { class: 'field-select', id: 'drives-source' }, SOURCES.map(([k, t]) => h('option', { value: k, selected: k === f.source }, t)));
  const flagSel = h('select', { class: 'field-select', id: 'drives-flags' }, FLAGS.map(([k, t]) => h('option', { value: k, selected: k === f.flags }, t)));
  const sortSel = h('select', { class: 'field-select', id: 'drives-sort' }, SORTS.map(([k, t]) => h('option', { value: k, selected: k === f.sort }, t)));
  let timer = 0;
  const changed = () => {
    ctx.setQuery({
      q: f.q || null, places: f.places.length ? f.places.join(',') : null, place_side: f.places.length && f.side !== 'any' ? f.side : null,
      min_distance_km: f.min || null, min_avg_kph: f.speed || null, source: f.source || null, flags: f.flags || null,
      sort: f.sort === 'start' ? null : f.sort, efficiency: f.basis === 'plain' ? null : f.basis,
    });
    clearBtn.hidden = !filtered();
    loadList().catch(() => {});
  };
  qIn.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(() => { f.q = qIn.value.trim(); changed(); }, 250); });
  ctx.onCleanup(() => clearTimeout(timer));
  minIn.addEventListener('change', () => { const n = num(minIn.value); f.min = n != null ? Math.round(toMetric(n, 'distance') * 1000) / 1000 : null; changed(); });
  speedIn.addEventListener('change', () => { const n = num(speedIn.value); f.speed = n != null ? Math.round(toMetric(n, 'speed') * 100) / 100 : null; changed(); });
  srcSel.addEventListener('change', () => { f.source = srcSel.value; changed(); });
  flagSel.addEventListener('change', () => { f.flags = flagSel.value; changed(); });
  sortSel.addEventListener('change', () => { f.sort = sortSel.value; changed(); });

  const placeFilter = placePicker(ctx, {
    id: 'drives-places', places, failed: placesFailed, chosen: f.places, none: 'Any place',
    side: { value: f.side, onchange: (v) => { f.side = v; changed(); } },
    onchange: (ids) => { f.places = ids; changed(); },
  });
  const basisChoice = choice({
    label: 'Consumption', name: 'efficiency', value: f.basis, options: BASES, cover: 'drives.setting.efficiency_method',
    onchange: (v) => { f.basis = v; basisHint.textContent = basisWords(v); changed(); },
  });
  const basisWords = (v) => (v === 'hill_corrected'
    ? 'Each drive’s Wh per distance with the energy for climbing taken off and the energy won back going down added, so hilly and flat drives compare fairly.'
    : 'Each drive’s Wh per distance as the pack measured it, regen already taken off.');
  const basisHint = h('p', { class: 'field-hint' }, basisWords(f.basis));
  const clearBtn = button('Clear the filters', {
    iconName: 'reset', cls: 'button-small',
    onclick: () => {
      Object.assign(f, { q: '', places: [], side: 'any', min: null, speed: null, source: '', flags: '' });
      qIn.value = ''; minIn.value = ''; speedIn.value = ''; srcSel.value = ''; flagSel.value = '';
      placeFilter.set([], 'any');
      changed();
    },
  });
  clearBtn.hidden = !filtered();

  const fieldOf = (label, control, opts = {}) => h('div', { class: ['field', opts.cls], cover: opts.cover }, label, control, opts.hint ? h('p', { class: 'field-hint' }, opts.hint) : null);
  const filters = h('section', { class: 'drives-filters', 'aria-label': 'Filters for the list', cover: ['drives.filter.car', 'drives.filter.time_range', 'ds.filter.car', 'ds.filter.time_range'] },
    h('div', { class: 'filter-row filter-find' },
      h('div', { class: 'field field-search', cover: ['drives.filter.location', 'drives.filter.location.hint'] }, h('label', { class: 'field-label', for: 'drives-q' }, 'Find'),
        h('div', { class: 'field-control' }, icon('search', 'icon field-icon'), qIn),
        h('p', { class: 'field-hint' }, 'Matches the start and end place, address and purpose; it only hides drives.')),
      fieldOf(h('label', { class: 'field-label', for: 'drives-places' }, 'Places'), placeFilter, { cls: 'field-places', cover: 'drives.filter.geofence', hint: 'Drives that start or end at the places you tick.' }),
      h('div', { class: 'field field-basis' }, basisChoice, basisHint)),
    h('div', { class: 'filter-row filter-narrow' },
      fieldOf(minLabel, minIn, { cover: 'drives.filter.min_distance' }),
      fieldOf(speedLabel, speedIn, { cover: 'drives.filter.min_speed' }),
      fieldOf(h('label', { class: 'field-label', for: 'drives-source' }, 'Source'), srcSel),
      fieldOf(h('label', { class: 'field-label', for: 'drives-flags' }, 'Show'), flagSel),
      fieldOf(h('label', { class: 'field-label', for: 'drives-sort' }, 'Order'), sortSel),
      h('div', { class: 'filter-clear' }, clearBtn)));

  /* ---------------------------------------------------------------- list, totals, map */

  const totals = h('div', { class: 'figs drives-totals', cover: ['drives.summary.energy', 'drives.summary.duration', 'drives.summary.distance', 'drives.summary.consumption'] });
  const listBox = h('div', { class: 'drives-list', cover: 'drives.empty_states' });
  const more = h('div', { class: 'list-more' });
  const csv = h('a', { class: 'button button-quiet button-small', download: '', href: '#' }, icon('download'), 'Download CSV');
  const count = h('p', { class: 'ledger-count', 'aria-live': 'polite' });
  const pickNote = h('span', { class: 'pick-note' });
  const pickGo = h('a', { class: 'button button-primary button-small', href: '#' }, 'Open as a trip', icon('arrowRight'));
  const pickClear = button('Clear', { cls: 'button-small', onclick: () => { picked.clear(); drawList(); } });
  const pickBar = h('div', { class: 'pick-bar', 'aria-live': 'polite' }, pickNote, pickGo, pickClear);
  const mapBox = h('div', { class: 'map map-period', role: 'region', 'aria-label': 'Map of the drives listed' });
  const mapNote = h('span', { class: 'label-note' });
  const side = h('aside', { class: 'drives-side', 'aria-label': 'The listed drives on a map' }, h('h2', { class: 'label' }, 'On the map', mapNote), mapBox);

  const patternsBody = h('div', { class: 'patterns-body', cover: 'ds.empty_states' }, loadingBlock(4));
  const excludeFilter = placePicker(ctx, {
    id: 'patterns-exclude', places, failed: placesFailed, chosen: f.exclude, none: 'No place left out', verb: 'left out',
    onchange: (ids) => { f.exclude = ids; ctx.setQuery({ exclude: ids.length ? ids.join(',') : null }); drawPatterns(ctx, patternsBody, f.exclude).catch(() => {}); },
  });
  const patterns = h('section', { class: 'block patterns', 'aria-labelledby': 'patterns-title', cover: 'shots.drive-stats' },
    h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'patterns-title' }, 'Driving patterns', h('span', { class: 'label-note patterns-span' }))),
    h('div', { class: 'patterns-filter', cover: ['ds.filter.exclude_locations', 'ds.filter.exclude_locations.hint'] },
      h('div', { class: 'field' }, h('label', { class: 'field-label', for: 'patterns-exclude' }, 'Leave out drives to or from'), excludeFilter),
      h('p', { class: 'field-hint patterns-hint' }, 'Tick home, work or any place to see the rest of your driving without those trips. Patterns count every drive of the period apart from these; the list filters above do not apply here.')),
    patternsBody);
  const missing = h('section', { class: 'block missing' });

  ctx.main.append(
    pageHead({ eyebrow: car.name, title: 'Drives', deck: 'Find and compare drives, and see driving patterns. Pick a row to open the drive, or tick several to see them as one trip.' }),
    filters, totals,
    h('div', { class: 'drives-body', cover: 'shots.drives' },
      h('div', { class: 'drives-main' },
        h('div', { class: 'ledger-top' }, count, csv),
        listBox, pickBar, more), side),
    patterns, missing);

  let pager = null, items = [], tracks = new Map(), sideMap = null, gen = 0;
  const picked = new Map();
  async function loadList() {
    const my = ++gen;
    pager = ctx.api.pager(api.apiPath`cars/${car.id}/drives`, listParams(), { limit: 50 });
    items = [];
    picked.clear();
    replace(listBox, loadingBlock(6));
    replace(more);
    csv.setAttribute('href', ctx.api.csvUrl(api.apiPath`cars/${car.id}/drives`, listParams()));
    await more1(true, my);
  }
  async function more1(first = false, my = gen) {
    try {
      const got = await pager.more();
      if (my !== gen) return;
      items = pager.items;
      if (first) drawTotals(pager.last.summary || {});
      await loadTracks(got);
      if (my !== gen) return;
      drawList();
      drawMap();
    } catch (err) {
      if (ctx.signal.aborted || my !== gen) return;
      if (first) replace(totals);
      replace(listBox, errorView(err, { retry: () => loadList().catch(() => {}) }));
      if (first) throw err;
    }
  }
  async function loadTracks(got) {
    if (!got.length) return;
    const from = Math.min(...got.map((d) => d.start_ms)), to = Math.max(...got.map((d) => d.end_ms ?? api.now())) + 1;
    try {
      const t = await ctx.api.get('tracks', { cars: car.id, from_ms: from, to_ms: to, max_points: Math.min(20000, Math.max(200, got.length * 60)) });
      for (const d of t.drives || []) tracks.set(d.drive, d.lon.map((x, i) => [x, d.lat[i]]));
    } catch { /* the sketches fall back to a straight line */ }
  }
  /** The address under a place, linked to the place editor (edit the place, or name the spot). */
  const placeCell = (p) => {
    const text = (p.address && (p.address.short || p.address.locality)) || p.label || '—';
    const href = placeLink(p);
    return href ? h('a', { class: 'quiet-link', href, title: p.place ? `Edit ${p.place.name} in Places` : 'Name this place in Places' }, text) : text;
  };
  const shapeOf = (d) => tracks.get(d.id) || [[d.start.lon, d.start.lat], [d.end.lon, d.end.lat]];

  function drawTotals(s) {
    const n = s.count || 0;
    const measured = n - (s.without_energy || 0);
    replace(totals,
      h('div', { class: 'fig totals-caption' }, h('p', { class: 'eyebrow' }, filtered() ? 'The drives listed' : 'For the period'), h('p', { class: 'totals-span' }, periodLabel(ctx.period, tz, api.now()))),
      figure('Drives', formatNumber(n), filtered() ? 'that match the filters' : null),
      figure('Distance', q(s.distance_km, 'distance'), n ? `${format(n ? s.distance_km / n : null, 'distance')} a drive on average` : null),
      figure('Driving time', n ? formatDuration(s.duration_ms || 0) : q(null, 'duration', { why: 'no drive' }), n ? `${format(s.duration_ms ? (s.distance_km / (s.duration_ms / 3600000)) : null, 'speed')} on average` : null),
      figure('Energy used', q(n ? s.energy_kwh : null, 'energy', { why: 'no drive' }), !n ? null
        : s.energy_estimated_kwh ? `${format(s.energy_measured_kwh, 'energy')} measured, ${format(s.energy_estimated_kwh, 'energy')} estimated (imported)`
          : `measured at the pack on ${tn(measured, '{n} drive', '{n} drives')}${s.without_energy ? `; ${tn(s.without_energy, '{n} has', '{n} have')} no reading` : ''}`),
      figure('Consumption', q(s.wh_per_km, 'efficiency', { why: n ? 'too short to say' : 'no drive' }), 'energy used over distance driven, as measured'));
  }

  function row(d) {
    const href = state.buildUrl('drive', { car: car.id, drive: d.id, period: ctx.state.period });
    const going = d.end_ms == null;
    const box = h('input', { type: 'checkbox', class: 'pick-box', id: `pick-${d.id}`, checked: picked.has(d.id), 'aria-label': `Add the drive from ${endLabel(d.start)} to ${endLabel(d.end)} to a trip` });
    box.addEventListener('change', () => { if (box.checked) picked.set(d.id, d); else picked.delete(d.id); drawPick(); });
    const startSoc = format(d.start_soc_pct, 'number', { digits: 0 });
    const batt = d.start_soc_pct != null || d.end_soc_pct != null ? `${startSoc} → ${format(d.end_soc_pct, 'percent')}` : q(null, 'percent');
    const energySub = d.energy_kwh == null ? '' : d.energy_basis === 'estimated' ? 'estimated' : 'measured';
    // Every figure again as a labelled line under the route; CSS shows the ones whose column the width hides,
    // so each value is on screen (once) at every width.
    const fact = (k, name, ...val) => h('span', { class: `fact fact-${k}` }, h('span', { class: 'fact-name' }, name), ' ', h('span', { class: 'fact-value' }, ...val));
    const facts = h('span', { class: 'cell-facts' },
      fact('dist', 'Distance', q(d.distance_km, 'distance')),
      fact('dur', 'Took', formatDuration(d.duration_ms)),
      fact('batt', 'Battery', batt, ' start → end'),
      fact('temp', 'Outside', q(d.outside_avg_c, 'temperature', { digits: 0 }), ' average'),
      fact('speed', 'Speed', q(d.speed_avg_kph, 'speed'), ' average, top ', format(d.speed_max_kph, 'speed')),
      fact('power', 'Power', q(d.power_max_kw, 'power', { digits: 0 }), ' peak'),
      fact('energy', 'Energy', q(d.energy_kwh, 'energy', { why: 'no reading' }), energySub ? ` ${energySub}` : ''),
      fact('eff', 'Consumption', q(d.wh_per_km, 'efficiency'), d.efficiency_pct != null ? `, ${format(d.efficiency_pct, 'percent', { digits: 0 })} of rated` : ''));
    return h('tr', { class: ['drive-row', { 'is-going': going, 'is-picked': picked.has(d.id) }], dataset: { id: d.id, href } },
      h('td', { class: 'c-pick' }, box),
      h('td', { class: 'c-time' }, `${formatTime(d.start_ms, tz)} – ${going ? 'now' : formatTime(d.end_ms, tz)}`,
        f.sort !== 'start' ? h('span', { class: 'cell-sub' }, formatDayKey(dayKey(d.start_ms, tz), { year: dayKey(d.start_ms, tz).slice(0, 4) !== dayKey(api.now(), tz).slice(0, 4) })) : null),
      h('td', { class: 'c-route' },
        h('div', { class: 'route-cell' },
          routeSketch(shapeOf(d), { cls: 'sketch row-sketch' }),
          h('div', null,
            h('a', { class: 'route-name', href }, endLabel(d.start), h('span', { class: 'to' }, ' to '), endLabel(d.end)),
            h('span', { class: 'cell-sub' },
              placeCell(d.start), going ? null : [h('span', { class: 'sep', 'aria-hidden': 'true' }, ' → '), placeCell(d.end)],
              d.source === 'teslamate' ? [' ', flag('Imported', { kind: 'estimated' })] : null,
              going ? [' ', flag('In progress')] : null,
              d.flags && (d.flags.gap_ms || d.flags.lost_frames) ? [' ', flag(d.flags.gap_ms ? `Gap ${formatDuration(d.flags.gap_ms)}` : 'Lost frames', { kind: 'warn', title: 'Part of this drive has no data; its figures cover only the time with data' })] : null,
              d.flags && d.flags.reduced_range ? [' ', flag('Cold battery', { title: 'Part of the battery was locked by the cold during this drive' })] : null),
            facts))),
      h('td', { class: 'c-num c-dist' }, q(d.distance_km, 'distance')),
      h('td', { class: 'c-num c-dur' }, formatDuration(d.duration_ms)),
      h('td', { class: 'c-num c-batt' }, batt, h('span', { class: 'cell-sub' }, 'start → end')),
      h('td', { class: 'c-num c-temp' }, q(d.outside_avg_c, 'temperature', { digits: 0 }), h('span', { class: 'cell-sub' }, 'average')),
      h('td', { class: 'c-num c-speed' }, q(d.speed_avg_kph, 'speed'), h('span', { class: 'cell-sub' }, 'top ', format(d.speed_max_kph, 'speed'))),
      h('td', { class: 'c-num c-power' }, q(d.power_max_kw, 'power', { digits: 0 }), h('span', { class: 'cell-sub' }, 'peak')),
      h('td', { class: 'c-num c-energy' }, q(d.energy_kwh, 'energy', { why: 'no reading' }), h('span', { class: 'cell-sub' }, energySub)),
      h('td', { class: 'c-num c-eff' }, q(d.wh_per_km, 'efficiency'), h('span', { class: 'cell-sub' }, d.efficiency_pct != null ? `${format(d.efficiency_pct, 'percent', { digits: 0 })} of rated` : '')),
      h('td', { class: 'c-go' }, icon('chevronRight')));
  }
  const COLS = [['pick', '', false], ['time', 'Time', false], ['route', 'From and to', false], ['dist', 'Distance', true], ['dur', 'Duration', true], ['batt', 'Battery', true], ['temp', 'Outside', true], ['speed', 'Speed', true], ['power', 'Power', true], ['energy', 'Energy', true], ['eff', 'Consumption', true], ['go', '', false]];
  const colHead = ([k, t, isNum]) => h('th', { scope: 'col', class: [`c-${k}`, { 'c-num': isNum }] },
    k === 'go' ? h('span', { class: 'sr-only' }, 'Open') : k === 'pick' ? h('span', { class: 'sr-only' }, 'Pick for a trip')
      : k === 'eff' && f.basis === 'hill_corrected' ? [t, h('span', { class: 'th-sub' }, 'hill-corrected')] : t);

  function emptyWhy() {
    const bits = [];
    if (f.q) bits.push(`matches “${f.q}”`);
    if (f.places.length) bits.push(`${f.side === 'start' ? 'starts at' : f.side === 'end' ? 'ends at' : 'starts or ends at'} ${placeFilter.summary()}`);
    if (f.min) bits.push(`is ${format(f.min, 'distance')} or longer`);
    if (f.speed) bits.push(`averages ${format(f.speed, 'speed')} or more`);
    if (f.source) bits.push(f.source === 'can' ? 'comes from the CAN Bridge' : 'was imported');
    if (f.flags) bits.push({ gap: 'has a gap in its data', in_progress: 'is still going', reduced_range: 'had a cold-locked battery' }[f.flags]);
    return bits;
  }
  function drawList() {
    const n = items.length;
    count.textContent = pager.done ? tn(n, '{n} drive', '{n} drives') : `${formatNumber(n)} drives shown, more to load`;
    csv.hidden = !n;
    drawPick();
    if (!n) {
      const bits = emptyWhy();
      const span = `this period (${periodLabel(ctx.period, tz, api.now())})`;
      replace(listBox, empty({
        title: bits.length ? `No drive in ${span} ${bits.join(' and ')}.` : `${car.name} made no drive in ${span}.`,
        text: bits.length ? `Loosen ${bits.length > 1 ? 'one of these filters' : 'this filter'} or clear the filters; a longer period in the picker at the top may help too.` : 'Choose a longer period in the period picker at the top.',
        action: bits.length ? button('Clear the filters', { kind: 'primary', onclick: () => clearBtn.click() }) : null,
      }));
      replace(more);
      return;
    }
    const head = h('thead', null, h('tr', null, COLS.map(colHead)));
    let bodies;
    if (f.sort === 'start') {
      const groups = new Map();
      for (const d of items) { const k = dayKey(d.start_ms, tz); if (!groups.has(k)) groups.set(k, []); groups.get(k).push(d); }
      const today = dayKey(api.now(), tz);
      bodies = [...groups.entries()].map(([k, list]) => {
        const km = list.reduce((a, d) => a + (d.distance_km || 0), 0), kwh = list.reduce((a, d) => a + (d.energy_kwh || 0), 0);
        const name = k === today ? 'Today' : k === addDays(today, -1) ? 'Yesterday' : formatDayKey(k, { weekday: 'long', month: 'long', year: k.slice(0, 4) !== today.slice(0, 4) });
        return h('tbody', { class: 'day', id: `drives-day-${k}` },
          h('tr', { class: 'day-row' },
            h('th', { colspan: 3, scope: 'rowgroup' },
              h('span', { class: 'day-name' }, name, name === 'Today' || name === 'Yesterday' ? h('span', { class: 'day-date' }, formatDayKey(k, { weekday: 'long', month: 'long', year: false })) : null),
              h('span', { class: 'day-sum' }, h('span', { class: 'day-sum-text' }, tn(list.length, '{n} drive', '{n} drives'), sep(), format(km, 'distance'), sep(), format(kwh, 'energy')))),
            COLS.slice(3).map(([c]) => h('td', { class: `c-${c}`, 'aria-hidden': 'true' }))),
          list.map(row));
      });
    } else {
      bodies = [h('tbody', null, items.map(row))];
    }
    const table = h('table', { class: 'drives', cover: ['drives.table', 'drives.table.date', 'drives.table.start', 'drives.table.destination', 'drives.table.duration', 'drives.table.distance', 'drives.table.soc_start', 'drives.table.soc_end', 'drives.table.outside_temp', 'drives.table.speed_avg', 'drives.table.speed_max', 'drives.table.power_max', 'drives.table.reduced_range', 'drives.table.efficiency', 'drives.table.energy', 'drives.table.consumption', 'drives.table.link.details', 'drives.table.link.geofence_start', 'drives.table.link.geofence_end'] },
      h('caption', { class: 'sr-only' }, 'Drives. Select a drive to open it, or tick it to add it to a trip.'), head, bodies);
    table.addEventListener('click', (e) => {
      if (e.target.closest('a, input, label, .c-pick')) return;
      const tr = e.target.closest('tr[data-href]');
      if (tr) state.navigate(tr.dataset.href);
    });
    table.addEventListener('mouseover', (e) => hover(e.target.closest('tr[data-id]')?.dataset.id || ''));
    table.addEventListener('mouseleave', () => hover(''));
    replace(listBox, table);
    if (!pager.done) {
      const btn = button('Show earlier drives', { kind: 'primary', onclick: async () => { btn.disabled = true; btn.textContent = 'Loading…'; await more1(); } });
      replace(more, h('p', { class: 'list-more-note' }, `Showing ${formatNumber(n)} drives; the CSV has every one.`), btn);
    } else replace(more, h('p', { class: 'list-more-note' }, n > 1 ? `All ${formatNumber(n)} drives are shown.` : 'The only drive.'));
  }

  // Ticked drives open as one trip: the Trip page for the span from the first start to the last end.
  function drawPick() {
    const list = [...picked.values()];
    pickBar.hidden = !list.length;
    for (const tr of listBox.querySelectorAll('tr[data-id]')) tr.classList.toggle('is-picked', picked.has(tr.dataset.id));
    if (!list.length) return;
    const from = Math.min(...list.map((d) => d.start_ms)), to = Math.max(...list.map((d) => d.end_ms ?? api.now()));
    pickNote.textContent = `${tn(list.length, '{n} drive', '{n} drives')} ticked, ${format(list.reduce((a, d) => a + (d.distance_km || 0), 0), 'distance')}; the trip covers everything from ${formatDayKey(dayKey(from, tz), { year: false })} ${formatTime(from, tz)} to ${formatDayKey(dayKey(to, tz), { year: false })} ${formatTime(to, tz)}.`;
    pickGo.setAttribute('href', state.buildUrl('trip', { car: car.id, period: { from, to: to + 1 } }));
  }

  // The listed drives on a map, beside the list on wide screens (no map is made while it is hidden).
  let hovered = '';
  function hover(hid) { if (hid === hovered) return; hovered = hid; if (sideMap) sideMap.map.setFilter('period-hover', ['==', ['get', 'id'], hid]); }
  async function drawMap() {
    if (!side.clientWidth) return;
    const lines = items.map((d) => ({ id: d.id, c: shapeOf(d) })).filter((x) => x.c.length > 1 && x.c.every((p) => p[0] != null));
    mapNote.textContent = !items.length ? 'no drive listed' : pager.done ? tn(items.length, 'the one drive', 'all {n} drives') : `the ${formatNumber(items.length)} drives listed`;
    const fc = { type: 'FeatureCollection', features: lines.map((x) => ({ type: 'Feature', properties: { id: x.id }, geometry: { type: 'LineString', coordinates: x.c } })) };
    if (sideMap) { sideMap.map.getSource('period')?.setData(fc); if (lines.length) sideMap.frame(boundsOf(lines.flatMap((x) => x.c))); return; }
    if (!lines.length) return;
    const m = await makeMap(ctx, mapBox, { bounds: boundsOf(lines.flatMap((x) => x.c)), padding: 28, maxZoom: 15 });
    if (!m) return;
    await m.loaded;
    if (ctx.signal.aborted) return;
    m.map.addSource('period', { type: 'geojson', data: fc });
    m.addLine({ id: 'period-line', source: 'period', paint: { 'line-opacity': 0.8 } }, 2.2, '--ink-3');
    m.addLine({ id: 'period-hover', source: 'period', filter: ['==', ['get', 'id'], ''] }, 4.5, '--accent');
    sideMap = m;
  }

  const spanText = () => { patterns.querySelector('.patterns-span').textContent = periodLabel(ctx.period, tz, api.now()); };
  spanText();
  await Promise.all([loadList(), drawPatterns(ctx, patternsBody, f.exclude), drawMissing(ctx, missing)]);
  ctx.ready();
  return {
    async update(c, why) {
      if (why && why.prefs) {
        unitLabels();
        minIn.value = shown(f.min, 'distance');
        speedIn.value = shown(f.speed, 'speed');
      }
      tracks = new Map();
      replace(totals);
      spanText();
      await Promise.all([loadList(), drawPatterns(c, patternsBody, f.exclude), drawMissing(c, missing)]);
      c.ready();
    },
  };
}

/* ------------------------------------------------------------------ place picker */

/**
 * A pick list of places in a small menu: a button that says what is chosen, and under it a search box and
 * a ticked list (any number of places). side adds the start / end / either choice. el.set(ids, side),
 * el.summary() for sentences.
 */
function placePicker(ctx, { id, places, failed, chosen, none, verb = null, side = null, onchange }) {
  let sel = new Set(chosen);
  let sideValue = side ? side.value : 'any';
  const byId = new Map(places.map((p) => [p.id, p]));
  const trigger = h('button', { type: 'button', class: 'field-select pick-trigger', id, disabled: failed || !places.length });
  const search = h('input', { class: 'field-input pick-search', type: 'search', placeholder: 'Find a place', 'aria-label': 'Find a place', autocomplete: 'off' });
  const list = h('ul', { class: 'pick-list' });
  const more = h('p', { class: 'pick-more' });
  const sideChoice = side ? choice({ label: 'Where the drive', name: `${id}-side`, value: sideValue, options: SIDES, cls: 'pick-side', onchange: (v) => { sideValue = v; text(); side.onchange(v); } }) : null;
  let timer = 0;
  ctx.onCleanup(() => clearTimeout(timer));
  const fire = () => { clearTimeout(timer); timer = setTimeout(() => onchange([...sel]), 350); };
  const names = () => [...sel].map((x) => byId.get(x)?.name).filter(Boolean);
  const summary = () => {
    const n = names();
    if (!n.length && sel.size) return tn(sel.size, 'the place in the link', 'the {n} places in the link');
    return n.length <= 2 ? n.join(' or ') : `${n[0]}, ${n[1]} or ${tn(n.length - 2, '{n} other place', '{n} other places')}`;
  };
  function text() {
    const n = names();
    trigger.textContent = failed ? (sel.size ? `${tn(sel.size, '{n} place', '{n} places')} from the link ${verb || 'applied'} (names could not be loaded)` : 'Places could not be loaded')
      : !places.length ? 'No places saved yet' : !n.length ? none
      : (n.length === 1 ? n[0] : `${n[0]} and ${formatNumber(n.length - 1)} more`) + (verb ? ` ${verb}` : side && sideValue !== 'any' ? ` (${sideValue === 'start' ? 'start' : 'end'})` : '');
  }
  function draw() {
    const s = search.value.trim().toLowerCase();
    const where = (p) => [p.address && p.address.short, p.address && p.address.locality].filter(Boolean).join(', ');
    const found = places.filter((p) => !s || p.name.toLowerCase().includes(s) || where(p).toLowerCase().includes(s));
    // Ticked places first, then this car's places, then by name; at most 60 shown (type to find the rest).
    const mine = (p) => (p.counts && (p.counts.drives || p.counts.visits) ? 1 : 0);
    found.sort((a, b) => (sel.has(b.id) - sel.has(a.id)) || (mine(b) - mine(a)) || a.name.localeCompare(b.name));
    replace(list, found.slice(0, 60).map((p) => {
      const box = h('input', { type: 'checkbox', id: `${id}-${p.id}`, checked: sel.has(p.id) });
      box.addEventListener('change', () => { if (box.checked) sel.add(p.id); else sel.delete(p.id); text(); fire(); });
      return h('li', null, h('label', { class: 'pick-item', for: `${id}-${p.id}` }, box,
        h('span', { class: 'pick-text' }, h('span', { class: 'pick-name' }, p.name), where(p) ? h('span', { class: 'pick-where' }, where(p)) : null),
        h('span', { class: 'pick-count' }, p.counts ? tn(p.counts.drives || 0, '{n} drive', '{n} drives') : '', h('span', { class: 'pick-count-of' }, `by ${ctx.car.name}`))));
    }));
    more.textContent = !found.length ? 'No place has that name.' : found.length > 60 ? `${formatNumber(found.length - 60)} more; type to narrow the list.` : '';
  }
  search.addEventListener('input', draw);
  const panel = h('div', { class: 'pop pick-pop', id: `${id}-menu`, role: 'group', 'aria-label': 'Places', tabindex: '-1' },
    search, list, more, sideChoice,
    h('div', { class: 'pick-foot' },
      button('Clear', { cls: 'button-small', onclick: () => { sel.clear(); draw(); text(); fire(); } }),
      button('Done', { kind: 'primary', cls: 'button-small', onclick: () => pop.close({ refocus: true }) })));
  const wrap = h('div', { class: 'pop-wrap pick-wrap' }, trigger, panel);
  const pop = popover(trigger, panel, { onOpen: draw, focus: '.pick-search' });
  text();
  wrap.set = (ids, sv = 'any') => { sel = new Set(ids); sideValue = sv; sideChoice?.set(sv); text(); };
  wrap.summary = summary;
  return wrap;
}

/* ------------------------------------------------------------------ driving patterns */

function csvButton(label, onclick) {
  return h('button', { type: 'button', class: 'button button-quiet button-small csv-button', onclick, 'aria-label': `Download CSV: ${label}` }, icon('download'), 'CSV');
}

/** The error of one part of the page (the rest loaded): what failed, why, and Try again. */
function partError(what, err, retry) {
  const w = errorWords(err);
  const why = err && err.status && ![0, 401, 403, 404, 429].includes(err.status) ? ((err && err.message) || String(err)) : `${w.title} ${w.text}`;
  return h('div', { class: 'part-error', role: 'alert' },
    h('p', { class: 'part-error-text' }, icon('warning'), h('span', null, `${what}: `, why)),
    button('Try again', { onclick: retry, iconName: 'reset', cls: 'button-small' }));
}

/** Every local day of the period (from the API's first day when it sent a whole series), with no-drive days as 0. */
function wholeDays(ctx, s) {
  const tz = ctx.tz, now = api.now(), got = s.per_day || [];
  const byDate = new Map(got.map((d) => [d.date, d]));
  const p = ctx.period || {};
  const end = Math.min(p.to_ms ?? now, now);
  const last = dayKey(Math.max(end - 1, p.from_ms ?? -Infinity), tz);
  const whole = got.length && s.days === got.length;
  let k = whole ? got[0].date : p.from_ms != null ? dayKey(p.from_ms, tz) : (got[0] ? got[0].date : last);
  const out = [];
  for (let i = 0; k <= last && i < 40000; i++, k = addDays(k, 1)) out.push(byDate.get(k) || { date: k, count: 0, distance_km: 0, energy_kwh: 0 });
  return out;
}

async function drawPatterns(ctx, box, exclude) {
  const car = ctx.car, tz = ctx.tz;
  let s;
  try {
    s = await ctx.api.get(api.apiPath`cars/${car.id}/drive-stats`, { ...ctx.params, exclude_places: exclude.length ? exclude.join(',') : undefined });
  } catch (err) { if (!ctx.signal.aborted) replace(box, partError('The driving patterns could not be loaded', err, () => { replace(box, loadingBlock(4)); drawPatterns(ctx, box, exclude); })); return; }
  const span = periodLabel(ctx.period, tz, api.now());
  const file = (what) => csvFilename(car.name, what, span);
  const days = wholeDays(ctx, s);
  const maxKm = Math.max(0.1, ...days.map((d) => d.distance_km || 0));
  const w = days.length ? 100 / days.length : 100;
  const bars = svg('svg', { class: 'by-day-svg', width: '100%', role: 'img', 'aria-label': `Distance per day: ${days.map((d) => `${d.date} ${format(d.distance_km, 'distance')}`).join(', ')}` },
    svg('line', { class: 'by-day-base', x1: 0, x2: '100%', y1: '78%', y2: '78%' }),
    days.map((d, i) => {
      const hp = ((d.distance_km || 0) / maxKm) * 62;
      return svg('g', { class: ['by-day-bar', { 'is-empty': !d.count }] },
        svg('rect', { class: 'by-day-fill', x: `${(i * w + w * 0.18).toFixed(3)}%`, y: `${(78 - Math.max(hp, 0.6)).toFixed(2)}%`, width: `${(w * 0.64).toFixed(3)}%`, height: `${Math.max(hp, 0.6).toFixed(2)}%` }),
        days.length <= 14 && d.count ? svg('text', { class: 'by-day-value', x: `${(i * w + w / 2).toFixed(3)}%`, y: `${(74 - hp).toFixed(2)}%`, 'text-anchor': 'middle' }, parts(d.distance_km, 'distance').number) : null,
        i % Math.ceil(days.length / 10) === 0 ? svg('text', { class: 'by-day-name', x: `${(i * w + w / 2).toFixed(3)}%`, y: '93%', 'text-anchor': 'middle' }, formatDayKey(d.date, { weekday: days.length <= 7, month: 'short', year: false })) : null,
        svg('title', null, `${formatDayKey(d.date, { year: false })}: ${tn(d.count, '{n} drive', '{n} drives')}, ${format(d.distance_km, 'distance')}, ${format(d.energy_kwh, 'energy')}`));
    }));
  const band = (b) => `${parts(Math.max(0, b.from_kph), 'speed').number}–${format(b.to_kph, 'speed')}`;
  const bands = (s.speed_bands || []).filter((b) => b.to_kph > 0);
  const maxShare = Math.max(1, ...bands.map((b) => b.share_pct || 0));
  const top = (s.top_destinations || []).slice(0, 8);
  const maxTop = Math.max(1, ...top.map((d) => d.count || 0));
  const ms = s.max_speed || {};
  const speedLink = (x) => (x && x.drive ? h('a', { href: state.buildUrl('drive', { car: car.id, drive: x.drive, period: ctx.state.period }) }, format(x.kph, 'speed')) : q(null, 'speed', { why: 'no drive' }));
  const destHref = (d) => state.buildUrl('drives', { car: car.id, period: ctx.state.period, query: { places: d.place, place_side: 'end' } });
  const perDay = (v) => (days.length ? v / days.length : null);
  const proj = s.projection || {};

  if (!s.count) {
    replace(box, empty({
      title: exclude.length ? 'Every drive in this period starts or ends at a place left out.' : 'No drives in this period, so there are no patterns to show.',
      text: exclude.length ? 'Untick a place under “Leave out drives to or from”, or choose a longer period.' : 'Choose a longer period at the top.',
    }));
    return;
  }
  replace(box,
    h('div', { class: 'figs patterns-figs', cover: ['ds.stat.drive_count', 'ds.stat.distance_total', 'ds.stat.energy_total', 'ds.stat.median_distance', 'ds.stat.distance_per_day', 'ds.stat.energy_per_day', 'ds.stat.monthly_mileage', 'ds.stat.annual_mileage'] },
      figure('Drives', formatNumber(s.count), exclude.length ? 'after the places left out' : 'in the period'),
      figure('Distance', q(s.distance_km, 'distance'), 'summed over the drives'),
      figure('Energy', q(s.energy_kwh, 'energy'), 'used while driving, at the pack'),
      figure('Median drive', q(s.median_distance_km, 'distance'), 'half the drives are shorter'),
      figure('Per day', q(perDay(s.distance_km), 'distance'), `and ${format(perDay(s.energy_kwh), 'energy')}, over ${tn(days.length, '{n} day', '{n} days')} with or without drives`),
      figure('Projected', q(proj.per_month_km, 'distance'), proj.per_month_km != null ? `a month; ${format(proj.per_year_km, 'distance')} a year, from the last ${tn(Math.round(proj.basis_days || 0), '{n} day', '{n} days')} of driving` : 'needs more days of driving')),
    h('div', { class: 'patterns-grid' },
      h('figure', { class: 'by-day', cover: 'ds.stat.distance_per_day' },
        h('div', { class: 'part-head' }, h('figcaption', { class: 'label' }, 'Distance by day'),
          csvButton('distance by day', () => downloadCsv(file('distance by day'), days, [
            { label: 'Date', value: (d) => d.date }, column('Drives', 'count'), column('Distance', 'distance_km', 'distance', { digits: 2 }), column('Energy', 'energy_kwh', 'energy', { digits: 2 })]))),
        bars),
      h('div', { class: 'speed-bands', cover: 'ds.chart.speed_histogram' },
        h('div', { class: 'part-head' }, h('p', { class: 'label' }, 'Time in each speed band'),
          csvButton('time in each speed band', () => downloadCsv(file('speed bands'), bands, [
            column('From', 'from_kph', 'speed', { digits: 1 }), column('To', 'to_kph', 'speed', { digits: 1 }), column('Seconds', 'seconds'), column('Share', 'share_pct', 'percent', { digits: 1 })]))),
        bands.length ? h('ul', { class: 'band-list' }, bands.map((b) => h('li', { class: 'band-row' },
          h('span', { class: 'band-name' }, band(b)),
          h('span', { class: 'band-bar' }, svg('svg', { class: 'band-svg', width: '100%', height: '100%', preserveAspectRatio: 'none', 'aria-hidden': 'true' }, svg('rect', { class: 'band-fill', x: 0, y: 0, width: `${((b.share_pct || 0) / maxShare) * 100}%`, height: '100%' }))),
          h('span', { class: 'band-value' }, formatDuration((b.seconds || 0) * 1000), ' · ', format(b.share_pct, 'percent', { digits: 0 })))))
          : h('p', { class: 'note' }, 'No speed readings in these drives (imported drives have none).'),
        bands.length ? h('p', { class: 'note bands-note' }, 'Counted second by second from the speed the car reported; the share is of all driving time.') : null),
      h('div', { class: 'top-dest' },
        h('div', { class: 'part-head' }, h('p', { class: 'label' }, 'Top speed'),
          csvButton('top speed', () => downloadCsv(file('top speed'), [['period', span], ['last_30d', 'Last 30 days'], ['last_7d', 'Last 7 days']].map(([k, t]) => ({ t, ...(ms[k] || {}) })), [
            { label: 'Span', value: (r) => r.t }, column('Top speed', 'kph', 'speed', { digits: 1 }), { label: 'Drive', value: (r) => r.drive || '' }]))),
        h('dl', { class: 'spec', cover: ['ds.stat.max_speed_period', 'ds.stat.max_speed_30d', 'ds.stat.max_speed_7d'] },
          h('div', { class: 'spec-row' }, h('dt', null, 'In the period'), h('dd', null, speedLink(ms.period))),
          h('div', { class: 'spec-row' }, h('dt', null, 'Last 30 days'), h('dd', null, speedLink(ms.last_30d))),
          h('div', { class: 'spec-row' }, h('dt', null, 'Last 7 days'), h('dd', null, speedLink(ms.last_7d)))),
        h('p', { class: 'note top-note' }, 'The last 30 and 7 days count back from now, whatever the period.'),
        h('div', { class: 'part-head top-dest-label' }, h('p', { class: 'label' }, 'Most frequent destinations'),
          csvButton('most frequent destinations', () => downloadCsv(file('destinations'), top, [
            { label: 'Destination', value: (d) => d.label }, { label: 'Place id', value: (d) => d.place || '' }, column('Drives', 'count')]))),
        top.length ? h('ol', { class: 'dest-list', cover: 'ds.chart.top_destinations' }, top.map((d) => h('li', { class: 'dest-row' },
          d.place ? h('a', { class: 'dest-name', href: destHref(d), title: `List the drives that end at ${d.label}` }, d.label) : h('span', { class: 'dest-name' }, d.label),
          h('span', { class: 'dest-bar' }, svg('svg', { class: 'band-svg', width: '100%', height: '100%', preserveAspectRatio: 'none', 'aria-hidden': 'true' }, svg('rect', { class: 'dest-fill', x: 0, y: 0, width: `${((d.count || 0) / maxTop) * 100}%`, height: '100%' }))),
          h('span', { class: 'dest-count' }, tn(d.count, '{n} drive', '{n} drives')))))
          : h('p', { class: 'note', cover: 'ds.chart.top_destinations' }, 'No destination yet.'))));
}

/* ------------------------------------------------------------------ drives with missing data */

async function drawMissing(ctx, box) {
  const car = ctx.car, tz = ctx.tz;
  const params = { ...ctx.params, flags: 'gap,in_progress' };
  let pg;
  try { pg = await ctx.api.page(api.apiPath`cars/${car.id}/drives`, params, { limit: 20 }); } catch (err) {
    if (!ctx.signal.aborted) replace(box, h('div', { class: 'block-head' }, h('h2', { class: 'label' }, 'Drives with missing data')), partError('The drives with missing data could not be loaded', err, () => { replace(box, loadingBlock(3)); drawMissing(ctx, box); }));
    return;
  }
  const list = pg.items || [];
  const admin = ctx.me && ctx.me.role === 'admin';
  const rebuild = admin && list.length ? button('Rebuild these drives from the stored logs', {
    onclick: async () => {
      if (!(await confirmDialog({ title: 'Rebuild drives?', text: 'Sentry Mate reads the stored logs again for this period and rebuilds its drives, charges, states and stops. Purposes and costs you typed are kept.', yes: 'Rebuild' }))) return;
      try {
        const r = await ctx.api.post(api.apiPath`cars/${car.id}/rebuild`, { from_ms: ctx.params.from_ms ?? list[list.length - 1].start_ms, to_ms: ctx.params.to_ms ?? api.now() });
        ctx.flash(`Rebuilt from ${tn(r.logs_read || 0, '{n} log', '{n} logs')} in ${formatDuration(r.took_ms || 0)}.`);
      } catch (err) { ctx.flash(`The rebuild did not start: ${err.message}`, { kind: 'error' }); }
    },
  }) : null;
  const csv = list.length ? h('a', { class: 'button button-quiet button-small', download: '', href: ctx.api.csvUrl(api.apiPath`cars/${car.id}/drives`, params) }, icon('download'), 'Download CSV') : null;
  const why = (d) => (d.flags.in_progress ? 'Still going: the figures grow as logs arrive'
    : d.flags.gap_ms ? `${formatDuration(d.flags.gap_ms)} without data${d.flags.lost_frames ? `, ${formatNumber(d.flags.lost_frames)} frames lost` : ''}`
      : `${formatNumber(d.flags.lost_frames || 0)} frames lost`);
  replace(box,
    h('div', { class: 'block-head' }, h('h2', { class: 'label' }, 'Drives with missing data', h('span', { class: 'label-note' }, list.length ? (pg.next ? `the first ${formatNumber(list.length)}` : tn(list.length, '{n} drive', '{n} drives')) : 'none')),
      h('div', { class: 'block-actions' }, csv, rebuild)),
    h('p', { class: 'note', cover: 'drives.incomplete.note' }, 'A drive is listed here when it overlaps time with no data (a gap, lost frames) or is still going. Its figures cover only the time with data; a late log or a rebuild fills them in.'),
    list.length ? h('div', { class: 'missing-scroll', role: 'region', 'aria-label': 'Drives with missing data', tabindex: '0' }, h('table', { class: 'ledger-table missing-table', cover: ['drives.incomplete.table', 'drives.incomplete.id', 'drives.incomplete.start', 'drives.incomplete.end', 'drives.incomplete.distance', 'drives.incomplete.duration'] },
      h('thead', null, h('tr', null, ['Drive', 'Start', 'End', 'Distance', 'Duration', 'What is missing'].map((t, i) => h('th', { scope: 'col', class: { num: i === 3 || i === 4 } }, t)))),
      h('tbody', null, list.map((d) => h('tr', null,
        h('td', null, h('a', { href: state.buildUrl('drive', { car: car.id, drive: d.id, period: ctx.state.period }) }, `${endLabel(d.start)} to ${endLabel(d.end)}`), h('span', { class: 'cell-sub' }, `Drive ${d.id}`)),
        h('td', null, `${formatDayKey(dayKey(d.start_ms, tz), { year: false })}, ${formatTime(d.start_ms, tz)}`),
        h('td', null, d.end_ms ? formatTime(d.end_ms, tz) : 'still going'),
        h('td', { class: 'num' }, q(d.distance_km, 'distance')),
        h('td', { class: 'num' }, formatDuration(d.duration_ms)),
        h('td', null, why(d)))))))
      : h('p', { class: 'note-serif' }, 'Every drive in this period has all its data.'));
}
