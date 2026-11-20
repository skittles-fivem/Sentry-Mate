// Efficiency (/cars/{car}/efficiency): how much energy driving takes and how the weather and speed change it,
// then the temperatures the car saw.
// - GET .../efficiency (range, min_distance_km, temp_group_c): consumption, the table by temperature band,
//   the energy factor. The speed bands are worked out here from the drives of the range (GET .../drives).
//   Energy is the pack's own, so consumption needs no factor; the factor is a cross-check only.
// - A temperature band opens the drives behind it (?group=<lower edge in °C>).
// - Other cars (?cars=c2,c3): GET .../efficiency for each and GET /compare?metric=wh_per_km for the daily line.
// - Temperatures: GET .../temperatures, GET .../series (outside, cabin, battery, set point while climate runs),
//   the charges of the range (outside temperature at each). The three time charts share one cursor.
// Asleep is not unknown: the car reads nothing while it sleeps; both are shaded, unknown darker.

import * as state from '../shared/state.js';
import { h, replace, cssVar } from '../shared/dom.js';
import { format, convert, toMetric, unitLabel, getUnits } from '../shared/units.js';
import { formatDateTime, formatDate, DAY } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { head, section, kv, note, link } from '../core/ui.js';
import { num, tag } from '../core/fmt.js';
import { table, cellBar } from '../core/table.js';
import { empty, loadingLine, errorBlock } from '../core/states.js';
import { loadUPlot, plot, line, timeAxis, valueAxis, cursor, readout, colors, hexA } from '../core/chart.js';
import { rpx } from '../core/display.js';

export const id = 'efficiency';
export const title = () => 'Efficiency';

const enc = encodeURIComponent;
const n0 = (v) => (v == null ? '—' : formatNumber(v, { maximumFractionDigits: 0 }));
const pct = (v, d = 0) => format(v, 'percent', { digits: d });
const none = (t) => h('span', { class: 'muted' }, t);
const DIST = [1, 5, 10, 25, 50];
const TSTEP = { c: [2, 5, 10], f: [5, 10, 20] };
const SSTEP = { km: 20, mi: 10 };
const round3 = (v) => (v == null ? null : Math.round(v * 1000) / 1000);

