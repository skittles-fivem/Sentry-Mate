// Trip (/cars/{car}/trip?from=&to=): any window as one trip, read from the trip computer. The window is
// the shared period ("all" starts at the car's first drive or charge); shortcuts set it to the last three
// drives, today, yesterday or this month, and a saved trip opens its own window. A second car can ride
// along for the same window. Then the trip counters (distance, speeds, consumption, cost), where the time
// went and the energy, the route of every drive with the charge stops, the activity strip, battery and
// elevation traces, and the trip's drives and charges (GET /cars/{car}/trip, /series, /activity, /tracks).

import * as state from '../shared/state.js';
import { h, svg, replace } from '../shared/dom.js';
import { format, getUnits, unitLabel, rangeLabel } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, formatDateRange, dayKey, dayRange, addDays, startOf } from '../shared/time.js';
import { column } from '../shared/csv.js';
import { icon, hud, plate, btn, linkBtn } from '../core/ui.js';
import { controls, slot, select, carOptions, periodBtn, carBtn, csvBtn, placeLink, stat, rack, counters, eq, plural } from '../core/kit.js';
import { srcChip, sourceLabel, estimated } from '../core/sources.js';
import { tracks } from '../core/tracks.js';
import { strip } from '../core/strip.js';
import { scope, loadUPlot, well } from '../core/chart.js';
import { createMap, addTrace, pin, boundsOf, pad, mapFallback } from '../core/map.js';
import { emptyState } from '../core/states.js';
import { money } from '../core/fmt.js';

export const id = 'trip';
export const css = true;
export const title = () => 'Trip';

const enc = encodeURIComponent;
const DAY = 86400000;
const SPLIT = [
  ['driving', 'Driving', 'driving'], ['charging_ac', 'Charging AC', 'charging'], ['charging_dc', 'Charging DC (fast)', 'updating'],
  ['parked_awake', 'Parked, awake', 'parked'], ['asleep', 'Asleep', 'asleep'], ['not_recorded', 'Not recorded (no data)', 'unknown'],
];
const COST = { tariff: 'place price', manual: 'typed by hand', free_supercharging: 'free Supercharging', teslamate: 'imported' };

async function collect(it, max) { const out = []; for await (const x of it) { out.push(x); if (out.length >= max) break; } return out; }

