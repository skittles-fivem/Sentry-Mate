// Efficiency and temperatures (/cars/{car}/efficiency): how much energy driving takes and what the weather
// does to it, read like an economy gauge. GET .../efficiency gives net (driving only, the shortest-drive
// filter applies) and gross (every drive plus parked energy) consumption, the groups by outside temperature
// and the energy factor (energy per km of rated or ideal range). A group opens its drives (GET .../drives).
// One other car can ride along (GET /compare, wh_per_km). Temperatures: GET .../temperatures (lowest,
// average, highest, coverage, the days) and GET .../series (outside, cabin, battery, the climate set point);
// the charges' outside temperature from GET .../charges. The three time charts share one cursor.

import * as state from '../shared/state.js';
import { h, replace, cssVar } from '../shared/dom.js';
import { column, timeColumn } from '../shared/csv.js';
import { format, convert, toMetric, unitLabel, getUnits } from '../shared/units.js';
import { formatDate, formatDateTime, DAY } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon, hud, plate, btn, seg, note, tag, bar } from '../core/ui.js';
import { controls, slot, periodBtn, carBtn, select, counters, csvBtn, band, plural, route } from '../core/kit.js';
import { dial } from '../core/gauges.js';
import { tracks } from '../core/tracks.js';
import { loadUPlot, mountPlot, scope, timeAxis, valueAxis, tone, alpha, well } from '../core/chart.js';
import { rpx } from '../core/display.js';
import { emptyState, errorState } from '../core/states.js';
import { srcChip } from '../core/sources.js';

export const id = 'efficiency';
export const css = true;
export const title = () => 'Efficiency and temperatures';

const enc = encodeURIComponent;
const DIST = [0, 1, 5, 10, 25];
const TEMPW = { c: [2, 5, 10], f: [5, 10, 20] };
const r3 = (v) => Math.round(v * 1000) / 1000;
const SERIES = {
  outside: { label: 'Outside', color: '--org', cover: 'tp.chart.outside' },
  cabin: { label: 'Cabin', color: '--cyan', cover: 'tp.chart.inside' },
  battery: { label: 'Battery', color: '--mag', cover: null },
  setpoint: { label: 'Set point (climate on)', color: '--violet', cover: 'tp.chart.setpoint' },
};

