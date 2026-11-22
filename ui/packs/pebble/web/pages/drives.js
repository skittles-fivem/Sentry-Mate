// Sentry Mate, "Pebble" UI pack: Drives (/cars/{car}/drives), drives as tiles grouped by day.
//
// A pill row picks the span (Last 7 days, Last 30 days, Pick dates) and Filters opens a sheet (place, words in the
// address, shortest drive, slowest average, how efficiency is worked out, places left out of the patterns). Then
// the totals (distance, time, energy, use), the driving patterns (counts, per day, top speeds, projections, a speed
// histogram, top destinations), drives with missing data, and one tile per drive under friendly day headers.
// Data: GET /api/v1/cars/{car}/drives (paged, with summary), /drive-stats, /places (for the filters).

import { h } from '../shared/dom.js';
import { format, unitLabel, convert } from '../shared/units.js';
import { formatTime, formatDate } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { tile, num, chip, btn, icon, note } from '../core/ui.js';
import { emptyTile } from '../core/states.js';
import { createSheet } from '../core/sheet.js';
import { field, segmented } from '../core/form.js';
import { bars } from '../core/chart.js';
import { openPeriod } from '../core/period.js';
import { placeLink, byDay, dayHead, socBar, filterBar, moreButton, statTile, dur, pills } from '../core/bits.js';
import { srcPill, estPill, SOURCE_OPTIONS, SOURCE_WORDS, sourceParam } from '../core/source.js';

