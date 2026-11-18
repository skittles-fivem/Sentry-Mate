// Statistics (/cars/{car}/statistics, or /statistics?cars=all|ids for several cars): totals per day,
// ISO week, month or year (or the whole period as one row), and the odometer over time.
// One grouping control, the shared period and a car picker drive every section:
// - the period totals (worked out from the rows below, money per currency);
// - the period table (GET .../statistics, every row, sorted here, CSV built from the rows);
// - odometer and mileage (GET /cars/{car}/mileage per car): the odometer per period, distance per
//   period split into logged and not logged, and a yearly projection per car;
// - milestones: odometer marks passed and records, from the same rows plus each car's longest drive.
// Periods follow each car's own time zone (the API groups them); rows of several cars sit in one table
// with a car column instead of the page repeated per car. Live: a drive or charge that starts or ends
// ("rows" events) loads everything again.

import * as state from '../shared/state.js';
import { h, replace, setVars, cssVar } from '../shared/dom.js';
import { downloadCsv, csvFilename, column } from '../shared/csv.js';
import { format, formatMoney, convert, unitLabel, getUnits } from '../shared/units.js';
import { formatDate, formatDateRange, formatDuration, dateFormat, startOf, addPeriod, DAY } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon } from '../core/icons.js';
import { pageHead, panel, metric, csvButton } from '../core/page.js';
import { dataTable } from '../core/table.js';
import { emptyState, skel } from '../core/states.js';
import { loadUPlot, mountPlot, valueAxis, colors, hexA, chartFont } from '../core/chart.js';
import { openPeriodPicker } from '../core/period.js';
import { placePopover } from '../core/popover.js';
import { rpx } from '../core/display.js';

export const id = 'statistics';
export const css = true;

const enc = encodeURIComponent;
const GROUPS = [
  ['day', 'Day', 'One row per day, midnight to midnight in the car’s time zone'],
  ['week', 'Week', 'One row per ISO week, Monday to Sunday in the car’s time zone'],
  ['month', 'Month', 'One row per calendar month in the car’s time zone'],
  ['year', 'Year', 'One row per calendar year in the car’s time zone'],
  ['all', 'Whole period', 'The whole period as one row per car'],
];
const GROUP_NAME = { day: 'day', week: 'week', month: 'month', year: 'year', all: 'whole period' };
// line colours for cars, in order (the legend names each car)
const CAR_COLORS = ['--live', '--c-charge', '--c-power', '--c-soc', '--c-regen', '--warm', '--c-elev'];
const iso = (ms) => (ms == null ? '' : new Date(ms).toISOString());
const sum = (xs) => xs.reduce((a, b) => a + (b ?? 0), 0);
const known = (xs) => xs.filter((x) => x != null);

// several currencies, one per line (never added together)
const stack = (items) => (items.length ? h('span', { class: 'st-money' }, items.map(([t, cur]) => h('span', null, t, items.length > 1 && cur ? h('small', { class: 'st-cur' }, ` ${cur}`) : null))) : null);
const moneyStack = (list) => stack((list || []).filter((x) => x.amount != null).map((x) => [formatMoney(x.amount, x.currency), x.currency]));
const priceStack = (list) => stack((list || []).filter((x) => x.value != null).map((x) => [format({ amount: x.value, currency: x.currency }, 'price_energy'), x.currency]));
const perDistStack = (list) => stack((list || []).filter((x) => x.amount != null).map((x) => [format({ amount: x.amount, currency: x.currency }, 'money_per_distance'), x.currency]));
const moneyCsv = (list) => (list || []).map((x) => `${x.amount ?? x.value} ${x.currency}`).join('; ');

// the grouping to use when the address names none: about 30 to 60 rows
function defaultGroup(period) {
  if (period?.from_ms == null) return 'month';
  const days = (period.to_ms - period.from_ms) / DAY;
  if (days <= 45) return 'day';
  if (days <= 190) return 'week';
  if (days <= 365 * 4) return 'month';
  return 'year';
}

// does a row cover only part of its day, week, month or year (cut by the period's edges or by now)?
function partOf(row, group, tz) {
  if (group === 'all') return null;
  const s = startOf(row.period_start_ms, group, tz);
  const e = addPeriod(s, group, 1, tz);
  const cutStart = s !== row.period_start_ms, cutEnd = e !== row.period_end_ms;
  if (!cutStart && !cutEnd) return null;
  return cutStart && cutEnd ? 'part' : cutStart ? 'from' : 'to';
}

