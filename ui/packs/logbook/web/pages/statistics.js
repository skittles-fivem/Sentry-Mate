// Sentry Mate, "Logbook" UI pack: Statistics (/cars/{car}/statistics, or /statistics?cars=all|ids).
//
// Totals per day, ISO week, month or year (or the whole period as one row), for one car, several or all,
// and the odometer over time. Data:
//   GET /cars/{car}/statistics or /statistics?cars=  with period=<group>  (the period table, the bars)
//   the same with period=all                                               (the totals, the per-car mileage)
//   GET /cars/{car}/mileage                                                (odometer lines, odometer marks)
//   GET /cars/{car}/drives?sort=distance&dir=desc&limit=1                  (the longest drive, a record)
//   GET /cars                                                              (the car pick list)
//
// Periods follow each car's own time zone: with several cars, a calendar period ('This year', 'Last 7
// days', dates picked in the calendar) is read in each car's zone, one request per zone, so 'This year'
// is each car's own year. A drive or charge belongs to the period it started in, so the rows add up to
// the Drives and Charges lists of the same span. Money stays per currency. The grouping and
// the cars live in the address; the period is the frame's. Charts are SVG in percentages of boxes sized in
// rem, so they stay sharp at any size, and every table and chart has its own CSV.

import { h, svg, replace } from '../shared/dom.js';
import { format, formatMoney, unitLabel, convert } from '../shared/units.js';
import { formatDate, formatDuration, formatDayKey, formatDateRange, dateFormat, startOf, addPeriod, periodLabel, resolvePeriod, periodParams, dayKey, dayStart, DAY } from '../shared/time.js';
import { formatNumber, tn } from '../shared/i18n.js';
import { downloadCsv, csvFilename, column, timeColumn } from '../shared/csv.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { pageHead, q, figure, icon, empty, errorView, flag, button, sep } from '../core/ui.js';
import { dataTable } from '../core/table.js';
import { popover } from '../core/dialog.js';
import { choice, soldPickRows } from '../core/form.js';
import { niceSplits } from '../core/chart.js';
import { zoneLabel } from '../core/format.js';

// a tick label near an end of the axis lines up with that end instead of centring on it (it would stick out)
const edgeTick = (x) => (x > 94 ? 'is-end' : x < 6 ? 'is-start' : null);

export const id = 'statistics';
export const css = true;
export const title = () => 'Statistics';

const GROUPS = [['day', 'Day'], ['week', 'Week'], ['month', 'Month'], ['year', 'Year'], ['all', 'Whole period']];
const GROUP_WORD = { day: 'day', week: 'ISO week (Monday to Sunday)', month: 'month', year: 'year', all: 'whole period' };
const MAX_LINES = 8;          // odometer lines in one chart
const MAX_DRIVE_LOOKUPS = 20; // cars asked for their longest drive
const idList = (v) => (v ? String(v).split(',').map((x) => x.trim()).filter(Boolean) : []);
const sum = (list, f) => list.reduce((a, x) => a + (f(x) || 0), 0);
const p3 = (v) => `${(Number.isFinite(v) ? v : 0).toFixed(3)}%`;
const csvButton = (label, onclick, cover = null) => h('button', { type: 'button', class: 'button button-quiet button-small', onclick, cover, 'aria-label': `Download CSV: ${label}` }, icon('download'), 'Download CSV');
const mixed = (list) => new Set(list.map((x) => x.currency)).size > 1;
const moneyText = (list) => { const l = (list || []).filter((x) => x && x.amount != null); return l.map((x) => formatMoney(x.amount, x.currency) + (mixed(l) ? ` ${x.currency}` : '')).join(' + '); };
const priceText = (list) => { const l = (list || []).filter((x) => x && x.value != null); return l.map((x) => format({ amount: x.value, currency: x.currency }, 'price_energy') + (mixed(l) ? ` ${x.currency}` : '')).join(' + '); };
const per100Text = (list) => { const l = (list || []).filter((x) => x && x.amount != null); return l.map((x) => format({ amount: x.amount, currency: x.currency }, 'money_per_distance') + (mixed(l) ? ` ${x.currency}` : '')).join(' + '); };
const addMoney = (rows, key, f = 'amount') => {
  const by = new Map();
  for (const r of rows) for (const x of r[key] || []) if (x && x[f] != null) by.set(x.currency, (by.get(x.currency) || 0) + x[f]);
  return [...by].sort((a, b) => a[0].localeCompare(b[0])).map(([currency, amount]) => ({ currency, amount }));
};
// The total cost of a row: priced plus estimated, per currency (an older server sends only the priced cost).
const totalOf = (r) => r.cost_total_by_currency || r.cost_by_currency || [];
const daysOf = (r) => Math.max(1, (r.period_end_ms - r.period_start_ms) / DAY);
const scaled = (list, f) => (list || []).filter((x) => x && x.amount != null).map((x) => ({ currency: x.currency, amount: x.amount * f }));
const totalPer100 = (r) => r.cost_total_per_100km || ((r.distance_logged_km || 0) > 1 ? scaled(totalOf(r), 100 / r.distance_logged_km) : []);
const totalPerDay = (r) => r.cost_total_per_day || scaled(totalOf(r), 1 / daysOf(r));
// A blank consumption figure says why: no distance, or too little to divide by.
const perKm = (kwh, km) => (km > 0.1 ? (kwh / km) * 1000 : null);
/** Overhead: the share of all energy (gross) that did not go to driving, 1 - net / gross; 0 to 100 %. */
const overheadOf = (r) => (r && r.net_wh_per_km != null && r.gross_wh_per_km > 0 ? Math.max(0, Math.min(100, (1 - r.net_wh_per_km / r.gross_wh_per_km) * 100)) : null);
const CALENDAR_PRESETS = ['7d', '30d', 'year'];

/** The automatic grouping when the address names none: days for a month or less, months up to three years, then years. */
function autoGroup(period, now) {
  if (!period || period.from_ms == null) return 'year';
  const days = ((period.to_ms ?? now) - period.from_ms) / DAY;
  return days <= 45 ? 'day' : days <= 3 * 366 ? 'month' : 'year';
}

// The page's reads; prefetch() warms the first ones with the same calls when a link here is under the pointer.
const fleetRead = (ctx) => ctx.api.list('cars', { sort: 'name' }, { limit: 200 });
const queryGroup = (q) => (GROUPS.some(([k]) => k === q.group) ? q.group : null);
const mileGroupOf = (g, period, now) => (g === 'all' ? autoGroup(period, now) === 'day' ? 'day' : 'month' : g);
const statsList = (ctx, path, params, cars, period) => ctx.api.list(path, { ...params, cars, period }, { limit: 500 });
const mileageList = (ctx, cid, params, group) => ctx.api.list(api.apiPath`cars/${cid}/mileage`, { ...params, group }, { limit: 500 });
const longestRead = (ctx, cid, params) => ctx.api.get(api.apiPath`cars/${cid}/drives`, { ...params, sort: 'distance', dir: 'desc', limit: 1 });
// one car's statistics (without a car, the cars the fleet has decide the queries)
export function prefetch(ctx) {
  const reads = [fleetRead(ctx).all()];
  if (ctx.car) {
    const now = api.now(), cid = ctx.car.id, path = api.apiPath`cars/${cid}/statistics`;
    const g = queryGroup(ctx.query) || autoGroup(ctx.period, now);
    reads.push(statsList(ctx, path, ctx.params, undefined, g).all(), g === 'all' ? null : statsList(ctx, path, ctx.params, undefined, 'all').all(),
      mileageList(ctx, cid, ctx.params, mileGroupOf(g, ctx.period, now)).all(), longestRead(ctx, cid, ctx.params));
  }
  return Promise.all(reads);
}

