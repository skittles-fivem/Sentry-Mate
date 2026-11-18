// Drives (/cars/{car}/drives): find and compare drives, and see driving patterns. One filter bar drives
// the list (GET /cars/{car}/drives, sorted and paged by the server, CSV), its summary, the drives with
// missing data, and the patterns (GET /cars/{car}/drive-stats, with places left out from a pick list).
// Energy is measured at the pack; imported drives say "estimated". Ticked drives open as one trip.
// Live: a drive that starts or ends ("rows" events) loads the lists and patterns again.

import * as state from '../shared/state.js';
import { h, replace, setVars, quantity } from '../shared/dom.js';
import { downloadCsv, csvFilename, column } from '../shared/csv.js';
import { format, toMetric, unitLabel, convert } from '../shared/units.js';
import { formatTime, formatDate, formatDuration } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon } from '../core/icons.js';
import { pageHead, metric, tag, csvButton } from '../core/page.js';
import { dataTable } from '../core/table.js';
import { emptyState } from '../core/states.js';
import { barChart } from '../core/chart.js';
import { openCarSwitcher } from '../core/carswitch.js';
import { openPeriodPicker } from '../core/period.js';
import { placePopover } from '../core/popover.js';
import { isAdmin } from '../core/prefs.js';
import { flash } from '../core/flash.js';

export const id = 'drives';
export const css = true;

const placeLabel = (p) => p?.place?.name || p?.label || p?.address?.short || '—';
const placeHref = (p) => (p?.place?.id ? state.buildUrl('places', { query: { place: p.place.id } })
  : p?.lat != null ? state.buildUrl('places', { query: { lat: p.lat.toFixed(5), lon: p.lon.toFixed(5), new: 1 } }) : null);
const placeCell = (p, cover) => {
  const href = placeHref(p);
  return href ? h('a', { class: 'tl-place-link', href, title: p.place ? `Edit the place ${p.place.name}` : 'Make this spot a place' }, placeLabel(p)) : h('span', null, placeLabel(p));
};

