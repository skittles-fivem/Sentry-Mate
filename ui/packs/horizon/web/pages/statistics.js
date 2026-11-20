// Statistics (/cars/{car}/statistics, or /statistics?cars=all): totals per day, week, month, year or the
// whole period as one row (GET .../statistics?period=), with trip counters for the sum, every row linking
// to that period as a trip, its drives, its charges and its charging habits. Then the odometer over the
// period and the distance per period (GET /cars/{car}/mileage per car), a line per car with logged and
// not-logged distance, and the milestones of the period (odometer marks passed, records). "rows" events
// load it again. Money is per currency and never added across currencies.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { column } from '../shared/csv.js';
import { format, formatMoney, unitLabel, getUnits } from '../shared/units.js';
import { formatDate, formatDateRange, formatDuration, dateFormat, startOf, addPeriod } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon, hud, plate, seg, note, tag } from '../core/ui.js';
import { controls, slot, periodBtn, carBtn, counters, csvBtn, band, plural } from '../core/kit.js';
import { tracks } from '../core/tracks.js';
import { scope, columns as colChart, well } from '../core/chart.js';
import { emptyState } from '../core/states.js';

export const id = 'statistics';
export const css = true;

const enc = encodeURIComponent;
const GROUPS = [['day', 'Day'], ['week', 'Week'], ['month', 'Month'], ['year', 'Year'], ['all', 'Whole period']];
const GROUP_NAME = { day: 'day', week: 'week', month: 'month', year: 'year', all: 'whole period' };
const CAR_COLORS = ['--cyan', '--mag', '--org', '--grn', '--violet', '--yel'];
const sum = (xs) => xs.reduce((a, b) => a + (b ?? 0), 0);
const iso = (ms) => (ms == null ? '' : new Date(ms).toISOString());
const money = (list) => ((list || []).filter((x) => x.amount != null).length ? h('span', { class: 'st-money' }, list.filter((x) => x.amount != null).map((x) => h('span', null, formatMoney(x.amount, x.currency)))) : '—');
const prices = (list, kind) => ((list || []).filter((x) => (x.value ?? x.amount) != null).length
  ? h('span', { class: 'st-money' }, list.filter((x) => (x.value ?? x.amount) != null).map((x) => h('span', null, format({ amount: x.value ?? x.amount, currency: x.currency }, kind)))) : '—');

function defaultGroup(period) {
  const span = (period?.to_ms ?? Date.now()) - (period?.from_ms ?? Date.now() - 400 * 86400000);
  const days = span / 86400000;
  return days <= 31 ? 'day' : days <= 190 ? 'week' : days <= 365 * 4 ? 'month' : 'year';
}
function partOf(row, group, tz) {
  if (group === 'all') return null;
  const s = startOf(row.period_start_ms, group, tz);
  const e = addPeriod(s, group, 1, tz);
  const a = s !== row.period_start_ms, b = e !== row.period_end_ms;
  return !a && !b ? null : a && b ? 'part' : a ? 'from' : 'to';
}

