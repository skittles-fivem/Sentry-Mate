// Drives (/cars/{car}/drives): a dense sortable table of the drives in the range (GET /cars/{car}/drives,
// sorted and paged by the server, CSV), with a filter line, the summary and a totals row; then the driving
// patterns (GET /cars/{car}/drive-stats) and the drives with missing data.
//
// filter> from:2026-10 to:2026-11 min:10km speed:40 place:home at:end source:imported flag:gap eff:hill not:home,work words
//   from / to   the range (a year, month or day), min the shortest drive, speed the lowest average speed,
//   place a place name at the start or end (at: picks the side), source, flag, eff the consumption basis,
//   not places left out of the patterns, free words: text in the start or destination.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { format, toMetric, convert } from '../shared/units.js';
import { formatTime, formatDuration } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { head, section, kv, note, link } from '../core/ui.js';
import { num, tag, date } from '../core/fmt.js';
import { table, cellBar } from '../core/table.js';
import { empty } from '../core/states.js';
import { filterLine, dateSpan, numberOf } from '../core/filterline.js';
import { placeLink } from './timeline.js';
import { isAdmin } from '../core/prefs.js';
import { toast } from '../core/toast.js';
import { src, est as estTag, srcText, sourceWord, SOURCE_KEY } from '../core/source.js';

export const id = 'drives';

const KEYS = {
  from: 'start of the range (2026, 2026-10, 2026-10-03)', to: 'end of the range', min: 'shortest drive (min:10km)', speed: 'lowest average speed',
  place: 'a place at the start or end', at: 'start | end', source: SOURCE_KEY, flag: 'gap | live | cold', eff: 'plain | hill', not: 'places left out of the patterns',
};

