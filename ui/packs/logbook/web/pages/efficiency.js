// Sentry Mate, "Logbook" UI pack: Efficiency and temperatures (/cars/{car}/efficiency).
//
// How much energy driving takes, how the outside temperature changes it, and the temperatures themselves.
// Data: GET /cars/{car}/efficiency (net and gross consumption, logged distance, the temperature groups and
// the car's energy factor), /cars/{car}/temperatures (lowest, average, highest and coverage, per day),
// /cars/{car}/series (outside, cabin and battery temperature, set points and climate over time),
// /cars/{car}/charges (outside temperature of each charge), /cars/{car}/drives (the drives of a temperature
// group), /cars and /compare (other cars beside this one).
//
// Consumption is measured from the pack, so it needs no energy factor; the factor is shown only as a
// cross-check. Every temperature figure has its coverage next to it. Asleep time is shaded apart from time
// with no data, and a line never crosses time with no data. The page's filters (minimum distance,
// temperature group width, the cars compared) live in the address; the period is the frame's.

import { h, replace, cover } from '../shared/dom.js';
import { format, convert, toMetric, unitLabel, rangeLabel } from '../shared/units.js';
import { formatDate, formatDateTime, formatDuration, periodLabel, dateFormat, dayKey, buckets, zonedParts } from '../shared/time.js';
import { formatNumber, tn } from '../shared/i18n.js';
import { downloadCsv, csvFilename, column, timeColumn } from '../shared/csv.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { pageHead, q, figure, figures, icon, empty, errorView, loadingBlock, flag, button, sep } from '../core/ui.js';
import { dataTable } from '../core/table.js';
import { stackedCharts, spansFromStates, niceSplits, niceStep } from '../core/chart.js';
import { choice } from '../core/form.js';
import { zoneLabel } from '../core/format.js';

export const id = 'efficiency';
export const css = true;
export const title = () => 'Efficiency';

const num = (v) => v != null && Number.isFinite(v);
const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
const p3 = (v) => `${v.toFixed(3)}%`;
const COLOURS = ['--accent', '--ochre', '--dusk', '--slate', '--rust', '--ink-2'];
const FACTOR_SOURCE = {
  energy_and_range: 'worked out from the pack’s energy and the car’s range read in the same second',
  owner: 'typed in by the owner in Settings: Cars (it overrides the measured value)',
};

/** Group widths offered, in the person's temperature unit. */
const groupOptions = () => (unitLabel('temperature') === '°F' ? [['5', '5 °F'], ['10', '10 °F'], ['20', '20 °F']] : [['2', '2 °C'], ['5', '5 °C'], ['10', '10 °C']]);
const defaultGroup = () => (unitLabel('temperature') === '°F' ? '10' : '5');

/** A temperature in the person's unit as a bare number, no unit. */
const tNum = (c, digits = 0) => (num(c) ? formatNumber(convert(c, 'temperature'), { maximumFractionDigits: digits, minimumFractionDigits: digits }) : '—');
const groupText = (g) => `${tNum(g.temp_c)} to ${tNum(g.temp_to_c)} ${unitLabel('temperature')}`;
/** A y axis on nice steps that holds min and max: { splits, lo, hi }. */
function yScale(min, max) {
  if (!(max > min)) { min -= 1; max += 1; }
  const step = niceStep(max - min, 4);
  const lo = Math.floor(min / step) * step, hi = Math.ceil(max / step) * step;
  return { splits: niceSplits(lo, hi, 4), lo, hi };
}
const pctText = (v, digits = 0) => (num(v) ? `${formatNumber(v, { maximumFractionDigits: digits, minimumFractionDigits: digits })} %` : '—');