export async function mount(ctx) {
  const { car, tz, period } = ctx;
  const cid = car.id;
  const path = `cars/${encodeURIComponent(cid)}/drives`;
  const q = ctx.query;
  const f = {
    places: q.places || '', place_side: q.place_side || 'any', q: q.q || '', min_distance: q.min_distance || '', min_speed: q.min_speed || '',
    source: q.source || '', flags: q.flags || '', efficiency: q.efficiency || 'plain', exclude: q.exclude || '',
  };
  const apiParams = () => ({
    ...ctx.params,
    places: f.places || null, place_side: f.places ? f.place_side : null, q: f.q || null,
    min_distance_km: f.min_distance ? +toMetric(+f.min_distance, 'distance').toFixed(3) : null,
    min_avg_kph: f.min_speed ? +toMetric(+f.min_speed, 'speed').toFixed(2) : null,
    source: f.source || null, flags: f.flags || null, efficiency: f.efficiency,
  });

  const root = h('div', { class: 'drives-page', cover: ['shots.drives', 'shots.drive-stats'] });
  replace(ctx.view, root);

  // places for the filters (the most visited first)
  let places = [];
  try { places = (await ctx.api.page('places', { sort: 'visits' }, { limit: 200 })).items; } catch { places = []; }
  if (!ctx.alive()) return;
  // two places may share a name (each car's Home): the drive count and, for twins, the position tell them apart
  const twins = new Set(places.map((p) => p.name).filter((n, i, a) => a.indexOf(n) !== i));
  const placeSub = (p) => [p.counts?.drives != null ? `${formatNumber(p.counts.drives)} ${p.counts.drives === 1 ? 'drive' : 'drives'}` : null,
    twins.has(p.name) && p.lat != null ? `${p.lat.toFixed(3)}, ${p.lon.toFixed(3)}` : null].filter(Boolean).join(' · ');
  const placeOpts = (sel) => places.map((p) => h('option', { value: p.id, selected: sel.split(',').includes(p.id) }, twins.has(p.name) ? `${p.name} (${placeSub(p)})` : p.name));

  /* ---------- filters */
  const placeSel = h('select', { class: 'select', 'aria-label': 'Place' }, h('option', { value: '' }, 'Any place'), placeOpts(f.places));
  const sideSel = h('select', { class: 'select', 'aria-label': 'Where the place is', title: 'Match the place at the start, the end, or either' },
    [['any', 'start or end'], ['start', 'start'], ['end', 'end']].map(([v, l]) => h('option', { value: v, selected: f.place_side === v }, `at the ${l}`)));
  const textIn = h('input', { class: 'input', type: 'search', value: f.q, placeholder: 'Start or destination', 'aria-label': 'Text in the start or destination',
    title: 'Keeps drives whose start or destination (place name or address) contains this text.' });
  const distIn = h('input', { class: 'input input--sm', type: 'number', min: '0', step: '0.1', value: f.min_distance, 'aria-label': `Shortest drive (${unitLabel('distance')})` });
  const speedIn = h('input', { class: 'input input--sm', type: 'number', min: '0', step: '1', value: f.min_speed, 'aria-label': `Lowest average speed (${unitLabel('speed')})` });
  const sourceSel = h('select', { class: 'select', 'aria-label': 'Source' }, [['', 'Every source'], ['can', 'CAN Bridge'], ['teslamate', 'Imported']].map(([v, l]) => h('option', { value: v, selected: f.source === v }, l)));
  const flagSel = h('select', { class: 'select', 'aria-label': 'Flags' }, [['', 'Every drive'], ['gap', 'With a gap'], ['in_progress', 'In progress'], ['reduced_range', 'Cold battery']].map(([v, l]) => h('option', { value: v, selected: f.flags === v }, l)));
  const effSel = h('select', { class: 'select', 'aria-label': 'Efficiency basis', title: 'Plain: distance against the range used. Hill corrected: the climb and descent taken off, using a typical mass for the model.' },
    [['plain', 'Efficiency: plain'], ['hill_corrected', 'Efficiency: hill corrected']].map(([v, l]) => h('option', { value: v, selected: f.efficiency === v }, l)));
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
    h('div', { class: 'excl-actions' }, h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: () => { excluded.clear(); paintExcl(); apply(true); } }, 'Leave nothing out'),
      h('button', { class: 'btn btn--sm', type: 'button', onclick: () => exclPop.hidePopover() }, 'Done')));
  const paintExclText = () => {
    const names = places.filter((p) => excluded.has(p.id)).map((p) => p.name);
    exclText.textContent = names.length ? `Patterns without ${names.length > 2 ? `${names.length} places` : names.join(', ')}` : 'Patterns: every place';
    exclBtn.classList.toggle('is-on', names.length > 0);
  };
  const paintExcl = () => {
    const needle = exclFind.value.trim().toLowerCase();
    const shown = places.filter((p) => !needle || p.name.toLowerCase().includes(needle));
    replace(exclList, shown.length ? shown.map((p) => h('label', { class: 'check' },
      h('input', { type: 'checkbox', checked: excluded.has(p.id), onchange: (e) => { if (e.target.checked) excluded.add(p.id); else excluded.delete(p.id); paintExclText(); apply(true); } }),
      h('span', null, p.name), h('span', { class: 'faint excl-sub' }, placeSub(p)))) : h('p', { class: 'note' }, places.length ? 'No place has that name.' : 'No place is saved yet. Add places on the Places page to leave them out here.'));
    paintExclText();
  };
  exclFind.addEventListener('input', paintExcl);
  exclBtn.addEventListener('click', () => {
    if (exclPop.matches(':popover-open')) { exclPop.hidePopover(); return; }
    exclPop.showPopover(); placePopover(exclPop, exclBtn, { align: 'start' });
  });
  exclPop.addEventListener('toggle', (e) => exclBtn.setAttribute('aria-expanded', String(e.newState === 'open')));
  paintExcl();

  let timer = 0;
  const apply = (now = false) => {
    clearTimeout(timer);
    timer = setTimeout(() => {
      Object.assign(f, { places: placeSel.value, place_side: sideSel.value, q: textIn.value.trim(), min_distance: distIn.value, min_speed: speedIn.value, source: sourceSel.value, flags: flagSel.value, efficiency: effSel.value, exclude: [...excluded].join(',') });
      ctx.setQuery({ places: f.places || null, place_side: f.places && f.place_side !== 'any' ? f.place_side : null, q: f.q || null, min_distance: f.min_distance || null, min_speed: f.min_speed || null, source: f.source || null, flags: f.flags || null, efficiency: f.efficiency === 'plain' ? null : f.efficiency, exclude: f.exclude || null });
      clearPicks();
      table.reload(apiParams());
      loadPatterns();
    }, now ? 0 : 350);
  };
  ctx.onCleanup(() => { clearTimeout(timer); if (exclPop.matches(':popover-open')) exclPop.hidePopover(); exclPop.remove(); });
  for (const el of [placeSel, sideSel, sourceSel, flagSel, effSel]) el.addEventListener('change', () => apply(true));
  for (const el of [textIn, distIn, speedIn]) el.addEventListener('input', () => apply());

  const filters = h('div', { class: 'filter-bar' },
    h('button', { class: 'btn', type: 'button', onclick: (e) => openPeriodPicker(e.currentTarget), cover: ['drives.filter.time_range', 'ds.filter.time_range'] }, icon('calendar', 'ico--sm'), period.preset === 'custom' ? 'Custom period' : period.label, icon('chev-d', 'ico--sm')),
    h('span', { class: 'filter', cover: 'drives.filter.geofence' }, icon('pin', 'ico--sm'), placeSel, sideSel),
    h('label', { class: 'field field--search', cover: ['drives.filter.location', 'drives.filter.location.hint'] }, icon('search'), textIn),
    h('label', { class: 'filter', cover: 'drives.filter.min_distance' }, h('span', null, 'At least'), distIn, h('span', { class: 'faint' }, unitLabel('distance'))),
    h('label', { class: 'filter', cover: 'drives.filter.min_speed' }, h('span', null, 'Average at least'), speedIn, h('span', { class: 'faint' }, unitLabel('speed'))),
    sourceSel, flagSel,
    h('span', { cover: 'drives.setting.efficiency_method' }, effSel),
    h('span', { class: 'filter', cover: ['ds.filter.exclude_locations', 'ds.filter.exclude_locations.hint'] }, exclBtn));

  /* ---------- summary */
  const summary = h('section', { class: 'panel drives-summary', 'aria-label': 'Summary of the listed drives' });
  const dailyEl = h('div', { class: 'daily-chart-plot' });
  const dailyRead = h('span');
  let perDay = [];
  const dailyCsv = csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'drives-per-day'), perDay,
    [column('Day', 'date'), column('Drives', 'count'), column('Distance', 'distance_km', 'distance'), column('Energy', 'energy_kwh', 'energy')]) });
  const paintSummary = (s) => {
    s = s || {};
    const none = !s.count;
    const measured = (s.count ?? 0) - (s.without_energy ?? 0);
    replace(summary,
      h('div', { class: 'drives-metrics' },
        metric('Drives', s.count ?? 0, 'count', { explain: none ? 'match the filters' : s.without_energy ? `${formatNumber(measured)} with energy, ${formatNumber(s.without_energy)} without` : 'all with energy' }),
        metric('Distance', none ? null : s.distance_km, 'distance', { cover: 'drives.summary.distance', why: 'No drive matches', explain: 'sum of the listed drives' }),
        metric('Driving time', none ? null : s.duration_ms, 'duration', { cover: 'drives.summary.duration', why: 'No drive matches', explain: 'from start to end of each drive' }),
        metric('Energy', none ? null : s.energy_kwh, 'energy', { cover: 'drives.summary.energy', why: 'No drive matches', explain: s.energy_estimated_kwh ? `${format(s.energy_measured_kwh, 'energy')} measured at the pack, ${format(s.energy_estimated_kwh, 'energy')} estimated (imported)` : 'measured at the pack, regen taken off' }),
        metric('Consumption', none ? null : s.wh_per_km, 'efficiency', { cover: 'drives.summary.consumption', why: 'No drive matches', explain: s.without_energy ? 'energy over distance, drives with energy only' : 'energy over distance' })));
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
    { key: 'start', label: 'Start', sort: 'start', firstDir: 'desc', cover: 'drives.table.date', render: (d) => h('span', { class: 'cell-text' },
      h('a', { class: 'main', href: state.buildUrl('drive', { car: cid, drive: d.id }) }, `${formatDate(d.start_ms, tz, { year: false })}, ${formatTime(d.start_ms, tz)}`),
      h('span', { class: 'sub' }, d.end_ms ? `to ${formatTime(d.end_ms, tz)}` : 'still driving')),
      csv: (d) => new Date(d.start_ms).toISOString() },
    { key: 'route', label: 'Route', cls: 'col-route', cover: ['drives.table.start', 'drives.table.destination', 'drives.table.link.geofence_start', 'drives.table.link.geofence_end'], render: (d) => h('span', { class: 'cell-text' },
      h('span', { class: 'route' }, placeCell(d.start), h('span', { class: 'tl-arrow', 'aria-hidden': 'true' }, ' → '), d.end && d.end_ms != null ? placeCell(d.end) : h('span', null, d.end_ms == null ? 'driving now' : '…')),
      h('span', { class: 'sub' }, [d.start?.address?.short, d.end?.address?.short].filter(Boolean).join(' → '))), csv: (d) => `${placeLabel(d.start)} → ${placeLabel(d.end)}` },
    { key: 'duration', label: 'Time', num: true, sort: 'duration', cover: 'drives.table.duration', render: (d) => formatDuration(d.duration_ms) },
    { key: 'distance', label: 'Distance', num: true, sort: 'distance', cover: 'drives.table.distance', render: (d) => format(d.distance_km, 'distance') },
    { key: 'soc', label: 'Battery', num: true, cover: ['drives.table.soc_start', 'drives.table.soc_end'], render: (d) => h('span', { class: 'nowrap' }, `${format(d.start_soc_pct, 'percent')} → ${format(d.end_soc_pct, 'percent')}`) },
    { key: 'outside', label: 'Outside', num: true, cover: 'drives.table.outside_temp', render: (d) => h('span', { class: ['temp-cell', d.outside_avg_c != null && (d.outside_avg_c < 5 ? 'is-cold' : d.outside_avg_c > 25 ? 'is-warm' : null)] }, format(d.outside_avg_c, 'temperature')) },
    { key: 'speed', label: 'Average', num: true, cover: 'drives.table.speed_avg', render: (d) => format(d.speed_avg_kph, 'speed') },
    { key: 'speed_max', label: 'Top', num: true, sort: 'speed_max', cover: 'drives.table.speed_max', render: (d) => format(d.speed_max_kph, 'speed') },
    { key: 'power', label: 'Peak', num: true, cover: 'drives.table.power_max', title: 'Highest power taken from the battery', render: (d) => format(d.power_max_kw, 'power') },
    { key: 'energy', label: 'Energy', num: true, sort: 'energy', cover: 'drives.table.energy', render: (d) => h('span', { class: 'nowrap' }, format(d.energy_kwh, 'energy'), d.energy_basis === 'estimated' ? [' ', tag('est.', 'warm')] : null) },
    { key: 'wh', label: 'Consumption', num: true, sort: 'wh_per_km', cover: 'drives.table.consumption', render: (d) => format(d.wh_per_km, 'efficiency') },
    { key: 'eff', label: 'Efficiency', cover: 'drives.table.efficiency', title: 'Distance driven against the range used (100 % = as rated)', render: effBar, csv: (d) => d.efficiency_pct },
    { key: 'flags', label: '', cover: 'drives.table.reduced_range', csv: false, render: (d) => h('span', { class: 'flag-icons' },
      d.flags?.reduced_range ? h('span', { class: 'flag-ico', title: 'Cold battery: part of the charge was locked, so range and consumption read high' }, icon('thermo', 'ico--sm')) : null,
      d.flags?.gap_ms ? h('span', { class: 'flag-ico', title: `Data missing for ${formatDuration(d.flags.gap_ms)}` }, icon('warn', 'ico--sm')) : null,
      d.flags?.in_progress ? h('span', { class: 'tag tag--live' }, 'live') : null,
      d.source && d.source !== 'can' ? h('span', { class: 'tag tag--plain', title: 'Imported from TeslaMate: energy is estimated, points are sparser' }, 'imported') : null) },
  ];
  const table = dataTable(ctx, {
    title: 'Drives', icon: 'route', path, params: apiParams(), limit: 25, columns, sort: { key: 'start', dir: 'desc' }, csvName: 'drives', cls: 'drives-table',
    cover: ['drives.table', 'drives.table.link.details', 'drives.empty_states'], side: pickBar,
    note: 'Energy is measured at the battery pack, with regen taken off; imported drives are marked "est." and "imported". Efficiency is the distance driven against the range used. Tick drives to open them together as a trip.',
    rowHref: (d) => state.buildUrl('drive', { car: cid, drive: d.id }),
    onPage: (pg) => { if (pg.summary) paintSummary(pg.summary); },
    empty: () => {
      const why = f.q ? `the text "${f.q}"` : f.places ? 'the place filter' : f.min_distance ? 'the shortest drive' : f.min_speed ? 'the lowest average speed' : f.flags ? 'the flag filter' : f.source ? 'the source filter' : null;
      return emptyState({ icon: 'route', title: 'No drives', text: why ? `No drive in ${period.label.toLowerCase()} passes ${why}. Clear it to see every drive.` : `${car.name} did not drive in this period. Pick a longer period.` });
    },
  });

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
    try { s = await ctx.api.get(`cars/${encodeURIComponent(cid)}/drive-stats`, { ...ctx.params, exclude_places: f.exclude || null }); } catch (e) { replace(patterns, h('p', { class: 'note' }, `Patterns could not be loaded: ${e.message}`)); return; }
    if (!ctx.alive()) return;
    const days = s.per_day || [];
    const nDays = Math.max(1, Math.round(((ctx.params.to_ms ?? ctx.api.now()) - (ctx.params.from_ms ?? (days[0] ? Date.parse(days[0].date) : ctx.api.now()))) / 86400000));
    const spark = (key, kind) => {
      const max = Math.max(1e-9, ...days.map((d) => d[key] || 0));
      return h('span', { class: 'spark', 'aria-hidden': 'true' }, days.slice(-60).map((d) => { const i = h('i'); setVars(i, { '--h': `${((d[key] || 0) / max) * 100}%` }); return i; }));
    };
    const top = (x) => (x?.kph != null ? h('a', { class: 'link', href: state.buildUrl('drive', { car: cid, drive: x.drive }) }, format(x.kph, 'speed')) : null);
    const bands = (s.speed_bands || []).filter((b) => b.to_kph > 0 && b.seconds > 0);
    const maxBand = Math.max(1e-9, ...bands.map((b) => b.share_pct || 0));
    const dests = s.top_destinations || [];
    const maxDest = Math.max(1, ...dests.map((d) => d.count));
    const left = places.filter((p) => excluded.has(p.id)).map((p) => p.name);
    const bandLabel = (b) => (b.to_kph == null ? `${format(b.from_kph, 'speed', { digits: 0 })} and over`
      : `${formatNumber(convert(b.from_kph, 'speed'), { maximumFractionDigits: 0 })}–${format(b.to_kph, 'speed', { digits: 0 })}`);
    perDay = days;
    const csvSmall = (name, rows, cols) => csvButton({ onClick: () => downloadCsv(csvFilename(car.name, name), rows, cols) });
    replace(patterns,
      h('div', { class: 'panel-head' }, h('h2', { class: 'panel-title' }, icon('stats'), 'Driving patterns'),
        h('span', { class: 'faint patterns-scope' }, left.length ? `${period.label}, without drives to or from ${left.join(', ')}` : `${period.label}, every drive (the list filters do not apply here)`),
        csvSmall('drive-patterns', [{ ...s, ...{ distance_per_day_km: s.count ? (s.distance_km || 0) / nDays : null, energy_per_day_kwh: s.count ? (s.energy_kwh || 0) / nDays : null } }], [
          column('Drives', 'count'), column('Distance', 'distance_km', 'distance'), column('Energy', 'energy_kwh', 'energy'), column('Median drive', 'median_distance_km', 'distance'),
          column('Distance per day', 'distance_per_day_km', 'distance'), column('Energy per day', 'energy_per_day_kwh', 'energy'),
          column('Top speed in the period', (r) => r.max_speed?.period?.kph, 'speed'), column('Top speed, last 30 days', (r) => r.max_speed?.last_30d?.kph, 'speed'), column('Top speed, last 7 days', (r) => r.max_speed?.last_7d?.kph, 'speed'),
          column('Projected per month', (r) => r.projection?.per_month_km, 'distance'), column('Projected per year', (r) => r.projection?.per_year_km, 'distance'), column('Projection basis (days)', (r) => r.projection?.basis_days)])),
      s.count ? null : h('p', { class: 'note warn patterns-empty' }, left.length ? `No drive in ${period.label.toLowerCase()} once ${left.join(', ')} ${left.length === 1 ? 'is' : 'are'} left out. Leave fewer places out to see patterns.` : `${car.name} did not drive in ${period.label.toLowerCase()}, so there are no patterns. Pick a longer period.`),
      h('div', { class: 'pattern-grid' },
        h('div', { class: 'metric', cover: 'ds.stat.drive_count' }, h('span', { class: 'metric-label' }, 'Drives'), h('span', { class: 'metric-value' }, formatNumber(s.count || 0)), spark('count'), h('span', { class: 'explain' }, 'bars: drives per day')),
        h('div', { class: 'metric', cover: 'ds.stat.distance_total' }, h('span', { class: 'metric-label' }, 'Distance'), h('span', { class: 'metric-value' }, s.count ? quantity(s.distance_km, 'distance') : '—'), spark('distance_km'), h('span', { class: 'explain' }, 'bars: distance per day')),
        h('div', { class: 'metric', cover: 'ds.stat.energy_total' }, h('span', { class: 'metric-label' }, 'Energy'), h('span', { class: 'metric-value' }, s.count ? quantity(s.energy_kwh, 'energy') : '—'), spark('energy_kwh'), h('span', { class: 'explain' }, 'bars: energy per day, at the pack')),
        metric('Typical drive', s.median_distance_km, 'distance', { cover: 'ds.stat.median_distance', explain: 'median: half are shorter', why: 'No drive' }),
        metric('Per day', s.count ? (s.distance_km || 0) / nDays : null, 'distance', { cover: 'ds.stat.distance_per_day', explain: `over ${formatNumber(nDays)} ${nDays === 1 ? 'day' : 'days'}; days without driving count as 0`, why: 'No drive' }),
        metric('Energy per day', s.count ? (s.energy_kwh || 0) / nDays : null, 'energy', { cover: 'ds.stat.energy_per_day', explain: 'driving energy over the same days', why: 'No drive' }),
        metric('Top speed', top(s.max_speed?.period), null, { cover: 'ds.stat.max_speed_period', explain: 'in the period; opens that drive', why: 'No drive' }),
        metric('Last 30 days', top(s.max_speed?.last_30d), null, { cover: 'ds.stat.max_speed_30d', explain: 'top speed in the 30 days to the period end', why: 'No drive in those days' }),
        metric('Last 7 days', top(s.max_speed?.last_7d), null, { cover: 'ds.stat.max_speed_7d', explain: 'top speed in the 7 days to the period end', why: 'No drive in those days' }),
        metric('Per month', s.projection?.per_month_km, 'distance', { cover: 'ds.stat.monthly_mileage', explain: s.projection?.basis_days ? `projected at the rate of ${formatNumber(Math.round(s.projection.basis_days))} recorded days` : 'projected', why: 'Not enough driving to project' }),
        metric('Per year', s.projection?.per_year_km, 'distance', { cover: 'ds.stat.annual_mileage', explain: 'projected at the same rate', why: 'Not enough driving to project' })),
      h('div', { class: 'daily-chart' }, h('div', { class: 'daily-chart-head' }, h('span', null, "Distance per day, in the car's days"), h('span', { class: 'daily-chart-side' }, dailyRead, dailyCsv)), dailyEl),
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

    // distance per day
    const xs = days.map((d) => Date.parse(`${d.date}T12:00:00Z`));
    const best = days.reduce((a, d) => (d.distance_km > (a?.distance_km ?? -1) ? d : a), null);
    const idle = () => replace(dailyRead, best ? ['busiest day ', h('b', null, format(best.distance_km, 'distance'))] : 'no driving');
    idle();
    dailyEl.hidden = !days.length;
    if (days.length) barChart(dailyEl, ctx, { t_ms: xs, values: days.map((d) => d.distance_km), kind: 'distance', tz: 'UTC', onCursor: (i) => { if (i == null) { idle(); return; } const d = days[i]; replace(dailyRead, `${formatDate(xs[i], 'UTC', { year: false })} · `, h('b', null, format(d.distance_km, 'distance')), ` · ${d.count} ${d.count === 1 ? 'drive' : 'drives'}`); } });
  }

  // a drive started or ended: load the lists and patterns again (a moment later, so bursts load once)
  let rowsTimer = 0;
  ctx.events('rows', (ev) => {
    if (ev.data?.car && ev.data.car !== cid) return;
    clearTimeout(rowsTimer);
    rowsTimer = setTimeout(() => { if (!ctx.alive()) return; table.reload(apiParams()); incomplete.reload(); loadPatterns(); }, 2000);
  });
  ctx.onCleanup(() => clearTimeout(rowsTimer));

  replace(root,
    pageHead({ eyebrow: car.name, title: 'Drives', sub: [period.label, `times in ${tz.replace(/_/g, ' ')}`],
      actions: [h('button', { class: 'btn', type: 'button', onclick: openCarSwitcher, cover: ['drives.filter.car', 'ds.filter.car'] }, icon('car'), 'Switch car'), h('a', { class: 'btn', href: ctx.link('trip') }, icon('flag'), 'This period as a trip')] }),
    filters, summary, table.el, patterns, incomplete.el, exclPop);
  paintSummary(null);

  try { await Promise.all([table.ready, loadPatterns(), incomplete.ready]); } catch (e) { ctx.fail(e); return; }
  ctx.ready();
}
