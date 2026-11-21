// Charges (/cars/{car}/charges, or /charges?cars=all|ids): a dense table of the charging sessions in the
// range (GET .../charges, sorted and paged by the server, CSV) with a filter line, a stat line of the
// listed charges' totals, a power sparkline per row and a totals row; the charges with missing data; then
// the charging statistics (GET .../charging-stats, charging-stats/places, .../charging-stats/dc-curve) as
// key: value lines, text bars, text heat maps and two uPlot charts.
//
// filter> place:home kind:dc at:charger cost:5 unpriced dur:10 source:smo flag:gap words
//   place a saved place (names, comma), kind ac | dc | unknown, at the kind of place, cost the lowest
//   cost (unpriced keeps those without a price too), dur the shortest charging time in minutes, source,
//   flag, free words: text in the place or address.
// AC or DC comes from the charge port's own signal; a session without one is unknown, never DC. Money is
// per currency and never added across currencies.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { format, formatMoney, unitLabel, rangeLabel } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, buckets, bucketKey } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { head, section, kv, note, link } from '../core/ui.js';
import { num, tag, date } from '../core/fmt.js';
import { table, cellBar } from '../core/table.js';
import { empty } from '../core/states.js';
import { filterLine, numberOf } from '../core/filterline.js';
import { loadUPlot, plot, line, timeAxis, valueAxis, readout, colors, hexA } from '../core/chart.js';
import { safeMap } from '../core/map.js';
import { spark, textGrid, gridAxis } from '../core/spark.js';
import { rpx } from '../core/display.js';
import { isAdmin } from '../core/prefs.js';
import { toast } from '../core/toast.js';
import { placeLink } from './timeline.js';
import { src, est as estTag, srcText, sourceWord, SOURCE_KEY } from '../core/source.js';

export const id = 'charges';

const enc = encodeURIComponent;
export const KIND = { ac: 'ac', dc: 'dc', unknown: '?' };
export const KIND_TONE = { ac: 'accent', dc: 'warn', unknown: 'muted' };
const COST_SOURCE = { tariff: 'place price', manual: 'typed', free_supercharging: 'free', teslamate: 'imported' };
const KEYS = {
  place: 'saved places (place:home,work)', kind: 'ac | dc | unknown', at: 'home | work | charger | service | other | none',
  cost: 'lowest cost (cost:5)', unpriced: 'with cost: keep charges with no price', dur: 'shortest charging time in min', source: SOURCE_KEY, flag: 'gap | live',
};
const DAYS = ['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun'];

/** Money per currency, one per line, never added across. */
export const money = (list) => {
  const l = (list || []).filter((x) => x.amount != null);
  return l.length ? h('span', { class: 'money' }, l.map((x, i) => [i ? h('span', { class: 'muted' }, ' + ') : null, formatMoney(x.amount, x.currency)])) : null;
};
const prices = (list) => {
  const l = (list || []).filter((x) => x.value != null);
  return l.length ? h('span', { class: 'money' }, l.map((x, i) => [i ? h('span', { class: 'muted' }, ' + ') : null, format({ amount: x.value, currency: x.currency }, 'price_energy')])) : null;
};
const pct = (v) => format(v, 'percent');
const none = (t) => h('span', { class: 'muted' }, t);

