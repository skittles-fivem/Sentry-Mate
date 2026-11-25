// Battery (/cars/{car}/battery): how healthy the pack is, how the charge level is used, and where energy
// goes while the car is parked.
// - Now, capacity and health, full-charge range and the lifetime totals come from GET .../battery (the
//   pack's own readings and lifetime counters), GET .../totals (drives and charges) and the energy factor
//   of GET .../efficiency. They describe the whole life of the car, not the period.
// - Capacity history (GET .../battery/capacity) is the whole life too: the trend needs every reading.
// - Charge level (GET .../battery-level), projected range (GET .../projected-range) and parked drain
//   (GET .../parked) follow the shared period. Their controls live in the address (cl_bucket, cl_moving,
//   cl_window, pr_bucket, min_h, cap_x, cap_group), so a link keeps them.
// - Reference values (capacity and range when new, warranty threshold) are the car's settings
//   (PATCH .../settings, admin), typed in the person's units and checked by the server.
// Asleep is not unknown: asleep spans are shaded blue and lines go on through them; unknown spans are
// grey and lines break there. Live: `now` deltas move the Now panel; `rows` events of the car build the page again.

import * as state from '../shared/state.js';
import { h, svg, replace, cssVar, steady } from '../shared/dom.js';
import { downloadCsv, csvFilename, column, timeColumn } from '../shared/csv.js';
import { format, formatMoney, convert, toMetric, unitLabel, getUnits } from '../shared/units.js';
import { formatDateTime, formatDate, formatTime, formatDuration, startOf, toIsoZoned } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon } from '../core/icons.js';
import { pageHead, panel, metric, csvButton, note, tag, kv } from '../core/page.js';
import { dataTable } from '../core/table.js';
import { emptyState, errorState } from '../core/states.js';
import { loadUPlot, mountPlot, timeAxis, valueAxis, colors, hexA, areaFill } from '../core/chart.js';
import { openPeriodPicker } from '../core/period.js';
import { openCarSwitcher } from '../core/carswitch.js';
import { field, form, showApiError } from '../core/form.js';
import { flash } from '../core/flash.js';
import { rpx } from '../core/display.js';
import { srcMark, srcText, srcShort } from '../core/source.js';
import { onRows } from '../shared/live.js';
const CL_IDLE = 'Point at the chart for the values at a time.';
const LIFE_KINDS = ['smo', 'tesla_api', 'teslafi_api', 'teslamate_local', 'import'];
const LIFE_WORDS = { smo: 'SMO', tesla_api: 'the Tesla API', teslafi_api: 'TeslaFi', teslamate_local: 'local TeslaMate', import: 'imported history' };

export const id = 'battery';
export const css = true;

const enc = encodeURIComponent;
const DAY = 86400000;
const pct = (v, digits = 0) => format(v, 'percent', { digits });
const n0 = (v) => (v == null ? '—' : formatNumber(v, { maximumFractionDigits: 0 }));

const NEW_SOURCE = {
  owner: 'typed by the owner',
  early_bms: 'median of the pack’s readings in its first 30 days',
  imported_charges: 'per charge, from the earliest imported charges: the energy each stored (the rated range it added × the car’s own energy per km of range, from the BMS), pack side, over the % it added',
  imported_charges_energy_added: 'per charge, from the earliest imported charges: the energy each added over the % it added (no BMS energy readings to do better; reads a few % high)',
  pack_code: 'the pack-size code the car reports (a hint, not a measurement)',
  vehicle_info: 'the car’s build (VIN and CAN), not a measurement',
};
const RANGE_SOURCE = {
  owner: 'typed by the owner',
  first_30_days: 'median full-charge range of the first 30 days',
  pack_code: 'the pack-size code the car reports (a hint, not a measurement)',
  vehicle_info: 'the car’s build (VIN and CAN), not a measurement',
};
// what a charge's capacity reading is worked out from (usable_basis)
const CHARGE_BASIS = {
  rated_range: 'energy stored over the % added, pack side',
  energy_added: 'energy added over the % added (reads a few % high)',
};
/** What a capacity reading is, by its source: the BMS, or per charge from imported charges or from CAN. */
function readingLabel(it) {
  if (it.method !== 'charge') return 'BMS';
  const from = it.source === 'can' ? 'per charge from CAN' : it.source === 'teslamate' || it.source === 'teslafi' ? 'per charge from imported charges' : 'per charge';
  return `${from}, ${CHARGE_BASIS[it.usable_basis] || 'energy over the % added'}`;
}
/** Where the two figures health compares come from. */
function healthSources(cap) {
  const now = cap.basis === 'charges' ? 'Usable now is worked out per charge from the last charges' : 'Usable now comes from the battery management system (BMS), which reports the pack’s capacity every awake minute';
  const src = cap.new_source;
  const when = src === 'imported_charges' ? 'usable when new is worked out per charge from the earliest imported charges, on the same basis (energy stored, pack side, not energy added)'
    : src === 'imported_charges_energy_added' ? 'usable when new is the earliest imported charges’ energy added over the % added, which reads a few % high'
      : src === 'early_bms' ? 'usable when new is the BMS’s reading in the first 30 days'
        : src === 'owner' ? 'usable when new is the value typed under Reference values'
          : src ? 'usable when new is what the car’s build gives' : 'usable when new is not known';
  return `${now}; ${when}.`;
}
const NOTES = {
  new_value_typed: 'The “when new” values are the ones typed under Reference values.',
  new_value_from_early_readings: 'No value was typed, so “when new” is the median of the earliest pack readings. Type the real figure under Reference values if you know it.',
  new_value_from_imported_charges: 'The car was logged late, so “when new” comes from the earliest imported charges, on the same basis as the BMS (energy stored, not energy added). Type the real figure if you know it.',
  new_value_from_imported_charges_energy_added: 'The car was logged late and there are no BMS energy readings, so “when new” is the earliest imported charges’ energy added over the % added. That counts charging losses and reads a few % high: type the real figure if you know it.',
  new_value_from_pack_code: 'There are no early readings, so “when new” is only the pack-size code. Type the real figure for a true health figure.',
  new_value_from_vehicle_info: 'There are no early readings, so “when new” is what the car’s build (VIN and CAN) gives. Type the real figure under Reference values if you know it.',
  cold_battery_locks_part: 'The pack is cold: part of the energy is locked until it warms up, so usable is below battery %.',
  lfp_charge_to_100_weekly: 'LFP pack: charge to 100 % about once a week so the car can recalibrate its battery %.',
  imported_history_estimated: 'Part of the history is imported; figures from it are estimated.',
  no_history: 'There is no history for this car yet.',
  pack_replaced: 'The pack was replaced: capacity, health and the trend cover the new pack only.',
  reading_above_new: 'The capacity now reads above the value when new, so no loss is shown. If that value was typed, it is probably too low: check it under Reference values.',
};
const CHEM = { lfp: 'LFP (iron phosphate)', nca_nmc: 'NCA / NMC (nickel)' };
const REASONS = {
  sentry: 'Sentry mode', keep_climate_on: 'Climate kept on', keep_climate_dog: 'Dog mode', keep_climate_camp: 'Camp mode',
  user_present: 'Someone in or near the car', preconditioning_cabin: 'Cabin preconditioning', preconditioning_battery: 'Battery preconditioning',
  charging: 'Charging', keep_warm: 'Keeping the pack warm', door_open: 'A door open', accessory_power: 'Accessory power',
};
const ENERGY_METHOD = { counter: 'pack energy counters', bms_energy: 'pack energy reading', range: 'range change (estimated)' };

// The calls that start at once: [battery, efficiency, totals, settings, totals of each input]. prefetch() warms
// the same ones when a link here is under the pointer.
function firstReads(ctx) {
  const base = `cars/${enc(ctx.car.id)}`;
  return [
    ctx.api.get(`${base}/battery`), ctx.api.get(`${base}/efficiency`), ctx.api.get(`${base}/totals`), ctx.api.get(`${base}/settings`),
    ...LIFE_KINDS.map((k) => ctx.api.get(`${base}/totals`, { source: k })),
  ];
}
// the section controls of the address, and the history sections' reads
const optsOf = (q) => ({
  capX: q.cap_x === 'odometer' ? 'odometer' : 'time',
  capGroup: q.cap_group === 'day' ? 'day' : 'charge',
  clBucket: q.cl_bucket === 'hour' || q.cl_bucket === 'day' ? q.cl_bucket : null,
  clMoving: q.cl_moving !== '0',
  clWindow: q.cl_window === '30d' ? '30d' : '7d',
  prBucket: ['hour', 'day', 'week'].includes(q.pr_bucket) ? q.pr_bucket : 'auto',
  minH: [0, 1, 3, 6, 12, 24].includes(Number(q.min_h)) ? Number(q.min_h) : 0,
});
const levelBucket = (opt, from, to) => (opt.clBucket && !(opt.clBucket === 'hour' && to - from > 62 * DAY) ? opt.clBucket : (to - from <= 31 * DAY ? 'hour' : 'day'));
const levelRead = (ctx, opt, from, to) => ctx.api.get(`cars/${enc(ctx.car.id)}/battery-level`, { from_ms: from, to_ms: to, bucket: levelBucket(opt, from, to), moving: opt.clMoving ? 'true' : 'false', window: opt.clWindow });
function projectedParams(ctx, opt, nowMs) {
  const p = ctx.period, params = { ...ctx.params };
  if (opt.prBucket !== 'auto') params.bucket = opt.prBucket; else params.max_points = 600;
  const span = (p.to_ms ?? nowMs) - (p.from_ms ?? nowMs - 365 * DAY);
  const hourTooFine = span > 62 * DAY;
  if (params.bucket === 'hour' && hourTooFine) { delete params.bucket; params.max_points = 600; }
  return { params, hourTooFine };
}
const parkedParams = (ctx, opt) => ({ ...ctx.params, ...(opt.minH ? { min_hours: opt.minH } : {}) });
export function prefetch(ctx) {
  const base = `cars/${enc(ctx.car.id)}`, opt = optsOf(ctx.query), p = ctx.period, now = ctx.api.now();
  return Promise.all([
    ...firstReads(ctx), Promise.resolve().then(loadUPlot).catch(() => null),
    ctx.api.get(`${base}/battery/capacity`, { group: opt.capGroup, x: opt.capX, limit: 500 }), // the first page of collect()
    ctx.api.get(`${base}/battery/capacity`, { group: 'month', x: opt.capX, limit: 500 }),
    p.from_ms != null ? levelRead(ctx, opt, p.from_ms, p.to_ms ?? now) : null,
    ctx.api.get(`${base}/projected-range`, projectedParams(ctx, opt, now).params),
    ctx.api.pager(`${base}/parked`, parkedParams(ctx, opt), { limit: 50 }).more(), // the parked table (50 a page)
  ]);
}

