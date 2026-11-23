// Efficiency and temperatures (/cars/{car}/efficiency): how much energy driving takes, how outside
// temperature changes it, and the temperatures themselves.
// - Consumption, the table by temperature and the energy factor come from GET .../efficiency with the
//   shared period, the minimum drive distance (min_d, in the person's distance unit) and the temperature
//   group width (tg_w, in the person's temperature unit). Energy is the pack's own: no factor is needed
//   to get consumption, so the factor is shown only as a cross-check.
// - A temperature group opens the drives behind it (?group=<lower edge in °C>), each linking to its drive.
// - Other cars (?cars=c2,c3) are compared over the same period: GET .../efficiency for each car and
//   GET /compare?metric=wh_per_km for the daily line.
// - Temperatures: GET .../temperatures (lowest, average, highest and coverage, the days), GET .../series
//   (outside, cabin, battery, set point while climate runs) and the charges of the period (outside
//   temperature at each charge). The three time charts share one cursor.
// Asleep is not unknown: the car reads no temperature while it sleeps, so lines break in both, but asleep
// spans are shaded blue and unknown ones grey, and the summary says how much of the period each took.

import * as state from '../shared/state.js';
import { h, replace, cssVar } from '../shared/dom.js';
import { downloadCsv, csvFilename, column, timeColumn } from '../shared/csv.js';
import { format, convert, toMetric, unitLabel, getUnits } from '../shared/units.js';
import { formatDateTime, formatDate } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon } from '../core/icons.js';
import { pageHead, panel, metric, csvButton, note, tag } from '../core/page.js';
import { dataTable, cardLabels } from '../core/table.js';
import { emptyState, errorState } from '../core/states.js';
import { loadUPlot, mountPlot, timeAxis, TIME_INCRS, valueAxis, colors, hexA } from '../core/chart.js';
import { openPeriodPicker } from '../core/period.js';
import { openCarSwitcher, soldParts, soldToggle } from '../core/carswitch.js';
import { rpx } from '../core/display.js';
import { srcMark } from '../core/source.js';

export const id = 'efficiency';
export const css = true;

const enc = encodeURIComponent;
const DAY = 86400000;
const n0 = (v) => (v == null ? '—' : formatNumber(v, { maximumFractionDigits: 0 }));
const pct = (v, digits = 0) => format(v, 'percent', { digits });
const CAR_COLORS = ['--live', '--warm', '--c-soc', '--c-elev', '--c-power', '--c-regen', '--c-charge', '--c-speed'];
const DIST_CHOICES = [1, 5, 10, 25, 50];
const TEMP_CHOICES = { c: [2, 5, 10], f: [5, 10, 20] };