export async function mount(ctx) {
  const fixedCar = ctx.car;
  const tz = ctx.tz;
  const now = api.now();

  /* ---------------------------------------------------------------- which cars */

  let fleet = [];
  try { fleet = await fleetRead(ctx).all({ max: 2000 }); } catch (err) { if (ctx.signal.aborted) throw err; fleet = fixedCar ? [fixedCar] : []; }
  const byId = new Map(fleet.map((c) => [c.id, c]));
  if (fixedCar) byId.set(fixedCar.id, fixedCar);
  const asked = idList(ctx.query.cars).filter((x) => byId.has(x));
  const all = !fixedCar && (!asked.length || ctx.query.cars === 'all');
  const viewCars = fixedCar ? [fixedCar.id] : all ? fleet.map((c) => c.id) : asked;
  const multi = viewCars.length > 1;
  const carsParam = fixedCar ? undefined : all ? 'all' : asked.join(',');
  const carName = (cid) => (byId.get(cid) && byId.get(cid).name) || cid;
  const scopeName = fixedCar ? fixedCar.name : all ? (fleet.length === 1 ? fleet[0].name : `All ${formatNumber(fleet.length)} cars`) : asked.length === 1 ? carName(asked[0]) : `${formatNumber(asked.length)} cars`;
  const statsPath = fixedCar ? api.apiPath`cars/${fixedCar.id}/statistics` : 'statistics';
  const colourOf = new Map(viewCars.map((cid, i) => [cid, i < MAX_LINES ? `st-c${i}` : 'st-cx']));

  // The period's edges per time zone. One car: the frame's period, already in its zone. Several cars: a
  // calendar period is read again in each car's own zone (its 'This year' starts on its own Jan 1), so no
  // row is cut only because the zones differ; a rolling 24 hours or exact times stay the same moments.
  const zoneOfCar = (cid) => (byId.get(cid) && byId.get(cid).time_zone) || tz;
  const p0 = ctx.period;
  const isDayEdge = (ms) => Number.isFinite(ms) && startOf(ms, 'day', tz) === ms;
  const edgeMode = !multi || !p0 ? 'one' : CALENDAR_PRESETS.includes(p0.preset) ? 'calendar'
    : p0.preset === 'custom' && (isDayEdge(p0.from_ms) || isDayEdge(p0.to_ms)) ? 'calendar' : p0.preset === 'all' ? 'all' : 'moments';
  function edgesIn(zone) {
    if (edgeMode !== 'calendar' || zone === tz) return { ...ctx.params };
    if (CALENDAR_PRESETS.includes(p0.preset)) return periodParams(resolvePeriod({ preset: p0.preset }, { now, tz: zone }));
    const out = { ...ctx.params };
    if (isDayEdge(p0.from_ms)) out.from_ms = dayStart(dayKey(p0.from_ms, tz), zone);
    if (isDayEdge(p0.to_ms)) out.to_ms = Math.min(dayStart(dayKey(p0.to_ms, tz), zone), Math.max(now, out.from_ms ?? now));
    return out;
  }
  const zoneGroups = (() => {
    if (!multi) return [{ zone: tz, cars: carsParam, ids: viewCars, params: { ...ctx.params } }];
    const by = new Map();
    for (const cid of viewCars) { const z = edgeMode === 'calendar' ? zoneOfCar(cid) : tz; if (!by.has(z)) by.set(z, []); by.get(z).push(cid); }
    if (by.size === 1) { const [z] = [...by.keys()]; return [{ zone: z, cars: carsParam, ids: viewCars, params: edgesIn(z) }]; }
    return [...by].map(([z, ids]) => ({ zone: z, cars: ids.join(','), ids, params: edgesIn(z) }));
  })();
  const paramsOf = (cid) => (zoneGroups.find((x) => x.ids.includes(cid)) || zoneGroups[0]).params;

  let group = queryGroup(ctx.query);
  // Several cars: lines from zero by default (odometers of different cars differ by tens of thousands).
  let odoScale = ctx.query.scale === 'odometer' ? 'odometer' : 'from';
  const groupNow = () => group || autoGroup(ctx.period, now);

  /* ---------------------------------------------------------------- head and filters */

  ctx.main.append(pageHead({
    eyebrow: fixedCar ? fixedCar.name : 'Reports',
    title: 'Statistics',
    deck: `Totals per day, week, month or year, and the odometer over time, for ${multi ? 'every car you picked, in one table' : scopeName}.`,
  }));

  const carField = carPicker(ctx, {
    fleet, chosen: fixedCar ? [fixedCar.id] : all ? [] : asked,
    onapply: (ids) => {
      const query = { group };
      const every = !ids.length || (fleet.length > 1 && ids.length === fleet.length);
      ctx.go(ids.length === 1 ? state.buildUrl('statistics', { car: ids[0], period: ctx.state.period, query })
        : state.buildUrl('statistics', { period: ctx.state.period, query: { ...query, cars: every ? 'all' : ids.join(',') } }));
    },
  });
  const groupHint = h('p', { class: 'field-hint' });
  const drawGroupHint = () => {
    const g = groupNow();
    replace(groupHint, g === 'all' ? (multi ? 'One row per car for the whole period.' : 'One row for the whole period.') : multi ? `One row per ${GROUP_WORD[g]} and car, in each car’s own time zone.` : `One row per ${GROUP_WORD[g]}, in ${zoneLabel(tz)}.`,
      group ? null : ' Chosen for the length of the period; pick one to keep it.');
  };
  const groupChoice = choice({
    label: 'Group by', name: 'statistics-group', value: groupNow(), options: GROUPS,
    onchange: (v) => { group = v; ctx.setQuery({ group: v }); drawGroupHint(); loadAll().catch(() => {}); },
  });
  drawGroupHint();

  const periodLine = h('p', { class: 'st-period', cover: ['statistics.control.time_range', 'mi.filter.time_range'] },
    h('span', { class: 'field-label' }, 'Period'),
    h('span', { class: 'st-period-text' }, periodLabel(ctx.period, tz, now)), sep(),
    h('span', { class: 'st-period-why' }, !multi ? `days in ${zoneLabel(tz)}`
      : edgeMode === 'calendar' ? 'its dates read in each car’s own zone, and each car’s rows in its zone'
        : edgeMode === 'all' ? 'each car’s rows in its own zone'
          : `the same moments for every car (times in ${zoneLabel(tz)}); each car’s rows in its own zone`, sep(),
      'a drive or charge counts in the period it started in; the first and last rows may be part periods'),
    button('Change the period', { cls: 'button-small', iconName: 'calendar', onclick: () => document.querySelector('.period-trigger')?.click() }));

  const filters = h('section', { class: 'st-filters', 'aria-label': 'What the page covers' },
    periodLine,
    h('div', { class: 'st-filter-row' },
      h('div', { class: 'field st-f-group', cover: 'statistics.filter.period' }, groupChoice, groupHint),
      h('div', { class: 'field st-f-cars', cover: 'statistics.filter.car' },
        h('label', { class: 'field-label', for: 'statistics-cars' }, 'Cars'), carField,
        h('p', { class: 'field-hint' }, fleet.length > 1 ? 'One, several or every car, in one table with a car column.' : 'The one car on this server.')),
      h('div', { class: 'st-precision', cover: 'statistics.filter.high_precision' },
        h('p', { class: 'st-precision-title' }, icon('info'), 'Gross consumption is exact'),
        h('p', { class: 'st-precision-text' }, 'It adds everything the pack gave out while parked (Sentry, climate, sleep, updates), measured continuously, to the energy of the drives. There is no precision switch to set: every period is worked out the same, complete way.'))));
  ctx.main.append(filters);

  /* ---------------------------------------------------------------- the parts */

  const totalsBox = h('div', { class: 'st-totals-box' });
  const tableBox = h('div', { class: 'st-table-box' });
  const mileBox = h('div', { class: 'st-mile-box' });
  const stonesBox = h('div', { class: 'st-stones-box' });
  ctx.main.append(totalsBox,
    h('section', { class: 'block st-table', id: 'st-table', 'aria-labelledby': 'st-table-title', cover: 'shots.statistics' },
      h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'st-table-title' }, 'Per period', h('span', { class: 'label-note', id: 'st-table-note' }))),
      tableBox),
    h('section', { class: 'block st-mile', id: 'st-mile', 'aria-labelledby': 'st-mile-title', cover: 'shots.mileage' },
      h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'st-mile-title' }, 'Odometer and mileage', h('span', { class: 'label-note' }, 'how far, and when'))),
      mileBox),
    h('section', { class: 'block st-stones', id: 'st-stones', 'aria-labelledby': 'st-stones-title', cover: 'projects.achievements' },
      h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'st-stones-title' }, 'Milestones', h('span', { class: 'label-note' }, 'odometer marks passed and records, from the stored drives and charges'))),
      stonesBox));

  if (!viewCars.length) {
    replace(tableBox, empty({ title: 'No car to show.', text: fleet.length ? 'Pick one or more cars above.' : 'Add a car in Settings: Cars, then its drives and charges show up here.' }));
    ctx.ready();
    return {};
  }

  // Each load replaces what the one before it drew; a slower, older answer is dropped.
  let gen = 0;
  let shownGroup = null; // the grouping the totals, charts and milestones show now
  const groupText = (g) => (g === 'all' ? 'the whole period as one row' : `by ${g === 'week' ? 'ISO week' : g}`);
  const setTableNote = (g, extra = '') => {
    const note = document.getElementById('st-table-note');
    if (note) note.textContent = (g === 'all' ? 'the whole period, one row per car' : `one row per ${g === 'week' ? 'ISO week' : g}${multi ? ' and car' : ''}, newest first`) + extra;
  };
  // The rows of every zone group, newest first.
  const statsRows = (period, max) => Promise.all(zoneGroups.map((zg) => statsList(ctx, statsPath, zg.params, zg.cars, period).all({ max })))
    .then((lists) => lists.flat().sort((a, b) => b.period_start_ms - a.period_start_ms || String(a.car).localeCompare(String(b.car))));
  /** The rows did not load: say so in the table, and mark what still shows another grouping as such. */
  function rowsFailed(err, g) {
    setTableNote(g, ' (could not be loaded)');
    replace(tableBox, errorView(err, { retry: () => loadAll(), eyebrow: 'Could not load the statistics', title: 'The statistics could not be loaded.', home: false }));
    for (const box of [totalsBox, mileBox, stonesBox]) {
      box.querySelector(':scope > .st-stale-note')?.remove();
      if (!box.childElementCount) continue;
      box.classList.add('is-stale');
      box.prepend(h('p', { class: 'st-stale-note', role: 'note' }, icon('warning'),
        shownGroup && shownGroup !== g ? `Still from the previous grouping (${groupText(shownGroup)}): the ${g === 'all' ? 'whole period' : g === 'week' ? 'weeks' : `${g}s`} could not be loaded.` : 'Not brought up to date: the statistics could not be loaded.'));
    }
  }
  async function loadAll() {
    const my = ++gen;
    const g = groupNow();
    const mileGroup = mileGroupOf(g, ctx.period, now);
    const lineCars = viewCars.slice(0, MAX_LINES);
    const [rowsR, wholeR, mileR, longR] = await Promise.allSettled([
      statsRows(g, 20000),
      g === 'all' ? null : statsRows('all', 5000),
      Promise.allSettled(lineCars.map((cid) => mileageList(ctx, cid, paramsOf(cid), mileGroup).all({ max: 5000 }))),
      Promise.allSettled(viewCars.slice(0, MAX_DRIVE_LOOKUPS).map((cid) => longestRead(ctx, cid, paramsOf(cid)))),
    ]);
    if (my !== gen || !ctx.alive()) return;
    if (rowsR.status === 'rejected') { if (!api.isAbort(rowsR.reason)) rowsFailed(rowsR.reason, g); return; }
    for (const box of [totalsBox, mileBox, stonesBox]) box.classList.remove('is-stale');
    shownGroup = g;
    const rows = rowsR.value;
    const whole = g === 'all' ? rows : wholeR.status === 'fulfilled' ? wholeR.value : null;
    const mileage = mileR.status === 'fulfilled' ? mileR.value.map((x, i) => ({ car: lineCars[i], items: x.status === 'fulfilled' ? x.value : null, error: x.status === 'rejected' ? x.reason : null })) : [];
    const longest = longR.status === 'fulfilled' ? longR.value.map((x) => (x.status === 'fulfilled' ? (x.value.items || [])[0] : null)).filter(Boolean) : [];

    drawTotals(whole, wholeR.status === 'rejected' ? wholeR.reason : null);
    drawTable(rows, g);
    drawMileage(rows, whole, mileage, g, mileGroup);
    drawStones(rows, mileage, longest, g);
  }

  /* ---------------------------------------------------------------- totals for the period */

  function drawTotals(whole, err) {
    if (!whole) { replace(totalsBox, errorView(err, { retry: () => loadAll(), eyebrow: 'Could not load the totals', title: 'The totals for the period could not be loaded.', home: false })); return; }
    const km = sum(whole, (r) => r.distance_odometer_km), logged = sum(whole, (r) => r.distance_logged_km);
    const used = sum(whole, (r) => r.energy_used_kwh), standing = sum(whole, (r) => r.energy_standing_kwh);
    const drives = sum(whole, (r) => r.drives), charges = sum(whole, (r) => r.charges);
    const span = sum(whole, (r) => r.period_end_ms - r.period_start_ms), unknown = sum(whole, (r) => r.completeness && r.completeness.unknown_ms);
    const dropped = sum(whole, (r) => r.completeness && r.completeness.dropped_stretches);
    const cost = addMoney(whole.map((r) => ({ t: totalOf(r) })), 't');
    const est = addMoney(whole, 'cost_estimated_by_currency').filter((x) => x.amount);
    const priced = whole.some((r) => r.charges_priced != null) ? sum(whole, (r) => r.charges_priced) : null;
    const days = Math.max(1, sum(whole, (r) => (r.period_end_ms - r.period_start_ms) / DAY));
    const many = mixed(cost);
    const curT = (amount, c, kind = 'money') => format({ amount, currency: c }, kind) + (many ? ` ${c}` : '');
    const costNote = cost.length ? [
      priced != null ? `priced ${formatNumber(priced)} of ${tn(charges, '{n} charge', '{n} charges')}` : 'per currency, never added across',
      est.length ? `${moneyText(est)} estimated` : null,
      logged > 1 ? cost.map((x) => curT((x.amount / logged) * 100, x.currency, 'money_per_distance')).join(' + ') : null,
      `${cost.map((x) => curT(x.amount / days, x.currency)).join(' + ')} per day`,
    ].filter(Boolean).join(' · ') : 'per currency, never added across';
    const odoDropped = sum(whole, (r) => r.completeness && r.completeness.odometer_dropped);
    const why = whole.length ? 'nothing driven' : 'no data in the period';
    replace(totalsBox, h('div', { class: 'figs st-totals', 'aria-label': 'Totals for the period' },
      figure('Distance', q(whole.length ? km : null, 'distance', { why }), whole.length ? [`by odometer; ${format(logged, 'distance')} on logged drives`, km - logged > 0.5 ? `, ${format(km - logged, 'distance')} in gaps` : '', odoDropped > 0 ? `; ${tn(odoDropped, '{n} odometer reading', '{n} odometer readings')} left out` : ''] : null),
      figure('Time driven', drives ? formatDuration(sum(whole, (r) => r.time_driven_ms)) : q(null, 'duration', { why: 'no drive' }), drives ? tn(drives, 'over {n} drive', 'over {n} drives') : null),
      figure('Energy added', q(charges ? sum(whole, (r) => r.energy_added_kwh) : null, 'energy', { why: 'no charge' }),
        charges ? `${tn(charges, '{n} charge', '{n} charges')}; ${format(sum(whole, (r) => r.energy_grid_kwh), 'energy')} metered from the grid (AC)` : 'into the pack'),
      figure('Total cost', cost.length ? moneyText(cost) : q(null, 'money', { why: charges ? 'no charge has a price' : 'no charge' }), costNote),
      figure('Net consumption', q(perKm(used, logged), 'efficiency', { why }), drives ? `driving only: ${format(used, 'energy')} over ${format(logged, 'distance')} of logged drives` : null),
      figure('Gross consumption', q(perKm(used + standing, logged), 'efficiency', { why }), drives ? `with ${format(standing, 'energy')} used while parked` : null),
      figure('Recorded', recorded(span ? (100 * (span - unknown)) / span : null, unknown, 'no time in the period'),
        span ? `${formatDuration(unknown)} not recorded; ${tn(dropped, '{n} dropped stretch', '{n} dropped stretches')}` : null)));
  }

  /* ---------------------------------------------------------------- the period table */

  const zoneOf = (r) => r.time_zone || (byId.get(r.car) && byId.get(r.car).time_zone) || tz;
  const spanOf = (r) => ({ from: r.period_start_ms, to: r.period_end_ms });
  const linkFor = (page, r) => state.buildUrl(page, { car: r.car, period: spanOf(r) });

  /** 'November 2026', 'Week 47, 2026', 'Mon, Nov 16, 2026', '2026' or 'Whole period', in the row's zone. */
  function periodName(r, g) {
    const z = zoneOf(r);
    if (g === 'day') return formatDayKey(r.label, { weekday: true, year: true });
    if (g === 'week') { const m = /^(\d{4})-W(\d{2})$/.exec(r.label || ''); return m ? `Week ${Number(m[2])}, ${m[1]}` : r.label; }
    if (g === 'month') return dateFormat(z, { month: 'long', year: 'numeric' }).format(r.period_start_ms);
    if (g === 'year') return r.label || String(dateFormat(z, { year: 'numeric' }).format(r.period_start_ms));
    return 'Whole period';
  }
  /** 'so far' for a period that runs up to now, 'part' for one the chosen period cuts, else null. */
  function partOf(r, g) {
    if (g === 'all') return null;
    const z = zoneOf(r);
    const s0 = startOf(r.period_start_ms, g === 'week' ? 'week' : g, z);
    const e0 = addPeriod(s0, g === 'week' ? 'week' : g, 1, z);
    if (r.period_end_ms < e0 && r.period_end_ms >= now - 120000) return 'so far';
    return s0 !== r.period_start_ms || e0 !== r.period_end_ms ? 'part' : null;
  }
  const spanText = (r) => formatDateRange(r.period_start_ms, Math.max(r.period_start_ms, r.period_end_ms - 1), zoneOf(r), { year: true });

  function periodCell(r, g) {
    const part = partOf(r, g);
    return h('span', null,
      h('a', { class: 'cell-main st-period-link', href: linkFor('trip', r), title: 'Open this period as a trip: map, time, energy and cost' }, periodName(r, g)),
      h('span', { class: 'cell-sub' }, g === 'day' ? 'open as a trip' : spanText(r),
        part ? [' ', flag(part === 'so far' ? 'So far' : 'Part', { kind: part === 'so far' ? null : 'estimated', title: part === 'so far' ? 'This period is still running: the row counts up to now.' : 'The chosen period cuts this one: the row counts only the part inside it.' })] : null));
  }
  function dataCell(r) {
    const c = r.completeness || {};
    const lines = [];
    if (c.unknown_ms >= 60000) lines.push(`${formatDuration(c.unknown_ms)} not recorded`);
    if (c.dropped_stretches) lines.push(tn(c.dropped_stretches, '{n} dropped stretch', '{n} dropped stretches'));
    const flags = [];
    if (c.open_drive) flags.push(flag('Drive under way', { kind: 'warn', title: 'A drive was still going, or cut by a gap: its figures may grow.' }));
    if (c.open_charge) flags.push(flag('Charge under way', { kind: 'warn', title: 'A charge was still going, or cut by a gap: its figures may grow.' }));
    if (c.teslamate_rows) flags.push(flag(`${formatNumber(c.teslamate_rows)} imported`, { kind: 'estimated', title: 'Drives and charges imported from TeslaMate: their energy is estimated.' }));
    const pct = c.recorded_pct;
    return h('span', { class: 'st-data' },
      h('span', { class: ['st-rec', { 'is-low': pct != null && pct < 95, 'is-full': pct != null && pct >= 99.95 }] }, recorded(pct, c.unknown_ms, 'no time'), h('span', { class: 'st-rec-word' }, ' recorded')),
      lines.length ? h('span', { class: 'cell-sub' }, lines.join(' · ')) : h('span', { class: 'cell-sub' }, !c.unknown_ms ? 'nothing missing' : 'under a minute missing'),
      flags.length ? h('span', { class: 'st-flags' }, flags) : null);
  }
  const zeroText = (r, what) => h('span', { class: 'st-quiet' }, what);
  // under the total: the estimated part, else how many charges have a price
  const costSub = (r) => {
    const est = (r.cost_estimated_by_currency || []).filter((x) => x && x.amount);
    if (est.length) return `est. ${moneyText(est)}`;
    return r.charges_priced != null && r.charges && r.charges_priced < r.charges && totalOf(r).length ? `${formatNumber(r.charges_priced)} of ${formatNumber(r.charges)} priced` : '';
  };

  function drawTable(rows, g) {
    setTableNote(g);
    const unitD = unitLabel('distance');
    const columns = [
      multi ? { key: 'car', label: 'Car', sort: true, cls: 'c-car', value: (r) => carName(r.car), cell: (r) => h('a', { class: 'st-car', href: state.buildUrl('car', { car: r.car }) }, h('span', { class: ['st-swatch', colourOf.get(r.car)] }), carName(r.car)) } : null,
      { key: 'period', label: 'Period', th: true, sort: true, cls: 'c-period', firstDir: 'desc', cover: ['statistics.col.period', 'statistics.link.trip'], value: (r) => r.period_start_ms, cell: (r) => periodCell(r, g) },
      { key: 'drives', label: 'Drives', sort: true, num: true, cls: 'c-drives', cover: ['statistics.col.drives_count', 'statistics.link.drives', 'statistics.col.time_driven'], hint: 'Drives that started in the period, and the time spent driving', value: (r) => r.drives,
        cell: (r) => h('span', null, r.drives ? h('a', { class: 'cell-main', href: linkFor('drives', r), title: 'These drives in the Drives list' }, formatNumber(r.drives)) : zeroText(r, 'none'),
          h('span', { class: 'cell-sub' }, r.drives ? formatDuration(r.time_driven_ms) : '')) },
      { key: 'distance', label: 'Distance', sort: true, num: true, cls: 'c-dist', cover: 'statistics.col.distance', hint: 'By odometer, so driving the log missed counts too; then the distance of the logged drives', value: (r) => r.distance_odometer_km,
        cell: (r) => h('span', null, h('span', { class: 'cell-main' }, q(r.distance_odometer_km, 'distance', { why: 'no odometer reading' })),
          h('span', { class: 'cell-sub' }, r.distance_logged_km != null && Math.abs((r.distance_odometer_km || 0) - r.distance_logged_km) > 0.5 ? `${format(r.distance_logged_km, 'distance')} logged` : r.drives ? 'all logged' : ''),
          h('span', { class: 'cell-sub st-fold-temp' }, r.drives && r.outside_avg_c != null ? [format(r.outside_avg_c, 'temperature'), ' outside'] : '')) },
      { key: 'temp', label: 'Outside', sort: true, num: true, cls: 'c-temp st-mid', cover: 'statistics.col.avg_temp', hint: 'Average outside temperature while driving, weighted by time', value: (r) => r.outside_avg_c,
        cell: (r) => (r.drives ? q(r.outside_avg_c, 'temperature', { why: 'no reading' }) : zeroText(r, '—')) },
      { key: 'eff', label: 'Efficiency', sort: true, num: true, cls: 'c-eff', cover: 'statistics.col.driving_efficiency', hint: 'The car’s rated consumption over the net consumption of the drives: 100 % drives exactly as rated', value: (r) => r.driving_efficiency_pct,
        cell: (r) => !r.drives ? zeroText(r, '—') : h('span', null, q(r.driving_efficiency_pct, 'percent', { why: 'too little distance' }), h('span', { class: 'cell-sub' }, r.driving_efficiency_pct != null ? 'of rated' : '')) },
      { key: 'net', label: 'Net', sort: true, num: true, cls: 'c-net', cover: 'statistics.col.consumption_net', hint: 'Energy of the drives over their distance', value: (r) => r.net_wh_per_km,
        cell: (r) => !r.drives ? zeroText(r, '—') : h('span', null, q(r.net_wh_per_km, 'efficiency', { why: 'too little distance' }), h('span', { class: 'cell-sub' }, r.energy_used_kwh ? [format(r.energy_used_kwh, 'energy'), ' driving'] : ''),
          h('span', { class: 'cell-sub st-fold-eff' }, r.driving_efficiency_pct != null ? `${format(r.driving_efficiency_pct, 'percent', { digits: 0 })} of rated` : '')) },
      { key: 'gross', label: 'Gross', sort: true, num: true, cls: 'c-gross', cover: 'statistics.col.consumption_gross', hint: 'Driving plus everything used while parked, over the distance', value: (r) => r.gross_wh_per_km,
        cell: (r) => !r.drives ? h('span', null, zeroText(r, '—'), h('span', { class: 'cell-sub' }, r.energy_standing_kwh ? ['+', format(r.energy_standing_kwh, 'energy'), ' parked'] : '')) : h('span', null, q(r.gross_wh_per_km, 'efficiency', { why: 'too little distance' }),
          h('span', { class: 'cell-sub' }, r.energy_standing_kwh ? ['+', format(r.energy_standing_kwh, 'energy'), ' parked'] : ''),
          h('span', { class: 'cell-sub st-fold st-fold-over' }, overheadOf(r) != null ? `${format(overheadOf(r), 'percent', { digits: 0 })} of it parked` : '')) },
      { key: 'over', label: 'Overhead', sort: true, num: true, cls: 'c-over st-wide', cover: 'statistics.col.consumption_overhead', hint: 'The share of all the energy (gross) that did not go to driving but was used while parked: 1 − net ÷ gross, never above 100 %', value: overheadOf,
        cell: (r) => !r.drives ? zeroText(r, '—') : h('span', null, q(overheadOf(r), 'percent', { digits: 0, why: 'too little distance' }), h('span', { class: 'cell-sub' }, overheadOf(r) != null ? 'of all energy, parked' : '')) },
      { key: 'charges', label: 'Charges', sort: true, num: true, cls: 'c-charges', cover: ['statistics.col.charges_count', 'statistics.link.charges'], hint: 'Charges that started in the period', value: (r) => r.charges,
        cell: (r) => h('span', null, r.charges ? h('a', { class: 'cell-main', href: linkFor('charges', r), title: 'These charges in the Charges list' }, formatNumber(r.charges)) : zeroText(r, 'none'),
          h('span', { class: 'cell-sub st-fold st-fold-each' }, r.energy_per_charge_kwh != null ? [format(r.energy_per_charge_kwh, 'energy'), ' each'] : '')) },
      { key: 'added', label: 'Added', sort: true, num: true, cls: 'c-added', cover: ['statistics.col.energy_used', 'statistics.link.charging_stats'], hint: 'Energy put into the pack by the charges, then the part metered from the grid', value: (r) => r.energy_added_kwh,
        cell: (r) => h('span', null, r.charges ? h('a', { class: 'cell-main', href: `${linkFor('charges', r)}#ch-costs-title`, title: 'The charging statistics of this period: costs and totals, then habits and places' }, q(r.energy_added_kwh, 'energy')) : zeroText(r, '—'),
          h('span', { class: 'cell-sub' }, r.energy_grid_kwh ? [format(r.energy_grid_kwh, 'energy'), ' from grid'] : r.charges ? 'grid not metered' : '')) },
      { key: 'each', label: 'Per charge', sort: true, num: true, cls: 'c-each st-wide', cover: 'statistics.col.avg_energy_per_charge', hint: 'Energy added over the number of charges', value: (r) => r.energy_per_charge_kwh,
        cell: (r) => (r.charges ? q(r.energy_per_charge_kwh, 'energy') : zeroText(r, '—')) },
      { key: 'cost', label: 'Cost', sort: true, num: true, cls: 'c-cost', cover: 'statistics.col.costs', hint: 'Total: priced charges plus estimates, per currency, never added across currencies', value: (r) => totalOf(r)[0]?.amount ?? null,
        cell: (r) => h('span', null, totalOf(r).length ? h('span', { class: 'cell-main' }, moneyText(totalOf(r))) : zeroText(r, r.charges ? 'no price' : '—'),
          h('span', { class: 'cell-sub' }, costSub(r)),
          h('span', { class: 'cell-sub st-fold st-fold-ppk' }, priceText(r.price_per_kwh)),
          h('span', { class: 'cell-sub st-fold st-fold-p100' }, per100Text(r.cost_per_100km))) },
      { key: 'ppk', label: 'Per kWh', sort: true, num: true, cls: 'c-ppk st-wide', cover: 'statistics.col.cost_per_kwh', hint: 'Cost over the energy it paid for (grid energy where metered, else pack energy)', value: (r) => (r.price_per_kwh || [])[0]?.value ?? null,
        cell: (r) => ((r.price_per_kwh || []).length ? priceText(r.price_per_kwh) : zeroText(r, '—')) },
      { key: 'p100', label: `Per 100 ${unitD}`, sort: true, num: true, cls: 'c-p100 st-wide', cover: 'statistics.col.cost_per_distance', hint: 'Priced cost of the charges over the distance of the logged drives', value: (r) => (r.cost_per_100km || [])[0]?.amount ?? null,
        cell: (r) => ((r.cost_per_100km || []).length ? per100Text(r.cost_per_100km) : zeroText(r, '—')) },
      { key: 'data', label: 'Data', sort: true, num: false, cls: 'c-data', cover: 'statistics.col.data_complete', hint: 'How much of the period was recorded, time not recorded, dropped stretches, drives or charges still open, imported rows', value: (r) => r.completeness && r.completeness.recorded_pct,
        cell: dataCell },
    ].filter(Boolean);

    const money = (key, f) => (r) => (r[key] || []).map((x) => `${x[f]} ${x.currency}`).join(' + ');
    const csvCols = [
      multi ? column('Car', (r) => carName(r.car)) : null,
      column('Period', 'label'), timeColumn('Starts', 'period_start_ms', tz), timeColumn('Ends', 'period_end_ms', tz), column('Time zone', (r) => zoneOf(r)),
      column('Part period', (r) => partOf(r, g) || ''),
      column('Drives', 'drives'), column('Time driven (min)', (r) => Math.round((r.time_driven_ms || 0) / 60000)),
      column('Distance by odometer', 'distance_odometer_km', 'distance'), column('Distance logged', 'distance_logged_km', 'distance'),
      column('Outside while driving', 'outside_avg_c', 'temperature'), column('Driving efficiency', 'driving_efficiency_pct', 'percent'),
      column('Energy driving', 'energy_used_kwh', 'energy'), column('Energy parked', 'energy_standing_kwh', 'energy'),
      column('Net consumption', 'net_wh_per_km', 'efficiency'), column('Gross consumption', 'gross_wh_per_km', 'efficiency'), column('Overhead (share of all energy used parked)', overheadOf, 'percent'),
      column('Charges', 'charges'), column('Energy added', 'energy_added_kwh', 'energy'), column('Energy from the grid', 'energy_grid_kwh', 'energy'),
      column('Energy per charge', 'energy_per_charge_kwh', 'energy'),
      column('Priced cost (amount and currency)', money('cost_by_currency', 'amount')), column('Price per kWh', money('price_per_kwh', 'value')), column(`Priced cost per 100 ${unitD}`, (r) => (r.cost_per_100km || []).map((x) => `${round2(convert(x.amount, 'money_per_distance'))} ${x.currency}`).join(' + ')),
      column('Charges priced', 'charges_priced'), column('Charges estimated', 'charges_estimated'),
      column('Estimated cost (amount and currency)', money('cost_estimated_by_currency', 'amount')), column('Total cost (amount and currency)', (r) => totalOf(r).map((x) => `${x.amount} ${x.currency}`).join(' + ')),
      column(`Total cost per 100 ${unitD} (amount and currency)`, (r) => totalPer100(r).map((x) => `${round2(convert(x.amount, 'money_per_distance'))} ${x.currency}`).join(' + ')),
      column('Total cost per day (amount and currency)', (r) => totalPerDay(r).map((x) => `${round2(x.amount)} ${x.currency}`).join(' + ')),
      column('Recorded', (r) => r.completeness && r.completeness.recorded_pct, 'percent'), column('Not recorded (min)', (r) => Math.round(((r.completeness && r.completeness.unknown_ms) || 0) / 60000)),
      column('Dropped stretches', (r) => r.completeness && r.completeness.dropped_stretches),
      column('Drive under way', (r) => (r.completeness && r.completeness.open_drive ? 'yes' : 'no')), column('Charge under way', (r) => (r.completeness && r.completeness.open_charge ? 'yes' : 'no')),
      column('Imported rows', (r) => r.completeness && r.completeness.teslamate_rows),
    ].filter(Boolean);

    const table = dataTable(ctx, {
      rows, columns, itemName: g === 'all' ? ['car', 'cars'] : [`${g === 'week' ? 'week' : g}`, `${g === 'week' ? 'weeks' : g + 's'}`],
      csv: { filename: csvFilename(scopeName, 'statistics', g, ctx.period && ctx.period.label), columns: csvCols },
      caption: multi ? `${tn(viewCars.length, '{n} car', '{n} cars')}, each in its own time zone` : `in ${zoneLabel(tz)}`,
      rowAttrs: (r) => ({ class: [{ 'is-zero': !r.drives && !r.charges }] }),
      empty: () => empty({ title: 'Nothing recorded in this period.', text: 'The cars have no data before their first log. Pick a later or longer period, or All time.' }),
      cover: ['statistics.table'], cls: 'st-ledger', label: 'Statistics per period',
    });
    replace(tableBox, table.el);
    table.load();
  }

  /* ---------------------------------------------------------------- odometer and mileage */

  function drawMileage(rows, whole, mileage, g, mileGroup) {
    const parts = [];
    // Odometer over time: one line per car (up to MAX_LINES), from each period's first and last reading.
    const lines = mileage.filter((m) => m.items && m.items.some((x) => x.odometer_end_km != null));
    const failed = mileage.filter((m) => m.error);
    const odoCsv = () => downloadCsv(csvFilename(scopeName, 'odometer', mileGroup, ctx.period && ctx.period.label),
      mileage.flatMap((m) => (m.items || []).map((x) => ({ ...x, car: m.car }))),
      [column('Car', (r) => carName(r.car)), column('Period', 'label'), timeColumn('Starts', 'period_start_ms', tz), timeColumn('Ends', 'period_end_ms', tz),
        column('Odometer at start', 'odometer_start_km', 'odometer'), column('Odometer at end', 'odometer_end_km', 'odometer'),
        column('Distance by odometer', 'distance_km', 'distance'), column('Logged', 'logged_km', 'distance'), column('Not logged', 'not_logged_km', 'distance')]);
    const odoSlot = h('div', { class: 'st-odo-slot' });
    const drawOdo = () => replace(odoSlot, lines.length ? odoChart(lines, odoScale) : failed.length ? errorView(failed[0].error, { retry: () => loadAll(), eyebrow: 'Could not load the odometer', title: 'The odometer could not be loaded.', home: false })
      : empty({ title: 'No odometer reading in this period.', text: 'The odometer is read while the car is awake. Pick a longer period.' }));
    const scaleChoice = lines.length > 1 ? choice({
      label: 'Scale', labelHidden: true, name: 'statistics-odo-scale', value: odoScale, cls: 'st-scale',
      options: [['from', 'From the period’s start'], ['odometer', 'Odometer']],
      onchange: (v) => { odoScale = v; ctx.setQuery({ scale: v === 'from' ? null : v }); drawOdo(); },
    }) : null;
    drawOdo();
    parts.push(h('div', { class: 'st-chart-part st-odo', cover: 'mi.chart.odometer' },
      h('div', { class: 'part-head' }, h('h3', { class: 'st-part-title' }, 'Odometer over time', h('span', { class: 'st-part-note' }, `${unitLabel('odometer')}, first and last reading of each ${mileGroup === 'week' ? 'week' : mileGroup}`)),
        lines.length ? csvButton('odometer', odoCsv) : null),
      scaleChoice ? h('div', { class: 'st-scale-row' }, scaleChoice, h('p', { class: 'st-scale-note' }, odoScale === 'from' ? 'Each line starts at zero so cars with very different odometers can be compared; the readout keeps the real odometer.' : 'The real odometer of each car.')) : null,
      odoSlot,
      viewCars.length > MAX_LINES ? h('p', { class: 'note st-more-note' }, `Lines for the first ${formatNumber(MAX_LINES)} of ${tn(viewCars.length, '{n} car', '{n} cars')}; pick fewer cars to see the others. Their distance is in the bars and the table below.`) : null));

    // Distance per period (or per car for the whole period): logged and not logged, summed over the cars.
    // One car over the whole period: its months (or days) from the mileage, rather than one lone bar.
    const one = g === 'all' && !multi && mileage[0] && mileage[0].items && mileage[0].items.length > 1;
    const bars = one ? barData(mileage[0].items.map((x) => ({ ...x, car: mileage[0].car, distance_odometer_km: x.distance_km, distance_logged_km: x.logged_km })), mileGroup) : barData(rows, g);
    const barUnit = one ? mileGroup : g;
    const barsCsv = () => downloadCsv(csvFilename(scopeName, 'distance', barUnit, ctx.period && ctx.period.label), bars.list,
      [column(barUnit === 'all' ? 'Car' : 'Period', 'name'), column('Distance by odometer', 'km', 'distance'), column('Logged', 'logged', 'distance'), column('Not logged', 'gap', 'distance')]);
    parts.push(h('div', { class: 'st-chart-part st-dist' },
      h('div', { class: 'part-head' }, h('h3', { class: 'st-part-title' }, barUnit === 'all' ? 'Distance per car' : `Distance per ${barUnit === 'week' ? 'week' : barUnit}`,
        h('span', { class: 'st-part-note' }, `${unitLabel('distance')}${multi && g !== 'all' ? ', the cars added up' : ''}`)),
        bars.list.length ? csvButton('distance', barsCsv) : null),
      bars.list.some((b) => b.km > 0) ? barChart(bars) : empty({ title: 'No distance in this period.', text: 'Nothing was driven. Pick another period.' }),
      h('ul', { class: 'st-key' },
        h('li', null, h('span', { class: 'st-key-swatch st-key-logged' }), 'Logged: drives the bridge recorded'),
        h('li', null, h('span', { class: 'st-key-swatch st-key-gap' }), 'Not logged: the odometer moved while nothing was recorded'))));

    // Each car: distance, logged and not logged, and a yearly projection from the period.
    const carRows = (whole || []).map((r) => {
      const days = (r.period_end_ms - r.period_start_ms) / DAY;
      const km = r.distance_odometer_km;
      return { car: r.car, km, logged: r.distance_logged_km, gap: km != null && r.distance_logged_km != null ? Math.max(0, km - r.distance_logged_km) : null, days,
        year: km != null && days >= 7 ? (km / days) * 365.25 : null, from: r.period_start_ms, to: r.period_end_ms };
    });
    const perCar = dataTable(ctx, {
      rows: carRows, itemName: ['car', 'cars'], sort: { key: 'km', dir: 'desc' }, label: 'Mileage per car',
      columns: [
        { key: 'car', label: 'Car', th: true, sort: true, value: (r) => carName(r.car), cell: (r) => h('a', { class: 'st-car', href: linkFor('trip', { car: r.car, period_start_ms: r.from, period_end_ms: r.to }), title: 'This car’s period as a trip' }, h('span', { class: ['st-swatch', colourOf.get(r.car)] }), carName(r.car)) },
        { key: 'km', label: 'Distance', sort: true, num: true, value: (r) => r.km, cell: (r) => h('span', null, q(r.km, 'distance', { why: 'no odometer reading' }), h('span', { class: 'cell-sub' }, 'by odometer')) },
        { key: 'logged', label: 'Logged', sort: true, num: true, value: (r) => r.logged, cell: (r) => q(r.logged, 'distance') },
        { key: 'gap', label: 'Not logged', sort: true, num: true, value: (r) => r.gap, cell: (r) => h('span', { class: { 'st-gap': r.gap > 0.5 } }, q(r.gap, 'distance', { why: 'no odometer reading' })) },
        { key: 'year', label: 'A year at this pace', sort: true, num: true, value: (r) => r.year,
          cell: (r) => h('span', null, q(r.year, 'distance', { why: 'needs a week of data' }), h('span', { class: 'cell-sub' }, r.year != null ? `from ${tn(Math.round(r.days), '{n} day', '{n} days')}` : '')) },
      ],
      csv: { filename: csvFilename(scopeName, 'mileage-per-car', ctx.period && ctx.period.label), columns: [
        column('Car', (r) => carName(r.car)), column('Distance by odometer', 'km', 'distance'), column('Logged', 'logged', 'distance'), column('Not logged', 'gap', 'distance'),
        column('Days', (r) => Math.round(r.days * 10) / 10), column('A year at this pace', 'year', 'distance')] },
      empty: () => empty({ title: 'No car has data in this period.', text: 'Pick a longer period or All time.' }),
    });
    parts.push(h('div', { class: 'st-chart-part st-percar', cover: 'mi.section.per_car' },
      h('div', { class: 'part-head' }, h('h3', { class: 'st-part-title' }, 'Each car', h('span', { class: 'st-part-note' }, 'distance in the period and where it is heading'))),
      whole ? perCar.el : errorView(new Error('The totals did not load.'), { retry: () => loadAll(), eyebrow: 'Could not load the cars’ totals', title: 'The cars’ totals could not be loaded.', home: false })));
    replace(mileBox, h('div', { class: 'st-mile-grid' }, parts));
    if (whole) perCar.load();
  }

  function barData(rows, g) {
    if (g === 'all') {
      const list = rows.map((r) => ({ name: carName(r.car), km: r.distance_odometer_km || 0, logged: Math.min(r.distance_logged_km || 0, r.distance_odometer_km || 0), car: r.car }));
      return { list: list.map((b) => ({ ...b, gap: Math.max(0, b.km - b.logged) })), byCar: true };
    }
    const by = new Map();
    for (const r of rows) {
      const b = by.get(r.label) || { key: r.label, name: periodName(r, g), short: shortName(r, g), km: 0, logged: 0, start: r.period_start_ms, part: partOf(r, g) };
      b.km += r.distance_odometer_km || 0;
      b.logged += Math.min(r.distance_logged_km || 0, r.distance_odometer_km || 0);
      by.set(r.label, b);
    }
    const list = [...by.values()].sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
    if (g === 'month' && list.length > 12) for (const b of list) b.short = dateFormat(tz, { month: 'short', year: '2-digit' }).format(b.start);
    return { list: list.map((b) => ({ ...b, gap: Math.max(0, b.km - b.logged) })), byCar: false };
  }
  function shortName(r, g) {
    const z = zoneOf(r);
    if (g === 'day') return formatDate(r.period_start_ms, z, { weekday: false, year: false });
    if (g === 'week') return `W${Number((/W(\d+)/.exec(r.label) || [])[1] || 0)}`;
    if (g === 'month') return dateFormat(z, { month: 'short' }).format(r.period_start_ms);
    return r.label;
  }

  /* ---------------------------------------------------------------- milestones */

  function drawStones(rows, mileage, longest, g) {
    const items = [];
    const disp = (km) => convert(km, 'odometer');
    // Odometer marks: every 10,000 in the person's unit, found in the period's own readings.
    const step = 10000;
    for (const m of mileage) {
      for (const x of m.items || []) {
        if (x.odometer_start_km == null || x.odometer_end_km == null) continue;
        const a = Math.floor(disp(x.odometer_start_km) / step), b = Math.floor(disp(x.odometer_end_km) / step);
        for (let k = a + 1; k <= b; k++) {
          items.push({ kind: 'Milestone', at: x.period_start_ms, title: `${multi ? `${carName(m.car)} passed` : 'Passed'} ${formatNumber(k * step)} ${unitLabel('odometer')}`,
            note: `on the odometer, in ${x.label && /^\d{4}-\d{2}$/.test(x.label) ? dateFormat(tz, { month: 'long', year: 'numeric' }).format(x.period_start_ms) : formatDateRange(x.period_start_ms, x.period_end_ms - 1, tz, { year: true })}`,
            href: state.buildUrl('trip', { car: m.car, period: { from: x.period_start_ms, to: x.period_end_ms } }), linkText: 'Open that stretch' });
        }
      }
    }
    // The longest drive of the period (of each car asked, then the longest of them).
    const best = [...longest].sort((a, b) => (b.distance_km || 0) - (a.distance_km || 0))[0];
    if (best && best.distance_km) {
      const from = (best.start && (best.start.label || (best.start.place && best.start.place.name))) || 'an unnamed spot';
      const to = (best.end && (best.end.label || (best.end.place && best.end.place.name))) || 'an unnamed spot';
      items.push({ kind: 'Record', title: `Longest drive: ${format(best.distance_km, 'distance')}`, at: best.start_ms,
        note: `${multi ? `${carName(best.car)}, ` : ''}${from} to ${to}, ${formatDate(best.start_ms, best.time_zone || tz, { now, year: true })}, ${formatDuration(best.duration_ms)}`,
        href: state.buildUrl('drive', { car: best.car, drive: best.id, period: ctx.state.period }), linkText: 'Open the drive' });
    }
    // Records among the periods: farthest, most efficient, most charging, cheapest per distance.
    if (g !== 'all' && rows.length > 1) {
      const pick = (list, f, lower = false) => list.filter((r) => f(r) != null).sort((a, b) => (lower ? f(a) - f(b) : f(b) - f(a)))[0];
      const who = (r) => (multi ? `${carName(r.car)}, ` : '');
      const far = pick(rows, (r) => (r.distance_odometer_km > 0 ? r.distance_odometer_km : null));
      if (far) items.push({ kind: 'Record', title: `Farthest ${g === 'week' ? 'week' : g}: ${format(far.distance_odometer_km, 'distance')}`, note: `${who(far)}${periodName(far, g)}, ${tn(far.drives, '{n} drive', '{n} drives')}`, href: linkFor('trip', far), linkText: 'Open as a trip', at: far.period_start_ms });
      const eff = pick(rows.filter((r) => (r.distance_logged_km || 0) >= 50), (r) => r.net_wh_per_km, true);
      if (eff) items.push({ kind: 'Record', title: `Most efficient ${g === 'week' ? 'week' : g}: ${format(eff.net_wh_per_km, 'efficiency')}`, note: `${who(eff)}${periodName(eff, g)}, net, over ${format(eff.distance_logged_km, 'distance')} (periods of at least ${format(50, 'distance', { digits: 0 })})`, href: linkFor('trip', eff), linkText: 'Open as a trip', at: eff.period_start_ms });
      const chg = pick(rows, (r) => (r.energy_added_kwh > 0 ? r.energy_added_kwh : null));
      if (chg) items.push({ kind: 'Record', title: `Most energy added: ${format(chg.energy_added_kwh, 'energy')}`, note: `${who(chg)}${periodName(chg, g)}, ${tn(chg.charges, '{n} charge', '{n} charges')}`, href: linkFor('charges', chg), linkText: 'See the charges', at: chg.period_start_ms });
      const curs = new Set(rows.flatMap((r) => (r.cost_per_100km || []).map((x) => x.currency)));
      if (curs.size === 1) {
        const cheap = pick(rows.filter((r) => (r.distance_logged_km || 0) >= 50), (r) => (r.cost_per_100km || [])[0]?.amount ?? null, true);
        if (cheap) items.push({ kind: 'Record', title: `Cheapest ${g === 'week' ? 'week' : g} to drive: ${per100Text(cheap.cost_per_100km)}`, note: `${who(cheap)}${periodName(cheap, g)}, charging cost over the distance driven`, href: linkFor('charges', cheap), linkText: 'See the charges', at: cheap.period_start_ms });
      }
    }
    items.sort((a, b) => (a.kind === b.kind ? (b.at || 0) - (a.at || 0) : a.kind === 'Milestone' ? -1 : 1));
    const stonesCsv = () => downloadCsv(csvFilename(scopeName, 'milestones', ctx.period && ctx.period.label), items,
      [column('Kind', 'kind'), column('What', 'title'), column('Detail', 'note'), timeColumn('When', 'at', tz)]);
    replace(stonesBox,
      items.length ? h('div', null,
        h('div', { class: 'st-stones-top' }, h('p', { class: 'note' }, `Odometer marks every ${formatNumber(step)} ${unitLabel('odometer')}${viewCars.length > MAX_LINES ? ` (first ${formatNumber(MAX_LINES)} cars)` : ''}, and the period’s records. Each one links to where it happened.`), csvButton('milestones', stonesCsv)),
        h('ol', { class: 'st-stone-list' }, items.map((it) => h('li', { class: ['st-stone', { 'is-mark': it.kind === 'Milestone' }] },
          h('span', { class: 'st-stone-kind' }, it.kind),
          h('p', { class: 'st-stone-title' }, it.title),
          h('p', { class: 'st-stone-note' }, it.note),
          it.href ? h('a', { class: 'link-arrow st-stone-link', href: it.href }, it.linkText, icon('arrowRight')) : null))))
        : empty({ title: 'No milestones in this period yet.', text: 'Milestones come from drives and charges: odometer marks every 10,000, the longest drive and the best periods. Pick a longer period.' }));
  }

  /* ---------------------------------------------------------------- charts (SVG in percentages) */

  function odoChart(lines, scale) {
    const rel = scale === 'from' && lines.length > 1;
    const baseOf = (m) => { const f = m.items.find((x) => x.odometer_start_km != null); return rel && f ? f.odometer_start_km : 0; };
    const pts = lines.map((m) => { const b = baseOf(m); return { car: m.car, points: m.items.flatMap((x) => (x.odometer_start_km != null && x.odometer_end_km != null
      ? [{ t: x.period_start_ms, v: disp(x.odometer_start_km - b), x }, { t: x.period_end_ms, v: disp(x.odometer_end_km - b), x }] : [null])) }; });
    const base = new Map(lines.map((m) => [m.car, baseOf(m)]));
    function disp(km) { return convert(km, 'odometer'); }
    const flat = pts.flatMap((p) => p.points.filter(Boolean));
    const t0 = Math.min(...flat.map((p) => p.t)), t1 = Math.max(...flat.map((p) => p.t));
    let v0 = Math.min(...flat.map((p) => p.v)), v1 = Math.max(...flat.map((p) => p.v));
    if (rel) v0 = 0;
    if (v1 - v0 < 10) { v0 = rel ? 0 : v0 - 5; v1 += rel ? 10 : 5; }
    const ticks = niceSplits(v0, v1, 4);
    const lo = Math.min(v0, ticks[0]), hi = Math.max(v1, ticks[ticks.length - 1]);
    const xOf = (t) => (t1 > t0 ? ((t - t0) / (t1 - t0)) * 100 : 50);
    const yOf = (v) => 100 - ((v - lo) / (hi - lo)) * 100;
    const segs = pts.flatMap((p) => p.points.slice(1).map((b, i) => {
      const a = p.points[i];
      return a && b ? svg('line', { class: ['st-odo-line', colourOf.get(p.car)], x1: p3(xOf(a.t)), y1: p3(yOf(a.v)), x2: p3(xOf(b.t)), y2: p3(yOf(b.v)) }) : null;
    }));
    const cross = svg('line', { class: 'st-cross', x1: '0%', x2: '0%', y1: '0%', y2: '100%', visibility: 'hidden' });
    const dots = svg('g');
    const plot = h('div', { class: 'st-plot' },
      svg('svg', { class: 'st-svg', width: '100%', height: '100%', role: 'img', 'aria-label': `${rel ? 'Distance since the start of the period' : 'Odometer'} over time for ${lines.map((m) => carName(m.car)).join(', ')}` },
        ticks.map((v) => svg('line', { class: 'st-grid', x1: '0%', x2: '100%', y1: p3(yOf(v)), y2: p3(yOf(v)) })), segs, cross, dots));
    const yAxis = h('div', { class: 'st-yaxis', 'aria-hidden': 'true' }, ticks.map((v) => h('span', { class: 'st-ytick', vars: { '--y': p3(100 - yOf(v)) } }, formatNumber(v))));
    const xAxis = h('div', { class: 'st-xaxis st-xaxis-time', 'aria-hidden': 'true' }, timeTicks(t0, t1).map((t) => h('span', { class: ['st-xtick', edgeTick(xOf(t.t))], vars: { '--x': p3(xOf(t.t)) } }, t.text)));
    const readout = h('p', { class: 'st-readout', 'aria-live': 'polite' });
    const show = (t) => {
      const parts = lines.map((m) => {
        const x = m.items.find((it) => it.period_start_ms <= t && t < it.period_end_ms) || (t >= t1 ? m.items[m.items.length - 1] : null);
        return x && x.odometer_end_km != null ? { car: m.car, x } : null;
      }).filter(Boolean);
      if (!parts.length) return;
      const x0 = parts[0].x;
      replace(readout, h('span', { class: 'st-readout-when' }, formatDateRange(x0.period_start_ms, x0.period_end_ms - 1, tz, { year: true })),
        parts.map((p) => h('span', { class: 'st-readout-item' }, h('span', { class: ['st-swatch', colourOf.get(p.car)] }), multi ? `${carName(p.car)} ` : 'at the end ', q(p.x.odometer_end_km, 'odometer'),
          p.x.distance_km != null ? h('span', { class: 'st-readout-sub' }, ` +${format(p.x.distance_km, 'distance')}`) : null)));
      return parts;
    };
    plot.addEventListener('pointermove', (e) => {
      const box = plot.getBoundingClientRect();
      const f = Math.max(0, Math.min(1, (e.clientX - box.left) / Math.max(1, box.width)));
      const t = t0 + f * (t1 - t0);
      const parts = show(t);
      cross.setAttribute('x1', p3(f * 100)); cross.setAttribute('x2', p3(f * 100)); cross.setAttribute('visibility', 'visible');
      replace(dots, (parts || []).map((p) => svg('circle', { class: ['st-dot', colourOf.get(p.car)], cx: p3(xOf(p.x.period_end_ms)), cy: p3(yOf(disp(p.x.odometer_end_km - base.get(p.car)))), r: '0.25rem' })));
    });
    plot.addEventListener('pointerleave', () => { cross.setAttribute('visibility', 'hidden'); replace(dots); show(t1); });
    show(t1);
    return h('div', { class: 'st-chart' },
      h('div', { class: 'st-chart-grid' }, yAxis, plot, h('span'), xAxis), readout,
      multi ? h('ul', { class: 'st-key' }, lines.map((m) => h('li', null, h('span', { class: ['st-swatch', colourOf.get(m.car)] }), carName(m.car)))) : null);
  }

  function timeTicks(t0, t1) {
    const span = t1 - t0;
    if (!(span > 0)) return [{ t: t0, text: formatDate(t0, tz, { weekday: false, year: true }) }];
    const n = 6; // seven dates; a narrow axis labels every other one, which still spaces them evenly (statistics.css)
    const out = [];
    const long = span > 300 * DAY;
    for (let i = 0; i <= n; i++) {
      const t = t0 + (span * i) / n;
      out.push({ t, text: long ? dateFormat(tz, { month: 'short', year: 'numeric' }).format(t) : span > 3 * DAY ? formatDate(t, tz, { weekday: false, year: false }) : formatDate(t, tz, { weekday: true, year: false }) });
    }
    return out;
  }

  function barChart({ list, byCar }) {
    const top = Math.max(...list.map((b) => convert(b.km, 'distance')), 1);
    const ticks = niceSplits(0, top, 4);
    const hi = Math.max(top, ticks[ticks.length - 1]);
    const yOf = (km) => 100 - (convert(km, 'distance') / hi) * 100;
    const n = list.length;
    const slot = 100 / n, w = Math.min(slot * (n > 60 ? 0.86 : 0.7), 14);
    const sel = svg('rect', { class: 'st-bar-hover', x: '0%', y: '0%', width: p3(slot), height: '100%', visibility: 'hidden' });
    const plot = h('div', { class: 'st-plot' },
      svg('svg', { class: 'st-svg', width: '100%', height: '100%', role: 'img', 'aria-label': `Distance ${byCar ? 'per car' : 'per period'}, logged and not logged` },
        sel,
        ticks.map((v) => svg('line', { class: 'st-grid', x1: '0%', x2: '100%', y1: p3(100 - (v / hi) * 100), y2: p3(100 - (v / hi) * 100) })),
        list.map((b, i) => {
          const x = i * slot + (slot - w) / 2;
          const yl = yOf(b.logged), yt = yOf(b.km);
          return svg('g', null,
            svg('rect', { class: ['st-bar-logged', byCar ? colourOf.get(b.car) : null, { 'is-part': b.part }], x: p3(x), y: p3(yl), width: p3(w), height: p3(100 - yl) }),
            b.gap > 0.05 ? svg('rect', { class: 'st-bar-gap', x: p3(x), y: p3(yt), width: p3(w), height: p3(Math.max(0, yl - yt)) }) : null);
        })));
    const yAxis = h('div', { class: 'st-yaxis', 'aria-hidden': 'true' }, ticks.map((v) => h('span', { class: 'st-ytick', vars: { '--y': p3((v / hi) * 100) } }, formatNumber(v))));
    const every = Math.max(1, Math.ceil(n / (byCar ? 12 : 8)));
    const xAxis = h('div', { class: 'st-xaxis', 'aria-hidden': 'true' }, list.map((b, i) => (i % every === 0 ? h('span', { class: ['st-xtick', edgeTick((i + 0.5) * slot)], vars: { '--x': p3((i + 0.5) * slot) } }, b.short || b.name) : null)));
    const readout = h('p', { class: 'st-readout', 'aria-live': 'polite' });
    const show = (i) => {
      const b = list[i];
      if (!b) return;
      replace(readout, h('span', { class: 'st-readout-when' }, b.name, b.part ? ` (${b.part})` : ''), h('span', { class: 'st-readout-item' }, q(b.km, 'distance'), ' by odometer'),
        h('span', { class: 'st-readout-item' }, h('span', { class: 'st-key-swatch st-key-logged' }), q(b.logged, 'distance'), ' logged'),
        h('span', { class: 'st-readout-item' }, h('span', { class: 'st-key-swatch st-key-gap' }), q(b.gap, 'distance'), ' not logged'));
    };
    plot.addEventListener('pointermove', (e) => {
      const box = plot.getBoundingClientRect();
      const i = Math.max(0, Math.min(n - 1, Math.floor(((e.clientX - box.left) / Math.max(1, box.width)) * n)));
      sel.setAttribute('x', p3(i * slot)); sel.setAttribute('visibility', 'visible');
      show(i);
    });
    plot.addEventListener('pointerleave', () => { sel.setAttribute('visibility', 'hidden'); show(n - 1); });
    show(n - 1);
    return h('div', { class: 'st-chart' }, h('div', { class: 'st-chart-grid' }, yAxis, plot, h('span'), xAxis), readout);
  }

  await loadAll();
  ctx.ready();
  return {};
}