export async function mount(ctx) {
  const car = ctx.car;
  const tz = ctx.tz;
  const now = api.now();
  const pLabel = ctx.period ? periodLabel(ctx.period, tz, now) : 'All time';
  const file = (...p) => csvFilename(car.name, ...p, pLabel);

  /* ---------------------------------------------------------------- filters, from the address */

  const opt = {
    minKm: (() => { const n = parseFloat(ctx.query.min_distance_km); return Number.isFinite(n) && n >= 0 ? n : 1; })(),
    group: groupOptions().some(([k]) => k === ctx.query.group) ? ctx.query.group : defaultGroup(),
    compare: ctx.query.compare == null ? null : ctx.query.compare.split(',').filter(Boolean),
  };
  const groupC = () => clamp(toMetric(parseFloat(opt.group), 'temperature_delta'), 1, 20);
  const remember = () => ctx.setQuery({
    min_distance_km: opt.minKm === 1 ? null : String(+opt.minKm.toFixed(3)),
    group: opt.group === defaultGroup() ? null : opt.group,
    compare: opt.compare == null ? null : opt.compare.join(',') || 'none',
  });
  const effParams = () => ({ ...ctx.params, min_distance_km: opt.minKm, temp_group_c: +groupC().toFixed(4) });

  const head = (idName, text, note, ...actions) => h('div', { class: 'block-head' },
    h('h2', { class: 'label', id: idName }, text, note ? h('span', { class: 'label-note' }, note) : null),
    actions.filter(Boolean).length ? h('div', { class: 'block-actions' }, actions.filter(Boolean)) : null);
  const csvButton = (label, onclick, attrs = {}) => h('button', { type: 'button', class: 'button button-quiet button-small', onclick, 'aria-label': `Download CSV: ${label}`, ...attrs }, icon('download'), 'Download CSV');

  /* ---------------------------------------------------------------- the boxes, in page order */

  const filterBox = h('section', { class: 'block ef-filters', 'aria-labelledby': 'ef-filters-title' });
  const consBox = h('section', { class: 'block ef-cons', 'aria-labelledby': 'ef-cons-title' }, loadingBlock(3));
  const tempBox = h('section', { class: 'block ef-bytemp', 'aria-labelledby': 'ef-bytemp-title' }, loadingBlock(5));
  const factorBox = h('section', { class: 'block ef-factor', 'aria-labelledby': 'ef-factor-title' }, loadingBlock(3));
  const sumBox = h('div', { class: 'ef-tsum' }, loadingBlock(4));
  const histBox = h('div', { class: 'ef-hist' }, loadingBlock(6));
  const chargeBox = h('div', { class: 'ef-chg' }, loadingBlock(4));
  const dailyBox = h('div', { class: 'ef-daily' }, loadingBlock(4));
  const carsBox = h('section', { class: 'block ef-cars', 'aria-labelledby': 'ef-cars-title' }, loadingBlock(3));

  const part = (cls, summary, note, body, cover) => h('details', { class: ['ef-part', cls], open: true, cover },
    h('summary', { class: 'ef-part-head' }, icon('chevronDown', 'icon ef-part-icon'), h('span', { class: 'ef-part-name' }, summary), note ? h('span', { class: 'label-note' }, note) : null), body);

  const tempsBox = h('section', { class: 'block ef-temps', 'aria-labelledby': 'ef-temps-title', cover: ['tp.sections', 'shots.temperatures'] },
    head('ef-temps-title', 'Temperatures', 'outside, cabin and battery over the period'),
    part('ef-part-sum', 'Lowest, average and highest', 'with how much of the period has data', sumBox),
    part('ef-part-hist', 'Over time', 'one time axis; point at a moment to see it on every chart below', h('div', null, histBox, chargeBox)),
    part('ef-part-daily', 'Each day', 'the day’s full span and its typical band', dailyBox));

  ctx.main.append(
    pageHead({
      eyebrow: car.name, title: 'Efficiency and temperatures',
      deck: 'How much energy driving takes, how the outside temperature changes it, and the temperatures themselves. Energy is measured at the pack, so no conversion factor is needed.',
    }),
    filterBox,
    h('div', { class: 'ef-eff', cover: 'shots.efficiency' }, consBox, h('div', { class: 'ef-grid' }, tempBox, factorBox)),
    tempsBox,
    carsBox);

  /* ---------------------------------------------------------------- filters */

  const minIn = h('input', { id: 'ef-min', class: 'field-input ef-num', type: 'number', min: '0', step: '0.5', inputmode: 'decimal', 'aria-describedby': 'ef-min-hint' });
  minIn.value = String(+convert(opt.minKm, 'distance').toFixed(2));
  let minTimer = 0;
  minIn.addEventListener('input', () => {
    clearTimeout(minTimer);
    minTimer = setTimeout(() => {
      const v = parseFloat(minIn.value);
      opt.minKm = Number.isFinite(v) && v >= 0 ? toMetric(v, 'distance') : 0;
      remember();
      drivesCache = null; // loaded with the old shortest drive: a lower one needs drives it left out
      drawEfficiency().catch(() => {});
      drawCars().catch(() => {});
    }, 400);
  });
  ctx.onCleanup(() => clearTimeout(minTimer));
  const groupChoice = choice({
    label: 'Temperature groups', name: 'ef-group', value: opt.group, options: groupOptions(),
    onchange: (v) => { opt.group = v; remember(); drawEfficiency().catch(() => {}); },
  });
  const carPick = h('div', { class: 'field ef-carpick', cover: ['ef.section.per_car', 'tp.filter.car'] });
  replace(filterBox,
    head('ef-filters-title', 'Filters', 'they change every part of the page'),
    h('div', { class: 'ef-controls' },
      h('p', { class: 'ef-period', cover: ['ef.filter.time_range', 'tp.filter.time_range'] },
        h('span', { class: 'field-label' }, 'Period'), h('span', { class: 'ef-period-text' }, pLabel), sep(),
        h('span', null, `times and days in ${zoneLabel(tz)}`),
        button('Change the period', { cls: 'button-small', iconName: 'calendar', onclick: () => document.querySelector('.period-trigger')?.click() })),
      h('div', { class: 'field', cover: 'ef.filter.min_distance' },
        h('label', { class: 'field-label', for: 'ef-min' }, 'Shortest drive counted'),
        h('div', { class: 'ef-inline' }, minIn, h('span', { class: 'ef-unit' }, unitLabel('distance'))),
        h('p', { class: 'field-hint', id: 'ef-min-hint' }, 'Very short drives (moving the car in a driveway) use a lot per distance and skew the averages. 0 counts every drive.')),
      h('div', { class: 'field' }, groupChoice, h('p', { class: 'field-hint' }, 'How wide each outside-temperature group is in the table.')),
      carPick));

  /* ---------------------------------------------------------------- consumption, by temperature, factor */

  let eff = null;
  async function drawEfficiency() {
    replace(consBox, loadingBlock(3));
    replace(tempBox, loadingBlock(5));
    replace(factorBox, loadingBlock(3));
    try { eff = await ctx.api.get(api.apiPath`cars/${car.id}/efficiency`, effParams()); } catch (err) {
      if (ctx.signal.aborted) return;
      const retry = () => drawEfficiency();
      replace(consBox, head('ef-cons-title', 'Consumption'), errorView(err, { eyebrow: 'Consumption', retry }));
      replace(tempBox, head('ef-bytemp-title', 'By outside temperature'), errorView(err, { eyebrow: 'By outside temperature', retry }));
      replace(factorBox, head('ef-factor-title', 'Energy factor'), errorView(err, { eyebrow: 'Energy factor', retry }));
      return;
    }
    if (!ctx.alive()) return;
    drawConsumption(eff);
    drawByTemp(eff);
    drawFactor(eff);
  }

  function drawConsumption(e) {
    const noDrives = !e.drives;
    const minText = opt.minKm > 0 ? ` of ${format(opt.minKm, 'distance')} or more` : '';
    const total = num(e.energy_kwh) && num(e.parked_kwh) ? e.energy_kwh + e.parked_kwh : null;
    const csv = () => downloadCsv(file('consumption'), [e], [
      column('Net consumption', 'net_wh_per_km', 'efficiency', { digits: 1 }), column('Gross consumption', 'gross_wh_per_km', 'efficiency', { digits: 1 }),
      column('Logged distance', 'distance_km', 'distance', { digits: 3 }), column('Driving energy', 'energy_kwh', 'energy', { digits: 3 }),
      column('Parked energy', 'parked_kwh', 'energy', { digits: 3 }), { label: 'Drives', value: (r) => r.drives }, { label: 'Estimated drives', value: (r) => r.estimated_drives },
      column('Shortest drive counted', () => opt.minKm, 'distance', { digits: 3 }),
    ]);
    replace(consBox,
      head('ef-cons-title', 'Consumption', `drives${minText} in the period`, csvButton('consumption', csv)),
      figures([
        figure('Net consumption', q(e.net_wh_per_km, 'efficiency', { why: noDrives ? 'no drive in the period' : 'too little distance' }),
          ['Driving only: the pack’s energy out during drives, less what regenerative braking put back, divided by the distance. ', q(e.energy_kwh, 'energy', { digits: 1 }), ' over ', q(e.distance_km, 'distance'), '.'],
          { cls: 'fig-hero', cover: 'ef.stat.consumption_net' }),
        figure('Gross consumption', q(e.gross_wh_per_km, 'efficiency', { why: noDrives ? 'no drive in the period' : 'too little distance' }),
          ['Everything the car used, parked time included (', q(e.parked_kwh, 'energy', { digits: 1 }), ' while parked, from the pack’s own counters), divided by the same distance', total != null ? [': ', q(total, 'energy', { digits: 1 }), ' in all'] : '', '.'],
          { cls: 'fig-hero', cover: 'ef.stat.consumption_gross' }),
        figure('Logged distance', q(e.distance_km, 'distance', { why: 'no drive' }),
          [tn(e.drives || 0, '{n} drive', '{n} drives'), minText, ', added up from the recorded drives. Distance driven while nothing was logged is not in it.',
            e.estimated_drives ? [' ', flag(`${e.estimated_drives} estimated`, { kind: 'estimated' }), ' imported drives carry estimated energy.'] : ''],
          { cls: 'fig-hero', cover: 'ef.stat.logged_distance' }),
      ], { cls: 'ef-figs', cover: 'ef.help.headline_stats' }),
      h('p', { class: 'note ef-why', cover: 'faq.no-consumption' }, icon('info'),
        h('span', null, 'Consumption is measured from the first drive: there is no waiting for charges to learn a factor. A figure is blank only when no drive', minText,
          ' falls in the period, or when the drives are too short to divide by. Lower the shortest drive above, or choose a longer period. Imported drives without measured energy are marked estimated.')));
  }

  /* by temperature, with the drives of a group */

  let drivesCache = null; // the period's drives, loaded when a group is first opened
  let openKey = null;
  const drill = h('div', { class: 'ef-drill', 'aria-live': 'polite' });

  function drawByTemp(e) {
    const groups = e.by_temperature || [];
    const rows = groups.map((g) => ({ ...g, key: String(g.temp_c) }));
    if (e.no_temperature && e.no_temperature.drives) rows.push({ key: 'none', temp_c: null, temp_to_c: null, drives: e.no_temperature.drives, distance_km: e.no_temperature.distance_km, none: true });
    const maxWh = Math.max(0, ...rows.map((r) => r.wh_per_km || 0));
    const net = e.net_wh_per_km;
    openKey = null;
    replace(drill);
    const table = dataTable(ctx, {
      rows, itemName: ['group', 'groups'], label: 'Consumption by outside temperature', cover: 'ef.table.temperature',
      caption: 'Each drive goes in the group of its average outside temperature. Open a group for its drives.',
      csv: { filename: file('consumption by temperature'), columns: [
        column('From', 'temp_c', 'temperature', { digits: 2 }), column('To', 'temp_to_c', 'temperature', { digits: 2 }),
        { label: 'Drives', value: (r) => r.drives }, column('Distance', 'distance_km', 'distance', { digits: 3 }),
        column('Energy', 'energy_kwh', 'energy', { digits: 3 }), column('Consumption', 'wh_per_km', 'efficiency', { digits: 1 }),
        column('Range efficiency', 'range_efficiency_pct', 'percent', { digits: 1 }), column('Average speed', 'avg_kph', 'speed', { digits: 1 }),
      ] },
      columns: [
        { key: 'temp', label: 'Outside', th: true, sort: true, cover: 'ef.temp.col.temperature', value: (r) => (r.none ? -Infinity : r.temp_c),
          hint: 'The drive’s average outside temperature, from the car’s own sensor.',
          cell: (r) => h('button', { type: 'button', class: 'ef-group-btn', 'aria-expanded': 'false', dataset: { key: r.key }, onclick: (ev) => openGroup(r, ev.currentTarget) },
            icon('chevronRight', 'icon ef-open-icon'), h('span', { class: 'cell-main' }, r.none ? 'No temperature' : groupText(r)),
            h('span', { class: 'cell-sub' }, r.none ? 'the car sent none on these drives' : tn(r.drives, '{n} drive', '{n} drives'))) },
        { key: 'wh', label: 'Consumption', num: true, sort: true, cover: 'ef.temp.col.consumption', value: (r) => r.wh_per_km,
          hint: 'Energy used per distance on the drives of the group.',
          cell: (r) => (r.none ? q(null, 'efficiency', { why: 'not grouped' }) : [h('span', { class: 'cell-main' }, q(r.wh_per_km, 'efficiency')),
            num(net) && num(r.wh_per_km) ? h('span', { class: ['cell-sub', r.wh_per_km > net ? 'ef-worse' : 'ef-better'] }, `${r.wh_per_km >= net ? '+' : '−'}${pctText(Math.abs((r.wh_per_km / net - 1) * 100))} against the period`) : null]) },
        { key: 'bar', label: 'Compared', cls: 'ef-barcell', value: (r) => r.wh_per_km,
          cell: (r) => (num(r.wh_per_km) && maxWh ? h('span', { class: 'ef-bar', vars: { '--w': p3((r.wh_per_km / maxWh) * 100), '--n': num(net) ? p3((net / maxWh) * 100) : '-10%' }, role: 'img', 'aria-label': `${format(r.wh_per_km, 'efficiency')}${num(net) ? `; the period’s average is ${format(net, 'efficiency')}` : ''}` }) : '') },
        { key: 'eff', label: 'Range efficiency', num: true, sort: true, cover: 'ef.temp.col.efficiency', value: (r) => r.range_efficiency_pct,
          hint: `How far the car went for each unit of ${rangeLabel().toLowerCase()} it used: 100 % means the range shown on the screen came true.`,
          cell: (r) => (r.none ? q(null, 'percent', { why: 'not grouped' }) : [h('span', { class: 'cell-main' }, q(r.range_efficiency_pct, 'percent', { digits: 0 })), h('span', { class: 'cell-sub' }, 'of the range used')]) },
        { key: 'dist', label: 'Distance', num: true, sort: true, cover: 'ef.temp.col.distance', value: (r) => r.distance_km,
          hint: 'How much driving the group is based on. A small group is a rough figure.',
          cell: (r) => [h('span', { class: 'cell-main' }, q(r.distance_km, 'distance')), num(r.energy_kwh) ? h('span', { class: 'cell-sub' }, q(r.energy_kwh, 'energy', { digits: 1 })) : null] },
        { key: 'speed', label: 'Average speed', num: true, sort: true, cover: 'ef.temp.col.speed', value: (r) => r.avg_kph,
          hint: 'Distance divided by driving time. Faster driving uses more, whatever the weather.',
          cell: (r) => q(r.avg_kph, 'speed', { why: r.none ? 'not grouped' : 'no time' }) },
      ],
      sort: { key: 'temp', dir: 'asc' },
      empty: () => empty({ title: e.drives ? 'No drive with an outside temperature in this period.' : `No drive${opt.minKm > 0 ? ` of ${format(opt.minKm, 'distance')} or more` : ''} in this period.`, text: 'Lower the shortest drive above, or choose a longer period.' }),
    });
    replace(tempBox,
      head('ef-bytemp-title', 'By outside temperature', `groups of ${groupOptions().find(([k]) => k === opt.group)?.[1] || ''}`),
      table.el,
      h('p', { class: 'note' }, 'Cold drives use more: the cabin and the pack are heated, and cold tyres and air resist more. The bar shows each group’s consumption against the highest group’s (a full bar); the tick is the period’s average.'),
      drill);
    table.load();
  }

  async function openGroup(g, btn) {
    const was = openKey === g.key;
    tempBox.querySelectorAll('.ef-group-btn').forEach((b) => { b.setAttribute('aria-expanded', 'false'); b.closest('tr')?.classList.remove('is-open'); });
    if (was) { openKey = null; replace(drill); return; }
    openKey = g.key;
    btn.setAttribute('aria-expanded', 'true');
    btn.closest('tr')?.classList.add('is-open');
    replace(drill, loadingBlock(4));
    if (!drivesCache) {
      try { drivesCache = await ctx.api.list(api.apiPath`cars/${car.id}/drives`, { ...ctx.params, min_distance_km: opt.minKm || undefined, sort: 'start', dir: 'asc' }, { limit: 200 }).all({ max: 5000 }); } catch (err) {
        if (!ctx.signal.aborted) replace(drill, errorView(err, { eyebrow: 'Drives of the group', retry: () => openGroup(g, btn) }));
        return;
      }
      if (!ctx.alive() || openKey !== g.key) return;
    }
    const list = drivesCache.filter((d) => (opt.minKm ? (d.distance_km || 0) >= opt.minKm : true)
      && (g.none ? d.outside_avg_c == null : num(d.outside_avg_c) && d.outside_avg_c >= g.temp_c && d.outside_avg_c < g.temp_to_c));
    const t = dataTable(ctx, {
      rows: list, itemName: ['drive', 'drives'], label: `Drives at ${g.none ? 'no temperature' : groupText(g)}`,
      csv: { filename: file('drives', g.none ? 'no temperature' : groupText(g)), columns: [
        timeColumn('Start', 'start_ms', tz), timeColumn('End', 'end_ms', tz), { label: 'From', value: (d) => d.start?.label || '' }, { label: 'To', value: (d) => d.end?.label || '' },
        column('Distance', 'distance_km', 'distance', { digits: 3 }), column('Energy', 'energy_kwh', 'energy', { digits: 3 }), { label: 'Energy basis', value: (d) => d.energy_basis || '' },
        column('Consumption', 'wh_per_km', 'efficiency', { digits: 1 }), column('Outside', 'outside_avg_c', 'temperature', { digits: 1 }), column('Average speed', 'speed_avg_kph', 'speed', { digits: 1 }),
      ] },
      columns: [
        { key: 'start', label: 'Drive', th: true, sort: true, value: (d) => d.start_ms,
          cell: (d) => [h('a', { class: 'cell-main', href: state.buildUrl('drive', { car: car.id, drive: d.id, period: ctx.state.period }) }, formatDateTime(d.start_ms, tz, { now })),
            h('span', { class: 'cell-sub' }, `${d.start?.label || 'an unnamed spot'} to ${d.end?.label || 'an unnamed spot'}`)] },
        { key: 'dist', label: 'Distance', num: true, sort: true, value: (d) => d.distance_km, cell: (d) => q(d.distance_km, 'distance') },
        { key: 'wh', label: 'Consumption', num: true, sort: true, value: (d) => d.wh_per_km,
          cell: (d) => [h('span', { class: 'cell-main' }, q(d.wh_per_km, 'efficiency', { why: 'not measured' })), d.energy_basis === 'estimated' ? h('span', { class: 'cell-sub' }, flag('estimated', { kind: 'estimated' })) : null] },
        { key: 'out', label: 'Outside', num: true, sort: true, value: (d) => d.outside_avg_c, cell: (d) => q(d.outside_avg_c, 'temperature', { why: 'not sent' }) },
        { key: 'speed', label: 'Average speed', num: true, sort: true, value: (d) => d.speed_avg_kph, cell: (d) => q(d.speed_avg_kph, 'speed') },
      ],
      sort: { key: 'start', dir: 'desc' },
      empty: () => empty({ title: 'No drive in this group any more.', text: 'The drives changed since the table was loaded; reload the page.' }),
    });
    replace(drill,
      h('div', { class: 'ef-drill-head' },
        h('p', { class: 'ef-sub' }, `Drives at ${g.none ? 'no outside temperature' : groupText(g)}`),
        h('a', { class: 'link-arrow', href: state.buildUrl('drives', { car: car.id, period: ctx.state.period, query: { min_distance_km: opt.minKm || null } }) }, 'Every drive of the period, all temperatures', icon('arrowRight')),
        button('Close', { cls: 'button-small', iconName: 'close', onclick: () => openGroup(g, btn) })),
      t.el);
    t.load();
  }

  function drawFactor(e) {
    const f = e.factor || {};
    const rows = [
      { key: 'rated', name: 'Rated range', wh: f.rated_wh_per_km, readings: f.readings, cover: ['ef.table.derived_rated', 'ef.rated.col.efficiency', 'ef.rated.col.count'] },
      { key: 'ideal', name: 'Ideal range', wh: f.ideal_wh_per_km, readings: f.readings, cover: ['ef.table.derived_ideal', 'ef.ideal.col.efficiency', 'ef.ideal.col.count'] },
    ];
    const mine = rangeLabel().toLowerCase().startsWith('ideal') ? rows[1] : rows[0];
    // a factor with no reading in the period comes from earlier readings (or the owner typed it)
    const earlier = f.source === 'owner' ? 'none in this period; the owner’s value is used' : 'none in this period; the factor comes from earlier readings';
    const when = f.as_of_ms ? [' Latest reading ', formatDate(f.as_of_ms, tz, { now }), '.']
      : num(mine.wh) && f.source !== 'owner' ? ' The date of the latest reading is not known.' : '';
    const fromEarlier = f.readings === 0 && num(mine.wh) && f.source !== 'owner' ? ' No reading falls in this period, so the value comes from earlier ones.' : '';
    const real = num(e.net_wh_per_km) && num(mine.wh) ? (mine.wh / e.net_wh_per_km) * 100 : null;
    const table = dataTable(ctx, {
      rows, itemName: ['factor', 'factors'], label: 'Energy factors',
      csv: { filename: file('energy factor'), columns: [{ label: 'Range kind', value: (r) => r.name }, column('Energy per distance of range', 'wh', 'efficiency', { digits: 1 }), { label: 'Readings', value: (r) => r.readings }, { label: 'Source', value: () => f.source || '' }] },
      columns: [
        { key: 'name', label: 'Range', th: true, value: (r) => r.name, cell: (r) => h('span', { class: 'cell-main' }, r.name, r === mine ? h('span', { class: 'cell-sub' }, 'the one you chose to see') : null) },
        { key: 'wh', label: 'Per distance of range', num: true, value: (r) => r.wh, cell: (r) => q(r.wh, 'efficiency', { why: 'no readings yet' }) },
        { key: 'n', label: 'Readings', num: true, value: (r) => r.readings, cell: (r) => (num(r.readings)
          ? [h('span', { class: 'cell-main' }, formatNumber(r.readings)), r.readings === 0 && num(r.wh) ? h('span', { class: 'cell-sub' }, earlier) : null]
          : q(null, 'count', { why: 'none' })) },
      ],
      rowAttrs: (r) => ({ cover: r.cover }),
    });
    replace(factorBox,
      head('ef-factor-title', 'Energy factor', 'a cross-check, not needed for the figures'),
      figures([figure(`${rangeLabel()} factor`, q(mine.wh, 'efficiency', { why: 'no readings yet' }),
        [`How much energy one ${unitLabel('distance')} of ${rangeLabel().toLowerCase()} stands for: ${FACTOR_SOURCE[f.source] || 'not worked out yet'}.`, fromEarlier, when],
        { cls: 'fig-hero', cover: 'ef.stat.current_factor' })], { cls: 'ef-figs-1' }),
      table.el,
      h('p', { class: 'note' }, real != null
        ? ['Driving used ', q(e.net_wh_per_km, 'efficiency'), ' in this period, so each ', unitLabel('distance'), ' of ', rangeLabel().toLowerCase(), ' took the car about ', h('b', null, pctText(real)), ' as far on the road.']
        : 'Once there are drives in the period, this compares the factor with what driving really used.'));
    table.load();
  }

  /* ---------------------------------------------------------------- temperatures */

  let temps = null;
  async function drawSummary() {
    try { temps = await ctx.api.get(api.apiPath`cars/${car.id}/temperatures`, ctx.params); } catch (err) {
      if (ctx.signal.aborted) return;
      replace(sumBox, errorView(err, { eyebrow: 'Temperatures', retry: () => drawSummary().then(drawDaily) }));
      replace(dailyBox, errorView(err, { eyebrow: 'Each day', retry: () => drawSummary().then(drawDaily) }));
      temps = null;
      return;
    }
    if (!ctx.alive()) return;
    const t = temps;
    const miss = t.missing_ms || {};
    const kinds = [
      ['outside', 'Outside', 'From the car’s outside sensor. It reads high in sun and in a warm garage, and settles once driving.', ['tp.outside.min', 'tp.outside.avg', 'tp.outside.max', 'tp.outside.coverage']],
      ['cabin', 'Cabin', 'Inside the car, next to the climate control.', ['tp.inside.min', 'tp.inside.avg', 'tp.inside.max', 'tp.inside.coverage']],
      ['battery', 'Battery', 'The pack’s own average temperature. A cold pack locks part of its energy.', [null, null, null, null]],
    ];
    const csv = () => downloadCsv(file('temperature summary'), kinds.map(([k, name]) => ({ name, ...(t[k] || {}) })), [
      { label: 'Where', value: (r) => r.name }, column('Lowest', 'min_c', 'temperature', { digits: 1 }), column('Average', 'avg_c', 'temperature', { digits: 1 }),
      column('Highest', 'max_c', 'temperature', { digits: 1 }), column('Coverage', 'coverage_pct', 'percent', { digits: 1 }),
    ]);
    const cell = (v, why, cover) => h('div', { class: 'ef-tcell', cover }, q(v, 'temperature', { digits: 1, why }));
    replace(sumBox,
      h('div', { class: 'ef-sub-head' }, h('p', { class: 'ef-sub' }, 'In the period'), csvButton('temperature summary', csv)),
      h('div', { class: 'ef-tgrid', role: 'table', 'aria-label': 'Lowest, average and highest temperature, with coverage' },
        h('div', { class: 'ef-trow ef-trow-head', role: 'row' }, ['', 'Lowest', 'Average', 'Highest', 'Has data'].map((x) => h('span', { role: 'columnheader' }, x))),
        kinds.map(([k, name, why, cv]) => {
          const v = t[k] || {};
          const none = !num(v.avg_c);
          const blank = none ? (k === 'cabin' ? 'not sent by this car' : 'no reading') : null;
          return h('div', { class: 'ef-trow', role: 'row' },
            h('div', { class: 'ef-tname', role: 'rowheader' }, h('span', { class: 'ef-tname-main' }, name), h('span', { class: 'cell-sub' }, why)),
            cell(v.min_c, blank, cv[0]), cell(v.avg_c, blank, cv[1]), cell(v.max_c, blank, cv[2]),
            h('div', { class: 'ef-tcell ef-cov', cover: cv[3] },
              q(v.coverage_pct, 'percent', { digits: 0, why: 'no data' }),
              h('span', { class: 'ef-covbar', vars: { '--w': p3(clamp(v.coverage_pct || 0, 0, 100)) }, 'aria-hidden': 'true' })));
        })),
      h('p', { class: 'note ef-help', cover: 'tp.summary.help' }, icon('info'),
        h('span', null, 'The average is taken over the time the car was awake and sending, not per drive, so a long day parked in the sun counts more than a short drive. ‘Has data’ is the share of the period with readings: the car only reads temperatures while awake. In this period it slept ',
          h('b', null, formatDuration(miss.asleep || 0)), ' (nothing changes while asleep) and there was no data for ', h('b', { class: 'ef-unknown' }, formatDuration(miss.unknown || 0)),
          ' (no log). With little coverage, treat the figures as a sample, not the whole period.')));
  }

  // the moment shown on every temperature chart (shared crosshair): set by any of them
  const marks = new Set();
  let plotsRef = null;
  const showMoment = (ms) => { for (const m of marks) m(ms); };
  const pointUplot = (ms) => {
    const p = plotsRef && plotsRef.plots && plotsRef.plots[0];
    if (!p) return;
    if (ms == null) { p.setCursor({ left: -10, top: -10 }); return; }
    const left = p.valToPos(ms / 1000, 'x');
    if (left >= 0 && left <= p.bbox.width / (window.devicePixelRatio || 1) + 1) p.setCursor({ left, top: 10 });
  };

  async function drawHistory() {
    const readout = h('p', { class: 'readout ef-readout', 'aria-live': 'off' });
    const charts = h('div', { class: 'charts' });
    let d = null;
    const csv = h('button', { type: 'button', class: 'button button-quiet button-small', disabled: true, 'aria-label': 'Download CSV: temperatures over time', onclick: () => d && downloadCsv(file('temperatures'), d.t_ms.map((t, i) => ({ t_ms: t, i })), [
      timeColumn('Time', 't_ms', tz), { label: 'State', value: (r) => d.state?.[r.i] ?? '' },
      column('Outside', (r) => d.values.outside_c?.[r.i], 'temperature', { digits: 2 }), column('Cabin', (r) => d.values.cabin_c?.[r.i], 'temperature', { digits: 2 }),
      column('Battery', (r) => d.values.battery_c?.[r.i], 'temperature', { digits: 2 }), { label: 'Climate', value: (r) => d.values.climate?.[r.i] ?? '' },
      column('Set point driver', (r) => d.values.setpoint_left_c?.[r.i], 'temperature', { digits: 1 }), column('Set point passenger', (r) => d.values.setpoint_right_c?.[r.i], 'temperature', { digits: 1 }),
      { label: 'Coverage (%)', value: (r) => d.coverage_pct?.[r.i] ?? '' },
    ]) }, icon('download'), 'Download CSV');
    const key = h('ul', { class: 'ef-key' },
      h('li', null, h('span', { class: 'ef-sw ef-sw-out', 'aria-hidden': 'true' }), 'outside'),
      h('li', { cover: 'tp.chart.inside' }, h('span', { class: 'ef-sw ef-sw-cabin', 'aria-hidden': 'true' }), 'cabin'),
      h('li', { cover: 'tp.chart.setpoint' }, h('span', { class: 'ef-sw ef-sw-set', 'aria-hidden': 'true' }), 'set point, only while the climate runs'),
      h('li', null, h('span', { class: 'ef-sw ef-sw-bat', 'aria-hidden': 'true' }), 'battery'),
      h('li', null, h('span', { class: 'ef-sw ef-sw-asleep', 'aria-hidden': 'true' }), 'asleep'),
      h('li', null, h('span', { class: 'ef-sw ef-sw-unknown', 'aria-hidden': 'true' }), 'no data'));
    replace(histBox,
      h('div', { class: 'ef-sub-head' }, h('p', { class: 'ef-sub' }, 'Temperatures'), csv),
      key, readout, h('div', { cover: ['tp.control.shared_crosshair', 'tp.chart.outside'] }, charts));
    replace(charts, loadingBlock(5));
    try {
      d = await ctx.api.get(api.apiPath`cars/${car.id}/series`, { ...ctx.params, fields: 'outside_c,cabin_c,battery_c,setpoint_left_c,setpoint_right_c,climate', max_points: 1000 });
    } catch (err) {
      if (!ctx.signal.aborted) replace(charts, errorView(err, { eyebrow: 'Temperatures over time', retry: () => drawHistory() }));
      return;
    }
    if (!ctx.alive()) return;
    const v = d.values || {};
    replace(charts);
    if (!(d.t_ms || []).length || !['outside_c', 'cabin_c', 'battery_c'].some((k) => (v[k] || []).some(num))) {
      replace(charts, empty({ title: 'No temperature readings in this period.', text: 'The car reads temperatures only while awake. Choose a longer period with the period picker at the top.' }));
      return;
    }
    csv.disabled = false;
    const climateOn = (i) => v.climate && v.climate[i] != null && v.climate[i] !== 'off';
    const setL = (v.setpoint_left_c || []).map((x, i) => (climateOn(i) ? x : null));
    const setR = (v.setpoint_right_c || []).map((x, i) => (climateOn(i) ? x : null));
    const states = d.state || null;
    const idle = () => replace(readout, h('span', { class: 'readout-idle' }, `${tn(d.t_ms.length, '{n} point', '{n} points')}, one per ${formatDuration(d.bucket_ms || 3600000)}. Point at a chart for every value at that moment; drag to zoom, double-click to zoom out.`));
    idle();
    const CLIMATE = { on: 'climate on', precondition: 'preconditioning', overheat_protect: 'overheat protection' };
    plotsRef = await stackedCharts(ctx, charts, {
      tz, t_ms: d.t_ms, states, spans: spansFromStates(d.t_ms, states, d.bucket_ms),
      rows: [
        { key: 'out', name: 'Outside', kind: 'temperature', values: v.outside_c || [], colour: '--slate', ticks: 4, note: 'average per point' },
        { key: 'cabin', name: 'Cabin', kind: 'temperature', values: v.cabin_c || [], colour: '--accent', ticks: 4, note: 'dashed: set point while climate runs',
          extra: [{ values: setL, colour: '--ochre', dash: true }, { values: setR, colour: '--ochre', dash: true }] },
        { key: 'bat', name: 'Battery', kind: 'temperature', values: v.battery_c || [], colour: '--dusk', ticks: 4, note: 'the pack’s average' },
      ],
      onHover: (i) => {
        if (i == null) { idle(); showMoment(null); return; }
        const val = (arr) => (arr && arr[i] != null ? format(arr[i], 'temperature') : '—');
        replace(readout, h('time', { class: 'readout-time' }, formatDateTime(d.t_ms[i], tz, { now })),
          states && states[i] ? h('span', null, states[i] === 'unknown' ? 'no data' : states[i]) : null,
          h('span', null, 'outside ', h('b', null, val(v.outside_c))), h('span', null, 'cabin ', h('b', null, val(v.cabin_c))),
          setL[i] != null ? h('span', null, `set ${CLIMATE[v.climate[i]] ? `(${CLIMATE[v.climate[i]]}) ` : ''}`, h('b', null, format(setL[i], 'temperature'))) : null,
          h('span', null, 'battery ', h('b', null, val(v.battery_c))));
        showMoment(d.t_ms[i] + (d.bucket_ms || 0) / 2);
      },
    });
    cover(charts.querySelector('.chart-cabin'), 'tp.chart.inside');
  }

  /**
   * A small time axis for the hand-drawn charts: a tick on each shown day's local midnight (every Nth day,
   * about six), labelled with that day. starts: the midnights, else those in [a, b); xOf: where a midnight
   * sits, in % (default: by time from a to b). Returns [x %, label].
   */
  function dayTicks(a, b, starts, xOf = null) {
    const span = Math.max(1, b - a);
    const days = (starts || buckets(a, b, 'day', tz, { max: 20000 }).map((x) => x.start_ms)).filter((ms) => ms >= a && ms < b);
    const long = span > 200 * 86400000;
    const fmt = dateFormat(tz, long ? { month: 'short', year: 'numeric' } : { month: 'short', day: 'numeric' });
    // long periods: ticks on the first day of a month; otherwise every Nth day
    const pool = long ? days.filter((ms) => zonedParts(ms, tz).day === 1) : days;
    const every = Math.max(1, Math.ceil(pool.length / 6));
    const out = [];
    for (let i = 0; i < pool.length; i += every) {
      const x = xOf ? xOf(pool[i]) : ((pool[i] - a) / span) * 100;
      if (x > 88 && out.length) break; // no room for a label at the right edge
      out.push([p3(x), fmt.format(pool[i])]);
    }
    return out;
  }

  async function drawCharges() {
    replace(chargeBox, loadingBlock(4));
    let list;
    try { list = await ctx.api.list(api.apiPath`cars/${car.id}/charges`, ctx.params, { limit: 200 }).all({ max: 3000 }); } catch (err) {
      if (!ctx.signal.aborted) replace(chargeBox, errorView(err, { eyebrow: 'Outside temperature while charging', retry: () => drawCharges() }));
      return;
    }
    if (!ctx.alive()) return;
    const pts = list.filter((c) => num(c.outside_avg_c));
    const csv = () => downloadCsv(file('charging temperatures'), list, [
      timeColumn('Start', 'start_ms', tz), timeColumn('End', 'end_ms', tz), { label: 'Kind', value: (c) => (c.kind || '').toUpperCase() }, { label: 'Place', value: (c) => c.place?.name || c.address?.short || '' },
      column('Outside', 'outside_avg_c', 'temperature', { digits: 1 }), column('Battery', 'battery_avg_c', 'temperature', { digits: 1 }), column('Energy added', 'energy_added_kwh', 'energy', { digits: 3 }),
      column('Average power', 'power_avg_kw', 'power', { digits: 1 }),
    ]);
    const headEl = h('div', { class: 'ef-sub-head' }, h('p', { class: 'ef-sub' }, 'Outside temperature while charging', h('span', { class: 'label-note' }, 'one dot per charge; open a dot for its details')), csvButton('charging temperatures', csv));
    if (!pts.length) {
      replace(chargeBox, headEl, empty({ title: list.length ? 'No charge in this period has an outside temperature.' : 'No charge in this period.', text: 'Choose a longer period with the period picker at the top.' }));
      cover(chargeBox, 'tp.chart.charging_outside');
      return;
    }
    const a = ctx.params.from_ms ?? Math.min(...list.map((c) => c.start_ms));
    const b = Math.min(ctx.period?.to_ms ?? now, now);
    const vals = pts.map((c) => convert(c.outside_avg_c, 'temperature'));
    const { splits, lo, hi } = yScale(Math.min(...vals) - 0.5, Math.max(...vals) + 0.5);
    const yOf = (v) => (1 - (v - lo) / (hi - lo)) * 100;
    const xOf = (ms) => clamp(((ms - a) / Math.max(1, b - a)) * 100, 0, 100);
    const cursor = h('span', { class: 'ef-cursor', hidden: true, 'aria-hidden': 'true' });
    const tip = h('p', { class: 'readout ef-readout ef-chg-tip' }, h('span', { class: 'readout-idle' }, `${tn(pts.length, '{n} charge', '{n} charges')}. Green dots are AC, ochre DC. Point at one for its figures.`));
    const dots = pts.map((c, i) => h('a', {
      class: ['ef-dot', `ef-dot-${c.kind === 'dc' ? 'dc' : 'ac'}`], href: state.buildUrl('charge', { car: car.id, charge: c.id, period: ctx.state.period }),
      vars: { '--x': p3(xOf(c.start_ms)), '--y': p3(yOf(vals[i])) },
      'aria-label': `${formatDateTime(c.start_ms, tz, { now })}, ${(c.kind || '').toUpperCase()} at ${c.place?.name || c.address?.short || 'an unnamed spot'}: outside ${format(c.outside_avg_c, 'temperature')}`,
      onmouseenter: () => { hoverCharge(c); pointUplot(c.start_ms); }, onfocus: () => hoverCharge(c), onmouseleave: () => pointUplot(null),
    }));
    function hoverCharge(c) {
      replace(tip, h('time', { class: 'readout-time' }, formatDateTime(c.start_ms, tz, { now })), h('span', null, (c.kind || '').toUpperCase(), ' at ', c.place?.name || c.address?.short || 'an unnamed spot'),
        h('span', null, 'outside ', h('b', null, format(c.outside_avg_c, 'temperature'))), h('span', null, 'battery ', h('b', null, format(c.battery_avg_c, 'temperature'))),
        h('span', null, 'added ', h('b', null, format(c.energy_added_kwh, 'energy'))));
    }
    marks.add((ms) => {
      if (ms == null || ms < a || ms > b) { cursor.hidden = true; return; }
      cursor.hidden = false;
      cursor.style.setProperty('--x', p3(xOf(ms)));
    });
    replace(chargeBox, headEl, tip,
      h('div', { class: 'ef-plot', cover: 'tp.chart.charging_outside' },
        h('div', { class: 'ef-y', 'aria-hidden': 'true' }, splits.map((s) => h('span', { vars: { '--y': p3(yOf(s)) } }, `${formatNumber(s)} ${unitLabel('temperature')}`))),
        h('div', { class: 'ef-area' },
          splits.map((s) => h('span', { class: 'ef-gline', vars: { '--y': p3(yOf(s)) }, 'aria-hidden': 'true' })),
          cursor, dots),
        h('div', { class: 'ef-x ef-x-days', 'aria-hidden': 'true' }, dayTicks(a, b).map(([x, t]) => h('span', { vars: { '--x': x } }, t)))));
  }

  function drawDaily() {
    if (!temps) return;
    const days = (temps.daily || []);
    const csv = () => downloadCsv(file('daily outside temperature'), days, [
      { label: 'Date', value: (r) => r.date }, column('Lowest', 'min_c', 'temperature', { digits: 1 }), column('Typical low (15th percentile)', 'p15_c', 'temperature', { digits: 1 }),
      column('Typical high (85th percentile)', 'p85_c', 'temperature', { digits: 1 }), column('Highest', 'max_c', 'temperature', { digits: 1 }), column('Coverage', 'coverage_pct', 'percent', { digits: 1 }),
    ]);
    const headEl = h('div', { class: 'ef-sub-head' }, h('p', { class: 'ef-sub' }, 'Outside temperature each day', h('span', { class: 'label-note' }, `days from midnight to midnight in ${zoneLabel(tz)}`)), csvButton('daily outside temperature', csv));
    const withData = days.filter((x) => num(x.min_c) && num(x.max_c));
    if (!withData.length) {
      replace(dailyBox, headEl, empty({ title: 'No outside temperature on any day of this period.', text: 'The car reads it only while awake. Choose a longer period with the period picker at the top.', cover: ['tp.chart.daily_envelope', 'tp.chart.daily_coverage', 'tp.chart.daily_envelope_help'] }));
      return;
    }
    const { splits, lo, hi } = yScale(Math.min(...withData.map((x) => convert(x.min_c, 'temperature'))), Math.max(...withData.map((x) => convert(x.max_c, 'temperature'))));
    const yOf = (c) => (1 - (convert(c, 'temperature') - lo) / (hi - lo)) * 100;
    const n = days.length;
    const w = 100 / n;
    const tip = h('p', { class: 'readout ef-readout' });
    const idle = () => replace(tip, h('span', { class: 'readout-idle' }, `${tn(withData.length, '{n} day', '{n} days')} with readings. Point at a day for its figures.`));
    idle();
    let onDay = -1;
    const dayEls = days.map((x, i) => {
      const has = num(x.min_c) && num(x.max_c);
      const thin = has && (x.coverage_pct || 0) < 10;
      return h('div', {
        class: ['ef-day', { 'is-thin': thin, 'is-empty': !has }], vars: { '--x': p3(i * w), '--w': p3(w) }, tabindex: '0', role: 'img',
        'aria-label': has ? `${formatDate(x.t_ms, tz, { now })}: ${format(x.min_c, 'temperature')} to ${format(x.max_c, 'temperature')}, mostly ${format(x.p15_c, 'temperature')} to ${format(x.p85_c, 'temperature')}; data for ${pctText(x.coverage_pct)} of the day` : `${formatDate(x.t_ms, tz, { now })}: no outside temperature`,
        onmouseenter: () => hoverDay(i, true), onfocus: () => hoverDay(i, true), onmouseleave: () => hoverDay(-1, true),
      },
      has ? [
        h('span', { class: 'ef-day-span', vars: { '--top': p3(yOf(x.max_c)), '--bot': p3(100 - yOf(x.min_c)) } }),
        num(x.p15_c) && num(x.p85_c) ? h('span', { class: 'ef-day-band', vars: { '--top': p3(yOf(x.p85_c)), '--bot': p3(100 - yOf(x.p15_c)) } }) : null,
      ] : null);
    });
    const covEls = days.map((x, i) => h('span', { class: ['ef-cov-day', { 'is-thin': (x.coverage_pct || 0) < 10 }], vars: { '--x': p3(i * w), '--w': p3(w), '--h': p3(clamp(x.coverage_pct || 0, 0, 100)) } }));
    function hoverDay(i, fromHere) {
      if (onDay >= 0) dayEls[onDay].classList.remove('is-on');
      onDay = i;
      if (i < 0) { idle(); if (fromHere) pointUplot(null); return; }
      dayEls[i].classList.add('is-on');
      const x = days[i];
      replace(tip, h('time', { class: 'readout-time' }, formatDate(x.t_ms, tz, { now })),
        num(x.min_c) ? [h('span', null, 'low ', h('b', null, format(x.min_c, 'temperature'))), h('span', null, 'mostly ', h('b', null, `${tNum(x.p15_c, 1)} to ${format(x.p85_c, 'temperature')}`)), h('span', null, 'high ', h('b', null, format(x.max_c, 'temperature')))] : h('span', null, 'no readings'),
        h('span', null, 'data for ', h('b', null, pctText(x.coverage_pct)), ' of the day'), (x.coverage_pct || 0) < 10 && num(x.min_c) ? h('span', { class: 'ef-unknown' }, 'too little to trust') : null);
      if (fromHere) pointUplot(x.t_ms + 12 * 3600000);
    }
    marks.add((ms) => {
      if (ms == null) { if (onDay >= 0) hoverDay(-1, false); return; }
      const i = days.findIndex((x, j) => ms >= x.t_ms && (j === n - 1 || ms < days[j + 1].t_ms));
      if (i >= 0 && i !== onDay) hoverDay(i, false);
    });
    // the axis ends with the last day shown; each day's tick sits at the left edge of its own column
    const a = days[0].t_ms, b = days[n - 1].t_ms + 1;
    const colOf = new Map(days.map((x, i) => [x.t_ms, i * w]));
    replace(dailyBox, headEl, tip,
      h('div', { class: 'ef-plot ef-plot-daily', cover: 'tp.chart.daily_envelope' },
        h('div', { class: 'ef-y', 'aria-hidden': 'true' }, splits.map((s) => h('span', { vars: { '--y': p3(yOf(toMetric(s, 'temperature'))) } }, `${formatNumber(s)} ${unitLabel('temperature')}`))),
        h('div', { class: 'ef-area' }, splits.map((s) => h('span', { class: 'ef-gline', vars: { '--y': p3(yOf(toMetric(s, 'temperature'))) }, 'aria-hidden': 'true' })), dayEls),
        h('div', { class: 'ef-y ef-y-cov', 'aria-hidden': 'true' }, h('span', { vars: { '--y': '0%' } }, '100 %'), h('span', { vars: { '--y': '100%' } }, '0 %')),
        h('div', { class: 'ef-covarea', cover: 'tp.chart.daily_coverage', role: 'img', 'aria-label': 'How much of each day had outside temperature data' }, covEls),
        h('div', { class: 'ef-x ef-x-days', 'aria-hidden': 'true' }, dayTicks(a, b, days.map((x) => x.t_ms), (ms) => colOf.get(ms)).map(([x, t]) => h('span', { vars: { '--x': x } }, t)))),
      h('ul', { class: 'ef-key' },
        h('li', null, h('span', { class: 'ef-sw ef-sw-span', 'aria-hidden': 'true' }), 'lowest to highest of the day'),
        h('li', null, h('span', { class: 'ef-sw ef-sw-band', 'aria-hidden': 'true' }), 'typical band: 70 % of the readings fell here (15th to 85th percentile)'),
        h('li', null, h('span', { class: 'ef-sw ef-sw-cov', 'aria-hidden': 'true' }), 'lower strip: share of the day with data')),
      h('p', { class: 'note ef-help', cover: 'tp.chart.daily_envelope_help' }, icon('info'),
        h('span', null, 'Read each day as a thin line from its lowest to its highest reading and a thick band where most of its readings fell. The car reads the outside only while awake, so a day it mostly slept shows just those hours: days with data for less than a tenth of the day are drawn faint and marked ‘too little to trust’.')));
  }

  /* ---------------------------------------------------------------- other cars */

  let fleet = [];
  async function drawCars() {
    replace(carsBox, head('ef-cars-title', 'Compare cars'), loadingBlock(3));
    if (!fleet.length) {
      try { fleet = await ctx.api.list('cars', { sort: 'name' }, { limit: 200 }).all({ max: 5000 }); } catch (err) {
        if (ctx.signal.aborted) return;
        replace(carsBox, head('ef-cars-title', 'Compare cars'), errorView(err, { eyebrow: 'Compare cars', retry: () => drawCars() }));
        replace(carPick, h('p', { class: 'field-label' }, 'Compare with'), h('p', { class: 'field-hint' }, 'The list of cars did not load.'));
        return;
      }
      if (!ctx.alive()) return;
      if (!fleet.some((c) => c.id === car.id)) fleet.unshift(car);
    }
    const others = fleet.filter((c) => c.id !== car.id);
    if (opt.compare == null) opt.compare = others.length <= 3 ? others.map((c) => c.id) : [];
    opt.compare = opt.compare.filter((idv) => others.some((c) => c.id === idv));
    drawPicker(others);
    const chosen = [car, ...others.filter((c) => opt.compare.includes(c.id))];
    if (!others.length) {
      replace(carsBox, head('ef-cars-title', 'Compare cars'), empty({ title: 'There is only this car to see.', text: 'Other cars your account may see would be compared here, side by side.' }));
      return;
    }
    const rows = await Promise.all(chosen.map(async (c) => {
      const [e, t] = await Promise.allSettled([
        ctx.api.get(api.apiPath`cars/${c.id}/efficiency`, { ...ctx.params, min_distance_km: opt.minKm }),
        ctx.api.get(api.apiPath`cars/${c.id}/temperatures`, ctx.params),
      ]);
      return { id: c.id, name: c.name, model: c.model, year: c.year, eff: e.status === 'fulfilled' ? e.value : null, temp: t.status === 'fulfilled' ? t.value : null, err: e.status === 'rejected', tempErr: t.status === 'rejected' };
    }));
    if (!ctx.alive()) return;
    const table = dataTable(ctx, {
      rows, itemName: ['car', 'cars'], label: 'Cars side by side',
      caption: `The same period and shortest drive for every car; days are this car’s (${zoneLabel(tz)}).`,
      csv: { filename: file('cars compared'), columns: [
        { label: 'Car', value: (r) => r.name }, column('Net consumption', (r) => r.eff?.net_wh_per_km, 'efficiency', { digits: 1 }), column('Gross consumption', (r) => r.eff?.gross_wh_per_km, 'efficiency', { digits: 1 }),
        column('Logged distance', (r) => r.eff?.distance_km, 'distance', { digits: 3 }), { label: 'Drives', value: (r) => r.eff?.drives ?? '' }, column('Parked energy', (r) => r.eff?.parked_kwh, 'energy', { digits: 3 }),
        column('Outside average', (r) => r.temp?.outside?.avg_c, 'temperature', { digits: 1 }), column('Cabin average', (r) => r.temp?.cabin?.avg_c, 'temperature', { digits: 1 }),
        column('Battery average', (r) => r.temp?.battery?.avg_c, 'temperature', { digits: 1 }), column('Temperature coverage', (r) => r.temp?.outside?.coverage_pct, 'percent', { digits: 1 }),
      ] },
      columns: [
        { key: 'name', label: 'Car', th: true, sort: true, value: (r) => r.name,
          cell: (r) => [h('span', { class: 'ef-carname' }, h('span', { class: 'ef-sw ef-sw-car', vars: { '--swatch': `var(${COLOURS[chosen.findIndex((c) => c.id === r.id) % COLOURS.length]})` }, 'aria-hidden': 'true' }),
            h('a', { class: 'cell-main', href: state.buildUrl('efficiency', { car: r.id, period: ctx.state.period }), 'aria-current': r.id === car.id ? 'page' : null }, r.name)),
          h('span', { class: 'cell-sub' }, [r.model, r.year].filter(Boolean).join(' '), r.id === car.id ? ' · shown above' : '')] },
        { key: 'net', label: 'Net', num: true, sort: true, value: (r) => r.eff?.net_wh_per_km, cell: (r) => (r.err ? 'could not load' : q(r.eff?.net_wh_per_km, 'efficiency', { why: 'no drive' })) },
        { key: 'gross', label: 'Gross', num: true, sort: true, value: (r) => r.eff?.gross_wh_per_km, cell: (r) => (r.err ? 'could not load' : q(r.eff?.gross_wh_per_km, 'efficiency', { why: 'no drive' })) },
        { key: 'dist', label: 'Logged distance', num: true, sort: true, value: (r) => r.eff?.distance_km, cell: (r) => (r.err ? 'could not load' : [h('span', { class: 'cell-main' }, q(r.eff?.distance_km, 'distance')), h('span', { class: 'cell-sub' }, tn(r.eff?.drives || 0, '{n} drive', '{n} drives'))]) },
        { key: 'out', label: 'Outside', num: true, sort: true, value: (r) => r.temp?.outside?.avg_c, cell: (r) => (r.tempErr ? 'could not load' : [h('span', { class: 'cell-main' }, q(r.temp?.outside?.avg_c, 'temperature', { why: 'no reading' })), num(r.temp?.outside?.coverage_pct) ? h('span', { class: 'cell-sub' }, `average; data ${pctText(r.temp.outside.coverage_pct)}`) : null]) },
        { key: 'cabin', label: 'Cabin', num: true, sort: true, value: (r) => r.temp?.cabin?.avg_c, cell: (r) => (r.tempErr ? 'could not load' : q(r.temp?.cabin?.avg_c, 'temperature', { why: 'not sent' })) },
        { key: 'bat', label: 'Battery', num: true, sort: true, value: (r) => r.temp?.battery?.avg_c, cell: (r) => (r.tempErr ? 'could not load' : q(r.temp?.battery?.avg_c, 'temperature', { why: 'not sent' })) },
      ],
    });
    const chartBox = h('div', { class: 'charts ef-cmp-charts' });
    const cmpLegend = h('ul', { class: 'ef-key' }, chosen.map((c, i) => h('li', null, h('span', { class: 'ef-sw ef-sw-car', vars: { '--swatch': `var(${COLOURS[i % COLOURS.length]})` }, 'aria-hidden': 'true' }), c.name)));
    const cmpReadout = h('p', { class: 'readout ef-readout' });
    let cmp = null;
    const cmpCsv = h('button', { type: 'button', class: 'button button-quiet button-small', disabled: true, 'aria-label': 'Download CSV: consumption per day, cars compared',
      onclick: () => cmp && downloadCsv(file('consumption per day, cars'), cmp.t.map((t, i) => ({ t, i })), [timeColumn('Day', 't', tz), ...cmp.lines.map((l) => column(l.name, (r) => l.values[r.i], 'efficiency', { digits: 1 }))]) }, icon('download'), 'Download CSV');
    replace(carsBox,
      head('ef-cars-title', 'Compare cars', tn(chosen.length, '{n} car side by side', '{n} cars side by side')),
      table.el,
      h('div', { class: 'ef-sub-head' }, h('p', { class: 'ef-sub' }, 'Net consumption per day', h('span', { class: 'label-note' }, 'one point per day with drives (per week in periods over 120 days)')), cmpCsv),
      cmpLegend, cmpReadout, chartBox);
    table.load();
    replace(chartBox, loadingBlock(3));
    try {
      const c = await ctx.api.get('compare', { cars: chosen.map((x) => x.id).join(','), metric: 'wh_per_km', ...ctx.params });
      if (!ctx.alive()) return;
      const series = c.series || [];
      // each car's points are its own local days (or weeks): line them up by that day, drawn at this car's midnight
      const own = series.find((s) => s.car === car.id) || series[0];
      const at = new Map();
      for (const s of [own, ...series]) (s?.t_ms || []).forEach((tt) => { const k = dayKey(tt, s.time_zone || tz); if (!at.has(k)) at.set(k, tt); });
      const keys = [...at.keys()].sort((x, y) => at.get(x) - at.get(y));
      const t = keys.map((k) => at.get(k));
      const lines = series.map((s) => { const m = new Map((s.t_ms || []).map((tt, i) => [dayKey(tt, s.time_zone || tz), s.values[i]])); return { name: s.name, values: keys.map((k) => (m.has(k) ? m.get(k) : null)) }; });
      cmp = { t, lines };
      replace(chartBox);
      if (!t.length) { replace(chartBox, empty({ title: 'No drive in this period for these cars.', text: 'Choose a longer period with the period picker at the top.' })); return; }
      cmpCsv.disabled = false;
      const idleText = () => replace(cmpReadout, h('span', { class: 'readout-idle' }, 'Point at the chart for each car’s figure that day. A day without drives is a gap.'));
      idleText();
      const colourOf = (name) => COLOURS[chosen.findIndex((x) => x.name === name) % COLOURS.length] || '--ink-2';
      await stackedCharts(ctx, chartBox, {
        tz, t_ms: t,
        rows: [{ key: 'cmp', name: 'Net consumption', kind: 'efficiency', values: lines[0]?.values || [], colour: colourOf(lines[0]?.name), ticks: 4, note: 'each car’s colour as in the table',
          extra: lines.slice(1).map((l) => ({ values: l.values, colour: colourOf(l.name) })) }],
        onHover: (i) => {
          if (i == null) { idleText(); return; }
          replace(cmpReadout, h('time', { class: 'readout-time' }, formatDate(t[i], tz, { now })), lines.map((l) => h('span', null, `${l.name} `, h('b', null, l.values[i] == null ? 'no drive' : format(l.values[i], 'efficiency')))));
        },
      });
    } catch (err) {
      if (!ctx.signal.aborted) replace(chartBox, errorView(err, { eyebrow: 'Cars compared', retry: () => drawCars() }));
    }
  }

  function drawPicker(others) {
    const search = h('input', { class: 'field-input field-search ef-carsearch', type: 'search', placeholder: 'Find a car', 'aria-label': 'Find a car to compare' });
    const listEl = h('ul', { class: 'ef-carlist' });
    const SHOW = 8;
    const drawList = () => {
      const qv = search.value.trim().toLowerCase();
      const match = others.filter((c) => !qv || `${c.name} ${c.model || ''}`.toLowerCase().includes(qv));
      const picked = match.filter((c) => opt.compare.includes(c.id));
      const rest = match.filter((c) => !opt.compare.includes(c.id));
      const show = [...picked, ...rest.slice(0, Math.max(0, SHOW - picked.length))];
      replace(listEl, show.map((c) => h('li', null, h('label', { class: 'ef-carcheck' },
        h('input', { type: 'checkbox', checked: opt.compare.includes(c.id), onchange: (ev) => {
          opt.compare = ev.target.checked ? [...opt.compare, c.id] : opt.compare.filter((x) => x !== c.id);
          remember();
          drawCars().catch(() => {});
        } }), h('span', null, c.name)))),
      match.length > show.length ? h('li', { class: 'ef-carmore' }, `${formatNumber(match.length - show.length)} more: type to find them`) : null,
      !match.length ? h('li', { class: 'ef-carmore' }, 'No car matches.') : null);
    };
    search.addEventListener('input', drawList);
    drawList();
    replace(carPick,
      h('p', { class: 'field-label' }, 'Compare with'),
      others.length ? [others.length > SHOW ? search : null, listEl,
        h('div', { class: 'ef-carbtns' },
          button('None', { cls: 'button-small', onclick: () => { opt.compare = []; remember(); drawCars().catch(() => {}); } }),
          others.length <= 20 ? button('Every car', { cls: 'button-small', onclick: () => { opt.compare = others.map((c) => c.id); remember(); drawCars().catch(() => {}); } }) : null)]
        : h('p', { class: 'field-hint' }, 'No other car to compare with.'),
      h('p', { class: 'field-hint' }, 'The cars chosen sit side by side under Compare cars, at the end of the page.'));
  }

  /* ---------------------------------------------------------------- load */

  // A drive or charge started or ended: the figures load again (quietly, a few seconds later).
  let rowsTimer = 0;
  ctx.events(['rows'], (ev) => {
    const d = ev.data || {};
    if (d.car && d.car !== car.id) return;
    clearTimeout(rowsTimer);
    rowsTimer = setTimeout(() => { if (ctx.alive()) { drivesCache = null; drawEfficiency().catch(() => {}); } }, 3000);
  });
  ctx.onCleanup(() => clearTimeout(rowsTimer));

  await Promise.all([drawEfficiency(), drawSummary().then(drawDaily), drawHistory(), drawCharges(), drawCars()]);
  ctx.ready();
}

