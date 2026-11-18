// Sentry Mate, "Logbook" UI pack: Drives (/cars/{car}/drives).
//
// Find and compare drives, and see driving patterns, for the shared period. Data:
// GET /api/v1/cars/{car}/drives (the list: filters, sort, cursor paging, summary, CSV), /tracks (the
// shapes of the listed drives, for the sketches and the map), /drive-stats (patterns for the period),
// /drives?flags=gap,in_progress (drives with missing data), and POST /cars/{car}/rebuild for admins.

import { h, svg, replace } from '../shared/dom.js';
import { format, parts, convert, toMetric, unitLabel } from '../shared/units.js';
import { formatTime, formatDuration, dayKey, formatDayKey, addDays, periodLabel } from '../shared/time.js';
import { formatNumber, tn } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { pageHead, q, figure, icon, empty, errorView, loadingBlock, sep, routeSketch, flag, button } from '../core/ui.js';
import { makeMap, boundsOf } from '../core/map.js';
import { confirmDialog } from '../core/dialog.js';
import { placeLink } from '../core/journal.js';

export const id = 'drives';
export const css = true;
export const title = () => 'Drives';

const SORTS = [['start', 'Newest first'], ['distance', 'Longest first'], ['duration', 'Longest time first'], ['energy', 'Most energy first'], ['wh_per_km', 'Highest consumption first'], ['speed_max', 'Fastest first']];
const SOURCES = [['', 'All sources'], ['can', 'CAN Bridge only'], ['teslamate', 'Imported only']];

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz;
  const f = {
    q: ctx.query.q || '',
    min: ctx.query.min_distance_km ? Number(ctx.query.min_distance_km) : null,
    source: SOURCES.some(([k]) => k === ctx.query.source) ? ctx.query.source || '' : '',
    sort: SORTS.some(([k]) => k === ctx.query.sort) ? ctx.query.sort : 'start',
  };
  const listParams = () => ({ ...ctx.params, q: f.q || undefined, min_distance_km: f.min || undefined, source: f.source || undefined, sort: f.sort, dir: 'desc' });

  // Filters: text, minimum distance (typed in the person's unit), source and order. The period is the frame's.
  const qIn = h('input', { class: 'field-input', type: 'search', id: 'drives-q', value: f.q, placeholder: 'A place or an address', autocomplete: 'off' });
  const minIn = h('input', { class: 'field-input field-num', type: 'number', id: 'drives-min', min: '0', step: '0.1', value: f.min != null ? String(Math.round(convert(f.min, 'distance') * 10) / 10) : '' });
  const srcSel = h('select', { class: 'field-select', id: 'drives-source' }, SOURCES.map(([k, t]) => h('option', { value: k, selected: k === f.source }, t)));
  const sortSel = h('select', { class: 'field-select', id: 'drives-sort' }, SORTS.map(([k, t]) => h('option', { value: k, selected: k === f.sort }, t)));
  let timer = 0;
  const changed = () => {
    ctx.setQuery({ q: f.q || null, min_distance_km: f.min || null, source: f.source || null, sort: f.sort === 'start' ? null : f.sort });
    loadList();
  };
  qIn.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(() => { f.q = qIn.value.trim(); changed(); }, 250); });
  ctx.onCleanup(() => clearTimeout(timer));
  minIn.addEventListener('change', () => { const n = parseFloat(minIn.value); f.min = Number.isFinite(n) && n > 0 ? Math.round(toMetric(n, 'distance') * 1000) / 1000 : null; changed(); });
  srcSel.addEventListener('change', () => { f.source = srcSel.value; changed(); });
  sortSel.addEventListener('change', () => { f.sort = sortSel.value; changed(); });
  const filters = h('div', { class: 'toolbar', cover: ['drives.filter.car', 'drives.filter.time_range', 'drives.filter.location', 'drives.filter.location.hint', 'drives.filter.min_distance', 'ds.filter.car', 'ds.filter.time_range'] },
    h('div', { class: 'field field-search' }, h('label', { class: 'field-label', for: 'drives-q' }, 'Find'),
      h('div', { class: 'field-control' }, icon('search', 'icon field-icon'), qIn),
      h('p', { class: 'field-hint' }, 'Matches the start and end place and address; it only hides drives.')),
    h('div', { class: 'toolbar-end' },
      h('div', { class: 'field' }, h('label', { class: 'field-label', for: 'drives-min' }, `At least (${unitLabel('distance')})`), minIn),
      h('div', { class: 'field' }, h('label', { class: 'field-label', for: 'drives-source' }, 'Source'), srcSel),
      h('div', { class: 'field' }, h('label', { class: 'field-label', for: 'drives-sort' }, 'Order'), sortSel)));

  const totals = h('div', { class: 'figs drives-totals', cover: ['drives.summary.energy', 'drives.summary.duration', 'drives.summary.distance', 'drives.summary.consumption'] });
  const listBox = h('div', { class: 'drives-list' });
  const more = h('div', { class: 'list-more' });
  const csv = h('a', { class: 'button button-quiet button-small', download: '', href: '#' }, icon('download'), 'Download CSV');
  const mapBox = h('div', { class: 'map map-period', role: 'region', 'aria-label': 'Map of the drives listed' });
  const mapNote = h('span', { class: 'label-note' });
  const side = h('aside', { class: 'drives-side', 'aria-label': 'The listed drives on a map' }, h('h2', { class: 'label' }, 'On the map', mapNote), mapBox);
  const patterns = h('section', { class: 'block patterns', 'aria-labelledby': 'patterns-title' }, loadingBlock(4));
  const missing = h('section', { class: 'block missing' });

  ctx.main.append(
    pageHead({ eyebrow: car.name, title: 'Drives', deck: ctx.info.purpose }),
    filters, totals,
    h('div', { class: 'drives-body' },
      h('div', { class: 'drives-main' },
        h('div', { class: 'ledger-top' }, h('p', { class: 'ledger-count', 'aria-live': 'polite' }), csv),
        listBox, more), side),
    patterns, missing);
  const count = ctx.main.querySelector('.ledger-count');

  let pager = null, items = [], tracks = new Map(), sideMap = null;
  async function loadList() {
    pager = ctx.api.pager(api.apiPath`cars/${car.id}/drives`, listParams(), { limit: 50 });
    items = [];
    replace(listBox, loadingBlock(6));
    csv.setAttribute('href', ctx.api.csvUrl(api.apiPath`cars/${car.id}/drives`, listParams()));
    await more1(true);
  }
  async function more1(first = false) {
    try {
      const got = await pager.more();
      items = pager.items;
      if (first) drawTotals(pager.last.summary || {});
      await loadTracks(got);
      drawList();
      drawMap();
    } catch (err) {
      if (ctx.signal.aborted) return;
      replace(listBox, errorView(err, { retry: () => loadList() }));
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
    const measured = s.count - (s.without_energy || 0);
    replace(totals,
      h('div', { class: 'fig totals-caption' }, h('p', { class: 'eyebrow' }, 'For the period'), h('p', { class: 'totals-span' }, periodLabel(ctx.period, tz, api.now()))),
      figure('Drives', formatNumber(s.count || 0)),
      figure('Distance', q(s.distance_km, 'distance')),
      figure('Driving time', formatDuration(s.duration_ms || 0)),
      figure('Energy used', q(s.energy_kwh, 'energy'), s.energy_estimated_kwh ? `${format(s.energy_estimated_kwh, 'energy')} of it estimated` : `measured on ${tn(measured, '{n} drive', '{n} drives')}`),
      figure('Consumption', q(s.wh_per_km, 'efficiency'), 'average, measured at the pack'));
  }

  function row(d) {
    const href = state.buildUrl('drive', { car: car.id, drive: d.id, period: ctx.state.period });
    const going = d.end_ms == null;
    return h('tr', { class: ['drive-row', { 'is-going': going }], dataset: { id: d.id, href } },
      h('td', { class: 'c-time' }, `${formatTime(d.start_ms, tz)} – ${going ? 'now' : formatTime(d.end_ms, tz)}`),
      h('td', { class: 'c-route' },
        h('div', { class: 'route-cell' },
          routeSketch(shapeOf(d), { cls: 'sketch row-sketch' }),
          h('div', null,
            h('a', { class: 'route-name', href }, d.start.label, h('span', { class: 'to' }, ' to '), d.end.label),
            h('span', { class: 'cell-sub' },
              placeCell(d.start), going ? null : [h('span', { class: 'sep', 'aria-hidden': 'true' }, ' → '), placeCell(d.end)],
              d.source === 'teslamate' ? [' ', flag('Imported', { kind: 'estimated' })] : null,
              going ? [' ', flag('In progress')] : null,
              d.flags && d.flags.gap_ms ? [' ', flag('Gap', { kind: 'warn', title: `${formatDuration(d.flags.gap_ms)} without data` })] : null,
              d.flags && d.flags.reduced_range ? [' ', flag('Cold battery', { title: 'Part of the battery was locked by the cold' })] : null)))),
      h('td', { class: 'c-num c-dist' }, q(d.distance_km, 'distance')),
      h('td', { class: 'c-num c-dur' }, formatDuration(d.duration_ms)),
      h('td', { class: 'c-num c-batt', title: 'Battery at the start and the end' }, `${format(d.start_soc_pct, 'number', { digits: 0 })}–${format(d.end_soc_pct, 'percent')}`),
      h('td', { class: 'c-num c-temp' }, q(d.outside_avg_c, 'temperature', { digits: 0 })),
      h('td', { class: 'c-num c-speed' }, q(d.speed_avg_kph, 'speed'), h('span', { class: 'cell-sub' }, 'top ', format(d.speed_max_kph, 'speed'))),
      h('td', { class: 'c-num c-power' }, q(d.power_max_kw, 'power', { digits: 0 })),
      h('td', { class: 'c-num c-energy' }, q(d.energy_kwh, 'energy'), h('span', { class: 'cell-sub' }, d.energy_basis === 'estimated' ? 'estimated' : 'measured')),
      h('td', { class: 'c-num c-eff' }, q(d.wh_per_km, 'efficiency'), h('span', { class: 'cell-sub' }, d.efficiency_pct != null ? `${format(d.efficiency_pct, 'percent')} of rated` : '')),
      h('td', { class: 'c-go' }, icon('chevronRight')));
  }
  const COLS = [['time', 'Time', false], ['route', 'From and to', false], ['dist', 'Distance', true], ['dur', 'Duration', true], ['batt', 'Battery', true], ['temp', 'Outside', true], ['speed', 'Speed', true], ['power', 'Peak power', true], ['energy', 'Energy', true], ['eff', 'Consumption', true], ['go', '', false]];
  function drawList() {
    const n = items.length;
    count.textContent = pager.done ? tn(n, '{n} drive', '{n} drives') : `${formatNumber(n)} drives shown, more to load`;
    csv.hidden = !n;
    if (!n) {
      const why = f.q ? `No drive matches “${f.q}”` : f.min ? `No drive is ${format(f.min, 'distance')} or longer` : f.source ? `No ${f.source === 'can' ? 'CAN Bridge' : 'imported'} drive` : `${car.name} made no drive`;
      replace(listBox, empty({
        title: `${why} in ${periodLabel(ctx.period, tz, api.now()).toLowerCase()}.`,
        text: f.q || f.min || f.source ? 'Clear the filters above, or choose a longer period.' : 'Choose a longer period in the period picker at the top.',
        cover: 'drives.empty_states',
        action: f.q || f.min || f.source ? button('Clear the filters', { kind: 'primary', onclick: () => { f.q = ''; f.min = null; f.source = ''; qIn.value = ''; minIn.value = ''; srcSel.value = ''; changed(); } }) : null,
      }));
      replace(more);
      return;
    }
    const head = h('thead', null, h('tr', null, COLS.map(([k, t, num]) => h('th', { scope: 'col', class: [`c-${k}`, { 'c-num': num }] }, k === 'go' ? h('span', { class: 'sr-only' }, 'Open') : t))));
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
            h('th', { colspan: 2, scope: 'rowgroup' },
              h('span', { class: 'day-name' }, name, name === 'Today' || name === 'Yesterday' ? h('span', { class: 'day-date' }, formatDayKey(k, { weekday: 'long', month: 'long', year: false })) : null),
              h('span', { class: 'day-sum' }, h('span', { class: 'day-sum-text' }, tn(list.length, '{n} drive', '{n} drives'), sep(), format(km, 'distance'), sep(), format(kwh, 'energy')))),
            COLS.slice(2).map(([c]) => h('td', { class: `c-${c}`, 'aria-hidden': 'true' }))),
          list.map(row));
      });
    } else {
      bodies = [h('tbody', null, items.map(row))];
    }
    const table = h('table', { class: 'drives', cover: ['drives.table', 'drives.table.date', 'drives.table.start', 'drives.table.destination', 'drives.table.duration', 'drives.table.distance', 'drives.table.soc_start', 'drives.table.soc_end', 'drives.table.outside_temp', 'drives.table.speed_avg', 'drives.table.speed_max', 'drives.table.power_max', 'drives.table.reduced_range', 'drives.table.efficiency', 'drives.table.energy', 'drives.table.consumption', 'drives.table.link.details', 'drives.table.link.geofence_start', 'drives.table.link.geofence_end'] },
      h('caption', { class: 'sr-only' }, 'Drives. Select a drive to open it.'), head, bodies);
    table.addEventListener('click', (e) => {
      if (e.target.closest('a')) return;
      const tr = e.target.closest('tr[data-href]');
      if (tr) state.navigate(tr.dataset.href);
    });
    table.addEventListener('mouseover', (e) => hover(e.target.closest('tr[data-id]')?.dataset.id || ''));
    replace(listBox, table);
    if (!pager.done) {
      const btn = button('Show earlier drives', { kind: 'primary', onclick: async () => { btn.disabled = true; btn.textContent = 'Loading…'; await more1(); } });
      replace(more, h('p', { class: 'list-more-note' }, `Showing ${formatNumber(n)} drives`), btn);
    } else replace(more, h('p', { class: 'list-more-note' }, n > 1 ? `All ${formatNumber(n)} drives are shown.` : 'The only drive.'));
  }

  // The listed drives on a map, beside the list on wide screens (no map is made while it is hidden).
  let hovered = '';
  function hover(id) { if (id === hovered) return; hovered = id; if (sideMap) sideMap.map.setFilter('period-hover', ['==', ['get', 'id'], id]); }
  async function drawMap() {
    if (!side.clientWidth) return;
    const lines = items.map((d) => ({ id: d.id, c: shapeOf(d) })).filter((x) => x.c.length > 1 && x.c.every((p) => p[0] != null));
    mapNote.textContent = pager.done ? tn(items.length, 'the one drive', 'all {n} drives') : `the ${formatNumber(items.length)} drives listed`;
    const fc = { type: 'FeatureCollection', features: lines.map((x) => ({ type: 'Feature', properties: { id: x.id }, geometry: { type: 'LineString', coordinates: x.c } })) };
    if (!lines.length) return;
    const bounds = boundsOf(lines.flatMap((x) => x.c));
    if (sideMap) { sideMap.map.getSource('period')?.setData(fc); sideMap.frame(bounds); return; }
    const m = await makeMap(ctx, mapBox, { bounds, padding: 28, maxZoom: 15 });
    if (!m) return;
    await m.loaded;
    if (ctx.signal.aborted) return;
    m.map.addSource('period', { type: 'geojson', data: fc });
    m.addLine({ id: 'period-line', source: 'period', paint: { 'line-opacity': 0.8 } }, 2.2, '--ink-3');
    m.addLine({ id: 'period-hover', source: 'period', filter: ['==', ['get', 'id'], ''] }, 4.5, '--accent');
    sideMap = m;
  }

  await Promise.all([loadList(), drawPatterns(ctx, patterns), drawMissing(ctx, missing)]);
  ctx.ready();
  return {
    async update(c) {
      tracks = new Map();
      replace(totals);
      await Promise.all([loadList(), drawPatterns(c, patterns), drawMissing(c, missing)]);
      c.ready();
    },
  };
}

