// Sentry Mate, "Pebble" UI pack: Statistics (/cars/{car}/statistics, or /statistics for every car), totals per
// week, month or year (or day, or the whole period as one), and the odometer over time.
//
// A segmented control picks the grouping. Then the totals of the period as number tiles, a bar tile of the
// distance per group, one tile per group with every figure (each opens as a trip, its drives and its charges), the
// odometer over time with the yearly pace, and the milestones (marks passed and records) of the period.
// Data: GET /api/v1/cars/{car}/statistics (or /statistics?cars=all), /cars/{car}/mileage, /cars/{car}/drives.

import { h } from '../shared/dom.js';
import { format, formatMoney, convert, getUnits, unitLabel } from '../shared/units.js';
import { formatDate, formatDateRange, formatDuration, dateFormat, DAY } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { tile, num, icon, note, chip } from '../core/ui.js';
import { emptyTile } from '../core/states.js';
import { segmented } from '../core/form.js';
import { timeChart, bars } from '../core/chart.js';
import { filterBar, statTile, money, sectionHead, nums } from '../core/bits.js';

export const id = 'statistics';
export const css = true;
export const shape = [['full', 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1'], ['full', 'h2'], [2, 'h2'], [2, 'h2']];
export const title = () => 'Statistics';

const GROUPS = [['day', 'Day'], ['week', 'Week'], ['month', 'Month'], ['year', 'Year'], ['all', 'Whole period']];
const WORD = { day: 'day', week: 'week', month: 'month', year: 'year', all: 'period' };
const n0 = (v) => (v == null ? '—' : formatNumber(v, { maximumFractionDigits: 0 }));
const pct = (v, d = 0) => format(v, 'percent', { digits: d });
const sum = (rows, f) => rows.reduce((a, r) => a + (f(r) || 0), 0);

/** Money lists added per currency (never across currencies). */
function addMoney(lists, key = 'amount') {
  const by = new Map();
  for (const l of lists) for (const x of l || []) if (x && x[key] != null) by.set(x.currency, (by.get(x.currency) || 0) + x[key]);
  return [...by].map(([currency, amount]) => ({ currency, amount }));
}

// the total cost of a row: priced plus estimated, per currency (an older server sends only the priced cost)
const totalOf = (r) => r.cost_total_by_currency || r.cost_by_currency || [];
const moneyLine = (list, kind = 'money') => list.map((x) => (kind === 'money' ? formatMoney(x.amount, x.currency) : format({ amount: x.amount, currency: x.currency }, kind))).join(' + ');
// under a row's total: the estimated part, else how many of its charges have a price
function costSub(r) {
  const est = (r.cost_estimated_by_currency || []).filter((x) => x && x.amount);
  if (est.length) return `est. ${moneyLine(est)}`;
  return r.charges_priced != null && r.charges && r.charges_priced < r.charges && totalOf(r).length ? `${n0(r.charges_priced)} of ${n0(r.charges)} priced` : null;
}

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz, period = ctx.period;
  const statsPath = car ? api.apiPath`cars/${car.id}/statistics` : 'statistics';
  let group = GROUPS.some(([g]) => g === ctx.query.group) ? ctx.query.group : (period.preset === 'all' || period.preset === 'year' ? 'month' : 'week');

  const seg = segmented({ name: 'One tile per', value: group, cover: 'statistics.control.time_range', options: GROUPS,
    onchange: (g) => { group = g; ctx.setQuery({ group: g }); load(); } });
  const bar = filterBar(ctx, { carCover: ['statistics.filter.car', 'mi.section.per_car'], periodCover: ['statistics.filter.period', 'mi.filter.time_range'] }, seg);
  const precise = tile({ w: 'full', cls: 'st-precise', cover: 'statistics.filter.high_precision' },
    h('p', { class: 'note' }, 'No precision switch is needed: use comes from the pack’s own energy counters, so it is exact for a day as much as for a year. The first and last tiles can be part periods; they say so.'));
  const totBox = h('div', { class: 'contents' });
  const chartT = tile({ w: 'full', title: 'Distance', icon: 'bars', cls: 'chart-tile', cover: ['shots.statistics', 'shots.mileage'] });
  const rowsHead = sectionHead('Each period', 'Tap a period to open it as a trip, or its drives and charges.', { cover: 'statistics.table' });
  const rowsBox = h('div', { class: 'contents' });
  const odoT = tile({ w: 2, title: 'Odometer', icon: 'gauge', cls: 'chart-tile' });
  const stoneT = tile({ w: 2, title: 'Milestones', icon: 'flag', cover: 'projects.achievements' });

  const periodOf = (r) => ({ from: r.period_start_ms, to: r.period_end_ms });
  const linkFor = (page, r) => state.buildUrl(page, { car: r.car || car?.id, period: periodOf(r) });
  const zoneOf = (r) => r.time_zone || tz;
  function rowName(r) {
    const t = zoneOf(r);
    if (group === 'day') return formatDate(r.period_start_ms, t, { year: true });
    if (group === 'week') { const m = /^(\d{4})-W(\d{2})$/.exec(r.label || ''); return m ? `Week ${Number(m[2])}, ${m[1]}` : r.label; }
    if (group === 'month') return dateFormat(t, { month: 'long', year: 'numeric' }).format(r.period_start_ms);
    if (group === 'year') return r.label;
    return 'Whole period';
  }
  function shortName(r) {
    const t = zoneOf(r);
    if (group === 'day') return dateFormat(t, { month: 'short', day: 'numeric' }).format(r.period_start_ms);
    if (group === 'week') { const m = /^(\d{4})-W(\d{2})$/.exec(r.label || ''); return m ? `W${Number(m[2])}` : r.label; }
    if (group === 'month') return dateFormat(t, { month: 'short' }).format(r.period_start_ms);
    return r.label === 'All' ? 'All' : r.label;
  }

  function rowTile(r) {
    const c = r.completeness || {};
    const good = (c.recorded_pct ?? 0) >= 99.5 && !c.dropped_stretches && !c.open_drive && !c.open_charge;
    const missing = [c.unknown_ms ? `${formatDuration(c.unknown_ms)} unknown` : null, c.dropped_stretches ? `${n0(c.dropped_stretches)} dropped stretches` : null,
      c.open_drive ? 'a drive is open' : null, c.open_charge ? 'a charge is open' : null, c.teslamate_rows ? `${n0(c.teslamate_rows)} imported rows` : null].filter(Boolean);
    const recFill = h('span', { class: 'cov-fill' });
    if (c.recorded_pct > 0) recFill.style.setProperty('--v', String(Math.min(1, c.recorded_pct / 100))); else recFill.hidden = true;
    return tile({ w: 'full', cls: ['item', 'st-row'] },
      h('div', { class: 'item-top' },
        h('span', { class: 'item-ic tone-coral', 'aria-hidden': 'true' }, icon('calendar')),
        h('div', { class: 'item-text' },
          h('a', { class: 'item-title st-name', href: linkFor('trip', r), cover: ['statistics.col.period', 'statistics.link.trip'], title: 'Open this period as a trip' }, rowName(r)),
          h('p', { class: 'item-sub' }, group === 'all' || group === 'week' ? formatDateRange(r.period_start_ms, r.period_end_ms - 1, zoneOf(r)) : `${formatDuration(r.time_driven_ms || 0)} driving`)),
        h('span', { class: 'item-flags' }, chip(good ? 'Complete' : 'Gaps', { tone: good ? 'teal' : 'sun' }))),
      nums([
        ['Distance', num(r.distance_odometer_km ?? r.distance_logged_km, 'distance', { size: 's' }), 'statistics.col.distance'],
        ['Driving', h('span', { class: 'num num-s' }, h('span', { class: 'num-n' }, r.time_driven_ms ? formatDuration(r.time_driven_ms) : '—')), 'statistics.col.time_driven'],
        ['Drives', r.drives ? h('a', { class: 'st-count', href: linkFor('drives', r), cover: 'statistics.link.drives' }, n0(r.drives)) : h('span', { class: 'st-count is-zero', cover: 'statistics.link.drives' }, '0'), 'statistics.col.drives_count'],
        ['Outside', num(r.outside_avg_c, 'temperature', { size: 's' }), 'statistics.col.avg_temp'],
        ['Of rated use', num(r.driving_efficiency_pct, 'percent', { size: 's' }), 'statistics.col.driving_efficiency'],
        ['Used driving', num(r.energy_used_kwh, 'energy', { size: 's' }), 'statistics.col.energy_used'],
        ['Added', r.charges ? h('a', { class: 'st-count', href: `${linkFor('charges', r)}#habits`, cover: 'statistics.link.charging_stats', title: 'Charging habits of this period' }, format(r.energy_added_kwh, 'energy')) : h('span', { class: 'st-count is-zero', cover: 'statistics.link.charging_stats' }, format(r.energy_added_kwh ?? 0, 'energy'))],
        ['Charges', r.charges ? h('a', { class: 'st-count', href: linkFor('charges', r), cover: 'statistics.link.charges' }, n0(r.charges)) : h('span', { class: 'st-count is-zero', cover: 'statistics.link.charges' }, '0'), 'statistics.col.charges_count'],
        ['Per charge', num(r.energy_per_charge_kwh, 'energy', { size: 's' }), 'statistics.col.avg_energy_per_charge'],
        ['Cost', [money(totalOf(r), { size: 's' }), costSub(r) ? h('span', { class: 'tile-line' }, costSub(r)) : null], 'statistics.col.costs'],
        ['Per kWh', money(r.price_per_kwh, { size: 's', key: 'value', kind: 'price_energy' }), 'statistics.col.cost_per_kwh'],
        [`Priced per 100 ${unitLabel('distance')}`, money(r.cost_per_100km, { size: 's', kind: 'money_per_distance' }), 'statistics.col.cost_per_distance'],
        ['Net use', num(r.net_wh_per_km, 'efficiency', { size: 's' }), 'statistics.col.consumption_net'],
        ['Gross use', num(r.gross_wh_per_km, 'efficiency', { size: 's' }), 'statistics.col.consumption_gross'],
        ['Parked on top', num(r.overhead_pct, 'percent', { size: 's' }), 'statistics.col.consumption_overhead'],
      ]),
      h('div', { class: 'st-rec', cover: 'statistics.col.data_complete' },
        h('span', { class: 'cov', 'aria-hidden': 'true' }, recFill),
        h('span', { class: 'tile-line' }, `${c.recorded_pct == null ? '—' : pct(c.recorded_pct, c.recorded_pct >= 99 && c.recorded_pct < 100 ? 1 : 0)} recorded${missing.length ? ` · ${missing.join(' · ')}` : ' · nothing missing'}`)));
  }

  function paintTotals(rows) {
    const cost = addMoney(rows.map(totalOf));
    const est = addMoney(rows.map((r) => r.cost_estimated_by_currency)).filter((x) => x.amount);
    const charges = sum(rows, (r) => r.charges), logged = sum(rows, (r) => r.distance_logged_km);
    const priced = rows.some((r) => r.charges_priced != null) ? sum(rows, (r) => r.charges_priced) : null;
    const days = Math.max(1, sum(rows, (r) => (r.period_end_ms - r.period_start_ms) / DAY));
    const costNote = cost.length ? [
      priced != null ? `priced ${n0(priced)} of ${n0(charges)} charges` : 'per currency, never added across',
      est.length ? `${moneyLine(est)} estimated` : null,
      logged > 1 ? moneyLine(cost.map((x) => ({ currency: x.currency, amount: (x.amount / logged) * 100 })), 'money_per_distance') : null,
      `${moneyLine(cost.map((x) => ({ currency: x.currency, amount: x.amount / days })))} per day`,
    ].filter(Boolean).join(' · ') : 'per currency, never added across';
    const odoDropped = sum(rows, (r) => r.completeness?.odometer_dropped);
    totBox.replaceChildren(
      statTile({ label: 'Distance', value: sum(rows, (r) => r.distance_odometer_km ?? r.distance_logged_km), kind: 'distance', icon: 'route', sub: `${n0(sum(rows, (r) => r.drives))} drives${odoDropped > 0 ? `; ${n0(odoDropped)} odometer ${odoDropped === 1 ? 'reading' : 'readings'} left out` : ''}` }),
      statTile({ label: 'Driving time', value: sum(rows, (r) => r.time_driven_ms), kind: 'duration', icon: 'clock', sub: 'drives that started in the period' }),
      statTile({ label: 'Energy added', value: sum(rows, (r) => r.energy_added_kwh), kind: 'energy', icon: 'bolt', sub: `${n0(sum(rows, (r) => r.charges))} charges` }),
      tile({ w: 1, title: 'Total cost', icon: 'coin', cls: 'tile-stat' }, money(cost, { why: charges ? 'no charge has a price' : 'no charge' }), h('p', { class: 'tile-line' }, costNote)));
  }

  function paintChart(rows) {
    chartT.querySelectorAll(':scope > :not(.tile-head)').forEach((e) => e.remove());
    const list = rows.slice().sort((a, b) => a.period_start_ms - b.period_start_ms).slice(-24);
    if (!list.length) { chartT.append(emptyTile({ icon: 'bars', title: 'Nothing driven in this period', text: 'Pick a longer period above.' })); return; }
    chartT.append(
      h('p', { class: 'chart-note' }, `${unitLabel('distance')} per ${WORD[group]} by odometer${rows.length > 24 ? ', the last 24' : ''}. Tap a bar to open that ${WORD[group]} as a trip.`),
      bars(list.map((r) => {
        const v = r.distance_odometer_km ?? r.distance_logged_km ?? 0;
        return { label: shortName(r), value: v, text: formatNumber(Math.round(convert(v, 'distance'))), href: linkFor('trip', r), title: `${rowName(r)}: ${format(v, 'distance')}` };
      }), { tone: 'c1', label: `Distance per ${WORD[group]}` }));
  }

  // A live reload keeps the old chart and milestones on screen until the new answers are in, then swaps them in one go.
  const clear = (t) => t.querySelectorAll(':scope > :not(.tile-head)').forEach((e) => e.remove());
  async function paintOdometer() {
    if (!car) { clear(odoT); odoT.append(h('div', { cover: 'mi.chart.odometer' }, note('Open one car to see its odometer over time.'))); return; }
    const g = group === 'all' ? 'month' : group;
    const m = await ctx.api.get(api.apiPath`cars/${car.id}/mileage`, { ...ctx.params, group: g }).catch(() => null);
    if (!ctx.alive()) return;
    const old = odoT.querySelector('.plot');
    const oldH = old ? odoT.offsetHeight : 0;
    if (oldH) odoT.style.minHeight = `${oldH}px`;
    clear(odoT);
    const items = (m?.items || []).filter((x) => x.odometer_end_km != null).sort((a, b) => a.period_start_ms - b.period_start_ms);
    if (!items.length) { odoT.style.minHeight = ''; odoT.append(h('div', { cover: 'mi.chart.odometer' }, emptyTile({ icon: 'gauge', title: 'No odometer readings in this period', text: 'The car sends it while driving.', w: 2 }))); return; }
    await timeChart(ctx, odoT, { tz, t_ms: items.map((x) => x.period_start_ms), short: true, label: 'Odometer over time', cover: 'mi.chart.odometer', series: [
      { key: 'o', name: 'Odometer', kind: 'odometer', values: items.map((x) => x.odometer_end_km), tone: 'c1', fill: true, axis: 'left' },
      { key: 'n', name: 'Not logged', kind: 'distance', values: items.map((x) => x.not_logged_km), tone: 'c2', dashed: true, axis: 'right', floor: 0 },
    ] });
    odoT.style.minHeight = '';
    if (!ctx.alive()) return;
    const p = m.projection;
    odoT.append(h('p', { class: 'chart-note' }, p?.km_per_year != null ? `At this pace about ${format(p.km_per_year, 'distance', { digits: 0 })} a year (from the last ${n0(p.basis_days)} days).` : 'Not enough days yet for a yearly pace.',
      ' Not logged is distance driven while nothing was recorded.'));
  }

  async function paintStones(rows) {
    const items = [];
    const odoRows = rows.filter((r) => r.distance_odometer_km);
    const [longest, totals] = await Promise.all([
      car ? ctx.api.page(api.apiPath`cars/${car.id}/drives`, { ...ctx.params, sort: 'distance', dir: 'desc' }, { limit: 1 }).then((p) => p.items?.[0]).catch(() => null) : null,
      car && odoRows.length ? ctx.api.get(api.apiPath`cars/${car.id}/totals`).catch(() => null) : null,
    ]);
    if (!ctx.alive()) return;
    const stone = (ic, tone, name, text, href) => h('li', { class: 'stone' }, h('span', { class: `item-ic tone-${tone}`, 'aria-hidden': 'true' }, icon(ic)),
      h('span', { class: 'stone-text' }, h('b', null, name), h('span', null, text)), href ? h('a', { class: 'stone-go', href, 'aria-label': `Open: ${name}` }, icon('chevronRight')) : null);
    const mi = getUnits().distance === 'mi';
    const MARKS = [1000, 5000, 10000, 25000, 50000, 75000, 100000, 150000, 200000, 250000, 300000];
    const odo = rows.filter((r) => r.distance_odometer_km).sort((a, b) => a.period_start_ms - b.period_start_ms);
    if (longest) items.push(stone('route', 'coral', 'Longest drive', `${format(longest.distance_km, 'distance')} in ${formatDuration(longest.duration_ms)}, ${formatDate(longest.start_ms, tz, { year: true })}`, state.buildUrl('drive', { car: car.id, drive: longest.id })));
    const pick = (f, dir) => rows.filter((r) => f(r) != null).sort((a, b) => (f(a) - f(b)) * dir)[0] || null;
    if (rows.length > 1) {
      const far = pick((r) => r.distance_odometer_km ?? r.distance_logged_km, -1);
      if (far) items.push(stone('flag', 'sky', `Furthest ${WORD[group]}`, `${format(far.distance_odometer_km ?? far.distance_logged_km, 'distance')} in ${rowName(far)}`, linkFor('trip', far)));
      const eff = pick((r) => ((r.distance_logged_km ?? 0) >= 20 ? r.net_wh_per_km : null), 1);
      if (eff) items.push(stone('leaf', 'teal', `Most efficient ${WORD[group]}`, `${format(eff.net_wh_per_km, 'efficiency')} net in ${rowName(eff)}`, linkFor('trip', eff)));
      const chg = pick((r) => r.energy_added_kwh || null, -1);
      if (chg) items.push(stone('bolt', 'teal', `Most charged in a ${WORD[group]}`, `${format(chg.energy_added_kwh, 'energy')} in ${rowName(chg)}`, linkFor('charges', chg)));
    }
    if (car && odo.length) {
      const total = totals?.odometer_last_km;
      const shown = total != null ? (mi ? total / 1.609344 : total) : null;
      const next = shown != null ? MARKS.find((x) => x > shown) : null;
      if (next) items.push(stone('gauge', 'sun', `Next: ${formatNumber(next)} ${mi ? 'mi' : 'km'}`, `${formatNumber(Math.round(next - shown))} ${mi ? 'mi' : 'km'} to go on the odometer`, null));
    }
    clear(stoneT);
    stoneT.append(items.length ? h('ul', { class: 'stones' }, items) : emptyTile({ icon: 'flag', title: 'No milestones yet', text: 'Records need drives and charges. Pick a longer period.', w: 2 }),
      note('Firsts and records in this period, worked out from the tiles above.'));
  }

  async function load() {
    const rows = await ctx.api.list(statsPath, { ...ctx.params, period: group, cars: car ? undefined : 'all' }, { limit: 500 }).all({ max: 5000 }).catch((e) => ({ error: e }));
    if (!ctx.alive()) return;
    if (rows.error) { ctx.fail(rows.error, { retry: () => load() }); return; }
    paintTotals(rows);
    paintChart(rows);
    rowsBox.replaceChildren(...(rows.length ? rows.map(rowTile) : [emptyTile({ icon: 'calendar', title: 'Nothing in this period', text: 'Pick a longer period above.' })]));
    // the first time, the page goes in with its tiles filled, so nothing is pushed down when the rows arrive
    if (!bar.isConnected) ctx.view.replaceChildren(bar, precise, totBox, chartT, rowsHead, rowsBox, odoT, stoneT);
    await Promise.all([paintOdometer(), paintStones(rows)]);
    if (!ctx.alive()) return;
    ctx.ready();
  }
  await load();
  let timer = 0;
  ctx.onCleanup(() => clearTimeout(timer));
  ctx.events?.('rows', () => { clearTimeout(timer); timer = setTimeout(() => ctx.alive() && load(), 1500); });
  return {};
}
