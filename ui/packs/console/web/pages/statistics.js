// Statistics (/cars/{car}/statistics, or /statistics?cars=all|ids): a period switch [day] [week] [month]
// [year] [all], then one table row per period with distance, time, energy, cost and consumption and a totals
// row; the odometer and distance per period (uPlot), each car's mileage, and milestones worked out from the
// same rows. GET .../statistics (every row; sorted here), GET cars/{car}/mileage, GET cars/{car}/drives
// (the longest). Periods follow each car's own time zone. Live: "rows" events load it again.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { format, formatMoney, convert, unitLabel, getUnits } from '../shared/units.js';
import { formatDate, formatDateRange, formatDuration, dateFormat, startOf, addPeriod, DAY } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { head, section, kv, note, link } from '../core/ui.js';
import { carKeys } from '../core/carpick.js';
import { num, tag } from '../core/fmt.js';
import { table, cellBar } from '../core/table.js';
import { empty, loadingLine, errorBlock } from '../core/states.js';
import { chartOrNote, line, valueAxis, readout, colors, hexA, font } from '../core/chart.js';
import { rpx } from '../core/display.js';

export const id = 'statistics';
export const title = () => 'Statistics';

const enc = encodeURIComponent;
const GROUPS = [['day', 'one row per day'], ['week', 'one row per iso week, monday to sunday'], ['month', 'one row per calendar month'], ['year', 'one row per calendar year'], ['all', 'the whole range as one row']];
const NAME = { day: 'day', week: 'week', month: 'month', year: 'year', all: 'range' };
const sum = (xs) => xs.reduce((a, b) => a + (b ?? 0), 0);
const known = (xs) => xs.filter((x) => x != null);
const none = (t) => h('span', { class: 'muted' }, t);
const pct = (v, d = 0) => format(v, 'percent', { digits: d });
const moneyLines = (list) => {
  const xs = (list || []).filter((x) => (x.amount ?? x.value) != null);
  return xs.length ? h('span', { class: 'money' }, xs.map((x, i) => [i ? ' ' : null, x.value != null && x.amount == null ? format({ amount: x.value, currency: x.currency }, 'price_energy') : formatMoney(x.amount, x.currency)])) : null;
};

function defaultGroup(p) {
  if (p?.from_ms == null) return 'month';
  const d = (p.to_ms - p.from_ms) / DAY;
  return d <= 45 ? 'day' : d <= 190 ? 'week' : d <= 365 * 4 ? 'month' : 'year';
}
function partOf(r, g, tz) {
  if (g === 'all') return null;
  const s = startOf(r.period_start_ms, g, tz), e = addPeriod(s, g, 1, tz);
  return s !== r.period_start_ms || e !== r.period_end_ms;
}