/* ------------------------------------------------------------------ driving patterns */

async function drawPatterns(ctx, box) {
  const car = ctx.car, tz = ctx.tz;
  let s;
  try { s = await ctx.api.get(api.apiPath`cars/${car.id}/drive-stats`, ctx.params); } catch (err) { if (!ctx.signal.aborted) replace(box, errorView(err, { eyebrow: 'Driving patterns' })); return; }
  const days = s.per_day || [];
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
  const top = s.top_destinations || [];
  const ms = s.max_speed || {};
  const speedLink = (x) => (x && x.drive ? h('a', { href: state.buildUrl('drive', { car: car.id, drive: x.drive }) }, format(x.kph, 'speed')) : '—');
  replace(box, 
    h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'patterns-title' }, 'Driving patterns', h('span', { class: 'label-note' }, periodLabel(ctx.period, tz, api.now())))),
    s.count ? [
      h('div', { class: 'figs', cover: ['ds.stat.drive_count', 'ds.stat.distance_total', 'ds.stat.energy_total', 'ds.stat.median_distance', 'ds.stat.distance_per_day', 'ds.stat.energy_per_day', 'ds.stat.monthly_mileage', 'ds.stat.annual_mileage'] },
        figure('Drives', formatNumber(s.count)), figure('Distance', q(s.distance_km, 'distance')), figure('Energy', q(s.energy_kwh, 'energy')),
        figure('Median drive', q(s.median_distance_km, 'distance')),
        figure('Per day', q(days.length ? s.distance_km / days.length : null, 'distance'), `${format(days.length ? s.energy_kwh / days.length : null, 'energy')} a day`),
        figure('Projected', q(s.projection?.per_month_km, 'distance'), `a month; ${format(s.projection?.per_year_km, 'distance')} a year, from ${formatNumber(s.projection?.basis_days || 0, { maximumFractionDigits: 0 })} days`)),
      h('div', { class: 'patterns-grid' },
        h('figure', { class: 'by-day', cover: 'ds.stat.distance_per_day' }, h('figcaption', { class: 'label' }, 'Distance by day'), bars),
        h('div', { class: 'speed-bands', cover: 'ds.chart.speed_histogram' },
          h('p', { class: 'label' }, 'Time in each speed band', h('span', { class: 'label-note' }, 'counted second by second')),
          h('ul', { class: 'band-list' }, bands.map((b) => h('li', { class: 'band-row' },
            h('span', { class: 'band-name' }, band(b)),
            h('span', { class: 'band-bar' }, svg('svg', { class: 'band-svg', width: '100%', height: '100%', preserveAspectRatio: 'none', 'aria-hidden': 'true' }, svg('rect', { class: 'band-fill', x: 0, y: 0, width: `${((b.share_pct || 0) / maxShare) * 100}%`, height: '100%' }))),
            h('span', { class: 'band-value' }, formatDuration((b.seconds || 0) * 1000), ' · ', format(b.share_pct, 'percent')))))),
        h('div', { class: 'top-dest', cover: ['ds.chart.top_destinations', 'ds.stat.max_speed_period', 'ds.stat.max_speed_30d', 'ds.stat.max_speed_7d'] },
          h('p', { class: 'label' }, 'Top speed'),
          h('dl', { class: 'spec' },
            h('div', { class: 'spec-row' }, h('dt', null, 'In the period'), h('dd', null, speedLink(ms.period))),
            h('div', { class: 'spec-row' }, h('dt', null, 'Last 30 days'), h('dd', null, speedLink(ms.last_30d))),
            h('div', { class: 'spec-row' }, h('dt', null, 'Last 7 days'), h('dd', null, speedLink(ms.last_7d)))),
          h('p', { class: 'label top-dest-label' }, 'Most frequent destinations'),
          top.length ? h('ol', { class: 'dest-list' }, top.slice(0, 8).map((d) => h('li', null, h('span', null, d.label), h('span', { class: 'dest-count' }, tn(d.count, '{n} drive', '{n} drives'))))) : h('p', { class: 'note' }, 'No destinations yet.')))]
      : empty({ title: 'No drives in this period, so there are no patterns to show.', text: 'Choose a longer period at the top.', cover: 'ds.empty_states' }));
}

