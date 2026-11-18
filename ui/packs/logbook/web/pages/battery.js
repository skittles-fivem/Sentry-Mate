// Sentry Mate, "Logbook" UI pack: Battery (/cars/{car}/battery).
//
// How healthy the battery is, how the charge level is used, and where energy goes while parked. Data:
// GET /cars/{car}/battery (now, capacity, full-charge range, lifetime counters), /battery/capacity (the
// readings against time or distance, with the trend), /totals and /gaps (logged distance and what was
// not logged), /efficiency (the energy per rated distance), /settings (the reference values; PATCH for
// admins), /projected-range, /battery-level and /parked for the period, and /cars with each car's
// /battery, /battery-level and /parked for the comparison of every car.
//
// Capacity comes from the battery management system, never from range times a guessed factor. 'When new'
// always names its source. Asleep and unknown time are shaded apart and a line never crosses unknown
// time. Parked drain is in kWh from the pack's own counters. The section controls (resolution, bucket,
// moving lines, minimum parked hours, capacity axis) live in the address, so a link or Back shows the same
// view; the period is the frame's. Charts are uPlot (core/chart.js) or SVG in percentages of boxes sized
// in rem, so they stay sharp at any size.

import { h, svg, replace, cover } from '../shared/dom.js';
import { format, formatMoney, convert, toMetric, unitLabel, rangeLabel } from '../shared/units.js';
import { formatDate, formatDateTime, formatDuration, periodLabel, dateFormat, HOUR } from '../shared/time.js';
import { formatNumber, formatList, tn } from '../shared/i18n.js';
import { downloadCsv, csvFilename, column, timeColumn } from '../shared/csv.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { pageHead, q, figure, figures, icon, empty, errorView, loadingBlock, flag, button, sep, ago } from '../core/ui.js';
import { dataTable } from '../core/table.js';
import { stackedCharts, spansFromStates, niceSplits } from '../core/chart.js';
import { field, form, choice, switchField, setFieldError } from '../core/form.js';
import { AWAKE_WORDS } from '../core/journal.js';
import { zoneLabel } from '../core/format.js';

export const id = 'battery';
export const css = true;
export const title = () => 'Battery';

const NEW_SOURCE = {
  owner: 'typed in by the owner (Reference values below)',
  early_bms: 'the median of the pack’s own readings in its first 30 days of logging',
  imported_charges: 'the earliest imported charges (estimated)',
  pack_code: 'the pack’s size code: a hint only, type the real value below if you know it',
};
const RANGE_NEW_SOURCE = {
  owner: 'typed in by the owner (Reference values below)',
  first_30_days: 'the median full-charge range of the first 30 days of logging',
  pack_code: 'the pack’s size code: a hint only, type the real value below if you know it',
};
const NOTE_WORDS = {
  new_value_typed: 'The ‘when new’ capacity is the value typed in by the owner.',
  new_value_from_early_readings: 'The ‘when new’ capacity is the median of the earliest readings, because no value was typed in. If the car was bought used or logged late, type the real value under Reference values.',
  new_value_from_imported_charges: 'The ‘when new’ capacity comes from the earliest imported charges, which are estimated.',
  new_value_from_pack_code: 'The ‘when new’ capacity is only the nominal size of this pack type. Type the real value under Reference values for a true health figure.',
  cold_battery_locks_part: 'The battery is cold: part of its energy is locked until it warms, so usable % is lower than battery %.',
  lfp_charge_to_100_weekly: 'This is an LFP pack: charging it to 100 % about once a week keeps its level reading accurate, so 100 % is inside its everyday band.',
  imported_history_estimated: 'Part of this car’s history was imported; energy figures from it are estimated.',
  no_history: 'No history has been logged for this car yet.',
};
const CHEM = { lfp: 'LFP (iron phosphate)', nca_nmc: 'NCA / NMC (nickel)' };
const CHEM_SOURCE = { detected: 'detected from the pack', owner: 'set by the owner', imported: 'from an import' };
const METHOD = { counter: 'from the pack’s energy counters', bms_energy: 'from the pack’s stored energy', range: 'estimated from range (imported)' };
const RESOLUTIONS = [['', 'Auto'], ['hour', 'Hour'], ['day', 'Day'], ['week', 'Week']];
const BUCKETS = [['', 'Auto'], ['hour', 'Hour'], ['day', 'Day']];
const WINDOWS = [['7d', '7 days'], ['30d', '30 days']];
const GROUPS = [['charge', 'Each charge'], ['day', 'Each day'], ['month', 'Each month']];
const AXES = [['odometer', 'Against distance'], ['time', 'Against time']];
const oneOf = (list, v, dflt) => (list.some(([k]) => k === v) ? v : dflt);
const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
const p3 = (v) => `${v.toFixed(3)}%`;
const num = (v) => v != null && Number.isFinite(v);
const pctText = (v, digits = 0) => (num(v) ? `${formatNumber(v, { maximumFractionDigits: digits, minimumFractionDigits: digits })} %` : '—');
const csvButton = (label, onclick, cover = null) => h('button', { type: 'button', class: 'button button-quiet button-small', onclick, cover, 'aria-label': `Download CSV: ${label}` }, icon('download'), 'Download CSV');
const rangeWord = () => rangeLabel().toLowerCase();