export async function mount(ctx) {
  const car = ctx.car, cid = car.id, tz = ctx.tz, base = `cars/${enc(cid)}`;
  const period = ctx.period;
  const q = ctx.query;
  const now = () => ctx.api.now();
  const opt = optsOf(q);
  let first = null; // the car's first reading, for the All time period (see firstMs)

  // Every call starts at once and each section paints when its own answers are in: until then a panel of its
  // kind with a grey block keeps its place, so the page shows what it has instead of waiting for the slowest call.
  const [batP, effRead, totalsRead, settingsRead, ...bySrc] = firstReads(ctx);
  // (a read that failed is kept: its figures are said as not loaded, not as "not worked out" or as dashes alone)
  let effErr = null, totalsErr = null;
  const effP = effRead.catch((e) => { effErr = e; return null; });
  const totalsP = totalsRead.catch((e) => { totalsErr = e; return null; });
  const settingsP = settingsRead.catch(() => null);
  // which inputs gave drives or charges at all: /totals names the inputs the filter takes, not the ones that gave rows
  const bySrcP = Promise.all(bySrc.map((p) => p.catch(() => null)));
  const uplotP = loadUPlot().catch(() => null);
  // filled when GET battery (and efficiency) is in; the sections below read them then
  let bat = {}, totals = null, settings = null, eff = null, lifeKinds = [], cap = {}, life = {}, fr = {}, toIdeal = null, rangeWord = 'rated', rng = {};
  const batReady = Promise.all([batP, effP]).then(([b, e]) => {
    bat = b; eff = e;
    cap = bat.capacity || {}; life = bat.lifetime || {};
    // /battery sends rated range; with the ideal preference it is turned into ideal range by the two energy
    // factors (the same energy over a smaller factor), and stays labelled rated when a factor is missing
    fr = eff?.factor || {};
    toIdeal = getUnits().range === 'ideal' && fr.rated_wh_per_km > 0 && fr.ideal_wh_per_km > 0 ? fr.rated_wh_per_km / fr.ideal_wh_per_km : null;
    const rng0 = bat.range || {};
    rangeWord = toIdeal ? 'ideal' : 'rated';
    const rk = (km) => (km == null ? km : toIdeal ? km * toIdeal : km);
    rng = { ...rng0, full_now_km: rk(rng0.full_now_km), full_now_from_energy_km: rk(rng0.full_now_from_energy_km), full_new_km: rk(rng0.full_new_km), lost_km: rk(rng0.lost_km) };
  });
  const batSettled = batReady.catch(() => null);
  const waitPanel = (title, ic, cls) => panel({ title, icon: ic, cls }, h('div', { class: 'skel skel--block', 'aria-hidden': 'true' }));

  /* ---------- the head */
  const periodBtn = (cv) => h('button', { class: 'btn btn--sm', type: 'button', cover: cv, title: 'Choose the period (shared by every page)', onclick: (e) => openPeriodPicker(e.currentTarget) },
    icon('calendar', 'ico--sm'), period.label, icon('chev-d', 'ico--sm'));
  // the chemistry and the time of the readings come with GET battery; the line keeps its height until then
  const chemEl = h('span', null, '\u00a0'), asOfEl = h('span', null, '\u00a0');
  const head = pageHead({
    eyebrow: car.name, title: 'Battery',
    sub: [chemEl, asOfEl],
    purpose: 'Health, capacity and range describe the whole life of the car. Charge level, projected range and parked drain follow the period.',
    actions: [
      h('button', { class: 'btn', type: 'button', onclick: openCarSwitcher, cover: ['bh.filter.car', 'cl.filter.car', 'pr.section.per_car', 'vd.section.per_car'], title: 'Show another car on this page' }, icon('car'), 'Switch car'),
      h('a', { class: 'btn', href: state.buildUrl('statistics', { query: { cars: 'all', metric: 'capacity' } }), title: 'Capacity and range of several cars side by side' }, icon('stats'), 'Compare cars'),
    ],
  });

  /* ---------- Now */
  const nowSlot = h('div', { class: 'bat-now' });
  // "Live" only while the car is watched and its readings are recent: a sold car or old readings are the last known ones
  const nowSide = h('span', { class: 'faint' }, state.isSold(car) ? 'Last known' : 'Live');
  function paintNow(live = {}) {
    const soc = live.soc ?? bat.soc_pct, usable = live.usable ?? bat.usable_soc_pct, kwh = live.kwh ?? bat.energy_now_kwh;
    const band = bat.band || {};
    const lim = live.limit ?? band.charge_limit_pct;
    const lockedPart = soc != null && usable != null && soc - usable >= 0.5;
    replace(nowSlot,
      h('div', { class: 'bat-now-top' },
        h('div', { class: 'bat-soc', cover: 'bh.soc.current' },
          h('span', { class: 'metric-label' }, 'Battery'),
          h('span', { class: 'bat-soc-val' }, soc == null ? '—' : formatNumber(soc, { maximumFractionDigits: 0 }), h('small', null, '%')),
          h('span', { class: 'explain' }, soc == null ? 'No reading yet.' : 'The battery % the car shows.')),
        h('div', { class: 'bat-now-side' },
          metric('Usable', usable, 'percent', { size: 'sm', digits: 0, explain: lockedPart ? 'Lower than battery %: a cold pack locks part of its energy.' : 'Energy the car can use now, as %.', why: 'Not reported' }),
          metric('Stored energy', kwh, 'energy', { size: 'sm', cover: 'bh.bar.stored_energy', explain: cap.usable_kwh ? `of ${format(cap.usable_kwh, 'energy')} usable when full` : 'Usable energy in the pack now.', why: 'Not reported' }))),
      h('div', { class: 'soc-track-wrap', cover: 'bh.soc.band' },
        h('div', { class: 'soc-track', role: 'img', 'aria-label': `Battery ${pct(soc)}, everyday band ${pct(band.low_pct)} to ${pct(band.high_pct)}, charge limit ${pct(lim)}`,
          vars: { '--soc': `${soc ?? 0}%`, '--usable': `${usable ?? 0}%`, '--lo': `${band.low_pct ?? 20}%`, '--hi': `${band.high_pct ?? 80}%`, '--lim': `${lim ?? 0}%` } },
          h('i', { class: 'soc-band' }), h('i', { class: 'soc-fill' }), usable != null && lockedPart ? h('i', { class: 'soc-usable' }) : null,
          lim != null ? h('i', { class: 'soc-limit', title: `Charge limit set in the car: ${pct(lim)}` }) : null),
        h('div', { class: 'soc-scale', 'aria-hidden': 'true', vars: { '--lo': `${band.low_pct ?? 20}%`, '--hi': `${band.high_pct ?? 80}%`, '--lim': `${lim ?? 0}%` } },
          h('span', { class: 's0' }, '0'), h('span', { class: 'slo' }, pct(band.low_pct)), band.high_pct < 100 ? h('span', { class: 'shi' }, pct(band.high_pct)) : null, h('span', { class: 's100' }, '100 %')),
        h('p', { class: 'soc-key' },
          h('span', null, h('i', { class: 'k-band' }), `Everyday band ${pct(band.low_pct)} – ${pct(band.high_pct)}`, band.high_pct >= 100 ? ' (LFP: full charges are fine)' : ''),
          lim != null ? h('span', null, h('i', { class: 'k-limit' }), `Charge limit ${pct(lim)}`) : h('span', { class: 'faint' }, 'The car has not sent its charge limit.'))),
      kv([
        ['Range now at this level', soc != null && rng.full_now_km != null ? format((rng.full_now_km * soc) / 100, 'range') : '—', { icon: 'gauge', title: 'Full-charge range times battery %' }],
        ['Energy still to the charge limit', lim != null && kwh != null && cap.usable_kwh ? format(Math.max(0, (cap.usable_kwh * lim) / 100 - kwh), 'energy') : '—', { icon: 'bolt', title: 'Usable capacity at the limit minus the energy stored now' }],
        ['Buffer kept back by the car', format(cap.buffer_kwh, 'energy'), { icon: 'lock', title: 'Energy below 0 % that the car never lets you use' }],
      ]));
  }
  const paintTop = async () => {
    await batReady;
    if (!ctx.alive()) return;
    replace(chemEl, bat.chemistry?.value ? `${CHEM[bat.chemistry.value] || bat.chemistry.value} pack${bat.chemistry?.confidence === 'low' ? ' (low confidence)' : ''}` : 'Pack chemistry not known');
    replace(asOfEl, `Pack readings as of ${formatDateTime(bat.as_of_ms, tz, { now: now() })}`);
    if (state.isSold(car) || (bat.as_of_ms != null && now() - bat.as_of_ms > 3600000)) replace(nowSide, 'Last known');
    paintNow();
    ctx.events('now', (ev) => {
      if (ev.data?.car !== cid) return;
      const d = ev.data.now || {};
      const v = (x) => (x && typeof x === 'object' && 'value' in x ? x.value : undefined);
      const live = { soc: v(d.soc_pct), usable: v(d.usable_soc_pct), kwh: v(d.energy_remaining_kwh), limit: v(d.charge?.limit_pct) };
      // "Live" by the same rule as the first paint: not sold, not marked stale, readings from the last hour; an event
      // that tells neither (only the car's inputs changed) leaves the label as it is
      const asOf = d.at_ms ?? d.received_ms;
      if (d.stale != null || asOf != null) {
        const old = state.isSold(car) || d.stale === true || (asOf != null && now() - asOf > 3600000);
        replace(nowSide, old ? 'Last known' : 'Live');
      }
      if (Object.values(live).some((x) => x != null)) paintNow(Object.fromEntries(Object.entries(live).filter(([, x]) => x != null)));
    });

    /* ---------- Capacity and health */
    const health = cap.health_pct, warranty = cap.warranty_pct;
    const factor = eff?.factor ? (toIdeal ? fr.ideal_wh_per_km : fr.rated_wh_per_km) : null;
    const ring = healthRing(health, warranty);
    const pk = bat.pack || {};
    const packLine = pk.replaced_ms != null ? h('p', { class: 'note bat-note' }, icon('info', 'ico--sm'), ` Pack replaced on ${formatDate(pk.replaced_ms, tz, { weekday: false, month: 'long' })} (${pk.source === 'owner' ? 'typed by the owner' : "detected from the pack's readings"}).`) : null;
    const notesEl = (bat.notes || []).filter((c) => NOTES[c]).map((c) => h('p', { class: ['note', 'bat-note', /cold|lfp/.test(c) && 'warn'] }, icon(/cold|lfp/.test(c) ? 'warn' : 'info', 'ico--sm'), NOTES[c]));
    const readingsWord = cap.readings == null ? '' : ` (${n0(cap.readings)} ${cap.readings === 1 ? 'reading' : 'readings'})`;
    const capPanel = panel({ title: 'Capacity and health', icon: 'battery', cls: 'bat-cap', cover: 'shots.battery-health' },
      h('div', { class: 'cap-body' },
        h('div', { class: 'cap-ring', cover: ['bh.bar.health', 'bh.gauge.degradation'] }, ring,
          h('div', { class: 'cap-ring-text' },
            h('span', { class: 'cap-ring-val' }, health == null ? '—' : formatNumber(health, { maximumFractionDigits: 1 }), h('small', null, '%')),
            h('span', { class: 'metric-label' }, 'Health'),
            h('span', { class: 'cap-ring-sub' }, cap.degradation_pct == null ? 'Lost: not known' : `${formatNumber(cap.degradation_pct, { maximumFractionDigits: 1 })} % lost`))),
        h('div', { class: 'cap-figs' },
          metric('Usable now', cap.usable_kwh, 'energy', { cover: 'bh.capacity.usable_now', explain: cap.usable_kwh == null ? null : cap.basis === 'charges' ? `Per charge: the median of the last charges${readingsWord}, ${CHARGE_BASIS[cap.usable_basis] || 'energy over the % added'}.` : cap.full_pack_kwh != null && cap.buffer_kwh != null ? `From the BMS: full pack ${format(cap.full_pack_kwh, 'energy')} minus the ${format(cap.buffer_kwh, 'energy')} buffer the car keeps back${readingsWord}.` : `From the BMS${readingsWord}.`, why: 'The pack has not reported it' }),
          metric('Usable when new', cap.new_kwh, 'energy', { cover: 'bh.capacity.usable_new', explain: cap.new_source ? `Source: ${NEW_SOURCE[cap.new_source] || cap.new_source}.` : null, why: 'Not known: type it under Reference values' }),
          metric('Difference', cap.difference_kwh == null ? null : -cap.difference_kwh, 'energy', { cover: 'bh.capacity.difference', sign: true, explain: 'Usable now minus usable when new.', why: 'Needs a “when new” value' }),
          metric('Energy per distance of range', factor, 'efficiency', { cover: 'bh.stat.efficiency', explain: eff?.factor ? `How much energy one ${unitLabel('range')} of ${rangeWord} range stands for, from ${n0(eff.factor.readings)} pack readings. Used to turn energy into range.` : null, why: effErr ? 'Could not be loaded' : 'Not worked out yet' }),
          metric('Brick voltage spread', bat.brick_spread_v == null ? null : h('span', { class: 'q' }, h('span', { class: 'q-v' }, formatNumber(bat.brick_spread_v * 1000, { maximumFractionDigits: 0 })), '\u00a0', h('span', { class: 'q-u' }, 'mV')), null, { explain: 'Highest minus lowest cell group: the pack’s balance. Under 30 mV is well balanced.', why: 'Not reported' }),
        )),
      h('div', { class: 'cap-explain', cover: ['bh.capacity.help', 'bh.gauge.degradation_help'] },
        h('p', { class: 'note cap-sources' }, icon('info', 'ico--sm'), ` Health is usable now as a share of usable when new. ${healthSources(cap)} It is a guide, not a lab test: it moves a little with temperature and with how long since the pack last balanced.`),
        warranty != null ? h('p', { class: 'note' }, h('i', { class: 'k-warranty', 'aria-hidden': 'true' }), ` Warranty line: ${pct(warranty)} health. Above it the pack is inside the usual battery warranty threshold; the threshold is set under Reference values.`) : null,
        packLine, notesEl));

    /* ---------- Full-charge range */
    const rangePanel = panel({ title: 'Full-charge range', icon: 'gauge', cls: 'bat-range' },
      h('div', { class: 'range-figs' },
        metric('At 100 % now', rng.full_now_km, 'range', { size: 'lg', cover: 'bh.ranges.max_now', explain: `The ${rangeWord} range the car would show at a full charge${toIdeal ? ' (worked out from rated range with the two energy factors)' : ''}. From energy: ${format(rng.full_now_from_energy_km, 'range')}.`, why: 'Not enough readings' }),
        metric('At 100 % when new', rng.full_new_km, 'range', { cover: 'bh.ranges.max_new', explain: rng.new_source ? `Source: ${RANGE_SOURCE[rng.new_source] || rng.new_source}.` : null, why: 'Not known: type it under Reference values' }),
        metric('Range lost', rng.lost_km == null ? null : rng.lost_km, 'range', { cover: 'bh.ranges.lost', explain: rng.lost_km != null && rng.full_new_km ? `${formatNumber((rng.lost_km / rng.full_new_km) * 100, { maximumFractionDigits: 1 })} % of the range when new.` : 'When new minus now.', why: 'Needs a “when new” value' })),
      h('div', { class: 'range-bar', role: 'img', 'aria-label': `Range now ${format(rng.full_now_km, 'range')} of ${format(rng.full_new_km, 'range')} when new`,
        vars: { '--w': rng.full_new_km && rng.full_now_km ? `${Math.min(100, (rng.full_now_km / rng.full_new_km) * 100)}%` : '0%' } }, h('i')),
      note('The car’s range estimate scales with its own energy factor, so it drops in step with capacity. Your real range depends on speed, weather and climate; see Efficiency.'));
    capWait.replaceWith(capPanel); rangeWait.replaceWith(rangePanel);
  };

  /* ---------- Lifetime totals: GET totals (once per input too) */
  const paintLife = async () => {
    const [, t, by] = await Promise.all([batReady, totalsP, bySrcP]);
    if (!ctx.alive()) return;
    totals = t;
    lifeKinds = LIFE_KINDS.filter((k, i) => by[i] && ((by[i].drives || 0) + (by[i].charges || 0)) > 0);
    lifeWait.replaceWith(lifetimePanel());
  };

  /* ---------- Reference values: GET settings */
  const paintRef = async () => {
    const [, s] = await Promise.all([batReady, settingsP]);
    if (!ctx.alive()) return;
    settings = s;
    refWait.replaceWith(referencePanel());
  };
  const capWait = waitPanel('Capacity and health', 'battery', 'bat-cap'), rangeWait = waitPanel('Full-charge range', 'gauge', 'bat-range');
  const lifeWait = waitPanel('Lifetime totals', 'odo', 'bat-life'), refWait = waitPanel('Reference values', 'settings', 'bat-ref');
  nowSlot.append(h('div', { class: 'skel skel--block', 'aria-hidden': 'true' }));

  /* ---------- history sections (slots filled after the first paint) */
  const capChartSlot = h('div', { class: 'cap-chart-slot' });
  const capSide = h('span', { class: 'panel-side' });
  const capHist = panel({ title: 'Capacity over the car’s life', icon: 'stats', cls: 'bat-caphist', side: capSide, cover: ['bh.chart.capacity_by_distance.points', 'bh.chart.capacity_by_distance.median'] }, capChartSlot);

  const clSide = h('span', { class: 'panel-side' });
  const clSlot = h('div', { class: 'cl-slot' });
  const clPanel = panel({ title: 'Charge level', icon: 'battery', cls: 'bat-cl', side: clSide, cover: ['cl.chart.charge_level', 'shots.charge-level'] }, clSlot);

  const prSide = h('span', { class: 'panel-side' });
  const prSlot = h('div', { class: 'pr-slot' });
  const prPanel = panel({ title: 'Projected full-charge range', icon: 'gauge', cls: 'bat-pr', side: prSide, cover: ['shots.projected-range'] }, prSlot);

  const vdSlot = h('div', { class: 'vd-slot' });

  replace(ctx.view, h('div', { class: 'battery-page' },
    head,
    h('div', { class: 'bat-grid' },
      panel({ title: 'Now', icon: 'live', cls: 'bat-nowp', side: nowSide }, nowSlot),
      capWait,
      rangeWait, lifeWait,
      capHist, refWait),
    h('div', { class: 'bat-history-head' },
      h('h2', { class: 'section-title' }, 'In the period'),
      periodBtn(['cl.filter.time_range', 'pr.filter.time_range', 'vd.filter.time_range']),
      h('span', { class: 'faint' }, `Times in the car’s zone. Shaded blue: asleep (the battery holds). Grey: unknown, no log.`)),
    clPanel, prPanel, vdSlot));

  for (const slot of [capChartSlot, clSlot, prSlot, vdSlot]) slot.append(h('div', { class: 'skel skel--block', 'aria-hidden': 'true' }));
  const sync = `battery-${cid}`;
  const uplot = (paint) => async () => { await uplotP; if (ctx.alive()) await paint(); };
  // GET battery failing fails the page (as before); the history sections show their own errors
  await Promise.all([
    paintTop(), paintLife(), paintRef(),
    uplot(paintCapacity)().catch((e) => replace(capChartSlot, errorState(e, { compact: true, home: false, retry: () => paintCapacity() }))),
    uplot(paintChargeLevel)().catch((e) => replace(clSlot, errorState(e, { compact: true, home: false, retry: () => paintChargeLevel() }))),
    uplot(paintProjected)().catch((e) => replace(prSlot, errorState(e, { compact: true, home: false, retry: () => paintProjected() }))),
    uplot(paintParked)(),
  ]);
  // live: a drive, charge or parked spell of the car started or ended (a burst of them, once it has passed): the
  // capacity, lifetime and parked figures change, so the page is built again off screen and swapped in whole
  onRows(ctx.api, { car: cid, kinds: ['drives', 'charges', 'states', 'visits'], waitMs: 1500, run: () => { if (ctx.alive()) ctx.refresh(); } });
  // ready once every section has its data: the charts and the parked list are part of the first render
  if (ctx.alive()) ctx.ready();
  return {};

  /* ================================================================ sections */

  // "Drives and charges counted from SMO and the Tesla API.": only the inputs that gave this car rows
  function lifeSource() {
    if (!lifeKinds.length) return null;
    const list = lifeKinds.map((k) => (totals?.sources || []).find((x) => x.kind === k) || { kind: k });
    const words = lifeKinds.map((k) => LIFE_WORDS[k]);
    const line = words.length > 1 ? `${words.slice(0, -1).join(', ')} and ${words[words.length - 1]}` : words[0];
    return h('p', { class: 'note life-src' }, 'Drives and charges counted from ', srcMark({ sources: list, source_label: `${line}.` }));
  }

  function lifetimePanel() {
    const t = totals || {};
    // from the valid odometer readings only, never below 0 (the server picks them)
    const mileage = t.driven_since_km ?? (t.odometer_last_km != null && t.odometer_first_km != null ? Math.max(0, t.odometer_last_km - t.odometer_first_km) : null);
    // charges whose kind no input told are neither AC nor DC: their own part, only where there are any
    const unknownKwh = life.unknown_kwh > 0 ? life.unknown_kwh : 0;
    const inKwh = (life.ac_kwh || 0) + (life.dc_kwh || 0) + unknownKwh + (life.regen_kwh || 0);
    const parts = [
      { key: 'ac', label: 'AC charging', kwh: life.ac_kwh, cls: 'p-ac' },
      { key: 'dc', label: 'DC fast charging', kwh: life.dc_kwh, cls: 'p-dc' },
      ...(unknownKwh ? [{ key: 'unknown', label: 'Charging, kind not known', kwh: unknownKwh, cls: 'p-unknown' }] : []),
      { key: 'regen', label: 'Regen braking', kwh: life.regen_kwh, cls: 'p-regen' },
    ];
    const share = (k) => (inKwh ? ((k || 0) / inKwh) * 100 : 0);
    // what left the pack, of what is known (a car whose counters were never read has neither)
    const outOfPack = [life.drive_kwh != null ? `${format(life.drive_kwh, 'energy')} driving` : null, life.standing_kwh != null ? `${format(life.standing_kwh, 'energy')} while parked` : null].filter(Boolean);
    const split = h('div', { class: 'acdc', cover: 'bh.chart.ac_dc' },
      h('div', { class: 'acdc-head' }, h('span', { class: 'metric-label' }, 'Energy into the pack, by source'),
        csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'battery-energy-sources'), [...parts, { label: 'Driving (out)', kwh: life.drive_kwh }, { label: 'Parked (out)', kwh: life.standing_kwh }].map((p) => ({ source: p.label, kwh: p.kwh, share: p.key ? share(p.kwh) : null })), [column('Source', 'source'), column('Energy', 'kwh', 'energy'), column('Share of energy in', 'share', 'percent', { digits: 1 })]) })),
      h('div', { class: 'acdc-bar', role: 'img', 'aria-label': parts.map((p) => `${p.label} ${formatNumber(share(p.kwh), { maximumFractionDigits: 0 })} %`).join(', ') },
        parts.map((p) => h('i', { class: p.cls, vars: { '--w': `${share(p.kwh)}%` }, title: `${p.label}: ${format(p.kwh, 'energy')}` }))),
      h('div', { class: 'acdc-key' }, parts.map((p) => h('span', null, h('i', { class: p.cls }), p.label, h('b', null, ` ${formatNumber(share(p.kwh), { maximumFractionDigits: 0 })} %`), h('span', { class: 'faint' }, ` ${format(p.kwh, 'energy')}`)))),
      h('p', { class: 'note' }, `From the pack’s own lifetime counters (${life.basis || 'pack counters'}), so it covers years before logging too.${outOfPack.length ? ` Out of the pack: ${outOfPack.join(', ')}.` : ''}`));

    return panel({ title: 'Lifetime totals', icon: 'odo', cls: 'bat-life' },
      totalsErr ? h('p', { class: 'note warn', role: 'status' }, `The logged drives and charges could not be loaded: ${totalsErr.message} A dash below stands for a figure that is not known, not for none. `,
        h('button', { class: 'btn btn--sm', type: 'button', onclick: () => ctx.reload() }, icon('reset', 'ico--sm'), 'Try again')) : null,
      lifeSource(),
      h('div', { class: 'life-cols' },
        h('div', { class: 'life-col' },
          h('h3', { class: 'life-h' }, icon('route', 'ico--sm'), 'Driving', h('a', { class: 'panel-link', href: ctx.link('statistics'), cover: 'bh.drive.link_stats' }, 'Statistics', icon('chev-r', 'ico--sm'))),
          kv([
            ['Odometer', format(t.odometer_last_km, 'odometer'), { cover: 'bh.drive.odometer', title: 'The car’s own odometer, last reading' }],
            ['Driven since logging began', format(mileage, 'distance'), { cover: 'bh.drive.mileage', title: 'Last odometer minus the first logged one' }],
            ['Logged in drives', h('a', { class: 'link', href: ctx.link('drives', { period: { preset: 'all' } }) }, format(t.distance_km, 'distance'), ` · ${n0(t.drives)} drives`), { cover: 'bh.drive.logged' }],
            ['Not logged', t.not_logged_km ? h('a', { class: ['link', t.not_logged_km > 1 && 'warn'], href: state.buildUrl('health', { car: cid, period: { preset: 'all' } }), title: 'See each gap, over the whole life of the car' }, format(t.not_logged_km, 'distance'), ' · see the gaps') : format(t.not_logged_km, 'distance'), { cover: 'bh.drive.data_lost' }],
          ]),
          h('p', { class: 'note', cover: 'bh.drive.help' }, 'Not logged is distance the odometer moved while nothing was recorded (no log, a log that failed its check). Each gap is listed in Data health.')),
        h('div', { class: 'life-col' },
          h('h3', { class: 'life-h' }, icon('bolt', 'ico--sm'), 'Charging', h('a', { class: 'panel-link', href: ctx.link('charges', { period: { preset: 'all' } }), cover: 'bh.charging.link_stats' }, 'Charges', icon('chev-r', 'ico--sm'))),
          kv([
            ['Charging sessions', `${n0(t.charges)}  (AC ${n0(t.ac?.charges)}, DC ${n0(t.dc?.charges)}${t.unknown_kind?.charges ? `, ${n0(t.unknown_kind.charges)} of no known kind` : ''})`, { cover: 'bh.charging.count' }],
            ['Energy added to the pack (pack side)', format(t.energy_added_kwh, 'energy'), { cover: 'bh.charging.energy_added', title: 'What went into the pack, as the car counts it: more than the pack stores, since charging loses some in the cells' }],
            ['Energy from the grid (wall side)', h('span', null, format(t.energy_grid_kwh, 'energy'), t.grid_coverage_pct != null && t.grid_coverage_pct < 100 ? h('span', { class: 'faint' }, ` (measured for ${pct(t.grid_coverage_pct)} of energy)`) : null), { cover: 'bh.charging.energy_used' }],
            ['Charging efficiency', t.charging_efficiency_pct == null ? '— not measured' : pct(t.charging_efficiency_pct, 1), { cover: 'bh.charging.efficiency' }],
            ['Equivalent full cycles', life.equivalent_cycles == null ? '—' : formatNumber(life.equivalent_cycles, { maximumFractionDigits: 1 }), { cover: 'bh.charging.cycles', title: 'Energy charged divided by usable capacity' }],
          ]),
          h('p', { class: 'note', cover: 'bh.charging.help' }, 'Efficiency is energy into the pack (pack side) over energy from the grid (wall side), for AC charges where the car measured both. Neither is what the pack stores, so capacity is never worked out from them. DC fast chargers do not report their grid side, so they are left out instead of counted as 100 %. A full cycle is one usable pack’s worth of charging.'))),
      split);
  }

  function referencePanel() {
    const s = settings || {};
    const can = !!ctx.admin && !!settings;
    const rUnit = unitLabel('range');
    const capF = field({ label: 'Usable capacity when new (kWh)', name: 'capacity_new_kwh', type: 'number', value: s.capacity_new_kwh, path: 'capacity_new_kwh',
      hint: `Leave empty to use ${NEW_SOURCE[cap.new_source === 'owner' ? 'early_bms' : cap.new_source] || 'the earliest readings'}. 10 to 250 kWh.`, attrs: { min: 10, max: 250, step: 0.1, inputmode: 'decimal', disabled: !can } });
    const rngF = field({ label: `Full-charge range when new (${rUnit})`, name: 'full_range_new_km', type: 'number', value: s.full_range_new_km == null ? null : Math.round(convert(s.full_range_new_km, 'range')), path: 'full_range_new_km',
      hint: 'Leave empty to use the median full-charge range of the first 30 days.', attrs: { min: 1, step: 1, inputmode: 'numeric', disabled: !can } });
    const warF = field({ label: 'Warranty threshold (% health)', name: 'warranty_threshold_pct', type: 'number', value: s.warranty_threshold_pct, path: 'warranty_threshold_pct',
      hint: 'The health your battery warranty promises, often 70 %.', attrs: { min: 50, max: 100, step: 1, inputmode: 'numeric', disabled: !can } });
    let version = s.version;
    const f = form({
      fields: [capF, rngF, warF], submit: 'Save reference values',
      async onSubmit(v) {
        if (!can) return;
        const body = { version, capacity_new_kwh: v.capacity_new_kwh, full_range_new_km: v.full_range_new_km == null ? null : Math.round(toMetric(v.full_range_new_km, 'range') * 10) / 10, warranty_threshold_pct: v.warranty_threshold_pct };
        try {
          const r = await ctx.api.patch(`${base}/settings`, body);
          version = r.version;
          flash('Reference values saved. Health and range use them now.', { next: true });
          ctx.reload();
        } catch (err) { showApiError(f, err); }
      },
    });
    if (!can) f.el.querySelector('button[type="submit"]')?.setAttribute('disabled', '');
    capF.el.dataset.cover = 'bh.setting.capacity_new_override';
    rngF.el.dataset.cover = 'bh.setting.range_new_override';
    return panel({ title: 'Reference values', icon: 'settings', cls: 'bat-ref', side: h('a', { class: 'panel-link', href: state.buildUrl('settings-cars', { car: cid }) }, 'Car settings', icon('chev-r', 'ico--sm')) },
      h('p', { class: 'note', cover: 'bh.setting.override_help' }, 'Typed once for this car and checked when saved. Use the figures from the car’s papers or its first weeks; they set what “when new” means for health and range lost. Each car has its own.'),
      f.el,
      !settings ? h('p', { class: 'note warn' }, 'The car’s settings could not be read, so they cannot be changed here.')
        : !ctx.admin ? h('p', { class: 'note' }, icon('lock', 'ico--sm'), ' Only an admin can change these.') : null,
      bat.chemistry ? h('p', { class: 'note' }, `Chemistry: ${CHEM[bat.chemistry.value] || 'not known'}, ${bat.chemistry.source === 'owner' ? 'set by the owner' : 'detected from the pack'}.${s.battery_chemistry?.evidence && bat.chemistry.source !== 'owner' ? ` Evidence: ${s.battery_chemistry.evidence.replace(/\.$/, '')}.` : ''} Change it in Car settings.`) : null);
  }

  /* ---------- capacity history */
  async function paintCapacity() {
    const seg = segment([['time', 'By date'], ['odometer', 'By distance']], opt.capX, (v) => { opt.capX = v; ctx.setQuery({ cap_x: v === 'odometer' ? v : null }); paintCapacity(); }, 'Capacity against');
    const segG = segment([['charge', 'Per charge'], ['day', 'Per day']], opt.capGroup, (v) => { opt.capGroup = v; ctx.setQuery({ cap_group: v === 'day' ? v : null }); paintCapacity(); }, 'One reading');
    const [pts, med] = await Promise.all([
      collect(`${base}/battery/capacity`, { group: opt.capGroup, x: opt.capX }),
      ctx.api.get(`${base}/battery/capacity`, { group: 'month', x: opt.capX, limit: 500 }),
    ]);
    if (!ctx.alive()) return;
    const items = pts.items, trend = pts.trend || med.trend;
    const csv = csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'battery-capacity', opt.capGroup), items, [timeColumn('Time', 't_ms', tz), column('Odometer', 'odometer_km', 'odometer'), column('Full pack', 'full_pack_kwh', 'energy'), column('Usable', 'usable_kwh', 'energy'), column('Method', 'method'), column('Basis', 'usable_basis'), column('Energy added', 'energy_added_kwh', 'energy'), column('Energy stored', 'stored_kwh', 'energy'), column('Charge', 'charge'), { label: 'Data from', value: (it) => srcText(it) }]) });
    replace(capSide, seg, segG, csv);
    if (!items.length) { replace(capChartSlot, emptyState({ icon: 'battery', title: 'No capacity readings yet', text: opt.capGroup === 'charge' ? 'A reading needs a finished charge that added at least 10 %. Try Per day for the pack’s own daily readings.' : 'The pack reports its capacity while the car is awake; none is stored yet.' })); return; }

    const xOf = (it) => (opt.capX === 'odometer' ? convert(it.odometer_km, 'odometer') : it.t_ms / 1000);
    const P = items.map((it) => ({ x: xOf(it), y: it.usable_kwh, it, can: it.method === 'charge' && it.source === 'can' })).filter((p) => p.x != null && p.y != null);
    const M = (med.items || []).map((it) => ({ x: xOf(it), y: it.usable_kwh })).filter((p) => p.x != null && p.y != null);
    // one x column for both series (uPlot needs sorted x)
    // per charge, the imported (and API) charges and SMO's own (CAN) are two series, so each is labelled
    const all = [...P.map((p) => ({ ...p, s: p.can ? 4 : 1 })), ...M.map((p) => ({ ...p, s: 2 }))].sort((a, b) => a.x - b.x);
    const xs = all.map((p) => p.x), ys1 = all.map((p) => (p.s === 1 ? p.y : null)), ys2 = all.map((p) => (p.s === 2 ? p.y : null));
    const ys4 = all.map((p) => (p.s === 4 ? p.y : null));
    const perCharge = opt.capGroup === 'charge', hasCan = P.some((p) => p.can), hasOther = P.some((p) => !p.can);
    const anyAdded = items.some((it) => it.usable_basis === 'energy_added');
    // least-squares trend over the readings
    const fit = linFit(P);
    const ys3 = all.map((p) => (fit ? fit.a + fit.b * p.x : null));
    await batSettled; // the capacity when new comes with GET battery
    if (!ctx.alive()) return;
    const newKwh = cap.new_kwh;
    const pk = bat.pack || {};
    const readout = h('span', { class: 'val' }, 'Point at a reading; click a per-charge reading to open that charge.');
    const plotEl = h('div', { class: 'chart chart--cap' });
    const proj = trend?.projection;
    const slope = trend?.slope_kwh_per_10000_km;
    replace(capChartSlot,
      h('div', { class: 'chart-label' },
        !perCharge ? h('span', { class: 'key', vars: { '--c': 'var(--c-soc)' } }, h('i'), 'Each day (BMS)') : null,
        perCharge && hasOther ? h('span', { class: 'key cap-key-imported', vars: { '--c': 'var(--c-soc)' } }, h('i'), 'Per charge, imported charges (energy stored)') : null,
        perCharge && hasCan ? h('span', { class: 'key cap-key-can', vars: { '--c': 'var(--c-power)' } }, h('i'), 'Per charge, CAN (energy stored)') : null,
        h('span', { class: 'key', vars: { '--c': 'var(--live)' } }, h('i'), 'Monthly median (BMS)'),
        h('span', { class: 'key', vars: { '--c': 'var(--warm)' } }, h('i'), 'Trend'),
        newKwh != null ? h('span', { class: 'key', vars: { '--c': 'var(--text-3)' } }, h('i'), `When new ${format(newKwh, 'energy')}`) : null,
        pk.replaced_ms != null ? h('span', { class: 'key', vars: { '--c': 'var(--warm)' } }, h('i'), 'Pack replaced: left of the line is the previous pack') : null,
        h('span', { class: 'chart-unit' }, 'usable kWh'), readout),
      // what the readings came from: every input among them, once
      h('p', { class: 'note cap-src' }, 'Readings from: ', srcMark({ sources: [...new Map(items.flatMap((it) => it.sources || []).map((x) => [`${x.kind}:${x.site || ''}`, x])).values()] }),
        '. Pointing at a reading names its own.'),
      plotEl,
      h('p', { class: 'note' },
        slope == null ? 'Not enough readings for a trend yet.'
          : slope >= 0 ? 'The pack is not losing capacity at the moment.'
            : `Trend: ${format(Math.abs(slope) / convert(1, 'distance'), 'energy')} less per ${formatNumber(10000)} ${unitLabel('distance')} driven.`,
        proj ? ` At this rate the pack reaches the ${pct(proj.warranty_pct)} warranty line (${format(proj.full_pack_kwh_at_warranty, 'energy')}) at about ${format(proj.odometer_km, 'odometer')}, around ${formatDate(proj.t_ms, tz, { weekday: false, month: 'long' })}. A straight line is a rough guide: packs usually lose more early and less later.` : '',
        h('span', { class: 'cap-method' }, ' Per charge: the energy the charge stored over the % it added, on the same basis as the BMS (usable, pack side): the rated range it added × the car’s own energy per km of range from the BMS. Not the energy added: that also counts what charging loses in the pack and reads a few % high; the grid energy (wall side) is never used.',
          anyAdded ? ' Charges marked “energy added” had no BMS energy readings to work with and read high.' : '',
          ' Per day and the monthly median: the BMS’s own reading (full pack minus the buffer).')));
    const make = (width, height) => {
      const c = colors();
      const soc = cssVar('--c-soc'), can = cssVar('--c-power');
      return {
        width, height, padding: [rpx(10), rpx(14), 0, 0], legend: { show: false },
        cursor: { drag: { x: false, y: false }, points: { size: rpx(9), width: rpx(2), stroke: c.cursor }, dataIdx: (u, si, i) => nearestNonNull(u.data[si], i) },
        scales: { x: { time: opt.capX === 'time' }, y: { range: (u, a, b) => { const lo = Math.min(a, newKwh ?? a), hi = Math.max(b, newKwh ?? b); const pad = Math.max(0.5, (hi - lo) * 0.15); return [lo - pad, hi + pad]; } } },
        axes: [
          opt.capX === 'time' ? timeAxis(tz, { spanMs: (xs[xs.length - 1] - xs[0]) * 1000 + 3 * DAY })
            : { ...valueAxis(), size: rpx(26), gap: rpx(6), space: rpx(90), values: (u, v) => v.map((x) => `${formatNumber(x, { maximumFractionDigits: 0 })} ${unitLabel('odometer')}`) },
          valueAxis({ digits: 1 })],
        series: [{},
          { label: 'Reading', stroke: soc, width: 0, paths: () => null, points: { show: true, space: 0, size: rpx(opt.capGroup === 'day' ? 4 : 6), width: 0, fill: hexA(soc, 0.7), stroke: hexA(soc, 0.7) } },
          { label: 'Median', stroke: c.live, width: rpx(2.25), spanGaps: true, points: { show: false } },
          { label: 'Trend', stroke: c.warm, width: rpx(1.5), dash: [rpx(6), rpx(4)], spanGaps: true, points: { show: false } },
          { label: 'CAN reading', stroke: can, width: 0, paths: () => null, points: { show: true, space: 0, size: rpx(6), width: 0, fill: hexA(can, 0.75), stroke: hexA(can, 0.75) } }],
        hooks: {
          draw: [(u) => {
            // the day the pack was replaced: readings left of the line are the previous pack's
            const swapX = pk.replaced_ms == null ? null : opt.capX === 'time' ? pk.replaced_ms / 1000 : P.find((p) => p.it.t_ms >= pk.replaced_ms)?.x;
            if (swapX != null && swapX >= u.scales.x.min && swapX <= u.scales.x.max) {
              const x = Math.round(u.valToPos(swapX, 'x', true)) + 0.5, g = u.ctx;
              g.save(); g.strokeStyle = c.warm; g.lineWidth = Math.max(1, rpx(1.5)); g.beginPath(); g.moveTo(x, u.bbox.top); g.lineTo(x, u.bbox.top + u.bbox.height); g.stroke();
              g.fillStyle = c.axis; g.font = `${rpx(11)}px sans-serif`; g.textAlign = 'right'; g.fillText('previous pack ', x, u.bbox.top + rpx(12)); g.restore();
            }
            if (newKwh == null) return;
            const y = u.valToPos(newKwh, 'y', true), cx = u.ctx;
            cx.save(); cx.strokeStyle = c.axis; cx.lineWidth = Math.max(1, rpx(1)); cx.setLineDash([rpx(2), rpx(3)]);
            cx.beginPath(); cx.moveTo(u.bbox.left, y); cx.lineTo(u.bbox.left + u.bbox.width, y); cx.stroke(); cx.restore();
          }],
          setCursor: [(u) => {
            const i = pick(u);
            if (i == null) { readout.textContent = 'Point at a reading; click a per-charge reading to open that charge.'; return; }
            const it = all[i].it;
            readout.textContent = `${formatDate(it.t_ms, tz, { weekday: false })} · ${format(it.odometer_km, 'odometer')} · usable ${format(it.usable_kwh, 'energy')} (full pack ${format(it.full_pack_kwh, 'energy')}) · ${readingLabel(it)}${it.stored_kwh != null ? ` · stored ${format(it.stored_kwh, 'energy')} of ${format(it.energy_added_kwh, 'energy')} added` : ''} · ${srcShort(it) || 'source not known'}${it.charge ? ' · click to open the charge' : ''}`;
          }],
          ready: [(u) => {
            u.over.addEventListener('click', () => {
              const i = pick(u);
              const it = i != null ? all[i]?.it : null;
              if (it?.charge) ctx.navigate(state.buildUrl('charge', { car: cid, charge: it.charge }));
            });
            u.over.classList.add('is-clickable');
          }],
        },
      };
    };
    // the reading under the cursor, of either point series
    const pick = (u) => {
      if (u.cursor.left < 0) return null;
      const i = u.cursor.idxs?.[1], j = u.cursor.idxs?.[4];
      if (i != null && ys1[i] != null) return i;
      return j != null && ys4[j] != null ? j : null;
    };
    mountPlot(plotEl, make, [xs, ys1, ys2, ys3, ys4], ctx);
  }

  /* ---------- charge level */
  async function paintChargeLevel() {
    const from = period.from_ms ?? (await firstMs());
    const to = period.to_ms ?? now();
    const longSpan = to - from > 62 * DAY;
    const bucket = levelBucket(opt, from, to);
    const d = await levelRead(ctx, opt, from, to);
    if (!ctx.alive()) return;
    const segB = segment([['hour', 'Hourly'], ['day', 'Daily']], bucket, (v) => { opt.clBucket = v; ctx.setQuery({ cl_bucket: v }); paintChargeLevel(); }, 'Bucket size', { cover: 'cl.control.bucket_width', disabled: longSpan ? ['hour'] : [], why: 'Hourly is for periods up to 62 days.' });
    const mov = h('label', { class: 'check filter-check', cover: 'cl.control.include_moving', title: 'Show or hide the moving average, median and the 7.5 and 92.5 percentile lines' },
      h('input', { type: 'checkbox', checked: opt.clMoving, onchange: (e) => { opt.clMoving = e.currentTarget.checked; ctx.setQuery({ cl_moving: opt.clMoving ? null : '0' }); paintChargeLevel(); } }), h('span', null, 'Moving lines'));
    const segW = segment([['7d', '7 days'], ['30d', '30 days']], opt.clWindow, (v) => { opt.clWindow = v; ctx.setQuery({ cl_window: v === '30d' ? v : null }); paintChargeLevel(); }, 'Moving window', { cover: 'cl.control.window_width', disabledAll: !opt.clMoving, why: 'Turn the moving lines on first.' });
    const b = d.buckets || [];
    const ts = b.map((x) => x.t_ms);
    // asleep is not unknown: the level holds while the car sleeps, so the lines go on through asleep
    // buckets; they break only where nothing is known
    const bms = d.bucket_ms || 3600000;
    const asleepAt = (t) => (d.spans || []).some((sp) => sp.state === 'asleep' && sp.start_ms <= t + bms && (sp.end_ms ?? Infinity) >= t);
    const isAsleep = b.map((x) => x.soc_pct == null && asleepAt(x.t_ms));
    const soc = carry(b.map((x) => x.soc_pct), isAsleep), usable = carry(b.map((x) => x.usable_soc_pct), isAsleep);
    const mv = d.moving || {};
    const limits = [...(d.charge_limit || [])].sort((a, z) => a.t_ms - z.t_ms);
    const limAt = (t) => { let v = null; for (const l of limits) { if (l.t_ms <= t + (d.bucket_ms || 0)) v = l.pct; else break; } return v; };
    const lim = ts.map((t) => limAt(t));
    const rows = b.map((x, i) => ({ t_ms: x.t_ms, soc: x.soc_pct, usable: x.usable_soc_pct, mean: mv.mean?.[i], median: mv.median?.[i], p7_5: mv.p7_5?.[i], p92_5: mv.p92_5?.[i], limit: lim[i] }));
    const csv = csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'charge-level', period.preset, bucket), rows, [timeColumn('Bucket start', 't_ms', tz), column('Battery', 'soc', 'percent', { digits: 2 }), column('Usable', 'usable', 'percent', { digits: 2 }), column(`Moving mean ${opt.clWindow}`, 'mean', 'percent', { digits: 2 }), column(`Moving median ${opt.clWindow}`, 'median', 'percent', { digits: 2 }), column('Moving 7.5th percentile', 'p7_5', 'percent', { digits: 2 }), column('Moving 92.5th percentile', 'p92_5', 'percent', { digits: 2 }), column('Charge limit', 'limit', 'percent')]) });
    replace(clSide, segB, mov, segW, csv);
    if (!b.some((x) => x.soc_pct != null)) { replace(clSlot, emptyState({ icon: 'battery', title: 'No battery readings in this period', text: 'The car sent nothing in this period (unknown time). Choose a longer period in the period picker.' })); return; }

    const st = d.stats || {};
    const stat = (a) => { const f = a.filter((v) => v != null); if (!f.length) return null; return { mean: f.reduce((s, v) => s + v, 0) / f.length, min: Math.min(...f), max: Math.max(...f), last: f[f.length - 1] }; };
    const sU = stat(usable);
    if (!d.band) await batSettled;
    if (!ctx.alive()) return;
    const band = d.band || bat.band || {};
    const entries = [
      { key: 'soc', label: 'Battery', color: '--c-soc', index: 1, cover: 'cl.chart.battery_level', st: { mean: st.mean, min: st.min, max: st.max } },
      { key: 'usable', label: 'Usable', color: '--c-power', index: 2, cover: 'cl.chart.usable_level', st: sU },
      ...(opt.clMoving ? [
        { key: 'mean', label: `Moving average (${opt.clWindow})`, color: '--live', index: 3, cover: 'cl.chart.moving_avg', st: stat(mv.mean || []) },
        { key: 'median', label: `Moving median (${opt.clWindow})`, color: '--c-elev', index: 4, cover: 'cl.chart.moving_median', st: stat(mv.median || []) },
        { key: 'p7_5', label: '7.5th percentile', color: '--text-3', index: 5, cover: 'cl.chart.moving_p075', st: stat(mv.p7_5 || []) },
        { key: 'p92_5', label: '92.5th percentile', color: '--text-3', index: 6, cover: 'cl.chart.moving_p925', st: stat(mv.p92_5 || []) },
      ] : []),
      { key: 'limit', label: 'Charge limit', color: '--warm', index: opt.clMoving ? 7 : 3, st: stat(lim) },
    ];
    const plotEl = h('div', { class: 'chart chart--cl' });
    const plot = {};
    const chips = entries.map((e) => {
      const btn = h('button', { class: 'legend-chip', type: 'button', 'aria-pressed': 'true', title: 'Show or hide this line', cover: e.cover || null },
        h('i', { class: ['legend-swatch', /^p/.test(e.key) && 'is-dashed'], vars: { '--c': `var(${e.color})` } }), h('span', null, e.label), steady(h('b', { class: 'legend-val' }, '')));
      btn.addEventListener('click', () => { const on = btn.getAttribute('aria-pressed') !== 'true'; btn.setAttribute('aria-pressed', String(on)); plot.p?.u?.setSeries(e.index, { show: on }); });
      return btn;
    });
    const statsTable = h('table', { class: 'cl-stats', cover: 'cl.chart.legend_stats' },
      h('thead', null, h('tr', null, h('th', { scope: 'col' }, 'Line'), h('th', { scope: 'col', class: 'is-num' }, 'Average'), h('th', { scope: 'col', class: 'is-num' }, 'Lowest'), h('th', { scope: 'col', class: 'is-num' }, 'Highest'))),
      h('tbody', null, entries.filter((e) => e.st).map((e) => h('tr', null, h('th', { scope: 'row' }, h('i', { class: 'legend-swatch', vars: { '--c': `var(${e.color})` } }), e.label), h('td', { class: 'is-num' }, pct(e.st.mean, 1)), h('td', { class: 'is-num' }, pct(e.st.min, 1)), h('td', { class: 'is-num' }, pct(e.st.max, 1))))));
    const hi = band.high_pct ?? 80, lo = band.low_pct ?? 20;
    replace(clSlot,
      h('div', { class: 'cl-top' },
        h('div', { class: 'legend', role: 'group', 'aria-label': 'Charge level lines' }, chips),
        steady(h('span', { class: 'cl-cursor faint', 'aria-live': 'off' }), [CL_IDLE, ...ts.map((t) => formatDateTime(t, tz, { now: now() }))])),
      plotEl,
      h('div', { class: 'cl-foot' },
        h('div', { class: 'cl-shares' },
          metric(`Time above ${pct(80)}`, d.share_above_80_pct, 'percent', { size: 'sm', digits: 1, explain: hi >= 100 ? 'An LFP pack can sit high without harm.' : 'Long stays high age a nickel pack faster.' }),
          metric(`Time below ${pct(20)}`, d.share_below_20_pct, 'percent', { size: 'sm', digits: 1, explain: 'Long stays low are hard on any pack.' }),
          metric('Median level', st.median, 'percent', { size: 'sm', digits: 1, explain: `Of the ${bucket === 'hour' ? 'hourly' : 'daily'} levels in the period.` })),
        statsTable),
      h('p', { class: 'note', cover: 'cl.chart.band' }, h('i', { class: 'k-band', 'aria-hidden': 'true' }), ` Shaded band: the everyday range ${pct(lo)} – ${pct(hi)}${hi >= 100 ? ' (LFP packs are fine up to 100 %)' : ''}. Moving lines use a fixed ${opt.clWindow === '30d' ? '30-day' : '7-day'} window of daily means ending each day, so they do not change when the period does. Shares count known time only.`));

    const xs = ts.map((t) => t / 1000);
    const spans = d.spans || [];
    const make = (width, height) => {
      const c = colors();
      const col = (v) => cssVar(v);
      const dashed = [rpx(4), rpx(3)];
      const series = [{},
        { label: 'Battery', stroke: col('--c-soc'), width: rpx(2), fill: areaFill(col('--c-soc'), 0.18, 0.01), points: { show: false } },
        { label: 'Usable', stroke: col('--c-power'), width: rpx(1.5), dash: dashed, points: { show: false } }];
      if (opt.clMoving) series.push(
        { label: 'Average', stroke: c.live, width: rpx(1.75), points: { show: false } },
        { label: 'Median', stroke: col('--c-elev'), width: rpx(1.75), points: { show: false } },
        { label: 'P7.5', stroke: c.axis, width: rpx(1), dash: [rpx(2), rpx(3)], points: { show: false } },
        { label: 'P92.5', stroke: c.axis, width: rpx(1), dash: [rpx(2), rpx(3)], points: { show: false } });
      series.push({ label: 'Limit', stroke: c.warm, width: rpx(1.5), paths: window.uPlot.paths.stepped({ align: 1 }), points: { show: false } });
      return {
        width, height, padding: [rpx(10), rpx(14), 0, 0], legend: { show: false },
        cursor: { sync: { key: sync, setSeries: false }, drag: { x: false, y: false }, points: { size: rpx(7), width: rpx(2), stroke: c.cursor } },
        scales: { x: { time: true, min: from / 1000, max: to / 1000 }, y: { range: [0, 100] } },
        axes: [timeAxis(tz, { spanMs: to - from }), { ...valueAxis({ size: 44 }), values: (u, v) => v.map((n) => `${n} %`) }],
        series,
        hooks: {
          drawClear: [spanShade(spans, lo, hi)],
          setCursor: [(u) => {
            const i = u.cursor.idx;
            const out = clSlot.querySelector('.cl-cursor');
            if (i == null || u.cursor.left < 0) { chips.forEach((ch) => { ch.querySelector('.legend-val').textContent = ''; }); if (out) out.textContent = CL_IDLE; return; }
            entries.forEach((e, k) => { chips[k].querySelector('.legend-val').textContent = pct(u.data[e.index]?.[i], 1); });
            if (out) out.textContent = formatDateTime(ts[i], tz, { now: now() });
          }],
        },
      };
    };
    const data = [xs, soc, usable];
    if (opt.clMoving) data.push(mv.mean || xs.map(() => null), mv.median || xs.map(() => null), mv.p7_5 || xs.map(() => null), mv.p92_5 || xs.map(() => null));
    data.push(lim);
    // each value in its chip gets the room of the widest it can show, so pointing at the chart resizes no chip
    entries.forEach((e, k) => chips[k].querySelector('.steady').fit(['—', ...(data[e.index] || []).map((v) => pct(v, 1))]));
    plot.p = mountPlot(plotEl, make, data, ctx);
    clSlot.querySelector('.cl-cursor').textContent = CL_IDLE;
  }

  // asleep (blue) and unknown (grey) spans, and the everyday band, behind the lines
  function spanShade(spans, lo, hi) {
    return (u) => {
      const cx = u.ctx, { left, top, width, height } = u.bbox;
      cx.save();
      const y0 = u.valToPos(hi, 'y', true), y1 = u.valToPos(lo, 'y', true);
      cx.fillStyle = hexA(cssVar('--live'), 0.06);
      cx.fillRect(left, y0, width, y1 - y0);
      cx.strokeStyle = hexA(cssVar('--live'), 0.35); cx.lineWidth = Math.max(1, rpx(1)); cx.setLineDash([rpx(5), rpx(4)]);
      for (const y of [y0, y1]) { cx.beginPath(); cx.moveTo(left, y); cx.lineTo(left + width, y); cx.stroke(); }
      cx.setLineDash([]);
      const asleep = cssVar('--st-asleep'), unknown = cssVar('--st-unknown');
      for (const s of spans) {
        const a = Math.max(left, u.valToPos(s.start_ms / 1000, 'x', true)), b = Math.min(left + width, u.valToPos((s.end_ms ?? now()) / 1000, 'x', true));
        if (b <= a) continue;
        cx.fillStyle = hexA(s.state === 'asleep' ? asleep : unknown, s.state === 'asleep' ? 0.12 : 0.24);
        cx.fillRect(a, top, Math.max(1, b - a), height);
      }
      cx.restore();
    };
  }

  /* ---------- projected range */
  async function paintProjected() {
    const { params, hourTooFine } = projectedParams(ctx, opt, now());
    const d = await ctx.api.get(`${base}/projected-range`, params);
    if (!ctx.alive()) return;
    const seg = segment([['auto', 'Auto'], ['hour', 'Hour'], ['day', 'Day'], ['week', 'Week']], hourTooFine && opt.prBucket === 'hour' ? 'auto' : opt.prBucket,
      (v) => { opt.prBucket = v; ctx.setQuery({ pr_bucket: v === 'auto' ? null : v }); paintProjected(); }, 'Resolution', { cover: 'pr.control.resolution', disabled: hourTooFine ? ['hour'] : [], why: 'Hour is for periods up to 62 days.' });
    const v = d.values || {};
    const ts = d.t_ms || [];
    const rows = ts.map((t, i) => ({ t_ms: t, ...Object.fromEntries(Object.keys(v).map((k) => [k, v[k][i]])), state: d.state?.[i], coverage: d.coverage_pct?.[i] }));
    const csv = csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'projected-range', period.preset), rows, [timeColumn('Bucket start', 't_ms', tz), column('Full range from battery %', 'full_range_km', 'range', { digits: 1 }), column('Full range from energy', 'full_range_from_energy_km', 'range', { digits: 1 }), column('Full range from usable %', 'full_range_usable_km', 'range', { digits: 1 }), column('Battery % shown', 'soc_pct', 'percent', { digits: 1 }), column('Pack % (BMS)', 'soc_bms_pct', 'percent', { digits: 1 }), column('Odometer', 'odometer_km', 'odometer', { digits: 1 }), column('Outside', 'outside_c', 'temperature', { digits: 1 }), column('Battery temperature', 'battery_c', 'temperature', { digits: 1 }), column('State', 'state'), column('Coverage', 'coverage', 'percent')]) });
    replace(prSide, seg, csv);
    if (!ts.length || !(v.full_range_km || []).some((x) => x != null)) { replace(prSlot, emptyState({ icon: 'gauge', title: 'No range readings in this period', text: 'Nothing was logged in this period. Choose a longer period in the period picker.' })); return; }
    const bucketText = d.bucket_ms ? (d.bucket_ms >= 7 * DAY ? 'week' : d.bucket_ms >= DAY ? 'day' : d.bucket_ms >= 3600000 ? `${formatNumber(d.bucket_ms / 3600000, { maximumFractionDigits: 0 })} h` : `${formatNumber(d.bucket_ms / 60000, { maximumFractionDigits: 0 })} min`) : '';
    const R = (k) => (v[k] || []).map((x) => (x == null ? null : convert(x, 'range')));
    const charts = [
      { key: 'mileage', title: 'Against distance driven', cover: ['pr.mileage.projected', 'pr.mileage.odometer'],
        left: [{ label: 'From battery %', color: '--c-soc', data: R('full_range_km') }, { label: 'From energy', color: '--live', data: R('full_range_from_energy_km'), dash: true }],
        right: { name: 'Odometer', kind: 'odometer', color: '--text-3', series: [{ label: 'Odometer', color: '--text-3', data: (v.odometer_km || []).map((x) => (x == null ? null : convert(x, 'odometer'))) }] } },
      { key: 'level', title: 'Against battery level', cover: ['pr.battery.projected_usable', 'pr.battery.projected_total', 'pr.battery.levels'],
        left: [{ label: 'From usable %', color: '--c-power', data: R('full_range_usable_km') }, { label: 'From battery %', color: '--c-soc', data: R('full_range_km'), dash: true }],
        right: { name: 'Level', kind: 'percent', series: [{ label: 'Battery % shown', color: '--c-elev', data: v.soc_pct || [] }, { label: 'Pack % (BMS)', color: '--text-3', data: v.soc_bms_pct || [], dash: true }] } },
      { key: 'temp', title: 'Against temperature', cover: ['pr.temp.projected', 'pr.temp.outdoor'],
        left: [{ label: 'From battery %', color: '--c-soc', data: R('full_range_km') }, { label: 'From usable %', color: '--c-power', data: R('full_range_usable_km'), dash: true }],
        right: { name: 'Temperature', kind: 'temperature', series: [{ label: 'Outside', color: '--warm', data: (v.outside_c || []).map((x) => (x == null ? null : convert(x, 'temperature'))) }, { label: 'Battery', color: '--c-regen', data: (v.battery_c || []).map((x) => (x == null ? null : convert(x, 'temperature'))), dash: true }] } },
    ];
    const xs = ts.map((t) => t / 1000);
    const slept = (d.state || []).map((st) => st === 'asleep');
    for (const cfg of charts) for (const s of [...cfg.left, ...cfg.right.series]) s.data = carry(s.data, slept);
    const blocks = charts.map((cfg, k) => {
      const el = h('div', { class: 'chart chart--pr' });
      const vals = h('span', { class: 'val' });
      const rUnit = cfg.right.kind === 'percent' ? '%' : unitLabel(cfg.right.kind);
      const label = h('div', { class: 'chart-label pr-label', cover: k === 0 ? ['pr.axis.titles', 'pr.control.shared_crosshair'] : null },
        h('span', { class: 'name' }, cfg.title),
        h('span', { class: 'chart-unit' }, `left axis: full-charge range (${unitLabel('range')}) · right axis: ${cfg.right.name.toLowerCase()} (${rUnit})`),
        [...cfg.left, ...cfg.right.series].map((s) => h('span', { class: ['key', s.dash && 'is-dashed'], vars: { '--c': `var(${s.color})` } }, h('i'), s.label)),
        vals);
      const block = h('div', { class: 'chart-block', cover: cfg.cover }, label, el);
      return { cfg, el, vals, block, last: k === charts.length - 1 };
    });
    replace(prSlot, blocks.map((b) => b.block),
      h('p', { class: 'note' }, `Each point is one ${bucketText} bucket: the range the car would show at 100 %, worked out from its range and its battery % (or usable %, or energy). From usable % sits lower in the cold, when part of the pack is locked; from energy uses the pack’s energy reading and the energy factor. The three charts share one crosshair. Asleep time is shaded blue; grey is unknown and the lines break there.`));
    for (const b of blocks) {
      const { cfg } = b;
      const left = cfg.left, right = cfg.right.series;
      const make = (width, height) => {
        const c = colors();
        return {
          width, height, padding: [rpx(8), rpx(4), b.last ? 0 : rpx(4), 0], legend: { show: false },
          cursor: { sync: { key: sync, setSeries: false }, drag: { x: false, y: false }, points: { size: rpx(7), width: rpx(2), stroke: c.cursor } },
          scales: { x: { time: true }, r: { auto: true, range: (u, a, z) => { const pad = Math.max(2, (z - a) * 0.12); return [a - pad, z + pad]; } }, o: cfg.right.kind === 'percent' ? { range: [0, 100] } : { auto: true } },
          axes: [timeAxis(tz, { show: b.last, spanMs: (ts[ts.length - 1] ?? 0) - (ts[0] ?? 0) }), { ...valueAxis({ size: 48 }), scale: 'r' }, { ...valueAxis({ size: 56, digits: cfg.right.kind === 'temperature' ? 0 : 0 }), scale: 'o', side: 1, grid: { show: false } }],
          series: [{},
            ...left.map((s) => ({ label: s.label, scale: 'r', stroke: cssVar(s.color), width: rpx(s.dash ? 1.5 : 2.25), dash: s.dash ? [rpx(5), rpx(3)] : undefined, points: { show: false } })),
            ...right.map((s) => ({ label: s.label, scale: 'o', stroke: hexA(cssVar(s.color), 0.85), width: rpx(1.25), dash: s.dash ? [rpx(3), rpx(3)] : undefined, points: { show: false } }))],
          hooks: {
            drawClear: [stateBuckets(d.state, xs)],
            setCursor: [(u) => {
              const i = u.cursor.idx;
              if (i == null || u.cursor.left < 0) { b.vals.textContent = ''; return; }
              const fmt = (s, k) => (s.data[i] == null ? '—' : k === 'percent' ? pct(s.data[i], 1) : `${formatNumber(s.data[i], { maximumFractionDigits: k === 'temperature' ? 1 : 0 })} ${unitLabel(k)}`);
              b.vals.replaceChildren(h('b', null, (d.bucket_ms || DAY) < DAY ? formatDateTime(ts[i], tz, { now: now() }) : formatDate(ts[i], tz, { weekday: false })), ' · ', [...left.map((s) => `${s.label} ${fmt(s, 'range')}`), ...right.map((s) => `${s.label} ${fmt(s, cfg.right.kind)}`)].join(' · '));
            }],
          },
        };
      };
      mountPlot(b.el, make, [xs, ...left.map((s) => s.data), ...right.map((s) => s.data)], ctx);
    }
  }

  // asleep and unknown buckets of a columns answer, behind the lines
  function stateBuckets(states, xs) {
    return (u) => {
      if (!states) return;
      const cx = u.ctx, { top, height } = u.bbox;
      const asleep = cssVar('--st-asleep'), unknown = cssVar('--st-unknown');
      cx.save();
      for (let i = 0; i < xs.length; i++) {
        const s = states[i];
        if (s !== 'asleep' && s !== 'unknown') continue;
        const x0 = u.valToPos(xs[i], 'x', true), x1 = u.valToPos(xs[i + 1] ?? xs[i] + (xs[i] - (xs[i - 1] ?? xs[i] - 3600)), 'x', true);
        cx.fillStyle = hexA(s === 'asleep' ? asleep : unknown, s === 'asleep' ? 0.12 : 0.24);
        cx.fillRect(x0, top, Math.max(1, x1 - x0), height);
      }
      cx.restore();
    };
  }

  /* ---------- parked drain */
  async function paintParked() {
    const sumEl = h('div', { class: 'vd-summary' });
    const minSel = h('select', { class: 'select', 'aria-label': 'Shortest parked spell', cover: 'vd.filter.min_idle',
      onchange: (e) => { opt.minH = Number(e.currentTarget.value); ctx.setQuery({ min_h: opt.minH || null }); t.reload(params()); } },
    [[0, 'Any length'], [1, 'At least 1 h'], [3, 'At least 3 h'], [6, 'At least 6 h'], [12, 'At least 12 h'], [24, 'At least 24 h']].map(([v, l]) => h('option', { value: v, selected: v === opt.minH }, l)));
    const params = () => parkedParams(ctx, opt);
    const tripHref = (r) => state.buildUrl('trip', { car: cid, period: { from: r.start_ms, to: r.end_ms ?? now() } });
    const tlHref = (r) => state.buildUrl('timeline', { car: cid, period: { from: startOf(r.start_ms, 'day', tz), to: r.end_ms ?? now() } });
    const columns = [
      { key: 'start', label: 'Parked', cover: ['vd.col.start', 'vd.link.start_details'], render: (r) => h('span', { class: 'cell-text' },
        h('a', { class: 'link', href: tripHref(r), title: 'Open exactly this span as a trip (battery and state charts)' }, formatDateTime(r.start_ms, tz, { now: now() })),
        h('span', { class: 'sub' }, r.place?.name || r.address || 'Not a saved place')), csv: { label: 'Start', value: (r) => formatIso(r.start_ms) } },
      { key: 'end', label: 'Until', cover: 'vd.col.end', render: (r) => h('span', { class: 'cell-text' }, r.end_ms == null ? h('span', { class: 'ok' }, 'Still parked') : h('span', null, startOf(r.end_ms, 'day', tz) === startOf(r.start_ms, 'day', tz) ? formatTime(r.end_ms, tz) : formatDateTime(r.end_ms, tz, { now: now() })),
        h('a', { class: 'sub link', href: tlHref(r) }, 'Timeline')), csv: { label: 'End', value: (r) => formatIso(r.end_ms) } },
      { key: 'length', label: 'Length', num: true, cover: 'vd.col.period', render: (r) => h('span', { class: ['nowrap', r.duration_ms >= 12 * 3600000 && 'is-long'] }, formatDuration(r.duration_ms)), csv: column('Length (h)', (r) => (r.duration_ms == null ? null : Math.round(r.duration_ms / 36000) / 100)) },
      { key: 'standby', label: 'Asleep · awake', title: 'Time asleep (blue), awake and unknown (grey, no log)', cover: 'vd.col.standby', render: standbyCell,
        csv: column('Asleep (%)', (r) => (r.duration_ms ? Math.round(((r.asleep_ms || 0) / r.duration_ms) * 1000) / 10 : null)) },
      { key: 'energy', label: 'Energy lost', num: true, cover: 'vd.col.energy', render: (r) => h('span', { class: 'cell-text cell-num' }, format(r.energy_kwh, 'energy'), r.energy_method && r.energy_method !== 'counter' ? h('span', { class: 'sub' }, ENERGY_METHOD[r.energy_method] || r.energy_method) : null), csv: column('Energy lost', 'energy_kwh', 'energy') },
      { key: 'power', label: 'Average power', num: true, cover: 'vd.col.avg_power', render: (r) => h('span', { class: 'cell-text cell-num' }, format(r.avg_power_w, 'power_w'), r.awake_avg_power_w != null ? h('span', { class: 'sub' }, `awake ${format(r.awake_avg_power_w, 'power_w')}`) : null), csv: column('Average power (W)', 'avg_power_w') },
      { key: 'soc', label: 'Battery', num: true, cover: 'vd.col.soc_diff', render: (r) => h('span', { class: 'cell-text cell-num' }, signedPct(r.soc_start_pct, r.soc_end_pct), h('span', { class: 'sub' }, `${pct(r.soc_start_pct)} → ${pct(r.soc_end_pct)}`)), csv: column('Battery lost', (r) => (r.soc_start_pct == null || r.soc_end_pct == null ? null : Math.max(0, r.soc_start_pct - r.soc_end_pct)), 'percent', { digits: 2 }) },
      { key: 'range', label: 'Range lost', title: 'Range lost, and below it range lost per hour parked', num: true, cover: 'vd.col.range_loss', render: (r) => h('span', { class: 'cell-text cell-num' }, format(r.range_lost_km, 'range', { digits: 1 }),
        h('span', { class: 'sub', cover: 'vd.col.range_per_hour', title: 'Range lost per hour parked' }, `${format(r.range_lost_per_hour_km, 'range_rate', { digits: 2 })}`)),
      csv: [column('Range lost', 'range_lost_km', 'range'), column('Range lost per hour', 'range_lost_per_hour_km', 'range_rate')] },
      { key: 'why', label: 'Kept awake by', cover: 'vd.flag.cold', render: (r) => h('span', { class: 'why-cell' },
        r.cold ? h('span', { class: 'tag tag--cold', title: `Coldest pack temperature ${format(r.battery_min_c, 'temperature')}: a cold pack reads lower and may keep itself warm` }, icon('thermo', 'ico--sm'), 'Cold pack') : null,
        (r.awake_reasons || []).length ? (r.awake_reasons || []).map((x) => tag(REASONS[x] || x)) : h('span', { class: 'faint' }, r.awake_ms > 0 ? 'No reason sent' : 'Slept throughout')),
      csv: { label: 'Awake reasons', value: (r) => [(r.cold ? 'cold pack' : null), ...(r.awake_reasons || []).map((x) => REASONS[x] || x)].filter(Boolean).join('; ') } },
      ];
    const t = dataTable(ctx, {
      title: 'Parked drain', icon: 'park', path: `${base}/parked`, params: params(), cls: 'vd-table', csvName: 'parked-drain', box: 'Parked spells', autoMore: true,
      // the CSV is built here from every spell (all pages), with readable times, the chosen units and reasons
      csv: false,
      side: h('span', { class: 'vd-side' }, h('label', { class: 'vd-min' }, h('span', { class: 'faint' }, 'Show'), minSel),
        csvButton({ onClick: async () => {
          const cols = columns.flatMap((c) => c.csv);
          const all = (await collect(`${base}/parked`, params())).items;
          downloadCsv(csvFilename(car.name, 'parked-drain', period.preset), all, [cols[0], column('Place', (r) => r.place?.name || r.address || ''), cols[1], cols[2], cols[3],
            column('Unknown, no log (h)', (r) => (r.unknown_ms == null ? null : Math.round(r.unknown_ms / 36000) / 100)), ...cols.slice(4)]);
        } })),
      cover: ['vd.table', 'shots.vampire-drain'],
      note: 'Each stay parked off a charger at one spot. Energy is measured at the pack (its own counters), so it shows small losses that whole battery % steps hide. Unknown time is never counted as asleep.',
      columns,
      empty: () => emptyState({ icon: 'park', title: 'No parked spells in this period', text: opt.minH ? `None was at least ${opt.minH} h long. Choose Any length, or a longer period.` : 'The car did not stay parked off a charger in this period. Choose a longer period.' }),
      onPage: (pg) => { if (pg.summary) paintSummary(pg.summary); },
    });
    function paintSummary(s) {
      replace(sumEl,
        metric('Parked spells', s.spells, 'count', { size: 'sm', explain: `${formatNumber(s.hours ?? 0, { maximumFractionDigits: 0 })} h parked off a charger in the period` }),
        metric('Energy lost while parked', s.energy_kwh, 'energy', { size: 'sm', explain: 'Measured at the pack.' }),
        metric('Per day parked', s.kwh_per_day, 'energy', { size: 'sm', explain: 'Energy lost per 24 h parked.' }),
        metric('What it cost', s.cost?.amount != null ? formatMoney(s.cost.amount, s.cost.currency) : null, null, { size: 'sm', explain: s.cost ? `At the ${s.cost.basis || 'home price'}.` : 'Set a home price in Places to see this.', why: 'No home price' }));
    }
    replace(vdSlot, h('div', { class: 'vd-wrap' }, sumEl, t.el));
    await t.ready;
  }

  function standbyCell(r) {
    const d = r.duration_ms || 1;
    const a = (r.asleep_ms || 0) / d * 100, u = (r.unknown_ms || 0) / d * 100, w = Math.max(0, 100 - a - u);
    return h('span', { class: 'standby' },
      h('span', { class: 'standby-bar', role: 'img', 'aria-label': `Asleep ${formatNumber(a, { maximumFractionDigits: 0 })} %, awake ${formatNumber(w, { maximumFractionDigits: 0 })} %, unknown ${formatNumber(u, { maximumFractionDigits: 0 })} %` },
        h('i', { class: 'sb-asleep', vars: { '--w': `${a}%` } }), h('i', { class: 'sb-awake', vars: { '--w': `${w}%` } }), h('i', { class: 'sb-unknown', vars: { '--w': `${u}%` } })),
      h('span', { class: 'sub' }, `${formatDuration(r.asleep_ms || 0)} asleep · ${formatDuration((r.awake_ms || 0) + (r.awake_vehicle_bus_only_ms || 0))} awake${r.unknown_ms ? ` · ${formatDuration(r.unknown_ms)} unknown` : ''}`));
  }

  /* ---------- helpers */
  function formatIso(ms) { return ms == null ? '' : toIsoZoned(ms, tz); }
  function signedPct(a, b) { if (a == null || b == null) return '—'; const dlt = Math.round(Math.max(0, a - b) * 10) / 10; return dlt === 0 ? pct(0, 1) : `−${pct(dlt, 1)}`; }
  async function collect(path, params) {
    const items = []; let trend = null;
    let after;
    for (let k = 0; k < 20; k++) {
      const pg = await ctx.api.get(path, { ...params, limit: 500, ...(after ? { after } : {}) });
      items.push(...(pg.items || [])); trend = trend || pg.trend;
      if (!pg.next) break; after = pg.next;
    }
    return { items, trend };
  }
  // carry the last value through buckets where the car was asleep (never through unknown ones)
  function carry(a, asleep) { let last = null; return (a || []).map((v, i) => { if (v != null) { last = v; return v; } return asleep[i] ? last : (last = null); }); }
  async function firstMs() {
    if (first != null) return first;
    try { const pg = await ctx.api.get(`${base}/battery/capacity`, { group: 'month', limit: 1 }); first = pg.items?.[0]?.t_ms ?? now() - 365 * DAY; } catch { first = now() - 365 * DAY; }
    return first;
  }
  function segment(choices, cur, onPick, label, { cover = null, disabled = [], disabledAll = false, why = null } = {}) {
    const el = h('span', { class: 'seg', role: 'group', 'aria-label': label, cover, title: label },
      choices.map(([v, l]) => {
        const off = disabledAll || disabled.includes(v);
        return h('button', { class: 'seg-btn', type: 'button', 'aria-pressed': String(v === cur), disabled: off, title: off ? why : null,
          onclick: () => { if (v !== cur) onPick(v); } }, l);
      }));
    return el;
  }
}