export async function mount(ctx) {
  const { tz, period } = ctx;
  const fixed = ctx.car;
  const q = ctx.query;
  let fleet = [];
  try { fleet = (await ctx.api.page('cars', { sort: 'name' }, { limit: 200 })).items || []; } catch { fleet = fixed ? [fixed] : []; }
  if (!ctx.alive()) return;
  const carById = new Map(fleet.map((c) => [c.id, c]));
  if (fixed) carById.set(fixed.id, fixed);
  const chosen = fixed ? [fixed.id] : (!q.cars || q.cars === 'all' ? fleet.map((c) => c.id) : q.cars.split(',').filter(Boolean));
  const multi = !fixed;
  const carsParam = fixed ? null : (!q.cars || q.cars === 'all' ? 'all' : chosen.join(','));
  const carName = (cid) => carById.get(cid)?.name || cid;
  const scope = fixed ? fixed.name : chosen.length === fleet.length ? `all ${formatNumber(fleet.length)} cars` : chosen.map(carName).join(', ');
  const listPath = fixed ? `cars/${enc(fixed.id)}/charges` : 'charges';
  const statsPath = fixed ? `cars/${enc(fixed.id)}/charging-stats` : 'charging-stats';
  const chargeHref = (c) => state.buildUrl('charge', { car: c.car, charge: c.id });

  let places = [];
  try { places = (await ctx.api.page('places', { sort: 'visits' }, { limit: 200 })).items || []; } catch { places = []; }
  if (!ctx.alive()) return;
  const findPlaces = (v) => String(v || '').split(',').map((n) => n.trim().toLowerCase()).filter(Boolean)
    .map((n) => places.find((p) => p.name.toLowerCase() === n) || places.find((p) => p.name.toLowerCase().startsWith(n)) || places.find((p) => p.id === n)).filter(Boolean);

  /* ---------- the filter line */
  let f = {};
  const toF = (p) => {
    const k = p.keys;
    const pl = findPlaces(k.place);
    return {
      places: pl.map((x) => x.id).join(',') || null,
      place_kind: ['home', 'work', 'charger', 'service', 'other', 'none'].includes(k.at) ? k.at : null,
      kind: ['ac', 'dc', 'unknown'].includes(String(k.kind || '').toLowerCase()) ? String(k.kind).toLowerCase() : null,
      q: [p.text, k.place && !pl.length ? k.place : null].filter(Boolean).join(' ') || null,
      min_cost: numberOf(k.cost), unpriced: k.unpriced != null && !/^(no|0|false)$/i.test(k.unpriced),
      min_dur: numberOf(k.dur), source: sourceWord(k.source),
      flags: { gap: 'gap', live: 'in_progress' }[String(k.flag || '').toLowerCase()] || null,
    };
  };
  const base = () => ({ ...ctx.params, cars: carsParam });
  const listParams = () => ({
    ...base(), places: f.places, place_kind: f.place_kind, kind: f.kind, q: f.q,
    min_cost: f.min_cost, include_unpriced: f.min_cost != null && f.unpriced ? 'true' : null,
    min_duration_min: f.min_dur, source: f.source, flags: f.flags,
  });
  const statsParams = () => ({ ...base(), places: f.places, min_duration_min: f.min_dur });
  const fl = filterLine({
    keys: KEYS, value: q.f || '', placeholder: 'place:home kind:dc dur:10', label: 'Filter the charges',
    cover: ['charges.filter.geofence', 'charges.filter.location', 'charges.filter.charge_type', 'charges.filter.min_cost', 'charges.filter.min_duration', 'cs.filter.min_duration', 'cs.filter.geofence'],
    onApply: (p, raw) => {
      ctx.setQuery({ f: raw || null });
      f = toF(p);
      paintSummary(null, 'loading');
      list.reload(listParams());
      loadStats();
    },
  });
  f = toF(fl.parsed());

  /* ---------- the stat line (totals of the listed charges) */
  const summary = h('div', { class: 'stats' });
  function paintSummary(t, st = null) {
    t = t || {};
    const n = t.count ?? 0;
    const why = st === 'loading' ? 'loading' : st === 'error' ? 'could not be loaded' : 'no charge';
    const blank = st || !n;
    const v = (x) => (blank ? none(why) : x);
    const s = (k, val, cv) => h('span', { class: 'stat', cover: cv }, h('span', { class: 'stat-k' }, k), val);
    replace(summary,
      s('sessions', st ? none(why) : formatNumber(n)),
      s('added', v(num(t.energy_added_kwh, 'energy')), 'charges.stat.energy_added'),
      s('from grid', v(t.energy_grid_known ? [num(t.energy_grid_kwh, 'energy'), none(` ac, ${formatNumber(t.energy_grid_known)} of ${formatNumber(n)}`)] : none('no ac meter reading')), 'charges.stat.energy_used'),
      s('cost', v(money(t.cost_by_currency) || none('no price')), 'charges.stat.cost'),
      t.unpriced && !blank ? s('unpriced', formatNumber(t.unpriced)) : null,
      s('avg time', v(formatDuration(t.avg_duration_ms)), 'charges.stat.avg_duration'));
  }

  /* ---------- power sparklines, loaded a few at a time after the rows */
  const sparkJobs = [];
  let sparkBusy = 0;
  const sparkQueue = [];
  const runSparks = () => {
    while (sparkBusy < 4 && sparkQueue.length) {
      const [c, el, done] = sparkQueue.shift();
      sparkBusy++;
      ctx.api.get(`cars/${enc(c.car)}/charges/${enc(c.id)}/curve`, { fields: 'power_kw', max_points: 48 })
        .then((r) => { if (ctx.alive()) replace(el, spark(r?.values?.power_kw || [], { label: `power through the charge, peak ${format(c.power_max_kw, 'power')}` })); })
        .catch(() => { if (ctx.alive()) replace(el, none('-')); })
        .finally(() => { sparkBusy--; done(); runSparks(); });
    }
  };
  const sparkCell = (c) => {
    const el = h('span', { class: 'spark-slot' });
    sparkJobs.push(new Promise((done) => sparkQueue.push([c, el, done])));
    runSparks();
    return el;
  };

  /* ---------- the session table */
  const sum = (pg) => pg?.totals || null;
  const kindTag = (c) => h('span', { class: 'nowrap', title: c.kind === 'unknown' || !c.kind ? 'the charge port did not say ac or dc' : null },
    tag(KIND[c.kind] || '?', KIND_TONE[c.kind] || 'muted'),
    c.fast_charger ? h('span', { class: 'muted' }, ` ${c.fast_charger.type === 'supercharger' ? 'sc' : 'fast'}${c.fast_charger.class ? ` ${c.fast_charger.class.toLowerCase()}` : ''}`) : null);
  const costCell = (c) => {
    const txt = c.cost ? formatMoney(c.cost.amount, c.cost.currency) : 'set';
    const src = c.cost ? h('span', { class: 'muted' }, ` ${COST_SOURCE[c.cost.source] || c.cost.source}`) : null;
    return isAdmin()
      ? h('span', { class: 'nowrap' }, h('a', { class: ['lnk-plain', !c.cost && 'tone--accent'], href: state.buildUrl('charge', { car: c.car, charge: c.id, query: { edit: 'cost' } }), title: c.cost ? 'change the cost' : 'set a cost for this charge' }, c.cost ? txt : '[set]'), src)
      : h('span', { class: 'nowrap' }, c.cost ? txt : none('no price'), src);
  };
  const columns = [
    multi ? { key: 'car', label: 'car', ph: 'sub', render: (c) => carName(c.car) } : null,
    { key: 'start', label: 'date', sort: 'start', firstDir: 'desc', ph: 'main', cover: ['charges.table.date', 'charges.link.view_details'],
      render: (c) => h('a', { class: 'lnk-plain', href: chargeHref(c) }, `${date(c.start_ms, c.time_zone || tz, { year: false })} ${formatTime(c.start_ms, c.time_zone || tz)}`),
      total: (pg) => (sum(pg) ? `total ${formatNumber(sum(pg).count)}` : 'total') },
    { key: 'place', label: 'place', cls: 'wrap-cell', ph: 'sub', cover: ['charges.table.location', 'charges.link.geofence'], render: (c) => placeLink(c) || '—' },
    { key: 'kind', label: 'type', ph: 'sub', cover: 'charges.table.type', render: kindTag },
    { key: 'duration', label: 'time', num: true, sort: 'duration', cover: 'charges.table.duration', title: 'time charging (plugged-in waiting left out)', ph: 'sub', render: (c) => formatDuration(c.charging_ms) },
    { key: 'added', label: 'added', num: true, sort: 'energy', cover: 'charges.table.energy_added', ph: 'num', render: (c) => h('span', null, num(c.energy_added_kwh, 'energy'), estTag(c)), total: (pg) => (sum(pg) ? num(sum(pg).energy_added_kwh, 'energy') : null) },
    { key: 'grid', label: 'used', num: true, cover: 'charges.table.energy_used', title: 'ac: from the grid, measured; dc: delivered at the inlet',
      render: (c) => (c.energy_grid_kwh != null ? num(c.energy_grid_kwh, 'energy') : c.dc_inlet_kwh != null ? h('span', { title: 'delivered at the inlet; the grid side of a fast charger is not measured' }, num(c.dc_inlet_kwh, 'energy'), none(' in')) : null),
      total: (pg) => (sum(pg)?.energy_grid_known ? num(sum(pg).energy_grid_kwh, 'energy') : null) },
    { key: 'eff', label: 'eff', num: true, cover: 'charges.table.efficiency', title: 'energy into the pack over energy from the grid (ac only)',
      render: (c) => (c.efficiency_pct != null ? pct(c.efficiency_pct) : c.kind === 'dc' ? none('dc') : null) },
    { key: 'cost', label: 'cost', num: true, sort: 'cost', ph: 'num', cover: ['charges.table.cost', 'charges.link.set_cost'], render: costCell, total: (pg) => (sum(pg) ? money(sum(pg).cost_by_currency) : null) },
    { key: 'ppk', label: '/kwh', num: true, cover: 'charges.table.cost_per_kwh', title: 'cost over the grid energy (ac) or the energy added (dc)',
      render: (c) => (c.price_per_kwh?.value != null ? format({ amount: c.price_per_kwh.value, currency: c.cost?.currency }, 'price_energy') : null) },
    { key: 'power', label: 'avg', num: true, sort: 'power_max', cover: 'charges.table.avg_power', title: 'average power while charging', render: (c) => num(c.power_avg_kw, 'power') },
    { key: 'curve', label: 'power', cls: 'spark-col', title: 'power through the charge', render: sparkCell },
    { key: 'range', label: 'range+', num: true, cover: 'charges.table.range_added', title: `${rangeLabel()} added`, render: (c) => num(c.range_added_km, 'range') },
    { key: 'rate', label: '/h', num: true, cover: 'charges.table.charge_rate', title: 'range added per hour of charging', render: (c) => num(c.range_per_hour_km, 'range_rate') },
    { key: 'soc', label: 'soc', num: true, sort: 'end_soc', ph: 'sub', cover: ['charges.table.start_soc', 'charges.table.end_soc'], render: (c) => h('span', { class: 'nowrap' }, pct(c.start_soc_pct), h('span', { class: 'arrow' }, '->'), pct(c.end_soc_pct)) },
    { key: 'temp', label: 'out', num: true, cover: 'charges.table.temp', title: 'average outside temperature (battery in the tooltip)', render: (c) => h('span', { title: c.battery_avg_c != null ? `battery ${format(c.battery_avg_c, 'temperature')}` : null }, num(c.outside_avg_c, 'temperature')) },
    { key: 'odo', label: 'odo', num: true, cover: 'charges.table.odometer', render: (c) => num(c.odometer_km, 'odometer') },
    { key: 'flags', label: 'flags', title: 'gap: data missing; live: charging now',
      render: (c) => [c.has_gaps ? tag('gap', 'bad') : null, c.flags?.in_progress || c.end_ms == null ? tag('live', 'warn') : null].filter(Boolean) },
    { key: 'from', label: 'data', title: 'where the data came from: smo (the CAN Bridge), api (the Tesla API), imp (imported)', ph: 'sub', render: (c) => src(c), value: (c) => srcText(c) },
  ].filter(Boolean);
  const list = table(ctx, {
    title: 'charges', path: listPath, params: listParams(), limit: 25, columns, sort: { key: 'start', dir: 'desc' }, csvName: 'charges',
    cover: 'charges.table', noteCover: 'charges.text.list_help',
    note: 'time is charging time; waiting while plugged in is left out, so the average power is not watered down. added is measured at the pack. dc efficiency is unknown: a fast charger does not report its grid side. open a row for its curve and cost.',
    rowHref: chargeHref,
    onPage: (pg) => { if (pg?.totals) paintSummary(pg.totals); },
    onError: () => paintSummary(null, 'error'),
    empty: () => empty(`no charges in this range${fl.input.value.trim() ? ' that pass the filter' : ''}`, fl.input.value.trim() ? h('button', { class: 'btn', type: 'button', onclick: () => fl.set('') }, 'clear the filter') : `pick a longer range than ${period.label.toLowerCase()}`),
  });

  /* ---------- charges with missing data */
  const incomplete = table(ctx, {
    title: 'charges with missing data', path: listPath, params: { ...base(), flags: 'gap,in_progress' }, limit: 20, csvName: 'charges-missing-data',
    note: 'charges over a gap in the logs, or still under way. their figures cover what was recorded; late logs fill them in by themselves, and an admin can rebuild the range from the stored logs.',
    noteCover: 'charges.text.incomplete_help', cover: 'charges.incomplete.table',
    side: isAdmin() ? h('button', { class: 'btn btn--line tone--accent', type: 'button', onclick: rebuild, title: 'read the stored logs again for this range' }, '[rebuild]') : null,
    rowHref: chargeHref,
    columns: [
      multi ? { key: 'car', label: 'car', ph: 'sub', render: (c) => carName(c.car) } : null,
      { key: 'id', label: 'charge', ph: 'main', cover: 'charges.incomplete.col.id', render: (c) => h('a', { class: 'lnk-plain', href: chargeHref(c) }, c.id) },
      { key: 'start', label: 'start', ph: 'sub', cover: 'charges.incomplete.col.start_date', render: (c) => `${date(c.start_ms, c.time_zone || tz, { year: false })} ${formatTime(c.start_ms, c.time_zone || tz)}` },
      { key: 'end', label: 'last recorded', ph: 'sub', cover: 'charges.incomplete.col.end_date', render: (c) => (c.end_ms ? formatTime(c.end_ms, c.time_zone || tz) : 'still going') },
      { key: 'why', label: 'missing', cls: 'wrap-cell', render: (c) => (c.flags?.in_progress || c.end_ms == null ? 'under way; it ends when the next logs arrive' : `no data for ${formatDuration(c.flags?.gap_ms || 0)}`) },
      { key: 'added', label: 'added', num: true, ph: 'num', cover: 'charges.incomplete.col.energy_added', render: (c) => num(c.energy_added_kwh, 'energy') },
      { key: 'grid', label: 'used', num: true, cover: 'charges.incomplete.col.energy_used', render: (c) => (c.energy_grid_kwh != null ? num(c.energy_grid_kwh, 'energy') : c.kind === 'dc' ? none('dc') : null) },
      { key: 'soc_a', label: 'start', num: true, cover: 'charges.incomplete.col.start_soc', render: (c) => pct(c.start_soc_pct) },
      { key: 'soc_b', label: 'end', num: true, cover: 'charges.incomplete.col.end_soc', render: (c) => pct(c.end_soc_pct) },
      { key: 'duration', label: 'time', num: true, cover: 'charges.incomplete.col.duration', render: (c) => formatDuration(c.charging_ms) },
    ].filter(Boolean),
    empty: () => empty('no charge is missing data in this range'),
  });
  async function rebuild() {
    const ids = fixed ? [fixed.id] : [...new Set(incomplete.rows().map((c) => c.car))];
    if (!ids.length) { toast('No charge in this range is missing data.', { kind: 'info' }); return; }
    try {
      let logs = 0;
      for (const cid of ids) logs += (await ctx.api.post(`cars/${enc(cid)}/rebuild`, ctx.params))?.logs_read ?? 0;
      toast(`Rebuilt from ${formatNumber(logs)} stored logs.`);
      ctx.reload();
    } catch (e) { toast(`Could not rebuild: ${e.message}`, { kind: 'error' }); }
  }

  /* ---------- charging statistics */
  const scopeLine = h('p', { class: 'note' });
  const totalsEl = h('div');
  const splitEl = h('div');
  const habitsEl = h('div');
  const placesEl = h('div');
  const curveEl = h('div');
  let gen = 0;
  async function loadStats() {
    const my = ++gen;
    const sp = statsParams();
    replace(scopeLine, `statistics for ${scope}, ${period.label.toLowerCase()}${f.places || f.min_dur ? ', with the place and shortest-time filters' : ''}. of the filter keys only place and dur apply here.`);
    let s, rows;
    try {
      [s, rows] = await Promise.all([ctx.api.get(statsPath, sp), ctx.api.list(listPath, sp, { limit: 500 }).all({ max: 5000 })]);
    } catch (e) {
      if (my !== gen || !ctx.alive()) return;
      const why = h('p', { class: 'note' }, `-- the statistics could not be loaded: ${e.message} --`);
      replace(totalsEl, section({ title: 'costs and totals' }, why));
      replace(splitEl); replace(habitsEl);
      await Promise.all([paintPlaces(sp), paintCurve([], my)]);
      return;
    }
    if (my !== gen || !ctx.alive()) return;
    paintTotals(s);
    paintSplit(s);
    await paintHabits(s, rows);
    if (my !== gen || !ctx.alive()) return;
    await Promise.all([paintPlaces(sp), paintCurve(rows, my)]);
  }

  function paintTotals(s) {
    const ppk = s.price_per_kwh || {};
    const c100 = s.cost_per_100km || {};
    replace(totalsEl, section({ title: 'costs and totals' },
      kv([
        ['sessions', [formatNumber(s.count ?? 0), none(` ${formatNumber(ppk.priced ?? 0)} priced`)], { cover: 'cs.stat.count' }],
        ['energy added', s.count ? num(s.energy_added_kwh, 'energy') : none('no charge'), { cover: 'cs.stat.energy_added' }],
        ['total cost', money(s.cost_by_currency) || none('no charge has a price'), { cover: 'cs.stat.total_cost' }],
        ['supercharger', money(s.supercharger_cost) || none('no priced supercharger session'), { cover: 'cs.stat.supercharger_cost' }],
        ['per kwh', prices(ppk.all) || none('no priced charge'), { cover: 'cs.stat.cost_per_kwh' }],
        ['per kwh ac', prices(ppk.ac) || none('no priced ac charge'), { cover: 'cs.stat.cost_per_kwh_ac' }],
        ['per kwh dc', prices(ppk.dc) || none('no priced dc charge'), { cover: 'cs.stat.cost_per_kwh_dc' }],
        [`per 100 ${unitLabel('distance')}`, (c100.value || []).length ? h('span', { class: 'money' }, c100.value.map((x, i) => [i ? ' + ' : null, format({ amount: x.amount, currency: x.currency }, 'money_per_distance')]), c100.distance_km != null ? none(` over ${format(c100.distance_km, 'distance')}`) : null)
          : none(c100.distance_km ? 'no charge has a price' : 'no driving in the range'), { cover: 'cs.stat.cost_per_100' }],
      ]),
      note('unpriced charges are left out of the prices, not counted as free. ac prices are over the grid energy, dc over what the charger billed.')));
  }

  function paintSplit(s) {
    const bk = s.by_kind || {};
    const kinds = ['ac', 'dc', 'unknown'];
    const block = (k, title, cv, fmt) => {
      const total = kinds.reduce((a, x) => a + (bk[x]?.[k] || 0), 0);
      return h('div', { cover: cv }, h('h3', { class: 'sub-title' }, title, ' ', h('span', { class: 'tone--text' }, fmt(total))),
        h('div', { class: 'bars' }, kinds.map((x) => h('div', { class: 'bar-row' },
          h('span', { class: 'bar-k' }, tag(KIND[x], KIND_TONE[x]), ' ', fmt(bk[x]?.[k] || 0)),
          cellBar(total ? (bk[x]?.[k] || 0) / total : 0, 24, x === 'ac' ? 'accent' : x === 'dc' ? 'warn' : 'text'),
          h('span', { class: 'bar-v' }, total ? `${formatNumber(((bk[x]?.[k] || 0) / total) * 100, { maximumFractionDigits: 0 })} %` : '—')))));
    };
    replace(splitEl, section({ title: 'ac and dc', side: none('from the charge port’s own signal; ? is unknown, never dc') },
      h('div', { class: 'cols2' },
        block('kwh', 'energy added', 'cs.chart.ac_dc_energy', (v) => format(v, 'energy')),
        block('ms', 'time charging', 'cs.chart.ac_dc_duration', (v) => formatDuration(v)))));
  }

  let side = q.heat === 'end' ? 'end' : 'start';
  async function paintHabits(s, rowsIn) {
    const rows = rowsIn.filter((c) => c.start_soc_pct != null).sort((a, b) => a.start_ms - b.start_ms);
    const from = ctx.params.from_ms ?? (rows[0]?.start_ms ?? ctx.api.now());
    const to = Math.min(ctx.params.to_ms ?? ctx.api.now(), ctx.api.now() + 60000);
    const spanDays = (to - from) / 86400000;
    const unit = spanDays <= 62 ? 'day' : spanDays <= 730 ? 'week' : 'month';
    let cols = [];
    try { cols = buckets(from, to, unit, tz, { max: 400 }); } catch { cols = []; }
    const colIndex = new Map(cols.map((b, i) => [b.key, i]));
    const read = readout(rows.length ? `${formatNumber(rows.length)} sessions; hover a column or a bar` : 'no session in the range');
    const gridSlot = h('div');
    const weekSlot = h('div');
    let delta = null;
    const colText = (b) => (unit === 'month' ? formatDate(b.start_ms + 86400000, tz, { weekday: false, year: 'auto' }) : `${unit === 'week' ? 'week of ' : ''}${formatDate(b.start_ms + 3600000, tz, { weekday: false, year: false })}`);
    const showCol = (i, fromChart = false) => {
      if (i == null || !cols[i]) { read.show(null); return; }
      const inCol = rows.filter((c) => colIndex.get(bucketKey(side === 'start' ? c.start_ms : c.end_ms ?? c.start_ms, unit, tz)) === i);
      read.show([[unit, colText(cols[i])], ['sessions', formatNumber(inCol.length)], [side, inCol.map((c) => pct(side === 'start' ? c.start_soc_pct : c.end_soc_pct)).join(' ') || '—']]);
      if (!fromChart && delta?.u && inCol[0]) {
        const u = delta.u;
        const k = rows.indexOf(inCol[0]);
        if (k >= 0) u.setCursor({ left: u.valToPos(u.data[0][k], 'x'), top: u.valToPos(50, 'y') });
      }
    };
    const paintGrid = () => {
      const g = Array.from({ length: 10 }, () => new Array(cols.length).fill(0));
      for (const c of rows) {
        const v = side === 'start' ? c.start_soc_pct : c.end_soc_pct;
        if (v == null) continue;
        const i = colIndex.get(bucketKey(side === 'start' ? c.start_ms : c.end_ms ?? c.start_ms, unit, tz));
        if (i != null) g[Math.max(0, Math.min(9, Math.floor(v / 10)))][i]++;
      }
      const step = unit === 'day' ? 7 : unit === 'week' ? 4 : 3;
      replace(gridSlot, textGrid({
        rows: g.map((cells, r) => ({ label: `${r * 10}-${r * 10 + 10}%`, cells })).reverse(),
        axis: gridAxis(cols.length, step, (i) => formatDate(cols[i].start_ms + 3600000, tz, { weekday: false, year: false })),
        tone: side === 'start' ? 'accent' : 'warn', onCol: (i) => showCol(i), labelW: 8,
      }));
      const w = s.heatmap?.[side] || [];
      replace(weekSlot, textGrid({
        rows: DAYS.map((d, i) => ({ label: d, cells: w[i] || new Array(24).fill(0) })),
        axis: gridAxis(24, 6, (hr) => String(hr).padStart(2, '0')), tone: side === 'start' ? 'accent' : 'warn', labelW: 4,
        title: (r, c, n) => `${DAYS[r]} ${String(c).padStart(2, '0')}:00 ${n} ${n === 1 ? 'session' : 'sessions'}`,
      }));
    };
    const sideKeys = h('span', { class: 'keys', role: 'group', 'aria-label': 'start or end level' },
      [['start', 'start'], ['end', 'end']].map(([v, l]) => h('button', { class: 'key', type: 'button', 'aria-pressed': String(side === v),
        onclick: (e) => { side = v; ctx.setQuery({ heat: v === 'end' ? 'end' : null }); for (const b of sideKeys.children) b.setAttribute('aria-pressed', String(b === e.currentTarget)); paintGrid(); } }, l)));
    const bandList = (list, which) => {
      const max = Math.max(1, ...(list || []).map((b) => b.count));
      return h('div', { class: 'bars' }, (list || []).slice().reverse().map((b) => {
        const lo = parseInt(b.band, 10);
        const tone = which === 'end' ? (lo >= 80 ? 'warn' : 'accent') : (lo < 10 ? 'bad' : lo < 20 ? 'warn' : 'accent');
        return h('div', { class: 'bar-row' }, h('span', { class: 'bar-k' }, `${b.band} %`), cellBar(b.count / max, 24, tone), h('span', { class: 'bar-v' }, formatNumber(b.count)));
      }));
    };
    const deltaEl = h('div', { class: 'chart' });
    const periodBtn = h('button', { class: 'btn btn--line tone--accent', type: 'button', cover: ['cs.filter.heatmap_window', 'cs.filter.charge_delta_window'], title: 'these charts follow the range of the page',
      onclick: () => document.querySelector('.ph-side .ph-btn')?.click() }, `[range ${period.label.toLowerCase()}]`);
    const runs = table(ctx, {
      title: 'back-to-back charging', rows: s.runs || [], limit: 10, sort: { key: 'start', dir: 'desc' }, csv: false,
      note: 'two or more charges, each starting within a day of the one before: top-ups that belong together.',
      columns: [
        multi ? { key: 'car', label: 'car', ph: 'sub', render: (r) => carName(r.car) } : null,
        { key: 'start', label: 'first start', sort: true, ph: 'main', value: (r) => r.start_ms, render: (r) => `${date(r.start_ms, tz, { year: false })} ${formatTime(r.start_ms, tz)}` },
        { key: 'charges', label: 'charges', num: true, sort: true, ph: 'num', render: (r) => formatNumber(r.charges) },
        { key: 'min_start_soc_pct', label: 'lowest start', num: true, sort: true, ph: 'sub', render: (r) => pct(r.min_start_soc_pct) },
        { key: 'max_end_soc_pct', label: 'highest end', num: true, sort: true, ph: 'sub', render: (r) => pct(r.max_end_soc_pct) },
      ].filter(Boolean),
      empty: () => empty('no back-to-back charges in this range'),
    });
    replace(habitsEl, section({ title: 'charging habits', side: [sideKeys, periodBtn] },
      kv([
        ['ended above 80%', pct(s.share_above_80_pct)],
        ['started below 20%', pct(s.share_below_20_pct)],
        ['daily band', '20-80 %'],
      ]),
      h('div', { class: 'blk', cover: ['cs.chart.heatmap', 'cs.control.shared_crosshair'] },
        h('h3', { class: 'sub-title' }, `battery level at the ${side} of each session, by ${unit}`), gridSlot),
      h('div', { class: 'blk', cover: ['cs.chart.charge_delta', 'cs.chart.charge_delta.band'] },
        h('div', { class: 'chart-head' }, h('b', null, 'each session, start to end level'), h('span', null, '%'),
          h('span', { class: 'chart-key' }, h('span', { class: 'chart-key-i chart-key-i--1' }, '# ac'), h('span', { class: 'chart-key-i chart-key-i--2' }, '# dc'), h('span', { class: 'chart-key-i chart-key-i--3' }, '# ?'), h('span', { class: 'chart-key-i chart-key-i--4' }, '- - 20-80 % band'))),
        rows.length ? deltaEl : empty('no session in the range'), read.el, note('hover the grid or the chart: the other one points at the same session. click a bar to open its charge.')),
      h('div', { class: 'cols2' },
        h('div', { cover: ['cs.table.discharge_stats', 'cs.table.discharge_stats.soc', 'cs.table.discharge_stats.count'] },
          h('h3', { class: 'sub-title' }, 'level when plugging in'), bandList(s.start_bands, 'start'),
          h('p', { class: 'note', cover: 'cs.text.discharge_stats_help' }, 'how low the battery is when charging starts; each session counts once. below 10 % is red, below 20 % amber.')),
        h('div', { cover: ['cs.table.charge_stats', 'cs.table.charge_stats.soc', 'cs.table.charge_stats.count'] },
          h('h3', { class: 'sub-title' }, 'level when charging stops'), bandList(s.end_bands, 'end'),
          note('where sessions end; above the daily band (80 %) is amber.'))),
      h('div', { class: 'blk' }, h('h3', { class: 'sub-title' }, 'when in the week, by hour (each car’s own time)'), weekSlot),
      runs.el));
    paintGrid();
    if (!rows.length) return;
    try { await loadUPlot(); } catch { replace(deltaEl, h('p', { class: 'chart-note' }, '-- the chart could not be drawn --')); return; }
    if (!ctx.alive()) return;
    const xs = rows.map((c) => c.start_ms);
    const lo = rows.map((c) => c.start_soc_pct);
    const hi = rows.map((c) => c.end_soc_pct ?? c.start_soc_pct);
    delta = plot(deltaEl, ctx, ({ colors: c }) => ({
      padding: [rpx(6), 0, 0, 0],
      cursor: { x: true, y: false, points: { show: false }, drag: { x: false, y: false }, sync: { key: 'charges-habits', setSeries: false } },
      scales: { x: { time: true, range: () => [from, to] }, y: { range: () => [0, 100] } },
      axes: [timeAxis(tz), { ...valueAxis((v) => `${v}%`, { size: 48 }), splits: () => [0, 20, 40, 60, 80, 100] }],
      series: [{}, { stroke: 'transparent', width: 0, points: { show: false } }, { stroke: 'transparent', width: 0, points: { show: false } }],
      hooks: {
        drawAxes: [(u) => {
          const g = u.ctx; const { left, width } = u.bbox;
          const y20 = Math.round(u.valToPos(20, 'y', true)), y80 = Math.round(u.valToPos(80, 'y', true));
          g.save(); g.strokeStyle = c.muted; g.lineWidth = 1; g.setLineDash([rpx(4) * devicePixelRatio, rpx(4) * devicePixelRatio]);
          for (const y of [y20, y80]) { g.beginPath(); g.moveTo(left, y + 0.5); g.lineTo(left + width, y + 0.5); g.stroke(); }
          g.restore();
        }],
        draw: [(u) => {
          const g = u.ctx;
          const pr = devicePixelRatio || 1;
          const bw = Math.max(2, Math.round(Math.min(rpx(8) * pr, (u.bbox.width / Math.max(1, rows.length)) * 0.6)));
          const tone = { ac: c.c1, dc: c.c2, unknown: c.c3 };
          g.save();
          rows.forEach((r, i) => {
            const x = Math.round(u.valToPos(xs[i], 'x', true) - bw / 2);
            const y1 = Math.round(u.valToPos(lo[i], 'y', true)), y2 = Math.round(u.valToPos(hi[i], 'y', true));
            g.fillStyle = hexA(tone[r.kind] || c.c3, u.cursor.idx === i ? 1 : r.kind === 'ac' ? 0.85 : 0.6);
            g.fillRect(x, Math.min(y1, y2), bw, Math.max(2, Math.abs(y1 - y2)));
          });
          g.restore();
        }],
        setCursor: [(u) => {
          const i = u.cursor.idx;
          if (i == null || u.cursor.left < 0) { read.show(null); return; }
          const r = rows[i];
          read.show([multi ? ['car', carName(r.car)] : null, ['at', `${date(r.start_ms, r.time_zone || tz, { year: false })} ${formatTime(r.start_ms, r.time_zone || tz)}`], ['soc', `${pct(r.start_soc_pct)} -> ${pct(r.end_soc_pct)}`], ['type', KIND[r.kind] || '?'], ['place', r.place?.name || r.address?.short || '—']].filter(Boolean));
          u.redraw(false, false);
        }],
      },
    }), [xs, lo, hi], {
      onBuild: (u) => {
        u.over.classList.add('is-click');
        u.over.addEventListener('click', () => { const i = u.cursor.idx; if (i != null && rows[i]) ctx.navigate(chargeHref(rows[i])); });
      },
    });
  }

  async function paintPlaces(sp) {
    const pp = { cars: fixed ? fixed.id : sp.cars, from_ms: sp.from_ms, to_ms: sp.to_ms, places: sp.places, min_duration_min: sp.min_duration_min };
    const placeCell = (p) => h('a', { class: 'lnk-plain', href: p.place ? state.buildUrl('places', { query: { place: p.place } }) : state.buildUrl('places', { query: { lat: p.lat.toFixed(5), lon: p.lon.toFixed(5), new: 1 } }), title: p.place ? `edit the place ${p.label}` : 'make this spot a place' }, p.label);
    const placeCol = (cv) => ({ key: 'label', label: 'place', cls: 'wrap-cell', ph: 'main', cover: cv, render: placeCell });
    const sessions = { key: 'sessions', label: 'sessions', num: true, ph: 'sub', render: (p) => formatNumber(p.sessions) };
    const byEnergy = table(ctx, {
      title: 'by energy', path: 'charging-stats/places', params: { ...pp, sort: 'energy' }, limit: 10, csvName: 'charging-places-energy', cover: 'cs.table.top_energy',
      columns: [placeCol('cs.table.top_energy.location'), sessions, { key: 'energy', label: 'added', num: true, ph: 'num', cover: 'cs.table.top_energy.charged', render: (p) => num(p.energy_added_kwh, 'energy') }],
      empty: () => empty('no charge in this range'),
    });
    const byCost = table(ctx, {
      title: 'by cost', path: 'charging-stats/places', params: { ...pp, sort: 'cost' }, limit: 10, csvName: 'charging-places-cost', cover: 'cs.table.top_cost',
      columns: [placeCol('cs.table.top_cost.location'), sessions,
        { key: 'cost', label: 'cost', num: true, ph: 'num', cover: 'cs.table.top_cost.cost', render: (p) => money(p.cost_by_currency) || none('no price') },
        { key: 'ppk', label: '/kwh', num: true, render: (p) => (p.price_per_kwh != null && p.cost_by_currency?.length === 1 ? format({ amount: p.price_per_kwh, currency: p.cost_by_currency[0].currency }, 'price_energy') : null) }],
      empty: () => empty('no charge in this range'),
    });
    const mapEl = h('div', { class: 'map map--sm', role: 'img', 'aria-label': 'Map of the charging places' });
    const mapRead = readout('bigger squares took more energy; click one to filter by it');
    replace(placesEl, section({ title: 'charging places' },
      h('div', { class: 'cols2' },
        h('div', { cover: 'cs.map.energy_by_location' }, mapEl, mapRead.el, note('grouped by saved place, else by position, never by address text.')),
        h('div', null, byEnergy.el, byCost.el))));
    let pts = [];
    try { pts = (await ctx.api.page('charging-stats/places', { ...pp, sort: 'energy' }, { limit: 200 })).items || []; } catch { pts = []; }
    if (!ctx.alive()) return;
    const mapping = (async () => {
      if (!pts.length) { mapEl.append(h('p', { class: 'map-note' }, '-- no charge in this range, nothing to map --')); return; }
      const m = await safeMap(mapEl, ctx, { center: [pts[0].lon, pts[0].lat], zoom: 10 });
      if (!m || !ctx.alive()) return;
      const max = Math.max(1, ...pts.map((p) => p.energy_added_kwh || 0));
      m.points('cplaces', pts.map((p, i) => ({ lon: p.lon, lat: p.lat, props: { i, e: p.energy_added_kwh || 0 } })), { token: '--warn', size: 16, sizeBy: { prop: 'e', min: 0, max } });
      m.map.on('mousemove', 'cplaces', (e) => {
        const p = pts[e.features?.[0]?.properties?.i];
        if (p) mapRead.show([['place', p.label], ['added', format(p.energy_added_kwh, 'energy')], ['sessions', formatNumber(p.sessions)]]);
      });
      m.map.on('mouseleave', 'cplaces', () => mapRead.show(null));
      m.map.on('click', 'cplaces', (e) => {
        const p = pts[e.features?.[0]?.properties?.i];
        if (!p) return;
        if (p.place) fl.set(`${fl.input.value.replace(/\bplace:\S+/g, '').trim()} place:${(places.find((x) => x.id === p.place)?.name || p.place).replace(/\s+/g, '')}`.trim());
        else ctx.navigate(state.buildUrl('places', { query: { lat: p.lat.toFixed(5), lon: p.lon.toFixed(5), new: 1 } }));
      });
      m.fit(pts.map((p) => [p.lon, p.lat]), { pad: 32, maxZoom: 14 });
    })();
    await Promise.all([byEnergy.ready, byCost.ready, mapping]);
  }

  let curveCar = q.curve_car && chosen.includes(q.curve_car) ? q.curve_car : null;
  async function paintCurve(rows, my) {
    const dcCount = new Map();
    for (const c of rows) if (c.kind === 'dc') dcCount.set(c.car, (dcCount.get(c.car) || 0) + 1);
    if (!curveCar) curveCar = fixed ? fixed.id : ([...dcCount.entries()].sort((a, b) => b[1] - a[1])[0]?.[0] || chosen[0]);
    const carKeys = multi && chosen.length > 1 ? h('span', { class: 'keys' }, chosen.map((cid) => h('button', { class: 'key', type: 'button', 'aria-pressed': String(cid === curveCar),
      onclick: () => { curveCar = cid; ctx.setQuery({ curve_car: cid }); paintCurve(rows, gen); } }, `${carName(cid)} ${formatNumber(dcCount.get(cid) || 0)}`))) : null;
    const plotEl = h('div', { class: 'chart' });
    const read = readout('one dot per battery percent of each dc session; the line is the median. click a dot to open its charge.');
    replace(curveEl, section({ title: 'fast-charging curve', side: carKeys },
      h('div', { class: 'blk', cover: ['cs.chart.dc_curve_sessions', 'cs.chart.dc_curve_median', 'cs.link.dc_curve_details'] },
        h('div', { class: 'chart-head' }, h('b', null, 'power against battery level'), h('span', null, 'kW'),
          h('span', { class: 'chart-key' }, h('span', { class: 'chart-key-i chart-key-i--2' }, '. sessions'), h('span', { class: 'chart-key-i chart-key-i--1' }, '-- median'))),
        plotEl, read.el)));
    let data;
    try { data = await ctx.api.get(`cars/${enc(curveCar)}/charging-stats/dc-curve`, { from_ms: ctx.params.from_ms, to_ms: ctx.params.to_ms, places: f.places || null }); } catch (e) {
      replace(plotEl, h('p', { class: 'chart-note' }, `-- the curve could not be loaded: ${e.message} --`)); return;
    }
    if (my !== gen || !ctx.alive()) return;
    const sessions = data.sessions || [];
    if (!sessions.length) { replace(plotEl, empty(`${carName(curveCar)} did not fast-charge in this range`)); plotEl.classList.remove('chart'); return; }
    try { await loadUPlot(); } catch { replace(plotEl, h('p', { class: 'chart-note' }, '-- the chart could not be drawn --')); return; }
    if (!ctx.alive()) return;
    const xs = Array.from({ length: 101 }, (_, i) => i);
    const ys = sessions.map((s) => { const a = new Array(101).fill(null); for (const p of s.points) if (p.soc_pct >= 0 && p.soc_pct <= 100) a[Math.round(p.soc_pct)] = p.power_kw; return a; });
    const med = new Array(101).fill(null);
    for (const p of data.median || []) if (p.soc_pct >= 0 && p.soc_pct <= 100) med[Math.round(p.soc_pct)] = p.power_kw;
    let hover = null;
    plot(plotEl, ctx, ({ colors: c }) => ({
      padding: [rpx(6), 0, 0, 0],
      cursor: { x: true, y: true, points: { show: false }, drag: { x: false, y: false } },
      scales: { x: { time: false, range: () => [0, 100] }, y: { range: (u, a, b) => [0, Math.max(10, (b || 0) * 1.08)] } },
      axes: [{ ...valueAxis((v) => `${v}%`, { scale: 'x', side: 2, size: 28 }), space: rpx(64) }, valueAxis((v) => formatNumber(v), { size: 48 })],
      series: [{}, ...ys.map(() => ({ stroke: hexA(c.c2, 0.6), width: 0, paths: () => null, points: { show: true, size: rpx(4), width: 0, fill: hexA(c.c2, 0.55), stroke: hexA(c.c2, 0.55) } })),
        { ...line('median', 0, { width: 2 }), dash: undefined, spanGaps: true }],
      hooks: {
        setCursor: [(u) => {
          const i = u.cursor.idx;
          if (i == null || u.cursor.left < 0) { hover = null; read.show(null); return; }
          const y = u.posToVal(u.cursor.top, 'y');
          let best = null, bd = Infinity;
          ys.forEach((a, k) => { if (a[i] != null && Math.abs(a[i] - y) < bd) { bd = Math.abs(a[i] - y); best = k; } });
          hover = best;
          const s = best != null ? sessions[best] : null;
          read.show([['soc', `${i}%`], ['median', med[i] != null ? format(med[i], 'power') : '—'], s ? ['session', `${date(s.start_ms, data.time_zone || tz, { year: false })} ${s.label || ''}`] : null, s ? ['power', format(ys[best][i], 'power')] : null].filter(Boolean));
        }],
      },
    }), [xs, ...ys, med], {
      onBuild: (u) => {
        u.over.classList.add('is-click');
        u.over.addEventListener('click', () => { if (hover != null) ctx.navigate(state.buildUrl('charge', { car: curveCar, charge: sessions[hover].charge })); });
      },
    });
  }

  /* ---------- the page */
  const hd = head(ctx, { title: 'Charges', extra: multi ? h('span', { class: 'ph-extra muted' }, scope) : null });
  const btns = hd.querySelectorAll('.ph-btn');
  if (btns[0]) btns[0].dataset.cover = 'charges.filter.time_range cs.filter.time_range';
  const carLine = multi
    ? h('p', { class: 'note filt-note', cover: ['charges.filter.car', 'cs.filter.car'] }, 'cars: ', h('span', { class: 'keys keys--inline' },
      h('a', { class: 'key', href: state.buildUrl('charges', { period: state.current().period, query: { cars: 'all' } }), 'aria-pressed': String(!q.cars || q.cars === 'all') }, 'all'),
      fleet.map((c) => h('a', { class: 'key', href: state.buildUrl('charges', { car: c.id, period: state.current().period }) }, c.name))))
    : null;
  if (!multi && btns[1]) btns[1].dataset.cover = 'charges.filter.car cs.filter.car';
  replace(ctx.view, h('div', { class: 'page', dataset: { page: 'charges' }, cover: ['shots.charges', 'shots.charging-stats'] },
    hd, carLine, fl.el,
    h('p', { class: 'note filt-note' }, fixed ? link(state.buildUrl('charges', { period: state.current().period, query: { cars: 'all' } }), 'every car at once') : null, fixed ? '  ' : null,
      'kinds: ', tag('ac', 'accent'), ' the car’s charger  ', tag('dc', 'warn'), ' a fast charger  ', tag('?', 'muted'), ' not said'),
    summary, list.el, incomplete.el,
    section({ title: 'charging statistics', cls: 'sec--group' }, scopeLine),
    totalsEl, splitEl, habitsEl, placesEl, curveEl));
  paintSummary(null, 'loading');

  try { await Promise.all([list.ready, incomplete.ready, loadStats()]); } catch (e) { ctx.fail(e); return; }
  await Promise.race([Promise.all(sparkJobs), new Promise((r) => setTimeout(r, 4000))]);
  if (!ctx.alive()) return;
  ctx.ready();

  let rowsT = 0;
  ctx.events('rows', (ev) => {
    if (ev.data?.car && !chosen.includes(ev.data.car)) return;
    if (ev.data?.kinds && !ev.data.kinds.includes('charges')) return;
    clearTimeout(rowsT);
    rowsT = setTimeout(() => { if (!ctx.alive()) return; list.reload(listParams(), { quiet: true }); incomplete.reload(null, { quiet: true }); loadStats(); }, 2000);
  });
  ctx.onCleanup(() => clearTimeout(rowsT));
}