export async function mount(ctx) {
  const { car, tz, period } = ctx;
  const cid = car.id, base = `cars/${enc(cid)}`, q = ctx.query;
  const now = () => ctx.api.now();
  const fahr = getUnits().temperature === 'f';
  const rangeWord = getUnits().range === 'ideal' ? 'ideal' : 'rated';
  const opt = {
    minD: DIST.includes(Number(q.min_d)) ? Number(q.min_d) : 1,
    tgW: TEMPW[fahr ? 'f' : 'c'].includes(Number(q.tg_w)) ? Number(q.tg_w) : fahr ? 10 : 5,
    group: q.group != null && q.group !== '' && Number.isFinite(Number(q.group)) ? Number(q.group) : null,
    other: q.cars ? String(q.cars).split(',').find((x) => x && x !== cid) || null : null,
  };
  const groupC = () => r3(toMetric(opt.tgW, 'temperature_delta'));
  const effParams = () => ({ ...ctx.params, min_distance_km: r3(toMetric(opt.minD, 'distance')), temp_group_c: groupC() });
  const inGroup = (x, g) => x.outside_avg_c != null && Math.floor(x.outside_avg_c / groupC()) === Math.round(g.temp_c / groupC());

  const [eff, temps, fleet] = await Promise.all([
    ctx.api.get(`${base}/efficiency`, effParams()),
    ctx.api.get(`${base}/temperatures`, ctx.params).catch((e) => ({ error: e })),
    ctx.api.page('cars', { sort: 'name' }, { limit: 100 }).then((p) => p.items || []).catch(() => []),
  ]);
  if (!ctx.alive()) return;
  const dataStart = temps.daily?.[0]?.t_ms ?? null;
  const fromMs = period.from_ms ?? dataStart ?? now() - 30 * DAY;
  const toMs = Math.min(period.to_ms ?? now(), now());
  const sync = `ef-${cid}`;

  /* ---------- the control deck */
  const minText = () => format(toMetric(opt.minD, 'distance'), 'distance', { digits: 0 });
  const segMin = seg(DIST.map((v) => [v, v ? `${v} ${unitLabel('distance')}` : 'Any']), opt.minD, (v) => { opt.minD = v; opt.group = null; ctx.setQuery({ min_d: v === 1 ? null : String(v), group: null }); refresh(); }, { label: 'Shortest drive' });
  const segT = seg(TEMPW[fahr ? 'f' : 'c'].map((v) => [v, `${v} ${unitLabel('temperature')}`]), opt.tgW, (v) => { opt.tgW = v; opt.group = null; ctx.setQuery({ tg_w: v === (fahr ? 10 : 5) ? null : String(v), group: null }); refresh(); }, { label: 'Temperature groups' });
  const others = fleet.filter((c) => c.id !== cid);
  const cmpSel = select([['', others.length ? 'No other car' : 'Only one car'], ...others.map((c) => [c.id, c.name])], opt.other || '', (v) => { opt.other = v || null; ctx.setQuery({ cars: opt.other }); paintCompare(); }, { label: 'Compare with another car' });
  if (!others.length) cmpSel.disabled = true;
  const deck = controls({ note: 'Drives count when they start in the period. The shortest drive applies to net consumption, logged distance, the groups and the comparison; gross consumption and the temperatures take everything.' },
    slot('Period', periodBtn(ctx, ['ef.filter.time_range', 'tp.filter.time_range'])),
    slot('Shortest drive', segMin.el, { cover: 'ef.filter.min_distance' }),
    slot('Temperature groups', segT.el),
    slot('Compare with', cmpSel, { cover: 'ef.section.per_car' }));

  /* ---------- consumption: the economy cluster */
  const consEl = h('div');
  function paintConsumption(d) {
    const why = d.drives ? 'Too little distance' : `No drive of at least ${minText()}`;
    const all = d.all_drives || { drives: d.drives, distance_km: d.distance_km, energy_kwh: d.energy_kwh };
    const used = (all.energy_kwh || 0) + (d.parked_kwh || 0);
    const parkedShare = used > 0 ? (100 * (d.parked_kwh || 0)) / used : null;
    const fac = rangeWord === 'ideal' ? d.factor?.ideal_wh_per_km : d.factor?.rated_wh_per_km;
    const top = Math.max(250, (fac || 0) * 1.6, (d.gross_wh_per_km || 0) * 1.15, (d.net_wh_per_km || 0) * 1.15);
    replace(consEl,
      h('div', { class: 'ef-dials' },
        h('div', { class: 'ef-dial', cover: 'ef.stat.consumption_net' }, dial({ value: d.net_wh_per_km, max: top, kind: 'efficiency', label: 'Net', why, warm: fac && d.net_wh_per_km > fac }),
          h('p', { class: 'ef-dial-how' }, `Driving only: ${format(d.energy_kwh, 'energy')} from the pack, regen taken off, over ${format(d.distance_km, 'distance')}.`)),
        h('div', { class: 'ef-dial', cover: 'ef.stat.consumption_gross' }, dial({ value: d.gross_wh_per_km, max: top, kind: 'efficiency', label: 'Gross', why, warm: true }),
          h('p', { class: 'ef-dial-how' }, `Everything: all ${plural(all.drives || 0, 'drive')} plus ${format(d.parked_kwh, 'energy')} used while parked, over ${format(all.distance_km, 'distance')}.`))),
      counters([
        { label: 'Logged distance', value: d.distance_km, kind: 'distance', cover: 'ef.stat.logged_distance', how: `${plural(d.drives || 0, 'drive')} of at least ${minText()}` },
        { label: 'Parked share', value: parkedShare, kind: 'percent', how: 'of all energy used, while standing' },
        { label: `As rated`, value: fac, kind: 'efficiency', how: `one ${unitLabel('range')} of ${rangeWord} range` },
      ]),
      d.estimated_drives ? h('p', { class: 'note note--warn' }, icon('warn', 'i--sm'), h('span', null, `${plural(d.estimated_drives, 'drive')} of ${formatNumber(d.drives)} are imported history: their energy is worked out from range, not measured.`)) : null,
      h('div', { class: 'ef-help', cover: 'ef.help.headline_stats' },
        note(`Net shows how efficiently the car drives (drives of at least ${minText()}). Gross is what owning it costs per distance: every drive and all parked energy, so the shortest drive does not change it.`),
        h('p', { class: 'note', cover: 'faq.no-consumption' }, icon('check', 'i--sm'), h('span', null, `Nothing to wait for: consumption is measured from the first drive, with no charges needed to calibrate it. A blank dial means no drive of at least ${minText()} in the period.`))));
  }

  /* ---------- the energy factor */
  const factorEl = h('div');
  function paintFactor(d) {
    const f = d.factor || {};
    const owner = f.source === 'owner';
    const mine = rangeWord === 'ideal' ? f.ideal_wh_per_km : f.rated_wh_per_km;
    const rows = [
      { kind: 'Rated', value: f.rated_wh_per_km, readings: owner ? null : f.readings, cv: 'ef.table.derived_rated', vc: 'ef.rated.col.efficiency', cc: 'ef.rated.col.count' },
      { kind: 'Ideal', value: f.ideal_wh_per_km, readings: f.readings, cv: 'ef.table.derived_ideal', vc: 'ef.ideal.col.efficiency', cc: 'ef.ideal.col.count' },
    ];
    replace(factorEl,
      counters([{ label: `Energy per ${unitLabel('range')} of ${rangeWord} range`, value: mine, kind: 'efficiency', cover: 'ef.stat.current_factor', how: owner ? 'typed by the owner in Car settings' : f.as_of_ms ? `last reading ${formatDateTime(f.as_of_ms, tz, { now: now() })}` : f.readings ? `median of ${plural(f.readings, 'reading')}` : 'not worked out yet' }]),
      h('table', { class: 'ef-ftab', 'aria-label': 'Energy factor by range kind' },
        h('thead', null, h('tr', null, h('th', { scope: 'col' }, 'Range kind'), h('th', { scope: 'col', class: 'is-num' }, `Per ${unitLabel('range')}`), h('th', { scope: 'col', class: 'is-num' }, 'Readings'))),
        h('tbody', null, rows.map((r) => h('tr', { cover: r.cv, class: r.kind.toLowerCase() === rangeWord ? 'is-mine' : null },
          h('th', { scope: 'row' }, r.kind, r.kind.toLowerCase() === rangeWord ? [' ', tag('yours', 'cyan')] : null),
          h('td', { class: 'is-num', cover: r.vc }, format(r.value, 'efficiency', { digits: 1 })),
          h('td', { class: 'is-num', cover: r.cc }, r.readings == null ? '—' : formatNumber(r.readings)))))),
      note('A cross-check only: the car uses this to turn energy into the range it shows. Consumption is measured, so it never depends on it. Each reading is a second when the pack energy and the range were read together.'),
      h('a', { class: 'plc ef-set', href: state.buildUrl('settings-cars', { car: cid }) }, 'Type a value in Car settings'));
  }

  /* ---------- by outside temperature */
  const byTempSlot = h('div', { class: 'stack' });
  const groupSlot = h('div');
  let drivesCache = null;
  const groupLabel = (g) => `${formatNumber(convert(g.temp_c, 'temperature'), { maximumFractionDigits: 0 })} to ${formatNumber(convert(g.temp_to_c ?? g.temp_c + groupC(), 'temperature'), { maximumFractionDigits: 0 })} ${unitLabel('temperature')}`;
  const rangeEff = (g, f) => { const fac = rangeWord === 'ideal' ? f?.ideal_wh_per_km : f?.rated_wh_per_km; return fac && g.wh_per_km ? (fac / g.wh_per_km) * 100 : g.range_efficiency_pct ?? null; };
  function paintByTemp(d) {
    const groups = d.by_temperature || [];
    const maxWh = Math.max(1, ...groups.map((g) => g.wh_per_km || 0));
    const nt = d.no_temperature || {};
    const t = tracks(ctx, {
      title: 'By outside temperature', icon: 'thermo', tone: 'orange', rows: groups, cover: 'ef.table.temperature', csvName: 'efficiency-by-temperature', sort: { key: 'temp', dir: 'desc' }, dense: true,
      note: `Each drive goes in the group of its average outside temperature, in ${opt.tgW} ${unitLabel('temperature')} steps. "Of ${rangeWord} range" is the distance driven for each ${unitLabel('range')} of range used.`,
      columns: [
        { key: 'temp', label: 'Outside', sort: true, lead: true, value: (g) => g.temp_c, cover: 'ef.temp.col.temperature', render: (g) => h('span', { class: 'cell-main nowrap' }, groupLabel(g)), csv: (g) => groupLabel(g) },
        { key: 'drives', label: 'Drives', num: true, sort: true, value: (g) => g.drives, cls: 'is-act', render: (g) => btn(formatNumber(g.drives), () => openGroup(g, d), { sm: true, kind: opt.group === g.temp_c ? 'go' : 'ghost', icon: 'route', title: 'Show the drives in this group' }), csv: (g) => g.drives },
        { key: 'wh', label: 'Consumption', num: true, sort: true, value: (g) => g.wh_per_km, cover: 'ef.temp.col.consumption',
          render: (g) => h('span', { class: 'ef-whcell' }, h('span', null, format(g.wh_per_km, 'efficiency')), h('span', { class: 'ef-whbar', 'aria-hidden': 'true' }, h('i', { vars: { '--w': `${(((g.wh_per_km || 0) / maxWh) * 100).toFixed(1)}%` } }))), csv: column('Consumption', 'wh_per_km', 'efficiency') },
        { key: 'eff', label: `Of ${rangeWord} range`, num: true, sort: true, value: (g) => rangeEff(g, d.factor), cover: 'ef.temp.col.efficiency', render: (g) => { const v = rangeEff(g, d.factor); return h('span', { class: v == null ? null : v < 65 ? 'warn' : v >= 99 ? 'ok' : null }, format(v, 'percent')); }, csv: (g) => rangeEff(g, d.factor) },
        { key: 'dist', label: 'Distance', num: true, sort: true, value: (g) => g.distance_km, cover: 'ef.temp.col.distance', render: (g) => format(g.distance_km, 'distance'), csv: column('Distance', 'distance_km', 'distance') },
        { key: 'speed', label: 'Average speed', num: true, sort: true, value: (g) => g.avg_kph, cover: 'ef.temp.col.speed', render: (g) => format(g.avg_kph, 'speed'), csv: column('Average speed', 'avg_kph', 'speed') },
      ],
      empty: () => emptyState({ icon: 'thermo', title: 'No drives to group', text: d.drives ? 'None of the drives has an outside temperature.' : `No drive of at least ${minText()} started in this period. Pick a longer period or a shorter shortest drive.` }),
    });
    replace(byTempSlot, t.el, nt.drives ? note(`${plural(nt.drives, 'drive')} (${format(nt.distance_km, 'distance')}) had no outside temperature: counted above, in no group.`) : null);
  }
  function openGroup(g, d) {
    opt.group = opt.group === g.temp_c ? null : g.temp_c;
    ctx.setQuery({ group: opt.group == null ? null : String(opt.group) });
    paintByTemp(d);
    if (opt.group == null) replace(groupSlot); else paintGroup(d);
  }
  async function paintGroup(d) {
    const g = (d.by_temperature || []).find((x) => x.temp_c === opt.group);
    if (!g) { replace(groupSlot); return; }
    try {
      const key = JSON.stringify(effParams());
      if (!drivesCache || drivesCache.key !== key) drivesCache = { key, items: await ctx.api.list(`${base}/drives`, { ...ctx.params, min_distance_km: effParams().min_distance_km, sort: 'start', dir: 'desc' }, { limit: 200, max: 5000 }).all() };
      if (!ctx.alive() || opt.group !== g.temp_c) return;
      const rows = drivesCache.items.filter((x) => inGroup(x, g));
      const t = tracks(ctx, {
        title: `Drives at ${groupLabel(g)}`, icon: 'route', rows, sort: { key: 'start', dir: 'desc' }, csvName: 'drives-in-group', dense: true,
        side: btn('Close', () => openGroup(g, d), { sm: true, kind: 'ghost', icon: 'x' }),
        rowHref: (x) => state.buildUrl('drive', { car: cid, drive: x.id }),
        columns: [
          { key: 'start', label: 'Start', sort: true, lead: true, value: (x) => x.start_ms, render: (x) => formatDateTime(x.start_ms, tz, { now: now() }), csv: (x) => new Date(x.start_ms).toISOString() },
          { key: 'route', label: 'Route', cls: 'is-wide', render: (x) => route(x.start, x.end) },
          { key: 'temp', label: 'Outside', num: true, sort: true, value: (x) => x.outside_avg_c, render: (x) => format(x.outside_avg_c, 'temperature') },
          { key: 'dist', label: 'Distance', num: true, sort: true, value: (x) => x.distance_km, render: (x) => format(x.distance_km, 'distance') },
          { key: 'wh', label: 'Consumption', num: true, sort: true, value: (x) => x.wh_per_km, render: (x) => [format(x.wh_per_km, 'efficiency'), x.energy_basis === 'estimated' ? ' est.' : ''] },
          { key: 'speed', label: 'Average speed', num: true, sort: true, value: (x) => x.speed_avg_kph, render: (x) => format(x.speed_avg_kph, 'speed') },
        ],
        empty: () => emptyState({ icon: 'route', title: 'No drives in this group', text: 'The drives changed since the table was built. Load the page again.' }),
      });
      replace(groupSlot, t.el);
    } catch (e) { if (ctx.alive()) replace(groupSlot, errorState(e, { retry: () => paintGroup(d), home: false })); }
  }

  /* ---------- another car */
  const compareSlot = h('div');
  async function paintCompare() {
    if (!opt.other) { replace(compareSlot); return; }
    const other = fleet.find((c) => c.id === opt.other);
    if (!other) { replace(compareSlot); return; }
    const ids = [cid, other.id];
    const [effs, cmp] = await Promise.all([
      Promise.all(ids.map((i) => (i === cid ? Promise.resolve(current) : ctx.api.get(`cars/${enc(i)}/efficiency`, effParams()).catch(() => null)))),
      ctx.api.get('compare', { cars: ids.join(','), metric: 'wh_per_km', min_distance_km: effParams().min_distance_km, ...ctx.params }).catch((e) => ({ error: e })),
    ]);
    if (!ctx.alive() || opt.other !== other.id) return;
    const names = [car.name, other.name], colors = ['--cyan', '--mag'];
    const series = cmp.error ? [] : (cmp.series || []).filter((s) => ids.includes(s.car));
    const xs = [...new Set(series.flatMap((s) => s.t_ms || []))].sort((a, b) => a - b);
    const cols = ids.map((i) => { const s = series.find((x) => x.car === i); const m = new Map((s?.t_ms || []).map((t, k) => [t, s.values[k]])); return xs.map((t) => m.get(t) ?? null); });
    const chartEl = well('', 'Net consumption per day');
    const read = h('span', { class: 'scope-read' });
    replace(compareSlot, hud({ title: `${car.name} against ${other.name}`, icon: 'car', tone: 'cyan',
      side: csvBtn({ name: [car.name, 'efficiency-compare'], rows: () => xs.map((t, k) => ({ t_ms: t, a: cols[0][k], b: cols[1][k] })), columns: [timeColumn('Day', 't_ms', tz), column(names[0], 'a', 'efficiency'), column(names[1], 'b', 'efficiency')] }) },
    counters(ids.flatMap((i, k) => [
      { label: `${names[k]} net`, value: effs[k]?.net_wh_per_km, kind: 'efficiency', how: `${format(effs[k]?.distance_km, 'distance')} logged` },
      { label: `${names[k]} gross`, value: effs[k]?.gross_wh_per_km, kind: 'efficiency', how: `${format(effs[k]?.parked_kwh, 'energy')} used parked` },
    ])),
    h('div', { class: 'scope-head ef-cmp-head' }, h('span', { class: 'scope-title' }, `Net consumption per day · ${unitLabel('efficiency')}`), h('span', { class: 'keys' }, names.map((n, k) => h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': `var(${colors[k]})` } }), n))), read),
    cmp.error ? errorState(cmp.error, { retry: () => paintCompare(), home: false }) : xs.length ? chartEl : note(`No day in this period has drives of at least ${minText()} for both cars.`),
    note('Same period and shortest drive for both cars. A point is one day with drives (one week for periods over 120 days).')));
    if (cmp.error || !xs.length) return;
    await scope(chartEl, ctx, { tz, t_ms: xs, series: ids.map((i, k) => ({ label: names[k], values: cols[k], kind: 'efficiency', color: colors[k] })),
      onCursor: (i) => { read.textContent = i == null ? '' : `${formatDate(xs[i], tz, { now: now() })} · ${names.map((n, k) => `${n} ${format(cols[k][i], 'efficiency')}`).join(' · ')}`; } });
  }

  let current = eff;
  async function refresh() {
    try {
      const d = await ctx.api.get(`${base}/efficiency`, effParams());
      if (!ctx.alive()) return;
      current = d;
      paintConsumption(d); paintFactor(d); paintByTemp(d); replace(groupSlot);
      if (opt.other) paintCompare().catch(() => null);
    } catch (e) { if (ctx.alive()) replace(byTempSlot, errorState(e, { retry: () => refresh(), home: false })); }
  }

  /* ---------- temperatures */
  const sumEl = h('div');
  function paintTempSummary() {
    if (temps.error) { replace(sumEl, errorState(temps.error, { retry: () => ctx.reload(), home: false })); return; }
    const span = Math.max(1, toMs - fromMs);
    const miss = temps.missing_ms || {};
    const col = (key, label, cv, why) => {
      const t = temps[key] || {};
      const none = t.avg_c == null;
      return h('div', { class: 'ef-tcol' },
        h('h3', { class: 'ef-tcol-h' }, h('i', { class: 'key-sw', vars: { '--c': `var(${SERIES[key].color})` } }), label),
        h('div', { class: 'ef-tfigs' },
          h('span', { class: 'ef-tfig', cover: cv.min }, h('span', { class: 'stat-label' }, h('span', null, 'Lowest')), h('span', { class: 'ef-tval' }, format(t.min_c, 'temperature'))),
          h('span', { class: 'ef-tfig ef-tfig--avg', cover: cv.avg }, h('span', { class: 'stat-label' }, h('span', null, 'Average')), h('span', { class: 'ef-tval' }, none ? '—' : format(t.avg_c, 'temperature'))),
          h('span', { class: 'ef-tfig', cover: cv.max }, h('span', { class: 'stat-label' }, h('span', null, 'Highest')), h('span', { class: 'ef-tval' }, format(t.max_c, 'temperature')))),
        h('div', { class: 'ef-tcov', cover: cv.cov }, h('span', { class: 'stat-how' }, none ? why : `readings cover ${format(t.coverage_pct, 'percent', { digits: 1 })} of the period`), bar((t.coverage_pct ?? 0) / 100, { label: `${label}: share of the period with readings`, warm: (t.coverage_pct ?? 0) < 25 })));
    };
    replace(sumEl,
      h('div', { class: 'cols--3 cols ef-tcols' },
        col('outside', 'Outside', { min: 'tp.outside.min', avg: 'tp.outside.avg', max: 'tp.outside.max', cov: 'tp.outside.coverage' }, 'no reading in this period'),
        col('cabin', 'Cabin', { min: 'tp.inside.min', avg: 'tp.inside.avg', max: 'tp.inside.max', cov: 'tp.inside.coverage' }, 'not sent by this car, or it slept'),
        col('battery', 'Battery', {}, 'no reading in this period')),
      h('p', { class: 'ef-miss' }, `Asleep ${format((100 * (miss.asleep || 0)) / span, 'percent', { digits: 1 })} of the period (nothing is read), unknown ${format((100 * (miss.unknown || 0)) / span, 'percent', { digits: 1 })} (no log).`),
      h('p', { class: 'note', cover: 'tp.summary.help' }, icon('info', 'i--sm'), h('span', null, 'Lowest and highest are single readings. The average is the mean of each day\'s mean, so one long drive does not outweigh quiet days. Coverage is the share of the period the car was awake and reading.')));
  }

  const histEl = h('div', { class: 'stack' });
  const histSide = h('span');
  async function paintHistory() {
    const d = await ctx.api.get(`${base}/series`, { fields: 'outside_c,cabin_c,battery_c,setpoint_left_c,climate', ...ctx.params, from_ms: fromMs, to_ms: toMs, max_points: 800 });
    if (!ctx.alive()) return;
    const ts = d.t_ms || [], v = d.values || {};
    const on = (i) => v.climate?.[i] === 'on' || v.climate?.[i] === true;
    const raw = { outside: v.outside_c || [], cabin: v.cabin_c || [], battery: v.battery_c || [], setpoint: (v.setpoint_left_c || []).map((x, i) => (on(i) ? x : null)) };
    const keys = ['outside', 'cabin', 'battery', 'setpoint'];
    const read = h('span', { class: 'scope-read' });
    const box = well('well--lg', 'Temperatures over time');
    replace(histEl,
      h('div', { class: 'scope-head' }, h('span', { class: 'scope-title' }, `Over time · ${unitLabel('temperature')}`),
        h('span', { class: 'keys' }, keys.map((k) => h('span', { class: 'key', cover: SERIES[k].cover }, h('i', { class: 'key-sw', vars: { '--c': `var(${SERIES[k].color})` } }), SERIES[k].label))), read),
      ts.length && keys.some((k) => raw[k].some((x) => x != null)) ? box : emptyState({ icon: 'thermo', title: 'No temperature readings in this period', text: 'The car was asleep or not logged the whole time. Pick a longer period.' }),
      note(`Averaged per ${formatNumber((d.bucket_ms || 60000) / 60000)} min; the lines break where the car read nothing. The set point shows only while climate runs.`));
    replace(histSide, srcChip(d), csvBtn({ name: [car.name, 'temperatures'], rows: () => ts.map((t, i) => ({ t_ms: t, outside: raw.outside[i], cabin: raw.cabin[i], battery: raw.battery[i], setpoint: raw.setpoint[i] })),
      columns: [timeColumn('Time', 't_ms', tz), column('Outside', 'outside', 'temperature'), column('Cabin', 'cabin', 'temperature'), column('Battery', 'battery', 'temperature'), column('Set point', 'setpoint', 'temperature')] }));
    if (!box.isConnected) return;
    await scope(box, ctx, { tz, t_ms: ts, sync, series: keys.map((k) => ({ label: SERIES[k].label, values: raw[k], kind: 'temperature', color: SERIES[k].color, dash: k === 'setpoint' ? [5, 4] : null, width: k === 'outside' ? 2.5 : 1.75 })), axes: { y: { digits: 0 } },
      onCursor: (i) => { read.textContent = i == null ? '' : `${formatDateTime(ts[i], tz, { now: now() })} · ${keys.filter((k) => raw[k][i] != null).map((k) => `${SERIES[k].label} ${format(raw[k][i], 'temperature')}`).join(' · ') || 'no reading'}`; } });
  }

  const chgEl = h('div', { class: 'stack' });
  async function paintCharges() {
    const items = await ctx.api.list(`${base}/charges`, { ...ctx.params, sort: 'start', dir: 'asc' }, { limit: 200, max: 3000 }).all();
    if (!ctx.alive()) return;
    const list = items.filter((c) => c.outside_avg_c != null).sort((a, b) => a.start_ms - b.start_ms);
    if (!list.length) { replace(chgEl, h('div', { cover: 'tp.chart.charging_outside' }, emptyState({ icon: 'bolt', title: items.length ? 'No charge has an outside temperature' : 'No charge in this period', text: 'Charges that start in the period show here.' }))); return; }
    const box = well('', 'Outside temperature at each charge');
    const read = h('span', { class: 'scope-read' });
    const sv = list.map((c) => c.outside_avg_c);
    replace(chgEl,
      h('div', { class: 'scope-head' }, h('span', { class: 'scope-title' }, `${plural(list.length, 'charge')} · lowest ${format(Math.min(...sv), 'temperature')} · highest ${format(Math.max(...sv), 'temperature')}`), h('span'), read),
      h('div', { cover: 'tp.chart.charging_outside' }, box),
      note('One dot per charge at its start. Cold charges are slower and spend energy warming the pack. Click a dot to open its charge.'));
    await loadUPlot();
    if (!ctx.alive()) return;
    const xs = list.map((c) => c.start_ms / 1000), ys = list.map((c) => convert(c.outside_avg_c, 'temperature'));
    let hover = null;
    const make = (width, height) => {
      const c = tone(), col = cssVar('--grn');
      return {
        width, height, padding: [rpx(10), rpx(14), 0, 0], legend: { show: false },
        cursor: { sync: { key: sync, setSeries: false }, drag: { x: false, y: false }, points: { size: rpx(10), width: rpx(2), stroke: c.hub } },
        scales: { x: { time: true, min: fromMs / 1000, max: toMs / 1000 }, y: { range: (u, a, b) => [Math.floor(a - 2), Math.ceil(b + 2)] } },
        axes: [timeAxis(tz, { spanMs: toMs - fromMs }), valueAxis({ size: 44 })],
        series: [{}, { label: 'Outside', stroke: col, width: 0, paths: () => null, points: { show: true, size: rpx(8), width: rpx(1), fill: alpha(col, 0.75), stroke: col } }],
        hooks: { setCursor: [(u) => {
          const i = u.cursor.idx;
          if (i == null || u.cursor.left < 0 || Math.abs(u.valToPos(xs[i], 'x') - u.cursor.left) > rpx(20)) { hover = null; read.textContent = ''; return; }
          hover = i; const ch = list[i];
          read.textContent = `${formatDateTime(ch.start_ms, tz, { now: now() })} · ${ch.place?.name || ch.address?.short || 'Unnamed spot'} · ${format(ch.outside_avg_c, 'temperature')}`;
        }] },
      };
    };
    mountPlot(box, make, [xs, ys], ctx, (u) => u.over.addEventListener('click', () => { if (hover != null) ctx.navigate(state.buildUrl('charge', { car: cid, charge: list[hover].id })); }));
  }

  const dayEl = h('div', { class: 'stack' });
  const daySide = h('span');
  async function paintDaily() {
    if (temps.error) { replace(dayEl, errorState(temps.error, { retry: () => ctx.reload(), home: false })); return; }
    const days = temps.daily || [];
    const help = h('p', { class: 'note', cover: 'tp.chart.daily_envelope_help' }, icon('info', 'i--sm'), h('span', null, 'The thin line runs from the day\'s lowest to its highest reading; the box holds the middle 70 % of readings. The bars at the foot are how much of the day the car was awake: trust a day with a small bar less.'));
    if (!days.some((x) => x.max_c != null)) { replace(dayEl, h('div', { cover: ['tp.chart.daily_envelope', 'tp.chart.daily_coverage'] }, emptyState({ icon: 'calendar', title: 'No day with an outside reading', text: 'The car was asleep or not logged the whole period.' })), help); return; }
    const box = well('well--lg', 'Each day outside');
    const read = h('span', { class: 'scope-read' });
    replace(dayEl,
      h('div', { class: 'scope-head' }, h('span', { class: 'scope-title' }, `Each day outside · ${unitLabel('temperature')}`), h('span'), read),
      h('div', { cover: ['tp.chart.daily_envelope', 'tp.chart.daily_coverage'] }, box),
      help);
    replace(daySide, csvBtn({ name: [car.name, 'daily-outside'], rows: days, columns: [column('Day', 'date'), column('Lowest', 'min_c', 'temperature'), column('15th percentile', 'p15_c', 'temperature'), column('85th percentile', 'p85_c', 'temperature'), column('Highest', 'max_c', 'temperature'), column('Awake share', 'coverage_pct', 'percent')] }));
    await loadUPlot();
    if (!ctx.alive()) return;
    const xs = days.map((x) => (x.t_ms ?? Date.parse(`${x.date}T12:00:00Z`)) / 1000 + 43200);
    const T = (k) => days.map((x) => (x[k] == null ? null : convert(x[k], 'temperature')));
    const mn = T('min_c'), mx = T('max_c'), lo = T('p15_c'), hi = T('p85_c');
    const make = (width, height) => {
      const org = cssVar('--org'), cy = cssVar('--cyan');
      return {
        width, height, padding: [rpx(10), rpx(14), 0, 0], legend: { show: false },
        cursor: { sync: { key: sync, setSeries: false }, drag: { x: false, y: false }, points: { show: false } },
        scales: { x: { time: true, min: fromMs / 1000, max: toMs / 1000 }, y: { range: (u, a, b) => [Math.floor((a ?? 0) - 3), Math.ceil((b ?? 10) + 2)] } },
        axes: [timeAxis(tz, { spanMs: toMs - fromMs }), valueAxis({ size: 44 })],
        series: [{}, { stroke: 'transparent', points: { show: false } }, { stroke: 'transparent', points: { show: false } }],
        hooks: { draw: [(u) => {
          const g = u.ctx, pr = devicePixelRatio || 1;
          const dayW = Math.max(3 * pr, Math.min(16 * pr, (u.bbox.width / Math.max(1, days.length)) * 0.6));
          const { top, height: hh } = u.bbox;
          g.save();
          days.forEach((x, i) => {
            const px = Math.round(u.valToPos(xs[i], 'x', true));
            const cov = Math.max(0, Math.min(100, x.coverage_pct ?? 0)) / 100;
            g.fillStyle = alpha(cy, 0.35);
            const ch = Math.round(cov * hh * 0.12);
            g.fillRect(Math.round(px - dayW / 2), top + hh - ch, Math.round(dayW), ch);
            if (mx[i] == null) return;
            g.strokeStyle = alpha(org, 0.9); g.lineWidth = Math.max(1, Math.round(pr));
            g.beginPath(); g.moveTo(px + 0.5, Math.round(u.valToPos(mn[i], 'y', true))); g.lineTo(px + 0.5, Math.round(u.valToPos(mx[i], 'y', true))); g.stroke();
            if (lo[i] != null && hi[i] != null) {
              const y1 = Math.round(u.valToPos(hi[i], 'y', true)), y2 = Math.round(u.valToPos(lo[i], 'y', true));
              g.fillStyle = alpha(org, 0.75); g.fillRect(Math.round(px - dayW / 2), y1, Math.round(dayW), Math.max(Math.round(pr), y2 - y1));
            }
          });
          g.restore();
        }], setCursor: [(u) => {
          const i = u.cursor.idx;
          if (i == null || u.cursor.left < 0) { read.textContent = ''; return; }
          const x = days[i];
          read.textContent = x.max_c == null ? `${formatDate(xs[i] * 1000, tz, { now: now() })}: no reading` : `${formatDate(xs[i] * 1000, tz, { now: now() })}: ${format(x.min_c, 'temperature')} to ${format(x.max_c, 'temperature')} · awake ${format(x.coverage_pct, 'percent')}`;
        }] },
      };
    };
    mountPlot(box, make, [xs, mn, mx], ctx);
  }

  replace(ctx.view, h('div', { class: 'page ef' },
    plate({ kicker: `Energy · ${car.name}`, title: 'Efficiency', aim: ctx.info.aim, tools: [carBtn(ctx, 'tp.filter.car')] }),
    deck,
    h('div', { class: 'cols cols--wide ef-top', cover: 'shots.efficiency' },
      hud({ title: 'Economy', icon: 'leaf', tone: 'cyan' }, consEl),
      hud({ title: 'Energy factor', icon: 'gauge', side: csvBtn({ name: [car.name, 'energy-factor'], rows: () => [current.factor || {}], columns: [column('Rated', 'rated_wh_per_km', 'efficiency'), column('Ideal', 'ideal_wh_per_km', 'efficiency'), column('Readings', 'readings'), column('Source', 'source')] }) }, factorEl)),
    byTempSlot, groupSlot, compareSlot,
    h('div', { cover: 'tp.sections' }, band(1, 'Temperatures', h('span', { class: 'trk-count', cover: 'tp.control.shared_crosshair' }, 'the time charts share one cursor'))),
    h('div', { class: 'stack', cover: 'shots.temperatures' },
      hud({ title: 'Lowest, average, highest', icon: 'thermo', tone: 'orange' }, sumEl),
      hud({ title: 'Cabin, outside and battery', icon: 'timeline', side: histSide }, histEl),
      h('div', { class: 'cols' },
        hud({ title: 'Outside at each charge', icon: 'bolt' }, chgEl),
        hud({ title: 'Each day outside', icon: 'calendar', side: daySide }, dayEl)))));

  paintConsumption(eff); paintFactor(eff); paintByTemp(eff); paintTempSummary();
  if (opt.group != null) paintGroup(eff);
  await Promise.all([
    paintCompare().catch((e) => replace(compareSlot, errorState(e, { home: false }))),
    paintHistory().catch((e) => replace(histEl, errorState(e, { retry: () => paintHistory(), home: false }))),
    paintCharges().catch((e) => replace(chgEl, errorState(e, { retry: () => paintCharges(), home: false }))),
    paintDaily(),
  ]);
  if (ctx.alive()) ctx.ready();
}
