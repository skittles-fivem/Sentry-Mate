// Drives (/cars/{car}/drives): the playlist. Every drive is a track (start, route, length, figures), sorted
// and paged by the server (GET /cars/{car}/drives) with its summary on the first page. The control deck
// holds the period, a place (at the start, the end or either), a text, the shortest drive, the lowest
// average speed, the source, flags and the efficiency basis; places left out feed only the driving
// patterns (GET /cars/{car}/drive-stats). Ticked tracks open together as one trip. Drives with missing data
// have their own list. Rows events reload everything.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { column } from '../shared/csv.js';
import { format, toMetric, unitLabel, convert } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, formatDayKey, isoWeek } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon, hud, plate, btn, linkBtn, tag } from '../core/ui.js';
import { controls, slot, select, search, periodBtn, carBtn, csvBtn, route, stat, rack, counters, eq, spark, band, plural, sourceTag } from '../core/kit.js';
import { tracks } from '../core/tracks.js';
import { columns as colChart, well } from '../core/chart.js';
import { emptyState } from '../core/states.js';
import { popover, toast } from '../core/layer.js';
import { isAdmin } from '../core/prefs.js';

export const id = 'drives';
export const css = true;

export async function mount(ctx) {
  const { car, tz, period } = ctx;
  const cid = car.id;
  const path = `cars/${encodeURIComponent(cid)}/drives`;
  const q = ctx.query;
  const f = {
    places: q.places || '', place_side: q.place_side || 'any', q: q.q || '', min_distance: q.min_distance || '', min_speed: q.min_speed || '',
    source: q.source || '', flags: q.flags || '', efficiency: q.efficiency || 'plain', exclude: q.exclude || '',
  };
  const hill = () => f.efficiency === 'hill_corrected';
  const apiParams = () => ({
    ...ctx.params,
    places: f.places || null, place_side: f.places ? f.place_side : null, q: f.q || null,
    min_distance_km: f.min_distance ? +toMetric(+f.min_distance, 'distance').toFixed(3) : null,
    min_avg_kph: f.min_speed ? +toMetric(+f.min_speed, 'speed').toFixed(2) : null,
    source: f.source || null, flags: f.flags || null, efficiency: f.efficiency,
  });

  // this car's places, the ones it drove to or from most first
  let allPlaces = [];
  try { allPlaces = await ctx.api.list('places', { car: cid, sort: 'drives' }, { limit: 200 }).all(); } catch { allPlaces = []; }
  if (!ctx.alive()) return;
  const chosen = new Set([...(f.places ? f.places.split(',') : []), ...(f.exclude ? f.exclude.split(',') : [])]);
  const places = allPlaces.filter((p) => (p.counts?.drives || 0) > 0 || chosen.has(p.id));
  const byId = new Map(allPlaces.map((p) => [p.id, p]));
  const drivesAt = (p) => plural(p.counts?.drives || 0, 'drive');

  /* ---------- the control deck */
  const syncQuery = () => ctx.setQuery({ places: f.places || null, place_side: f.places && f.place_side !== 'any' ? f.place_side : null, q: f.q || null, min_distance: f.min_distance || null, min_speed: f.min_speed || null, source: f.source || null, flags: f.flags || null, efficiency: f.efficiency === 'plain' ? null : f.efficiency, exclude: f.exclude || null });
  let tmr = 0;
  const set = (patch, wait = 0) => {
    clearTimeout(tmr);
    tmr = setTimeout(() => {
      if (Object.keys(patch).every((k) => patch[k] === f[k])) return;
      Object.assign(f, patch);
      syncQuery(); paintHeads(); paintBasis();
      paintSummary(null, 'loading');
      clearPicks();
      list.reload(apiParams());
    }, wait);
  };
  ctx.onCleanup(() => clearTimeout(tmr));
  const placeSel = select([['', 'Any place'], ...(places.length ? [{ group: `Where ${car.name} drove`, items: places.map((p) => [p.id, `${p.name} (${drivesAt(p)})`]) }] : [])], f.places, (v) => set({ places: v }), { label: 'Place' });
  const sideSel = select([['any', 'start or end'], ['start', 'start'], ['end', 'end']], f.place_side, (v) => set({ place_side: v }), { label: 'Where the place is' });
  const num = (value, label, onv) => { const i = h('input', { class: 'input ctl-num', type: 'number', min: '0', step: 'any', value, 'aria-label': label }); i.addEventListener('input', () => onv(i.value)); return i; };
  const basisEl = h('span');
  const paintBasis = () => { basisEl.textContent = hill() ? 'Consumption is hill corrected: the energy for the climb, less what the descent gave back, is taken off.' : 'Consumption is plain: all the energy the drive took, measured at the pack with regen taken off.'; };
  paintBasis();

  // places left out of the patterns: a popover of check boxes
  const excluded = new Set(f.exclude ? f.exclude.split(',') : []);
  const exclText = h('span', { class: 'ell' });
  const paintExclText = () => {
    const names = [...excluded].map((x) => byId.get(x)?.name).filter(Boolean);
    exclText.textContent = names.length ? `Without ${names.length > 2 ? plural(names.length, 'place') : names.join(', ')}` : 'Every place';
  };
  paintExclText();
  const applyExclude = () => { f.exclude = [...excluded].join(','); syncQuery(); paintExclText(); loadPatterns(); };
  const exclPop = popover({ cls: 'dv-excl', label: 'Leave places out of the patterns', fill: (el) => replace(el,
    h('div', { class: 'dv-excl-in' },
      h('p', { class: 'note', cover: 'ds.filter.exclude_locations.hint' }, icon('info', 'i--sm'), h('span', null, 'Drives that start or end at a ticked place are left out of the driving patterns, so home or work does not top the destinations. The drive list keeps them.')),
      places.length ? h('div', { class: 'dv-excl-list' }, places.map((p) => h('label', { class: 'check' },
        h('input', { type: 'checkbox', checked: excluded.has(p.id), onchange: (e) => { if (e.target.checked) excluded.add(p.id); else excluded.delete(p.id); applyExclude(); } }),
        h('span', null, p.name, h('span', { class: 'faint' }, ` · ${drivesAt(p)}`)))))
        : h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, `${car.name} has not driven to or from a saved place yet.`)),
      h('div', { class: 'row' }, btn('Leave nothing out', () => { excluded.clear(); applyExclude(); exclPop.hide(); }, { sm: true, kind: 'ghost' }), btn('Done', () => exclPop.hide(), { sm: true })))) });
  ctx.onCleanup(() => { exclPop.hide(); exclPop.el.remove(); });
  const exclBtn = h('button', { class: 'btn ctl-btn', type: 'button', 'aria-haspopup': 'dialog', 'aria-expanded': 'false', onclick: (e) => exclPop.toggle(e.currentTarget) }, icon('pin', 'i--sm'), exclText, icon('chev-d', 'i--sm'));

  const deck = controls({ label: 'Filters', note: [h('span', { cover: 'drives.filter.location.hint' }, 'The text keeps drives whose start or destination contains it, in the place name or the address. '), basisEl] },
    slot('Period', periodBtn(ctx, ['drives.filter.time_range', 'ds.filter.time_range'])),
    slot('Place', h('span', { class: 'ctl-in' }, placeSel, sideSel), { cover: 'drives.filter.geofence' }),
    slot('Text', search(f.q, (v) => set({ q: v }), { placeholder: 'Start or destination', ctx }), { wide: true, cover: 'drives.filter.location' }),
    slot(`At least (${unitLabel('distance')})`, num(f.min_distance, `Shortest drive in ${unitLabel('distance')}`, (v) => set({ min_distance: v }, 350)), { cover: 'drives.filter.min_distance' }),
    slot(`Average at least (${unitLabel('speed')})`, num(f.min_speed, `Lowest average speed in ${unitLabel('speed')}`, (v) => set({ min_speed: v }, 350)), { cover: 'drives.filter.min_speed' }),
    slot('Source', select([['', 'Every source'], ['can', 'CAN Bridge'], ['teslamate', 'Imported']], f.source, (v) => set({ source: v }), { label: 'Source' })),
    slot('Flags', select([['', 'Every drive'], ['gap', 'With a gap'], ['in_progress', 'In progress'], ['reduced_range', 'Cold battery']], f.flags, (v) => set({ flags: v }), { label: 'Flags' })),
    slot('Efficiency', select([['plain', 'Plain'], ['hill_corrected', 'Hill corrected']], f.efficiency, (v) => set({ efficiency: v }), { label: 'Basis for consumption and efficiency' }), { cover: 'drives.setting.efficiency_method' }),
    slot('Patterns', exclBtn, { cover: ['ds.filter.exclude_locations', 'ds.filter.exclude_locations.hint'] }));

  /* ---------- summary */
  const summaryEl = h('div');
  const paintSummary = (s, st = null) => {
    s = s || {};
    const why = st === 'loading' ? 'Loading' : st === 'error' ? 'Could not be loaded' : 'No drive matches';
    const blank = st != null || !s.count;
    const hillS = s.efficiency ? s.efficiency === 'hill_corrected' : hill();
    replace(summaryEl, counters([
      { label: 'Drives', value: st ? null : s.count ?? 0, kind: 'count', how: s.without_energy ? `${formatNumber(s.without_energy)} without energy` : 'all with energy' },
      { label: 'Distance', value: blank ? null : s.distance_km, kind: 'distance', digits: 1, cover: 'drives.summary.distance', how: 'the listed drives' },
      { label: 'Driving time', text: blank ? '—' : formatDuration(s.duration_ms), cover: 'drives.summary.duration', how: 'start to end of each' },
      { label: 'Energy', value: blank ? null : s.energy_kwh, kind: 'energy', digits: 1, cover: 'drives.summary.energy', how: s.energy_estimated_kwh ? `${format(s.energy_estimated_kwh, 'energy')} of it estimated (imported)` : 'at the pack, regen off' },
      { label: hillS ? 'Consumption, hill corrected' : 'Consumption', value: blank ? null : s.wh_per_km, kind: 'efficiency', cover: 'drives.summary.consumption', how: blank ? why : 'energy over distance' },
    ]));
  };

  /* ---------- the playlist */
  const picked = new Map();
  const pickText = h('span', { class: 'dv-pick-text', 'aria-live': 'polite' });
  const pickTrip = h('a', { class: 'btn btn--sm btn--go', href: '#' }, icon('flag', 'i--sm'), h('span', null, 'Open as a trip'));
  const pickBar = h('span', { class: 'dv-pick', hidden: true }, pickText, pickTrip, btn('Clear', () => clearPicks(), { sm: true, kind: 'ghost' }));
  function paintPicks() {
    const ds = [...picked.values()];
    pickBar.hidden = !ds.length;
    if (!ds.length) return;
    const from = Math.min(...ds.map((d) => d.start_ms));
    const ends = ds.map((d) => d.end_ms);
    const to = ends.some((e) => e == null) ? null : Math.ceil(Math.max(...ends) / 60000) * 60000;
    pickText.textContent = `${plural(ds.length, 'drive')} · ${format(ds.reduce((a, d) => a + (d.distance_km || 0), 0), 'distance')}`;
    pickTrip.setAttribute('href', state.buildUrl('trip', { car: cid, period: { from, to } }));
  }
  function clearPicks() { picked.clear(); for (const b of ctx.view.querySelectorAll('.dv-tick input')) b.checked = false; paintPicks(); }
  const whHead = h('span'); const effHead = h('span');
  const paintHeads = () => { whHead.textContent = hill() ? 'Wh, hill corr.' : 'Consumption'; effHead.textContent = hill() ? 'Efficiency, hill corr.' : 'Efficiency'; };
  paintHeads();
  const effMeter = (d) => {
    const v = d.efficiency_pct;
    if (v == null) return '—';
    return h('span', { class: 'dv-eff' }, h('span', { class: 'dv-eff-bar', 'aria-hidden': 'true' }, h('i', { vars: { '--w': `${Math.max(4, Math.min(100, v)).toFixed(1)}%` } })), h('span', null, format(v, 'percent')));
  };
  const list = tracks(ctx, {
    title: 'Playlist', icon: 'route', path, params: apiParams(), limit: 25, csvName: 'drives', sort: { key: 'start', dir: 'desc' },
    cover: ['drives.table', 'drives.table.link.details', 'drives.empty_states'], side: pickBar,
    note: 'Energy is measured at the battery pack, regen taken off; imported drives say estimated. Efficiency is the rated consumption against the drive\'s own (100 % = as rated). Tick drives to open them together as a trip.',
    rowHref: (d) => state.buildUrl('drive', { car: cid, drive: d.id }),
    onPage: (pg) => { if (pg.summary) paintSummary(pg.summary); },
    onError: () => paintSummary(null, 'error'),
    columns: [
      { key: 'pick', label: '', csv: false, render: (d) => h('label', { class: 'dv-tick', title: 'Pick this drive for a trip' }, h('input', { type: 'checkbox', checked: picked.has(d.id), 'aria-label': `Pick the drive of ${formatDate(d.start_ms, tz, { year: false })}, ${formatTime(d.start_ms, tz)}`,
        onchange: (e) => { if (e.target.checked) picked.set(d.id, d); else picked.delete(d.id); paintPicks(); } })) },
      { key: 'start', label: 'Start', sort: 'start', firstDir: 'desc', cover: 'drives.table.date', lead: true, csv: (d) => new Date(d.start_ms).toISOString(),
        render: (d, i) => h('span', { class: 'cell' }, h('a', { class: 'cell-main', href: state.buildUrl('drive', { car: cid, drive: d.id }) }, h('span', { class: 'trk-no' }, `${String(i + 1).padStart(2, '0')} `), `${formatDate(d.start_ms, tz, { year: false })}, ${formatTime(d.start_ms, tz)}`),
          h('span', { class: 'cell-sub' }, d.end_ms ? `to ${formatTime(d.end_ms, tz)}` : 'still driving')) },
      { key: 'route', label: 'Route', cls: 'is-wide', cover: ['drives.table.start', 'drives.table.destination', 'drives.table.link.geofence_start', 'drives.table.link.geofence_end'],
        csv: (d) => `${d.start?.label || ''} → ${d.end?.label || ''}`,
        render: (d) => h('span', { class: 'cell' }, route(d.start, d.end_ms != null ? d.end : null), h('span', { class: 'cell-sub' }, [d.start?.address?.short, d.end?.address?.short].filter(Boolean).join(' → '))) },
      { key: 'duration', label: 'Time', num: true, sort: 'duration', cover: 'drives.table.duration', render: (d) => formatDuration(d.duration_ms), csv: (d) => Math.round(d.duration_ms / 60000) },
      { key: 'distance', label: 'Distance', num: true, sort: 'distance', cover: 'drives.table.distance', render: (d) => format(d.distance_km, 'distance'), csv: column('Distance', 'distance_km', 'distance') },
      { key: 'soc', label: 'Battery', num: true, cover: ['drives.table.soc_start', 'drives.table.soc_end'], render: (d) => `${format(d.start_soc_pct, 'percent')} → ${format(d.end_soc_pct, 'percent')}` },
      { key: 'outside', label: 'Out', num: true, title: 'Average outside temperature', cover: 'drives.table.outside_temp', render: (d) => format(d.outside_avg_c, 'temperature') },
      { key: 'speed_max', label: 'Speed, avg · top', num: true, sort: 'speed_max', cover: ['drives.table.speed_avg', 'drives.table.speed_max'], csv: (d) => d.speed_max_kph,
        render: (d) => h('span', { class: 'cell' }, h('span', null, format(d.speed_avg_kph, 'speed')), h('span', { class: 'cell-sub' }, `top ${format(d.speed_max_kph, 'speed')}`)) },
      { key: 'power', label: 'Peak', num: true, cover: 'drives.table.power_max', title: 'Highest power taken from the battery', render: (d) => format(d.power_max_kw, 'power') },
      { key: 'energy', label: 'Energy', num: true, sort: 'energy', cover: 'drives.table.energy', render: (d) => [format(d.energy_kwh, 'energy'), d.energy_basis === 'estimated' ? ' est.' : ''] },
      { key: 'wh', label: whHead, num: true, sort: 'wh_per_km', cover: 'drives.table.consumption', render: (d) => format(d.wh_per_km, 'efficiency') },
      { key: 'eff', label: effHead, cover: 'drives.table.efficiency', render: effMeter, csv: (d) => d.efficiency_pct },
      { key: 'flags', label: 'Flags', cover: 'drives.table.reduced_range', csv: false, render: (d) => h('span', { class: 'flagline' },
        d.flags?.reduced_range ? h('span', { class: 'tag tag--cyan', title: 'Cold battery: part of the charge was locked, so range and consumption read high' }, 'Cold') : null,
        d.flags?.gap_ms ? h('span', { class: 'tag tag--org', title: `Data missing for ${formatDuration(d.flags.gap_ms)}` }, 'Gap') : null,
        d.flags?.in_progress ? tag('Live', 'grn') : null, sourceTag(d.source)) },
    ],
    empty: () => {
      const on = activeFilters();
      return emptyState({ icon: 'route', title: 'No drives on this playlist',
        text: on.length ? `No drive in ${period.label.toLowerCase()} passes every filter that is on: ${on.join('; ')}.` : `${car.name} did not drive in ${period.label.toLowerCase()}. Pick a longer period.`,
        actions: on.length ? btn('Clear the filters', () => { ctx.setQuery({ places: null, place_side: null, q: null, min_distance: null, min_speed: null, source: null, flags: null }); ctx.reload(); }, { icon: 'x' }) : null });
    },
  });
  function activeFilters() {
    const side = { start: 'at the start', end: 'at the end' }[f.place_side] || 'at the start or end';
    return [
      f.places && `place ${f.places.split(',').map((x) => byId.get(x)?.name || x).join(', ')} ${side}`,
      f.q && `text "${f.q}"`,
      f.min_distance && `at least ${format(toMetric(+f.min_distance, 'distance'), 'distance')}`,
      f.min_speed && `average at least ${format(toMetric(+f.min_speed, 'speed'), 'speed')}`,
      f.source && (f.source === 'can' ? 'CAN Bridge drives only' : 'imported drives only'),
      f.flags && ({ gap: 'with a gap', in_progress: 'in progress', reduced_range: 'cold battery' }[f.flags] || f.flags),
    ].filter(Boolean);
  }

  /* ---------- drives with missing data */
  const missing = tracks(ctx, {
    title: 'Drives with missing data', icon: 'warn', tone: 'orange', path, params: { ...ctx.params, flags: 'gap,in_progress' }, limit: 20, csvName: 'drives-missing-data', dense: true,
    note: 'Drives that overlap a gap in the logs, or are still under way. Late logs fill them in by themselves; an admin can also rebuild the period from the stored logs.', noteCover: 'drives.incomplete.note',
    cover: ['drives.incomplete.table', 'drives.incomplete.id', 'drives.incomplete.start', 'drives.incomplete.end', 'drives.incomplete.distance', 'drives.incomplete.duration'],
    side: isAdmin() ? btn('Rebuild the period', rebuild, { sm: true, kind: 'ghost', icon: 'arrow-r' }) : null,
    rowHref: (d) => state.buildUrl('drive', { car: cid, drive: d.id }),
    columns: [
      { key: 'id', label: 'Drive', lead: true, render: (d) => h('a', { class: 'cell-main mono', href: state.buildUrl('drive', { car: cid, drive: d.id }) }, d.id), csv: (d) => d.id },
      { key: 'start', label: 'Start', render: (d) => `${formatDate(d.start_ms, tz, { year: false })}, ${formatTime(d.start_ms, tz)}`, csv: (d) => new Date(d.start_ms).toISOString() },
      { key: 'end', label: 'Last recorded', render: (d) => (d.end_ms ? formatTime(d.end_ms, tz) : 'still going'), csv: (d) => (d.end_ms ? new Date(d.end_ms).toISOString() : '') },
      { key: 'why', label: 'What is missing', cls: 'is-wide', render: (d) => (d.flags?.in_progress ? 'Under way; it ends when the next logs arrive' : `No data for ${formatDuration(d.flags?.gap_ms)} inside it${d.flags?.lost_frames ? ', frames lost' : ''}`) },
      { key: 'distance', label: 'Distance', num: true, render: (d) => format(d.distance_km, 'distance') },
      { key: 'duration', label: 'Time', num: true, render: (d) => formatDuration(d.duration_ms) },
    ],
    empty: () => emptyState({ icon: 'check', title: 'Nothing is missing', text: 'Every drive in this period has all its data.' }),
  });
  async function rebuild() {
    try { const r = await ctx.api.post(`cars/${encodeURIComponent(cid)}/rebuild`, ctx.params); toast(`Rebuilt from ${plural(r.logs_read ?? 0, 'stored log')}.`); ctx.reload(); } catch (e) { toast(`Could not rebuild: ${e.message}`, { kind: 'error' }); }
  }

  /* ---------- driving patterns */
  const patterns = h('div', { class: 'stack', cover: 'ds.empty_states' });
  async function loadPatterns() {
    let s;
    try { s = await ctx.api.get(`cars/${encodeURIComponent(cid)}/drive-stats`, { ...ctx.params, exclude_places: f.exclude || null }); } catch (e) {
      replace(patterns, h('p', { class: 'empty-line' }, icon('warn', 'i--sm'), h('span', null, `The patterns could not be loaded: ${e.message}`))); return;
    }
    if (!ctx.alive()) return;
    const days = s.per_day || [];
    const nDays = Math.max(1, s.days ?? days.length);
    const unit = days.length > 434 ? 'month' : days.length > 62 ? 'week' : 'day';
    const groups = [];
    for (const d of days) {
      const [y, m, dd] = d.date.split('-').map(Number);
      const wk = unit === 'week' ? isoWeek(y, m, dd) : null;
      const key = unit === 'day' ? d.date : unit === 'month' ? d.date.slice(0, 7) : `${wk.year}-W${wk.week}`;
      let g = groups[groups.length - 1];
      if (!g || g.key !== key) { g = { key, first: d.date, count: 0, distance_km: 0, energy_kwh: 0 }; groups.push(g); }
      g.count += d.count || 0; g.distance_km += d.distance_km || 0; g.energy_kwh += d.energy_kwh || 0;
    }
    const endMs = Math.min(ctx.params.to_ms ?? ctx.api.now(), ctx.api.now());
    const endDay = formatDate(endMs - 1, tz, { weekday: false });
    const top = (x) => (x?.kph != null ? h('a', { href: state.buildUrl('drive', { car: cid, drive: x.drive }) }, format(x.kph, 'speed')) : null);
    const bands = (s.speed_bands || []).filter((b) => b.to_kph > 0 && b.seconds > 0);
    const maxBand = Math.max(1e-9, ...bands.map((b) => b.share_pct || 0));
    const dests = (s.top_destinations || []).filter((d) => d.place || !['Driving now', 'Data stopped here'].includes(d.label));
    const maxDest = Math.max(1, ...dests.map((d) => d.count));
    const left = [...excluded].map((x) => byId.get(x)?.name).filter(Boolean);
    const bandLabel = (b) => (b.to_kph == null ? `${format(b.from_kph, 'speed', { digits: 0 })} and over` : `${formatNumber(convert(b.from_kph, 'speed'), { maximumFractionDigits: 0 })}–${format(b.to_kph, 'speed', { digits: 0 })}`);
    const chartEl = well('well--sm', 'Distance per day');
    const sparkSpan = days.length ? `${formatDayKey(days[0].date, { weekday: false })} to ${formatDayKey(days[days.length - 1].date, { weekday: false })}` : 'no days';
    replace(patterns,
      band(2, 'Driving patterns', h('span', { class: 'trk-count' }, left.length ? `without ${left.join(', ')}` : 'every drive; the list filters do not apply')),
      s.count ? null : h('p', { class: 'empty-line' }, icon('warn', 'i--sm'), h('span', null, left.length ? `No drive is left once ${left.join(', ')} ${left.length === 1 ? 'is' : 'are'} left out. Leave fewer places out.` : `${car.name} did not drive in ${period.label.toLowerCase()}, so there are no patterns. Pick a longer period.`)),
      hud({ title: 'Totals and pace', icon: 'stats', side: csvBtn({ name: [car.name, 'drive-patterns'], rows: () => days, columns: [column('Day', 'date'), column('Drives', 'count'), column('Distance', 'distance_km', 'distance'), column('Energy', 'energy_kwh', 'energy')] }) },
        h('div', { class: 'dv-sparks' },
          h('div', { class: 'dv-spark', cover: 'ds.stat.drive_count' }, stat('Drives', formatNumber(s.count || 0), null), spark(groups.map((g) => g.count)), h('span', { class: 'stat-how' }, `per ${unit}, ${sparkSpan}`)),
          h('div', { class: 'dv-spark', cover: 'ds.stat.distance_total' }, stat('Distance', s.count ? s.distance_km : null, 'distance', { why: 'No drive' }), spark(groups.map((g) => g.distance_km)), h('span', { class: 'stat-how' }, `per ${unit}`)),
          h('div', { class: 'dv-spark', cover: 'ds.stat.energy_total' }, stat('Energy', s.count ? s.energy_kwh : null, 'energy', { why: 'No drive' }), spark(groups.map((g) => g.energy_kwh)), h('span', { class: 'stat-how' }, `at the pack, per ${unit}`))),
        rack([
          stat('Typical drive', s.median_distance_km, 'distance', { cover: 'ds.stat.median_distance', how: 'median: half are shorter', why: 'No drive' }),
          stat('Per day', s.count ? (s.distance_km || 0) / nDays : null, 'distance', { cover: 'ds.stat.distance_per_day', how: `over ${plural(nDays, 'day')}; days without driving count as 0`, why: 'No drive' }),
          stat('Energy per day', s.count ? (s.energy_kwh || 0) / nDays : null, 'energy', { cover: 'ds.stat.energy_per_day', how: 'over the same days', why: 'No drive' }),
          stat('Top speed', top(s.max_speed?.period), null, { cover: 'ds.stat.max_speed_period', how: 'in the period; opens that drive', why: 'No drive' }),
          stat('Top, last 30 days', top(s.max_speed?.last_30d), null, { cover: 'ds.stat.max_speed_30d', how: `30 days to ${endDay}, every drive`, why: 'No drive then' }),
          stat('Top, last 7 days', top(s.max_speed?.last_7d), null, { cover: 'ds.stat.max_speed_7d', how: `7 days to ${endDay}, every drive`, why: 'No drive then' }),
          stat('Per month', s.projection?.per_month_km, 'distance', { cover: 'ds.stat.monthly_mileage', how: s.projection?.basis_days ? `projected from ${plural(Math.round(s.projection.basis_days), 'day')}` : 'projected', why: 'Not enough driving' }),
          stat('Per year', s.projection?.per_year_km, 'distance', { cover: 'ds.stat.annual_mileage', how: 'at the same rate', why: 'Not enough driving' }),
        ], { cls: 'dv-rack' }),
        days.length ? [h('p', { class: 'scope-title dv-chart-title' }, "Distance per day, in the car's days"), chartEl] : null),
      h('div', { class: 'cols' },
        hud({ title: 'Time at each speed', icon: 'gauge', cover: 'ds.chart.speed_histogram',
          side: csvBtn({ name: [car.name, 'drive-speeds'], rows: bands, columns: [column('From', 'from_kph', 'speed'), column('To', 'to_kph', 'speed'), column('Seconds', 'seconds'), column('Share', 'share_pct', 'percent')] }) },
        eq(bands.map((b) => ({ label: bandLabel(b), value: (b.share_pct || 0) / maxBand, text: `${format(b.share_pct, 'percent')} · ${formatDuration(b.seconds * 1000)}` })), { empty: 'No driving in the period.' }),
        h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, 'Counted second by second while driving.'))),
        hud({ title: 'Most frequent destinations', icon: 'flag', cover: 'ds.chart.top_destinations',
          side: csvBtn({ name: [car.name, 'drive-destinations'], rows: dests, columns: [column('Destination', 'label'), column('Place id', 'place'), column('Drives', 'count')] }) },
        eq(dests.map((d) => ({ label: d.label, value: d.count / maxDest, text: plural(d.count, 'drive'), tone: 'org',
          href: d.place ? state.buildUrl('drives', { car: cid, period: state.current().period, query: { places: d.place, place_side: 'end' } }) : null })), { empty: 'No destination in the period.' }),
        h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, 'Where each drive ended. A saved place opens its drives.')))));
    if (days.length) colChart(chartEl, ctx, { labels: days.map((d) => formatDayKey(d.date, { weekday: false, year: false })), values: days.map((d) => d.distance_km), kind: 'distance', color: '--mag' });
  }

  replace(ctx.view, h('div', { class: 'page dv', cover: ['shots.drives', 'shots.drive-stats'] },
    plate({ kicker: `Driving · ${car.name}`, title: 'Drives', aim: ctx.info.aim,
      tools: [carBtn(ctx, ['drives.filter.car', 'ds.filter.car']), linkBtn('This period as a trip', ctx.link('trip'), { icon: 'flag', sm: true, kind: 'ghost' })] }),
    deck, hud({ title: 'On this playlist', icon: 'stats', cls: 'dv-sum' }, summaryEl), list.el, patterns, missing.el));
  paintSummary(null, 'loading');

  try { await Promise.all([list.ready, loadPatterns(), missing.ready]); } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  ctx.ready();

  let rowsT = 0;
  ctx.events('rows', (ev) => {
    if (ev.data?.car && ev.data.car !== cid) return;
    clearTimeout(rowsT);
    rowsT = setTimeout(() => { if (!ctx.alive()) return; list.reload(apiParams(), { quiet: true }); missing.reload(null, { quiet: true }); loadPatterns(); }, 2000);
  });
  ctx.onCleanup(() => clearTimeout(rowsT));
}