export async function mount(ctx) {
  const { car, tz } = ctx;
  const cid = car.id;
  const now = ctx.api.now();
  const ideal = getUnits().range === 'ideal';
  const rk = ideal ? 'ideal_range_km' : 'range_km';
  const vsId = ctx.query.vs && ctx.query.vs !== cid ? String(ctx.query.vs) : null;

  let from = ctx.period?.from_ms ?? null;
  const to = Math.min(ctx.period?.to_ms ?? now, now);
  if (from == null) {
    const [d0, c0] = await Promise.all([
      ctx.api.page(`cars/${enc(cid)}/drives`, { sort: 'start', dir: 'asc' }, { limit: 1 }).catch(() => ({ items: [] })),
      ctx.api.page(`cars/${enc(cid)}/charges`, { sort: 'start', dir: 'asc' }, { limit: 1 }).catch(() => ({ items: [] })),
    ]);
    if (!ctx.alive()) return;
    const starts = [d0.items?.[0]?.start_ms, c0.items?.[0]?.start_ms].filter((v) => v != null);
    from = starts.length ? Math.min(...starts) : to - 7 * DAY;
  }
  const win = { from_ms: from, to_ms: to };

  let trip, series, act, last3, saved, fleet, vsTrip = null, vsCar = null;
  try {
    [trip, series, act, last3, saved, fleet] = await Promise.all([
      ctx.api.get(`cars/${enc(cid)}/trip`, win),
      ctx.api.get(`cars/${enc(cid)}/series`, { fields: `soc_pct,${rk},elevation_m`, ...win, max_points: 1500 }).catch(() => null),
      collect(ctx.api.list(`cars/${enc(cid)}/activity`, win, { limit: 1000 }), 20000).catch(() => []),
      ctx.api.page(`cars/${enc(cid)}/drives`, { sort: 'start', dir: 'desc' }, { limit: 3 }).catch(() => ({ items: [] })),
      ctx.api.page('trips', { car: cid }, { limit: 100 }).catch(() => ({ items: [] })),
      ctx.api.page('cars', { sort: 'name' }, { limit: 100 }).catch(() => ({ items: [car] })),
    ]);
    if (vsId) [vsCar, vsTrip] = await Promise.all([ctx.api.get(`cars/${enc(vsId)}`).catch(() => null), ctx.api.get(`cars/${enc(vsId)}/trip`, win).catch(() => null)]);
  } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;

  const nDrives = trip.drives?.length ?? 0;
  const nCharges = trip.charges?.length ?? 0;
  const savedItems = saved.items || [];
  const match = savedItems.find((t) => Math.abs(t.from_ms - from) < 60000 && Math.abs((t.to_ms ?? now) - to) < 60000);
  ctx.setTitle(match ? `Trip ${match.name}` : `Trip ${formatDateRange(from, Math.max(from, to - 1), tz, { year: false })}`);
  const link = (period, vs = vsId) => state.buildUrl('trip', { car: cid, period, query: vs ? { vs } : null });

  /* ---------- window and cars */
  const l3 = last3.items || [];
  const l3From = l3.length ? l3[l3.length - 1].start_ms : null;
  const today = dayKey(now, tz);
  const shortcuts = h('div', { class: 'row tp-short', role: 'group', 'aria-label': 'Window shortcuts' },
    l3From != null ? linkBtn('Last three drives', link({ from: l3From, to: null }), { sm: true, icon: 'route', cover: 'trip.link.last_three_drives' })
      : btn('Last three drives', null, { sm: true, icon: 'route', disabled: true, cover: 'trip.link.last_three_drives' }),
    linkBtn('Today', link({ from: dayRange(today, today, tz).from, to: null }), { sm: true, kind: 'ghost' }),
    linkBtn('Yesterday', link(dayRange(addDays(today, -1), addDays(today, -1), tz)), { sm: true, kind: 'ghost' }),
    linkBtn('This month', link({ from: startOf(now, 'month', tz), to: null }), { sm: true, kind: 'ghost' }));
  const savedSel = select([['', savedItems.length ? `Saved trips (${savedItems.length})` : 'No saved trips yet'], ...savedItems.map((t) => [t.id, `${t.name} · ${formatDateRange(t.from_ms, Math.max(t.from_ms, (t.to_ms ?? now) - 1), tz, { year: true })}`])],
    match?.id || '', (v) => { const t = savedItems.find((x) => x.id === v); if (t) ctx.navigate(link({ from: t.from_ms, to: t.to_ms })); }, { label: 'Open a saved trip' });
  const vsSel = select([['', 'No second car'], ...carOptions((fleet.items || []).filter((c) => c.id !== cid), vsId)], vsId || '',
    (v) => ctx.navigate(link(state.current().period ?? { from, to: ctx.period?.to_ms ?? null }, v || null)), { label: 'A second car for the same window' });

  /* ---------- figures */
  const figsOf = (t, who) => {
    const dist = t.distance || {}, tm = t.time_ms || {}, en = t.energy_kwh || {}, cost = t.cost || {};
    const nD = t.drives?.length ?? 0, nC = t.charges?.length ?? 0;
    const noDrive = !nD;
    const whyDrive = 'No drive in the window';
    const costList = cost.by_currency || [];
    const costWhy = nC ? (cost.priced ? null : 'No price for these charges') : 'No charge in the window';
    return [
      counters([
        { label: `${who}distance`, value: dist.odometer_km, kind: 'distance', digits: 1, cover: who ? null : 'trip.stat.mileage', how: noDrive ? whyDrive : `by odometer; ${plural(nD, 'drive')}` },
        { label: 'Driving time', text: formatDuration(tm.driving || 0) },
        { label: 'Charges', text: String(nC) },
      ]),
      rack([
        stat('Average speed, moving', t.speed_kph?.moving, 'speed', { cover: who ? null : 'trip.stat.speed_moving', why: whyDrive, how: 'distance over the time driving' }),
        stat('With fast charging', t.speed_kph?.incl_dc, 'speed', { cover: who ? null : 'trip.stat.speed_incl_dc', why: whyDrive, how: tm.charging_dc ? `the ${formatDuration(tm.charging_dc)} at DC chargers counted in` : 'no DC charging in the window' }),
        stat('Net consumption', t.wh_per_km?.net, 'efficiency', { cover: who ? null : 'trip.stat.consumption_net', why: noDrive ? whyDrive : 'Too little distance', how: `${format(en.drive_net, 'energy')} while driving, regen off` }),
        stat('Gross consumption', t.wh_per_km?.gross, 'efficiency', { cover: who ? null : 'trip.stat.consumption_gross', why: noDrive ? whyDrive : 'Too little distance', how: `${format(en.gross, 'energy')} in all, parked time included` }),
        stat('Charging cost', costList.length ? money(costList) : null, null, { cover: who ? null : 'trip.stat.charging_cost', why: costWhy, hot: true, how: nC ? `${cost.priced ?? 0} of ${plural(nC, 'charge')} priced${cost.unpriced ? `; ${cost.unpriced} without a price` : ''}` : null }),
        stat('Cost per distance', (cost.per_100km || []).length ? cost.per_100km.map((x) => format(x, 'money_per_distance')).join(' + ') : null, null, { cover: who ? null : 'trip.stat.cost_per_100', why: noDrive ? whyDrive : costWhy || 'Too little distance', how: 'the charging cost over the distance' }),
      ]),
    ];
  };
  const tm = trip.time_ms || {};
  const totalT = Math.max(1, SPLIT.reduce((a, [k]) => a + (tm[k] || 0), 0));
  const en = trip.energy_kwh || {};
  const eRows = [['drive_net', 'Driving (net)', en.drive_net], ['gross', 'Used in all', en.gross], ['added_ac', 'Added on AC', en.added_ac], ['added_dc', 'Added on DC', en.added_dc]];
  const eMax = Math.max(1e-9, ...eRows.map((r) => r[2] || 0));
  const fileBase = [car.name, 'trip', dayKey(from, tz)];

  /* ---------- traces */
  const battWell = well('', 'Battery and range through the trip');
  const elevWell = well('well--sm', 'Elevation through the trip');
  const readEl = h('span', { class: 'scope-read' });
  const resetBtn = btn('Whole trip', () => zoomTo(null), { sm: true, kind: 'ghost', icon: 'arrow-l' });
  resetBtn.hidden = true;
  const mapEl = h('div', { class: 'map tp-map', role: 'img', 'aria-label': 'Map of the trip' });

  /* ---------- drives and charges */
  const listParams = { from_ms: from, to_ms: to };
  const drives = tracks(ctx, {
    title: 'Drives', icon: 'route', path: `cars/${enc(cid)}/drives`, params: listParams, limit: 50, csvName: 'trip-drives', sort: { key: 'start', dir: 'asc' },
    cover: 'trip.drives.table', note: 'Every drive that started in the window, oldest first.',
    rowHref: (d) => state.buildUrl('drive', { car: cid, drive: d.id }),
    columns: [
      { key: 'start', label: 'Start', sort: 'start', firstDir: 'asc', lead: true, cover: ['trip.drives.date', 'trip.drives.link.details'], csv: (d) => new Date(d.start_ms).toISOString(),
        render: (d) => h('a', { class: 'cell-main', href: state.buildUrl('drive', { car: cid, drive: d.id }) }, `${formatDate(d.start_ms, tz, { year: false })}, ${formatTime(d.start_ms, tz)}`) },
      { key: 'from', label: 'From', cover: ['trip.drives.start', 'trip.drives.link.geofence_start'], csv: (d) => d.start?.label, render: (d) => placeLink(d.start) },
      { key: 'to', label: 'To', cover: ['trip.drives.destination', 'trip.drives.link.geofence_end'], csv: (d) => d.end?.label, render: (d) => (d.flags?.in_progress ? h('span', { class: 'plc plc--bare' }, 'driving now') : placeLink(d.end)) },
      { key: 'duration', label: 'Time', num: true, sort: 'duration', cover: ['trip.drives.duration', 'trip.drives.link.duration_hint'], title: 'Opens the timeline for that span', csv: (d) => Math.round((d.duration_ms || 0) / 60000),
        render: (d) => h('a', { href: state.buildUrl('timeline', { car: cid, period: { from: d.start_ms, to: d.end_ms ?? null } }), title: 'That span on the timeline' }, formatDuration(d.duration_ms)) },
      { key: 'distance', label: 'Distance', num: true, sort: 'distance', cover: 'trip.drives.distance', csv: column('Distance', 'distance_km', 'distance'), render: (d) => format(d.distance_km, 'distance') },
      { key: 'soc', label: 'Battery', num: true, cover: ['trip.drives.soc_start', 'trip.drives.soc_end'], render: (d) => `${format(d.start_soc_pct, 'percent')} → ${format(d.end_soc_pct, 'percent')}` },
      { key: 'wh', label: 'Consumption', num: true, sort: 'wh_per_km', cover: 'trip.drives.consumption', csv: column('Consumption', 'wh_per_km', 'efficiency'),
        render: (d) => (d.distance_km != null && d.distance_km < 0.5 ? h('span', { class: 'faint', title: 'Too short to be reliable' }, '—') : [format(d.wh_per_km, 'efficiency'), d.energy_basis === 'estimated' ? ' est.' : '']) },
      { key: 'src', label: 'Source', render: (d) => srcChip(d), csv: (d) => sourceLabel(d) },
    ],
    empty: () => emptyState({ icon: 'route', title: 'No drive in this window', text: 'Pick a longer window, or one of the shortcuts above.' }),
  });
  const charges = tracks(ctx, {
    title: 'Charges', icon: 'bolt', path: `cars/${enc(cid)}/charges`, params: listParams, limit: 50, csvName: 'trip-charges', sort: { key: 'start', dir: 'asc' },
    cover: 'trip.charges.table', note: 'Every charge that started in the window. A cost opens the place price; a charge without one can be priced there.',
    rowHref: (c) => state.buildUrl('charge', { car: cid, charge: c.id }),
    columns: [
      { key: 'start', label: 'Start', sort: 'start', firstDir: 'asc', lead: true, cover: ['trip.charges.date', 'trip.charges.link.details'], csv: (c) => new Date(c.start_ms).toISOString(),
        render: (c) => h('span', { class: 'cell' }, h('a', { class: 'cell-main', href: state.buildUrl('charge', { car: cid, charge: c.id }) }, `${formatDate(c.start_ms, tz, { year: false })}, ${formatTime(c.start_ms, tz)}`),
          h('span', { class: 'cell-sub' }, c.kind ? c.kind.toUpperCase() : '')) },
      { key: 'place', label: 'Place', cover: ['trip.charges.location', 'trip.charges.link.geofence'], csv: (c) => c.place?.name || c.address?.short,
        render: (c) => placeLink({ place: c.place, address: c.address, lat: c.position?.lat, lon: c.position?.lon }) },
      { key: 'duration', label: 'Charging', num: true, sort: 'duration', cover: 'trip.charges.duration', csv: (c) => Math.round((c.charging_ms || 0) / 60000), render: (c) => formatDuration(c.charging_ms) },
      { key: 'cost', label: 'Cost', num: true, sort: 'cost', cover: ['trip.charges.cost', 'trip.charges.link.cost'], csv: (c) => (c.cost ? `${c.cost.amount} ${c.cost.currency}` : ''),
        render: (c) => {
          const href = c.place?.id ? state.buildUrl('places', { query: { place: c.place.id } }) : state.buildUrl('charge', { car: cid, charge: c.id });
          return c.cost ? h('a', { href, title: `Cost from the ${COST[c.cost.source] || c.cost.source || 'price'}` }, format(c.cost, 'money')) : h('a', { class: 'faint', href, title: 'No price: give the place a price' }, 'no price');
        } },
      { key: 'added', label: 'Added', num: true, sort: 'energy', cover: 'trip.charges.energy_added', csv: column('Added', 'energy_added_kwh', 'energy'), render: (c) => (estimated(c) ? [format(c.energy_added_kwh, 'energy'), h('span', { class: 'est', title: 'Estimated' }, ' est.')] : format(c.energy_added_kwh, 'energy')) },
      { key: 'grid', label: 'From grid', num: true, cover: 'trip.charges.energy_used', title: 'Energy drawn, losses included (AC only)', csv: column('From grid', 'energy_grid_kwh', 'energy'), render: (c) => format(c.energy_grid_kwh, 'energy', { why: '' }) },
      { key: 'power', label: 'Avg power', num: true, cover: 'trip.charges.power_avg', csv: column('Average power', 'power_avg_kw', 'power'), render: (c) => format(c.power_avg_kw, 'power') },
      { key: 'range', label: 'Range added', num: true, cover: 'trip.charges.range_added', title: rangeLabel(), csv: column('Range added', 'range_added_km', 'range'), render: (c) => format(c.range_added_km, 'range') },
      { key: 'soc', label: 'Battery', num: true, sort: 'end_soc', cover: ['trip.charges.soc_start', 'trip.charges.soc_end'], render: (c) => `${format(c.start_soc_pct, 'percent')} → ${format(c.end_soc_pct, 'percent')}` },
      { key: 'src', label: 'Source', render: (c) => srcChip(c), csv: (c) => sourceLabel(c) },
    ],
    empty: () => emptyState({ icon: 'bolt', title: 'No charge in this window', text: 'The car did not charge between these times.' }),
  });

  replace(ctx.view, h('div', { class: 'page tp', cover: 'shots.trip' },
    plate({ kicker: `Driving · ${car.name}`, title: match ? match.name : 'Trip', aim: ctx.info.aim, tools: carBtn(ctx),
      extra: h('p', { class: 'tp-window' }, icon('calendar', 'i--sm'), h('span', null, `${formatDateRange(from, Math.max(from, to - 1), tz, { time: true, weekday: true })} · times in ${tz.replace(/_/g, ' ')}`)) }),
    controls({ label: 'Window and cars', note: 'Drives and charges belong to the trip by their start. Drag across a trace below to zoom into part of the trip.' },
      slot('Window', periodBtn(ctx, 'trip.filter.time_range')),
      slot('Shortcuts', shortcuts),
      slot('Saved', savedSel),
      slot('Ride along', vsSel, { cover: 'trip.control.per_car' })),
    h('div', { class: vsTrip ? 'cols' : null },
      hud({ title: `Trip computer · ${car.name}`, icon: 'gauge', tone: 'cyan', cls: 'tp-comp', cover: 'trip.empty_states',
        side: csvBtn({ name: [...fileBase, 'figures'], rows: () => [{ ...trip.distance, ...trip.speed_kph, ...trip.wh_per_km }], columns: [column('Distance', 'odometer_km', 'distance'), column('Moving speed', 'moving', 'speed'), column('Speed with DC', 'incl_dc', 'speed'), column('Net', 'net', 'efficiency'), column('Gross', 'gross', 'efficiency')] }) },
      figsOf(trip, '')),
      vsTrip ? hud({ title: `Riding along · ${vsCar?.name || vsId}`, icon: 'car', cls: 'tp-comp', side: linkBtn('Leave', link(state.current().period ?? { from, to: ctx.period?.to_ms ?? null }, null), { sm: true, kind: 'ghost', icon: 'x' }) },
        figsOf(vsTrip, `${vsCar?.name || ''} `)) : null),
    h('div', { class: 'cols' },
      hud({ title: 'Time spent', icon: 'clock', cover: 'trip.chart.time_spent', side: csvBtn({ name: [...fileBase, 'time'], rows: () => SPLIT.map(([k, l]) => ({ l, m: Math.round((tm[k] || 0) / 60000) })), columns: [column('Part', 'l'), column('Minutes', 'm')] }) },
        h('div', { class: 'tp-time', 'aria-hidden': 'true' }, svg('svg', { class: 'strip-svg', viewBox: '0 0 1000 10', preserveAspectRatio: 'none', 'shape-rendering': 'crispEdges' }, (() => { let x = 0; return SPLIT.map(([k, , tone]) => { const w = ((tm[k] || 0) / totalT) * 1000; const r = w ? svg('rect', { class: 'strip-rect', dataset: { tone }, x: x.toFixed(3), y: 0, width: w.toFixed(3), height: 10 }) : null; x += w; return r; }); })())),
        eq(SPLIT.map(([k, l]) => ({ label: l, value: (tm[k] || 0) / totalT, text: `${formatDuration(tm[k] || 0)} · ${format(((tm[k] || 0) / totalT) * 100, 'percent')}` })))),
      hud({ title: 'Energy', icon: 'bolt', cover: 'trip.chart.energy', side: csvBtn({ name: [...fileBase, 'energy'], rows: () => eRows.map(([, l, v]) => ({ l, v })), columns: [column('Part', 'l'), column('Energy', 'v', 'energy')] }) },
        eq(eRows.map(([, l, v]) => ({ label: l, value: (v || 0) / eMax, text: format(v, 'energy'), tone: /Added/.test(l) ? 'org' : null }))),
        h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, 'Driving is what the pack gave while moving, regen taken off; used in all adds parked use, climate and sentry.')))),
    hud({ title: 'Route', icon: 'route', pad: false, cover: 'trip.map.route', cls: 'tp-map-panel',
      side: h('span', { class: 'keys' }, h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': 'var(--mag)' } }), car.name), vsTrip ? h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': 'var(--cyan)' } }), vsCar?.name || vsId) : null,
        h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': 'var(--org)' } }), 'charge stop')) }, mapEl),
    hud({ title: 'What the car did', icon: 'timeline', cover: 'trip.chart.states' }, strip(act, from, to, { tz, now, tall: true })),
    hud({ title: 'Battery and elevation', icon: 'battery', cover: 'trip.control.zoom', side: [readEl, resetBtn] },
      h('div', { class: 'tp-trace', cover: ['trip.chart.battery.soc', 'trip.chart.battery.range'] },
        h('div', { class: 'scope-head' }, h('span', { class: 'scope-title' }, 'Battery and range'),
          h('span', { class: 'keys' }, h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': 'var(--c-soc)' } }), 'Battery %'), h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': 'var(--cyan)' } }), `${rangeLabel()} (${unitLabel('range')})`))),
        battWell),
      h('div', { class: 'tp-trace', cover: 'trip.chart.elevation' },
        h('div', { class: 'scope-head' }, h('span', { class: 'scope-title' }, `Elevation (${unitLabel('length')})`)), elevWell)),
    drives.el, charges.el));

  try { await Promise.all([drives.ready, charges.ready]); } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  ctx.ready();

  /* ---------- traces, after the first paint */
  const plots = [];
  const x0 = from / 1000, x1 = to / 1000;
  function zoomTo(r) { for (const p of plots) p?.u?.setScale('x', r ? { min: r[0], max: r[1] } : { min: x0, max: x1 }); resetBtn.hidden = !r; }
  if (series?.t_ms?.length) {
    await loadUPlot();
    if (!ctx.alive()) return;
    const V = series.values;
    const opts = { tz, t_ms: series.t_ms, sync: `tp-${cid}`, zoom: (a, b) => zoomTo([a / 1000, b / 1000]) };
    plots.push(await scope(battWell, ctx, { ...opts, series: [{ label: 'Battery', values: V.soc_pct || [], kind: 'percent', color: '--c-soc', fill: true }, { label: 'Range', values: V[rk] || [], kind: 'range', color: '--cyan', scale: 'y2', width: 1.5 }],
      ranges: { y: [0, 100] }, onCursor: (i) => { readEl.textContent = i == null ? '' : `${formatDate(series.t_ms[i], tz, { year: false })}, ${formatTime(series.t_ms[i], tz)} · ${format(V.soc_pct?.[i], 'percent')} · ${format(V[rk]?.[i], 'range')} · ${format(V.elevation_m?.[i], 'length')}`; } }));
    plots.push(await scope(elevWell, ctx, { ...opts, series: [{ label: 'Elevation', values: V.elevation_m || [], kind: 'length', color: '--c-elev', fill: true }] }));
  } else {
    replace(battWell, h('p', { class: 'map-fallback' }, 'No battery readings in this window.'));
    replace(elevWell, h('p', { class: 'map-fallback' }, 'No elevation readings in this window.'));
  }

  /* ---------- the map */
  const n = nDrives + (vsTrip?.drives?.length ?? 0);
  let tr = null;
  try { tr = await ctx.api.get('tracks', { cars: [cid, vsTrip ? vsId : null].filter(Boolean).join(','), ...win, max_points: Math.max(12000, Math.min(200000, n * 6)) }); } catch { tr = null; }
  if (!ctx.alive()) return;
  const lines = (part) => ({ type: 'FeatureCollection', features: (tr?.drives || []).filter((d) => (d.car === cid) === (part === 'a')).map((d) => ({ type: 'Feature', properties: {},
    geometry: { type: 'LineString', coordinates: d.lon.map((lo, i) => [lo, d.lat[i]]).filter((p) => p[0] != null && p[1] != null) } })).filter((f) => f.geometry.coordinates.length > 1) });
  const a = lines('a'), b = lines('b');
  const marks = (trip.markers || []).filter((k) => k.lat != null && k.lon != null);
  const all = [...a.features, ...b.features].flatMap((f) => f.geometry.coordinates).concat(marks.map((k) => [k.lon, k.lat]));
  if (!all.length) { mapEl.append(mapFallback(nDrives ? 'No positions were recorded for these drives.' : 'Nothing was driven in this window, so there is no route.')); return; }
  try {
    const m = await createMap(mapEl, ctx, { bounds: boundsOf(all) });
    if (!m) return;
    await m.ready;
    if (!ctx.alive()) return;
    addTrace(m, 'tp-a', a, { color: '--mag', width: 3 });
    if (b.features.length) addTrace(m, 'tp-b', b, { color: '--cyan', width: 3 });
    // stops first, then A and B on top of them
    for (const k of [...marks].sort((x, y) => (x.kind === 'start' || x.kind === 'end') - (y.kind === 'start' || y.kind === 'end'))) {
      if (k.kind === 'start') pin(m, [k.lon, k.lat], { text: 'A' });
      else if (k.kind === 'end') pin(m, [k.lon, k.lat], { text: 'B', tone: 'cyan' });
      else if (k.kind === 'charge') {
        const p = pin(m, [k.lon, k.lat], { tone: 'org', title: 'Charge stop: open the charge' });
        if (k.charge) p.getElement().addEventListener('click', () => ctx.navigate(state.buildUrl('charge', { car: cid, charge: k.charge })));
      } else pin(m, [k.lon, k.lat], { tone: 'grn', title: 'Night stop' });
    }
    m.frame(() => m.map.fitBounds(boundsOf(all), { padding: pad(40), maxZoom: 15, duration: 0 }));
  } catch (e) { console.warn(e); replace(mapEl, mapFallback()); }

  let t = 0;
  ctx.events('rows', (ev) => {
    if (ev.data?.car !== cid && ev.data?.car !== vsId) return;
    if (!(ev.data.kinds || []).some((k) => k === 'drives' || k === 'charges')) return;
    clearTimeout(t); t = setTimeout(() => { if (ctx.alive()) ctx.reload(); }, 1500);
  });
  ctx.onCleanup(() => clearTimeout(t));
}