export async function mount(ctx) {
  const { tz, period } = ctx;
  const fixedCar = ctx.car;
  const q = ctx.query;
  let group = GROUPS.some(([g]) => g === q.group) ? q.group : defaultGroup(period);

  /* ---------- which cars */
  let fleet = [];
  try { fleet = (await ctx.api.page('cars', { sort: 'name' }, { limit: 200 })).items || []; } catch { fleet = fixedCar ? [fixedCar] : []; }
  if (!ctx.alive()) return;
  const carById = new Map(fleet.map((c) => [c.id, c]));
  if (fixedCar) carById.set(fixedCar.id, fixedCar);
  const chosen = fixedCar ? [fixedCar.id] : (!q.cars || q.cars === 'all' ? 'all' : q.cars.split(',').filter(Boolean));
  const viewCars = chosen === 'all' ? fleet.map((c) => c.id) : chosen.filter((c) => carById.has(c));
  const multi = viewCars.length > 1 || !fixedCar;
  const carsParam = fixedCar ? null : (chosen === 'all' ? 'all' : viewCars.join(','));
  const carName = (cid) => carById.get(cid)?.name || cid;
  const carTz = (cid) => carById.get(cid)?.time_zone || tz;
  const colorOf = new Map(viewCars.map((cid, i) => [cid, CAR_COLORS[i % CAR_COLORS.length]]));
  const scopeName = fixedCar ? fixedCar.name : chosen === 'all' ? (fleet.length === 1 ? fleet[0].name : `All ${formatNumber(fleet.length)} cars`)
    : viewCars.length === 1 ? carName(viewCars[0]) : `${formatNumber(viewCars.length)} cars`;
  const statsPath = fixedCar ? `cars/${enc(fixedCar.id)}/statistics` : 'statistics';
  const periodOf = (r) => ({ from: r.period_start_ms, to: r.period_end_ms });
  const linkFor = (page, r) => state.buildUrl(page, { car: r.car, period: periodOf(r) });

  const root = h('div', { class: 'stats-page', cover: ['shots.statistics', 'shots.mileage'] });
  replace(ctx.view, root);

  /* ---------- the car picker (a popover of checkboxes) */
  const pops = [];
  ctx.onCleanup(() => { for (const p of pops) { if (p.matches(':popover-open')) p.hidePopover(); p.remove(); } });
  const goCars = (ids) => {
    const p0 = state.current().period;
    const all = !ids.length || (fleet.length > 1 && ids.length === fleet.length);
    ctx.navigate(ids.length === 1 ? state.buildUrl('statistics', { car: ids[0], period: p0, query: { group } })
      : state.buildUrl('statistics', { period: p0, query: { group, cars: all ? 'all' : ids.join(',') } }));
  };
  function carPicker() {
    const sel = new Set(chosen === 'all' ? [] : viewCars);
    const text = h('span', { class: 'st-pick-text' });
    const btn = h('button', { class: 'btn st-pick-btn', type: 'button', 'aria-haspopup': 'dialog', 'aria-expanded': 'false', title: 'Which cars this page covers',
      cover: ['statistics.filter.car', 'mi.section.per_car'] }, icon('car', 'ico--sm'), text, icon('chev-d', 'ico--sm'));
    const find = h('input', { class: 'input', type: 'search', placeholder: 'Find a car', 'aria-label': 'Find a car' });
    const list = h('div', { class: 'st-pick-list', role: 'group', 'aria-label': 'Cars' });
    const pop = h('div', { class: 'popover st-pick-pop', popover: 'auto', role: 'dialog', 'aria-label': 'Cars' },
      h('p', { class: 'st-pick-hint' }, 'Tick one car, several, or none for every car. Every total, row, chart and milestone then covers the cars you picked, in one view, with a car column.'),
      fleet.length > 8 ? find : null, list,
      h('div', { class: 'st-pick-actions' },
        h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: () => { sel.clear(); paint(); } }, 'Every car'),
        h('button', { class: 'btn btn--sm btn--primary', type: 'button', onclick: () => { pop.hidePopover(); goCars([...sel]); } }, 'Show')));
    const paintText = () => {
      const names = fleet.filter((c) => sel.has(c.id)).map((c) => c.name);
      text.textContent = names.length ? (names.length > 2 ? `${formatNumber(names.length)} cars` : names.join(', ')) : fleet.length === 1 ? fleet[0].name : 'Every car';
      btn.classList.toggle('is-on', names.length > 0);
    };
    const paint = () => {
      const needle = find.value.trim().toLowerCase();
      const shown = fleet.filter((c) => !needle || c.name.toLowerCase().includes(needle));
      replace(list, shown.length ? shown.map((c) => h('label', { class: 'check' },
        h('input', { type: 'checkbox', checked: sel.has(c.id), onchange: (e) => { if (e.target.checked) sel.add(c.id); else sel.delete(c.id); paintText(); } }),
        h('span', null, c.name), h('span', { class: 'faint st-pick-sub' }, [c.model, c.year].filter(Boolean).join(' · '))))
        : h('p', { class: 'note' }, fleet.length ? 'No car is called that.' : 'There is no car yet.'));
      paintText();
    };
    find.addEventListener('input', paint);
    btn.addEventListener('click', () => {
      if (pop.matches(':popover-open')) { pop.hidePopover(); return; }
      pop.showPopover(); placePopover(pop, btn, { align: 'start' });
    });
    pop.addEventListener('toggle', (e) => btn.setAttribute('aria-expanded', String(e.newState === 'open')));
    paint();
    pops.push(pop);
    return btn;
  }

  /* ---------- head and filters */
  const subGroup = h('span');
  const head = pageHead({
    eyebrow: scopeName, title: 'Statistics',
    sub: [period.label, subGroup, multi ? 'each car in its own time zone' : `days in ${tz.replace(/_/g, ' ')}`],
  });
  const groupSeg = h('span', { class: 'seg', role: 'group', 'aria-label': 'One row per', cover: 'statistics.control.time_range' },
    GROUPS.map(([g, label, help]) => h('button', {
      class: 'seg-btn', type: 'button', 'aria-pressed': String(group === g), title: help, dataset: { group: g },
      onclick: () => { if (group === g) return; group = g; for (const b of groupSeg.children) b.setAttribute('aria-pressed', String(b.dataset.group === g)); ctx.setQuery({ group: g }); load(); },
    }, label)));
  const periodBtn = h('button', { class: 'btn', type: 'button', onclick: (e) => openPeriodPicker(e.currentTarget), cover: ['statistics.filter.period', 'mi.filter.time_range'],
    title: 'The period of the whole page (the same picker as in the top bar). Drives and charges belong to it by their start.' },
  icon('calendar', 'ico--sm'), period.label, icon('chev-d', 'ico--sm'));
  const filters = h('section', { class: 'panel panel-pad st-filters', 'aria-label': 'What the page covers' },
    h('div', { class: 'filter-bar' },
      periodBtn, carPicker(),
      h('span', { class: 'st-filter' }, h('span', { class: 'faint' }, 'One row per'), groupSeg)),
    h('p', { class: 'note st-precision', cover: 'statistics.filter.high_precision' }, icon('info', 'ico--sm'),
      'There is no precision switch: net and gross consumption come from the pack’s own energy counters, so they are exact for a day as much as for a year. Rows at the edges of the period can be part periods; they say so.'));

  const totalsSlot = h('div');
  const tableSlot = h('div');
  const mileSlot = h('div');
  const milestonesSlot = h('div');
  root.append(head, filters, totalsSlot, tableSlot, mileSlot, milestonesSlot);

  /* ---------- the period table */
  const pctText = (v, d = 1) => format(v, 'percent', { digits: d });
  const periodCell = (r) => {
    const t = r.time_zone || carTz(r.car);
    const part = partOf(r, group, t);
    let main, sub;
    if (group === 'day') { main = formatDate(r.period_start_ms, t, { year: true }); sub = null; }
    else if (group === 'week') {
      const m = /^(\d{4})-W(\d{2})$/.exec(r.label || '');
      main = m ? `Week ${Number(m[2])}, ${m[1]}` : r.label;
      sub = formatDateRange(r.period_start_ms, r.period_end_ms - 1, t, { year: false });
    } else if (group === 'month') { main = dateFormat(t, { month: 'long', year: 'numeric' }).format(r.period_start_ms); sub = null; }
    else if (group === 'year') { main = r.label; sub = null; }
    else { main = 'Whole period'; sub = formatDateRange(r.period_start_ms, r.period_end_ms - 1, t); }
    if (part) {
      const edge = part === 'to' ? `to ${formatDate(r.period_end_ms - 1, t, { weekday: false, year: false })}`
        : part === 'from' ? `from ${formatDate(r.period_start_ms, t, { weekday: false, year: false })}`
          : formatDateRange(r.period_start_ms, r.period_end_ms - 1, t, { year: false });
      sub = `part ${GROUP_NAME[group]}: ${edge}`;
    }
    return h('span', { class: 'cell-text' },
      h('a', { class: 'main', href: linkFor('trip', r), cover: 'statistics.link.trip', title: 'Open this period as a trip: route, figures, drives and charges' }, main),
      sub ? h('span', { class: ['sub', part && 'st-part'] }, sub) : null);
  };
  const countLink = (n, page, r, what, cv) => (n
    ? h('a', { class: 'st-count', href: linkFor(page, r), cover: cv, title: `The ${what} of this period` }, formatNumber(n))
    : h('span', { class: 'faint', cover: cv }, '0'));
  const twoLine = (main, sub, title = null) => h('span', { class: 'cell-text cell-num', title },
    h('span', { class: 'main' }, main), sub ? h('span', { class: 'sub' }, sub) : null);
  const completeCell = (r) => {
    const c = r.completeness || {};
    const bits = [];
    if (c.unknown_ms) bits.push(`${formatDuration(c.unknown_ms)} unknown`);
    if (c.dropped_stretches) bits.push(`${formatNumber(c.dropped_stretches)} dropped ${c.dropped_stretches === 1 ? 'stretch' : 'stretches'}`);
    if (c.open_drive) bits.push('a drive is open');
    if (c.open_charge) bits.push('a charge is open');
    if (c.teslamate_rows) bits.push(`${formatNumber(c.teslamate_rows)} imported`);
    const bar = h('span', { class: 'st-rec-bar', 'aria-hidden': 'true' }, h('i'));
    setVars(bar, { '--w': `${Math.max(0, Math.min(100, c.recorded_pct ?? 0)).toFixed(2)}%` });
    const good = (c.recorded_pct ?? 0) >= 99.5 && !c.dropped_stretches && !c.open_drive && !c.open_charge;
    return h('span', { class: ['st-rec', good ? 'is-good' : 'is-warn'], title: 'Recorded share: the part of the period the CAN logs cover. Unknown time is when nothing was logged (asleep time counts as recorded: a quiet car is known to be asleep).' },
      h('span', { class: 'st-rec-top' }, bar, h('b', null, c.recorded_pct == null ? '—' : pctText(c.recorded_pct, c.recorded_pct >= 100 ? 0 : c.recorded_pct >= 99 ? 2 : 1))),
      h('span', { class: 'sub' }, bits.length ? bits.join(' · ') : 'nothing missing'));
  };
  const columns = [
    multi ? { key: 'car', label: 'Car', sort: true, value: (r) => carName(r.car), render: (r) => h('span', { class: 'st-car nowrap' }, h('i', { class: 'st-swatch', vars: { '--c': `var(${colorOf.get(r.car) || '--live'})` } }), carName(r.car)), csv: (r) => carName(r.car) } : null,
    { key: 'period', label: 'Period', sort: true, firstDir: 'desc', value: (r) => r.period_start_ms, cover: ['statistics.col.period', 'statistics.link.trip'], title: 'Open a period as a trip', render: periodCell, csv: (r) => r.label },
    { key: 'from', label: 'From', csv: (r) => iso(r.period_start_ms), render: null, cls: 'st-hide' },
    { key: 'time', label: 'Driving', num: true, sort: true, value: (r) => r.time_driven_ms, cover: 'statistics.col.time_driven', title: 'Time spent driving (drives that started in the period)',
      render: (r) => (r.time_driven_ms ? formatDuration(r.time_driven_ms) : h('span', { class: 'faint' }, '—')), csv: (r) => (r.time_driven_ms == null ? null : Math.round(r.time_driven_ms / 60000)) },
    { key: 'drives', label: 'Drives', num: true, sort: true, value: (r) => r.drives, cover: ['statistics.col.drives_count', 'statistics.link.drives'], title: 'Drives that started in the period; opens them on Drives',
      render: (r) => countLink(r.drives, 'drives', r, 'drives', 'statistics.link.drives'), csv: (r) => r.drives },
    { key: 'distance', label: 'Distance', num: true, sort: true, value: (r) => r.distance_odometer_km ?? r.distance_logged_km, cover: 'statistics.col.distance', title: 'By odometer, then the distance of the logged drives',
      render: (r) => twoLine(format(r.distance_odometer_km, 'distance'), r.distance_logged_km != null ? `logged ${format(r.distance_logged_km, 'distance')}` : null),
      csv: column('Distance by odometer', 'distance_odometer_km', 'distance') },
    { key: 'logged', label: 'Logged', csv: column('Distance logged', 'distance_logged_km', 'distance'), cls: 'st-hide', render: null },
    { key: 'temp', label: 'Outside', num: true, sort: true, value: (r) => r.outside_avg_c, cover: 'statistics.col.avg_temp', title: 'Average outside temperature while driving, weighted by driving time',
      render: (r) => twoLine(format(r.outside_avg_c, 'temperature'), r.outside_avg_c != null ? 'while driving' : 'no drive'), csv: column('Outside while driving', 'outside_avg_c', 'temperature') },
    { key: 'eff', label: 'Efficiency', num: true, sort: true, value: (r) => r.driving_efficiency_pct, cover: 'statistics.col.driving_efficiency', title: 'The car’s rated consumption over the consumption measured while driving: above 100 % is better than rated',
      render: (r) => (r.driving_efficiency_pct != null ? h('span', { class: ['st-eff', r.driving_efficiency_pct >= 100 && 'is-good'] }, pctText(r.driving_efficiency_pct, 0)) : h('span', { class: 'faint' }, '—')), csv: (r) => r.driving_efficiency_pct },
    { key: 'used', label: 'Energy used', num: true, sort: true, value: (r) => r.energy_used_kwh, cover: 'statistics.col.energy_used', title: 'Drawn from the pack while driving, then while parked (climate, Sentry, the car awake)',
      render: (r) => twoLine(format(r.energy_used_kwh, 'energy'), r.energy_standing_kwh != null ? `parked ${format(r.energy_standing_kwh, 'energy')}` : null),
      csv: column('Energy used driving', 'energy_used_kwh', 'energy') },
    { key: 'standing', label: 'Energy parked', csv: column('Energy used parked', 'energy_standing_kwh', 'energy'), cls: 'st-hide', render: null },
    { key: 'added', label: 'Energy added', num: true, sort: true, value: (r) => r.energy_added_kwh, cover: 'statistics.link.charging_stats', title: 'Into the pack by charging, then what AC charging took from the grid; opens the charging habits of the period',
      render: (r) => h('span', { class: 'cell-text cell-num' },
        r.charges ? h('a', { class: 'main', href: linkFor('charges', r), cover: 'statistics.link.charging_stats', title: 'Charging habits and costs of this period' }, format(r.energy_added_kwh, 'energy')) : h('span', { class: 'main faint' }, format(r.energy_added_kwh, 'energy')),
        r.energy_grid_kwh ? h('span', { class: 'sub' }, `grid ${format(r.energy_grid_kwh, 'energy')}`) : null),
      csv: column('Energy added', 'energy_added_kwh', 'energy') },
    { key: 'grid', label: 'Energy from grid', csv: column('Energy from grid (AC)', 'energy_grid_kwh', 'energy'), cls: 'st-hide', render: null },
    { key: 'charges', label: 'Charges', num: true, sort: true, value: (r) => r.charges, cover: ['statistics.col.charges_count', 'statistics.link.charges'], title: 'Charges that started in the period; opens them on Charges',
      render: (r) => countLink(r.charges, 'charges', r, 'charges', 'statistics.link.charges'), csv: (r) => r.charges },
    { key: 'per_charge', label: 'Per charge', num: true, sort: true, value: (r) => r.energy_per_charge_kwh, cover: 'statistics.col.avg_energy_per_charge', title: 'Energy added per charge, on average',
      render: (r) => format(r.energy_per_charge_kwh, 'energy'), csv: column('Energy per charge', 'energy_per_charge_kwh', 'energy') },
    { key: 'cost', label: 'Cost', num: true, sort: true, value: (r) => r.cost_by_currency?.[0]?.amount ?? null, cover: 'statistics.col.costs', title: 'Charging costs, summed per currency (never added across currencies)',
      render: (r) => moneyStack(r.cost_by_currency) || h('span', { class: 'faint' }, r.charges ? 'no price' : '—'), csv: (r) => moneyCsv(r.cost_by_currency) },
    { key: 'ppk', label: 'Per kWh', num: true, cover: 'statistics.col.cost_per_kwh', title: 'Cost over the energy it paid for: grid energy for AC, energy added for DC',
      render: (r) => priceStack(r.price_per_kwh) || h('span', { class: 'faint' }, '—'), csv: (r) => moneyCsv(r.price_per_kwh) },
    { key: 'p100', label: `Per 100 ${unitLabel('distance')}`, num: true, cover: 'statistics.col.cost_per_distance', title: 'Charging cost over the logged distance',
      render: (r) => perDistStack(r.cost_per_100km) || h('span', { class: 'faint' }, '—'), csv: (r) => (r.cost_per_100km || []).map((x) => `${format({ amount: x.amount, currency: x.currency }, 'money_per_distance')}`).join('; ') },
    { key: 'net', label: 'Net', num: true, sort: true, value: (r) => r.net_wh_per_km, cover: 'statistics.col.consumption_net', title: 'Net consumption: energy used while driving over the logged distance',
      render: (r) => format(r.net_wh_per_km, 'efficiency'), csv: column('Net consumption', 'net_wh_per_km', 'efficiency') },
    { key: 'gross', label: 'Gross', num: true, sort: true, value: (r) => r.gross_wh_per_km, cover: 'statistics.col.consumption_gross', title: 'Gross consumption: driving plus parked use, over the logged distance. Exact from the pack’s counters.',
      render: (r) => format(r.gross_wh_per_km, 'efficiency'), csv: column('Gross consumption', 'gross_wh_per_km', 'efficiency') },
    { key: 'overhead', label: 'Overhead', num: true, sort: true, value: (r) => r.overhead_pct, cover: 'statistics.col.consumption_overhead', title: 'How much parked use added on top of driving: gross over net, minus one',
      render: (r) => twoLine(r.overhead_pct != null ? pctText(r.overhead_pct, 0) : '—', r.overhead_pct != null ? 'parked on top' : null), csv: (r) => r.overhead_pct },
    { key: 'data', label: 'Data', sort: true, value: (r) => r.completeness?.recorded_pct, cover: 'statistics.col.data_complete', title: 'How complete the logs are for this period',
      render: completeCell, csv: (r) => r.completeness?.recorded_pct },
    { key: 'unknown', label: 'Unknown time (min)', cls: 'st-hide', render: null, csv: (r) => (r.completeness?.unknown_ms == null ? null : Math.round(r.completeness.unknown_ms / 60000)) },
    { key: 'dropped', label: 'Dropped stretches', cls: 'st-hide', render: null, csv: (r) => r.completeness?.dropped_stretches },
    { key: 'open', label: 'Open drive or charge', cls: 'st-hide', render: null, csv: (r) => [r.completeness?.open_drive && 'drive', r.completeness?.open_charge && 'charge'].filter(Boolean).join(' ') },
    { key: 'imported', label: 'Imported rows', cls: 'st-hide', render: null, csv: (r) => r.completeness?.teslamate_rows },
  ].filter(Boolean);
  // columns only for the CSV are not drawn in the table
  const shownColumns = columns.filter((c) => c.render !== null);
  const csvColumns = columns.filter((c) => c.csv !== false).map((c) => (c.csv && typeof c.csv === 'object' ? c.csv : { label: c.label, value: c.csv || ((r) => r[c.key]) }));

  /* ---------- loading */
  let gen = 0;
  async function load() {
    const my = ++gen;
    subGroup.textContent = `one row per ${GROUP_NAME[group]}`;
    replace(totalsSlot, panel({ title: 'Totals for the period', icon: 'stats' }, h('div', { class: 'st-totals' }, Array.from({ length: 8 }, () => h('div', { class: 'metric' }, skel('text'), skel('num'))))));
    let rows;
    try {
      rows = await ctx.api.list(statsPath, { ...ctx.params, period: group, cars: carsParam }, { limit: 500 }).all({ max: 20000 });
    } catch (e) {
      if (my !== gen || !ctx.alive()) return;
      if (my === 1) { ctx.fail(e, { retry: () => ctx.reload() }); return; }
      replace(totalsSlot, panel({ title: 'Totals for the period', icon: 'stats' }, h('p', { class: 'note warn' }, `The statistics could not be loaded: ${e.message}`)));
      return;
    }
    if (my !== gen || !ctx.alive()) return;
    paintTotals(rows);
    paintTable(rows);
    replace(mileSlot, panel({ title: 'Odometer and mileage', icon: 'odo' }, h('div', { class: 'st-chart-skel' }, skel('block'))));
    replace(milestonesSlot);
    if (my === 1) ctx.ready();
    const mileage = await loadMileage(my);
    if (my !== gen || !ctx.alive()) return;
    await paintMileage(mileage, my);
    if (my !== gen || !ctx.alive()) return;
    const longest = await loadLongest(my);
    if (my !== gen || !ctx.alive()) return;
    paintMilestones(rows, mileage, longest);
  }

  // a few requests at a time
  async function eachCar(fn) {
    const out = new Map();
    const ids = [...viewCars];
    const work = async () => { while (ids.length) { const cid = ids.shift(); try { out.set(cid, await fn(cid)); } catch { out.set(cid, null); } } };
    await Promise.all(Array.from({ length: Math.min(6, ids.length) }, work));
    return out;
  }
  const mileGroup = () => (group === 'all' ? 'month' : group);
  async function loadMileage() {
    return eachCar(async (cid) => {
      const p = `cars/${enc(cid)}/mileage`;
      const pages = [];
      for await (const pg of ctx.api.list(p, { ...ctx.params, group: mileGroup() }, { limit: 500 }).pages()) { pages.push(pg); if (pages.length > 40) break; }
      return { items: pages.flatMap((pg) => pg.items || []), projection: pages[0]?.projection || null };
    });
  }
  async function loadLongest() {
    return eachCar(async (cid) => (await ctx.api.page(`cars/${enc(cid)}/drives`, { ...ctx.params, sort: 'distance', dir: 'desc' }, { limit: 1 }))?.items?.[0] || null);
  }

  /* ---------- totals */
  function paintTotals(rows) {
    const odo = known(rows.map((r) => r.distance_odometer_km));
    const logged = sum(rows.map((r) => r.distance_logged_km));
    const used = sum(rows.map((r) => r.energy_used_kwh));
    const standing = sum(rows.map((r) => r.energy_standing_kwh));
    const added = sum(rows.map((r) => r.energy_added_kwh));
    const grid = sum(rows.map((r) => r.energy_grid_kwh));
    const ms = sum(rows.map((r) => r.time_driven_ms));
    const drives = sum(rows.map((r) => r.drives));
    const charges = sum(rows.map((r) => r.charges));
    const tW = sum(rows.map((r) => (r.outside_avg_c != null ? r.outside_avg_c * (r.time_driven_ms || 0) : 0)));
    const tMs = sum(rows.map((r) => (r.outside_avg_c != null ? r.time_driven_ms || 0 : 0)));
    const cost = new Map(), basis = new Map();
    for (const r of rows) {
      for (const c of r.cost_by_currency || []) cost.set(c.currency, (cost.get(c.currency) || 0) + (c.amount || 0));
      for (const p of r.price_per_kwh || []) {
        const amt = (r.cost_by_currency || []).find((c) => c.currency === p.currency)?.amount;
        if (amt && p.value) basis.set(p.currency, (basis.get(p.currency) || 0) + amt / p.value);
      }
    }
    const costList = [...cost].map(([currency, amount]) => ({ currency, amount: Math.round(amount * 100) / 100 }));
    const net = logged > 0.1 ? (used / logged) * 1000 : null;
    const gross = logged > 0.1 ? ((used + standing) / logged) * 1000 : null;
    let span = 0, rec = 0, unknown = 0, dropped = 0, imported = 0, open = 0;
    for (const r of rows) {
      const s = r.period_end_ms - r.period_start_ms, c = r.completeness || {};
      span += s; rec += s * (c.recorded_pct ?? 0) / 100; unknown += c.unknown_ms || 0; dropped += c.dropped_stretches || 0;
      imported += c.teslamate_rows || 0; open += (c.open_drive ? 1 : 0) + (c.open_charge ? 1 : 0);
    }
    const none = !rows.length;
    const nCars = new Set(rows.map((r) => r.car)).size;
    const across = multi ? ` across ${formatNumber(nCars)} ${nCars === 1 ? 'car' : 'cars'}` : '';
    replace(totalsSlot, panel({ title: 'Totals for the period', icon: 'stats', side: h('span', { class: 'faint st-side' }, `${period.label}${across}`) },
      h('div', { class: 'st-totals' },
        metric('Distance', odo.length ? sum(odo) : null, 'distance', { size: 'lg', why: none ? 'No data in this period' : 'No odometer reading',
          explain: `by odometer; ${format(logged, 'distance')} in logged drives${odo.length && sum(odo) - logged > 1 ? `, ${format(sum(odo) - logged, 'distance')} not logged` : ''}` }),
        metric('Time driving', none ? null : ms, 'duration', { why: 'No data in this period', explain: `over ${formatNumber(drives)} ${drives === 1 ? 'drive' : 'drives'}` }),
        metric('Energy used', none ? null : used + standing, 'energy', { why: 'No data in this period', explain: `${format(used, 'energy')} driving, ${format(standing, 'energy')} parked` }),
        metric('Energy added', none ? null : added, 'energy', { why: 'No data in this period', explain: `into the pack over ${formatNumber(charges)} ${charges === 1 ? 'charge' : 'charges'}${grid ? `; ${format(grid, 'energy')} from the grid (AC)` : ''}` }),
        metric('Net consumption', net, 'efficiency', { why: 'Not enough distance', explain: 'driving energy over the logged distance' }),
        metric('Gross consumption', gross, 'efficiency', { why: 'Not enough distance', explain: net && gross ? `with parked use: ${pctText(((gross - net) / net) * 100, 0)} on top of driving` : 'driving and parked energy over the logged distance' }),
        metric('Outside while driving', tMs ? tW / tMs : null, 'temperature', { why: 'No drive', explain: 'average, weighted by driving time' }),
        metric('Charging cost', costList.length ? moneyStack(costList) : null, null, { why: charges ? 'No charge has a price' : 'No charge', explain: costList.length
          ? `per currency, never added across${[...basis].length ? `; ${[...basis].map(([c, b]) => format({ amount: cost.get(c) / b, currency: c }, 'price_energy')).join(', ')} on average` : ''}` : 'per currency' }),
        metric('Data recorded', span ? (rec / span) * 100 : null, 'percent', { digits: 1, why: 'No data in this period', explain: [unknown ? `${formatDuration(unknown)} unknown` : 'no unknown time', dropped ? `${formatNumber(dropped)} dropped ${dropped === 1 ? 'stretch' : 'stretches'}` : null, open ? `${formatNumber(open)} still open` : null, imported ? `${formatNumber(imported)} imported rows (estimated energy)` : null].filter(Boolean).join(' · ') }))));
  }

  /* ---------- table */
  let table = null;
  function paintTable(rows) {
    const sorted = [...rows].sort((a, b) => b.period_start_ms - a.period_start_ms || carName(a.car).localeCompare(carName(b.car)));
    table = dataTable(ctx, {
      title: `Per ${GROUP_NAME[group]}`, icon: 'calendar', rows: sorted, columns: shownColumns, sort: { key: 'period', dir: 'desc' }, cls: 'st-table', cover: 'statistics.table',
      csv: { filename: csvFilename(scopeName, 'statistics', group), columns: csvColumns },
      note: `One row per ${GROUP_NAME[group]}${multi ? ' and car' : ''}, newest first; drives and charges count by their start, so a ${GROUP_NAME[group]} adds up to Drives and Charges for the same dates. Open a period as a trip, or its drives, charges and charging habits. Efficiency is the rated consumption over the measured one (above 100 % beats the rating). The CSV adds the parts this table folds into second lines.`,
      empty: () => emptyState({ icon: 'stats', title: 'Nothing in this period', text: `${scopeName} has no recorded data in ${period.label.toLowerCase()}. Pick a longer period, or another car.` }),
    });
    replace(tableSlot, table.el);
  }

  /* ---------- odometer and mileage */
  // labels across every car, oldest first (each car groups in its own time zone, the labels match)
  function axisOf(mileage) {
    const labels = new Map();
    for (const [, m] of mileage) for (const it of m?.items || []) if (!labels.has(it.label)) labels.set(it.label, it.period_start_ms);
    return [...labels].sort((a, b) => a[1] - b[1] || a[0].localeCompare(b[0])).map(([l, ms]) => ({ label: l, ms }));
  }
  const axisText = (lab, ms, t) => {
    const g = mileGroup();
    if (g === 'day') return formatDate(ms, t, { weekday: false, year: false });
    if (g === 'week') { const m = /^(\d{4})-W(\d{2})$/.exec(lab); return m ? `W${Number(m[2])} ${m[1].slice(2)}` : lab; }
    if (g === 'month') return dateFormat(t, { month: 'short', year: '2-digit' }).format(ms);
    return lab;
  };

  async function paintMileage(mileage) {
    const axis = axisOf(mileage);
    const cars = viewCars.filter((cid) => mileage.get(cid)?.items?.length);
    const perCar = viewCars.map((cid) => {
      const m = mileage.get(cid);
      const items = m?.items || [];
      const first = items.find((x) => x.odometer_start_km != null);
      const last = [...items].reverse().find((x) => x.odometer_end_km != null);
      return {
        car: cid, start: first?.odometer_start_km ?? null, end: last?.odometer_end_km ?? null,
        distance: known(items.map((x) => x.distance_km)).length ? sum(items.map((x) => x.distance_km)) : null,
        logged: sum(items.map((x) => x.logged_km)), not_logged: sum(items.map((x) => x.not_logged_km)),
        per_year: m?.projection?.km_per_year ?? null, basis_days: m?.projection?.basis_days ?? null, failed: m == null,
      };
    });

    const legend = cars.length > 1 ? h('div', { class: 'st-legend', role: 'group', 'aria-label': 'Cars on the odometer chart' }) : null;
    const odoBox = h('div', { class: 'st-chart st-chart--odo', role: 'img', 'aria-label': 'Odometer at the end of each period' });
    const distBox = h('div', { class: 'st-chart st-chart--dist', role: 'img', 'aria-label': 'Distance per period, logged and not logged' });
    const readOdo = h('p', { class: 'st-readout', 'aria-live': 'polite' }, 'Point at the chart for the odometer of a period.');
    const readDist = h('p', { class: 'st-readout', 'aria-live': 'polite' }, 'Point at a bar for its distance.');
    const groupNote = group === 'all' ? ' The charts are by month, since the table shows the whole period as one row.' : '';

    const odoCsv = () => downloadCsv(csvFilename(scopeName, 'odometer', mileGroup()),
      cars.flatMap((cid) => (mileage.get(cid).items || []).map((x) => ({ ...x, car: carName(cid) }))),
      [{ label: 'Car', value: (x) => x.car }, { label: 'Period', value: (x) => x.label }, { label: 'From', value: (x) => iso(x.period_start_ms) },
        column('Odometer at start', 'odometer_start_km', 'odometer'), column('Odometer at end', 'odometer_end_km', 'odometer'),
        column('Distance', 'distance_km', 'distance'), column('Logged', 'logged_km', 'distance'), column('Not logged', 'not_logged_km', 'distance')]);

    // summed over the cars for the distance chart
    const distTotal = axis.map(({ label }) => { const v = cars.map((cid) => mileage.get(cid).items.find((x) => x.label === label)?.distance_km).filter((v) => v != null); return v.length ? sum(v) : null; });
    const distLogged = axis.map(({ label }) => { const v = cars.map((cid) => mileage.get(cid).items.find((x) => x.label === label)?.logged_km).filter((v) => v != null); return v.length ? sum(v) : null; });

    const perCarTable = dataTable(ctx, {
      title: multi ? 'Each car' : 'This car', icon: 'car', rows: perCar, cls: 'st-percar', cover: 'mi.section.per_car',
      csv: { filename: csvFilename(scopeName, 'mileage per car'), columns: [{ label: 'Car', value: (r) => carName(r.car) }, column('Odometer at start', 'start', 'odometer'), column('Odometer at end', 'end', 'odometer'),
        column('Distance', 'distance', 'distance'), column('Logged', 'logged', 'distance'), column('Not logged', 'not_logged', 'distance'), column('Projected per year', 'per_year', 'distance')] },
      columns: [
        { key: 'car', label: 'Car', sort: true, value: (r) => carName(r.car), render: (r) => h('a', { class: 'st-car nowrap', href: state.buildUrl('car', { car: r.car }) }, h('i', { class: 'st-swatch', vars: { '--c': `var(${colorOf.get(r.car)})` } }), carName(r.car)) },
        { key: 'odo', label: 'Odometer', num: true, sort: true, value: (r) => r.end, render: (r) => twoLine(format(r.end, 'odometer'), r.start != null ? `from ${format(r.start, 'odometer')}` : r.failed ? 'could not be loaded' : 'no reading') },
        { key: 'distance', label: 'Distance', num: true, sort: true, value: (r) => r.distance, render: (r) => format(r.distance, 'distance') },
        { key: 'logged', label: 'Logged', num: true, sort: true, value: (r) => r.logged, render: (r) => twoLine(format(r.logged, 'distance'), r.distance ? `${pctText(Math.min(100, (r.logged / r.distance) * 100), 0)} of it` : null) },
        { key: 'not_logged', label: 'Not logged', num: true, sort: true, value: (r) => r.not_logged, render: (r) => twoLine(format(r.not_logged, 'distance'), r.not_logged > 0.5 ? 'driven in gaps' : 'none') },
        { key: 'per_year', label: 'Per year', num: true, sort: true, value: (r) => r.per_year, title: 'At the pace of this period',
          render: (r) => twoLine(format(r.per_year, 'distance', { digits: 0 }), r.per_year != null ? `pace of ${formatNumber(Math.round(r.basis_days))} days` : 'needs a week of data') },
      ],
      sort: { key: 'car', dir: 'asc' },
      note: 'Distance is by odometer. Logged is what recorded drives cover; not logged is what the car drove while nothing was recorded. The yearly figure projects this period’s pace over 365 days.',
    });

    const body = axis.length ? [
      h('div', { class: 'st-mile-grid' },
        h('div', { class: 'st-mile-chart' },
          h('div', { class: 'st-chart-head' }, h('h3', { class: 'st-h3' }, `Odometer, ${unitLabel('odometer')}`), legend, csvButton({ onClick: odoCsv })),
          odoBox, readOdo),
        h('div', { class: 'st-mile-chart' },
          h('div', { class: 'st-chart-head' }, h('h3', { class: 'st-h3' }, `Distance per ${GROUP_NAME[mileGroup()]}, ${unitLabel('distance')}`),
            h('span', { class: 'st-key' }, h('span', null, h('i', { class: 'st-key-logged' }), 'logged'), h('span', null, h('i', { class: 'st-key-gap' }), 'not logged')),
            csvButton({ onClick: odoCsv })),
          distBox, readDist)),
      h('p', { class: 'note' }, `The odometer is read at the end of each ${GROUP_NAME[mileGroup()]}; the line breaks where there was no reading. Not-logged distance is driven while nothing was recorded (no log, a log that failed its check).${groupNote}`),
    ] : [emptyState({ icon: 'odo', title: 'No odometer readings', text: `${scopeName} has no odometer reading in ${period.label.toLowerCase()}. Pick a longer period.` })];
    replace(mileSlot, h('section', { class: 'panel panel-pad st-mileage', 'aria-label': 'Odometer and mileage', cover: 'mi.chart.odometer' },
      h('div', { class: 'panel-head' }, h('h2', { class: 'panel-title' }, icon('odo'), 'Odometer and mileage'),
        h('span', { class: 'faint st-side' }, multi ? 'one line per car' : carName(viewCars[0]))),
      body), perCarTable.el);
    if (!axis.length) return;

    await loadUPlot();
    if (!ctx.alive()) return;
    const xs = axis.map((_, i) => i);
    const t0 = carTz(cars[0]);
    const labels = axis.map((a) => axisText(a.label, a.ms, t0));
    const fullLabel = (i) => {
      const a = axis[i];
      const g = mileGroup();
      if (g === 'day') return formatDate(a.ms, t0, { year: true });
      if (g === 'month') return dateFormat(t0, { month: 'long', year: 'numeric' }).format(a.ms);
      if (g === 'week') { const m = /^(\d{4})-W(\d{2})$/.exec(a.label); return m ? `Week ${Number(m[2])}, ${m[1]}` : a.label; }
      return a.label;
    };
    const catAxis = () => {
      const c = colors();
      return {
        show: true, size: rpx(24), gap: rpx(4), stroke: c.axis, font: chartFont(), grid: { show: false }, ticks: { show: false },
        splits: (u) => { const n = xs.length; const step = Math.max(1, Math.ceil(n / Math.max(3, u.width / rpx(70)))); return xs.filter((i) => (n - 1 - i) % step === 0); },
        values: (_, sp) => sp.map((i) => labels[i] ?? ''),
      };
    };
    const xScale = { time: false, range: () => [-0.6, xs.length - 0.4] };

    // odometer: one line per car
    const odoSeries = cars.map((cid) => axis.map(({ label }) => {
      const it = mileage.get(cid).items.find((x) => x.label === label);
      return it?.odometer_end_km == null ? null : convert(it.odometer_end_km, 'odometer');
    }));
    const few = xs.length <= 40;
    mountPlot(odoBox, (width, height) => {
      const c = colors();
      return {
        width, height, padding: [rpx(10), rpx(14), 0, 0], legend: { show: false },
        cursor: { y: false, points: { size: rpx(8), width: rpx(2), stroke: c.cursor }, drag: { x: false, y: false } },
        scales: { x: xScale, y: { auto: true } },
        axes: [catAxis(), valueAxis({ digits: 0, size: 64 })],
        series: [{}, ...cars.map((cid) => { const col = cssVar(colorOf.get(cid)); return { label: carName(cid), stroke: col, width: rpx(2), spanGaps: false, points: { show: few, size: rpx(5), fill: col, stroke: col } }; })],
        hooks: { setCursor: [(u) => {
          const i = u.cursor.idx;
          if (i == null || u.cursor.left < 0) { readOdo.textContent = 'Point at the chart for the odometer of a period.'; return; }
          replace(readOdo, h('b', null, fullLabel(i)), ...cars.map((cid, k) => h('span', { class: 'st-read-item' }, h('i', { class: 'st-swatch', vars: { '--c': `var(${colorOf.get(cid)})` } }),
            cars.length > 1 ? `${carName(cid)} ` : '', odoSeries[k][i] == null ? 'no reading' : `${formatNumber(Math.round(odoSeries[k][i]))} ${unitLabel('odometer')}`)));
        }] },
      };
    }, [xs, ...odoSeries], ctx, (u) => {
      if (!legend) return;
      replace(legend, cars.map((cid, k) => {
        const b = h('button', { class: 'legend-chip', type: 'button', 'aria-pressed': String(u.series[k + 1].show !== false), title: 'Show or hide this car' },
          h('i', { class: 'legend-swatch', vars: { '--c': `var(${colorOf.get(cid)})` } }), h('span', null, carName(cid)));
        b.addEventListener('click', () => { const on = b.getAttribute('aria-pressed') !== 'true'; b.setAttribute('aria-pressed', String(on)); u.setSeries(k + 1, { show: on }); });
        return b;
      }));
    });

    // distance: the whole bar by odometer, the logged part drawn over it
    const uPlot = window.uPlot;
    const dTot = distTotal.map((v) => (v == null ? null : convert(v, 'distance')));
    const dLog = distLogged.map((v) => (v == null ? null : convert(v, 'distance')));
    mountPlot(distBox, (width, height) => {
      const c = colors();
      const live = cssVar('--live'), gap = cssVar('--warm');
      const bars = uPlot.paths.bars({ size: [0.68, rpx(36), rpx(2)], radius: [0.2, 0], gap: 0 });
      return {
        width, height, padding: [rpx(10), rpx(14), 0, 0], legend: { show: false },
        cursor: { y: false, points: { show: false }, drag: { x: false, y: false } },
        scales: { x: xScale, y: { range: (_, a, b) => [0, Math.max(1, (b || 0) * 1.12)] } },
        axes: [catAxis(), valueAxis({ digits: 0, size: 52 })],
        series: [{},
          { label: 'Not logged', stroke: gap, fill: hexA(gap, 0.55), width: 0, paths: bars, points: { show: false } },
          { label: 'Logged', stroke: live, fill: hexA(live, 0.8), width: 0, paths: bars, points: { show: false } }],
        hooks: { setCursor: [(u) => {
          const i = u.cursor.idx;
          if (i == null || u.cursor.left < 0) { readDist.textContent = 'Point at a bar for its distance.'; return; }
          const tot = distTotal[i], lg = distLogged[i];
          replace(readDist, h('b', null, fullLabel(i)), h('span', { class: 'st-read-item' }, `${format(tot, 'distance')} by odometer`),
            h('span', { class: 'st-read-item' }, `${format(lg, 'distance')} logged`),
            tot != null && lg != null ? h('span', { class: 'st-read-item' }, `${format(Math.max(0, tot - lg), 'distance')} not logged`) : null);
        }] },
      };
    }, [xs, dTot, dLog], ctx);
  }

  /* ---------- milestones */
  function paintMilestones(rows, mileage, longest) {
    const items = [];
    const u = getUnits().distance === 'mi' ? 'mi' : 'km';
    const toKm = (v) => (u === 'mi' ? v * 1.609344 : v);
    // odometer marks passed in the period, in the person's distance unit
    const MARKS = [1000, 5000, 10000, 25000, 50000, 75000, 100000, 150000, 200000, 250000, 300000, 400000, 500000];
    const marks = [];
    for (const cid of viewCars) {
      for (const it of mileage.get(cid)?.items || []) {
        if (it.odometer_start_km == null || it.odometer_end_km == null) continue;
        for (const m of MARKS) {
          const km = toKm(m);
          if (it.odometer_start_km < km && it.odometer_end_km >= km) marks.push({ cid, m, it });
        }
      }
    }
    marks.sort((a, b) => b.it.period_start_ms - a.it.period_start_ms);
    for (const { cid, m, it } of marks.slice(0, 4)) {
      const t = carTz(cid);
      items.push(stone('flag', `${formatNumber(m)} ${u} on the odometer`, `${multi ? `${carName(cid)} passed it` : 'Passed'} in ${axisLong(it, t)}`,
        h('a', { class: 'link', href: state.buildUrl('trip', { car: cid, period: { from: it.period_start_ms, to: it.period_end_ms } }) }, 'Open that period'), 'first'));
    }
    // the longest drive
    let best = null;
    for (const [cid, d] of longest) if (d && (!best || (d.distance_km ?? 0) > (best.d.distance_km ?? 0))) best = { cid, d };
    if (best) {
      const t = carTz(best.cid);
      items.push(stone('route', 'Longest drive', `${format(best.d.distance_km, 'distance')} in ${formatDuration(best.d.duration_ms)}, ${formatDate(best.d.start_ms, t, { year: true })}${multi ? ` (${carName(best.cid)})` : ''}`,
        h('a', { class: 'link', href: state.buildUrl('drive', { car: best.cid, drive: best.d.id }) }, 'Open the drive'), 'record'));
    }
    const pick = (xs, f, dir) => xs.filter((r) => f(r) != null).sort((a, b) => (f(a) - f(b)) * dir)[0] || null;
    const what = group === 'all' ? 'period' : GROUP_NAME[group];
    const minKm = { day: 20, week: 60, month: 150, year: 500, all: 50 }[group];
    const rowLink = (r, text = 'Open it as a trip') => h('a', { class: 'link', href: linkFor('trip', r) }, text);
    const who = (r) => (multi ? ` (${carName(r.car)})` : '');
    if (group !== 'all' && rows.length > 1) {
      const far = pick(rows, (r) => r.distance_odometer_km ?? r.distance_logged_km, -1);
      if (far) items.push(stone('odo', `Furthest ${what}`, `${format(far.distance_odometer_km ?? far.distance_logged_km, 'distance')} in ${rowName(far)}${who(far)}`, rowLink(far), 'record'));
    }
    const eff = pick(rows.filter((r) => (r.distance_logged_km ?? 0) >= minKm), (r) => r.net_wh_per_km, 1);
    if (eff) items.push(stone('leaf', group === 'all' ? 'Most efficient car' : `Most efficient ${what}`, `${format(eff.net_wh_per_km, 'efficiency')} net in ${rowName(eff)}${who(eff)}`, rowLink(eff), 'record',
      `among ${what === 'period' ? 'cars' : `${what}s`} with at least ${format(minKm, 'distance', { digits: 0 })} driven`));
    const chg = pick(rows, (r) => (r.energy_added_kwh ? r.energy_added_kwh : null), -1);
    if (chg && rows.length > 1) items.push(stone('bolt', `Most energy charged in a ${what}`, `${format(chg.energy_added_kwh, 'energy')} over ${formatNumber(chg.charges)} ${chg.charges === 1 ? 'charge' : 'charges'} in ${rowName(chg)}${who(chg)}`,
      h('a', { class: 'link', href: linkFor('charges', chg) }, 'Open the charges'), 'record'));
    const cold = pick(rows.filter((r) => r.drives), (r) => r.outside_avg_c, 1);
    if (cold && rows.length > 1) items.push(stone('thermo', `Coldest ${what} on the road`, `${format(cold.outside_avg_c, 'temperature')} on average while driving in ${rowName(cold)}${who(cold)}`, rowLink(cold), 'record'));
    const busy = pick(rows, (r) => r.drives || null, -1);
    if (busy && rows.length > 1) items.push(stone('steer', `Most drives in a ${what}`, `${formatNumber(busy.drives)} drives in ${rowName(busy)}${who(busy)}`,
      h('a', { class: 'link', href: linkFor('drives', busy) }, 'Open the drives'), 'record'));

    replace(milestonesSlot, panel({ title: 'Milestones', icon: 'flag', cover: 'projects.achievements', side: h('span', { class: 'faint st-side' }, 'firsts and records in this period') },
      items.length ? h('ul', { class: 'st-stones' }, items) : emptyState({ icon: 'flag', title: 'No milestones yet', text: `Nothing in ${period.label.toLowerCase()} stands out yet: milestones need drives and charges. Pick a longer period.` }),
      h('p', { class: 'note' }, `Worked out from the rows above and each car’s longest drive. Odometer marks are in ${u}, the unit you picked; a mark shows the ${GROUP_NAME[mileGroup()]} the car passed it in.`)));
  }
  function stone(ic, title, text, link, kind, hint = null) {
    return h('li', { class: ['st-stone', `st-stone--${kind}`] },
      h('span', { class: 'st-stone-ico' }, icon(ic)),
      h('span', { class: 'st-stone-body' },
        h('span', { class: 'st-stone-kind' }, kind === 'first' ? 'Milestone' : 'Record'),
        h('b', null, title), h('span', null, text), hint ? h('span', { class: 'faint' }, hint) : null, link));
  }
  function axisLong(it, t) {
    const g = mileGroup();
    if (g === 'day') return formatDate(it.period_start_ms, t, { year: true });
    if (g === 'month') return dateFormat(t, { month: 'long', year: 'numeric' }).format(it.period_start_ms);
    if (g === 'week') { const m = /^(\d{4})-W(\d{2})$/.exec(it.label); return m ? `week ${Number(m[2])} of ${m[1]}` : it.label; }
    return it.label;
  }
  function rowName(r) {
    const t = r.time_zone || carTz(r.car);
    if (group === 'day') return formatDate(r.period_start_ms, t, { year: true });
    if (group === 'month') return dateFormat(t, { month: 'long', year: 'numeric' }).format(r.period_start_ms);
    if (group === 'week') { const m = /^(\d{4})-W(\d{2})$/.exec(r.label || ''); return m ? `week ${Number(m[2])} of ${m[1]}` : r.label; }
    if (group === 'year') return r.label;
    return 'the period';
  }

  /* ---------- live */
  let timer = 0;
  ctx.onCleanup(() => clearTimeout(timer));
  ctx.events('rows', () => { clearTimeout(timer); timer = setTimeout(() => load(), 1500); });

  await load();
}