/* ---------------------------------------------------------------- outside mount */

// the health ring: health % of new, with the warranty line as a tick
function healthRing(health, warranty) {
  const r = 42, c = 2 * Math.PI * r;
  const val = Math.max(0, Math.min(100, health ?? 0));
  const tickA = warranty != null ? (warranty / 100) * 2 * Math.PI - Math.PI / 2 : null;
  const tick = tickA != null ? svg('line', { class: 'ring-warranty', x1: 50 + Math.cos(tickA) * 34, y1: 50 + Math.sin(tickA) * 34, x2: 50 + Math.cos(tickA) * 50, y2: 50 + Math.sin(tickA) * 50 }) : null;
  return svg('svg', { class: 'ring', viewBox: '0 0 100 100', 'aria-hidden': 'true' },
    svg('circle', { class: 'ring-track', cx: 50, cy: 50, r }),
    svg('circle', { class: ['ring-fill', health != null && health < (warranty ?? 0) && 'is-low'], cx: 50, cy: 50, r, 'stroke-dasharray': `${(val / 100) * c} ${c}`, transform: 'rotate(-90 50 50)' }),
    tick);
}

function nearestNonNull(arr, i) {
  if (arr[i] != null) return i;
  for (let k = 1; k < arr.length; k++) {
    if (i - k >= 0 && arr[i - k] != null) return i - k;
    if (i + k < arr.length && arr[i + k] != null) return i + k;
  }
  return i;
}

function linFit(P) {
  if (P.length < 3) return null;
  const n = P.length;
  let sx = 0, sy = 0, sxx = 0, sxy = 0;
  for (const p of P) { sx += p.x; sy += p.y; sxx += p.x * p.x; sxy += p.x * p.y; }
  const den = n * sxx - sx * sx;
  if (!den) return null;
  const b = (n * sxy - sx * sy) / den;
  return { a: (sy - b * sx) / n, b };
}