export async function mount(ctx) {
  const car = ctx.car, cid = car.id, tz = ctx.tz, base = `cars/${enc(cid)}`;
  const period = ctx.period, q = ctx.query;
  const now = () => ctx.api.now();
  const units = getUnits();
  const fahr = units.temperature === 'f';
  const rangeWord = units.range === 'ideal' ? 'ideal' : 'rated';
  const dUnit = unitLabel('distance');
  const opt = {
    minD: DIST_CHOICES.includes(Number(q.min_d)) ? Number(q.min_d) : 1,
    tgW: TEMP_CHOICES[fahr ? 'f' : 'c'].includes(Number(q.tg_w)) ? Number(q.tg_w) : fahr ? 10 : 5,
    group: q.group != null && q.group !== '' && Number.isFinite(Number(q.group)) ? Number(q.group) : null,
    cars: (q.cars || '').split(',').map((s) => s.trim()).filter((s) => s && s !== cid).slice(0, 12),
  };
  let drivesCache = null;
  const groupC = () => round3(toMetric(opt.tgW, 'temperature_delta'));
  const effParams = () => ({ ...ctx.params, min_distance_km: round3(toMetric(opt.minD, 'distance')), temp_group_c: groupC() });
  // the API bins on exact multiples of the width it was sent and rounds the edges it returns to 0.1 °C
  // (8.3, 11.1 for °F widths), so a drive's group is worked out from the width, never from those edges
  const groupIndex = (tc) => Math.floor(tc / groupC());
  const inGroup = (x, g) => x.outside_avg_c != null && groupIndex(x.outside_avg_c) === Math.round(g.temp_c / groupC());

  // Both calls start at once; the page is laid out at once with a grey block in each part, the efficiency parts
  // paint when GET efficiency is in and the temperature parts when both are (their time span needs both)
  const effP = ctx.api.get(`${base}/efficiency`, effParams());
  const tempsP = ctx.api.get(`${base}/temperatures`, ctx.params).catch((e) => ({ error: e }));
  let eff = null, temps = {}, dataStart = null, fromMs = 0, toMs = 0, span = 1;

  /* ---------- head and filters */
  const periodBtn = h('button', { class: 'btn', type: 'button', cover: ['ef.filter.time_range', 'tp.filter.time_range'], title: 'Choose the period (shared by every page)', onclick: (e) => openPeriodPicker(e.currentTarget) },
    icon('calendar', 'ico--sm'), period.label, icon('chev-d', 'ico--sm'));
  const head = pageHead({
    eyebrow: car.name, title: 'Efficiency and temperatures',
    sub: [period.label, `Times and days in the car’s zone (${tz})`],
    purpose: 'How much energy driving takes, how the weather changes it, and the temperatures the car saw. Energy is measured by the pack, so no estimate is needed.',
    actions: [h('button', { class: 'btn', type: 'button', onclick: openCarSwitcher, cover: 'tp.filter.car', title: 'Show another car on this page' }, icon('car'), 'Switch car')],
  });

  const segMin = segment(DIST_CHOICES.map((v) => [v, `${v} ${dUnit}`]), opt.minD, (v) => { opt.minD = v; ctx.setQuery({ min_d: v === 1 ? null : String(v), group: null }); opt.group = null; refreshEfficiency(); }, 'Minimum drive distance');
  const segT = segment(TEMP_CHOICES[fahr ? 'f' : 'c'].map((v) => [v, `${v} ${unitLabel('temperature')}`]), opt.tgW, (v) => { opt.tgW = v; ctx.setQuery({ tg_w: v === (fahr ? 10 : 5) ? null : String(v), group: null }); opt.group = null; refreshEfficiency(); }, 'Temperature group width');
  const carsBox = h('div', { class: 'ef-cars' });
  const filters = h('section', { class: 'panel panel-pad ef-filters', 'aria-label': 'Filters' },
    h('div', { class: 'ef-filter-row' },
      h('div', { class: 'filter' }, h('span', { class: 'form-label' }, 'Period'), periodBtn),
      h('div', { class: 'filter', cover: 'ef.filter.min_distance', title: 'Drives shorter than this are left out of every efficiency figure on this page' }, h('span', { class: 'form-label' }, 'Shortest drive'), segMin),
      h('div', { class: 'filter', title: 'How wide each outside-temperature group is' }, h('span', { class: 'form-label' }, 'Temperature groups'), segT),
      carsBox),
    h('p', { class: 'note ef-filter-note' }, `Drives count when they start in the period. The shortest-drive filter applies to net consumption, logged distance, the table and the daily comparison; gross consumption and the parked share always take every drive, and temperatures below use every reading.`));

  /* ---------- slots */
  const consSlot = h('div', { class: 'ef-cons-body' });
  const consPanel = panel({ title: 'Consumption', icon: 'leaf', cls: 'ef-cons', cover: 'shots.efficiency' }, consSlot);
  const factorSlot = h('div');
  const factorSide = h('span', { class: 'panel-side' });
  const factorPanel = panel({ title: 'Energy factor', icon: 'gauge', cls: 'ef-factor', side: factorSide }, factorSlot);
  const byTempSlot = h('div', { class: 'ef-bytemp-slot' });
  const groupSlot = h('div', { class: 'ef-group-slot' });
  const compareSlot = h('div', { class: 'ef-compare-slot' });

  const tSumSlot = h('div', { class: 'tp-sum-body' });
  const tHistSlot = h('div', { class: 'tp-hist-body' });
  const tHistSide = h('span', { class: 'panel-side' });
  const tChgSlot = h('div', { class: 'tp-chg-body' });
  const tChgSide = h('span', { class: 'panel-side' });
  const tDaySlot = h('div', { class: 'tp-day-body' });
  const tDaySide = h('span', { class: 'panel-side' });

  replace(ctx.view, h('div', { class: 'efficiency-page' },
    head, filters,
    h('div', { class: 'ef-grid' },
      consPanel, factorPanel,
      h('div', { class: 'ef-bytemp' }, byTempSlot, groupSlot),
      compareSlot),
    h('div', { class: 'ef-section-head', cover: 'tp.sections' },
      h('h2', { class: 'section-title' }, icon('thermo'), 'Temperatures'),
      h('span', { class: 'faint' }, 'Summary, history, charges and days. The three charts share one cursor: point at a moment on one and the others mark it too.')),
    h('div', { class: 'ef-grid tp-grid', cover: 'shots.temperatures' },
      panel({ title: 'Lowest, average, highest', icon: 'thermo', cls: 'tp-sum' }, tSumSlot),
      panel({ title: 'Over time', icon: 'timeline', cls: 'tp-hist', side: tHistSide, cover: 'tp.control.shared_crosshair' }, tHistSlot),
      panel({ title: 'Outside temperature at each charge', icon: 'bolt', cls: 'tp-chg', side: tChgSide }, tChgSlot),
      panel({ title: 'Each day outside', icon: 'calendar', cls: 'tp-day', side: tDaySide }, tDaySlot))));

  const wait = () => h('div', { class: 'skel skel--block', 'aria-hidden': 'true' });
  for (const slot of [consSlot, factorSlot, byTempSlot, tSumSlot, tHistSlot, tChgSlot, tDaySlot]) slot.append(wait());
  const uplotP = loadUPlot().catch(() => null);
  const sync = `temps-${cid}`;

  const paintEff = async () => {
    eff = await effP;
    if (!ctx.alive()) return;
    paintConsumption(eff);
    paintFactor(eff);
    paintByTemp(eff);
  };
  // GET efficiency failing fails the page (as before)
  const effDone = paintEff();
  const paintRest = async () => {
    [, temps] = await Promise.all([effDone, tempsP]);
    if (!ctx.alive()) return;
    // "All" has no start: the charts start where the data does
    dataStart = temps.daily?.[0]?.t_ms ?? null;
    fromMs = period.from_ms ?? (dataStart ?? (eff.from_ms || now() - 30 * DAY)); toMs = Math.min(eff.to_ms ?? period.to_ms ?? now(), now());
    span = Math.max(1, toMs - fromMs);
    paintTempSummary();
    const picker = paintCarsPicker();
    if (opt.group != null) paintGroup(eff);
    await uplotP;
    if (!ctx.alive()) return;
    await Promise.all([
    paintCompare().catch((e) => replace(compareSlot, panel({ title: 'Compare cars', icon: 'car', cls: 'ef-compare' }, errorState(e, { compact: true, home: false, retry: () => paintCompare() })))),
    paintHistory().catch((e) => replace(tHistSlot, errorState(e, { compact: true, home: false, retry: () => paintHistory() }))),
    paintCharges().catch((e) => replace(tChgSlot, errorState(e, { compact: true, home: false, retry: () => paintCharges() }))),
    paintDaily(),
    picker,
    ]);
  };
  await Promise.all([effDone, paintRest()]);
  // ready once every section has its data: the charts and the car picker are part of the first render
  if (ctx.alive()) ctx.ready();
  return {};

  /* ================================================================ efficiency */

  async function refreshEfficiency() {
    replace(groupSlot);
    try {
      const d = await ctx.api.get(`${base}/efficiency`, effParams());
      if (!ctx.alive()) return;
      paintConsumption(d); paintFactor(d); paintByTemp(d);
      if (opt.cars.length) paintCompare().catch(() => null);
    } catch (e) {
      if (ctx.alive()) replace(byTempSlot, errorState(e, { compact: true, home: false, retry: () => refreshEfficiency() }));
    }
  }

  function paintConsumption(d) {
    const minText = format(toMetric(opt.minD, 'distance'), 'distance', { digits: 0 });
    const why = d.drives ? 'Too little distance to divide by' : `No drive of at least ${minText} in this period`;
    const est = d.estimated_drives || 0;
    // gross and the parked share cover every drive of the period, so they do not follow the filter
    const all = d.all_drives || { drives: d.drives, distance_km: d.distance_km, energy_kwh: d.energy_kwh };
    const used = (all.energy_kwh || 0) + (d.parked_kwh || 0);
    const parkedShare = used > 0 ? (100 * (d.parked_kwh || 0)) / used : null;
    replace(consSlot,
      h('div', { class: 'ef-cons-figs' },
        metric('Net consumption', d.net_wh_per_km, 'efficiency', { size: 'lg', cover: 'ef.stat.consumption_net', why,
          explain: `Driving only: the ${format(d.energy_kwh, 'energy')} the pack gave while driving, regen already taken off, over the distance driven.` }),
        metric('Gross consumption', d.gross_wh_per_km, 'efficiency', { size: 'lg', cover: 'ef.stat.consumption_gross', why,
          explain: `Everything: the ${format(all.energy_kwh, 'energy')} of all ${n0(all.drives)} ${all.drives === 1 ? 'drive' : 'drives'} in the period plus ${format(d.parked_kwh, 'energy')} used while parked (climate, Sentry, standby), over all ${format(all.distance_km, 'distance')} driven. Short drives count here whatever the shortest-drive filter says. From the pack’s own counters, exact for any period.` }),
        metric('Logged distance', d.distance_km, 'distance', { cover: 'ef.stat.logged_distance',
          explain: `The ${n0(d.drives)} ${d.drives === 1 ? 'drive' : 'drives'} that started in this period and are at least ${minText} long.` }),
        metric('Parked share', parkedShare, 'percent', { digits: 0,
          why: 'No energy used in this period', explain: 'Of all the energy used in the period (every drive plus parked), the part used while the car stood still.' })),
      est ? h('p', { class: 'note warn' }, tag('estimated', 'warm'), ` ${n0(est)} of the ${n0(d.drives)} drives are imported history: their energy is worked out from range and the energy factor, not measured.`) : null,
      h('div', { class: 'ef-cons-help', cover: 'ef.help.headline_stats' },
        h('p', { class: 'note' }, icon('info', 'ico--sm'), ` Net shows how efficiently the car drives: drives of at least ${minText}. Gross is what owning it costs in energy per distance: every drive and all parked energy, so the shortest-drive filter does not change it.`),
        h('p', { class: 'note', cover: 'faq.no-consumption' }, icon('check', 'ico--sm'), ` Nothing to wait for: consumption is measured from the first drive, with no charges needed to calibrate it. A blank figure means there was no drive of at least ${minText} in the period; pick a longer period or a shorter shortest drive.`)));
  }

  function paintFactor(d) {
    const f = d.factor || {};
    const mine = rangeWord === 'ideal' ? f.ideal_wh_per_km : f.rated_wh_per_km;
    const owner = f.source === 'owner';
    const rows = [
      { kind: 'Rated', value: f.rated_wh_per_km, readings: owner ? null : f.readings, source: owner ? 'typed by the owner' : 'energy and rated range read in the same second', cover: ['ef.table.derived_rated'], vc: 'ef.rated.col.efficiency', cc: 'ef.rated.col.count' },
      { kind: 'Ideal', value: f.ideal_wh_per_km, readings: f.readings, source: 'energy and ideal range read in the same second', cover: ['ef.table.derived_ideal'], vc: 'ef.ideal.col.efficiency', cc: 'ef.ideal.col.count' },
    ];
    replace(factorSide, csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'energy-factor'), rows.map((r) => ({ range: r.kind, wh_per_km: r.value, readings: r.readings, source: r.source })),
      [column('Range kind', 'range'), column('Energy per distance of range', 'wh_per_km', 'efficiency', { digits: 1 }), column('Readings', 'readings'), column('Source', 'source')]) }));
    replace(factorSlot,
      metric(`Energy per ${unitLabel('range')} of ${rangeWord} range`, mine, 'efficiency', { cover: 'ef.stat.current_factor', why: 'Not worked out yet: needs a drive or charge with energy and range readings',
        explain: owner ? 'Typed by the owner in Car settings; it replaces the worked-out value.' : `What one ${unitLabel('range')} of ${rangeWord} range on the car’s display stands for. Your preference is ${rangeWord} range.` }),
      h('div', { class: 'tbl-scroll' }, h('table', { class: 'tbl ef-factor-tbl', 'aria-label': 'Energy factor by range kind' },
        h('thead', null, h('tr', null, h('th', { scope: 'col' }, 'Range kind'), h('th', { scope: 'col', class: 'is-num' }, `Energy per ${unitLabel('range')}`), h('th', { scope: 'col', class: 'is-num' }, 'Readings behind it'), h('th', { scope: 'col' }, 'Worked out from'))),
        h('tbody', null, rows.map((r) => h('tr', { cover: r.cover, class: r.kind.toLowerCase() === rangeWord ? 'is-mine' : null },
          h('td', { class: 'ef-rowhead' }, r.kind, r.kind.toLowerCase() === rangeWord ? h('span', { class: 'faint' }, ' (yours)') : null),
          h('td', { class: 'is-num', cover: r.vc }, format(r.value, 'efficiency', { digits: 1 })),
          h('td', { class: 'is-num', cover: r.cc }, r.readings == null ? '—' : n0(r.readings)),
          h('td', null, r.source)))))),
      note(`A cross-check only: the car uses this to turn energy into the range it shows. Consumption above is measured, so it never depends on it. Each reading is one second where the pack energy and the range were both read${f.as_of_ms ? `; last reading ${formatDateTime(f.as_of_ms, tz, { now: now() })}` : ''}.`),
      h('a', { class: 'panel-link', href: state.buildUrl('settings-cars', { car: cid }) }, 'Type a value in Car settings', icon('chev-r', 'ico--sm')));
    factorSlot.querySelectorAll('table.tbl').forEach(cardLabels);
  }

  function groupLabel(g) {
    const lo = convert(g.temp_c, 'temperature'), hi = convert(g.temp_to_c ?? g.temp_c + toMetric(opt.tgW, 'temperature_delta'), 'temperature');
    return `${formatNumber(lo, { maximumFractionDigits: 0 })} to ${formatNumber(hi, { maximumFractionDigits: 0 })} ${unitLabel('temperature')}`;
  }
  function rangeEff(g, f) {
    const fac = rangeWord === 'ideal' ? f?.ideal_wh_per_km : f?.rated_wh_per_km;
    if (fac && g.wh_per_km) return (fac / g.wh_per_km) * 100;
    return g.range_efficiency_pct ?? null;
  }

  function paintByTemp(d) {
    const groups = d.by_temperature || [];
    const maxWh = Math.max(1, ...groups.map((g) => g.wh_per_km || 0));
    const nt = d.no_temperature || { drives: 0, distance_km: 0 };
    const t = dataTable(ctx, {
      title: 'By outside temperature', icon: 'thermo', cls: 'ef-bytemp-tbl', cover: 'ef.table.temperature',
      label: 'Consumption by outside temperature',
      rows: groups,
      sort: { key: 'temp', dir: 'desc' },
      columns: [
        { key: 'temp', label: 'Outside', sort: true, value: (g) => g.temp_c, cover: 'ef.temp.col.temperature', title: 'The drive’s average outside temperature falls in this group',
          render: (g) => h('span', { class: 'nowrap' }, groupLabel(g)), csv: false },
        { key: 'drives', label: 'Drives', num: true, sort: true, value: (g) => g.drives, title: 'Open the drives in this group',
          render: (g) => h('button', { class: ['btn', 'btn--sm', 'btn--ghost', opt.group === g.temp_c && 'is-on'], type: 'button', 'aria-pressed': String(opt.group === g.temp_c), onclick: () => openGroup(g, d) }, n0(g.drives), icon('chev-r', 'ico--sm')) },
        { key: 'wh', label: 'Consumption', num: true, sort: true, value: (g) => g.wh_per_km, cover: 'ef.temp.col.consumption', title: 'Pack energy over distance, driving only',
          render: (g) => h('span', { class: 'ef-whcell' }, h('span', { class: 'bar', 'aria-hidden': 'true', vars: { '--w': `${((g.wh_per_km || 0) / maxWh) * 100}%` } }, h('i')), format(g.wh_per_km, 'efficiency')) },
        { key: 'eff', label: `Of ${rangeWord} range`, num: true, sort: true, value: (g) => rangeEff(g, d.factor), cover: 'ef.temp.col.efficiency', title: `Distance driven as a share of the ${rangeWord} range used. 100 % matches the car’s estimate.`,
          render: (g) => { const v = rangeEff(g, d.factor); return h('span', { class: ['ef-eff', v == null ? null : v < 65 ? 'is-low' : v < 99 ? 'is-mid' : 'is-ok'] }, pct(v)); } },
        { key: 'dist', label: 'Distance', num: true, sort: true, value: (g) => g.distance_km, cover: 'ef.temp.col.distance', render: (g) => format(g.distance_km, 'distance') },
        { key: 'speed', label: 'Average speed', num: true, sort: true, value: (g) => g.avg_kph, cover: 'ef.temp.col.speed', title: 'Distance over driving time, stops in traffic included', render: (g) => format(g.avg_kph, 'speed') },
      ],
      csv: { filename: csvFilename(car.name, 'efficiency-by-temperature', period.preset), columns: [
        { label: `From (${unitLabel('temperature')})`, value: (g) => round1(convert(g.temp_c, 'temperature')) },
        { label: `To (${unitLabel('temperature')})`, value: (g) => round1(convert(g.temp_to_c, 'temperature')) },
        { label: 'Drives', value: (g) => g.drives },
        { label: `Consumption (${unitLabel('efficiency')})`, value: (g) => round1(g.wh_per_km == null ? null : convert(g.wh_per_km, 'efficiency')) },
        { label: `Of ${rangeWord} range (%)`, value: (g) => round1(rangeEff(g, d.factor)) },
        { label: `Distance (${unitLabel('distance')})`, value: (g) => round1(g.distance_km == null ? null : convert(g.distance_km, 'distance')) },
        { label: 'Energy (kWh)', value: (g) => g.energy_kwh },
        { label: `Average speed (${unitLabel('speed')})`, value: (g) => round1(g.avg_kph == null ? null : convert(g.avg_kph, 'speed')) },
      ] },
      empty: () => emptyState({ icon: 'thermo', title: 'No drives to group', text: d.drives ? 'None of the drives has an outside temperature.' : `No drive of at least ${format(toMetric(opt.minD, 'distance'), 'distance')} started in this period. Pick a longer period or a shorter shortest drive.` }),
      note: `Each drive goes in the group of its average outside temperature, in ${opt.tgW} ${unitLabel('temperature')} steps. “Of ${rangeWord} range” is the distance driven for each ${unitLabel('range')} of range used: under 100 % the car used more range than it drove. Drives button: the drives behind a group.`,
    });
    replace(byTempSlot, t.el,
      nt.drives ? h('p', { class: 'note ef-nt' }, icon('info', 'ico--sm'), ` ${n0(nt.drives)} ${nt.drives === 1 ? 'drive' : 'drives'} (${format(nt.distance_km, 'distance')}) had no outside temperature and are counted in the figures above but in no group.`) : null);
  }

  function openGroup(g, d) {
    opt.group = opt.group === g.temp_c ? null : g.temp_c;
    ctx.setQuery({ group: opt.group == null ? null : String(opt.group) });
    paintByTemp(d);
    if (opt.group == null) { replace(groupSlot); return; }
    paintGroup(d);
  }

  async function paintGroup(d) {
    const g = (d.by_temperature || []).find((x) => x.temp_c === opt.group);
    if (!g) { replace(groupSlot); return; }
    replace(groupSlot, panel({ title: `Drives at ${groupLabel(g)}`, icon: 'route', cls: 'ef-group' }, h('p', { class: 'faint' }, 'Loading the drives…')));
    try {
      const key = JSON.stringify(effParams());
      if (!drivesCache || drivesCache.key !== key) {
        const items = await ctx.api.list(`${base}/drives`, { ...ctx.params, min_distance_km: effParams().min_distance_km, sort: 'start', dir: 'desc' }, { limit: 200, max: 5000 }).all();
        drivesCache = { key, items };
      }
      if (!ctx.alive() || opt.group !== g.temp_c) return;
      const rows = drivesCache.items.filter((x) => inGroup(x, g));
      const off = rows.length !== g.drives;
      const close = h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: () => openGroup(g, d), title: 'Close this list' }, icon('x', 'ico--sm'), 'Close');
      const t = dataTable(ctx, {
        title: `Drives at ${groupLabel(g)}`, icon: 'route', cls: 'ef-group', rows, side: close,
        sort: { key: 'start', dir: 'desc' },
        rowHref: (x) => state.buildUrl('drive', { car: cid, drive: x.id }),
        columns: [
          { key: 'start', label: 'Start', sort: true, value: (x) => x.start_ms, render: (x) => formatDateTime(x.start_ms, tz, { now: now() }), csv: (x) => x.start_ms },
          { key: 'route', label: 'From and to', render: (x) => `${x.start?.label || x.start?.place || 'Unnamed spot'} → ${x.end?.label || x.end?.place || 'Unnamed spot'}`, csv: (x) => `${x.start?.label || ''} → ${x.end?.label || ''}` },
          { key: 'temp', label: 'Outside', num: true, sort: true, value: (x) => x.outside_avg_c, render: (x) => format(x.outside_avg_c, 'temperature') },
          { key: 'dist', label: 'Distance', num: true, sort: true, value: (x) => x.distance_km, render: (x) => format(x.distance_km, 'distance') },
          { key: 'wh', label: 'Consumption', num: true, sort: true, value: (x) => x.wh_per_km, render: (x) => h('span', null, format(x.wh_per_km, 'efficiency'), x.energy_basis === 'estimated' ? [' ', tag('est.', 'warm')] : null) },
          { key: 'speed', label: 'Average speed', num: true, sort: true, value: (x) => x.speed_avg_kph, render: (x) => format(x.speed_avg_kph, 'speed') },
        ],
        empty: () => emptyState({ icon: 'route', title: 'No drives in this group', text: 'The list changed since the table was built. Reload the page.' }),
        note: `${n0(rows.length)} ${rows.length === 1 ? 'drive' : 'drives'} whose average outside temperature was in this group. Open one for its route and charts.${off ? ` The table counted ${n0(g.drives)}: drives were added or changed since it was built; reload the page to bring both up to date.` : ''}`,
      });
      replace(groupSlot, t.el);
    } catch (e) {
      if (ctx.alive()) replace(groupSlot, panel({ title: `Drives at ${groupLabel(g)}`, icon: 'route', cls: 'ef-group' }, errorState(e, { compact: true, home: false, retry: () => paintGroup(d) })));
    }
  }

  /* ---------- other cars */
  async function paintCarsPicker() {
    let fleet = { items: [] };
    try { fleet = await ctx.api.page('cars', { sort: 'name' }, { limit: 100 }); } catch { /* the picker stays empty */ }
    if (!ctx.alive()) return;
    const list = h('div', { class: 'ef-cars-list', role: 'group', 'aria-label': 'Cars to compare' });
    // sold cars wait behind "Sold cars (n)" unless compared already or searched for
    let soldOpen = false, lastItems = [], lastSearch = false;
    const row = (c) => h('label', { class: 'check' },
      h('input', { type: 'checkbox', checked: opt.cars.includes(c.id), onchange: (e) => toggleCar(c.id, e.currentTarget.checked) }), h('span', null, c.name),
      state.isSold(c) ? h('span', { class: 'tag tag--plain' }, 'Sold') : null);
    const fill = (items, searched = lastSearch) => {
      lastItems = items; lastSearch = searched;
      const others = items.filter((c) => c.id !== cid);
      const { main, sold } = soldParts(others, opt.cars, { search: searched });
      replace(list, main.map(row), soldToggle(sold.length, soldOpen, () => { soldOpen = !soldOpen; fill(lastItems); }), soldOpen ? sold.map(row) : null,
        others.length ? null : h('span', { class: 'faint' }, 'No other car matches.'));
    };
    fill(fleet.items || [], false);
    const search = fleet.next ? h('input', { class: 'input input--sm ef-cars-search', type: 'search', placeholder: 'Find a car', 'aria-label': 'Find a car to compare' }) : null;
    if (search) {
      let tm = 0;
      search.addEventListener('input', () => {
        clearTimeout(tm);
        tm = setTimeout(async () => {
          const pg = await ctx.api.page('cars', { sort: 'name', search: search.value.trim() || null }, { limit: 100 }).catch(() => null);
          if (pg && ctx.alive()) fill(pg.items || [], !!search.value.trim());
        }, 300);
      });
      ctx.onCleanup(() => clearTimeout(tm));
    }
    const others = (fleet.items || []).filter((c) => c.id !== cid).length || fleet.next;
    const count = h('span', { class: 'ef-cars-count' }, opt.cars.length ? `${opt.cars.length + 1} cars` : 'This car only');
    const details = h('details', { class: 'ef-cars-pop' },
      h('summary', { class: 'btn', title: 'Compare this car with others over the same period' }, icon('car', 'ico--sm'), 'Compare cars', count),
      h('div', { class: 'ef-cars-menu glass-card' },
        h('p', { class: 'faint' }, 'Tick the cars to set beside this one. Each keeps its own time zone.'), search, list,
        opt.cars.length ? h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: () => { opt.cars = []; ctx.setQuery({ cars: null }); details.open = false; paintCarsPicker(); paintCompare(); } }, 'This car only') : null));
    replace(carsBox, h('div', { class: 'filter', cover: 'ef.section.per_car' }, h('span', { class: 'form-label' }, 'Cars'), others ? details : h('span', { class: 'faint' }, 'Only one car')));
    function toggleCar(id, on) {
      opt.cars = on ? [...new Set([...opt.cars, id])] : opt.cars.filter((x) => x !== id);
      ctx.setQuery({ cars: opt.cars.length ? opt.cars.join(',') : null });
      count.textContent = opt.cars.length ? `${opt.cars.length + 1} cars` : 'This car only';
      paintCompare().catch((e) => replace(compareSlot, panel({ title: 'Compare cars', icon: 'car', cls: 'ef-compare' }, errorState(e, { compact: true, home: false }))));
    }
  }

  async function paintCompare() {
    if (!opt.cars.length) { replace(compareSlot); return; }
    const ids = [cid, ...opt.cars];
    replace(compareSlot, panel({ title: 'Compare cars', icon: 'car', cls: 'ef-compare' }, h('p', { class: 'faint' }, 'Loading the other cars…')));
    const cars = await Promise.all(ids.map((i) => (i === cid ? Promise.resolve(car) : ctx.api.get(`cars/${enc(i)}`).catch(() => null))));
    if (!ctx.alive()) return;
    // only the cars this account can open: /compare answers 404 for the whole request if one is hidden
    const seen = ids.filter((i, k) => cars[k]?.name);
    const [effs, cmp] = await Promise.all([
      Promise.all(ids.map((i, k) => (cars[k]?.name ? ctx.api.get(`cars/${enc(i)}/efficiency`, effParams()).catch(() => null) : null))),
      ctx.api.get('compare', { cars: seen.join(','), metric: 'wh_per_km', min_distance_km: effParams().min_distance_km, ...ctx.params }).catch((e) => ({ error: e })),
    ]);
    if (!ctx.alive()) return;
    const rows = ids.map((i, k) => ({ id: i, name: cars[k]?.name || null, color: CAR_COLORS[k % CAR_COLORS.length], e: effs[k] })).filter((r) => r.name);
    const missing = ids.length - rows.length;
    const best = Math.min(...rows.map((r) => r.e?.net_wh_per_km ?? Infinity));
    const csvRows = rows.map((r) => ({ car: r.name, net: r.e?.net_wh_per_km, gross: r.e?.gross_wh_per_km, distance: r.e?.distance_km, drives: r.e?.drives, parked: r.e?.parked_kwh }));
    const tbl = h('div', { class: 'tbl-scroll' }, h('table', { class: 'tbl ef-compare-tbl', 'aria-label': 'Consumption of each car over the same period' },
      h('thead', null, h('tr', null, ['Car', 'Net consumption', 'Gross consumption', 'Logged distance', 'Drives', 'Used parked'].map((l, k) => h('th', { scope: 'col', class: k ? 'is-num' : null }, l)))),
      h('tbody', null, rows.map((r) => h('tr', null,
        h('td', { class: 'ef-rowhead' }, h('i', { class: 'legend-swatch', vars: { '--c': `var(${r.color})` } }), r.id === cid ? r.name : h('a', { class: 'link', href: ctx.link('efficiency', { car: r.id }) }, r.name)),
        h('td', { class: 'is-num' }, format(r.e?.net_wh_per_km, 'efficiency'), r.e?.net_wh_per_km === best && rows.length > 1 ? [' ', tag('lowest', 'live')] : null),
        h('td', { class: 'is-num' }, format(r.e?.gross_wh_per_km, 'efficiency')),
        h('td', { class: 'is-num' }, format(r.e?.distance_km, 'distance')),
        h('td', { class: 'is-num' }, n0(r.e?.drives)),
        h('td', { class: 'is-num' }, format(r.e?.parked_kwh, 'energy')))))));
    cardLabels(tbl.querySelector('table'));
    const chartEl = h('div', { class: 'chart ef-compare-chart' });
    const readout = h('span', { class: 'faint ef-readout' }, 'Point at the chart for each car’s day.');
    const series = (cmp.error ? [] : cmp.series || []).map((s) => ({ ...s, color: rows.find((r) => r.id === s.car)?.color || '--text-3' })).filter((s) => rows.some((r) => r.id === s.car));
    const xsSet = new Set(); series.forEach((s) => (s.t_ms || []).forEach((t) => xsSet.add(t)));
    const xsMs = [...xsSet].sort((a, b) => a - b);
    const cols = series.map((s) => { const m = new Map((s.t_ms || []).map((t, i) => [t, s.values[i]])); return xsMs.map((t) => (m.has(t) && m.get(t) != null ? convert(m.get(t), 'efficiency') : null)); });
    const dailyCsv = xsMs.map((t, i) => Object.fromEntries([['t_ms', t], ...series.map((s, k) => [s.name, cols[k][i]])]));
    replace(compareSlot, panel({ title: 'Compare cars', icon: 'car', cls: 'ef-compare', cover: 'ef.section.per_car',
      side: h('span', { class: 'panel-side' },
        csvButton({ label: 'Figures CSV', onClick: () => downloadCsv(csvFilename(car.name, 'efficiency-compare', period.preset), csvRows, [column('Car', 'car'), column('Net consumption', 'net', 'efficiency', { digits: 1 }), column('Gross consumption', 'gross', 'efficiency', { digits: 1 }), column('Logged distance', 'distance', 'distance', { digits: 1 }), column('Drives', 'drives'), column('Used parked', 'parked', 'energy', { digits: 2 })]) }),
        csvButton({ label: 'Daily CSV', onClick: () => downloadCsv(csvFilename(car.name, 'efficiency-compare-daily', period.preset), dailyCsv, [timeColumn('Day', 't_ms', tz), ...series.map((s) => column(`${s.name} (${unitLabel('efficiency')})`, s.name, null, { digits: 1 }))]) })) },
    tbl,
    missing ? h('p', { class: 'note warn' }, `${missing} ${missing === 1 ? 'car' : 'cars'} could not be shown: hidden from this account or removed.`) : null,
    h('div', { class: 'chart-label' }, h('span', { class: 'name' }, 'Net consumption per day'), h('span', { class: 'chart-unit' }, unitLabel('efficiency')), readout),
    cmp.error ? errorState(cmp.error, { compact: true, home: false, retry: () => paintCompare() })
      : xsMs.length ? chartEl : h('p', { class: 'faint' }, `No day in this period has drives of at least ${format(toMetric(opt.minD, 'distance'), 'distance', { digits: 0 })} for these cars.`),
    note('Same period and shortest drive for every car, in the table and the daily line. A point is one day with drives (one ISO week for periods over 120 days). The lowest net consumption is marked.')));
    if (cmp.error || !xsMs.length || !window.uPlot) return;
    const make = (width, height) => {
      const c = colors();
      return {
        width, height, padding: [rpx(10), rpx(14), 0, 0], legend: { show: false },
        cursor: { drag: { x: false, y: false }, points: { size: rpx(7), width: rpx(2), stroke: c.cursor } },
        scales: { x: { time: true, min: fromMs / 1000, max: toMs / 1000 }, y: { auto: true } },
        axes: [xAxis(tz, span), valueAxis({ size: 44 })],
        series: [{}, ...series.map((s) => ({ label: s.name, stroke: cssVar(s.color), width: rpx(2), spanGaps: false, points: { show: true, size: rpx(5), fill: cssVar(s.color) } }))],
        hooks: { setCursor: [(u) => {
          const i = u.cursor.idx;
          if (i == null || u.cursor.left < 0) { readout.textContent = 'Point at the chart for each car’s day.'; return; }
          readout.textContent = `${formatDate(xsMs[i], tz, { now: now() })}: ${series.map((s, k) => `${s.name} ${format(cols[k][i] == null ? null : toMetric(cols[k][i], 'efficiency'), 'efficiency')}`).join(' · ')}`;
        }] },
      };
    };
    mountPlot(chartEl, make, [xsMs.map((t) => t / 1000), ...cols], ctx);
  }

  /* ================================================================ temperatures */

  function paintTempSummary() {
    if (temps.error) { replace(tSumSlot, errorState(temps.error, { compact: true, home: false, retry: () => ctx.reload() })); return; }
    const miss = temps.missing_ms || {};
    const asleepPct = (100 * (miss.asleep || 0)) / span, unknownPct = (100 * (miss.unknown || 0)) / span;
    const col = (key, label, cv, why) => {
      const t = temps[key] || {};
      const none = t.avg_c == null;
      return h('div', { class: ['tp-col', `tp-col--${key}`] },
        h('h3', { class: 'tp-col-h' }, h('i', { class: 'legend-swatch', vars: { '--c': `var(${SERIES[key].color})` } }), label),
        h('div', { class: 'tp-col-figs' },
          metric('Lowest', t.min_c, 'temperature', { size: 'sm', cover: cv.min, why: none ? why : 'No reading' }),
          metric('Average', t.avg_c, 'temperature', { cover: cv.avg, why: none ? why : 'No reading', explain: none ? null : 'Mean of daily means' }),
          metric('Highest', t.max_c, 'temperature', { size: 'sm', cover: cv.max, why: none ? why : 'No reading' })),
        h('div', { class: 'tp-cov', cover: cv.cov, title: 'Share of the period with readings: the car reads temperatures only while awake' },
          h('span', { class: 'metric-label' }, 'Readings cover'),
          h('span', { class: ['bar', (t.coverage_pct ?? 0) < 25 && 'bar--warm'], 'aria-hidden': 'true', vars: { '--w': `${Math.min(100, t.coverage_pct ?? 0)}%` } }, h('i')),
          h('b', null, none ? '0 %' : pct(t.coverage_pct, 1)), h('span', { class: 'faint' }, ' of the period')));
    };
    replace(tSumSlot,
      h('div', { class: 'tp-cols' },
        col('outside', 'Outside', { min: 'tp.outside.min', avg: 'tp.outside.avg', max: 'tp.outside.max', cov: 'tp.outside.coverage' }, 'No reading in this period'),
        col('cabin', 'Cabin', { min: 'tp.inside.min', avg: 'tp.inside.avg', max: 'tp.inside.max', cov: 'tp.inside.coverage' }, 'This car does not send its cabin temperature, or it slept the whole period'),
        col('battery', 'Battery', {}, 'No reading in this period')),
      h('div', { class: 'tp-missing' },
        h('span', { class: 'tp-miss' }, h('i', { class: 'k-asleep', 'aria-hidden': 'true' }), `Asleep ${pct(asleepPct, 1)}`, h('span', { class: 'faint' }, ' (the car was quiet; nothing is read)')),
        h('span', { class: 'tp-miss' }, h('i', { class: 'k-unknown', 'aria-hidden': 'true' }), `Unknown ${pct(unknownPct, unknownPct < 1 && unknownPct > 0 ? 2 : 1)}`, h('span', { class: 'faint' }, ' (no log, or a log that failed its check)'))),
      h('p', { class: 'note', cover: 'tp.summary.help' }, icon('info', 'ico--sm'), ' Lowest and highest are single readings. The average is the mean of each day’s mean, so a long drive on one day does not outweigh quiet days. Coverage is the share of the period the car was awake and reading; with little coverage the figures describe only the times the car was in use, not the whole period.'));
  }

  async function paintHistory() {
    const d = await ctx.api.get(`${base}/series`, { fields: 'outside_c,cabin_c,battery_c,setpoint_left_c,climate', ...ctx.params, from_ms: fromMs, to_ms: toMs, max_points: 800 });
    if (!ctx.alive()) return;
    const ts = d.t_ms || [], v = d.values || {}, st = d.state || [], bms = d.bucket_ms || 60000;
    const climate = v.climate || [];
    const on = (i) => climate[i] === 'on' || climate[i] === true;
    const raw = {
      outside: v.outside_c || [], cabin: v.cabin_c || [], battery: v.battery_c || [],
      setpoint: (v.setpoint_left_c || []).map((x, i) => (on(i) ? x : null)),
    };
    const rows = ts.map((t, i) => ({ t_ms: t, outside: raw.outside[i], cabin: raw.cabin[i], battery: raw.battery[i], setpoint: raw.setpoint[i], climate: climate[i] ?? null, state: st[i] ?? null }));
    replace(tHistSide, srcMark(d, { short: true }), csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'temperatures', period.preset), rows, [timeColumn('Bucket start', 't_ms', tz), column('Outside', 'outside', 'temperature', { digits: 1 }), column('Cabin', 'cabin', 'temperature', { digits: 1 }), column('Battery', 'battery', 'temperature', { digits: 1 }), column('Set point (climate on)', 'setpoint', 'temperature', { digits: 1 }), column('Climate', 'climate'), column('State', 'state')]) }));
    if (!ts.length || !['outside', 'cabin', 'battery'].some((k) => raw[k].some((x) => x != null))) {
      replace(tHistSlot, emptyState({ icon: 'thermo', title: 'No temperature readings in this period', text: 'The car was asleep or not logged the whole time. Choose a longer period in the period picker.' }));
      return;
    }
    const conv = Object.fromEntries(Object.entries(raw).map(([k, a]) => [k, a.map((x) => (x == null ? null : convert(x, 'temperature')))]));
    const keys = ['outside', 'cabin', 'battery', 'setpoint'];
    const noSet = !raw.setpoint.some((x) => x != null);
    const stat = (a) => { const f = a.filter((x) => x != null); if (!f.length) return null; return { min: Math.min(...f), max: Math.max(...f), mean: f.reduce((s, x) => s + x, 0) / f.length }; };
    const plot = {};
    const chips = keys.map((k, idx) => {
      const s = SERIES[k];
      const b = h('button', { class: 'legend-chip', type: 'button', 'aria-pressed': 'true', title: 'Show or hide this line', cover: s.cover },
        h('i', { class: ['legend-swatch', k === 'setpoint' && 'is-dashed'], vars: { '--c': `var(${s.color})` } }), h('span', null, s.label), h('b', { class: 'legend-val' }, ''));
      b.addEventListener('click', () => { const vis = b.getAttribute('aria-pressed') !== 'true'; b.setAttribute('aria-pressed', String(vis)); plot.p?.u?.setSeries(idx + 1, { show: vis }); });
      return b;
    });
    const at = h('span', { class: 'faint ef-readout' }, 'Point at the chart for the values at a time.');
    const tUnit = unitLabel('temperature');
    const bw = bucketWords(bms);
    const statsTbl = h('div', { class: 'tbl-scroll' }, h('table', { class: 'tbl tp-stats', 'aria-label': `Lines in the chart, from ${bw} averages: lowest, average, highest and last` },
      h('thead', null, h('tr', null, h('th', { scope: 'col' }, 'Line'), [`Lowest ${bw} average`, 'Average', `Highest ${bw} average`, `Last ${bw} average`].map((l) => h('th', { scope: 'col', class: 'is-num' }, l)))),
      h('tbody', null, keys.map((k) => { const sv = stat(raw[k]); const last = [...raw[k]].reverse().find((x) => x != null);
        return h('tr', null, h('td', { class: 'ef-rowhead' }, h('i', { class: ['legend-swatch', k === 'setpoint' && 'is-dashed'], vars: { '--c': `var(${SERIES[k].color})` } }), SERIES[k].label),
          sv ? [h('td', { class: 'is-num' }, format(sv.min, 'temperature')), h('td', { class: 'is-num' }, format(sv.mean, 'temperature')), h('td', { class: 'is-num' }, format(sv.max, 'temperature')), h('td', { class: 'is-num' }, format(last, 'temperature'))]
            : h('td', { class: 'faint', colspan: 4 }, k === 'setpoint' ? 'Climate was not on, or this car does not send its set point' : 'No reading')); }))));
    cardLabels(statsTbl.querySelector('table'));
    const plotEl = h('div', { class: 'chart tp-hist-chart' });
    replace(tHistSlot,
      h('div', { class: 'tp-top' }, h('div', { class: 'legend', role: 'group', 'aria-label': 'Temperature lines' }, chips), at),
      plotEl,
      statsTbl,
      h('p', { class: 'note' }, h('i', { class: 'k-asleep', 'aria-hidden': 'true' }), ' Asleep (blue) and ', h('i', { class: 'k-unknown', 'aria-hidden': 'true' }), ` unknown (grey) time: the car reads nothing then, so the lines break instead of guessing. The dashed set point is the driver’s climate setting, drawn only while climate runs${noSet ? ' (none in this period)' : ''}. Values are ${tUnit}, averaged per ${bw}, so the table’s lowest and highest are of those averages: the single readings in the summary above reach further.`));
    const xs = ts.map((t) => t / 1000);
    const make = (width, height) => {
      const c = colors();
      return {
        width, height, padding: [rpx(10), rpx(14), 0, 0], legend: { show: false },
        cursor: { sync: { key: sync, setSeries: false }, drag: { x: false, y: false }, points: { size: rpx(7), width: rpx(2), stroke: c.cursor } },
        scales: { x: { time: true, min: fromMs / 1000, max: toMs / 1000 }, y: { auto: true } },
        axes: [xAxis(tz, span), { ...valueAxis({ size: 46 }), values: (u, vv) => vv.map((n) => `${formatNumber(n, { maximumFractionDigits: 0 })}°`) }],
        series: [{}, ...keys.map((k) => {
          const col = cssVar(SERIES[k].color);
          return k === 'setpoint'
            ? { label: SERIES[k].label, stroke: col, width: rpx(1.5), dash: [rpx(5), rpx(4)], paths: window.uPlot.paths.stepped({ align: 1 }), points: { show: false }, spanGaps: false }
            : { label: SERIES[k].label, stroke: col, width: rpx(k === 'outside' ? 2 : 1.75), points: { show: false }, spanGaps: false };
        })],
        hooks: {
          drawClear: [stateShade(ts, st, bms)],
          setCursor: [(u) => {
            const i = u.cursor.idx;
            if (i == null || u.cursor.left < 0) { chips.forEach((ch) => { ch.querySelector('.legend-val').textContent = ''; }); at.textContent = 'Point at the chart for the values at a time.'; return; }
            keys.forEach((k, j) => { chips[j].querySelector('.legend-val').textContent = format(raw[k][i], 'temperature'); });
            at.textContent = `${formatDateTime(ts[i], tz, { now: now() })}${st[i] && st[i] !== 'driving' && st[i] !== 'parked' && st[i] !== 'charging' ? ` · ${st[i]}` : ''}`;
          }],
        },
      };
    };
    plot.p = mountPlot(plotEl, make, [xs, ...keys.map((k) => conv[k])], ctx);
  }

  async function paintCharges() {
    const items = await ctx.api.list(`${base}/charges`, { ...ctx.params, sort: 'start', dir: 'asc' }, { limit: 200, max: 3000 }).all();
    if (!ctx.alive()) return;
    const list = items.filter((c) => c.start_ms != null).sort((a, b) => a.start_ms - b.start_ms);
    const withT = list.filter((c) => c.outside_avg_c != null);
    replace(tChgSide, csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'charging-temperatures', period.preset), list.map((c) => ({ t_ms: c.start_ms, place: c.place?.name || c.address?.short || '', kind: c.kind, outside: c.outside_avg_c, battery: c.battery_avg_c, added: c.energy_added_kwh })),
      [timeColumn('Start', 't_ms', tz), column('Place', 'place'), column('Kind', 'kind'), column('Outside average', 'outside', 'temperature', { digits: 1 }), column('Battery average', 'battery', 'temperature', { digits: 1 }), column('Energy added', 'added', 'energy', { digits: 2 })]) }));
    if (!withT.length) {
      replace(tChgSlot, h('div', { cover: 'tp.chart.charging_outside' }, emptyState({ icon: 'bolt', title: list.length ? 'No charge has an outside temperature' : 'No charge in this period', text: list.length ? 'The car sent no outside temperature while charging.' : 'Charges that start in the period show here. Choose a longer period in the period picker.' })));
      return;
    }
    const sv = withT.map((c) => c.outside_avg_c);
    const mean = sv.reduce((s, x) => s + x, 0) / sv.length;
    const link = h('span', { class: 'ef-readout' }, h('span', { class: 'faint' }, 'Point at a dot (or tap it); click it, or tap it again, to open that charge.'));
    const plotEl = h('div', { class: 'chart tp-chg-chart', cover: 'tp.chart.charging_outside' });
    replace(tChgSlot,
      h('div', { class: 'tp-top' },
        h('span', { class: 'tp-figs' }, `${n0(withT.length)} charges · lowest ${format(Math.min(...sv), 'temperature')} · average ${format(mean, 'temperature')} · highest ${format(Math.max(...sv), 'temperature')}`),
        link),
      plotEl,
      note('One dot per charge at its start, at the charge’s average outside temperature. Cold charges are slower and use more energy to warm the pack.'));
    const xs = withT.map((c) => c.start_ms / 1000);
    const ys = withT.map((c) => convert(c.outside_avg_c, 'temperature'));
    let hover = null;
    let tapped = null;
    // on a touch screen the first tap on a dot shows its charge above the chart, a second tap opens it
    const open = (e) => { if (hover == null) return; if (e?.pointerType === 'touch' && tapped !== hover) { tapped = hover; return; } ctx.navigate(state.buildUrl('charge', { car: cid, charge: withT[hover].id })); };
    const make = (width, height) => {
      const c = colors();
      const col = cssVar('--c-charge');
      return {
        width, height, padding: [rpx(10), rpx(14), 0, 0], legend: { show: false },
        cursor: { sync: { key: sync, setSeries: false }, drag: { x: false, y: false }, points: { size: rpx(10), width: rpx(2), stroke: c.cursor } },
        scales: { x: { time: true, min: fromMs / 1000, max: toMs / 1000 }, y: { auto: true, range: (u, a, b) => [Math.floor(a - 2), Math.ceil(b + 2)] } },
        axes: [xAxis(tz, span), { ...valueAxis({ size: 46 }), values: (u, vv) => vv.map((n) => `${formatNumber(n, { maximumFractionDigits: 0 })}°`) }],
        series: [{}, { label: 'Outside at charge', stroke: col, width: 0, paths: () => null, points: { show: true, size: rpx(7), width: rpx(1), fill: hexA(col, 0.8), stroke: col } }],
        hooks: { setCursor: [(u) => {
          const i = u.cursor.idx;
          if (i == null || u.cursor.left < 0 || Math.abs(u.valToPos(xs[i], 'x') - u.cursor.left) > rpx(24)) { hover = null; replace(link, h('span', { class: 'faint' }, 'Point at a dot (or tap it); click it, or tap it again, to open that charge.')); u.over.classList.remove('is-link'); return; }
          hover = i;
          u.over.classList.add('is-link');
          const ch = withT[i];
          replace(link, h('a', { class: 'link', href: state.buildUrl('charge', { car: cid, charge: ch.id }) }, `${formatDateTime(ch.start_ms, tz, { now: now() })} · ${ch.place?.name || ch.address?.short || 'Unnamed spot'} · ${format(ch.outside_avg_c, 'temperature')}`));
        }] },
      };
    };
    mountPlot(plotEl, make, [xs, ys], ctx, (u) => { u.over.addEventListener('click', open); });
  }

  async function paintDaily() {
    if (temps.error) { replace(tDaySlot, errorState(temps.error, { compact: true, home: false, retry: () => ctx.reload() })); return; }
    const days = temps.daily || [];
    replace(tDaySide, csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'daily-outside-temperature', period.preset), days, [column('Day', 'date'), column('Lowest', 'min_c', 'temperature', { digits: 1 }), column('15th percentile', 'p15_c', 'temperature', { digits: 1 }), column('85th percentile', 'p85_c', 'temperature', { digits: 1 }), column('Highest', 'max_c', 'temperature', { digits: 1 }), column('Awake share of the day', 'coverage_pct', 'percent', { digits: 1 })]) }));
    const help = h('p', { class: 'note', cover: 'tp.chart.daily_envelope_help' }, h('i', { class: 'k-whisker', 'aria-hidden': 'true' }), ' The thin line runs from the day’s lowest to its highest reading; ', h('i', { class: 'k-box', 'aria-hidden': 'true' }), ' the box holds the middle 70 % of readings (15th to 85th percentile). ', h('i', { class: 'k-cov', 'aria-hidden': 'true' }), ' The bars at the bottom are how much of the day the car was awake: a day with a small bar was measured only for a short time, so trust it less. A day with no box had no reading.');
    if (!days.some((x) => x.max_c != null)) {
      replace(tDaySlot, h('div', { cover: ['tp.chart.daily_envelope', 'tp.chart.daily_coverage'] }, emptyState({ icon: 'calendar', title: 'No day with an outside reading', text: 'The car was asleep or not logged the whole period. Choose a longer period in the period picker.' })), help);
      return;
    }
    const readout = h('span', { class: 'faint ef-readout' }, 'Point at a day for its figures.');
    const plotEl = h('div', { class: 'chart tp-day-chart', cover: ['tp.chart.daily_envelope', 'tp.chart.daily_coverage'] });
    replace(tDaySlot, h('div', { class: 'tp-top' }, h('span', { class: 'tp-figs' }, `${n0(days.length)} days`), readout), plotEl, help);
    // each day sits in the middle of its part inside the plot, so today (still running) and a first day
    // cut by the period start stay visible and can be pointed at
    const xs = days.map((x, i) => {
      const a = Math.max(x.t_ms, fromMs), b = Math.min(days[i + 1]?.t_ms ?? x.t_ms + DAY, toMs);
      return (b > a ? (a + b) / 2 : x.t_ms + DAY / 2) / 1000;
    });
    const T = (k) => days.map((x) => (x[k] == null ? null : convert(x[k], 'temperature')));
    const mn = T('min_c'), p15 = T('p15_c'), p85 = T('p85_c'), mx = T('max_c');
    const cov = days.map((x) => x.coverage_pct ?? 0);
    const make = (width, height) => {
      const c = colors();
      const col = cssVar('--warm'), covCol = cssVar('--live');
      return {
        width, height, padding: [rpx(10), rpx(14), 0, 0], legend: { show: false },
        cursor: { sync: { key: sync, setSeries: false }, drag: { x: false, y: false }, points: { show: false } },
        scales: { x: { time: true, min: fromMs / 1000, max: toMs / 1000 }, y: { auto: true, range: (u, a, b) => { const pad = Math.max(1, (b - a) * 0.08); return [a - pad - (b - a) * 0.3, b + pad]; } }, cov: { range: [0, 100] } },
        axes: [xAxis(tz, span), { ...valueAxis({ size: 46 }), values: (u, vv) => vv.map((n) => `${formatNumber(n, { maximumFractionDigits: 0 })}°`) }],
        series: [{}, ...[mn, p15, p85, mx].map((_, k) => ({ label: ['Lowest', '15th', '85th', 'Highest'][k], stroke: col, width: 0, paths: () => null, points: { show: false } })), { label: 'Awake', scale: 'cov', stroke: covCol, width: 0, paths: () => null, points: { show: false } }],
        hooks: {
          draw: [(u) => {
            const cx = u.ctx, { top, height: hgt } = u.bbox;
            const dayPx = Math.abs(u.valToPos(DAY / 1000, 'x', true) - u.valToPos(0, 'x', true));
            const pr = window.uPlot.pxRatio || 1;
            const w = Math.max(rpx(3) * pr, Math.min(rpx(14) * pr, dayPx * 0.62));
            cx.save();
            const covH = hgt * 0.2;
            for (let i = 0; i < xs.length; i++) {
              const x = u.valToPos(xs[i], 'x', true);
              // awake share: a bar in the bottom fifth
              const ch = (covH * Math.min(100, cov[i])) / 100;
              cx.fillStyle = hexA(covCol, cov[i] < 10 ? 0.25 : 0.4);
              cx.fillRect(x - w / 2, top + hgt - ch, w, Math.max(1, ch));
              if (mx[i] == null || mn[i] == null) continue;
              const y0 = u.valToPos(mx[i], 'y', true), y1 = u.valToPos(mn[i], 'y', true);
              cx.strokeStyle = col; cx.lineWidth = Math.max(1, rpx(1.25) * pr);
              cx.beginPath(); cx.moveTo(x, y0); cx.lineTo(x, y1); cx.stroke();
              if (p15[i] != null && p85[i] != null) {
                const b0 = u.valToPos(p85[i], 'y', true), b1 = u.valToPos(p15[i], 'y', true);
                cx.fillStyle = hexA(col, cov[i] < 10 ? 0.35 : 0.75);
                cx.fillRect(x - w / 2, b0, w, Math.max(rpx(2) * pr, b1 - b0));
              }
            }
            cx.restore();
          }],
          setCursor: [(u) => {
            const i = u.cursor.idx;
            if (i == null || u.cursor.left < 0) { readout.textContent = 'Point at a day for its figures.'; return; }
            const x = days[i];
            readout.textContent = x.max_c == null ? `${formatDate(x.t_ms, tz, { now: now() })}: no reading (awake ${pct(x.coverage_pct, 0)})`
              : `${formatDate(x.t_ms, tz, { now: now() })}: ${format(x.min_c, 'temperature')} to ${format(x.max_c, 'temperature')}, most ${format(x.p15_c, 'temperature')} to ${format(x.p85_c, 'temperature')} · awake ${pct(x.coverage_pct, 0)}`;
          }],
        },
      };
    };
    mountPlot(plotEl, make, [xs, mn, p15, p85, mx, cov], ctx);
  }

  /* ---------- helpers */
  function stateShade(ts, st, bms) {
    return (u) => {
      const cx = u.ctx, { left, top, width, height } = u.bbox;
      cx.save();
      const asleep = cssVar('--st-asleep'), unknown = cssVar('--st-unknown');
      for (let i = 0; i < ts.length; i++) {
        const s = st[i];
        if (s !== 'asleep' && s !== 'unknown') continue;
        let j = i; while (j + 1 < ts.length && st[j + 1] === s) j++;
        const a = Math.max(left, u.valToPos(ts[i] / 1000, 'x', true)), b = Math.min(left + width, u.valToPos((ts[j] + bms) / 1000, 'x', true));
        if (b > a) { cx.fillStyle = hexA(s === 'asleep' ? asleep : unknown, s === 'asleep' ? 0.12 : 0.24); cx.fillRect(a, top, Math.max(1, b - a), height); }
        i = j;
      }
      cx.restore();
    };
  }
  function segment(choices, cur, onPick, label) {
    let value = cur;
    const btns = choices.map(([v, l]) => h('button', { class: 'seg-btn', type: 'button', 'aria-pressed': String(v === cur), onclick: () => {
      if (v === value) return;
      value = v; btns.forEach((b, k) => b.setAttribute('aria-pressed', String(choices[k][0] === v))); onPick(v);
    } }, l));
    return h('span', { class: 'seg', role: 'group', 'aria-label': label, title: label }, btns);
  }
}