export async function mount(ctx) {
  const { car, tz, period } = ctx;
  const cid = car.id;
  const path = `cars/${encodeURIComponent(cid)}/drives`;

  let places = [];
  try { places = await ctx.api.list('places', { car: cid, sort: 'drives' }, { limit: 200 }).all(); } catch { places = []; }
  if (!ctx.alive()) return;
  const findPlaces = (v) => String(v || '').split(',').map((n) => n.trim().toLowerCase()).filter(Boolean)
    .map((n) => places.find((p) => p.name.toLowerCase() === n) || places.find((p) => p.name.toLowerCase().startsWith(n)) || places.find((p) => p.id === n)).filter(Boolean);

  const toParams = (p) => {
    const k = p.keys;
    const pl = findPlaces(k.place);
    const fl = { gap: 'gap', live: 'in_progress', cold: 'reduced_range' }[String(k.flag || '').toLowerCase()] || null;
    return {
      ...ctx.params,
      places: pl.length ? pl.map((x) => x.id).join(',') : null,
      place_side: pl.length && (k.at === 'start' || k.at === 'end') ? k.at : pl.length ? 'any' : null,
      q: [p.text, k.place && !pl.length ? k.place : null].filter(Boolean).join(' ') || null,
      min_distance_km: numberOf(k.min) != null ? +toMetric(numberOf(k.min), 'distance').toFixed(3) : null,
      min_avg_kph: numberOf(k.speed) != null ? +toMetric(numberOf(k.speed), 'speed').toFixed(2) : null,
      source: sourceWord(k.source),
      flags: fl,
      efficiency: String(k.eff || '').startsWith('hill') ? 'hill_corrected' : 'plain',
    };
  };
  const fl = filterLine({
    keys: KEYS, value: ctx.query.f || '', placeholder: 'from:2026-10 min:10km place:home', label: 'Filter the drives',
    cover: ['drives.filter.geofence', 'drives.filter.location', 'drives.filter.min_distance', 'drives.filter.min_speed', 'drives.setting.efficiency_method', 'ds.filter.exclude_locations'],
    onApply: (p, raw) => {
      ctx.setQuery({ f: raw || null });
      const a = p.keys.from ? dateSpan(p.keys.from, tz) : null;
      const b = p.keys.to ? dateSpan(p.keys.to, tz) : null;
      if (a || b) {
        const from = a ? a[0] : ctx.period.from_ms, to = b ? b[1] : a ? a[1] : ctx.period.to_ms;
        if (from !== ctx.period.from_ms || to !== ctx.period.to_ms) { state.setPeriod({ from, to }, { push: true }); return; }
      }
      params = toParams(p);
      paintSummary(null, 'loading');
      list.reload(params);
      incomplete.reload({ ...ctx.params, flags: 'gap,in_progress' });
      loadPatterns(p);
    },
  });
  let params = toParams(fl.parsed());
  const hill = () => params.efficiency === 'hill_corrected';

  /* ---------- summary */
  const summary = h('div', { class: 'stats' });
  function paintSummary(s, st = null) {
    s = s || {};
    const blank = st || !s.count;
    const why = st === 'loading' ? 'loading' : st === 'error' ? 'could not be loaded' : 'no drive';
    const v = (x, kind) => (blank ? h('span', { class: 'muted' }, why) : num(x, kind));
    replace(summary,
      h('span', { class: 'stat' }, h('span', { class: 'stat-k' }, 'drives'), st ? h('span', { class: 'muted' }, why) : formatNumber(s.count || 0)),
      h('span', { class: 'stat', cover: 'drives.summary.distance' }, h('span', { class: 'stat-k' }, 'distance'), v(s.distance_km, 'distance')),
      h('span', { class: 'stat', cover: 'drives.summary.duration' }, h('span', { class: 'stat-k' }, 'time'), blank ? h('span', { class: 'muted' }, why) : formatDuration(s.duration_ms)),
      h('span', { class: 'stat', cover: 'drives.summary.energy' }, h('span', { class: 'stat-k' }, 'energy'), v(s.energy_kwh, 'energy'), s.energy_estimated_kwh ? h('span', { class: 'muted' }, ` ${format(s.energy_estimated_kwh, 'energy')} est.`) : null),
      h('span', { class: 'stat', cover: 'drives.summary.consumption' }, h('span', { class: 'stat-k' }, hill() ? 'consumption (hill corrected)' : 'consumption'), v(s.wh_per_km, 'efficiency')));
  }

  /* ---------- the table */
  const sum = (pg) => pg?.summary || null;
  const columns = [
    { key: 'start', label: 'date', sort: 'start', firstDir: 'desc', cover: 'drives.table.date', ph: 'main',
      render: (d) => h('a', { class: 'lnk-plain', href: ctx.link('drive', { drive: d.id }) }, `${date(d.start_ms, tz, { year: false })} ${formatTime(d.start_ms, tz)}`),
      total: (pg) => (sum(pg) ? `total ${formatNumber(sum(pg).count)}` : 'total') },
    { key: 'route', label: 'route', cls: 'wrap-cell', ph: 'sub', cover: ['drives.table.start', 'drives.table.destination', 'drives.table.link.geofence_start', 'drives.table.link.geofence_end'],
      render: (d) => h('span', null, placeLink(d.start) || '—', h('span', { class: 'arrow' }, ' -> '), d.end && d.end_ms != null ? placeLink(d.end) || '—' : h('span', { class: 'tone--accent' }, 'driving now')) },
    { key: 'duration', label: 'time', num: true, sort: 'duration', cover: 'drives.table.duration', ph: 'sub', render: (d) => formatDuration(d.duration_ms), total: (pg) => (sum(pg) ? formatDuration(sum(pg).duration_ms) : null) },
    { key: 'distance', label: 'dist', num: true, sort: 'distance', cover: 'drives.table.distance', ph: 'num', render: (d) => num(d.distance_km, 'distance'), total: (pg) => (sum(pg) ? num(sum(pg).distance_km, 'distance') : null) },
    { key: 'soc', label: 'soc', num: true, cover: ['drives.table.soc_start', 'drives.table.soc_end'], ph: 'sub', render: (d) => h('span', null, format(d.start_soc_pct, 'percent'), h('span', { class: 'arrow' }, '->'), format(d.end_soc_pct, 'percent')) },
    { key: 'outside', label: 'out', num: true, cover: 'drives.table.outside_temp', render: (d) => num(d.outside_avg_c, 'temperature') },
    { key: 'speed', label: 'avg', num: true, cover: 'drives.table.speed_avg', render: (d) => num(d.speed_avg_kph, 'speed') },
    { key: 'speed_max', label: 'top', num: true, sort: 'speed_max', cover: 'drives.table.speed_max', render: (d) => num(d.speed_max_kph, 'speed') },
    { key: 'power', label: 'peak', num: true, cover: 'drives.table.power_max', title: 'highest power out of the battery', render: (d) => num(d.power_max_kw, 'power') },
    { key: 'energy', label: 'energy', num: true, sort: 'energy', cover: 'drives.table.energy', ph: 'num',
      render: (d) => h('span', null, num(d.energy_kwh, 'energy'), estTag(d)),
      total: (pg) => (sum(pg) ? num(sum(pg).energy_kwh, 'energy') : null) },
    { key: 'wh', label: 'use', num: true, sort: 'wh_per_km', cover: 'drives.table.consumption', title: 'consumption on the basis chosen with eff:', render: (d) => num(d.wh_per_km, 'efficiency'), total: (pg) => (sum(pg) ? num(sum(pg).wh_per_km, 'efficiency') : null) },
    { key: 'eff', label: 'eff', cover: 'drives.table.efficiency', title: 'rated consumption against this drive’s (100 % = as rated)',
      render: (d) => (d.efficiency_pct == null ? null : h('span', { class: 'nowrap' }, cellBar(d.efficiency_pct / 100, 8, d.efficiency_pct >= 90 ? 'accent' : 'text'), ' ', h('span', { class: 'eff-n' }, format(d.efficiency_pct, 'percent')))) },
    { key: 'flags', label: 'flags', cover: 'drives.table.reduced_range', title: 'cold: cold battery; gap: data missing; live: still driving',
      render: (d) => [d.flags?.reduced_range ? tag('cold', 'warn') : null, d.flags?.gap_ms ? tag('gap', 'bad') : null, d.flags?.in_progress ? tag('live', 'accent') : null].filter(Boolean) },
    { key: 'from', label: 'data', title: 'where the data came from: smo (the CAN Bridge), api (the Tesla API), imp (imported)', ph: 'sub', render: (d) => src(d), value: (d) => srcText(d) },
  ];
  const list = table(ctx, {
    title: 'drives', path, params, limit: 50, columns, sort: { key: 'start', dir: 'desc' }, csvName: 'drives',
    cover: ['drives.table', 'drives.table.link.details', 'drives.empty_states'],
    rowHref: (d) => ctx.link('drive', { drive: d.id }),
    onPage: (pg) => { if (pg?.summary) paintSummary(pg.summary); },
    onError: () => paintSummary(null, 'error'),
    empty: () => empty(`no drives in this range${fl.input.value.trim() ? ' that pass the filter' : ''}`, fl.input.value.trim() ? h('button', { class: 'btn', type: 'button', onclick: () => fl.set('') }, 'clear the filter') : `pick a longer range than ${period.label.toLowerCase()}`),
  });

  /* ---------- drives with missing data */
  const incomplete = table(ctx, {
    title: 'drives with missing data', path, params: { ...ctx.params, flags: 'gap,in_progress' }, limit: 20, csvName: 'drives-missing-data',
    note: 'Drives over a gap in the logs, or still under way. Late logs fill them in by themselves; an admin can also rebuild the range from the stored logs.', noteCover: 'drives.incomplete.note',
    cover: ['drives.incomplete.table', 'drives.incomplete.id', 'drives.incomplete.start', 'drives.incomplete.end', 'drives.incomplete.distance', 'drives.incomplete.duration'],
    side: isAdmin() ? h('button', { class: 'btn btn--line tone--accent', type: 'button', onclick: rebuild, title: 'read the stored logs again for this range' }, '[rebuild]') : null,
    rowHref: (d) => ctx.link('drive', { drive: d.id }),
    columns: [
      { key: 'id', label: 'drive', ph: 'main', render: (d) => h('a', { class: 'lnk-plain', href: ctx.link('drive', { drive: d.id }) }, d.id) },
      { key: 'start', label: 'start', ph: 'sub', render: (d) => `${date(d.start_ms, tz, { year: false })} ${formatTime(d.start_ms, tz)}` },
      { key: 'end', label: 'last recorded', ph: 'sub', render: (d) => (d.end_ms ? formatTime(d.end_ms, tz) : 'still going') },
      { key: 'why', label: 'missing', cls: 'wrap-cell', render: (d) => (d.flags?.in_progress ? 'under way; it ends when the next logs arrive' : `no data for ${formatDuration(d.flags?.gap_ms)}${d.flags?.lost_frames ? ', frames lost' : ''}`) },
      { key: 'distance', label: 'dist', num: true, ph: 'num', render: (d) => num(d.distance_km, 'distance') },
      { key: 'duration', label: 'time', num: true, render: (d) => formatDuration(d.duration_ms) },
    ],
    empty: () => empty('no drive is missing data in this range'),
  });
  async function rebuild() {
    try {
      const r = await ctx.api.post(`cars/${encodeURIComponent(cid)}/rebuild`, ctx.params);
      toast(`Rebuilt from ${formatNumber(r.logs_read ?? 0)} stored logs.`);
      ctx.reload();
    } catch (e) { toast(`Could not rebuild: ${e.message}`, { kind: 'error' }); }
  }

  /* ---------- driving patterns */
  const patterns = h('div', { cover: 'ds.empty_states' });
  async function loadPatterns(p = fl.parsed()) {
    const left = findPlaces(p.keys.not);
    let s;
    try { s = await ctx.api.get(`cars/${encodeURIComponent(cid)}/drive-stats`, { ...ctx.params, exclude_places: left.map((x) => x.id).join(',') || null }); } catch (e) {
      replace(patterns, section({ title: 'driving patterns' }, h('p', { class: 'note' }, `-- the patterns could not be loaded: ${e.message} --`)));
      return;
    }
    if (!ctx.alive()) return;
    const nDays = Math.max(1, s.days ?? (s.per_day || []).length);
    const top = (x) => (x?.kph != null ? h('a', { class: 'lnk-plain', href: ctx.link('drive', { drive: x.drive }) }, num(x.kph, 'speed')) : null);
    const bands = (s.speed_bands || []).filter((b) => b.to_kph == null || (b.to_kph > 0 && b.seconds > 0));
    const maxBand = Math.max(1e-9, ...bands.map((b) => b.share_pct || 0));
    const dests = (s.top_destinations || []).filter((d) => d.place || !['Driving now', 'Data stopped here'].includes(d.label));
    const maxDest = Math.max(1, ...dests.map((d) => d.count));
    const bandLabel = (b) => (b.to_kph == null ? `${format(b.from_kph, 'speed', { digits: 0 })}+` : `${formatNumber(convert(b.from_kph, 'speed'), { maximumFractionDigits: 0 })}-${format(b.to_kph, 'speed', { digits: 0 })}`);
    const none = s.count ? null : h('span', { class: 'muted' }, 'no drive');
    replace(patterns, section({ title: 'driving patterns', side: h('span', { class: 'muted' }, left.length ? `without ${left.map((x) => x.name).join(', ')}; ` : '', 'the list filters do not apply here') },
      s.count ? null : empty(left.length ? 'no drive once those places are left out' : 'no drives in this range', 'pick a longer range'),
      kv([
        ['drives', formatNumber(s.count || 0), { cover: 'ds.stat.drive_count' }],
        ['distance', none || num(s.distance_km, 'distance'), { cover: 'ds.stat.distance_total' }],
        ['energy', none || num(s.energy_kwh, 'energy'), { cover: 'ds.stat.energy_total' }],
        ['typical drive', none || [num(s.median_distance_km, 'distance'), h('span', { class: 'muted' }, ' median')], { cover: 'ds.stat.median_distance' }],
        ['per day', none || [num((s.distance_km || 0) / nDays, 'distance'), h('span', { class: 'muted' }, ` over ${formatNumber(nDays)} days`)], { cover: 'ds.stat.distance_per_day' }],
        ['energy per day', none || num((s.energy_kwh || 0) / nDays, 'energy'), { cover: 'ds.stat.energy_per_day' }],
        ['top speed', top(s.max_speed?.period) || none, { cover: 'ds.stat.max_speed_period' }],
        ['top, last 30 d', top(s.max_speed?.last_30d) || h('span', { class: 'muted' }, 'no drive'), { cover: 'ds.stat.max_speed_30d' }],
        ['top, last 7 d', top(s.max_speed?.last_7d) || h('span', { class: 'muted' }, 'no drive'), { cover: 'ds.stat.max_speed_7d' }],
        ['per month', s.projection?.per_month_km != null ? [num(s.projection.per_month_km, 'distance'), h('span', { class: 'muted' }, ' projected')] : h('span', { class: 'muted' }, 'not enough driving'), { cover: 'ds.stat.monthly_mileage' }],
        ['per year', s.projection?.per_year_km != null ? [num(s.projection.per_year_km, 'distance'), h('span', { class: 'muted' }, ' projected')] : h('span', { class: 'muted' }, 'not enough driving'), { cover: 'ds.stat.annual_mileage' }],
      ]),
      h('div', { class: 'cols2' },
        h('div', { cover: 'ds.chart.speed_histogram' }, h('h3', { class: 'sub-title' }, 'time at each speed'),
          bands.length ? h('div', { class: 'bars' }, bands.map((b) => h('div', { class: 'bar-row' }, h('span', { class: 'bar-k' }, bandLabel(b)), cellBar((b.share_pct || 0) / maxBand, 24), h('span', { class: 'bar-v' }, format(b.share_pct, 'percent'))))) : empty('no driving in this range')),
        h('div', { cover: 'ds.chart.top_destinations' }, h('h3', { class: 'sub-title' }, 'most frequent destinations'),
          dests.length ? h('div', { class: 'bars' }, dests.map((d) => h('div', { class: 'bar-row' },
            d.place ? h('a', { class: 'bar-k lnk-plain', href: state.buildUrl('drives', { car: cid, period: state.current().period, query: { f: `place:"${d.label}" at:end` } }) }, d.label) : h('span', { class: 'bar-k' }, d.label),
            cellBar(d.count / maxDest, 24), h('span', { class: 'bar-v' }, formatNumber(d.count))))) : empty('no destination in this range')))));
  }

  const hd = head(ctx, { title: 'Drives' });
  const btns = hd.querySelectorAll('.ph-btn');
  if (btns[0]) btns[0].dataset.cover = 'drives.filter.time_range ds.filter.time_range';
  if (btns[1]) btns[1].dataset.cover = 'drives.filter.car ds.filter.car';
  replace(ctx.view, h('div', { class: 'page', dataset: { page: 'drives' }, cover: ['shots.drives', 'shots.drive-stats'] },
    hd, fl.el,
    h('p', { class: 'note filt-note', cover: ['drives.filter.location.hint', 'ds.filter.exclude_locations.hint'] },
      'Free words keep drives whose start or destination has them, in the place name or the address. not: leaves places such as home out of the patterns only. ',
      'Energy is measured at the pack with regen taken off; est marks imported drives. ', link(ctx.link('trip'), 'this range as a trip')),
    summary, list.el, patterns, incomplete.el));
  paintSummary(null, 'loading');

  try { await Promise.all([list.ready, loadPatterns(), incomplete.ready]); } catch (e) { ctx.fail(e); return; }
  ctx.ready();

  let rowsT = 0;
  ctx.events('rows', (ev) => {
    if (ev.data?.car && ev.data.car !== cid) return;
    clearTimeout(rowsT);
    rowsT = setTimeout(() => { if (!ctx.alive()) return; list.reload(params, { quiet: true }); incomplete.reload(null, { quiet: true }); loadPatterns(); }, 2000);
  });
  ctx.onCleanup(() => clearTimeout(rowsT));
}
