// Sentry Mate, "Pebble" UI pack: Battery (/cars/{car}/battery), how healthy the battery is, how the charge level is
// used and where energy goes while parked.
//
// A hero tile with the level ring and the everyday band, a ring of the capacity now against new (health), the
// full-charge range now and new, capacity over the car's life (by distance), the lifetime totals (driving and
// charging, AC and DC), the reference values an admin may type, the charge level over the period (with moving
// lines), the projected full-charge range against distance, level and temperature (one shared cursor), and the
// parked drain, one tile per spell.
// Data: GET /api/v1/cars/{car}/battery, /totals, /settings, /efficiency, /battery/capacity, /battery-level,
// /projected-range, /parked; PATCH /cars/{car}/settings.

import { h } from '../shared/dom.js';
import { format, formatMoney, convert, toMetric, unitLabel, rangeLabel } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, startOf, dayKey } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { tile, num, chip, icon, note, ring } from '../core/ui.js';
import { emptyTile, ghostTile, plotGhost } from '../core/states.js';
import { field, form, segmented, toggle } from '../core/form.js';
import { timeChart, xyChart } from '../core/chart.js';
import { statTile, facts, filterBar, pills, splitBar, sectionHead, nums, moreButton, AWAKE_WORDS } from '../core/bits.js';
import { srcPill, unionOf } from '../core/source.js';