/* ------------------------------------------------------------------ drives with missing data */

async function drawMissing(ctx, box) {
  const car = ctx.car, tz = ctx.tz;
  let pg;
  try { pg = await ctx.api.page(api.apiPath`cars/${car.id}/drives`, { ...ctx.params, flags: 'gap,in_progress' }, { limit: 20 }); } catch (err) { if (!ctx.signal.aborted) replace(box, errorView(err, { eyebrow: 'Drives with missing data' })); return; }
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
  replace(box, 
    h('div', { class: 'block-head' }, h('h2', { class: 'label' }, 'Drives with missing data', h('span', { class: 'label-note' }, list.length ? tn(list.length, '{n} drive', '{n} drives') : 'none')), rebuild),
    h('p', { class: 'note', cover: 'drives.incomplete.note' }, 'A drive is listed here when it overlaps time with no data (a gap) or is still going. Its figures cover only the time with data; a late log or a rebuild fills them in.'),
    list.length ? h('table', { class: 'ledger-table', cover: ['drives.incomplete.table', 'drives.incomplete.id', 'drives.incomplete.start', 'drives.incomplete.end', 'drives.incomplete.distance', 'drives.incomplete.duration'] },
      h('thead', null, h('tr', null, ['Drive', 'Start', 'End', 'Distance', 'Duration', 'Why'].map((t, i) => h('th', { scope: 'col', class: { num: i === 3 || i === 4 } }, t)))),
      h('tbody', null, list.map((d) => h('tr', null,
        h('td', null, h('a', { href: state.buildUrl('drive', { car: car.id, drive: d.id }) }, `${d.start.label} to ${d.end.label}`)),
        h('td', null, `${formatDayKey(dayKey(d.start_ms, tz), { year: false })}, ${formatTime(d.start_ms, tz)}`),
        h('td', null, d.end_ms ? formatTime(d.end_ms, tz) : 'still going'),
        h('td', { class: 'num' }, q(d.distance_km, 'distance')),
        h('td', { class: 'num' }, formatDuration(d.duration_ms)),
        h('td', null, d.flags.in_progress ? 'In progress' : `${formatDuration(d.flags.gap_ms)} without data`)))))
      : h('p', { class: 'note-serif' }, 'Every drive in this period has all its data.'));
}