export async function mount(ctx) {
  const car = ctx.car;
  const tz = ctx.tz;
  const admin = !!(ctx.me && ctx.me.role === 'admin');
  const base = api.apiPath`cars/${car.id}`;
  const file = (...p) => csvFilename(car.name, ...p, ctx.period ? periodLabel(ctx.period, tz, api.now()) : null);

  // Section controls, from the address.
  const opt = {
    res: oneOf(RESOLUTIONS, ctx.query.res || '', ''),
    bucket: oneOf(BUCKETS, ctx.query.bucket || '', ''),
    moving: ctx.query.moving !== 'off',
    window: oneOf(WINDOWS, ctx.query.window, '7d'),
    group: oneOf(GROUPS, ctx.query.cap_group, 'charge'),
    axis: oneOf(AXES, ctx.query.cap_x, 'odometer'),
    min_hours: (() => { const n = parseFloat(ctx.query.min_hours); return Number.isFinite(n) && n >= 0 ? n : 1; })(),
  };
  const remember = () => ctx.setQuery({
    res: opt.res || null, bucket: opt.bucket || null, moving: opt.moving ? null : 'off', window: opt.window === '7d' ? null : opt.window,
    cap_group: opt.group === 'charge' ? null : opt.group, cap_x: opt.axis === 'odometer' ? null : opt.axis, min_hours: opt.min_hours === 1 ? null : String(opt.min_hours),
  });

  const periodNote = (cover, what) => h('p', { class: 'bt-period', cover },
    h('span', { class: 'field-label' }, 'Period'), h('span', null, periodLabel(ctx.period, tz, api.now())), sep(), h('span', null, what), sep(), h('span', null, `times in ${zoneLabel(tz)}`),
    button('Change the period', { cls: 'button-small', iconName: 'calendar', onclick: () => document.querySelector('.period-trigger')?.click() }));

  /* ---------------------------------------------------------------- the boxes, in page order */

  const nowBox = h('section', { class: 'block bt-now', 'aria-labelledby': 'bt-now-title' }, loadingBlock(3));
  const healthBox = h('section', { class: 'block bt-health', 'aria-labelledby': 'bt-health-title' }, loadingBlock(4));
  const rangeBox = h('section', { class: 'block bt-range', 'aria-labelledby': 'bt-range-title' }, loadingBlock(3));
  const refBox = h('section', { class: 'block bt-ref', 'aria-labelledby': 'bt-ref-title' }, loadingBlock(3));
  const capBox = h('section', { class: 'block bt-cap', 'aria-labelledby': 'bt-cap-title', cover: 'shots.battery-health' }, loadingBlock(5));
  const lifeBox = h('section', { class: 'block bt-life', 'aria-labelledby': 'bt-life-title' }, loadingBlock(5));
  const projBox = h('section', { class: 'block bt-proj', 'aria-labelledby': 'bt-proj-title', cover: 'shots.projected-range' }, loadingBlock(6));
  const levelBox = h('section', { class: 'block bt-level', 'aria-labelledby': 'bt-level-title', cover: 'shots.charge-level' }, loadingBlock(5));
  const drainBox = h('section', { class: 'block bt-drain', 'aria-labelledby': 'bt-drain-title', cover: 'shots.vampire-drain' });
  const carsBox = h('section', { class: 'block bt-cars', 'aria-labelledby': 'bt-cars-title' }, loadingBlock(3));
  const asOf = h('p', { class: 'bt-asof' });

  ctx.main.append(
    pageHead({
      eyebrow: car.name, title: 'Battery',
      deck: 'How healthy the battery is, how the charge level is used, and where energy goes while parked. Capacity and lifetime figures cover the car’s whole life; the charts and the parked list follow the period.',
      aside: asOf,
    }),
    nowBox,
    h('div', { class: 'bt-grid' }, healthBox, h('div', { class: 'bt-side' }, rangeBox, refBox)),
    capBox, lifeBox,
    h('h2', { class: 'bt-part' }, 'Over the period'),
    projBox, levelBox, drainBox, carsBox);

  const head = (idName, text, note, ...actions) => h('div', { class: 'block-head' },
    h('h2', { class: 'label', id: idName }, text, note ? h('span', { class: 'label-note' }, note) : null),
    actions.filter(Boolean).length ? h('div', { class: 'block-actions' }, actions) : null);

  /* ---------------------------------------------------------------- whole-life parts */

  let bat = null, settings = null;

  async function loadTop() {
    const [b, s, eff] = await Promise.allSettled([
      ctx.api.get(api.apiPath`cars/${car.id}/battery`),
      ctx.api.get(api.apiPath`cars/${car.id}/settings`),
      ctx.api.get(api.apiPath`cars/${car.id}/efficiency`, { from_ms: ctx.params.from_ms, to_ms: ctx.params.to_ms }),
    ]);
    if (b.status === 'rejected') throw b.reason;
    bat = b.value;
    settings = s.status === 'fulfilled' ? s.value : null;
    replace(asOf, bat.as_of_ms ? ['Battery readings as of ', ago(bat.as_of_ms)] : 'No battery readings yet');
    drawNow();
    drawHealth(eff.status === 'fulfilled' ? eff.value : null, eff.status === 'rejected' ? eff.reason : null);
    drawRange();
    drawRef(s.status === 'rejected' ? s.reason : null);
  }

  function drawNow() {
    const b = bat;
    const band = b.band || {};
    const soc = b.soc_pct, usable = b.usable_soc_pct;
    const locked = num(soc) && num(usable) && soc - usable > 0.25;
    const limit = band.charge_limit_pct;
    const scale = [0, 20, 40, 60, 80, 100];
    const gauge = h('div', { class: 'bt-gauge', cover: 'bh.soc.band' },
      svg('svg', { class: 'bt-gauge-bar', width: '100%', height: '100%', preserveAspectRatio: 'none', role: 'img',
        'aria-label': `Battery ${pctText(soc)}${locked ? `, usable ${pctText(usable)}` : ''}; everyday band ${pctText(band.low_pct)} to ${pctText(band.high_pct)}${num(limit) ? `; charge limit ${pctText(limit)}` : ''}` },
        svg('rect', { class: 'bt-track', x: 0, y: 0, width: '100%', height: '100%' }),
        num(band.low_pct) && num(band.high_pct) ? svg('rect', { class: 'bt-band', x: p3(band.low_pct), y: 0, width: p3(band.high_pct - band.low_pct), height: '100%' }) : null,
        num(soc) ? svg('rect', { class: ['bt-fill', { 'is-low': soc < 20 }], x: 0, y: '22%', width: p3(clamp(locked ? usable : soc, 0, 100)), height: '56%' }) : null,
        locked ? svg('rect', { class: 'bt-locked', x: p3(clamp(usable, 0, 100)), y: '22%', width: p3(clamp(soc - usable, 0, 100)), height: '56%' }) : null,
        num(limit) ? svg('rect', { class: 'bt-limit', x: p3(clamp(limit, 0, 100) - 0.25), y: 0, width: '0.5%', height: '100%' }) : null),
      h('div', { class: 'bt-scale', 'aria-hidden': 'true' },
        scale.map((v) => h('span', { vars: { '--x': p3(v) } }, `${v} %`)),
        num(limit) ? h('span', { class: 'bt-scale-limit', vars: { '--x': p3(clamp(limit, 0, 100)) } }, `limit ${limit} %`) : null),
      h('ul', { class: 'bt-key' },
        h('li', null, h('span', { class: 'bt-sw bt-sw-fill', 'aria-hidden': 'true' }), 'charge level'),
        locked ? h('li', null, h('span', { class: 'bt-sw bt-sw-locked', 'aria-hidden': 'true' }), 'locked by the cold') : null,
        h('li', null, h('span', { class: 'bt-sw bt-sw-band', 'aria-hidden': 'true' }), `everyday band ${pctText(band.low_pct)} to ${pctText(band.high_pct)}${b.chemistry && b.chemistry.value === 'lfp' ? ' (LFP: 100 % is fine)' : ''}`),
        num(limit) ? h('li', null, h('span', { class: 'bt-sw bt-sw-limit', 'aria-hidden': 'true' }), 'the car’s own charge limit') : null));
    replace(nowBox,
      head('bt-now-title', 'Now', 'the latest reading'),
      h('div', { class: 'bt-now-grid' },
        h('div', { class: 'bt-hero' },
          figure('Battery', q(soc, 'percent', { digits: 1, why: 'no reading yet' }), 'The level the car shows, from the battery management system.', { cls: 'fig-hero', cover: 'bh.soc.current' }),
          figure('Usable', q(usable, 'percent', { digits: 1, why: 'not sent by this car' }), locked ? `Lower than battery % because the pack is cold: ${pctText(soc - usable, 1)} is locked until it warms.` : 'The part the car can use now; the same as battery % while the pack is warm.', { cls: 'fig-hero' }),
          figure('Energy stored', q(b.energy_now_kwh, 'energy', { digits: 1, why: 'no reading yet' }), num(b.capacity && b.capacity.full_pack_kwh) ? ['Of ', q(b.capacity.full_pack_kwh, 'energy', { digits: 1 }), ' when full, including the ', q(b.capacity.buffer_kwh, 'energy', { digits: 1 }), ' buffer the car never uses.'] : 'From the pack’s own energy reading.', { cls: 'fig-hero', cover: 'bh.bar.stored_energy' })),
        gauge),
      (b.notes || []).length ? h('ul', { class: 'bt-notes' }, b.notes.map((n) => h('li', null, icon('info'), NOTE_WORDS[n] || n.replace(/_/g, ' ')))) : null);
  }

  function drawHealth(eff, effErr) {
    const c = bat.capacity || {};
    const usableNew = num(c.new_kwh) && num(c.buffer_kwh) ? c.new_kwh - c.buffer_kwh : null;
    const warranty = num(c.warranty_pct) ? c.warranty_pct : null;
    const health = c.health_pct;
    const healthBar = h('div', { class: 'bt-healthbar', cover: 'bh.bar.health' },
      svg('svg', { class: 'bt-hb', width: '100%', height: '100%', preserveAspectRatio: 'none', role: 'img', 'aria-label': `Health ${pctText(health, 1)} of new${warranty != null ? `; warranty line at ${warranty} %` : ''}` },
        svg('rect', { class: 'bt-track', x: 0, y: 0, width: '100%', height: '100%' }),
        num(health) ? svg('rect', { class: ['bt-hb-fill', { 'is-low': warranty != null && health < warranty + 5 }], x: 0, y: 0, width: p3(clamp(health, 0, 100)), height: '100%' }) : null,
        warranty != null ? svg('rect', { class: 'bt-hb-warranty', x: p3(warranty - 0.2), y: '-30%', width: '0.4%', height: '160%' }) : null),
      h('div', { class: 'bt-scale', 'aria-hidden': 'true' },
        [0, 50, 100].map((v) => h('span', { vars: { '--x': p3(v) } }, `${v} %`)),
        warranty != null ? h('span', { class: 'bt-scale-limit', vars: { '--x': p3(warranty) } }, `warranty ${warranty} %`) : null));
    const factor = eff && eff.factor;
    replace(healthBox,
      head('bt-health-title', 'Capacity and health', 'from the battery management system, every awake minute'),
      h('div', { class: 'bt-health-top' },
        figure('Health', q(health, 'percent', { digits: 1, why: 'no capacity reading yet' }), 'Full-pack capacity now as a share of when new.', { cls: 'fig-hero' }),
        figure('Lost since new', q(c.degradation_pct, 'percent', { digits: 1, why: 'no capacity reading yet' }),
          'Most packs lose a few percent in the first year or two, then much more slowly. A figure under about 10 % is normal wear; the warranty covers a pack that falls under its line.', { cls: 'fig-hero', cover: ['bh.gauge.degradation', 'bh.gauge.degradation_help'] }),
        healthBar),
      figures([
        figure('Full pack now', q(c.full_pack_kwh, 'energy', { digits: 1, why: 'no reading yet' }), num(c.readings) ? `Median of ${formatNumber(c.readings)} readings of the last 30 days, so one odd reading cannot move it.` : null),
        figure('Usable now', q(c.usable_kwh, 'energy', { digits: 1, why: 'no reading yet' }), ['Full pack less the ', q(c.buffer_kwh, 'energy', { digits: 1 }), ' buffer below 0 %.'], { cover: 'bh.capacity.usable_now' }),
        figure('When new', q(c.new_kwh, 'energy', { digits: 1, why: 'not known' }), [num(usableNew) ? ['Usable ', q(usableNew, 'energy', { digits: 1 }), '. '] : null, 'From ', NEW_SOURCE[c.new_source] || 'no source', '.'], { cover: 'bh.capacity.usable_new' }),
        figure('Difference', num(c.difference_kwh) ? q(-c.difference_kwh, 'energy', { digits: 1 }) : q(null, 'energy', { why: 'needs a value when new' }), num(c.difference_kwh) ? (c.difference_kwh >= 0 ? 'Less than when new.' : 'More than the ‘when new’ value: check the value typed in.') : null, { cover: 'bh.capacity.difference' }),
        figure('Energy per rated distance', factor && num(factor.rated_wh_per_km) ? q(factor.rated_wh_per_km, 'efficiency') : q(null, 'efficiency', { why: effErr ? 'could not be loaded' : 'not known yet' }),
          factor && num(factor.rated_wh_per_km) ? `How much energy one ${unitLabel('distance')} of rated range stands for (${factor.source === 'owner' || factor.source === 'override' ? 'typed in' : `learnt from ${formatNumber(factor.readings || 0)} readings of energy and range`}). It turns energy into the full-charge range below; capacity itself does not use it.` : null,
          { cover: 'bh.stat.efficiency' }),
      ]),
      h('dl', { class: 'bt-spec' },
        h('div', null, h('dt', null, 'Chemistry'), h('dd', null, bat.chemistry && bat.chemistry.value ? `${CHEM[bat.chemistry.value] || bat.chemistry.value}, ${CHEM_SOURCE[bat.chemistry.source] || bat.chemistry.source || ''}` : 'not known')),
        h('div', null, h('dt', null, 'Cell balance'), h('dd', null, num(bat.brick_spread_v) ? [q(bat.brick_spread_v * 1000, 'number', { digits: 0 }), ' mV between the highest and lowest cell group', bat.brick_spread_v > 0.05 ? flag('wide', { kind: 'warn' }) : ' (well balanced)'] : 'not sent by this car')),
        h('div', null, h('dt', null, 'Spread of readings'), h('dd', null, num(c.spread_kwh) ? ['±', q(c.spread_kwh / 2, 'energy', { digits: 2 }), ' around the median'] : '—'))),
      h('p', { class: 'note bt-help', cover: 'bh.capacity.help' },
        '‘Now’ is the pack’s own full-pack energy reading, sent by the battery management system while the car is awake: no guessing from range. ‘When new’ is the value typed in under Reference values, else the median of the earliest readings, else the pack type’s nominal size, and the source is always named. Fill in the value when new if the car was bought used or logging began late; leave it empty otherwise.'));
  }

  function drawRange() {
    const r = bat.range || {};
    replace(rangeBox,
      head('bt-range-title', 'Full-charge range', `${rangeWord()}, at 100 %`),
      figures([
        figure('Now', q(r.full_now_km, 'range', { why: 'no reading yet' }), num(r.full_now_from_energy_km) ? ['From battery % and range; ', q(r.full_now_from_energy_km, 'range'), ' from energy stored.'] : 'From battery % and range.', { cover: 'bh.ranges.max_now' }),
        figure('When new', q(r.full_new_km, 'range', { why: 'not known' }), ['From ', RANGE_NEW_SOURCE[r.new_source] || 'no source', '.'], { cover: 'bh.ranges.max_new' }),
        figure('Lost', q(r.lost_km, 'range', { why: 'needs both values' }), num(r.lost_km) && num(r.full_new_km) && r.full_new_km ? `${pctText((r.lost_km / r.full_new_km) * 100, 1)} of when new. Range also drops in the cold; the chart below shows how it moves with temperature.` : null, { cover: 'bh.ranges.lost' }),
      ], { cls: 'bt-figs-3' }));
  }

  function drawRef(err) {
    if (!settings) {
      replace(refBox, head('bt-ref-title', 'Reference values', 'for this car'), errorView(err, { eyebrow: 'Reference values', retry: () => loadTop().catch(() => {}) }));
      return;
    }
    const s = settings;
    const dist = unitLabel('distance');
    const rangeShown = num(s.full_range_new_km) ? Math.round(convert(s.full_range_new_km, 'range')) : null;
    if (!admin) {
      replace(refBox, head('bt-ref-title', 'Reference values', 'for this car'),
        h('dl', { class: 'bt-spec', cover: ['bh.setting.capacity_new_override', 'bh.setting.range_new_override', 'bh.setting.override_help'] },
          h('div', null, h('dt', null, 'Capacity when new'), h('dd', null, num(s.capacity_new_kwh) ? q(s.capacity_new_kwh, 'energy', { digits: 1 }) : 'not typed in')),
          h('div', null, h('dt', null, 'Full-charge range when new'), h('dd', null, num(s.full_range_new_km) ? q(s.full_range_new_km, 'range') : 'not typed in')),
          h('div', null, h('dt', null, 'Warranty line'), h('dd', null, pctText(s.warranty_threshold_pct)))),
        h('p', { class: 'note' }, 'Only an admin can change these. Empty values are worked out from the car’s own early readings.'));
      return;
    }
    const cap = field({ label: 'Capacity when new (kWh, full pack)', name: 'cap', type: 'number', value: s.capacity_new_kwh, attrs: { min: '10', max: '250', step: '0.1', inputmode: 'decimal', placeholder: 'from early readings' },
      hint: 'Only if the car was bought used or logging began late. Leave empty to use the median of the earliest readings. 10 to 250 kWh.', cover: 'bh.setting.capacity_new_override' });
    const rng = field({ label: `Full-charge range when new (${dist}, ${rangeWord()})`, name: 'range', type: 'number', value: rangeShown, attrs: { min: String(Math.ceil(convert(100, 'range'))), max: String(Math.floor(convert(1500, 'range'))), step: '1', inputmode: 'numeric', placeholder: 'from the first 30 days' },
      hint: `The range the car showed at 100 % when new. Leave empty to use the first 30 days. ${Math.ceil(convert(100, 'range'))} to ${Math.floor(convert(1500, 'range'))} ${dist}.`, cover: 'bh.setting.range_new_override' });
    const war = field({ label: 'Warranty line (% of new)', name: 'warranty', type: 'number', value: s.warranty_threshold_pct, attrs: { min: '50', max: '95', step: '1', inputmode: 'numeric', required: true },
      hint: 'The capacity the battery warranty promises, usually 70 %. 50 to 95 %.' });
    const check = (f, v, lo, hi, text) => { if (v != null && !(v >= lo && v <= hi)) { setFieldError(f, text); return false; } return true; };
    replace(refBox, head('bt-ref-title', 'Reference values', 'for this car only'),
      h('p', { class: 'note bt-ref-help', cover: 'bh.setting.override_help' }, 'Typed once for this car and checked as you save. Fill a box only when you know the real value when new; an empty box is worked out from the car’s own early readings, and the page always says which was used.'),
      form({
        fields: [cap, rng, war], submitLabel: 'Save the reference values', cls: 'bt-ref-form',
        onsubmit: async (v) => {
          const lo = Math.ceil(convert(100, 'range')), hi = Math.floor(convert(1500, 'range'));
          const ok = [check(cap, v.cap, 10, 250, 'Between 10 and 250 kWh, or empty.'), check(rng, v.range, lo, hi, `Between ${lo} and ${hi} ${dist}, or empty.`),
            v.warranty == null ? (setFieldError(war, 'A number from 50 to 95.'), false) : check(war, v.warranty, 50, 95, 'Between 50 and 95 %.')].every(Boolean);
          if (!ok) throw Object.assign(new Error('Check the values marked below.'), { fields: {} });
          const body = { version: settings.version, capacity_new_kwh: v.cap, full_range_new_km: v.range == null ? null : Math.round(toMetric(v.range, 'range') * 10) / 10, warranty_threshold_pct: v.warranty };
          let saved;
          try { saved = await ctx.api.patch(api.apiPath`cars/${car.id}/settings`, body); } catch (e) {
            if (e && e.fields) { const map = { capacity_new_kwh: 'cap', full_range_new_km: 'range', warranty_threshold_pct: 'warranty' }; e.fields = Object.fromEntries(Object.entries(e.fields).map(([k, m]) => [map[k] || k, m])); }
            throw e;
          }
          settings = saved;
          ctx.flash('Reference values saved. The health figures are worked out again.');
          await Promise.all([loadTop(), drawCap()]);
        },
      }));
  }

  /* ---------------------------------------------------------------- capacity against distance or time */

  let capGen = 0;
  async function drawCap() {
    const my = ++capGen;
    const groupChoice = choice({ label: 'Readings', name: 'bt-cap-group', value: opt.group, options: GROUPS, onchange: (v) => { opt.group = v; remember(); drawCap().catch(() => {}); } });
    const axisChoice = choice({ label: 'Across', name: 'bt-cap-x', value: opt.axis, options: AXES, onchange: (v) => { opt.axis = v; remember(); drawCap().catch(() => {}); } });
    const plot = h('div', { class: 'bt-cap-plot' }, loadingBlock(5));
    const read = h('p', { class: 'readout bt-readout', 'aria-live': 'polite' });
    const proj = h('p', { class: 'note bt-proj-text' });
    let items = [], trend = null;
    const csv = () => downloadCsv(csvFilename(car.name, 'battery capacity', opt.group), items, [
      timeColumn('Time', 't_ms', tz), column('Odometer', 'odometer_km', 'odometer', { digits: 1 }), column('Full pack', 'full_pack_kwh', 'energy', { digits: 2 }),
      column('Usable', 'usable_kwh', 'energy', { digits: 2 }), { label: 'Method', value: (r) => r.method }, { label: 'Readings', value: (r) => r.readings ?? '' },
      { label: 'Charge', value: (r) => r.charge || '' }, { label: 'Source', value: (r) => r.source }]);
    replace(capBox,
      head('bt-cap-title', 'Capacity over the car’s life', 'whole history, not the period', csvButton('capacity readings', csv)),
      h('div', { class: 'bt-controls' }, groupChoice, axisChoice),
      h('ul', { class: 'bt-key' },
        h('li', null, h('span', { class: 'bt-sw bt-sw-dot', 'aria-hidden': 'true' }), opt.group === 'charge' ? 'one reading per charge that added at least 10 % (click to open it)' : opt.group === 'day' ? 'the pack’s own reading, one per awake day' : 'median of the pack’s daily readings, per month'),
        h('li', null, h('span', { class: 'bt-sw bt-sw-median', 'aria-hidden': 'true' }), 'running median of nine readings'),
        h('li', null, h('span', { class: 'bt-sw bt-sw-trend', 'aria-hidden': 'true' }), 'straight-line trend'),
        h('li', null, h('span', { class: 'bt-sw bt-sw-new', 'aria-hidden': 'true' }), 'full pack when new')),
      read, plot, proj);
    try {
      const pager = ctx.api.list(api.apiPath`cars/${car.id}/battery/capacity`, { group: opt.group, x: opt.axis }, { limit: 500 });
      let first = null;
      for await (const pg of pager.pages()) { if (!first) first = pg; items.push(...(pg.items || [])); if (items.length >= 5000) break; }
      trend = first && first.trend;
    } catch (err) {
      if (!ctx.signal.aborted && my === capGen) replace(plot, errorView(err, { eyebrow: 'Capacity', retry: () => drawCap() }));
      return;
    }
    if (my !== capGen || !ctx.alive()) return;
    items = items.filter((r) => num(r.full_pack_kwh));
    const c = (bat && bat.capacity) || {};
    const xOf = (r) => (opt.axis === 'odometer' ? r.odometer_km : r.t_ms);
    const pts = items.filter((r) => num(xOf(r))).sort((a, b) => xOf(a) - xOf(b));
    const idle = () => replace(read, h('span', { class: 'readout-idle' }, pts.length ? `${tn(pts.length, '{n} reading', '{n} readings')}, ${formatDate(pts[0].t_ms, tz, { weekday: false })} to ${formatDate(pts[pts.length - 1].t_ms, tz, { weekday: false })}. Point at a dot for its value.` : ''));
    idle();
    if (!pts.length) {
      replace(plot, empty({ title: 'No capacity readings yet.', text: opt.group === 'charge' ? 'A reading needs a finished charge that added at least 10 %. Choose Each day for the pack’s own daily readings.' : 'The pack sends its capacity while the car is awake; imported days have none.' }));
      return;
    }
    const ys = pts.map((r) => r.full_pack_kwh);
    const yMin0 = Math.min(...ys), yMax0 = Math.max(...ys, num(c.new_kwh) ? c.new_kwh : -Infinity);
    const pad = Math.max(0.5, (yMax0 - yMin0) * 0.12);
    const yTicks = niceSplits(yMin0 - pad, yMax0 + pad, 4);
    const y0 = Math.min(yTicks[0], yMin0 - pad), y1 = Math.max(yTicks[yTicks.length - 1], yMax0 + pad);
    const x0 = xOf(pts[0]), x1 = Math.max(xOf(pts[pts.length - 1]), x0 + 1);
    const X = (v) => clamp(((v - x0) / (x1 - x0)) * 100, 0, 100);
    const Y = (v) => clamp(100 - ((v - y0) / (y1 - y0)) * 100, 0, 100);
    // running median of nine, in x order
    const med = pts.map((_, i) => { const w = ys.slice(Math.max(0, i - 4), i + 5).sort((a, b) => a - b); return w[Math.floor(w.length / 2)]; });
    // straight-line trend: least squares on the points as drawn
    const n = pts.length, mx = pts.reduce((s, r) => s + xOf(r), 0) / n, my2 = ys.reduce((s, v) => s + v, 0) / n;
    const sxx = pts.reduce((s, r) => s + (xOf(r) - mx) ** 2, 0), sxy = pts.reduce((s, r, i) => s + (xOf(r) - mx) * (ys[i] - my2), 0);
    const slope = sxx > 0 ? sxy / sxx : 0;
    const line = (a, b, cls) => svg('line', { class: cls, x1: p3(X(a[0])), y1: p3(Y(a[1])), x2: p3(X(b[0])), y2: p3(Y(b[1])) });
    const hoverG = svg('g');
    const graph = svg('svg', { class: 'bt-cap-svg', width: '100%', height: '100%', role: 'img', 'aria-label': `Full-pack capacity ${opt.axis === 'odometer' ? 'against odometer' : 'over time'}: ${tn(n, '{n} reading', '{n} readings')} from ${format(ys[0], 'energy')} to ${format(ys[n - 1], 'energy')}` },
      yTicks.map((v) => svg('line', { class: 'bt-gline', x1: 0, x2: '100%', y1: p3(Y(v)), y2: p3(Y(v)) })),
      num(c.new_kwh) ? line([x0, c.new_kwh], [x1, c.new_kwh], 'bt-new-line') : null,
      svg('g', { class: 'bt-dots' }, pts.map((r, i) => svg('circle', { class: ['bt-dot', { 'is-link': !!r.charge }], cx: p3(X(xOf(r))), cy: p3(Y(r.full_pack_kwh)), dataset: { i } }))),
      n > 2 ? med.slice(1).map((v, i) => line([xOf(pts[i]), med[i]], [xOf(pts[i + 1]), v], 'bt-median')) : null,
      n > 1 ? line([x0, my2 + slope * (x0 - mx)], [x1, my2 + slope * (x1 - mx)], 'bt-trend') : null,
      hoverG);
    const area = h('div', { class: 'bt-cap-area', cover: ['bh.chart.capacity_by_distance.points', 'bh.chart.capacity_by_distance.median'] }, graph);
    let on = null;
    area.addEventListener('mouseover', (e) => {
      const d = e.target.closest('circle[data-i]');
      if (!d) return;
      on = pts[+d.dataset.i];
      replace(hoverG, svg('circle', { class: 'bt-dot is-on', cx: p3(X(xOf(on))), cy: p3(Y(on.full_pack_kwh)) }));
      replace(read, h('span', { class: 'readout-time' }, formatDate(on.t_ms, tz, { weekday: false })), ' ',
        h('span', null, 'full pack ', h('b', null, format(on.full_pack_kwh, 'energy', { digits: 2 }))), ' ', h('span', null, 'usable ', h('b', null, format(on.usable_kwh, 'energy', { digits: 2 }))),
        ' ', h('span', null, 'at ', h('b', null, format(on.odometer_km, 'odometer'))), on.readings ? ` (${tn(on.readings, '{n} day', '{n} days')})` : '', on.charge ? ' · click to open the charge' : '');
    });
    area.addEventListener('mouseleave', () => { on = null; replace(hoverG); idle(); });
    area.addEventListener('click', () => { if (on && on.charge) ctx.go(state.buildUrl('charge', { car: car.id, charge: on.charge, period: ctx.state.period })); });
    const xTicks = opt.axis === 'odometer'
      ? niceSplits(convert(x0, 'odometer'), convert(x1, 'odometer'), 5).map((v) => [toMetric(v, 'odometer'), `${formatNumber(v)} ${unitLabel('odometer')}`])
      : timeTicks(x0, x1, tz);
    replace(plot,
      h('div', { class: 'bt-cap-y', 'aria-hidden': 'true' }, yTicks.filter((v) => v >= y0 && v <= y1).map((v) => h('span', { vars: { '--y': p3(Y(v)) } }, format(v, 'energy', { digits: 0 })))),
      area,
      h('div', { class: 'bt-cap-x', 'aria-hidden': 'true' }, xTicks.filter(([v]) => v >= x0 && v <= x1).map(([v, t]) => h('span', { vars: { '--x': p3(X(v)) } }, t))));
    const pr = trend && trend.projection;
    const per10k = trend && num(trend.slope_kwh_per_10000_km) ? trend.slope_kwh_per_10000_km : null;
    replace(proj,
      per10k != null ? ['Trend: ', q(per10k, 'energy', { digits: 2 }), ` per ${formatNumber(convert(10000, 'distance'), { maximumFractionDigits: 0 })} ${unitLabel('distance')} driven. `] : 'Not enough readings for a trend yet. ',
      pr ? ['At this rate the pack reaches its warranty line (', pctText(pr.warranty_pct), ', ', q(pr.full_pack_kwh_at_warranty, 'energy', { digits: 1 }), ') at about ', q(pr.odometer_km, 'odometer'), num(pr.t_ms) ? `, around ${dateFormat(tz, { year: 'numeric' }).format(pr.t_ms)}` : '', '. A straight line is a rough guide: packs lose most in their first years.']
        : per10k != null ? 'The pack is not losing capacity at a rate that reaches the warranty line.' : null);
  }

  /* ---------------------------------------------------------------- lifetime totals */

  async function drawLife() {
    const [t, g] = await Promise.allSettled([
      ctx.api.get(api.apiPath`cars/${car.id}/totals`),
      ctx.api.list(api.apiPath`cars/${car.id}/gaps`, {}, { limit: 200 }).all({ max: 2000 }),
    ]);
    if (!ctx.alive()) return;
    const L = (bat && bat.lifetime) || {};
    if (t.status === 'rejected') { replace(lifeBox, head('bt-life-title', 'Lifetime totals'), errorView(t.reason, { eyebrow: 'Lifetime totals', retry: () => drawLife() })); return; }
    const T = t.value;
    const since = num(T.odometer_last_km) && num(T.odometer_first_km) ? T.odometer_last_km - T.odometer_first_km : null;
    const jumps = g.status === 'fulfilled' ? g.value.filter((x) => num(x.odometer_jump_km) && x.odometer_jump_km > 0.05) : [];
    const gapList = g.status === 'rejected' ? h('p', { class: 'note' }, 'The gap list could not be loaded.')
      : jumps.length ? h('ol', { class: 'bt-gaps' }, jumps.slice(0, 6).map((x) => h('li', null,
        h('a', { href: state.buildUrl('timeline', { car: car.id, period: { from: x.start_ms - HOUR, to: (x.end_ms ?? api.now()) + HOUR } }) }, formatDateTime(x.start_ms, tz, { now: api.now() })),
        ': ', q(x.odometer_jump_km, 'distance'), ` from ${placeOf(x.from_place)} to ${placeOf(x.to_place)}`)), jumps.length > 6 ? h('li', null, `and ${tn(jumps.length - 6, '{n} more', '{n} more')}`) : null)
        : h('p', { class: 'note' }, 'Every distance the odometer counted was logged as a drive.');
    const ac = L.ac_kwh, dc = L.dc_kwh, regen = L.regen_kwh;
    const parts = [['ac', 'AC charging', ac, 'home and destination chargers'], ['dc', 'DC fast charging', dc, 'Superchargers and other fast chargers'], ['regen', 'Regenerative braking', regen, 'won back while slowing down']];
    const total = parts.reduce((s, p) => s + (num(p[2]) ? p[2] : 0), 0);
    let at = 0;
    const splitCsv = () => downloadCsv(csvFilename(car.name, 'battery energy in'), parts.map(([k, name, v]) => ({ k, name, v, share: total ? (v / total) * 100 : null })),
      [{ label: 'Kind', value: (r) => r.name }, column('Energy', 'v', 'energy', { digits: 1 }), column('Share', 'share', 'percent', { digits: 1 })]);
    replace(lifeBox,
      head('bt-life-title', 'Lifetime totals', 'whole history'),
      h('div', { class: 'bt-life-grid' },
        h('div', { class: 'bt-life-col' },
          h('h3', { class: 'bt-sub' }, 'Driving', pageLink2('Driving statistics', 'statistics', 'bh.drive.link_stats')),
          figures([
            figure('Logged drives', q(T.distance_km, 'distance', { digits: 0 }), `Sum of ${tn(T.drives || 0, '{n} recorded drive', '{n} recorded drives')}.`, { cover: 'bh.drive.logged' }),
            figure('Driven since logging began', q(since, 'distance', { digits: 0, why: 'no odometer yet' }), num(since) ? ['By the odometer, from ', q(T.odometer_first_km, 'odometer'), '.'] : null, { cover: 'bh.drive.mileage' }),
            figure('Odometer', q(T.odometer_last_km, 'odometer', { why: 'no reading yet' }), T.odometer_last_ms ? ['As of ', ago(T.odometer_last_ms)] : null, { cover: 'bh.drive.odometer' }),
            figure('Not logged', q(T.not_logged_km, 'distance', { digits: 1 }), 'Distance the odometer counted outside any recorded drive.', { cover: 'bh.drive.data_lost' }),
          ], { cls: 'bt-figs-2' }),
          h('div', { class: 'bt-gapbox' }, h('p', { class: 'bt-gap-head' }, 'Where the distance went unlogged', h('a', { class: 'link-arrow', href: state.buildUrl('health', { car: car.id, period: ctx.state.period }) }, 'Data health', icon('arrowRight'))), gapList),
          h('p', { class: 'note', cover: 'bh.drive.help' }, 'Logged drives add up what was recorded; the odometer counts every distance. A small difference is normal (a car moved while the logger restarted). A large one means drives were missed: open a gap above to see when, and Data health for why.')),
        h('div', { class: 'bt-life-col' },
          h('h3', { class: 'bt-sub' }, 'Charging', pageLink2('Charges', 'charges', 'bh.charging.link_stats')),
          figures([
            figure('Charging sessions', q(T.charges, 'count'), T.ac && T.dc ? `${formatNumber(T.ac.charges)} AC, ${formatNumber(T.dc.charges)} DC.` : null, { cover: 'bh.charging.count' }),
            figure('Equivalent full cycles', q(L.equivalent_cycles, 'number', { digits: 1, why: 'no counters yet' }), 'Energy the pack has given over its life, divided by its usable capacity when new.', { cover: 'bh.charging.cycles' }),
            figure('Energy added', q(T.energy_added_kwh, 'energy', { digits: 0 }), num(L.charged_kwh) ? ['Into the pack in logged charges; the pack’s own counters say ', q(L.charged_kwh, 'energy', { digits: 0 }), ' over its whole life.'] : 'Into the pack, in logged charges.', { cover: 'bh.charging.energy_added' }),
            figure('Drawn from chargers', q(T.energy_grid_kwh, 'energy', { digits: 0 }), num(T.grid_coverage_pct) ? `Metered for ${pctText(T.grid_coverage_pct)} of the energy added: AC only, a fast charger’s side is not measured.` : null, { cover: 'bh.charging.energy_used' }),
            figure('Charging efficiency', q(T.charging_efficiency_pct, 'percent', { digits: 1, why: 'needs metered sessions' }), 'Energy added ÷ energy drawn, for sessions where both were measured.', { cover: 'bh.charging.efficiency' }),
          ], { cls: 'bt-figs-2' }),
          h('p', { class: 'note', cover: 'bh.charging.help' }, 'A full cycle is one pack’s worth of energy in and out, however it was split: two charges from 50 % to 100 % make one. Charging efficiency is what reached the pack out of what the charger gave; the rest warmed the charger, cables and battery.'))),
      h('div', { class: 'bt-split', cover: 'bh.chart.ac_dc' },
        h('div', { class: 'bt-split-head' }, h('p', { class: 'bt-sub' }, 'Energy into the pack, by source', h('span', { class: 'label-note' }, `from the pack’s own lifetime counters (${L.basis || 'pack counters'}), including years before logging`)), csvButton('energy by source', splitCsv)),
        total ? svg('svg', { class: 'bt-split-bar', width: '100%', height: '100%', preserveAspectRatio: 'none', role: 'img', 'aria-label': parts.map(([, name, v]) => `${name} ${format(v, 'energy')}`).join(', ') },
          parts.map(([k, , v]) => { const w = num(v) ? (v / total) * 100 : 0; const r = svg('rect', { class: `bt-in-${k}`, x: p3(at), y: 0, width: p3(w), height: '100%' }); at += w; return r; })) : empty({ title: 'No lifetime counters from this car yet.', text: 'Older cars and imported history do not send them.' }),
        total ? h('ul', { class: 'bt-split-key' }, parts.map(([k, name, v, why]) => h('li', null, h('span', { class: `bt-sw bt-sw-${k}`, 'aria-hidden': 'true' }), h('span', { class: 'bt-split-name' }, name), ' ', q(v, 'energy', { digits: 0 }), h('span', { class: 'bt-split-pct' }, ` ${pctText(total ? (v / total) * 100 : null)}`), h('span', { class: 'bt-split-why' }, why)))) : null,
        num(L.drive_kwh) ? h('p', { class: 'note' }, 'Out of the pack: ', q(L.drive_kwh, 'energy', { digits: 0 }), ' driving and ', q(L.standing_kwh, 'energy', { digits: 0 }), ' while parked (', q(L.discharged_kwh, 'energy', { digits: 0 }), ' in all). AC and DC add up to the ', q(L.charged_kwh, 'energy', { digits: 0 }), ' charged.') : null));
  }
  const placeOf = (p) => (p && p.name) || 'an unnamed spot';
  const pageLink2 = (text, page, cover) => h('a', { class: 'link-arrow', href: state.buildUrl(page, { car: car.id, period: ctx.state.period }), cover }, text, icon('arrowRight'));

  /* ---------------------------------------------------------------- charts with a legend, readout and CSV */

  function timeBlock(box, { titleId, title, note, cover, controls, csvName, keyCover }) {
    const legend = h('ul', { class: 'bt-legend', cover: keyCover });
    const readout = h('p', { class: 'readout bt-readout', 'aria-live': 'off' });
    const charts = h('div', { class: 'charts' });
    let data = null;
    const csv = h('button', { type: 'button', class: 'button button-quiet button-small', disabled: true, 'aria-label': `Download CSV: ${title}`, onclick: () => data && downloadCsv(file(csvName), data.t_ms.map((t, i) => ({ t_ms: t, i })), [
      timeColumn('Time', 't_ms', tz),
      ...data.lines.map((l) => column(l.name, (row) => l.values[row.i], l.kind, { digits: 3 })),
      ...(data.extraCsv || []).map((c) => ({ label: c.label, value: (row) => c.value(row.i) })),
    ]) }, icon('download'), 'Download CSV');
    replace(box, head(titleId, title, note, csv), controls || null, legend, readout, h('div', { cover }, charts));
    const idle = () => data && replace(readout, h('span', { class: 'readout-idle' }, data.idle || `${tn(data.t_ms.length, '{n} point', '{n} points')}, one per ${formatDuration(data.bucket_ms || HOUR)}. Point at the chart for every value at that moment; drag to zoom, double-click to zoom out.`));
    return {
      fill(node) { replace(legend); replace(readout); replace(charts, node); },
      async draw(d) {
        data = d;
        csv.disabled = !d.t_ms.length;
        replace(legend, d.lines.filter((l) => !l.noLegend).map((l) => {
          const v = l.values.filter(num);
          const avg = v.length ? v.reduce((s, x) => s + x, 0) / v.length : null;
          return h('li', { vars: { '--swatch': `var(${l.colour})` }, class: { 'is-dash': l.dash } },
            h('span', { class: 'bt-legend-sw', 'aria-hidden': 'true' }), h('span', { class: 'bt-legend-name' }, l.name),
            h('span', { class: 'bt-legend-stats' }, v.length ? ['avg ', q(avg, l.kind, { digits: l.digits }), ' · low ', q(Math.min(...v), l.kind, { digits: l.digits }), ' · high ', q(Math.max(...v), l.kind, { digits: l.digits })] : 'no values'));
        }));
        idle();
        replace(charts);
        if (!d.t_ms.length) return;
        await stackedCharts(ctx, charts, {
          tz, t_ms: d.t_ms, states: d.states, spans: d.spans, rows: d.rows,
          onHover: (i) => {
            if (i == null) { idle(); return; }
            replace(readout, h('time', { class: 'readout-time' }, formatDateTime(d.t_ms[i], tz, { now: api.now() })),
              d.states && d.states[i] ? h('span', null, d.states[i] === 'asleep' ? 'asleep' : d.states[i] === 'unknown' ? 'no data' : d.states[i]) : null,
              d.lines.filter((l) => !l.noReadout).map((l) => h('span', null, `${l.short || l.name} `, h('b', null, l.values[i] == null ? '—' : format(l.values[i], l.kind, { digits: l.digits })))));
          },
        });
      },
    };
  }

  /* ---------------------------------------------------------------- projected range */

  async function drawProj() {
    const resChoice = choice({ label: 'Resolution', name: 'bt-res', value: opt.res, options: RESOLUTIONS, onchange: (v) => { opt.res = v; remember(); drawProj().catch(() => {}); } });
    const block = timeBlock(projBox, {
      titleId: 'bt-proj-title', title: 'Projected range', note: `${rangeWord()} at a full charge, next to what moves it`, csvName: 'projected range',
      cover: ['pr.control.shared_crosshair', 'pr.axis.titles', 'pr.temp.projected', 'pr.section.per_car'],
      controls: h('div', { class: 'bt-controls' },
        periodNote('pr.filter.time_range', 'one line per chart, all on one time axis'),
        h('div', { cover: 'pr.control.resolution' }, resChoice, h('p', { class: 'field-hint' }, 'How long each point averages. Auto fits about a thousand points in the period.'))),
    });
    block.fill(loadingBlock(6));
    let d;
    try { d = await ctx.api.get(api.apiPath`cars/${car.id}/projected-range`, { ...ctx.params, bucket: opt.res || undefined, max_points: 1000 }); } catch (err) {
      if (!ctx.signal.aborted) block.fill(errorView(err, { eyebrow: 'Projected range', retry: () => drawProj() }));
      return;
    }
    if (!ctx.alive()) return;
    const v = d.values || {};
    if (!(d.t_ms || []).length) { block.fill(empty({ title: 'No range readings in this period.', text: 'Choose a longer period with the period picker at the top.' })); return; }
    const states = d.state || null;
    const lines = [
      { name: 'Full range from battery %', short: 'from battery %', kind: 'range', values: v.full_range_km || [], colour: '--accent' },
      { name: 'Full range from energy', short: 'from energy', kind: 'range', values: v.full_range_from_energy_km || [], colour: '--ink-3', dash: true },
      { name: 'Full range, usable part only', short: 'usable only', kind: 'range', values: v.full_range_usable_km || [], colour: '--dusk' },
      { name: 'Odometer', kind: 'odometer', values: v.odometer_km || [], colour: '--ink-2' },
      { name: 'Battery %', short: 'battery', kind: 'percent', values: v.soc_pct || [], colour: '--accent', digits: 1 },
      { name: 'Battery % (pack’s raw reading)', short: 'raw', kind: 'percent', values: v.soc_bms_pct || [], colour: '--slate', dash: true, digits: 1 },
      { name: 'Outside', kind: 'temperature', values: v.outside_c || [], colour: '--slate' },
      { name: 'Battery temperature', short: 'battery temp.', kind: 'temperature', values: v.battery_c || [], colour: '--ochre' },
    ];
    lines.forEach((l, i) => { l.idx = i; });
    await block.draw({
      t_ms: d.t_ms, bucket_ms: d.bucket_ms, states, spans: spansFromStates(d.t_ms, states, d.bucket_ms), lines,
      extraCsv: [{ label: 'State', value: (i) => (states ? states[i] : '') }, { label: 'Coverage (%)', value: (i) => (d.coverage_pct ? d.coverage_pct[i] : '') }],
      rows: [
        { key: 'prange', name: 'Full-charge range', kind: 'range', values: lines[0].values, colour: '--accent', note: 'solid: from battery %; dashed grey: from energy; violet: usable part only (lower when cold)', ticks: 4,
          extra: [{ values: lines[1].values, colour: '--ink-3', dash: true }, { values: lines[2].values, colour: '--dusk' }] },
        { key: 'podo', name: 'Odometer', kind: 'odometer', values: lines[3].values, colour: '--ink-2', note: 'read range change against distance driven', ticks: 3 },
        { key: 'plevel', name: 'Charge level', kind: 'percent', values: lines[4].values, colour: '--accent', fill: '--chart-wash', floor: 0, ticks: 4, note: 'average per point; dashed: the pack’s raw reading',
          extra: [{ values: lines[5].values, colour: '--slate', dash: true }] },
        { key: 'ptemp', name: 'Temperatures', kind: 'temperature', values: lines[6].values, colour: '--slate', note: 'blue: outside; ochre: battery', ticks: 4,
          extra: [{ values: lines[7].values, colour: '--ochre' }] },
      ],
    });
    // the covers of the separate lines sit on their legend entries
    const li = projBox.querySelectorAll('.bt-legend li');
    const coverOf = ['pr.mileage.projected', 'pr.battery.projected_total', 'pr.battery.projected_usable', 'pr.mileage.odometer', null, null, 'pr.temp.outdoor', null];
    li.forEach((el, i) => { if (coverOf[i]) cover(el, coverOf[i]); });
    cover(projBox.querySelector('.chart-plevel'), 'pr.battery.levels');
  }

  /* ---------------------------------------------------------------- charge level history */

  async function drawLevel() {
    const bucketChoice = choice({ label: 'Group readings by', name: 'bt-bucket', value: opt.bucket, options: BUCKETS, onchange: (v) => { opt.bucket = v; remember(); drawLevel().catch(() => {}); } });
    const winChoice = choice({ label: 'Moving window', name: 'bt-window', value: opt.window, options: WINDOWS, onchange: (v) => { opt.window = v; remember(); drawLevel().catch(() => {}); } });
    const movingSw = switchField({ label: 'Moving lines', checked: opt.moving, onchange: (v) => { opt.moving = v; remember(); drawLevel().catch(() => {}); } });
    const shares = h('div', { class: 'bt-shares' });
    const block = timeBlock(levelBox, {
      titleId: 'bt-level-title', title: 'Charge level history', note: 'battery % and usable %, with asleep and no-data time shaded apart', csvName: 'charge level',
      cover: ['cl.chart.charge_level', 'cl.chart.battery_level', 'cl.chart.usable_level', 'cl.chart.band', 'cl.filter.car'], keyCover: 'cl.chart.legend_stats',
      controls: h('div', null,
        h('div', { class: 'bt-controls' },
          periodNote('cl.filter.time_range', 'the moving lines look back a fixed window, whatever the period'),
          h('div', { cover: 'cl.control.bucket_width' }, bucketChoice, h('p', { class: 'field-hint' }, 'Auto: hours up to 31 days, else days.')),
          h('div', { cover: 'cl.control.include_moving' }, movingSw),
          h('div', { cover: 'cl.control.window_width' }, winChoice, h('p', { class: 'field-hint' }, 'Each point of a moving line covers the days before it.'))),
        shares),
    });
    block.fill(loadingBlock(5));
    let d;
    try { d = await ctx.api.get(api.apiPath`cars/${car.id}/battery-level`, { ...ctx.params, bucket: opt.bucket || undefined, moving: opt.moving ? 'true' : 'false', window: opt.window }); } catch (err) {
      if (!ctx.signal.aborted) block.fill(errorView(err, { eyebrow: 'Charge level', retry: () => drawLevel() }));
      return;
    }
    if (!ctx.alive()) return;
    const bk = d.buckets || [];
    if (!bk.length) { block.fill(empty({ title: 'No charge level readings in this period.', text: 'Choose a longer period with the period picker at the top.' })); return; }
    const t = bk.map((b) => b.t_ms);
    const spans = d.spans || [];
    const states = t.map((ms) => { const mid = ms + (d.bucket_ms || HOUR) / 2; const s = spans.find((x) => x.start_ms <= mid && (x.end_ms ?? Infinity) > mid); return s ? s.state : null; });
    const band = d.band || {};
    const limits = (d.charge_limit || []).slice().sort((a, b) => a.t_ms - b.t_ms);
    const limitAt = (ms) => { let v = null; for (const l of limits) { if (l.t_ms <= ms + (d.bucket_ms || HOUR)) v = l.pct; else break; } return v; };
    const mv = d.moving || {};
    const has = (a) => Array.isArray(a) && a.length === t.length && a.some(num);
    const winText = opt.window === '30d' ? '30-day' : '7-day';
    const lines = [
      { name: 'Battery %', short: 'battery', kind: 'percent', values: bk.map((b) => b.soc_pct), colour: '--accent', digits: 1, cover: 'cl.chart.battery_level' },
      { name: 'Usable %', short: 'usable', kind: 'percent', values: bk.map((b) => b.usable_soc_pct), colour: '--dusk', digits: 1, cover: 'cl.chart.usable_level' },
      has(mv.mean) && { name: `${winText} average`, short: 'average', kind: 'percent', values: mv.mean, colour: '--ink', digits: 1, cover: 'cl.chart.moving_avg' },
      has(mv.median) && { name: `${winText} median`, short: 'median', kind: 'percent', values: mv.median, colour: '--slate', dash: true, digits: 1, cover: 'cl.chart.moving_median' },
      has(mv.p7_5) && { name: 'Low line (above it 92.5 % of the time)', short: 'low line', kind: 'percent', values: mv.p7_5, colour: '--rust', dash: true, digits: 1, cover: 'cl.chart.moving_p075' },
      has(mv.p92_5) && { name: 'High line (below it 92.5 % of the time)', short: 'high line', kind: 'percent', values: mv.p92_5, colour: '--ochre', dash: true, digits: 1, cover: 'cl.chart.moving_p925' },
      limits.length && { name: 'Charge limit', short: 'limit', kind: 'percent', values: t.map(limitAt), colour: '--ink-3', digits: 0 },
    ].filter(Boolean);
    const bandLines = num(band.low_pct) && num(band.high_pct) ? [
      { name: 'Band low', kind: 'percent', values: t.map(() => band.low_pct), colour: '--accent-strong', noLegend: true, noReadout: true },
      { name: 'Band high', kind: 'percent', values: t.map(() => band.high_pct), colour: '--accent-strong', noLegend: true, noReadout: true },
    ] : [];
    const st = d.stats || {};
    replace(shares,
      figures([
        figure('Above 80 %', q(d.share_above_80_pct, 'percent', { digits: 1, why: 'no readings' }), 'Share of the recorded time. Staying high for long wears an NCA/NMC pack faster.', { cover: 'cl.chart.legend_stats' }),
        figure('Below 20 %', q(d.share_below_20_pct, 'percent', { digits: 1, why: 'no readings' }), 'Share of the recorded time. Running low often also wears a pack.'),
        figure('Typical level', q(st.median, 'percent', { digits: 1 }), num(st.min) ? ['Median; ', q(st.min, 'percent'), ' to ', q(st.max, 'percent'), ' in the period.'] : null),
        figure('Recommended band', num(band.low_pct) ? `${pctText(band.low_pct)} to ${pctText(band.high_pct)}` : '—', band.high_pct === 100 ? 'LFP pack: charging to 100 % is fine and keeps its reading accurate.' : 'For everyday driving; charge higher only before a long trip.', { cover: 'cl.chart.band' }),
      ], { cls: 'bt-figs-4' }));
    await block.draw({
      t_ms: t, bucket_ms: d.bucket_ms, states, spans, lines: [...lines, ...bandLines],
      idle: `${tn(t.length, '{n} point', '{n} points')}, one per ${d.bucket === 'day' ? 'day' : 'hour'}${opt.moving ? `; moving lines over the ${winText} window` : '; moving lines off'}. Shading: violet asleep, red no data. Point at the chart for the values.`,
      extraCsv: [{ label: 'State', value: (i) => states[i] || '' }],
      rows: [{ key: 'level', name: 'Charge level', kind: 'percent', values: lines[0].values, colour: '--accent', fill: '--chart-wash', floor: 0, ticks: 5,
        note: `green: battery %; violet: usable; ${opt.moving ? 'black, grey dashed, red and ochre dashed: the moving lines; ' : ''}green dashes: the ${pctText(band.low_pct)} to ${pctText(band.high_pct)} band`,
        extra: [...lines.slice(1), ...bandLines].map((l) => ({ values: l.values, colour: l.colour, dash: !!l.dash || bandLines.includes(l) })) }],
    });
    const li = levelBox.querySelectorAll('.bt-legend li');
    lines.forEach((l, i) => { if (l.cover && li[i]) cover(li[i], l.cover); });
  }

  /* ---------------------------------------------------------------- parked drain */

  const minIn = h('input', { class: 'field-input bt-num', type: 'number', id: 'bt-min-hours', min: '0', step: '0.5', inputmode: 'decimal', value: String(opt.min_hours) });
  const drainSum = h('div', { class: 'bt-drain-sum' });
  let minTimer = 0;
  ctx.onCleanup(() => clearTimeout(minTimer));
  minIn.addEventListener('input', () => {
    clearTimeout(minTimer);
    minTimer = setTimeout(() => {
      const n = parseFloat(minIn.value);
      opt.min_hours = Number.isFinite(n) && n >= 0 ? n : 0;
      remember();
      replace(drainSum);
      drain.reload({ ...ctx.params, min_hours: opt.min_hours }).catch(() => {});
    }, 350);
  });
  const cold = (r) => !!r.cold;
  const drain = dataTable(ctx, {
    path: api.apiPath`cars/${car.id}/parked`, params: { ...ctx.params, min_hours: opt.min_hours }, limit: 50,
    itemName: ['parked spell', 'parked spells'], cover: 'vd.table', label: 'Parked spells',
    caption: 'Newest first. Spells crossing the period’s edges are cut at them. The CSV holds every listed spell.',
    columns: [
      { key: 'start', label: 'Parked', th: true, cover: ['vd.col.start', 'vd.col.end', 'vd.link.start_details'], value: (r) => r.start_ms,
        hint: 'When the car stopped and when it was next used. The time opens the battery and state charts for exactly this span.',
        cell: (r) => [h('a', { class: 'cell-main bt-when', href: state.buildUrl('trip', { car: car.id, period: { from: r.start_ms, to: r.end_ms } }) }, formatDateTime(r.start_ms, tz, { now: api.now() })),
          h('span', { class: 'cell-sub' }, 'until ', h('span', { class: 'bt-when' }, formatDateTime(r.end_ms, tz, { now: api.now() })), r.clipped ? ' (cut at the period’s edge)' : ''),
          h('span', { class: 'cell-sub' }, (r.place && r.place.name) || r.address || 'an unnamed spot', ' · ', h('a', { href: state.buildUrl('timeline', { car: car.id, period: { from: r.start_ms, to: r.end_ms } }) }, 'timeline'))] },
      { key: 'len', label: 'Length', num: true, sort: true, cover: ['vd.col.period', 'vd.col.standby'], value: (r) => r.duration_ms,
        hint: 'How long the car stood, and the share of it the car slept. Awake and no-data time are shown under it; no-data time is never counted as sleep.',
        cell: (r) => [h('span', { class: 'cell-main bt-when' }, formatDuration(r.duration_ms)),
          h('span', { class: 'cell-sub' }, r.duration_ms ? `${pctText((r.asleep_ms / r.duration_ms) * 100)} asleep` : ''),
          h('span', { class: 'cell-sub' }, `awake ${formatDuration(r.awake_ms + (r.awake_vehicle_bus_only_ms || 0))}`),
          r.unknown_ms ? h('span', { class: 'cell-sub bt-unknown' }, `no data ${formatDuration(r.unknown_ms)}`) : null] },
      { key: 'soc', label: 'Battery lost', num: true, sort: true, cover: 'vd.col.soc_diff', value: (r) => (num(r.soc_start_pct) && num(r.soc_end_pct) ? r.soc_start_pct - r.soc_end_pct : null),
        cell: (r) => (num(r.soc_start_pct) && num(r.soc_end_pct) ? [h('span', { class: 'cell-main' }, q(r.soc_start_pct - r.soc_end_pct, 'percent', { digits: 1 })), h('span', { class: 'cell-sub bt-when' }, `${pctText(r.soc_start_pct)} to ${pctText(r.soc_end_pct)}`)] : q(null, 'percent', { why: 'no reading' })) },
      { key: 'energy', label: 'Energy lost', num: true, sort: true, cover: ['vd.col.energy', 'vd.col.avg_power'], value: (r) => r.energy_kwh,
        hint: 'Energy the pack gave while parked, and the average power that makes over the whole spell (and while awake).',
        cell: (r) => [h('span', { class: 'cell-main' }, q(r.energy_kwh, 'energy', { digits: 2, why: 'not measured' })),
          h('span', { class: 'cell-sub bt-when' }, 'average ', q(r.avg_power_w, 'power_w')),
          num(r.awake_avg_power_w) && r.awake_ms > 0 ? h('span', { class: 'cell-sub bt-when' }, 'awake ', q(r.awake_avg_power_w, 'power_w')) : null,
          r.energy_method === 'range' ? h('span', { class: 'cell-sub' }, flag('estimated', { kind: 'estimated' })) : null] },
      { key: 'range', label: `Range lost`, num: true, sort: true, cover: ['vd.col.range_loss', 'vd.col.range_per_hour'], value: (r) => (cold(r) ? null : r.range_lost_km),
        hint: `${rangeLabel()} lost while parked, and per hour parked. Blank when the battery ended cold: that drop is locked energy, not energy used.`,
        cell: (r) => (cold(r) ? q(null, 'range', { why: 'cold battery' }) : [h('span', { class: 'cell-main' }, q(r.range_lost_km, 'range', { digits: 1 })),
          num(r.range_lost_per_hour_km) ? h('span', { class: 'cell-sub bt-when' }, q(r.range_lost_per_hour_km, 'distance', { digits: 2 }), ' per hour') : null,
          num(r.range_start_km) ? h('span', { class: 'cell-sub bt-when' }, `${format(r.range_start_km, 'range')} to ${format(r.range_end_km, 'range')}`) : null]) },
      { key: 'cold', label: 'Coldest battery', num: true, sort: true, cover: 'vd.flag.cold', value: (r) => r.battery_min_c,
        hint: 'A cold pack locks part of its energy, so its range drops without energy being used; range figures are left blank for those spells.',
        cell: (r) => [h('span', { class: 'cell-main' }, q(r.battery_min_c, 'temperature', { why: 'not sent' })), r.cold ? h('span', { class: 'cell-sub' }, flag('cold', { kind: 'warn', title: 'Part of the pack was locked by the cold at the end' })) : null] },
      { key: 'why', label: 'Why it stayed awake', cls: 'bt-why', value: (r) => (r.awake_reasons || []).join(';'),
        cell: (r) => ((r.awake_reasons || []).length ? formatList(r.awake_reasons.map((x) => AWAKE_WORDS[x] || x.replace(/_/g, ' '))) : r.awake_ms > 0 ? 'no reason sent' : 'slept throughout') },
    ],
    empty: () => empty({ title: opt.min_hours > 0 ? `No parked spell of ${formatNumber(opt.min_hours)} hours or more in this period.` : 'The car did not park off a charger in this period.', text: opt.min_hours > 0 ? 'Lower the shortest spell above, or choose a longer period.' : 'Choose a longer period with the period picker at the top.' }),
    onRows: (_, pg) => {
      const s = pg && pg.summary;
      if (!s) return;
      replace(drainSum, figures([
        figure('Parked spells', q(s.spells, 'count'), `${formatNumber(s.hours || 0, { maximumFractionDigits: 0 })} hours in all${opt.min_hours > 0 ? `, each at least ${formatNumber(opt.min_hours)} h` : ''}.`),
        figure('Energy lost', q(s.energy_kwh, 'energy', { digits: 1 }), 'From the pack’s own energy counters, not whole percent steps.'),
        figure('Per day parked', q(s.kwh_per_day, 'energy', { digits: 2, why: 'needs more parked time' }), 'Energy lost per 24 hours parked.'),
        figure('Cost at the home price', s.cost ? formatMoneyText(s.cost) : q(null, 'money', { why: 'no home price set' }), s.cost ? 'What putting it back at home would cost.' : 'Set a price on the Home place to see it.'),
      ], { cls: 'bt-figs-4' }));
    },
  });
  const formatMoneyText = (c) => formatMoney(c.amount, c.currency);
  replace(drainBox,
    head('bt-drain-title', 'Parked drain', 'each time the car stood off a charger', null),
    h('div', { class: 'bt-controls' },
      periodNote('vd.filter.time_range', 'a spell belongs to the period while it overlaps it'),
      h('div', { class: 'field', cover: 'vd.filter.min_idle' },
        h('label', { class: 'field-label', for: 'bt-min-hours' }, 'Shortest spell'),
        h('div', { class: 'bt-inline' }, minIn, h('span', { class: 'bt-unit' }, 'hours')),
        h('p', { class: 'field-hint' }, 'Leaves out short stops; 0 lists every spell.'))),
    drainSum,
    h('p', { class: 'note' }, 'Open a spell’s time for its battery and state charts over exactly that span. Asleep counts only real sleep: time with no data is shown apart, in red, and never counted as sleep. ‘Cold’ marks spells where the pack ended cold enough to lock part of its energy; their range columns stay blank because the range drop there is not energy used.'),
    drain.el);

  /* ---------------------------------------------------------------- every car */

  let fleet = [];
  let shown = 0;
  const carRows = [];
  const carTable = dataTable(ctx, {
    rows: [], itemName: ['car', 'cars'], label: 'Every car’s battery',
    csv: { filename: file('every car battery'), columns: [
      { label: 'Car', value: (r) => r.name }, column('Health', (r) => r.bat?.capacity?.health_pct, 'percent', { digits: 1 }),
      column('Full pack now', (r) => r.bat?.capacity?.full_pack_kwh, 'energy', { digits: 2 }), column('Full-charge range now', (r) => r.bat?.range?.full_now_km, 'range', { digits: 1 }),
      column('Range lost', (r) => r.bat?.range?.lost_km, 'range', { digits: 1 }), column('Battery now', (r) => r.bat?.soc_pct, 'percent', { digits: 1 }),
      column('Typical level in the period', (r) => r.level?.stats?.median, 'percent', { digits: 1 }), column('Time above 80 %', (r) => r.level?.share_above_80_pct, 'percent', { digits: 1 }),
      column('Parked drain per day', (r) => r.parked?.summary?.kwh_per_day, 'energy', { digits: 3 }), column('Equivalent full cycles', (r) => r.bat?.lifetime?.equivalent_cycles, null)] },
    columns: [
      { key: 'name', label: 'Car', th: true, sort: true, value: (r) => r.name, cell: (r) => [h('a', { class: 'cell-main', href: state.buildUrl('battery', { car: r.id, period: ctx.state.period }), 'aria-current': r.id === car.id ? 'page' : null }, r.name), h('span', { class: 'cell-sub' }, [r.model, r.year].filter(Boolean).join(' '), r.id === car.id ? ' · shown above' : '')] },
      { key: 'health', label: 'Health', num: true, sort: true, value: (r) => r.bat?.capacity?.health_pct, cell: (r) => (r.err ? 'could not load' : q(r.bat?.capacity?.health_pct, 'percent', { digits: 1 })) },
      { key: 'pack', label: 'Full pack now', num: true, sort: true, value: (r) => r.bat?.capacity?.full_pack_kwh, cell: (r) => q(r.bat?.capacity?.full_pack_kwh, 'energy', { digits: 1 }) },
      { key: 'range', label: `Full-charge range`, num: true, sort: true, value: (r) => r.bat?.range?.full_now_km, cell: (r) => [h('span', { class: 'cell-main' }, q(r.bat?.range?.full_now_km, 'range')), num(r.bat?.range?.lost_km) ? h('span', { class: 'cell-sub' }, ['lost ', q(r.bat.range.lost_km, 'range')]) : null] },
      { key: 'soc', label: 'Battery now', num: true, sort: true, value: (r) => r.bat?.soc_pct, cell: (r) => q(r.bat?.soc_pct, 'percent') },
      { key: 'level', label: 'Typical level', num: true, sort: true, value: (r) => r.level?.stats?.median, cell: (r) => [h('span', { class: 'cell-main' }, q(r.level?.stats?.median, 'percent', { why: 'no readings' })), num(r.level?.share_above_80_pct) ? h('span', { class: 'cell-sub' }, `${pctText(r.level.share_above_80_pct)} above 80 %`) : null] },
      { key: 'drain', label: 'Parked drain', num: true, sort: true, value: (r) => r.parked?.summary?.kwh_per_day, cell: (r) => (num(r.parked?.summary?.kwh_per_day) ? [q(r.parked.summary.kwh_per_day, 'energy', { digits: 2 }), h('span', { class: 'q-u' }, ' per day')] : q(null, 'energy', { why: 'not parked' })) },
      { key: 'cycles', label: 'Full cycles', num: true, sort: true, value: (r) => r.bat?.lifetime?.equivalent_cycles, cell: (r) => q(r.bat?.lifetime?.equivalent_cycles, 'number', { digits: 0 }) },
    ],
  });
  const moreCars = button('Show more cars', { cls: 'button-small', onclick: () => loadCars().catch(() => {}) });
  moreCars.hidden = true;
  async function loadCars() {
    const batch = fleet.slice(shown, shown + 10);
    shown += batch.length;
    moreCars.disabled = true;
    const got = await Promise.all(batch.map(async (c) => {
      const [b, l, p] = await Promise.allSettled([
        ctx.api.get(api.apiPath`cars/${c.id}/battery`),
        ctx.api.get(api.apiPath`cars/${c.id}/battery-level`, { ...ctx.params, bucket: 'day', moving: 'false' }),
        ctx.api.get(api.apiPath`cars/${c.id}/parked`, { ...ctx.params, min_hours: opt.min_hours, limit: 1 }),
      ]);
      return { id: c.id, name: c.name, model: c.model, year: c.year, bat: b.status === 'fulfilled' ? b.value : null, err: b.status === 'rejected', level: l.status === 'fulfilled' ? l.value : null, parked: p.status === 'fulfilled' ? p.value : null };
    }));
    if (!ctx.alive()) return;
    carRows.push(...got);
    carTable.setRows(carRows);
    moreCars.disabled = false;
    moreCars.hidden = shown >= fleet.length;
  }
  async function drawCars() {
    try { fleet = await ctx.api.list('cars', { sort: 'name' }, { limit: 200 }).all({ max: 5000 }); } catch (err) {
      if (ctx.signal.aborted) return;
      replace(carsBox, head('bt-cars-title', 'Every car'), errorView(err, { eyebrow: 'Every car', retry: () => drawCars() }));
      return;
    }
    if (!fleet.some((c) => c.id === car.id)) fleet.unshift(car);
    replace(carsBox,
      head('bt-cars-title', 'Every car', `${tn(fleet.length, '{n} car', '{n} cars')} side by side; open one for its whole page`),
      h('p', { class: 'note', cover: ['bh.filter.car', 'vd.section.per_car'] }, 'Health and full-charge range are whole-life figures; the typical level and parked drain follow the period and the shortest parked spell above. Choose a car here or with the car switcher at the top: the period stays as it is.'),
      carTable.el, h('div', { class: 'bt-more' }, moreCars));
    await carTable.load();
    await loadCars();
  }

  /* ---------------------------------------------------------------- load */

  // A drive, charge or parked spell started or ended: the parked list and the level load again.
  let rowsTimer = 0;
  ctx.events(['rows'], (ev) => {
    const d = ev.data || {};
    if (d.car && d.car !== car.id) return;
    clearTimeout(rowsTimer);
    rowsTimer = setTimeout(() => { if (ctx.alive()) { replace(drainSum); drain.reload({ ...ctx.params, min_hours: opt.min_hours }).catch(() => {}); } }, 3000);
  });
  ctx.onCleanup(() => clearTimeout(rowsTimer));

  await loadTop();
  await Promise.all([drawCap(), drawLife(), drawProj(), drawLevel(), drain.load(), drawCars()]);
  ctx.ready();
}

/** Date ticks between two times: [[ms, text]], about five, on month or year starts. */
function timeTicks(a, b, tz) {
  const span = b - a;
  const years = span > 3 * 365 * 86400000;
  const fmt = dateFormat(tz, years ? { year: 'numeric' } : { month: 'short', year: 'numeric' });
  const out = [];
  const stepMonths = years ? 12 : Math.max(1, Math.round(span / (5 * 30.4 * 86400000)));
  const d = new Date(a);
  let y = d.getUTCFullYear(), m = years ? 0 : d.getUTCMonth();
  for (let i = 0; i < 200; i++) {
    const ms = Date.UTC(y, m, 1);
    if (ms > b) break;
    if (ms >= a) out.push([ms, fmt.format(ms)]);
    m += stepMonths;
    y += Math.floor(m / 12);
    m %= 12;
  }
  return out;
}