export const id = 'battery';
export const css = true;
export const shape = [['full', 'h1'], [2, 'h3'], [2, 'h3'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1']];
export const title = () => 'Battery';

const DAY = 864e5;
const NEW_SOURCE = { owner: 'typed by the owner', early_bms: 'the pack’s earliest readings', profile: 'the model’s own figure', vehicle_info: 'the car’s build (VIN and CAN)' };
const RANGE_SOURCE = { owner: 'typed by the owner', first_30_days: 'the median of the first 30 days', profile: 'the model’s own figure', vehicle_info: 'the car’s build (VIN and CAN)' };
const CHEM = { lfp: 'LFP', nca_nmc: 'NCA or NMC' };
const pct = (v, d) => format(v, 'percent', d != null ? { digits: d } : undefined);
const n0 = (v) => (v == null ? '—' : formatNumber(v));

// The calls that start at once: [battery, totals, settings, efficiency], and the capacity chart's. prefetch()
// warms the same ones when a link here is under the pointer.
function firstReads(ctx) {
  const base = api.apiPath`cars/${ctx.car.id}`;
  return [ctx.api.get(`${base}/battery`), ctx.api.get(`${base}/totals`), ctx.api.get(`${base}/settings`), ctx.api.get(`${base}/efficiency`)];
}
const capacityReads = (ctx) => {
  const base = api.apiPath`cars/${ctx.car.id}`;
  return [
    ctx.api.list(`${base}/battery/capacity`, { group: 'day', x: 'odometer' }, { limit: 500 }).all({ max: 3000 }),
    ctx.api.get(`${base}/battery/capacity`, { group: 'month', x: 'odometer', limit: 500 }),
  ];
};
// the section controls of the address, and the period sections' reads
const optsOf = (q) => ({
  bucket: q.cl_bucket === 'hour' ? 'hour' : 'day', moving: q.cl_moving !== '0', window: q.cl_window === '30d' ? '30d' : '7d',
  prBucket: ['hour', 'day', 'week'].includes(q.pr_bucket) ? q.pr_bucket : 'auto', minH: Number(q.min_h) || 0,
});
const levelSpan = (ctx) => { const p = ctx.period; return [p.from_ms ?? api.now() - 365 * DAY, p.to_ms ?? api.now()]; };
const levelRead = (ctx, opt, from, to) => ctx.api.get(api.apiPath`cars/${ctx.car.id}/battery-level`, { from_ms: from, to_ms: to, bucket: to - from > 62 * DAY ? 'day' : opt.bucket, moving: opt.moving ? 'true' : 'false', window: opt.window });
function projectedParams(ctx, opt) {
  const p = ctx.period, params = { ...ctx.params };
  const span = (p.to_ms ?? api.now()) - (p.from_ms ?? api.now() - 365 * DAY);
  if (opt.prBucket !== 'auto' && !(opt.prBucket === 'hour' && span > 62 * DAY)) params.bucket = opt.prBucket; else params.max_points = 600;
  return params;
}
const parkedParams = (ctx, opt) => ({ ...ctx.params, ...(opt.minH ? { min_hours: opt.minH } : {}) });
export function prefetch(ctx) {
  const base = api.apiPath`cars/${ctx.car.id}`, opt = optsOf(ctx.query);
  return Promise.all([
    ...firstReads(ctx), ...capacityReads(ctx), levelRead(ctx, opt, ...levelSpan(ctx)),
    ctx.api.get(`${base}/projected-range`, projectedParams(ctx, opt)), ctx.api.pager(`${base}/parked`, parkedParams(ctx, opt), { limit: 12 }).more(),
  ]);
}

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz, period = ctx.period;
  const base = api.apiPath`cars/${car.id}`;
  const keep = (p) => p.catch(() => null);
  // Every call starts at once and each block paints when its own answers are in; until then a grey tile of its
  // size keeps its room, so the page shows what it has and nothing jumps when the slow parts arrive.
  const [batP, ...rest] = firstReads(ctx);
  const [totalsP, settingsP, effP] = rest.map(keep);
  const batOrNull = keep(batP);
  const opt = optsOf(ctx.query);

  const bar = filterBar(ctx, { carCover: ['bh.filter.car', 'cl.filter.car', 'pr.section.per_car', 'vd.section.per_car'], periodCover: ['cl.filter.time_range', 'pr.filter.time_range', 'vd.filter.time_range'] },
    h('span', { class: 'filter-note' }, 'The level, the projected range and the parked drain follow the period; health and lifetime figures do not.'));

  const nowS = ghostTile(2, 'h3'), healthS = ghostTile(2, 'h3'), rangeS = ghostTile(2, 'h2'), driveS = ghostTile(2, 'h3'), chargeS = ghostTile(2, 'h3'), refS = ghostTile(2, 'h3');
  const put = (slot, el) => { if (ctx.alive() && slot.parentNode) slot.replaceWith(el); };

  /* ------------------------------------------------------------ now, health, range: GET battery */
  const paintBattery = async () => {
    const [bat, eff] = await Promise.all([batP, effP]);
    if (!ctx.alive()) return;
    const cap = bat.capacity || {}, rng = bat.range || {}, band = bat.band || {};
    /* now: the level and the band */
    const lo = band.low_pct ?? 20, hi = band.high_pct ?? 80;
    const bandEl = h('div', { class: 'band', cover: 'bh.soc.band' },
      h('span', { class: 'band-track', 'aria-hidden': 'true' }, h('span', { class: 'band-low' }), h('span', { class: 'band-ok' }), h('span', { class: 'band-fill' }), h('span', { class: 'band-limit' })),
      h('span', { class: 'band-words' }, `Everyday band ${pct(lo)} to ${pct(hi)}${band.charge_limit_pct != null ? `; charge limit ${pct(band.charge_limit_pct)}` : ''}`));
    bandEl.querySelector('.band-fill').style.setProperty('--v', String(Math.max(0, Math.min(1, (bat.soc_pct ?? 0) / 100))));
    bandEl.querySelector('.band-ok').style.setProperty('--lo', String(lo / 100));
    bandEl.querySelector('.band-ok').style.setProperty('--hi', String(hi / 100));
    bandEl.querySelector('.band-limit').style.setProperty('--v', String((band.charge_limit_pct ?? hi) / 100));
    const nowT = tile({ w: 2, title: 'Now', icon: 'battery', cls: 'bat-now' },
      h('div', { class: 'bat-top' },
        h('div', { cover: 'bh.soc.current' }, ring(bat.soc_pct, { label: 'Battery', size: 'l' })),
        h('div', { class: 'bat-text' },
          h('div', { cover: 'bh.bar.stored_energy' }, h('span', { class: 'hero-k' }, 'Stored energy'), num(bat.energy_now_kwh, 'energy', { size: 'l', why: 'not reported' }),
            h('p', { class: 'tile-line' }, cap.usable_kwh ? `of ${format(cap.usable_kwh, 'energy')} usable when full` : 'usable energy in the pack now')),
          h('p', { class: 'tile-line' }, `${CHEM[bat.chemistry?.value] || 'Chemistry not known'}${bat.chemistry?.value ? `, ${bat.chemistry.source === 'owner' ? 'set by the owner' : 'detected'}${bat.chemistry?.confidence === 'low' ? ' (low confidence)' : ''}` : ''}${bat.brick_spread_v != null ? ` · cells within ${format(bat.brick_spread_v * 1000, 'number', { digits: 0 })} mV` : ''}`))),
      bandEl, bat.sources?.length ? h('p', { class: 'src-line' }, srcPill(bat)) : null);

    /* health: capacity now vs new */
    const factor = rangeLabel() === 'Ideal range' ? eff?.factor?.ideal_wh_per_km : eff?.factor?.rated_wh_per_km;
    const healthT = tile({ w: 2, title: 'Capacity and health', icon: 'shield', cls: 'bat-health', cover: 'shots.battery-health' },
      h('div', { class: 'bat-top' },
        h('div', { cover: ['bh.bar.health', 'bh.gauge.degradation'] }, ring(cap.health_pct, { label: 'of new', size: 'l', tone: 'teal' })),
        nums([
          ['Usable now', num(cap.usable_kwh, 'energy', { size: 'm', why: 'not reported' }), 'bh.capacity.usable_now'],
          ['Usable when new', num(cap.new_kwh, 'energy', { size: 'm', why: 'type it below' }), 'bh.capacity.usable_new'],
          ['Difference', num(cap.difference_kwh == null ? null : -cap.difference_kwh, 'energy', { size: 'm', why: 'needs a new value' }), 'bh.capacity.difference'],
          [`Per ${unitLabel('range')} of range`, num(factor, 'efficiency', { size: 'm', why: 'not worked out yet' }), 'bh.stat.efficiency'],
        ])),
      h('p', { class: 'note', cover: ['bh.capacity.help', 'bh.gauge.degradation_help'] },
        `Health is usable capacity now over usable when new (${NEW_SOURCE[cap.new_source] || 'not known'}). ${cap.degradation_pct != null ? `Down ${pct(cap.degradation_pct, 1)}; ` : ''}the warranty promises ${pct(cap.warranty_pct ?? 70)}. Usable is the full pack minus the ${format(cap.buffer_kwh, 'energy')} the car keeps back, from ${n0(cap.readings)} pack readings.`),
      bat.pack?.replaced_ms != null ? note(`Pack replaced on ${formatDate(bat.pack.replaced_ms, tz, { weekday: false, month: 'long' })} (${bat.pack.source === 'owner' ? 'typed by the owner' : 'detected from the pack’s readings'}). Capacity and health cover the new pack only.`) : null);
    const lostShare = rng.lost_km != null && rng.full_new_km ? (rng.lost_km / rng.full_new_km) * 100 : null;
    const rangeT = tile({ w: 2, title: 'Full-charge range', icon: 'gauge' },
      nums([
        ['At 100 % now', num(rng.full_now_km, 'range', { size: 'l', why: 'not enough readings' }), 'bh.ranges.max_now'],
        ['At 100 % when new', num(rng.full_new_km, 'range', { size: 'm', why: 'type it below' }), 'bh.ranges.max_new'],
        ['Range lost', num(rng.lost_km, 'range', { size: 'm', why: 'needs a new value' }), 'bh.ranges.lost'],
      ]),
      note(`${rangeLabel()} at a full charge. From energy: ${format(rng.full_now_from_energy_km, 'range')}.${lostShare != null ? ` ${pct(lostShare, 1)} of the range when new; new is ${RANGE_SOURCE[rng.new_source] || 'not known'}.` : ''}`));
    put(nowS, nowT); put(healthS, healthT); put(rangeS, rangeT);
  };

  /* ------------------------------------------------------------ lifetime totals: GET totals (and battery) */
  const paintTotals = async () => {
    const [bat, totals] = await Promise.all([batOrNull, totalsP]);
    if (!ctx.alive()) return;
    const t = totals || {}, life = (bat && bat.lifetime) || {};
    // from the valid odometer readings only, never below 0 (the server picks them)
    const mileage = t.driven_since_km ?? (t.odometer_last_km != null && t.odometer_first_km != null ? Math.max(0, t.odometer_last_km - t.odometer_first_km) : null);
    const driveT = tile({ w: 2, title: 'Driving, all time', icon: 'route', side: h('a', { class: 'tile-link', href: ctx.link('statistics'), cover: 'bh.drive.link_stats' }, 'Statistics', icon('chevronRight')) },
      facts([
        ['Odometer', format(t.odometer_last_km, 'odometer'), 'bh.drive.odometer'],
        ['Driven since logging began', format(mileage, 'distance'), 'bh.drive.mileage'],
        ['Logged in drives', `${format(t.distance_km, 'distance')} · ${n0(t.drives)} drives`, 'bh.drive.logged'],
        ['Not logged', t.not_logged_km ? h('a', { href: state.buildUrl('health', { car: car.id, period: { preset: 'all' } }) }, `${format(t.not_logged_km, 'distance')}, see the gaps`) : format(t.not_logged_km ?? 0, 'distance'), 'bh.drive.data_lost'],
        ['Energy driving', format(life.drive_kwh, 'energy')],
        ['Won back braking', format(life.regen_kwh, 'energy')],
      ]),
      h('p', { class: 'note', cover: 'bh.drive.help' }, 'Not logged is distance the odometer moved while nothing was recorded. Health lists each gap.'));
    const chargeT = tile({ w: 2, title: 'Charging, all time', icon: 'bolt', side: h('a', { class: 'tile-link', href: ctx.link('charges', { period: { preset: 'all' } }), cover: 'bh.charging.link_stats' }, 'Charges', icon('chevronRight')) },
      facts([
        ['Charging sessions', `${n0(t.charges)} (AC ${n0(t.ac?.charges)}, DC ${n0(t.dc?.charges)}${t.unknown_kind?.charges ? `, ${n0(t.unknown_kind.charges)} of no known kind` : ''})`, 'bh.charging.count'],
        ['Energy added to the pack', format(t.energy_added_kwh, 'energy'), 'bh.charging.energy_added'],
        ['Energy from the grid', `${format(t.energy_grid_kwh, 'energy')}${t.grid_coverage_pct != null && t.grid_coverage_pct < 100 ? ` (measured for ${pct(t.grid_coverage_pct)})` : ''}`, 'bh.charging.energy_used'],
        ['Charging efficiency', t.charging_efficiency_pct == null ? 'not measured' : pct(t.charging_efficiency_pct, 1), 'bh.charging.efficiency'],
        ['Equivalent full cycles', life.equivalent_cycles == null ? '—' : formatNumber(life.equivalent_cycles, { maximumFractionDigits: 1 }), 'bh.charging.cycles'],
      ]),
      splitBar({ label: 'Charged by kind', total: format((life.ac_kwh || 0) + (life.dc_kwh || 0), 'energy'), cover: 'bh.chart.ac_dc',
        parts: [{ name: 'AC', value: life.ac_kwh || 0, text: format(life.ac_kwh || 0, 'energy'), tone: 'c3' }, { name: 'DC', value: life.dc_kwh || 0, text: format(life.dc_kwh || 0, 'energy'), tone: 'c2' }] }),
      h('p', { class: 'note', cover: 'bh.charging.help' }, 'Efficiency is energy into the pack over energy from the grid, for AC charges where both were measured. DC is left out, not counted as 100 %. A full cycle is one usable pack’s worth of charging.'));
    put(driveS, driveT); put(chargeS, chargeT);
  };

  /* ------------------------------------------------------------ reference values: GET settings (and battery) */
  const paintReference = async () => {
    const [bat, settings] = await Promise.all([batOrNull, settingsP]);
    if (!ctx.alive()) return;
    put(refS, referenceTile(ctx, { base, settings, cap: (bat && bat.capacity) || {}, pack: bat && bat.pack, tz }));
  };

  /* ------------------------------------------------------------ charts and lists (filled below) */
  const capT = tile({ w: 2, title: 'Capacity over the car’s life', icon: 'bars', cls: 'chart-tile', cover: ['bh.chart.capacity_by_distance.points', 'bh.chart.capacity_by_distance.median'] });
  const clT = tile({ w: 'full', title: 'Charge level', icon: 'battery', cls: 'chart-tile', cover: ['cl.chart.charge_level', 'shots.charge-level'] });
  const prHead = sectionHead('Projected full-charge range', 'What a full battery would give, worked out from each reading. Point at one chart: the others follow.', { cover: 'shots.projected-range' });
  const prBox = h('div', { class: 'contents' });
  const vdHead = sectionHead('Parked drain', 'Each stay parked off a charger at one spot. Energy is measured at the pack; unknown time is never counted as asleep.', { cover: ['vd.table', 'shots.vampire-drain'] });
  const vdBox = h('div', { class: 'contents' });
  const vdMore = h('div', { class: 'contents' });

  capT.append(plotGhost());
  prBox.append(ghostTile(2, 'h2'), ghostTile(2, 'h2'));
  vdBox.append(ghostTile(1, 'h1'), ghostTile(1, 'h1'), ghostTile(1, 'h1'), ghostTile(1, 'h1'));
  ctx.view.replaceChildren(bar, nowS, healthS, rangeS, capT, driveS, chargeS, clT, prHead, prBox, vdHead, vdBox, vdMore, refS);

  /* capacity by distance: a dot per day, the monthly line */
  async function paintCapacity() {
    const [ptsP, medP] = capacityReads(ctx);
    const [pts, med] = await Promise.all([ptsP.catch(() => []), medP.catch(() => null)]);
    const bat = await batOrNull;
    if (!ctx.alive()) return;
    capT.querySelector('.plot-ghost')?.remove();
    const m = (med?.items || []).filter((x) => x.odometer_km != null && x.usable_kwh != null).sort((a, b) => a.odometer_km - b.odometer_km);
    const swapMs = bat?.pack?.replaced_ms ?? null;
    if (!pts.length && !m.length) { capT.append(emptyTile({ icon: 'battery', title: 'No capacity readings yet', text: 'The pack reports its capacity while the car is awake.', w: 2 })); return; }
    await xyChart(ctx, capT, { label: 'Usable capacity against the odometer', xs: m.map((x) => x.odometer_km), xKind: 'odometer', yKind: 'energy', yFloor: null,
      // readings before the pack was replaced are the previous pack's: drawn grey
      dots: pts.map((x) => ({ x: x.odometer_km, y: x.usable_kwh, tone: swapMs != null && x.t_ms < swapMs ? 'muted' : undefined })), dotsName: 'Per day',
      series: [{ name: 'Monthly median', kind: 'energy', values: m.map((x) => x.usable_kwh), tone: 'c3' }] });
    const tr = med?.trend;
    const from = unionOf([...pts, ...(med?.items || [])]);
    if (from.sources.length) capT.append(h('p', { class: 'src-line' }, srcPill(from)));
    if (swapMs != null) capT.append(note('Grey dots are the previous pack, before it was replaced.'));
    capT.append(note(tr?.slope_kwh_per_10000_km != null ? `About ${format(Math.abs(tr.slope_kwh_per_10000_km), 'energy')} ${tr.slope_kwh_per_10000_km < 0 ? 'less' : 'more'} every ${format(10000, 'distance')}${tr.projection?.odometer_km ? `; at this pace the warranty line comes at about ${format(tr.projection.odometer_km, 'odometer')}` : ''}.` : 'Not enough readings for a trend yet.'));
  }

  /* charge level over the period */
  const clHost = h('div', { class: 'cl-host' });
  const clStats = h('div', { class: 'contents' });
  const segB = segmented({ name: 'bucket', value: opt.bucket, cover: 'cl.control.bucket_width', options: [['hour', 'Hourly'], ['day', 'Daily']], onchange: (v) => { opt.bucket = v; ctx.setQuery({ cl_bucket: v === 'hour' ? v : null }); paintLevel(); } });
  const mov = toggle({ label: 'Moving lines', sub: 'Average, median and the 7.5 and 92.5 percentiles', checked: opt.moving, cover: 'cl.control.include_moving', onchange: (on) => { opt.moving = on; ctx.setQuery({ cl_moving: on ? null : '0' }); paintLevel(); } });
  const segW = segmented({ name: 'window', value: opt.window, cover: 'cl.control.window_width', options: [['7d', '7 days'], ['30d', '30 days']], onchange: (v) => { opt.window = v; ctx.setQuery({ cl_window: v === '30d' ? v : null }); paintLevel(); } });
  clHost.append(plotGhost());
  clT.append(h('div', { class: 'cl-controls' }, segB, segW, mov), clHost, clStats);
  async function paintLevel() {
    const [from, to] = levelSpan(ctx);
    const longSpan = to - from > 62 * DAY;
    const bucket = longSpan ? 'day' : opt.bucket;
    const d = await levelRead(ctx, opt, from, to).catch((e) => ({ error: e }));
    const band = (d.band ? null : (await batOrNull)?.band) || {};
    if (!ctx.alive()) return;
    clHost.replaceChildren();
    clStats.replaceChildren();
    if (d.error || !(d.buckets || []).length) { clHost.append(emptyTile({ icon: 'battery', title: 'No level readings in this period', text: 'Pick a longer period above.', w: 'full' })); return; }
    const b = d.buckets, mv = d.moving || {};
    const series = [
      { key: 'soc', name: 'Battery', kind: 'percent', values: b.map((x) => x.soc_pct), tone: 'c1', fill: true, cover: 'cl.chart.battery_level' },
      { key: 'usable', name: 'Usable', kind: 'percent', values: b.map((x) => x.usable_soc_pct), tone: 'c2', dashed: true, cover: 'cl.chart.usable_level' },
      ...(opt.moving ? [
        { key: 'mean', name: `Average (${opt.window})`, kind: 'percent', values: mv.mean || [], tone: 'c3', cover: 'cl.chart.moving_avg' },
        { key: 'median', name: `Median (${opt.window})`, kind: 'percent', values: mv.median || [], tone: 'c4', cover: 'cl.chart.moving_median' },
        { key: 'p7', name: '7.5th', kind: 'percent', values: mv.p7_5 || [], tone: 'c5', dashed: true, cover: 'cl.chart.moving_p075' },
        { key: 'p92', name: '92.5th', kind: 'percent', values: mv.p92_5 || [], tone: 'c5', dashed: true, cover: 'cl.chart.moving_p925' },
      ] : []),
    ].map((s) => ({ ...s, axis: 'left', floor: 0 }));
    const bl = d.band || { low_pct: band.low_pct ?? 20, high_pct: band.high_pct ?? 80 };
    await timeChart(ctx, clHost, { tz, t_ms: b.map((x) => x.t_ms), series, spans: d.spans || [], label: 'Charge level', yBand: { from: bl.low_pct, to: bl.high_pct } });
    const st = d.stats || {};
    clStats.append(
      h('div', { class: 'cl-stats', cover: 'cl.chart.legend_stats' },
        [['Average', st.mean], ['Median', st.median], ['Lowest', st.min], ['Highest', st.max], ['Above 80 %', d.share_above_80_pct, 'of the time'], ['Below 20 %', d.share_below_20_pct, 'of the time']]
          .map(([k, v, s]) => h('div', null, h('span', { class: 'hero-k' }, k), num(v, 'percent', { size: 's' }), s ? h('span', { class: 'tile-line' }, s) : null))),
      h('p', { class: 'note', cover: 'cl.chart.band' }, `The teal band is the everyday range ${pct(bl.low_pct)} to ${pct(bl.high_pct)}. Moving lines use a fixed ${opt.window === '30d' ? '30-day' : '7-day'} window of daily means, so they do not change with the period.${longSpan && opt.bucket === 'hour' ? ' Hourly is for periods up to 62 days, so this shows daily.' : ''} Grey shading: asleep or no data.`));
  }

  /* projected range, three charts with one cursor */
  async function paintProjected() {
    const params = projectedParams(ctx, opt);
    const d = await ctx.api.get(`${base}/projected-range`, params).catch((e) => ({ error: e }));
    if (!ctx.alive()) return;
    const seg = segmented({ name: 'resolution', value: opt.prBucket, cover: 'pr.control.resolution', options: [['auto', 'Auto'], ['hour', 'Hour'], ['day', 'Day'], ['week', 'Week']],
      onchange: (v) => { opt.prBucket = v; ctx.setQuery({ pr_bucket: v === 'auto' ? null : v }); paintProjected(); } });
    const v = d.values || {}, ts = d.t_ms || [];
    prBox.replaceChildren(h('div', { class: 'page-bar w-full' }, seg, h('span', { class: 'filter-note', cover: ['pr.axis.titles', 'pr.control.shared_crosshair'] }, 'Left: full-charge range. Right: what it is set against. One cursor for all three.')));
    if (d.error || !ts.length || !(v.full_range_km || []).some((x) => x != null)) { prBox.append(emptyTile({ icon: 'gauge', title: 'No range readings in this period', text: 'Pick a longer period above.' })); return; }
    const sync = `pr-${car.id}-${Date.now()}`;
    const common = { tz, t_ms: ts, sync, states: d.state, short: true };
    const mk = (title, ic, cover) => tile({ w: 2, title, icon: ic, cls: 'chart-tile', cover });
    const a = mk('Against distance driven', 'route', ['pr.mileage.projected', 'pr.mileage.odometer']);
    const b2 = mk('Against battery level', 'battery', ['pr.battery.projected_usable', 'pr.battery.projected_total', 'pr.battery.levels']);
    const c = mk('Against temperature', 'thermo', ['pr.temp.projected', 'pr.temp.outdoor']);
    prBox.append(a, b2, c);
    await Promise.all([
      timeChart(ctx, a, { ...common, label: 'Projected range against distance', series: [
        { key: 'r', name: 'Full range', kind: 'range', values: v.full_range_km, tone: 'c1', axis: 'left' },
        { key: 'o', name: 'Odometer', kind: 'odometer', values: v.odometer_km, tone: 'c2', dashed: true, axis: 'right' }] }),
      timeChart(ctx, b2, { ...common, label: 'Projected range against level', series: [
        { key: 'u', name: 'From usable %', kind: 'range', values: v.full_range_usable_km, tone: 'c1', axis: 'left' },
        { key: 's', name: 'Battery %', kind: 'percent', values: v.soc_pct, tone: 'c2', dashed: true, axis: 'right' },
        { key: 'b', name: 'Pack %', kind: 'percent', values: v.soc_bms_pct, tone: 'c3', axis: 'right' }] }),
      timeChart(ctx, c, { ...common, label: 'Projected range against temperature', series: [
        { key: 'r', name: 'Full range', kind: 'range', values: v.full_range_km, tone: 'c1', axis: 'left' },
        { key: 'o', name: 'Outside', kind: 'temperature', values: v.outside_c, tone: 'c2', dashed: true, axis: 'right', signed: true },
        { key: 'b', name: 'Battery', kind: 'temperature', values: v.battery_c, tone: 'c3', axis: 'right', signed: true }] }),
    ]);
    if (!ctx.alive()) return;
    prBox.append(tile({ w: 2, title: 'How to read these', icon: 'info' }, note(`Full range from battery %, from energy (${format((v.full_range_from_energy_km || []).filter((x) => x != null).slice(-1)[0], 'range')} last) and from the usable %. A cold pack reads lower; it comes back when warm.${d.bucket_ms ? ` One point every ${formatDuration(d.bucket_ms)}.` : ''}`)));
  }

  /* parked drain */
  async function paintParked() {
    const params = parkedParams(ctx, opt);
    const pager = ctx.api.pager(`${base}/parked`, params, { limit: 12 });
    await pager.more().catch(() => null);
    if (!ctx.alive()) return;
    const s = pager.last?.summary || {};
    const minPills = pills({ label: 'Shortest spell', value: String(opt.minH), cover: 'vd.filter.min_idle', options: [['0', 'Any length'], ['3', '3 h or more'], ['12', '12 h or more'], ['24', 'A day or more']],
      onchange: (val) => { opt.minH = Number(val); ctx.setQuery({ min_h: opt.minH || null }); paintParked(); } });
    vdBox.replaceChildren(h('div', { class: 'page-bar w-full' }, minPills),
      statTile({ label: 'Parked spells', value: s.spells, kind: 'count', icon: 'park', sub: `${formatNumber(Math.round(s.hours ?? 0))} h off a charger` }),
      statTile({ label: 'Energy lost parked', value: s.energy_kwh, kind: 'energy', icon: 'bolt', sub: 'measured at the pack' }),
      statTile({ label: 'Per day parked', value: s.kwh_per_day, kind: 'energy', icon: 'calendar', sub: 'per 24 h parked' }),
      tile({ w: 1, title: 'What it cost', icon: 'coin', cls: 'tile-stat' }, s.cost?.amount != null ? h('span', { class: 'num num-m' }, h('span', { class: 'num-n' }, formatMoney(s.cost.amount, s.cost.currency))) : num(null, 'number', { why: 'no home price' }),
        h('p', { class: 'tile-line' }, s.cost ? `at the ${s.cost.basis || 'home price'}` : 'set a home price in Places')));
    vdMore.replaceChildren();
    const draw = (list) => { for (const r of list) vdMore.append(spellTile(r)); };
    if (!pager.items.length) vdMore.append(emptyTile({ icon: 'park', title: 'No parked spells in this period', text: opt.minH ? `None was at least ${opt.minH} h long.` : 'Pick a longer period above.' }));
    else {
      draw(pager.items);
      const more = moreButton(async () => { const n = pager.items.length; await pager.more(); more.remove(); draw(pager.items.slice(n)); if (!pager.done) vdMore.append(more); return !pager.done; }, { text: 'Show earlier spells' });
      if (!pager.done) vdMore.append(more);
    }
  }
  function spellTile(r) {
    const d = r.duration_ms || 1;
    const asleep = r.asleep_ms || 0, unknown = r.unknown_ms || 0, awake = Math.max(0, d - asleep - unknown);
    const lost = r.soc_start_pct != null && r.soc_end_pct != null ? Math.max(0, r.soc_start_pct - r.soc_end_pct) : null;
    const trip = state.buildUrl('trip', { car: car.id, period: { from: r.start_ms, to: r.end_ms ?? api.now() } });
    const sameDay = r.end_ms != null && startOf(r.end_ms, 'day', tz) === startOf(r.start_ms, 'day', tz);
    return tile({ w: 2, cls: ['item', 'spell-tile'] },
      h('div', { class: 'item-top' },
        h('span', { class: 'item-ic tone-sky', 'aria-hidden': 'true' }, icon('park')),
        h('div', { class: 'item-text' },
          h('p', { class: 'item-title' }, r.place?.name || r.address || 'Not a saved place'),
          h('p', { class: 'item-sub' },
            h('a', { href: trip, cover: ['vd.col.start', 'vd.link.start_details'], title: 'Open this span as a trip' }, `${formatDate(r.start_ms, tz, { now: api.now() })}, ${formatTime(r.start_ms, tz)}`),
            ' – ', h('span', { cover: 'vd.col.end' }, r.end_ms == null ? 'still parked' : sameDay ? formatTime(r.end_ms, tz) : `${formatDate(r.end_ms, tz, { now: api.now() })}, ${formatTime(r.end_ms, tz)}`))),
        h('span', { class: 'item-flags', cover: 'vd.flag.cold' },
          r.cold ? chip('Cold pack', { tone: 'sky', icon: 'thermo' }) : null,
          ...(r.awake_reasons || []).slice(0, 2).map((x) => chip(AWAKE_WORDS[x] ? AWAKE_WORDS[x][0].toUpperCase() + AWAKE_WORDS[x].slice(1) : x, { tone: 'sun' })))),
      h('div', { class: 'item-nums' },
        h('span', { cover: 'vd.col.period' }, num(r.duration_ms, 'duration', { size: 's' })),
        h('span', { cover: 'vd.col.energy' }, num(r.energy_kwh, 'energy', { size: 's' })),
        h('span', { cover: 'vd.col.avg_power' }, num(r.avg_power_w, 'power_w', { size: 's' })),
        h('span', { cover: 'vd.col.soc_diff' }, num(lost == null ? null : -lost, 'percent', { size: 's', digits: 1 }))),
      h('div', { cover: 'vd.col.standby' }, splitBar({ label: 'Asleep and awake', parts: [
        { name: 'Asleep', value: asleep, text: formatDuration(asleep), tone: 'c2' }, { name: 'Awake', value: awake, text: formatDuration(awake), tone: 'c4' }, { name: 'Unknown', value: unknown, text: formatDuration(unknown), tone: 'c5' }] })),
      h('p', { class: 'item-sub' },
        h('span', { cover: 'vd.col.range_loss' }, `${format(r.range_lost_km, 'range', { digits: 1 })} of range lost`), ' · ',
        h('span', { cover: 'vd.col.range_per_hour' }, `${format(r.range_lost_per_hour_km, 'range_rate', { digits: 2 })}`), ' · ',
        `${format(r.soc_start_pct, 'percent')} → ${format(r.soc_end_pct, 'percent')}`));
  }

  // the battery block fails the page when GET battery does (as before); the rest show their own empty states
  await Promise.all([paintBattery(), paintTotals(), paintReference(), paintCapacity(), paintLevel(), paintProjected(), paintParked()]);
  if (!ctx.alive()) return {};
  ctx.ready();
  return {};
}

/** The reference values an admin may type: capacity and range when new, and the warranty line. */
function referenceTile(ctx, { base, settings, cap, pack, tz }) {
  const s = settings || {};
  const can = !!ctx.admin && !!settings;
  const capF = field({ label: 'Usable capacity when new (kWh)', name: 'capacity_new_kwh', type: 'number', value: s.capacity_new_kwh ?? '', cover: 'bh.setting.capacity_new_override',
    hint: `Leave empty to use ${NEW_SOURCE[cap.new_source === 'owner' ? 'early_bms' : cap.new_source] || 'the earliest readings'}. 10 to 250 kWh.`, attrs: { min: '10', max: '250', step: '0.1', inputmode: 'decimal', disabled: !can, placeholder: s.capacity_new_kwh_default != null ? String(s.capacity_new_kwh_default) : undefined } });
  const rngF = field({ label: `Full-charge range when new (${unitLabel('range')})`, name: 'full_range_new_km', type: 'number', cover: 'bh.setting.range_new_override',
    value: s.full_range_new_km == null ? '' : Math.round(convert(s.full_range_new_km, 'range')), hint: 'Leave empty to use the median of the first 30 days.', attrs: { min: '1', step: '1', inputmode: 'numeric', disabled: !can, placeholder: s.full_range_new_km_default != null ? String(Math.round(convert(s.full_range_new_km_default, 'range'))) : undefined } });
  const warF = field({ label: 'Warranty line (% health)', name: 'warranty_threshold_pct', type: 'number', value: s.warranty_threshold_pct ?? '', hint: 'The health your battery warranty promises, often 70 %.', attrs: { min: '50', max: '100', step: '1', inputmode: 'numeric', disabled: !can } });
  const detected = !s.pack_replaced_date && pack?.source === 'detected' && pack.replaced_ms != null ? dayKey(pack.replaced_ms, tz) : null;
  const swapF = field({ label: 'Pack replaced on', name: 'pack_replaced_date', type: 'date', value: s.pack_replaced_date || '',
    hint: `${detected ? `Detected from the pack’s readings: ${detected}. ` : ''}The day a new pack went in; from then on health covers the new pack. Leave empty if it was never replaced.`, attrs: { disabled: !can, placeholder: detected || undefined } });
  const swapKwhF = field({ label: 'New pack capacity when new (kWh)', name: 'pack_new_kwh', type: 'number', value: s.pack_new_kwh ?? '', hint: 'The usable capacity of the replacement pack when new. 10 to 250 kWh.',
    attrs: { min: '10', max: '250', step: '0.1', inputmode: 'decimal', disabled: !can } });
  let version = s.version;
  const f = form({
    fields: [capF, rngF, warF, swapF, swapKwhF], submitLabel: 'Save reference values',
    async onsubmit(v) {
      if (!can) return;
      const r = await ctx.api.patch(`${base}/settings`, { version, capacity_new_kwh: v.capacity_new_kwh, full_range_new_km: v.full_range_new_km == null ? null : Math.round(toMetric(v.full_range_new_km, 'range') * 10) / 10, warranty_threshold_pct: v.warranty_threshold_pct, pack_replaced_date: v.pack_replaced_date || null, pack_new_kwh: v.pack_new_kwh });
      version = r?.version;
      ctx.toast('Reference values saved. Health and range use them now.', { next: true });
      ctx.reload();
    },
  });
  if (!can) f.submitButton.disabled = true;
  return tile({ w: 2, title: 'Reference values', icon: 'edit', side: h('a', { class: 'tile-link', href: state.buildUrl('settings-cars', { car: ctx.car.id }) }, 'Car settings', icon('chevronRight')) },
    h('p', { class: 'note', cover: 'bh.setting.override_help' }, 'Typed once for this car and checked when saved. They set what “when new” means for health and range lost.'),
    f, !settings ? note('The car’s settings could not be read, so they cannot be changed here.') : !ctx.admin ? note('Only an admin can change these.') : null);
}