export async function mount(ctx) {
  const { tz, period } = ctx;
  const fixed = ctx.car;
  const q = ctx.query;
  let group = GROUPS.some(([g]) => g === q.group) ? q.group : defaultGroup(period);

  let fleet = [];
  try { fleet = (await ctx.api.page('cars', { sort: 'name' }, { limit: 200 })).items || []; } catch { fleet = fixed ? [fixed] : []; }
  if (!ctx.alive()) return;
  const carById = new Map(fleet.map((c) => [c.id, c]));
  if (fixed) carById.set(fixed.id, fixed);
  const viewCars = fixed ? [fixed.id] : (!q.cars || q.cars === 'all' ? fleet.map((c) => c.id) : q.cars.split(',').filter((c) => carById.has(c)));
  const multi = !fixed;
  const carsParam = fixed ? null : (!q.cars || q.cars === 'all' ? 'all' : viewCars.join(','));
  const carName = (cid) => carById.get(cid)?.name || cid;
  const carTz = (cid) => carById.get(cid)?.time_zone || tz;
  const statsPath = fixed ? `cars/${enc(fixed.id)}/statistics` : 'statistics';
  const linkFor = (page, r) => state.buildUrl(page, { car: r.car, period: { from: r.period_start_ms, to: r.period_end_ms } });
  const scope = fixed ? fixed.name : viewCars.length === fleet.length ? `all ${formatNumber(fleet.length)} cars` : viewCars.map(carName).join(', ');

  /* ---------- head, period switch, cars */
  const hd = head(ctx, { title: 'Statistics', extra: multi ? h('span', { class: 'ph-extra muted' }, scope) : null });
  const [periodBtn, carBtn] = hd.querySelectorAll('.ph-btn');
  if (periodBtn) periodBtn.dataset.cover = 'statistics.filter.period mi.filter.time_range';
  if (carBtn) carBtn.dataset.cover = 'statistics.filter.car mi.section.per_car';
  const groupKeys = h('span', { class: 'keys keys--inline', role: 'group', 'aria-label': 'One row per', cover: 'statistics.control.time_range' },
    GROUPS.map(([g, help]) => h('button', { class: 'key', type: 'button', title: help, 'aria-pressed': String(group === g), dataset: { group: g },
      onclick: () => { if (group === g) return; group = g; for (const b of groupKeys.children) b.setAttribute('aria-pressed', String(b.dataset.group === g)); ctx.setQuery({ group: g }); load(); } }, g)));
  const switchLine = h('div', { class: 'st-switch' }, h('span', { class: 'muted' }, 'per> '), groupKeys,
    multi ? h('span', { class: 'st-cars', cover: ['statistics.filter.car', 'mi.section.per_car'] }, h('span', { class: 'muted' }, '  cars> '), h('span', { class: 'keys keys--inline' },
      h('a', { class: 'key', href: state.buildUrl('statistics', { period: state.current().period, query: { group, cars: 'all' } }), 'aria-pressed': String(!q.cars || q.cars === 'all') }, 'all'),
      carKeys(fleet, (c) => h('a', { class: 'key', href: state.buildUrl('statistics', { car: c.id, period: state.current().period, query: { group } }) }, c.name), { keep: q.cars && q.cars !== 'all' ? viewCars : null })))
      : h('span', { class: 'st-cars' }, link(state.buildUrl('statistics', { period: state.current().period, query: { group, cars: 'all' } }), 'every car at once')));
  const precision = h('p', { class: 'note', cover: 'statistics.filter.high_precision' },
    'no precision switch: net and gross consumption come from the pack’s own energy counters, exact for a day as for a year. rows at the edges of the range can be part periods; they say so.');

  const totalsSlot = h('div');
  const tableSlot = h('div');
  const mileSlot = h('div');
  const stonesSlot = h('div');
  replace(ctx.view, h('div', { class: 'page', dataset: { page: 'statistics' }, cover: ['shots.statistics', 'shots.mileage'] },
    hd, switchLine, precision, totalsSlot, tableSlot, mileSlot, stonesSlot));

  /* ---------- the period table */
  const periodText = (r) => {
    const t = r.time_zone || carTz(r.car);
    if (group === 'day') return formatDate(r.period_start_ms, t, { weekday: true });
    if (group === 'week') { const m = /^(\d{4})-W(\d{2})$/.exec(r.label || ''); return m ? `${m[1]}-w${m[2]}` : r.label; }
    if (group === 'month') return dateFormat(t, { month: 'short', year: 'numeric' }).format(r.period_start_ms);
    if (group === 'year') return r.label;
    return formatDateRange(r.period_start_ms, r.period_end_ms - 1, t);
  };
  const countLink = (n, page, r, cv) => (n ? h('a', { class: 'lnk', href: linkFor(page, r), cover: cv }, formatNumber(n)) : h('span', { class: 'muted', cover: cv }, '0'));
  const rec = (r) => {
    const c = r.completeness || {};
    const good = (c.recorded_pct ?? 0) >= 99.5 && !c.dropped_stretches && !c.open_drive && !c.open_charge;
    const bits = [c.unknown_ms ? `${formatDuration(c.unknown_ms)} unknown` : null, c.dropped_stretches ? `${c.dropped_stretches} dropped` : null,
      c.open_drive ? 'drive open' : null, c.open_charge ? 'charge open' : null, c.teslamate_rows ? `${c.teslamate_rows} imported` : null].filter(Boolean);
    return h('span', { title: bits.join(', ') || 'nothing missing' }, cellBar((c.recorded_pct ?? 0) / 100, 5, good ? 'accent' : 'warn'), ' ', c.recorded_pct == null ? '—' : c.recorded_pct >= 99 && c.recorded_pct < 100 ? pct(Math.floor(c.recorded_pct * 10) / 10, 1) : pct(c.recorded_pct));
  };

  function totalsOf(rows) {
    const odo = known(rows.map((r) => r.distance_odometer_km));
    const logged = sum(rows.map((r) => r.distance_logged_km));
    const used = sum(rows.map((r) => r.energy_used_kwh));
    const standing = sum(rows.map((r) => r.energy_standing_kwh));
    const cost = new Map(), basis = new Map();
    for (const r of rows) {
      for (const c of r.cost_by_currency || []) cost.set(c.currency, (cost.get(c.currency) || 0) + (c.amount || 0));
      for (const p of r.price_per_kwh || []) {
        const a = (r.cost_by_currency || []).find((c) => c.currency === p.currency)?.amount;
        if (a && p.value) basis.set(p.currency, (basis.get(p.currency) || 0) + a / p.value);
      }
    }
    const tW = sum(rows.map((r) => (r.outside_avg_c != null ? r.outside_avg_c * (r.time_driven_ms || 0) : 0)));
    const tMs = sum(rows.map((r) => (r.outside_avg_c != null ? r.time_driven_ms || 0 : 0)));
    let span = 0, recd = 0;
    for (const r of rows) { const s = r.period_end_ms - r.period_start_ms; span += s; recd += (s * (r.completeness?.recorded_pct ?? 0)) / 100; }
    const net = logged > 0.1 ? (used / logged) * 1000 : null;
    const gross = logged > 0.1 ? ((used + standing) / logged) * 1000 : null;
    return {
      odo: odo.length ? sum(odo) : null, logged, used, standing, added: sum(rows.map((r) => r.energy_added_kwh)), grid: sum(rows.map((r) => r.energy_grid_kwh)),
      ms: sum(rows.map((r) => r.time_driven_ms)), drives: sum(rows.map((r) => r.drives)), charges: sum(rows.map((r) => r.charges)),
      cost: [...cost].map(([currency, amount]) => ({ currency, amount: Math.round(amount * 100) / 100 })),
      ppk: [...basis].map(([currency, b]) => ({ currency, value: cost.get(currency) / b })),
      temp: tMs ? tW / tMs : null, net, gross, overhead: net && gross ? ((gross - net) / net) * 100 : null, recorded: span ? (recd / span) * 100 : null,
    };
  }

  function paintTotals(t, rows) {
    const nothing = !rows.length;
    replace(totalsSlot, section({ title: 'totals', side: none(`${period.label}${multi ? `, ${scope}` : ''}`) }, kv([
      ['distance', nothing ? none('no data in this range') : [num(t.odo, 'distance'), none(`  ${format(t.logged, 'distance')} logged`)]],
      ['time driving', nothing ? none('—') : [formatDuration(t.ms), none(`  ${formatNumber(t.drives)} drives`)]],
      ['energy used', nothing ? none('—') : [num(t.used + t.standing, 'energy'), none(`  ${format(t.used, 'energy')} driving, ${format(t.standing, 'energy')} parked`)]],
      ['energy added', nothing ? none('—') : [num(t.added, 'energy'), none(`  ${formatNumber(t.charges)} charges${t.grid ? `, ${format(t.grid, 'energy')} from the grid` : ''}`)]],
      ['net', t.net != null ? num(t.net, 'efficiency') : none('not enough distance')],
      ['gross', t.gross != null ? [num(t.gross, 'efficiency'), t.overhead != null ? none(`  parked use adds ${pct(t.overhead)}`) : null] : none('not enough distance')],
      ['outside', t.temp != null ? [num(t.temp, 'temperature'), none('  while driving')] : none('no drive')],
      ['cost', moneyLines(t.cost) ? [moneyLines(t.cost), t.ppk.length ? none('  ') : null, moneyLines(t.ppk), t.ppk.length ? none(' avg') : null] : none(t.charges ? 'no charge has a price' : 'no charge')],
      ['recorded', t.recorded != null ? [cellBar(t.recorded / 100, 20, t.recorded >= 99.5 ? 'accent' : 'warn'), ' ', pct(t.recorded, 1)] : none('—')],
    ])));
  }

  let tbl = null;
  function paintTable(rows, t) {
    const sorted = [...rows].sort((a, b) => b.period_start_ms - a.period_start_ms || carName(a.car).localeCompare(carName(b.car)));
    const T = (f) => () => f();   // a fresh element per call: the desk and the phone row each get one
    tbl = table(ctx, {
      title: `per ${NAME[group]}`, rows: sorted, csv: false, cover: 'statistics.table', sort: { key: 'period', dir: 'desc' }, limit: 400,
      rowHref: (r) => linkFor('trip', r),
      note: `newest first${multi ? ', one row per car' : ''}. drives and charges count by their start. open a period as a trip; eff is the rated consumption over the measured one (above 100% beats the rating).`,
      columns: [
        { key: 'period', label: NAME[group], sort: true, firstDir: 'desc', ph: 'main', value: (r) => r.period_start_ms, cover: ['statistics.col.period', 'statistics.link.trip'],
          render: (r) => [h('a', { class: 'lnk-plain', href: linkFor('trip', r), cover: 'statistics.link.trip' }, periodText(r)), partOf(r, group, r.time_zone || carTz(r.car)) ? [' ', tag('part', 'muted')] : null] },
        multi ? { key: 'car', label: 'car', sort: true, ph: 'sub', value: (r) => carName(r.car), render: (r) => carName(r.car) } : null,
        { key: 'distance', label: 'distance', num: true, sort: true, ph: 'num', cover: 'statistics.col.distance', value: (r) => r.distance_odometer_km ?? r.distance_logged_km,
          render: (r) => num(r.distance_odometer_km ?? r.distance_logged_km, 'distance'), total: T(() => num(t.odo ?? t.logged, 'distance')) },
        { key: 'time', label: 'driving', num: true, sort: true, ph: 'sub', cover: 'statistics.col.time_driven', value: (r) => r.time_driven_ms,
          render: (r) => (r.time_driven_ms ? formatDuration(r.time_driven_ms) : null), total: T(() => formatDuration(t.ms)) },
        { key: 'drives', label: 'drives', num: true, sort: true, cover: ['statistics.col.drives_count', 'statistics.link.drives'], value: (r) => r.drives,
          render: (r) => countLink(r.drives, 'drives', r, 'statistics.link.drives'), total: T(() => formatNumber(t.drives)) },
        { key: 'temp', label: 'outside', num: true, sort: true, cover: 'statistics.col.avg_temp', value: (r) => r.outside_avg_c, render: (r) => (r.outside_avg_c != null ? num(r.outside_avg_c, 'temperature') : null),
          total: T(() => t.temp != null ? num(t.temp, 'temperature') : null) },
        { key: 'eff', label: 'eff', num: true, sort: true, cover: 'statistics.col.driving_efficiency', value: (r) => r.driving_efficiency_pct,
          render: (r) => (r.driving_efficiency_pct != null ? h('span', { class: r.driving_efficiency_pct >= 100 ? 'tone--accent' : null }, pct(r.driving_efficiency_pct)) : null) },
        { key: 'used', label: 'used', num: true, sort: true, cover: 'statistics.col.energy_used', value: (r) => r.energy_used_kwh,
          render: (r) => num(r.energy_used_kwh, 'energy'), total: T(() => num(t.used, 'energy')) },
        { key: 'added', label: 'added', num: true, sort: true, cover: 'statistics.link.charging_stats', value: (r) => r.energy_added_kwh,
          render: (r) => (r.charges ? h('a', { class: 'lnk-plain', href: `${linkFor('charges', r)}`, cover: 'statistics.link.charging_stats', title: 'charges and charging statistics of this period' }, num(r.energy_added_kwh, 'energy')) : num(r.energy_added_kwh, 'energy')),
          total: T(() => num(t.added, 'energy')) },
        { key: 'charges', label: 'charges', num: true, sort: true, cover: ['statistics.col.charges_count', 'statistics.link.charges'], value: (r) => r.charges,
          render: (r) => countLink(r.charges, 'charges', r, 'statistics.link.charges'), total: T(() => formatNumber(t.charges)) },
        { key: 'per_charge', label: '/charge', num: true, sort: true, cover: 'statistics.col.avg_energy_per_charge', value: (r) => r.energy_per_charge_kwh, render: (r) => num(r.energy_per_charge_kwh, 'energy'),
          total: T(() => t.charges ? num(t.added / t.charges, 'energy') : null) },
        { key: 'cost', label: 'cost', num: true, sort: true, ph: 'num', cover: 'statistics.col.costs', value: (r) => r.cost_by_currency?.[0]?.amount ?? null,
          render: (r) => moneyLines(r.cost_by_currency) || (r.charges ? none('no price') : null), total: T(() => moneyLines(t.cost)) },
        { key: 'ppk', label: '/kwh', num: true, cover: 'statistics.col.cost_per_kwh', render: (r) => moneyLines(r.price_per_kwh), total: T(() => moneyLines(t.ppk)) },
        { key: 'p100', label: `/100${unitLabel('distance')}`, num: true, cover: 'statistics.col.cost_per_distance',
          render: (r) => { const xs = (r.cost_per_100km || []).filter((x) => x.amount != null); return xs.length ? xs.map((x, i) => [i ? ' ' : null, formatMoney(convert(x.amount, 'money_per_distance'), x.currency)]) : null; } },
        { key: 'net', label: 'net', num: true, sort: true, cover: 'statistics.col.consumption_net', value: (r) => r.net_wh_per_km, render: (r) => num(r.net_wh_per_km, 'efficiency'), total: T(() => num(t.net, 'efficiency')) },
        { key: 'gross', label: 'gross', num: true, sort: true, cover: 'statistics.col.consumption_gross', value: (r) => r.gross_wh_per_km, render: (r) => num(r.gross_wh_per_km, 'efficiency'), total: T(() => num(t.gross, 'efficiency')) },
        { key: 'overhead', label: 'parked+', num: true, sort: true, cover: 'statistics.col.consumption_overhead', value: (r) => r.overhead_pct, render: (r) => (r.overhead_pct != null ? pct(r.overhead_pct) : null),
          total: T(() => t.overhead != null ? pct(t.overhead) : null) },
        { key: 'data', label: 'recorded', sort: true, cover: 'statistics.col.data_complete', value: (r) => r.completeness?.recorded_pct, render: rec },
      ].filter(Boolean),
      empty: () => empty('nothing in this range', `${scope} has no recorded data in ${String(period.label).toLowerCase()}`),
    });
    replace(tableSlot, tbl.el);
  }

  /* ---------- loading */
  let gen = 0;
  async function load() {
    const my = ++gen;
    replace(totalsSlot, loadingLine(`loading per ${NAME[group]}`));
    replace(tableSlot); replace(mileSlot); replace(stonesSlot);
    let rows;
    try {
      rows = await ctx.api.list(statsPath, { ...ctx.params, period: group, cars: carsParam }, { limit: 500 }).all({ max: 20000 });
    } catch (e) {
      if (my !== gen || !ctx.alive()) return;
      if (my === 1) { ctx.fail(e, { retry: () => ctx.reload() }); return; }
      replace(totalsSlot, errorBlock(e, { retry: () => load(), home: false }));
      return;
    }
    if (my !== gen || !ctx.alive()) return;
    const t = totalsOf(rows);
    paintTotals(t, rows);
    paintTable(rows, t);
    replace(mileSlot, loadingLine('loading the odometer'));
    const mileage = await loadMileage();
    if (my !== gen || !ctx.alive()) return;
    await paintMileage(mileage);
    if (my !== gen || !ctx.alive()) return;
    const longest = await eachCar(async (cid) => (await ctx.api.page(`cars/${enc(cid)}/drives`, { ...ctx.params, sort: 'distance', dir: 'desc' }, { limit: 1 }))?.items?.[0] || null);
    if (my !== gen || !ctx.alive()) return;
    paintStones(rows, mileage, longest);
    if (my === 1) ctx.ready();
  }
  async function eachCar(fn) {
    const out = new Map();
    const ids = [...viewCars];
    const work = async () => { while (ids.length) { const cid = ids.shift(); try { out.set(cid, await fn(cid)); } catch { out.set(cid, null); } } };
    await Promise.all(Array.from({ length: Math.min(6, ids.length) }, work));
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
    const labels = new Map();
    for (const [, m] of mileage) for (const it of m?.items || []) if (!labels.has(it.label)) labels.set(it.label, it.period_start_ms);
    const axis = [...labels].sort((a, b) => a[1] - b[1]).map(([l, ms]) => ({ label: l, ms }));
    const cars = viewCars.filter((cid) => mileage.get(cid)?.items?.length);
    const t0 = carTz(cars[0] || viewCars[0]);
    const g = mileGroup();
    const axisText = (a) => (g === 'day' ? formatDate(a.ms, t0, { weekday: false, year: false }) : g === 'week' ? a.label.replace(/^\d\d(\d\d)-W/, '$1w') : g === 'month' ? dateFormat(t0, { month: 'short', year: '2-digit' }).format(a.ms) : a.label);
    const perCar = viewCars.map((cid) => {
      const items = mileage.get(cid)?.items || [];
      const m = mileage.get(cid);
      return {
        car: cid, start: items.find((x) => x.odometer_start_km != null)?.odometer_start_km ?? null,
        end: [...items].reverse().find((x) => x.odometer_end_km != null)?.odometer_end_km ?? null,
        distance: known(items.map((x) => x.distance_km)).length ? sum(items.map((x) => x.distance_km)) : null,
        logged: sum(items.map((x) => x.logged_km)), not_logged: sum(items.map((x) => x.not_logged_km)),
        per_year: m?.projection?.km_per_year ?? null, basis_days: m?.projection?.basis_days ?? null, failed: m == null,
      };
    });
    const perTable = table(ctx, {
      title: multi ? 'each car' : 'mileage', rows: perCar, csv: false, cover: 'mi.section.per_car', sort: { key: 'car', dir: 'asc' },
      note: 'distance by odometer; logged is what recorded drives cover, not logged was driven while nothing recorded. per year projects this range’s pace.',
      columns: [
        { key: 'car', label: 'car', sort: true, ph: 'main', grow: true, value: (r) => carName(r.car), render: (r) => link(state.buildUrl('car', { car: r.car }), carName(r.car)) },
        { key: 'start', label: 'odo start', num: true, render: (r) => num(r.start, 'odometer') },
        { key: 'end', label: 'odo end', num: true, sort: true, ph: 'sub', value: (r) => r.end, render: (r) => (r.failed ? none('could not load') : num(r.end, 'odometer')) },
        { key: 'distance', label: 'distance', num: true, sort: true, ph: 'num', value: (r) => r.distance, render: (r) => num(r.distance, 'distance') },
        { key: 'logged', label: 'logged', num: true, sort: true, value: (r) => r.logged, render: (r) => [num(r.logged, 'distance'), r.distance ? none(` ${pct(Math.min(100, (r.logged / r.distance) * 100))}`) : null] },
        { key: 'not_logged', label: 'not logged', num: true, sort: true, value: (r) => r.not_logged, render: (r) => (r.not_logged > 0.5 ? h('span', { class: 'tone--warn' }, format(r.not_logged, 'distance')) : num(r.not_logged, 'distance')) },
        { key: 'per_year', label: 'per year', num: true, sort: true, value: (r) => r.per_year, render: (r) => (r.per_year != null ? num(r.per_year, 'distance', { digits: 0 }) : none('needs a week')) },
      ],
    });
    const odoBox = h('div', { class: 'chart chart--pane' });
    const distBox = h('div', { class: 'chart chart--pane' });
    const rOdo = readout('move over the chart for the odometer of a period');
    const rDist = readout('move over a bar for its distance');
    replace(mileSlot,
      section({ title: 'odometer and mileage', cover: 'mi.chart.odometer', side: none(`per ${NAME[g]}${group === 'all' ? ' (the charts are by month)' : ''}`) },
        axis.length ? [
          h('div', { class: 'chart-head' }, h('b', null, 'odometer'), h('span', null, unitLabel('odometer')),
            cars.length > 1 ? h('span', { class: 'chart-key' }, cars.slice(0, 3).map((cid, k) => h('span', { class: `chart-key-i chart-key-i--${k + 1}` }, `${['--', '- -', '. .'][k]} ${carName(cid)}`))) : null),
          odoBox, rOdo.el,
          h('div', { class: 'chart-head' }, h('b', null, `distance per ${NAME[g]}`), h('span', null, unitLabel('distance')),
            h('span', { class: 'chart-key' }, h('span', { class: 'chart-key-i chart-key-i--1' }, '## logged'), h('span', { class: 'chart-key-i chart-key-i--2' }, '[] not logged'))),
          distBox, rDist.el,
        ] : empty('no odometer readings in this range', 'pick a longer range')),
      perTable.el);
    if (!axis.length) return;
    const xs = axis.map((_, i) => i);
    const catAxis = () => {
      const c = colors();
      return { show: true, size: rpx(28), gap: rpx(4), stroke: c.muted, font: font(12), grid: { show: false }, ticks: { show: false },
        splits: (u) => { const n = xs.length; const step = Math.max(1, Math.ceil(n / Math.max(3, u.width / rpx(80)))); return xs.filter((i) => (n - 1 - i) % step === 0); },
        values: (_, sp) => sp.map((i) => (axis[i] ? axisText(axis[i]) : '')) };
    };
    const xScale = { time: false, range: () => [-0.6, xs.length - 0.4] };
    const odo = cars.map((cid) => axis.map(({ label }) => { const it = mileage.get(cid).items.find((x) => x.label === label); return it?.odometer_end_km == null ? null : convert(it.odometer_end_km, 'odometer'); }));
    await chartOrNote(odoBox, ctx, () => ({
      padding: [rpx(6), rpx(8), 0, 0], cursor: { x: true, y: false, points: { show: false }, drag: { x: false, y: false } }, select: { show: false },
      scales: { x: xScale, y: { auto: true } },
      axes: [catAxis(), valueAxis((v) => formatNumber(v), { size: 72 })],
      series: [{}, ...cars.map((cid, k) => ({ ...line(carName(cid), k % 3, { width: 2 }), points: { show: xs.length <= 40, size: rpx(5) } }))],
    }), [xs, ...odo], { onCursor: (i) => rOdo.show(i == null ? null : [[NAME[g], axisText(axis[i])], ...cars.map((cid, k) => [cars.length > 1 ? carName(cid) : 'odometer', odo[k][i] == null ? 'no reading' : `${formatNumber(Math.round(odo[k][i]))} ${unitLabel('odometer')}`])]) });
    if (!ctx.alive()) return;
    const tot = axis.map(({ label }) => { const v = cars.map((cid) => mileage.get(cid).items.find((x) => x.label === label)?.distance_km).filter((v) => v != null); return v.length ? convert(sum(v), 'distance') : null; });
    const lg = axis.map(({ label }) => { const v = cars.map((cid) => mileage.get(cid).items.find((x) => x.label === label)?.logged_km).filter((v) => v != null); return v.length ? convert(sum(v), 'distance') : null; });
    await chartOrNote(distBox, ctx, ({ colors: c }) => {
      const bars = window.uPlot.paths.bars({ size: [0.66, rpx(36), rpx(2)], gap: 0 });
      return {
        padding: [rpx(6), rpx(8), 0, 0], cursor: { x: true, y: false, points: { show: false }, drag: { x: false, y: false } }, select: { show: false },
        scales: { x: xScale, y: { range: (u, a, b) => [0, Math.max(1, (b || 0) * 1.1)] } },
        axes: [catAxis(), valueAxis((v) => formatNumber(v), { size: 72 })],
        series: [{}, { label: 'by odometer', stroke: c.c2, fill: hexA(c.c2, 0.18), width: rpx(1), dash: [rpx(3), rpx(2)], paths: bars, points: { show: false } },
          { label: 'logged', stroke: c.c1, fill: c.c1, width: 0, paths: bars, points: { show: false } }],
      };
    }, [xs, tot, lg], { onCursor: (i) => rDist.show(i == null ? null : [[NAME[g], axisText(axis[i])], ['odometer', tot[i] == null ? '—' : `${formatNumber(tot[i], { maximumFractionDigits: 1 })} ${unitLabel('distance')}`],
      ['logged', lg[i] == null ? '—' : `${formatNumber(lg[i], { maximumFractionDigits: 1 })} ${unitLabel('distance')}`], ['not logged', tot[i] != null && lg[i] != null ? `${formatNumber(Math.max(0, tot[i] - lg[i]), { maximumFractionDigits: 1 })} ${unitLabel('distance')}` : '—']]) });
  }

  /* ---------- milestones */
  function rowName(r) {
    const t = r.time_zone || carTz(r.car);
    if (group === 'day') return formatDate(r.period_start_ms, t, { weekday: false });
    if (group === 'month') return dateFormat(t, { month: 'long', year: 'numeric' }).format(r.period_start_ms);
    if (group === 'week') { const m = /^(\d{4})-W(\d{2})$/.exec(r.label || ''); return m ? `week ${Number(m[2])} of ${m[1]}` : r.label; }
    return group === 'year' ? r.label : 'the range';
  }
  function paintStones(rows, mileage, longest) {
    const out = [];
    const u = getUnits().distance === 'mi' ? 'mi' : 'km';
    const toKm = (v) => (u === 'mi' ? v * 1.609344 : v);
    const MARKS = [1000, 5000, 10000, 25000, 50000, 75000, 100000, 150000, 200000, 250000, 300000, 400000, 500000];
    const who = (cid) => (multi ? ` (${carName(cid)})` : '');
    const marks = [];
    for (const cid of viewCars) for (const it of mileage.get(cid)?.items || []) {
      if (it.odometer_start_km == null || it.odometer_end_km == null) continue;
      for (const m of MARKS) if (it.odometer_start_km < toKm(m) && it.odometer_end_km >= toKm(m)) marks.push({ cid, m, it });
    }
    marks.sort((a, b) => b.it.period_start_ms - a.it.period_start_ms);
    for (const { cid, m, it } of marks.slice(0, 4)) {
      out.push(['first', `${formatNumber(m)} ${u} on the odometer${who(cid)}`, formatDate(it.period_start_ms, carTz(cid), { weekday: false }), state.buildUrl('trip', { car: cid, period: { from: it.period_start_ms, to: it.period_end_ms } })]);
    }
    let best = null;
    for (const [cid, d] of longest) if (d && (!best || (d.distance_km ?? 0) > (best.d.distance_km ?? 0))) best = { cid, d };
    if (best) out.push(['record', `longest drive ${format(best.d.distance_km, 'distance')} in ${formatDuration(best.d.duration_ms)}${who(best.cid)}`, formatDate(best.d.start_ms, carTz(best.cid), { weekday: false }), state.buildUrl('drive', { car: best.cid, drive: best.d.id })]);
    const pick = (xs, f, dir) => xs.filter((r) => f(r) != null).sort((a, b) => (f(a) - f(b)) * dir)[0] || null;
    const what = NAME[group];
    const minKm = { day: 20, week: 60, month: 150, year: 500, all: 50 }[group];
    if (group !== 'all' && rows.length > 1) {
      const far = pick(rows, (r) => r.distance_odometer_km ?? r.distance_logged_km, -1);
      if (far) out.push(['record', `furthest ${what} ${format(far.distance_odometer_km ?? far.distance_logged_km, 'distance')}${who(far.car)}`, rowName(far), linkFor('trip', far)]);
    }
    const eff = pick(rows.filter((r) => (r.distance_logged_km ?? 0) >= minKm), (r) => r.net_wh_per_km, 1);
    if (eff) out.push(['record', `most efficient ${what} ${format(eff.net_wh_per_km, 'efficiency')} net${who(eff.car)}`, rowName(eff), linkFor('trip', eff)]);
    const chg = rows.length > 1 ? pick(rows, (r) => r.energy_added_kwh || null, -1) : null;
    if (chg) out.push(['record', `most charged in a ${what} ${format(chg.energy_added_kwh, 'energy')}${who(chg.car)}`, rowName(chg), linkFor('charges', chg)]);
    const cold = rows.length > 1 ? pick(rows.filter((r) => r.drives), (r) => r.outside_avg_c, 1) : null;
    if (cold) out.push(['record', `coldest ${what} on the road ${format(cold.outside_avg_c, 'temperature')}${who(cold.car)}`, rowName(cold), linkFor('trip', cold)]);
    const busy = rows.length > 1 ? pick(rows, (r) => r.drives || null, -1) : null;
    if (busy) out.push(['record', `most drives in a ${what}: ${formatNumber(busy.drives)}${who(busy.car)}`, rowName(busy), linkFor('drives', busy)]);
    replace(stonesSlot, section({ title: 'milestones', cover: 'projects.achievements', side: none('firsts and records in this range') },
      out.length ? h('ul', { class: 'stones' }, out.map(([kind, text, when, href]) => h('li', { dataset: { row: '', href } },
        tag(kind === 'first' ? 'first' : 'record', kind === 'first' ? 'accent' : 'warn'), ' ', h('a', { class: 'lnk-plain', href }, text), none(`  ${when}`))))
        : empty('no milestones yet', 'milestones need drives and charges; pick a longer range'),
      note(`odometer marks are in ${u}, the unit you picked.`)));
  }

  let timer = 0;
  ctx.onCleanup(() => clearTimeout(timer));
  // built again off screen and swapped in whole: the tables and charts never go back to loading lines
  ctx.events('rows', () => { clearTimeout(timer); timer = setTimeout(() => ctx.refresh(), 1500); });
  await load();
}