export async function mount(ctx) {
  const { tz, period } = ctx;
  const fixed = ctx.car;
  let group = GROUPS.some(([g]) => g === ctx.query.group) ? ctx.query.group : defaultGroup(period);

  let fleet = [];
  try { fleet = (await ctx.api.page('cars', { sort: 'name' }, { limit: 200 })).items || []; } catch { fleet = fixed ? [fixed] : []; }
  if (!ctx.alive()) return;
  const byId = new Map(fleet.map((c) => [c.id, c]));
  if (fixed) byId.set(fixed.id, fixed);
  const viewCars = fixed ? [fixed.id] : fleet.map((c) => c.id);
  const multi = !fixed;
  const carName = (cid) => byId.get(cid)?.name || cid;
  const carTz = (cid) => byId.get(cid)?.time_zone || tz;
  const colorOf = new Map(viewCars.map((cid, i) => [cid, CAR_COLORS[i % CAR_COLORS.length]]));
  const scopeName = fixed ? fixed.name : `All ${plural(fleet.length, 'car')}`;
  const statsPath = fixed ? `cars/${enc(fixed.id)}/statistics` : 'statistics';
  const linkFor = (page, r) => state.buildUrl(page, { car: r.car, period: { from: r.period_start_ms, to: r.period_end_ms } });

  /* ---------- the control deck */
  const groupSeg = seg(GROUPS, group, (g) => { if (g === group) return; group = g; ctx.setQuery({ group: g }); load(); }, { label: 'One row per', cover: 'statistics.control.time_range' });
  const deck = controls({ note: h('span', { cover: 'statistics.filter.high_precision' }, 'There is no precision switch: net and gross consumption come from the pack\'s own energy counters, so they are exact for a day as much as for a year. Rows at the edges of the period can be part periods; they say so.') },
    slot('Period', periodBtn(ctx, ['statistics.filter.period', 'mi.filter.time_range'])),
    slot('One row per', groupSeg.el));

  const totalsEl = h('div');
  const tableSlot = h('div');
  const mileSlot = h('div', { class: 'stack' });
  const stonesSlot = h('div');

  /* ---------- the period table */
  const periodCell = (r) => {
    const t = r.time_zone || carTz(r.car);
    const part = partOf(r, group, t);
    let main, sub = null;
    if (group === 'day') main = formatDate(r.period_start_ms, t, { year: true });
    else if (group === 'week') { const m = /^(\d{4})-W(\d{2})$/.exec(r.label || ''); main = m ? `Week ${Number(m[2])}, ${m[1]}` : r.label; sub = formatDateRange(r.period_start_ms, r.period_end_ms - 1, t, { year: false }); }
    else if (group === 'month') main = dateFormat(t, { month: 'long', year: 'numeric' }).format(r.period_start_ms);
    else if (group === 'year') main = r.label;
    else { main = 'Whole period'; sub = formatDateRange(r.period_start_ms, r.period_end_ms - 1, t); }
    if (part) sub = `part ${GROUP_NAME[group]}: ${part === 'to' ? `to ${formatDate(r.period_end_ms - 1, t, { weekday: false, year: false })}` : part === 'from' ? `from ${formatDate(r.period_start_ms, t, { weekday: false, year: false })}` : formatDateRange(r.period_start_ms, r.period_end_ms - 1, t, { year: false })}`;
    return h('span', { class: 'cell' }, h('a', { class: 'cell-main', href: linkFor('trip', r), cover: 'statistics.link.trip', title: 'Open this period as a trip' }, main), sub ? h('span', { class: ['cell-sub', part && 'st-part'] }, sub) : null);
  };
  const countLink = (n, page, r, cv) => (n ? h('a', { class: 'plc', href: linkFor(page, r), cover: cv }, formatNumber(n)) : h('span', { class: 'faint', cover: cv }, '0'));
  const dataCell = (r) => {
    const c = r.completeness || {};
    const good = (c.recorded_pct ?? 0) >= 99.5 && !c.dropped_stretches;
    return h('span', { class: 'cell' }, h('span', { class: good ? 'ok' : 'warn' }, c.recorded_pct == null ? '—' : format(c.recorded_pct, 'percent', { digits: 1 })),
      h('span', { class: 'cell-sub' }, [c.unknown_ms ? `${formatDuration(c.unknown_ms)} unknown` : null, c.dropped_stretches ? plural(c.dropped_stretches, 'dropped stretch', 'dropped stretches') : null, c.open_drive || c.open_charge ? 'still going' : null, c.teslamate_rows ? `${formatNumber(c.teslamate_rows)} imported` : null].filter(Boolean).join(' · ') || 'all recorded'));
  };
  const columns = [
    multi ? { key: 'car', label: 'Car', sort: true, value: (r) => carName(r.car), render: (r) => carName(r.car), csv: (r) => carName(r.car) } : null,
    { key: 'period', label: 'Period', sort: true, firstDir: 'desc', lead: true, value: (r) => r.period_start_ms, cover: 'statistics.col.period', render: periodCell, csv: (r) => r.label },
    { key: 'time', label: 'Driving', num: true, sort: true, value: (r) => r.time_driven_ms, cover: 'statistics.col.time_driven', render: (r) => formatDuration(r.time_driven_ms), csv: (r) => Math.round((r.time_driven_ms || 0) / 60000) },
    { key: 'drives', label: 'Drives', num: true, sort: true, value: (r) => r.drives, cover: 'statistics.col.drives_count', render: (r) => countLink(r.drives, 'drives', r, 'statistics.link.drives'), csv: (r) => r.drives },
    { key: 'distance', label: 'Distance', num: true, sort: true, value: (r) => r.distance_odometer_km ?? r.distance_logged_km, cover: 'statistics.col.distance',
      render: (r) => h('span', { class: 'cell' }, format(r.distance_odometer_km ?? r.distance_logged_km, 'distance'), h('span', { class: 'cell-sub' }, `${format(r.distance_logged_km, 'distance')} logged`)), csv: column('Distance', 'distance_odometer_km', 'distance') },
    { key: 'temp', label: 'Outside', num: true, sort: true, value: (r) => r.outside_avg_c, cover: 'statistics.col.avg_temp', render: (r) => format(r.outside_avg_c, 'temperature'), csv: column('Outside', 'outside_avg_c', 'temperature') },
    { key: 'eff', label: 'Efficiency', num: true, sort: true, value: (r) => r.driving_efficiency_pct, cover: 'statistics.col.driving_efficiency', render: (r) => format(r.driving_efficiency_pct, 'percent'), csv: (r) => r.driving_efficiency_pct },
    { key: 'used', label: 'Energy used', num: true, sort: true, value: (r) => r.energy_used_kwh, cover: 'statistics.col.energy_used',
      render: (r) => h('span', { class: 'cell' }, format(r.energy_used_kwh, 'energy'), r.energy_standing_kwh != null ? h('span', { class: 'cell-sub' }, `+${format(r.energy_standing_kwh, 'energy')} parked`) : null), csv: column('Energy used', 'energy_used_kwh', 'energy') },
    { key: 'added', label: 'Energy added', num: true, sort: true, value: (r) => r.energy_added_kwh,
      render: (r) => h('span', { class: 'cell' }, r.charges ? h('a', { class: 'plc', href: linkFor('charges', r), cover: 'statistics.link.charging_stats', title: 'Charging habits and costs of this period' }, format(r.energy_added_kwh, 'energy')) : h('span', { cover: 'statistics.link.charging_stats' }, format(r.energy_added_kwh, 'energy')),
        r.energy_grid_kwh != null ? h('span', { class: 'cell-sub' }, `${format(r.energy_grid_kwh, 'energy')} from the grid`) : null), csv: column('Energy added', 'energy_added_kwh', 'energy') },
    { key: 'charges', label: 'Charges', num: true, sort: true, value: (r) => r.charges, cover: 'statistics.col.charges_count', render: (r) => countLink(r.charges, 'charges', r, 'statistics.link.charges'), csv: (r) => r.charges },
    { key: 'per_charge', label: 'Per charge', num: true, sort: true, value: (r) => r.energy_per_charge_kwh, cover: 'statistics.col.avg_energy_per_charge', render: (r) => format(r.energy_per_charge_kwh, 'energy'), csv: column('Per charge', 'energy_per_charge_kwh', 'energy') },
    { key: 'cost', label: 'Cost', num: true, sort: true, value: (r) => r.cost_by_currency?.[0]?.amount ?? null, cover: 'statistics.col.costs', render: (r) => money(r.cost_by_currency), csv: (r) => (r.cost_by_currency || []).map((x) => `${x.amount} ${x.currency}`).join('; ') },
    { key: 'ppk', label: 'Per kWh', num: true, cover: 'statistics.col.cost_per_kwh', render: (r) => prices(r.price_per_kwh, 'price_energy'), csv: (r) => (r.price_per_kwh || []).map((x) => `${x.value} ${x.currency}`).join('; ') },
    { key: 'p100', label: `Per 100 ${unitLabel('distance')}`, num: true, cover: 'statistics.col.cost_per_distance', render: (r) => prices(r.cost_per_100km, 'money_per_distance'), csv: (r) => (r.cost_per_100km || []).map((x) => `${x.amount} ${x.currency}`).join('; ') },
    { key: 'net', label: 'Net', num: true, sort: true, value: (r) => r.net_wh_per_km, cover: 'statistics.col.consumption_net', title: 'Energy used while driving over the logged distance', render: (r) => format(r.net_wh_per_km, 'efficiency'), csv: column('Net', 'net_wh_per_km', 'efficiency') },
    { key: 'gross', label: 'Gross', num: true, sort: true, value: (r) => r.gross_wh_per_km, cover: 'statistics.col.consumption_gross', title: 'Driving plus parked use, over the logged distance', render: (r) => format(r.gross_wh_per_km, 'efficiency'), csv: column('Gross', 'gross_wh_per_km', 'efficiency') },
    { key: 'overhead', label: 'Overhead', num: true, sort: true, value: (r) => r.overhead_pct, cover: 'statistics.col.consumption_overhead', title: 'How much parked use added on top of driving', render: (r) => format(r.overhead_pct, 'percent'), csv: (r) => r.overhead_pct },
    { key: 'data', label: 'Data', sort: true, value: (r) => r.completeness?.recorded_pct, cover: 'statistics.col.data_complete', render: dataCell, csv: (r) => r.completeness?.recorded_pct },
  ].filter(Boolean);

  function paintTotals(rows) {
    const odo = rows.map((r) => r.distance_odometer_km).filter((x) => x != null);
    const cost = new Map();
    for (const r of rows) for (const c of r.cost_by_currency || []) if (c.amount != null) cost.set(c.currency, (cost.get(c.currency) || 0) + c.amount);
    const tW = sum(rows.map((r) => (r.outside_avg_c != null ? r.outside_avg_c * (r.time_driven_ms || 0) : 0)));
    const tMs = sum(rows.map((r) => (r.outside_avg_c != null ? r.time_driven_ms || 0 : 0)));
    const logged = sum(rows.map((r) => r.distance_logged_km));
    const used = sum(rows.map((r) => r.energy_used_kwh));
    replace(totalsEl, counters([
      { label: 'Distance', value: odo.length ? sum(odo) : null, kind: 'distance', digits: 0, how: `${format(logged, 'distance')} in logged drives` },
      { label: 'Driving time', text: formatDuration(sum(rows.map((r) => r.time_driven_ms))), how: plural(sum(rows.map((r) => r.drives)), 'drive') },
      { label: 'Energy used', value: used, kind: 'energy', digits: 0, how: `+${format(sum(rows.map((r) => r.energy_standing_kwh)), 'energy')} while parked` },
      { label: 'Energy added', value: sum(rows.map((r) => r.energy_added_kwh)), kind: 'energy', digits: 0, how: plural(sum(rows.map((r) => r.charges)), 'charge') },
      { label: 'Net', value: logged ? (used * 1000) / logged : null, kind: 'efficiency', how: 'driving energy over logged distance' },
      { label: 'Outside', value: tMs ? tW / tMs : null, kind: 'temperature', how: 'average while driving' },
      { label: 'Cost', text: cost.size ? [...cost].map(([c, a]) => formatMoney(a, c)).join(' + ') : '—', how: cost.size > 1 ? 'per currency, never added across' : 'charging, priced sessions' },
    ]));
  }

  let gen = 0;
  async function load() {
    const my = ++gen;
    const params = { ...ctx.params, period: group, cars: fixed ? null : 'all' };
    let rows;
    try { rows = await ctx.api.list(statsPath, params, { limit: 500 }).all({ max: 20000 }); } catch (e) {
      if (my === 1) throw e;
      if (ctx.alive()) replace(tableSlot, emptyState({ icon: 'warn', title: 'The rows could not be loaded', text: e.message }));
      return;
    }
    if (my !== gen || !ctx.alive()) return;
    paintTotals(rows);
    const t = tracks(ctx, {
      title: `Per ${GROUP_NAME[group]}`, icon: 'calendar', rows, columns, sort: { key: 'period', dir: 'desc' }, csvName: `statistics-${group}`, cover: 'statistics.table', cls: 'st-table', dense: true,
      note: 'Drives and charges belong to the period they started in, so the rows add up to the Drives and Charges lists. Open a period as a trip, or its drives and charges.',
      empty: () => emptyState({ icon: 'stats', title: 'Nothing in this period', text: `${scopeName} did not drive or charge in ${period.label.toLowerCase()}. Pick a longer period.` }),
    });
    replace(tableSlot, t.el);
    const mileage = await loadMileage();
    if (my !== gen || !ctx.alive()) return;
    await paintMileage(mileage);
    if (my !== gen || !ctx.alive()) return;
    const longest = await eachCar(async (cid) => (await ctx.api.page(`cars/${enc(cid)}/drives`, { ...ctx.params, sort: 'distance', dir: 'desc' }, { limit: 1 }))?.items?.[0] || null);
    if (my !== gen || !ctx.alive()) return;
    paintStones(rows, mileage, longest);
  }

  async function eachCar(fn) {
    const out = new Map();
    const ids = [...viewCars];
    const work = async () => { while (ids.length) { const cid = ids.shift(); try { out.set(cid, await fn(cid)); } catch { out.set(cid, null); } } };
    await Promise.all(Array.from({ length: Math.min(4, ids.length) }, work));
    return out;
  }
  const mileGroup = () => (group === 'all' ? 'month' : group);
  const loadMileage = () => eachCar(async (cid) => {
    const pages = [];
    for await (const pg of ctx.api.list(`cars/${enc(cid)}/mileage`, { ...ctx.params, group: mileGroup() }, { limit: 500 }).pages()) { pages.push(pg); if (pages.length > 40) break; }
    return { items: pages.flatMap((pg) => pg.items || []), projection: pages[0]?.projection || null };
  });

  /* ---------- odometer and mileage */
  async function paintMileage(mileage) {
    const cars = viewCars.filter((cid) => mileage.get(cid)?.items?.length);
    const axis = [...new Map(cars.flatMap((cid) => mileage.get(cid).items.map((x) => [x.label, x.period_start_ms]))).entries()].sort((a, b) => a[1] - b[1]);
    const perCar = viewCars.map((cid) => {
      const m = mileage.get(cid);
      const items = m?.items || [];
      const firstOdo = items.find((x) => x.odometer_start_km != null)?.odometer_start_km ?? null;
      const lastOdo = [...items].reverse().find((x) => x.odometer_end_km != null)?.odometer_end_km ?? null;
      return { car: cid, start: firstOdo, end: lastOdo, distance: sum(items.map((x) => x.distance_km)), logged: sum(items.map((x) => x.logged_km)), not_logged: sum(items.map((x) => x.not_logged_km)), per_year: m?.projection?.km_per_year ?? null, basis_days: m?.projection?.basis_days ?? null };
    });
    const perCarList = tracks(ctx, {
      title: multi ? 'Each car' : 'This car', icon: 'car', rows: perCar, csvName: 'mileage-per-car', dense: true, sort: { key: 'car', dir: 'asc' },
      columns: [
        { key: 'car', label: 'Car', sort: true, lead: true, value: (r) => carName(r.car), render: (r) => h('span', { class: 'st-car' }, h('i', { class: 'st-sw', vars: { '--c': `var(${colorOf.get(r.car)})` } }), carName(r.car)), csv: (r) => carName(r.car) },
        { key: 'odo', label: 'Odometer', num: true, sort: true, value: (r) => r.end, render: (r) => h('span', { class: 'cell' }, format(r.end, 'odometer'), h('span', { class: 'cell-sub' }, r.start != null ? `from ${format(r.start, 'odometer')}` : 'no reading')), csv: column('Odometer', 'end', 'odometer') },
        { key: 'distance', label: 'Distance', num: true, sort: true, render: (r) => format(r.distance, 'distance'), csv: column('Distance', 'distance', 'distance') },
        { key: 'logged', label: 'Logged', num: true, sort: true, render: (r) => format(r.logged, 'distance'), csv: column('Logged', 'logged', 'distance') },
        { key: 'not_logged', label: 'Not logged', num: true, sort: true, render: (r) => h('span', { class: r.not_logged > 0.5 ? 'warn' : null }, format(r.not_logged, 'distance')), csv: column('Not logged', 'not_logged', 'distance') },
        { key: 'per_year', label: 'Per year', num: true, sort: true, render: (r) => h('span', { class: 'cell' }, format(r.per_year, 'distance', { digits: 0 }), h('span', { class: 'cell-sub' }, r.per_year != null ? `at the pace of ${plural(Math.round(r.basis_days || 0), 'day')}` : 'needs a week')), csv: column('Per year', 'per_year', 'distance') },
      ],
      note: 'Distance is by odometer. Logged is what recorded drives cover; not logged is what the car drove while nothing was recorded.',
    });
    if (!axis.length) {
      replace(mileSlot, hud({ title: 'Odometer and mileage', icon: 'gauge', cover: 'mi.chart.odometer' }, emptyState({ icon: 'gauge', title: 'No odometer readings', text: `${scopeName} has no odometer reading in ${period.label.toLowerCase()}. Pick a longer period.` })), perCarList.el);
      return;
    }
    const at = (cid, label, key) => mileage.get(cid).items.find((x) => x.label === label)?.[key] ?? null;
    const t_ms = axis.map(([, ms]) => ms);
    const odoEl = well('', 'Odometer over the period');
    const distEl = well('', 'Distance per period');
    const odoRead = h('span', { class: 'scope-read' });
    const distRead = h('span', { class: 'scope-read' });
    const dist = axis.map(([label]) => { const v = cars.map((cid) => at(cid, label, 'distance_km')).filter((x) => x != null); return v.length ? sum(v) : null; });
    const logged = axis.map(([label]) => { const v = cars.map((cid) => at(cid, label, 'logged_km')).filter((x) => x != null); return v.length ? sum(v) : null; });
    const t0 = carTz(cars[0]);
    const label = (i) => { const g = mileGroup(); const ms = t_ms[i]; return g === 'day' ? formatDate(ms, t0, { weekday: false, year: false }) : g === 'month' ? dateFormat(t0, { month: 'short', year: '2-digit' }).format(ms) : g === 'year' ? axis[i][0] : formatDate(ms, t0, { weekday: false, year: false }); };
    const odoCsv = () => cars.flatMap((cid) => mileage.get(cid).items.map((x) => ({ ...x, car: carName(cid) })));
    const odoCols = [column('Car', 'car'), column('Period', 'label'), { label: 'From', value: (x) => iso(x.period_start_ms) }, column('Odometer at start', 'odometer_start_km', 'odometer'), column('Odometer at end', 'odometer_end_km', 'odometer'), column('Distance', 'distance_km', 'distance'), column('Logged', 'logged_km', 'distance'), column('Not logged', 'not_logged_km', 'distance')];
    replace(mileSlot,
      hud({ title: 'Odometer and mileage', icon: 'gauge', tone: 'cyan', cover: 'mi.chart.odometer', side: csvBtn({ name: [scopeName, 'odometer', mileGroup()], rows: odoCsv, columns: odoCols }) },
        h('div', { class: 'cols' },
          h('div', { class: 'st-chart' },
            h('div', { class: 'scope-head' }, h('span', { class: 'scope-title' }, `Odometer · ${unitLabel('odometer')}`), h('span', { class: 'keys' }, cars.map((cid) => h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': `var(${colorOf.get(cid)})` } }), carName(cid)))), odoRead),
            odoEl),
          h('div', { class: 'st-chart' },
            h('div', { class: 'scope-head' }, h('span', { class: 'scope-title' }, `Distance per ${GROUP_NAME[mileGroup()]} · ${unitLabel('distance')}`), h('span'), distRead),
            distEl)),
        note(`The odometer is read at the end of each ${GROUP_NAME[mileGroup()]}; the line breaks where there was no reading.${group === 'all' ? ' The charts are by month, since the table shows the whole period as one row.' : ''}`)),
      perCarList.el);
    await scope(odoEl, ctx, {
      tz: t0, t_ms, series: cars.map((cid) => ({ label: carName(cid), values: axis.map(([lb]) => at(cid, lb, 'odometer_end_km')), kind: 'odometer', color: colorOf.get(cid), width: 2.5 })), axes: { y: { size: 64 } },
      onCursor: (i) => { odoRead.textContent = i == null ? '' : `${label(i)} · ${cars.map((cid) => `${multi ? `${carName(cid)} ` : ''}${format(at(cid, axis[i][0], 'odometer_end_km'), 'odometer')}`).join(' · ')}`; },
    });
    if (!ctx.alive()) return;
    await colChart(distEl, ctx, { labels: axis.map((_, i) => label(i)), values: dist, kind: 'distance', color: '--mag',
      onCursor: (i) => { distRead.textContent = i == null || dist[i] == null ? '' : `${label(i)} · ${format(dist[i], 'distance')}, ${format(logged[i], 'distance')} logged`; } });
  }

  /* ---------- milestones */
  function paintStones(rows, mileage, longest) {
    const items = [];
    const mi = getUnits().distance === 'mi';
    const toKm = (v) => (mi ? v * 1.609344 : v);
    const u = mi ? 'mi' : 'km';
    const MARKS = [1000, 5000, 10000, 25000, 50000, 75000, 100000, 150000, 200000, 250000, 300000, 400000, 500000];
    const marks = [];
    for (const cid of viewCars) for (const it of mileage.get(cid)?.items || []) {
      if (it.odometer_start_km == null || it.odometer_end_km == null) continue;
      for (const m of MARKS) if (it.odometer_start_km < toKm(m) && it.odometer_end_km >= toKm(m)) marks.push({ cid, m, it });
    }
    marks.sort((a, b) => b.it.period_start_ms - a.it.period_start_ms);
    for (const { cid, m, it } of marks.slice(0, 4)) {
      items.push(stone('flag', 'Milestone', `${formatNumber(m)} ${u} on the odometer`, `${multi ? `${carName(cid)} passed it` : 'Passed'} in ${markWhen(it, carTz(cid))}`,
        h('a', { class: 'plc', href: state.buildUrl('trip', { car: cid, period: { from: it.period_start_ms, to: it.period_end_ms } }) }, 'Open that period')));
    }
    let best = null;
    for (const [cid, d] of longest) if (d && (!best || (d.distance_km ?? 0) > (best.d.distance_km ?? 0))) best = { cid, d };
    if (best) items.push(stone('route', 'Record', 'Longest drive', `${format(best.d.distance_km, 'distance')} in ${formatDuration(best.d.duration_ms)}, ${formatDate(best.d.start_ms, carTz(best.cid), { year: true })}${multi ? ` (${carName(best.cid)})` : ''}`,
      h('a', { class: 'plc', href: state.buildUrl('drive', { car: best.cid, drive: best.d.id }) }, 'Open the drive')));
    const pick = (xs, f, dir) => xs.filter((r) => f(r) != null).sort((a, b) => (f(a) - f(b)) * dir)[0] || null;
    const what = group === 'all' ? 'period' : GROUP_NAME[group];
    const rowName = (r) => { const t = r.time_zone || carTz(r.car); return group === 'day' ? formatDate(r.period_start_ms, t, { year: true }) : group === 'month' ? dateFormat(t, { month: 'long', year: 'numeric' }).format(r.period_start_ms) : group === 'week' ? `the week of ${formatDate(r.period_start_ms, t, { weekday: false })}` : group === 'year' ? r.label : 'the period'; };
    const trip = (r) => h('a', { class: 'plc', href: linkFor('trip', r) }, 'Open it as a trip');
    const who = (r) => (multi ? ` (${carName(r.car)})` : '');
    if (group !== 'all' && rows.length > 1) {
      const far = pick(rows, (r) => r.distance_odometer_km ?? r.distance_logged_km, -1);
      if (far) items.push(stone('gauge', 'Record', `Furthest ${what}`, `${format(far.distance_odometer_km ?? far.distance_logged_km, 'distance')} in ${rowName(far)}${who(far)}`, trip(far)));
    }
    const minKm = { day: 20, week: 60, month: 150, year: 500, all: 50 }[group];
    const eff = pick(rows.filter((r) => (r.distance_logged_km ?? 0) >= minKm), (r) => r.net_wh_per_km, 1);
    if (eff) items.push(stone('leaf', 'Record', group === 'all' ? 'Most efficient' : `Most efficient ${what}`, `${format(eff.net_wh_per_km, 'efficiency')} net in ${rowName(eff)}${who(eff)}`, trip(eff)));
    const chg = pick(rows, (r) => r.energy_added_kwh || null, -1);
    if (chg && rows.length > 1) items.push(stone('bolt', 'Record', `Most charged in a ${what}`, `${format(chg.energy_added_kwh, 'energy')} over ${plural(chg.charges || 0, 'charge')} in ${rowName(chg)}${who(chg)}`, h('a', { class: 'plc', href: linkFor('charges', chg) }, 'Open the charges')));
    const cold = pick(rows.filter((r) => r.drives), (r) => r.outside_avg_c, 1);
    if (cold && rows.length > 1) items.push(stone('thermo', 'Record', `Coldest ${what} on the road`, `${format(cold.outside_avg_c, 'temperature')} on average while driving in ${rowName(cold)}${who(cold)}`, trip(cold)));
    replace(stonesSlot, hud({ title: 'Milestones', icon: 'flag', tone: 'orange', cover: 'projects.achievements', side: h('span', { class: 'trk-count' }, 'firsts and records in this period') },
      items.length ? h('ul', { class: 'st-stones' }, items) : emptyState({ icon: 'flag', title: 'No milestones yet', text: `Nothing in ${period.label.toLowerCase()} stands out yet. Pick a longer period.` }),
      note(`Worked out from the rows above and each car's longest drive. Odometer marks are in ${u}, the unit you picked.`)));
  }
  function markWhen(it, t) {
    const g = mileGroup();
    if (g === 'day') return formatDate(it.period_start_ms, t, { year: true });
    if (g === 'month') return dateFormat(t, { month: 'long', year: 'numeric' }).format(it.period_start_ms);
    if (g === 'year') return it.label;
    return `the week of ${formatDate(it.period_start_ms, t, { weekday: false })}`;
  }
  function stone(ic, kind, title, text, link) {
    return h('li', { class: ['st-stone', kind === 'Milestone' && 'is-first'] },
      h('span', { class: 'st-stone-lamp' }, icon(ic)),
      h('span', { class: 'st-stone-body' }, h('span', { class: 'st-stone-kind' }, kind), h('b', null, title), h('span', null, text), link));
  }

  replace(ctx.view, h('div', { class: 'page st', cover: ['shots.statistics', 'shots.mileage'] },
    plate({ kicker: `Reports · ${scopeName}`, title: 'Statistics', aim: ctx.info.aim,
      tools: fixed ? [carBtn(ctx, ['statistics.filter.car', 'mi.section.per_car'])] : [tag('Every car', 'cyan')] }),
    deck,
    hud({ title: 'Sum of the rows', icon: 'stats', cls: 'st-sum' }, totalsEl),
    tableSlot,
    band(1, 'The road so far'),
    mileSlot,
    stonesSlot));
  if (!fixed) ctx.view.querySelector('.plate-tools .tag')?.setAttribute('data-cover', 'statistics.filter.car mi.section.per_car');

  let timer = 0;
  ctx.onCleanup(() => clearTimeout(timer));
  ctx.events('rows', () => { clearTimeout(timer); timer = setTimeout(() => load(), 1500); });

  try { await load(); } catch (e) { ctx.fail(e); return; }
  if (ctx.alive()) ctx.ready();
}