export async function mount(ctx) {
  const car = ctx.car, cid = car.id, tz = ctx.tz, base = `cars/${enc(cid)}`;
  const period = ctx.period, q = ctx.query;
  const now = () => ctx.api.now();
  const units = getUnits();
  const fahr = units.temperature === 'f';
  const rangeWord = units.range === 'ideal' ? 'ideal' : 'rated';
  const dUnit = unitLabel('distance'), tUnit = unitLabel('temperature');
  const opt = {
    minD: DIST.includes(Number(q.min_d)) ? Number(q.min_d) : 1,
    tgW: TSTEP[fahr ? 'f' : 'c'].includes(Number(q.tg_w)) ? Number(q.tg_w) : fahr ? 10 : 5,
    group: q.group != null && q.group !== '' && Number.isFinite(Number(q.group)) ? Number(q.group) : null,
    cars: (q.cars || '').split(',').map((s) => s.trim()).filter((s) => s && s !== cid).slice(0, 12),
  };
  const groupC = () => round3(toMetric(opt.tgW, 'temperature_delta'));
  const minKm = () => round3(toMetric(opt.minD, 'distance'));
  const effParams = () => ({ ...ctx.params, min_distance_km: minKm(), temp_group_c: groupC() });
  const inGroup = (x, g) => x.outside_avg_c != null && Math.floor(x.outside_avg_c / groupC()) === Math.round(g.temp_c / groupC());

  const [eff, temps, fleetPg] = await Promise.all([
    ctx.api.get(`${base}/efficiency`, effParams()),
    ctx.api.get(`${base}/temperatures`, ctx.params).catch((e) => ({ error: e })),
    ctx.api.page('cars', { sort: 'name' }, { limit: 100 }).catch(() => ({ items: [] })),
  ]);
  if (!ctx.alive()) return;
  const dataStart = temps.daily?.[0]?.t_ms ?? null;
  const fromMs = period.from_ms ?? (dataStart ?? (eff.from_ms || now() - 30 * DAY));
  const toMs = Math.min(eff.to_ms ?? period.to_ms ?? now(), now());
  const span = Math.max(1, toMs - fromMs);

  /* ---------- head and the choice lines */
  const hd = head(ctx);
  const [periodBtn, carBtn] = hd.querySelectorAll('.ph-btn');
  if (periodBtn) periodBtn.dataset.cover = 'ef.filter.time_range tp.filter.time_range';
  if (carBtn) carBtn.dataset.cover = 'tp.filter.car';
  const keyRow = (label, choices, cur, onPick, attrs = {}) => {
    const btns = choices.map(([v, l]) => h('button', { class: 'key', type: 'button', 'aria-pressed': String(v === cur), onclick: () => {
      btns.forEach((b, k) => b.setAttribute('aria-pressed', String(choices[k][0] === v))); onPick(v);
    } }, l));
    return h('div', { class: 'st-switch', ...attrs }, h('span', { class: 'muted' }, `${label}> `), h('span', { class: 'keys keys--inline', role: 'group', 'aria-label': label }, btns));
  };
  const minRow = keyRow('shortest', DIST.map((v) => [v, `${v} ${dUnit}`]), opt.minD, (v) => { opt.minD = v; opt.group = null; ctx.setQuery({ min_d: v === 1 ? null : String(v), group: null }); refresh(); },
    { cover: 'ef.filter.min_distance', title: 'drives shorter than this are left out of net consumption, the bands and the daily line' });
  const stepRow = keyRow('band', TSTEP[fahr ? 'f' : 'c'].map((v) => [v, `${v} ${tUnit}`]), opt.tgW, (v) => { opt.tgW = v; opt.group = null; ctx.setQuery({ tg_w: v === (fahr ? 10 : 5) ? null : String(v), group: null }); refresh(); });
  const others = (fleetPg.items || []).filter((c) => c.id !== cid);
  const carsRow = h('div', { class: 'st-switch', cover: 'ef.section.per_car' }, h('span', { class: 'muted' }, 'compare> '),
    others.length ? h('span', { class: 'keys keys--inline', role: 'group', 'aria-label': 'Cars to compare' }, others.map((c) => h('button', { class: 'key', type: 'button', 'aria-pressed': String(opt.cars.includes(c.id)),
      onclick: (e) => {
        const on = e.currentTarget.getAttribute('aria-pressed') !== 'true';
        e.currentTarget.setAttribute('aria-pressed', String(on));
        opt.cars = on ? [...new Set([...opt.cars, c.id])] : opt.cars.filter((x) => x !== c.id);
        ctx.setQuery({ cars: opt.cars.length ? opt.cars.join(',') : null });
        paintCompare().catch((err) => replace(compareSlot, errorBlock(err, { home: false })));
      } }, c.name))) : none('only one car'));
  const filterNote = note('drives count when they start in the range. the shortest-drive filter applies to net, logged distance, the bands and the daily line; gross and the parked share take every drive.');

  const consSlot = h('div');
  const tempSlot = h('div');
  const groupSlot = h('div');
  const speedSlot = h('div', null, loadingLine('loading drives'));
  const factorSlot = h('div');
  const compareSlot = h('div');
  const tSumSlot = h('div');
  const histSlot = h('div', null, loadingLine('loading temperatures'));
  const chgSlot = h('div');
  const daySlot = h('div');
  replace(ctx.view, h('div', { class: 'page', dataset: { page: 'efficiency' } },
    hd, minRow, stepRow, carsRow, filterNote,
    h('div', { cover: 'shots.efficiency' }, section({ title: 'consumption' }, consSlot), tempSlot, groupSlot, speedSlot, factorSlot), compareSlot,
    h('div', { cover: 'shots.temperatures' },
      section({ title: 'temperatures', cover: 'tp.sections', side: none('summary, over time, at each charge, each day') }, tSumSlot),
      section({ title: 'over time', cover: 'tp.control.shared_crosshair', side: none('the three charts share one cursor') }, histSlot),
      section({ title: 'outside at each charge' }, chgSlot),
      section({ title: 'each day outside' }, daySlot))));

  /* ---------- consumption */
  function paintCons(d) {
    const minText = format(toMetric(opt.minD, 'distance'), 'distance', { digits: 0 });
    const why = d.drives ? 'too little distance' : `no drive of at least ${minText}`;
    const all = d.all_drives || { drives: d.drives, distance_km: d.distance_km, energy_kwh: d.energy_kwh };
    const used = (all.energy_kwh || 0) + (d.parked_kwh || 0);
    const parked = used > 0 ? (100 * (d.parked_kwh || 0)) / used : null;
    replace(consSlot, kv([
      ['net', d.net_wh_per_km != null ? [num(d.net_wh_per_km, 'efficiency'), none(`  driving only, ${format(d.energy_kwh, 'energy')} over the distance`)] : none(why), { cover: 'ef.stat.consumption_net' }],
      ['gross', d.gross_wh_per_km != null ? [num(d.gross_wh_per_km, 'efficiency'), none(`  every drive plus ${format(d.parked_kwh, 'energy')} parked`)] : none(why), { cover: 'ef.stat.consumption_gross' }],
      ['logged distance', [num(d.distance_km, 'distance'), none(`  ${n0(d.drives)} drives of at least ${minText}`)], { cover: 'ef.stat.logged_distance' }],
      ['parked share', parked != null ? [cellBar(parked / 100, 20, 'warn'), ' ', pct(parked)] : none('no energy used')],
      d.estimated_drives ? ['estimated', [tag('warn', 'warn'), ` ${n0(d.estimated_drives)} of ${n0(d.drives)} drives are imported: their energy is worked out, not measured`], { tone: 'warn' }] : null,
    ]),
    h('p', { class: 'note', cover: 'ef.help.headline_stats' }, `net is how efficiently the car drives (drives of at least ${minText}). gross is what owning it costs per distance: every drive and all parked energy, so the filter does not change it.`),
    h('p', { class: 'note', cover: 'faq.no-consumption' }, `nothing to wait for: consumption is measured from the first drive, no charges needed to calibrate. a blank figure means no drive of at least ${minText} in the range.`));
  }

  /* ---------- by temperature */
  const bandLabel = (g) => {
    const lo = convert(g.temp_c, 'temperature'), hi = convert(g.temp_to_c ?? g.temp_c + groupC(), 'temperature');
    return `${formatNumber(lo, { maximumFractionDigits: 0 })}..${formatNumber(hi, { maximumFractionDigits: 0 })} ${tUnit}`;
  };
  const rangeEff = (g, f) => { const fac = rangeWord === 'ideal' ? f?.ideal_wh_per_km : f?.rated_wh_per_km; return fac && g.wh_per_km ? (fac / g.wh_per_km) * 100 : g.range_efficiency_pct ?? null; };
  function paintTemp(d) {
    const groups = d.by_temperature || [];
    const maxWh = Math.max(1, ...groups.map((g) => g.wh_per_km || 0));
    const loWh = 0.8 * Math.min(maxWh, ...groups.map((g) => g.wh_per_km || maxWh));   // bars start below the lowest, so the bands differ
    const nt = d.no_temperature || { drives: 0 };
    const t = table(ctx, {
      title: 'by outside temperature', rows: groups, csv: false, cover: 'ef.table.temperature', sort: { key: 'temp', dir: 'desc' }, label: 'Consumption by outside temperature',
      note: `bars run from 80% of the lowest band to the highest. each drive goes in the band of its average outside temperature, ${opt.tgW} ${tUnit} wide. "of ${rangeWord}" is distance driven per ${unitLabel('range')} of range used. [n] opens the drives.`,
      columns: [
        { key: 'temp', label: 'outside', sort: true, ph: 'main', value: (g) => g.temp_c, cover: 'ef.temp.col.temperature', render: bandLabel },
        { key: 'drives', label: 'drives', num: true, sort: true, value: (g) => g.drives,
          render: (g) => h('button', { class: ['btn', 'btn--line', opt.group === g.temp_c ? 'tone--accent' : null], type: 'button', 'aria-pressed': String(opt.group === g.temp_c), onclick: () => openGroup(g, d) }, `[${n0(g.drives)}]`) },
        { key: 'bar', label: '', grow: true, render: (g) => cellBar(((g.wh_per_km || loWh) - loWh) / (maxWh - loWh || 1), 20) },
        { key: 'wh', label: 'consumption', num: true, sort: true, ph: 'num', value: (g) => g.wh_per_km, cover: 'ef.temp.col.consumption', render: (g) => num(g.wh_per_km, 'efficiency') },
        { key: 'eff', label: `of ${rangeWord}`, num: true, sort: true, ph: 'sub', value: (g) => rangeEff(g, d.factor), cover: 'ef.temp.col.efficiency',
          render: (g) => { const v = rangeEff(g, d.factor); return h('span', { class: v == null ? null : v < 65 ? 'tone--bad' : v < 99 ? 'tone--warn' : 'tone--accent' }, pct(v)); } },
        { key: 'dist', label: 'distance', num: true, sort: true, value: (g) => g.distance_km, cover: 'ef.temp.col.distance', render: (g) => num(g.distance_km, 'distance') },
        { key: 'speed', label: 'avg speed', num: true, sort: true, value: (g) => g.avg_kph, cover: 'ef.temp.col.speed', render: (g) => num(g.avg_kph, 'speed') },
      ],
      empty: () => empty('no drives to group', d.drives ? 'none of the drives has an outside temperature' : 'pick a longer range or a shorter shortest drive'),
    });
    replace(tempSlot, t.el, nt.drives ? note(`${n0(nt.drives)} drives (${format(nt.distance_km, 'distance')}) had no outside temperature: in the figures above, in no band.`) : null);
  }
  let drivesCache = null;
  async function drivesOfRange() {
    const key = JSON.stringify(effParams());
    if (!drivesCache || drivesCache.key !== key) {
      const items = await ctx.api.list(`${base}/drives`, { ...ctx.params, min_distance_km: minKm(), sort: 'start', dir: 'desc' }, { limit: 200 }).all({ max: 5000 });
      drivesCache = { key, items };
    }
    return drivesCache.items;
  }
  function openGroup(g, d) {
    opt.group = opt.group === g.temp_c ? null : g.temp_c;
    ctx.setQuery({ group: opt.group == null ? null : String(opt.group) });
    paintTemp(d);
    paintGroup(d);
  }
  async function paintGroup(d) {
    const g = opt.group == null ? null : (d.by_temperature || []).find((x) => x.temp_c === opt.group);
    if (!g) { replace(groupSlot); return; }
    replace(groupSlot, loadingLine(`loading drives at ${bandLabel(g)}`));
    let items;
    try { items = await drivesOfRange(); } catch (e) { if (ctx.alive()) replace(groupSlot, errorBlock(e, { retry: () => paintGroup(d), home: false })); return; }
    if (!ctx.alive() || opt.group !== g.temp_c) return;
    const rows = items.filter((x) => inGroup(x, g));
    const t = table(ctx, {
      title: `drives at ${bandLabel(g)}`, rows, csv: false, sort: { key: 'start', dir: 'desc' },
      side: h('button', { class: 'btn btn--line', type: 'button', onclick: () => openGroup(g, d) }, '[close]'),
      rowHref: (x) => state.buildUrl('drive', { car: cid, drive: x.id }),
      columns: [
        { key: 'start', label: 'start', sort: true, ph: 'main', value: (x) => x.start_ms, render: (x) => formatDateTime(x.start_ms, tz, { now: now(), weekday: false }) },
        { key: 'route', label: 'from -> to', grow: true, ph: 'sub', render: (x) => `${x.start?.label || x.start?.place || 'a spot'} -> ${x.end?.label || x.end?.place || 'a spot'}` },
        { key: 'temp', label: 'outside', num: true, sort: true, value: (x) => x.outside_avg_c, render: (x) => num(x.outside_avg_c, 'temperature') },
        { key: 'dist', label: 'distance', num: true, sort: true, value: (x) => x.distance_km, render: (x) => num(x.distance_km, 'distance') },
        { key: 'wh', label: 'consumption', num: true, sort: true, ph: 'num', value: (x) => x.wh_per_km, render: (x) => [num(x.wh_per_km, 'efficiency'), x.energy_basis === 'estimated' ? [' ', tag('est', 'warn')] : null] },
        { key: 'speed', label: 'avg speed', num: true, sort: true, value: (x) => x.speed_avg_kph, render: (x) => num(x.speed_avg_kph, 'speed') },
      ],
      empty: () => empty('no drives in this band', 'the list changed since the table was built; reload'),
    });
    replace(groupSlot, t.el);
  }

  /* ---------- by speed (worked out here from the drives) */
  async function paintSpeed() {
    let items;
    try { items = await drivesOfRange(); } catch (e) { if (ctx.alive()) replace(speedSlot, errorBlock(e, { retry: () => paintSpeed(), home: false })); return; }
    if (!ctx.alive()) return;
    const stepU = SSTEP[dUnit === 'mi' ? 'mi' : 'km'];
    const stepKph = toMetric(stepU, 'speed');
    const bands = new Map();
    for (const x of items) {
      if (x.speed_avg_kph == null || x.wh_per_km == null || !x.distance_km) continue;
      const k = Math.floor(x.speed_avg_kph / stepKph);
      const b = bands.get(k) || { k, drives: 0, distance_km: 0, wh: 0, temp: 0, tn: 0 };
      b.drives++; b.distance_km += x.distance_km; b.wh += x.wh_per_km * x.distance_km;
      if (x.outside_avg_c != null) { b.temp += x.outside_avg_c * x.distance_km; b.tn += x.distance_km; }
      bands.set(k, b);
    }
    const rows = [...bands.values()].map((b) => ({ ...b, wh_per_km: b.wh / b.distance_km, temp_c: b.tn ? b.temp / b.tn : null }));
    const maxWh = Math.max(1, ...rows.map((r) => r.wh_per_km));
    const loWh = 0.8 * Math.min(maxWh, ...rows.map((r) => r.wh_per_km));
    const t = table(ctx, {
      title: 'by average speed', rows, csv: false, sort: { key: 'speed', dir: 'asc' },
      note: `each drive goes in the band of its average speed (stops included), ${stepU} ${unitLabel('speed')} wide; consumption is weighted by distance.`,
      columns: [
        { key: 'speed', label: 'avg speed', sort: true, ph: 'main', value: (r) => r.k, render: (r) => `${formatNumber(r.k * stepU)}..${formatNumber((r.k + 1) * stepU)} ${unitLabel('speed')}` },
        { key: 'drives', label: 'drives', num: true, sort: true, ph: 'sub', render: (r) => n0(r.drives) },
        { key: 'bar', label: '', grow: true, render: (r) => cellBar((r.wh_per_km - loWh) / (maxWh - loWh || 1), 20) },
        { key: 'wh', label: 'consumption', num: true, sort: true, ph: 'num', value: (r) => r.wh_per_km, render: (r) => num(r.wh_per_km, 'efficiency') },
        { key: 'dist', label: 'distance', num: true, sort: true, value: (r) => r.distance_km, render: (r) => num(r.distance_km, 'distance') },
        { key: 'temp', label: 'outside', num: true, sort: true, value: (r) => r.temp_c, render: (r) => num(r.temp_c, 'temperature') },
      ],
      empty: () => empty('no drives with a speed and energy in this range'),
    });
    replace(speedSlot, t.el);
  }

  /* ---------- energy factor */
  function paintFactor(d) {
    const f = d.factor || {};
    const mine = rangeWord === 'ideal' ? f.ideal_wh_per_km : f.rated_wh_per_km;
    const owner = f.source === 'owner';
    replace(factorSlot, section({ title: 'energy factor', side: link(state.buildUrl('settings-cars', { car: cid }), 'settings/cars') },
      kv([[`per ${unitLabel('range')} of ${rangeWord}`, mine != null ? [num(mine, 'efficiency', { digits: 1 }), none(owner ? '  typed by the owner' : '  worked out')] : none('not worked out yet'), { cover: 'ef.stat.current_factor' }]], { cols: 1 }),
      h('div', { class: 'tbl-scroll' }, h('table', { class: 'tbl', 'aria-label': 'Energy factor by range kind' },
        h('thead', null, h('tr', null, h('th', { scope: 'col' }, 'range'), h('th', { scope: 'col', class: 'num' }, `per ${unitLabel('range')}`), h('th', { scope: 'col', class: 'num' }, 'readings'), h('th', { scope: 'col', class: 'grow-col' }, 'from'))),
        h('tbody', null,
          [['rated', f.rated_wh_per_km, owner ? null : f.readings, owner ? 'typed by the owner' : 'energy and rated range in the same second', 'ef.table.derived_rated', 'ef.rated.col.efficiency', 'ef.rated.col.count'],
            ['ideal', f.ideal_wh_per_km, f.readings, 'energy and ideal range in the same second', 'ef.table.derived_ideal', 'ef.ideal.col.efficiency', 'ef.ideal.col.count']].map(([k, v, n, from, c1, c2, c3]) => h('tr', { cover: c1 },
            h('td', null, k, k === rangeWord ? none(' yours') : null), h('td', { class: 'num', cover: c2 }, num(v, 'efficiency', { digits: 1 })),
            h('td', { class: 'num', cover: c3 }, n == null ? '—' : n0(n)), h('td', { class: 'grow-col' }, none(from))))))),
      note(`a cross-check only: what one ${unitLabel('range')} of range on the display stands for. consumption above is measured and never uses it.${f.as_of_ms ? ` last reading ${formatDateTime(f.as_of_ms, tz, { now: now() })}.` : ''}`)));
  }

  /* ---------- compare cars */
  async function paintCompare() {
    if (!opt.cars.length) { replace(compareSlot); return; }
    const ids = [cid, ...opt.cars];
    replace(compareSlot, loadingLine('loading the other cars'));
    const cars = await Promise.all(ids.map((i) => (i === cid ? car : ctx.api.get(`cars/${enc(i)}`).catch(() => null))));
    if (!ctx.alive()) return;
    const seen = ids.filter((i, k) => cars[k]?.name);
    const [effs, cmp] = await Promise.all([
      Promise.all(ids.map((i, k) => (cars[k]?.name ? ctx.api.get(`cars/${enc(i)}/efficiency`, effParams()).catch(() => null) : null))),
      ctx.api.get('compare', { cars: seen.join(','), metric: 'wh_per_km', min_distance_km: minKm(), ...ctx.params }).catch((e) => ({ error: e })),
    ]);
    if (!ctx.alive()) return;
    const rows = ids.map((i, k) => ({ id: i, name: cars[k]?.name, e: effs[k], n: k })).filter((r) => r.name);
    const best = Math.min(...rows.map((r) => r.e?.net_wh_per_km ?? Infinity));
    const t = table(ctx, {
      title: 'compare cars', rows, csv: false, cover: 'ef.section.per_car',
      note: 'the same range and shortest drive for every car. the lowest net is marked.',
      columns: [
        { key: 'name', label: 'car', ph: 'main', grow: true, render: (r) => [h('span', { class: `chart-key-i chart-key-i--${(r.n % 3) + 1}` }, ['-- ', '- - ', '. . '][r.n % 3]), r.id === cid ? r.name : link(ctx.link('efficiency', { car: r.id }), r.name)] },
        { key: 'net', label: 'net', num: true, ph: 'num', render: (r) => [num(r.e?.net_wh_per_km, 'efficiency'), r.e?.net_wh_per_km === best && rows.length > 1 ? [' ', tag('lowest', 'accent')] : null] },
        { key: 'gross', label: 'gross', num: true, render: (r) => num(r.e?.gross_wh_per_km, 'efficiency') },
        { key: 'dist', label: 'logged', num: true, ph: 'sub', render: (r) => num(r.e?.distance_km, 'distance') },
        { key: 'drives', label: 'drives', num: true, render: (r) => n0(r.e?.drives) },
        { key: 'parked', label: 'parked', num: true, render: (r) => num(r.e?.parked_kwh, 'energy') },
      ],
    });
    const series = (cmp.error ? [] : cmp.series || []).filter((s) => rows.some((r) => r.id === s.car));
    const xsSet = new Set(); series.forEach((s) => (s.t_ms || []).forEach((x) => xsSet.add(x)));
    const xs = [...xsSet].sort((a, b) => a - b);
    const cols = series.map((s) => { const m = new Map((s.t_ms || []).map((x, i) => [x, s.values[i]])); return xs.map((x) => (m.get(x) == null ? null : convert(m.get(x), 'efficiency'))); });
    const box = h('div', { class: 'chart chart--pane' });
    const rd = readout('move over the chart for each car’s day');
    replace(compareSlot, t.el,
      h('div', { class: 'chart-head' }, h('b', null, 'net per day'), h('span', null, unitLabel('efficiency'))),
      cmp.error ? errorBlock(cmp.error, { retry: () => paintCompare(), home: false }) : xs.length ? [box, rd.el] : empty('no day with drives for these cars'));
    if (cmp.error || !xs.length) return;
    await loadUPlot().catch(() => null);
    if (!ctx.alive() || !window.uPlot) return;
    plot(box, ctx, () => ({
      padding: [rpx(6), rpx(8), 0, 0], cursor: { ...cursor(), drag: { x: false, y: false } }, select: { show: false },
      scales: { x: { time: true, min: fromMs, max: toMs }, y: { auto: true } },
      axes: [timeAxis(tz), valueAxis((v) => formatNumber(v))],
      series: [{}, ...series.map((s, k) => ({ ...line(s.name, k % 3, { width: 2 }), points: { show: true, size: rpx(4) } }))],
    }), [xs, ...cols], { onCursor: (i) => rd.show(i == null ? null : [['day', formatDate(xs[i], tz, { weekday: false })], ...series.map((s, k) => [s.name, cols[k][i] == null ? '—' : `${formatNumber(cols[k][i], { maximumFractionDigits: 0 })} ${unitLabel('efficiency')}`])]) });
  }

  async function refresh() {
    replace(groupSlot);
    try {
      const d = await ctx.api.get(`${base}/efficiency`, effParams());
      if (!ctx.alive()) return;
      paintCons(d); paintTemp(d); paintFactor(d);
      replace(speedSlot, loadingLine('loading drives'));
      paintSpeed();
      if (opt.cars.length) paintCompare().catch(() => null);
    } catch (e) { if (ctx.alive()) replace(tempSlot, errorBlock(e, { retry: () => refresh(), home: false })); }
  }

  /* ---------- temperatures */
  function paintTempSummary() {
    if (temps.error) { replace(tSumSlot, errorBlock(temps.error, { retry: () => ctx.reload(), home: false })); return; }
    const miss = temps.missing_ms || {};
    const asleepPct = (100 * (miss.asleep || 0)) / span, unknownPct = (100 * (miss.unknown || 0)) / span;
    const row = (k, label, cv) => ({ k, label, cv, ...(temps[k] || {}) });
    const rows = [row('outside', 'outside', { min: 'tp.outside.min', avg: 'tp.outside.avg', max: 'tp.outside.max', cov: 'tp.outside.coverage' }),
      row('cabin', 'cabin', { min: 'tp.inside.min', avg: 'tp.inside.avg', max: 'tp.inside.max', cov: 'tp.inside.coverage' }), row('battery', 'battery', {})];
    replace(tSumSlot,
      h('div', { class: 'tbl-scroll' }, h('table', { class: 'tbl tbl--plain', 'aria-label': 'Lowest, average and highest temperatures' },
        h('thead', null, h('tr', null, h('th', { scope: 'col' }, 'where'), ['lowest', 'average', 'highest'].map((l) => h('th', { scope: 'col', class: 'num' }, l)), h('th', { scope: 'col', class: 'grow-col' }, 'readings cover'))),
        h('tbody', null, rows.map((r) => h('tr', null, h('td', null, r.label),
          h('td', { class: 'num', cover: r.cv.min || null }, num(r.min_c, 'temperature')), h('td', { class: 'num', cover: r.cv.avg || null }, num(r.avg_c, 'temperature')),
          h('td', { class: 'num', cover: r.cv.max || null }, num(r.max_c, 'temperature')),
          h('td', { class: 'grow-col', cover: r.cv.cov || null }, cellBar((r.coverage_pct ?? 0) / 100, 20, (r.coverage_pct ?? 0) < 25 ? 'warn' : 'accent'), ' ', r.avg_c == null ? none('no reading') : pct(r.coverage_pct, 1))))))),
      h('p', { class: 'note' }, `asleep ${pct(asleepPct, 1)} (quiet car, nothing read)   unknown ${pct(unknownPct, unknownPct > 0 && unknownPct < 1 ? 2 : 1)} (no log, or a log that failed its check)`),
      h('p', { class: 'note', cover: 'tp.summary.help' }, 'lowest and highest are single readings; the average is the mean of each day’s mean, so one long drive does not outweigh quiet days. coverage is the share of the range the car was awake and reading.'));
  }

  const SYNC = `temps-${cid}`;
  const tempAxis = () => valueAxis((v) => `${formatNumber(v, { maximumFractionDigits: 0 })}°`, { size: 48 });
  function shade(ts, st, bms) {
    return (u) => {
      const cx = u.ctx, { left, top, width, height } = u.bbox;
      const m = cssVar('--muted');
      cx.save();
      for (let i = 0; i < ts.length; i++) {
        const s = st[i];
        if (s !== 'asleep' && s !== 'unknown') continue;
        let j = i; while (j + 1 < ts.length && st[j + 1] === s) j++;
        const a = Math.max(left, u.valToPos(ts[i], 'x', true)), b = Math.min(left + width, u.valToPos(ts[j] + bms, 'x', true));
        if (b > a) { cx.fillStyle = hexA(m, s === 'asleep' ? 0.06 : 0.2); cx.fillRect(a, top, Math.max(1, b - a), height); }
        i = j;
      }
      cx.restore();
    };
  }
  async function paintHistory() {
    const d = await ctx.api.get(`${base}/series`, { fields: 'outside_c,cabin_c,battery_c,setpoint_left_c,climate', ...ctx.params, from_ms: fromMs, to_ms: toMs, max_points: 800 });
    if (!ctx.alive()) return;
    const ts = d.t_ms || [], v = d.values || {}, st = d.state || [], bms = d.bucket_ms || 60000;
    const on = (i) => (v.climate || [])[i] === 'on' || (v.climate || [])[i] === true;
    const numOr = (x) => (typeof x === 'number' ? x : null);
    const raw = { outside: v.outside_c || [], cabin: v.cabin_c || [], battery: v.battery_c || [], setpoint: (v.setpoint_left_c || []).map((x, i) => (on(i) ? numOr(x) : null)) };
    if (!ts.length || !['outside', 'cabin', 'battery'].some((k) => raw[k].some((x) => x != null))) {
      replace(histSlot, h('div', { cover: ['tp.chart.outside', 'tp.chart.inside', 'tp.chart.setpoint'] }, empty('no temperature readings in this range', 'the car slept or was not logged; pick a longer range'))); return;
    }
    const keys = ['outside', 'cabin', 'battery', 'setpoint'];
    const conv = keys.map((k) => raw[k].map((x) => (x == null ? null : convert(x, 'temperature'))));
    const box = h('div', { class: 'chart' });
    const rd = readout('move over the chart for the values at a time');
    replace(histSlot,
      h('div', { class: 'chart-head' }, h('b', null, 'temperatures'), h('span', null, tUnit), h('span', { class: 'chart-key' },
        h('span', { class: 'chart-key-i chart-key-i--1', cover: 'tp.chart.outside' }, '-- outside'), h('span', { class: 'chart-key-i chart-key-i--2', cover: 'tp.chart.inside' }, '- - cabin'),
        h('span', { class: 'chart-key-i chart-key-i--3' }, '. . battery'), h('span', { class: 'chart-key-i muted', cover: 'tp.chart.setpoint' }, '_ set point'))),
      box, rd.el,
      note(`shaded: asleep (light) and unknown (darker) time; the lines break there instead of guessing. the set point is drawn only while climate runs${raw.setpoint.some((x) => x != null) ? '' : ' (not in this range)'}.`));
    const c = colors();
    plot(box, ctx, () => ({
      padding: [rpx(6), rpx(8), 0, 0], cursor: { ...cursor(SYNC), drag: { x: false, y: false } }, select: { show: false },
      scales: { x: { time: true, min: fromMs, max: toMs }, y: { auto: true } },
      axes: [timeAxis(tz), tempAxis()],
      series: [{}, line('outside', 0, { width: 2 }), line('cabin', 1), line('battery', 2),
        { label: 'set point', stroke: c.muted, width: rpx(1.5), dash: [rpx(1), rpx(3)], paths: window.uPlot.paths.stepped({ align: 1 }), points: { show: false } }],
      hooks: { drawClear: [shade(ts, st, bms)] },
    }), [ts, ...conv], { onCursor: (i) => rd.show(i == null ? null : [['at', formatDateTime(ts[i], tz, { now: now(), weekday: false })], ...keys.map((k, j) => [k, conv[j][i] == null ? '—' : format(raw[k][i], 'temperature')]), st[i] === 'asleep' || st[i] === 'unknown' ? ['state', st[i]] : null].filter(Boolean)) });
  }
  async function paintCharges() {
    const items = await ctx.api.list(`${base}/charges`, { ...ctx.params, sort: 'start', dir: 'asc' }, { limit: 200 }).all({ max: 3000 });
    if (!ctx.alive()) return;
    const withT = items.filter((x) => x.start_ms != null && x.outside_avg_c != null).sort((a, b) => a.start_ms - b.start_ms);
    if (!withT.length) { replace(chgSlot, h('div', { cover: 'tp.chart.charging_outside' }, empty(items.length ? 'no charge has an outside temperature' : 'no charge in this range'))); return; }
    const sv = withT.map((x) => x.outside_avg_c);
    const box = h('div', { class: 'chart chart--pane is-click', cover: 'tp.chart.charging_outside' });
    const rd = readout('move over a dot; click to open that charge');
    replace(chgSlot, h('p', { class: 'note' }, `${n0(withT.length)} charges   lowest ${format(Math.min(...sv), 'temperature')}   average ${format(sv.reduce((s, x) => s + x, 0) / sv.length, 'temperature')}   highest ${format(Math.max(...sv), 'temperature')}`), box, rd.el);
    const xs = withT.map((x) => x.start_ms), ys = withT.map((x) => convert(x.outside_avg_c, 'temperature'));
    let hover = null;
    plot(box, ctx, ({ colors: cl }) => ({
      padding: [rpx(6), rpx(8), 0, 0], cursor: { ...cursor(SYNC), drag: { x: false, y: false } }, select: { show: false },
      scales: { x: { time: true, min: fromMs, max: toMs }, y: { range: (u, a, b) => [Math.floor(a - 2), Math.ceil(b + 2)] } },
      axes: [timeAxis(tz), tempAxis()],
      series: [{}, { label: 'outside at charge', stroke: cl.c1, width: 0, paths: () => null, points: { show: true, size: rpx(6), width: rpx(1), fill: cl.c1, stroke: cl.c1 } }],
    }), [xs, ys], {
      onCursor: (i, u) => {
        if (i == null || Math.abs(u.valToPos(xs[i], 'x') - u.cursor.left) > rpx(24)) { hover = null; rd.show(null); return; }
        hover = i;
        const ch = withT[i];
        rd.show([['at', formatDateTime(ch.start_ms, tz, { now: now(), weekday: false })], ['place', ch.place?.name || ch.address?.short || 'a spot'], ['outside', format(ch.outside_avg_c, 'temperature')]]);
      },
      onBuild: (u) => u.over.addEventListener('click', () => { if (hover != null) ctx.navigate(state.buildUrl('charge', { car: cid, charge: withT[hover].id })); }),
    });
  }
  function paintDaily() {
    if (temps.error) { replace(daySlot, errorBlock(temps.error, { retry: () => ctx.reload(), home: false })); return; }
    const days = temps.daily || [];
    const help = h('p', { class: 'note', cover: 'tp.chart.daily_envelope_help' }, 'the thin line runs from the day’s lowest to its highest reading; the box holds the middle 70% (15th to 85th percentile). the bars at the bottom are how much of the day the car was awake: trust a day with a small bar less.');
    if (!days.some((x) => x.max_c != null)) { replace(daySlot, h('div', { cover: ['tp.chart.daily_envelope', 'tp.chart.daily_coverage'] }, empty('no day with an outside reading')), help); return; }
    const box = h('div', { class: 'chart', cover: ['tp.chart.daily_envelope', 'tp.chart.daily_coverage'] });
    const rd = readout('move over a day');
    replace(daySlot, box, rd.el, help);
    const xs = days.map((x, i) => { const a = Math.max(x.t_ms, fromMs), b = Math.min(days[i + 1]?.t_ms ?? x.t_ms + DAY, toMs); return b > a ? (a + b) / 2 : x.t_ms + DAY / 2; });
    const T = (k) => days.map((x) => (x[k] == null ? null : convert(x[k], 'temperature')));
    const mn = T('min_c'), p15 = T('p15_c'), p85 = T('p85_c'), mx = T('max_c');
    const cov = days.map((x) => x.coverage_pct ?? 0);
    plot(box, ctx, ({ colors: cl }) => ({
      padding: [rpx(6), rpx(8), 0, 0], cursor: { ...cursor(SYNC), drag: { x: false, y: false } }, select: { show: false },
      scales: { x: { time: true, min: fromMs, max: toMs }, y: { range: (u, a, b) => { const pad = Math.max(1, (b - a) * 0.08); return [a - pad - (b - a) * 0.3, b + pad]; } }, cov: { range: [0, 100] } },
      axes: [timeAxis(tz), tempAxis()],
      series: [{}, ...['lowest', '15th', '85th', 'highest'].map((l) => ({ label: l, stroke: cl.c1, width: 0, paths: () => null, points: { show: false } })),
        { label: 'awake', scale: 'cov', stroke: cl.c2, width: 0, paths: () => null, points: { show: false } }],
      hooks: { draw: [(u) => {
        const cx = u.ctx, { top, height: hg } = u.bbox;
        const pr = window.uPlot.pxRatio || 1;
        const dayPx = Math.abs(u.valToPos(fromMs + DAY, 'x', true) - u.valToPos(fromMs, 'x', true));
        const w = Math.round(Math.max(rpx(3) * pr, Math.min(rpx(14) * pr, dayPx * 0.6)));
        const covH = hg * 0.2;
        cx.save();
        for (let i = 0; i < xs.length; i++) {
          const x = Math.round(u.valToPos(xs[i], 'x', true));
          const ch = (covH * Math.min(100, cov[i])) / 100;
          cx.fillStyle = hexA(cl.c2, 0.35);
          cx.fillRect(x - w / 2, top + hg - ch, w, Math.max(1, ch));
          if (mx[i] == null || mn[i] == null) continue;
          cx.strokeStyle = cl.c1; cx.lineWidth = Math.max(1, Math.round(rpx(1) * pr));
          cx.beginPath(); cx.moveTo(x, u.valToPos(mx[i], 'y', true)); cx.lineTo(x, u.valToPos(mn[i], 'y', true)); cx.stroke();
          if (p15[i] != null && p85[i] != null) {
            const b0 = u.valToPos(p85[i], 'y', true), b1 = u.valToPos(p15[i], 'y', true);
            cx.fillStyle = cov[i] < 10 ? hexA(cl.c1, 0.4) : cl.c1;
            cx.fillRect(x - w / 2, b0, w, Math.max(rpx(2) * pr, b1 - b0));
          }
        }
        cx.restore();
      }] },
    }), [xs, mn, p15, p85, mx, cov], { onCursor: (i) => {
      if (i == null) { rd.show(null); return; }
      const x = days[i];
      rd.show([['day', formatDate(x.t_ms, tz, { weekday: false })], ...(x.max_c == null ? [['reading', 'none']] : [['range', `${format(x.min_c, 'temperature')} to ${format(x.max_c, 'temperature')}`], ['most', `${format(x.p15_c, 'temperature')} to ${format(x.p85_c, 'temperature')}`]]), ['awake', pct(x.coverage_pct)]]);
    } });
  }

  paintCons(eff); paintTemp(eff); paintFactor(eff); paintTempSummary();
  if (opt.group != null) paintGroup(eff);
  await loadUPlot().catch(() => null);
  if (!ctx.alive()) return;
  const guard = (slot, p) => p.catch((e) => { if (ctx.alive()) replace(slot, errorBlock(e, { home: false })); });
  await Promise.all([
    paintSpeed(),
    guard(compareSlot, paintCompare()),
    window.uPlot ? guard(histSlot, paintHistory()) : null,
    window.uPlot ? guard(chgSlot, paintCharges()) : null,
    window.uPlot ? Promise.resolve().then(paintDaily) : null,
  ]);
  if (!ctx.alive()) return;
  ctx.ready();
}