const SERIES = {
  outside: { label: 'Outside', color: '--warm', cover: 'tp.chart.outside' },
  cabin: { label: 'Cabin', color: '--live', cover: 'tp.chart.inside' },
  battery: { label: 'Battery', color: '--c-regen', cover: null },
  setpoint: { label: 'Set point (climate on)', color: '--c-elev', cover: 'tp.chart.setpoint' },
};

function bucketWords(ms) {
  if (ms >= DAY) return 'day';
  if (ms >= 3600000) return `${formatNumber(ms / 3600000, { maximumFractionDigits: 1 })} h`;
  if (ms >= 60000) return `${formatNumber(ms / 60000, { maximumFractionDigits: 0 })} min`;
  return `${formatNumber(ms / 1000, { maximumFractionDigits: 0 })} s`;
}
// long periods need month and year steps too, so the axis keeps its labels
function xAxis(tz, spanMs) {
  const ax = { ...timeAxis(tz, { spanMs }), incrs: [...TIME_INCRS, 5184000, 7776000, 15552000, 31536000] };
  if (spanMs > 300 * DAY) ax.values = (u, v) => v.map((t) => formatDate(t * 1000, tz, { weekday: false, month: 'short', year: true }));
  return ax;
}
const round1 = (v) => (v == null ? null : Math.round(v * 10) / 10);
const round3 = (v) => (v == null ? null : Math.round(v * 1000) / 1000);