export const id = 'drives';
export const css = true;
export const shape = [['full', 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [2, 'h2'], [2, 'h2']];
export const title = () => 'Drives';

/** One drive as a tile: from -> to, the four numbers, the SOC bar, more figures, flags, links. */
export function driveTile(d, ctx, { w = 2, cover = null, links = true } = {}) {
  const tz = ctx.tz, car = ctx.car;
  const going = !!d.flags?.in_progress;
  const href = state.buildUrl('drive', { car: car.id, drive: d.id, period: ctx.state.period });
  return tile({ w, cls: ['item', 'drive-tile'], cover },
    h('div', { class: 'item-top' },
      h('span', { class: 'item-ic tone-coral', 'aria-hidden': 'true' }, icon('route')),
      h('div', { class: 'item-text' },
        h('p', { class: 'item-title route-words' },
          h('span', { cover: ['drives.table.start', 'drives.table.link.geofence_start'] }, placeLink(d.start)),
          icon('arrowRight', 'route-arrow'),
          h('span', { cover: ['drives.table.destination', 'drives.table.link.geofence_end'] }, going ? 'now' : placeLink(d.end))),
        h('p', { class: 'item-sub', cover: 'drives.table.date' }, `${formatTime(d.start_ms, tz)} – ${d.end_ms ? formatTime(d.end_ms, tz) : 'now'}`)),
      h('span', { class: 'item-flags' },
        going ? chip('Driving now', { tone: 'coral' }) : null,
        d.flags?.reduced_range ? chip('Reduced range', { tone: 'sun', cover: 'drives.table.reduced_range' }) : h('span', { cover: 'drives.table.reduced_range', class: 'sr-only' }, 'Range not reduced'),
        d.flags?.gap_ms ? chip('Some data missing', { tone: 'bad' }) : null,
        null)),
    h('div', { class: 'item-nums' },
      h('span', { cover: 'drives.table.distance' }, num(d.distance_km, 'distance', { size: 's' })),
      h('span', { cover: 'drives.table.duration' }, num(d.duration_ms, 'duration', { size: 's' })),
      h('span', { cover: 'drives.table.energy' }, num(d.energy_kwh, 'energy', { size: 's' })),
      h('span', { cover: 'drives.table.consumption' }, num(d.wh_per_km, 'efficiency', { size: 's' }))),
    socBar(d.start_soc_pct, d.end_soc_pct, { cover: ['drives.table.soc_start', 'drives.table.soc_end'] }),
    h('p', { class: 'src-line' }, srcPill(d, { short: true }), estPill(d)),
    h('p', { class: 'item-sub drive-more' },
      h('span', { cover: 'drives.table.speed_avg' }, `Avg ${format(d.speed_avg_kph, 'speed')}`), ' · ',
      h('span', { cover: 'drives.table.speed_max' }, `top ${format(d.speed_max_kph, 'speed')}`), ' · ',
      h('span', { cover: 'drives.table.power_max' }, `peak ${format(d.power_max_kw, 'power')}`), ' · ',
      h('span', { cover: 'drives.table.outside_temp' }, format(d.outside_avg_c, 'temperature')), ' · ',
      h('span', { cover: 'drives.table.efficiency' }, `${format(d.efficiency_pct, 'percent')} of rated`)),
    links ? h('p', { class: 'item-links tile-foot' }, h('a', { href, cover: 'drives.table.link.details' }, 'Details')) : null);
}

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz;
  const Q = ctx.query;
  const f = {
    place: Q.place || '', side: ['start', 'end'].includes(Q.side) ? Q.side : 'any', q: Q.q || '',
    min: Number(Q.min) > 0 ? Number(Q.min) : null, speed: Number(Q.speed) > 0 ? Number(Q.speed) : null,
    eff: Q.eff === 'hill_corrected' ? 'hill_corrected' : 'plain', exclude: Q.exclude ? String(Q.exclude).split(',') : [],
    source: sourceParam(Q.source),
  };
  const placesP = ctx.api.list('places', { sort: 'name' }).all({ max: 300 }).catch(() => []);
  const params = () => ({
    ...ctx.params, places: f.place || undefined, place_side: f.place ? f.side : undefined, q: f.q || undefined,
    min_distance_km: f.min || undefined, min_avg_kph: f.speed || undefined, efficiency: f.eff, source: f.source || undefined,
  });
  const write = () => ctx.setQuery({ place: f.place || null, side: f.place && f.side !== 'any' ? f.side : null, q: f.q || null,
    min: f.min, speed: f.speed, eff: f.eff === 'plain' ? null : f.eff, exclude: f.exclude.length ? f.exclude.join(',') : null, source: f.source || null });

  // the span pills: a week, a month, or dates of your own
  const pre = ctx.period?.preset;
  const span = pills({ label: 'Span', value: pre === '7d' ? '7d' : pre === '30d' ? '30d' : 'pick', options: [['7d', 'Last 7 days'], ['30d', 'Last 30 days'], ['pick', 'Pick dates']],
    onchange: (val) => { if (val === 'pick') openPeriod(ctx.period, tz, span.querySelector('[data-value="pick"]')); else state.setPeriod({ preset: val }, { push: true }); } });
  const filtersBtn = btn('Filters', { kind: 'soft', icon: 'filter', cover: ['drives.filter.geofence', 'drives.filter.location', 'drives.filter.min_distance', 'drives.filter.min_speed', 'drives.setting.efficiency_method', 'ds.filter.exclude_locations'], onclick: (e) => openFilters(e.currentTarget) });
  const filterNote = h('span', { class: 'filter-note' });
  const bar = filterBar(ctx, { carCover: ['drives.filter.car', 'ds.filter.car'], periodCover: ['drives.filter.time_range', 'ds.filter.time_range'] }, span, filtersBtn, filterNote);
  const places = await placesP;
  const placeName = (pid) => places.find((p) => p.id === pid)?.name || pid;
  function drawFilterNote() {
    const bits = [f.place && `${f.side === 'start' ? 'from' : f.side === 'end' ? 'to' : 'at'} ${placeName(f.place)}`, f.q && `“${f.q}”`, f.min && `over ${format(f.min, 'distance')}`, f.speed && `avg over ${format(f.speed, 'speed')}`,
      f.source && (SOURCE_WORDS[f.source] || f.source), f.eff === 'hill_corrected' && 'hills taken out', f.exclude.length && `${f.exclude.length} place${f.exclude.length > 1 ? 's' : ''} left out of patterns`].filter(Boolean);
    filterNote.textContent = bits.length ? bits.join(' · ') : '';
  }

  /* ------------------------------------------------------------ the filters sheet */
  const sheet = createSheet({ title: 'Filters', cls: 'filters-sheet' });
  ctx.onCleanup(() => sheet.el.remove());
  function openFilters(opener) {
    fillFilters();
    sheet.open(opener);
  }
  function fillFilters() {
    const opts = [['', 'Any place'], ...places.map((p) => [p.id, p.name])];
    const fPlace = field({ label: 'Place', name: 'place', type: 'select', value: f.place, options: opts });
    const fSide = segmented({ label: 'The place is', name: 'side', value: f.side, options: [['any', 'Either end'], ['start', 'The start'], ['end', 'The end']] });
    const fQ = field({ label: 'Words in the address', name: 'q', value: f.q, hint: 'Part of a street, area or place name, for example “Mission”.', attrs: { autocomplete: 'off' }, cover: 'drives.filter.location.hint' });
    const fMin = field({ label: `Shortest drive (${unitLabel('distance')})`, name: 'min', type: 'number', value: f.min != null ? Math.round(convert(f.min, 'distance') * 10) / 10 : '', attrs: { min: '0', step: '0.1', inputmode: 'decimal' } });
    const fSpeed = field({ label: `Slowest average (${unitLabel('speed')})`, name: 'speed', type: 'number', value: f.speed != null ? Math.round(convert(f.speed, 'speed')) : '', attrs: { min: '0', step: '1', inputmode: 'numeric' } });
    const fEff = segmented({ label: 'Efficiency', name: 'eff', value: f.eff, options: [['plain', 'As driven'], ['hill_corrected', 'Hills taken out']] });
    const fSrc = segmented({ label: 'Data from', name: 'source', value: String(f.source).startsWith('import') ? 'import' : f.source, options: SOURCE_OPTIONS, cls: 'seg-tight' });
    const ex = new Set(f.exclude);
    const exList = h('div', { class: 'ex-list', role: 'group', 'aria-label': 'Places left out of the patterns' },
      places.slice(0, 40).map((p) => h('button', { type: 'button', class: 'fpill', 'aria-pressed': String(ex.has(p.id)), onclick: (e) => { if (ex.has(p.id)) ex.delete(p.id); else ex.add(p.id); e.currentTarget.setAttribute('aria-pressed', String(ex.has(p.id))); } }, p.name)));
    const toMetric = (val, kind) => { const n = Number(val); return val === '' || !(n > 0) ? null : n / (convert(1, kind) || 1); };
    sheet.body.replaceChildren(h('div', { class: 'form' }, fPlace, fSide, fQ, fMin, fSpeed, fEff, fSrc,
      h('p', { class: 'hint' }, 'SMO is the car’s CAN Bridge, the Tesla API is the Tesla account this server reads, Imported is history from TeslaMate. Where live and imported drives cover the same time, All shows one side (see Import).'),
      h('div', { class: 'field' }, h('span', { class: 'label' }, 'Leave out of the patterns'), h('p', { class: 'hint', cover: 'ds.filter.exclude_locations.hint' }, 'Drives that start or end at these places are not counted in the driving patterns, for example trips to work.'), exList),
      h('div', { class: 'sheet-actions' },
        btn('Clear', { kind: 'soft', onclick: () => { Object.assign(f, { place: '', side: 'any', q: '', min: null, speed: null, eff: 'plain', exclude: [], source: '' }); apply(); } }),
        btn('Show drives', { kind: 'primary', onclick: () => {
          f.place = fPlace.control.value; f.side = fSide.querySelector('[aria-checked="true"]')?.dataset.value || 'any'; f.q = fQ.control.value.trim();
          f.min = toMetric(fMin.control.value, 'distance'); f.speed = toMetric(fSpeed.control.value, 'speed');
          f.eff = fEff.querySelector('[aria-checked="true"]')?.dataset.value || 'plain'; f.exclude = [...ex];
          f.source = fSrc.querySelector('[aria-checked="true"]')?.dataset.value || '';
          apply();
        } }))));
  }
  fillFilters();
  async function apply() { sheet.close(); write(); drawFilterNote(); await load(); ctx.ready(); }

  /* ------------------------------------------------------------ the page */
  const sumBox = h('div', { class: 'contents' });
  const patBox = h('div', { class: 'contents' });
  const listStart = h('div', { class: 'tl-list-start w-full', cover: ['drives.table', 'shots.drives', 'drives.empty_states'] });
  let pager = null, lastDay = null;
  const more = moreButton(async () => { const b = pager.items.length; await pager.more(); drawFrom(b); return !pager.done; }, { text: 'Show earlier drives' });
  function drawFrom(start) {
    for (const g of byDay(pager.items.slice(start), tz)) {
      if (g.key !== lastDay) {
        const day = pager.items.filter((d) => byDay([d], tz)[0].key === g.key);
        const km = day.reduce((s, d) => s + (d.distance_km || 0), 0);
        ctx.view.insertBefore(dayHead(g.key, tz, `${day.length} ${day.length === 1 ? 'drive' : 'drives'} · ${format(km, 'distance')}`), more);
        lastDay = g.key;
      }
      for (const d of g.items) ctx.view.insertBefore(driveTile(d, ctx), more);
    }
    more.querySelector('button').hidden = pager.done;
  }

  async function load() {
    const p = params();
    pager = ctx.api.pager(api.apiPath`cars/${car.id}/drives`, p, { limit: 30 });
    const [, stats, inc] = await Promise.all([
      pager.more(),
      ctx.api.get(api.apiPath`cars/${car.id}/drive-stats`, { ...ctx.params, exclude_places: f.exclude.length ? f.exclude.join(',') : undefined }).catch(() => null),
      ctx.api.page(api.apiPath`cars/${car.id}/drives`, { ...ctx.params, flags: 'gap,in_progress' }, { limit: 6 }).catch(() => null),
    ]);
    if (!ctx.alive()) return;
    const sm = pager.last?.summary || {};
    sumBox.replaceChildren(
      statTile({ label: 'Distance', value: sm.distance_km, kind: 'distance', icon: 'route', cover: 'drives.summary.distance', sub: `${formatNumber(sm.count || 0)} drives` }),
      statTile({ label: 'Time driving', value: sm.duration_ms, kind: 'duration', icon: 'clock', cover: 'drives.summary.duration', sub: sm.count ? `about ${dur(sm.duration_ms / sm.count)} a drive` : 'no drive' }),
      statTile({ label: 'Energy', value: sm.energy_kwh, kind: 'energy', icon: 'bolt', cover: 'drives.summary.energy', sub: sm.energy_estimated_kwh ? `${format(sm.energy_estimated_kwh, 'energy')} estimated` : 'all measured' }),
      statTile({ label: 'Use', value: sm.wh_per_km, kind: 'efficiency', icon: 'leaf', cover: 'drives.summary.consumption', why: 'no drive', sub: sm.efficiency === 'hill_corrected' ? 'hills taken out' : 'as driven' }));
    drawPatterns(stats);
    drawIncomplete(inc);
    while (listStart.nextSibling && listStart.nextSibling !== more) listStart.nextSibling.remove();
    lastDay = null;
    if (!pager.items.length) {
      ctx.view.insertBefore(emptyTile({ icon: 'route', title: ctx.period?.preset === '7d' ? 'No drives this week' : 'No drives in this period', text: Object.values(f).some((x) => x && x !== 'any' && x !== 'plain' && !(Array.isArray(x) && !x.length)) ? 'No drive matches the filters. Clear them to see every drive.' : 'Pick a longer period above.', cover: 'drives.empty_states' }), more);
      more.querySelector('button').hidden = true;
    } else drawFrom(0);
  }

  function drawPatterns(s) {
    if (!s || !s.count) {
      patBox.replaceChildren(emptyTile({ icon: 'bars', w: 'full', title: 'No driving patterns yet', text: 'Patterns show once the period has drives.', cover: ['ds.empty_states', 'shots.drive-stats'] }));
      return;
    }
    const days = Math.max(1, s.days || 1);
    const bands = (s.speed_bands || []).filter((b) => b.to_kph > 0);
    const ms = s.max_speed || {};
    const top = (s.top_destinations || []).slice(0, 6);
    const most = Math.max(1, ...top.map((t) => t.count));
    patBox.replaceChildren(
      h('div', { class: 'section-head w-full', cover: 'shots.drive-stats' }, h('h2', { class: 'section-title' }, 'Driving patterns'), h('p', { class: 'note', cover: 'ds.empty_states' }, `${formatNumber(s.count)} drives over ${formatNumber(days)} days with driving.`)),
      statTile({ label: 'Drives', value: s.count, kind: 'count', icon: 'route', cover: 'ds.stat.drive_count', sub: `${format(s.distance_km, 'distance')}, ${format(s.energy_kwh, 'energy')}`, node: null }),
      statTile({ label: 'Typical drive', value: s.median_distance_km, kind: 'distance', icon: 'route', cover: ['ds.stat.median_distance', 'ds.stat.distance_total', 'ds.stat.energy_total'], sub: 'half are shorter, half longer' }),
      statTile({ label: 'Per driving day', value: s.distance_km / days, kind: 'distance', icon: 'calendar', cover: ['ds.stat.distance_per_day', 'ds.stat.energy_per_day'], sub: `${format(s.energy_kwh / days, 'energy')} a day` }),
      statTile({ label: 'A year at this pace', value: s.projection?.per_year_km, kind: 'distance', icon: 'trip', cover: ['ds.stat.annual_mileage', 'ds.stat.monthly_mileage'], sub: `${format(s.projection?.per_month_km, 'distance')} a month` }),
      tile({ w: 2, title: 'Top speeds', icon: 'gauge', cover: ['ds.stat.max_speed_period', 'ds.stat.max_speed_30d', 'ds.stat.max_speed_7d'] },
        h('div', { class: 'tops' },
          [['This period', ms.period], ['Last 30 days', ms.last_30d], ['Last 7 days', ms.last_7d]].map(([k, x]) => (x && x.drive
            ? h('a', { class: 'top', href: state.buildUrl('drive', { car: ctx.car.id, drive: x.drive }) }, h('span', { class: 'hero-k' }, k), num(x.kph, 'speed', { size: 'm' }))
            : h('div', { class: 'top' }, h('span', { class: 'hero-k' }, k), num(null, 'speed', { size: 'm', why: 'no drive' })))))),
      tile({ w: 2, title: 'Time at each speed', icon: 'bars', cls: 'chart-tile', cover: 'ds.chart.speed_histogram' },
        bars(bands.map((b) => ({ label: formatNumber(Math.round(convert(b.from_kph, 'speed'))), value: b.share_pct, text: b.share_pct >= 1 ? `${Math.round(b.share_pct)}%` : '', title: `${format(b.from_kph, 'speed')} to ${format(b.to_kph, 'speed')}: ${format(b.share_pct, 'percent')} of the time` })), { label: 'Share of driving time per speed band' }),
        note(`Speed in ${unitLabel('speed')}, from the lower end of each band.`)),
      tile({ w: 2, title: 'Where you drive to most', icon: 'pin', cover: 'ds.chart.top_destinations' },
        h('ol', { class: 'rank' }, top.map((t) => {
          const fill = h('span', { class: 'rank-fill' });
          fill.style.setProperty('--v', (t.count / most).toFixed(4));
          return h('li', { class: 'rank-row' }, h('span', { class: 'rank-name' }, t.label), h('span', { class: 'rank-n' }, `${formatNumber(t.count)}×`), h('span', { class: 'rank-bar', 'aria-hidden': 'true' }, fill));
        }))));
  }

  const incBox = h('div', { class: 'contents' });
  function drawIncomplete(res) {
    const list = res?.items || [];
    incBox.replaceChildren(tile({ w: 2, title: 'Drives with missing data', icon: 'warning', cls: 'inc-tile', cover: ['drives.incomplete.table', 'drives.incomplete.id', 'drives.incomplete.start', 'drives.incomplete.end', 'drives.incomplete.distance', 'drives.incomplete.duration'] },
      h('p', { class: 'note', cover: 'drives.incomplete.note' }, 'A drive shows here when part of it had no data (a gap, lost frames) or it is still going. Its figures cover only the time with data.'),
      list.length ? h('ul', { class: 'inc-list' }, list.map((d) => h('li', null,
        h('a', { class: 'inc-id mono', href: state.buildUrl('drive', { car: ctx.car.id, drive: d.id }) }, `#${d.id.slice(-5)}`),
        h('span', { class: 'inc-when' }, `${formatDate(d.start_ms, tz, { now: api.now() })}, ${formatTime(d.start_ms, tz)} – ${d.end_ms ? formatTime(d.end_ms, tz) : 'now'}`),
        h('span', { class: 'inc-n' }, `${format(d.distance_km, 'distance')} · ${dur(d.duration_ms)}`)))) : h('p', { class: 'lede' }, 'Every drive here has all its data.')));
  }

  drawFilterNote();
  ctx.view.replaceChildren(bar, sumBox, patBox, incBox, listStart, more);
  await load();
  ctx.ready();
  return { onQuery: async () => { await load(); } };
}