/** The share recorded: 'over 99.9 %' when a little is missing, so a rounded 100 % never hides a gap. */
function recorded(pct, unknownMs, why) {
  if (pct != null && pct >= 99.95 && unknownMs) return h('span', { class: 'q' }, h('span', { class: 'q-v' }, 'over 99.9'), h('span', { class: 'q-u' }, '%'));
  return q(pct, 'percent', { digits: pct != null && pct >= 99.95 ? 0 : 1, why });
}
const round2 = (v) => (v == null ? null : Math.round(v * 100) / 100);

/** The car pick list: tick one car, several, or none for every car; Apply opens that view. */
function carPicker(ctx, { fleet, chosen, onapply }) {
  let sel = new Set(chosen);
  let soldOpen = false;
  const trigger = h('button', { type: 'button', class: 'field-select st-pick-trigger', id: 'statistics-cars', disabled: fleet.length < 2 });
  const search = h('input', { class: 'field-input st-pick-search', type: 'search', placeholder: 'Find a car', 'aria-label': 'Find a car', autocomplete: 'off' });
  const list = h('ul', { class: 'st-pick-list' });
  const more = h('p', { class: 'st-pick-more' });
  const name = (cid) => (fleet.find((c) => c.id === cid) || {}).name || cid;
  const text = () => {
    const n = [...sel];
    trigger.textContent = fleet.length < 2 ? (fleet[0] ? fleet[0].name : 'No car') : !n.length ? 'Every car' : n.length === 1 ? name(n[0]) : `${name(n[0])} and ${formatNumber(n.length - 1)} more`;
  };
  function draw() {
    const s = search.value.trim().toLowerCase();
    const found = fleet.filter((c) => !s || String(c.name || '').toLowerCase().includes(s));
    found.sort((a, b) => (sel.has(b.id) - sel.has(a.id)) || String(a.name).localeCompare(String(b.name)));
    // Sold cars wait behind their own toggle unless ticked or searched for.
    const { rows, cut } = soldPickRows(found, {
      chosen: (c) => sel.has(c.id), searching: !!s, open: soldOpen, ontoggle: () => { soldOpen = !soldOpen; draw(); }, max: 60,
      row: (c) => {
        const box = h('input', { type: 'checkbox', id: `st-car-${c.id}`, checked: sel.has(c.id) });
        box.addEventListener('change', () => { if (box.checked) sel.add(c.id); else sel.delete(c.id); text(); });
        return h('li', null, h('label', { class: 'st-pick-item', for: `st-car-${c.id}` }, box, h('span', { class: 'st-pick-name' }, c.name || c.id), h('span', { class: 'st-pick-sub' }, [[c.model, c.year].filter(Boolean).join(' '), state.isSold(c) ? 'Sold' : null].filter(Boolean).join(' · '))));
      },
    });
    replace(list, rows);
    more.textContent = !found.length ? 'No car has that name.' : cut ? `${formatNumber(cut)} more; type to narrow the list.` : '';
  }
  search.addEventListener('input', draw);
  const panel = h('div', { class: 'pop st-pick-pop', id: 'statistics-cars-menu', role: 'group', 'aria-label': 'Cars', tabindex: '-1' },
    fleet.length > 6 ? search : null, list, more,
    h('div', { class: 'st-pick-foot' },
      button('Every car', { cls: 'button-small', onclick: () => { sel.clear(); pop.close({ refocus: true }); onapply([]); } }),
      button('Show these cars', { kind: 'primary', cls: 'button-small', onclick: () => { pop.close({ refocus: true }); onapply([...sel]); } })));
  const wrap = h('div', { class: 'pop-wrap st-pick-wrap' }, trigger, panel);
  const pop = popover(trigger, panel, { onOpen: draw, focus: '.st-pick-search' });
  text();
  return wrap;
}
