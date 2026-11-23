// Battery (/cars/{car}/battery): the pack now, capacity and health, full-charge range, capacity against the
// odometer (a scatter with the monthly median and a trend) and a table of capacity and full range by month,
// the lifetime totals and the reference values; then, for the range: charge level, projected full-charge
// range and parked drain.
// - Whole life: GET .../battery, .../totals, .../settings, .../efficiency (the energy factor),
//   .../battery/capacity (group=charge|day|month, x=odometer|time).
// - The range: GET .../battery-level, .../projected-range, .../parked. Their choices live in the address
//   (cap_x, cap_group, cl_bucket, cl_moving, cl_window, pr_bucket, min_h).
// - Reference values: PATCH .../settings (admin), typed in the person's units.
// Asleep is not unknown: lines carry on through asleep time and break where nothing is known.

import * as state from '../shared/state.js';
import { h, replace, cssVar } from '../shared/dom.js';
import { format, formatMoney, convert, toMetric, unitLabel, getUnits } from '../shared/units.js';
import { formatDateTime, formatDate, formatTime, formatDuration, startOf, dateFormat, DAY } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { head, section, kv, note, link, textBar } from '../core/ui.js';
import { num, tag } from '../core/fmt.js';
import { table, cellBar } from '../core/table.js';
import { empty, loadingLine, errorBlock } from '../core/states.js';
import { loadUPlot, plot, line, timeAxis, valueAxis, cursor, readout, colors, hexA } from '../core/chart.js';
import { field, form } from '../core/form.js';
import { toast } from '../core/toast.js';
import { rpx } from '../core/display.js';
import { src, srcText } from '../core/source.js';

export const id = 'battery';
export const title = () => 'Battery';

const enc = encodeURIComponent;
const pct = (v, d = 0) => format(v, 'percent', { digits: d });
const n0 = (v) => (v == null ? '—' : formatNumber(v, { maximumFractionDigits: 0 }));
const none = (t) => h('span', { class: 'muted' }, t);
const NEW_SOURCE = { owner: 'typed by the owner', early_bms: 'median of the first 30 days of pack readings', imported_charges: 'the earliest imported charges', pack_code: 'the pack-size code (a hint, not a measurement)', vehicle_info: 'the car\'s build (VIN and CAN), not a measurement' };
const RANGE_SOURCE = { owner: 'typed by the owner', first_30_days: 'median full range of the first 30 days', pack_code: 'the pack-size code (a hint)', vehicle_info: 'the car\'s build (EPA rating)' };
const NOTES = {
  new_value_typed: 'the "when new" values are the typed reference values.',
  new_value_from_early_readings: 'no value was typed: "when new" is the median of the earliest pack readings.',
  new_value_from_imported_charges: 'logged late: "when new" comes from the earliest imported charges.',
  new_value_from_pack_code: 'no early readings: "when new" is only the pack-size code; type the real figure for a true health figure.',
  new_value_from_vehicle_info: 'There are no early readings, so “when new” is what the car’s build (VIN and CAN) gives. Type the real figure under Reference values if you know it.',
  cold_battery_locks_part: 'the pack is cold: part of its energy is locked until it warms, so usable is below battery %.',
  lfp_charge_to_100_weekly: 'lfp pack: charge to 100% about once a week so the car can recalibrate.',
  imported_history_estimated: 'part of the history is imported; figures from it are estimated.',
  no_history: 'no history for this car yet.',
  pack_replaced: 'the pack was replaced: capacity, health and the trend cover the new pack only.',
};
const CHEM = { lfp: 'lfp (iron phosphate)', nca_nmc: 'nca / nmc (nickel)' };
const REASONS = {
  sentry: 'sentry', keep_climate_on: 'climate on', keep_climate_dog: 'dog mode', keep_climate_camp: 'camp mode', user_present: 'someone near',
  preconditioning_cabin: 'cabin precondition', preconditioning_battery: 'pack precondition', charging: 'charging', keep_warm: 'keeping warm',
  door_open: 'door open', accessory_power: 'accessory power',
};
const METHOD = { counter: 'pack counters', bms_energy: 'pack energy', range: 'from range (est)' };

export async function mount(ctx) {
  const car = ctx.car, cid = car.id, tz = ctx.tz, base = `cars/${enc(cid)}`;
  const period = ctx.period, q = ctx.query;
  const now = () => ctx.api.now();
  const opt = {
    capX: q.cap_x === 'time' ? 'time' : 'odometer',
    capGroup: q.cap_group === 'day' ? 'day' : 'charge',
    clBucket: q.cl_bucket === 'hour' || q.cl_bucket === 'day' ? q.cl_bucket : null,
    clMoving: q.cl_moving !== '0',
    clWindow: q.cl_window === '30d' ? '30d' : '7d',
    prBucket: ['hour', 'day', 'week'].includes(q.pr_bucket) ? q.pr_bucket : 'auto',
    minH: [0, 1, 3, 6, 12, 24].includes(Number(q.min_h)) ? Number(q.min_h) : 0,
  };

  // Every call starts at once and each block draws when its own answers are in (a loading line holds its place
  // until then), so the page shows what it has instead of waiting for the slowest call.
  const batP = ctx.api.get(`${base}/battery`);
  const effP = ctx.api.get(`${base}/efficiency`).catch(() => null);
  const totalsP = ctx.api.get(`${base}/totals`).catch(() => null);
  const settingsP = ctx.api.get(`${base}/settings`).catch(() => null);
  // filled when GET battery (and efficiency) is in; the blocks below read them then
  let bat = {}, cap = {}, life = {}, fr = {}, toIdeal = null, rangeWord = 'rated', rng = {}, factor = null;
  const batReady = Promise.all([batP, effP]).then(([b, eff]) => {
    bat = b; cap = bat.capacity || {}; life = bat.lifetime || {};
    fr = eff?.factor || {};
    toIdeal = getUnits().range === 'ideal' && fr.rated_wh_per_km > 0 && fr.ideal_wh_per_km > 0 ? fr.rated_wh_per_km / fr.ideal_wh_per_km : null;
    rangeWord = toIdeal ? 'ideal' : 'rated';
    const rk = (km) => (km == null ? km : toIdeal ? km * toIdeal : km);
    const rng0 = bat.range || {};
    rng = { ...rng0, full_now_km: rk(rng0.full_now_km), full_now_from_energy_km: rk(rng0.full_now_from_energy_km), full_new_km: rk(rng0.full_new_km), lost_km: rk(rng0.lost_km) };
    factor = toIdeal ? fr.ideal_wh_per_km : fr.rated_wh_per_km;
  });
  const batSettled = batReady.catch(() => null);

  /* ---------- head */
  const chemEl = h('span', { class: 'ph-extra muted' });
  const hd = head(ctx, { extra: chemEl });
  const [periodBtn, carBtn] = hd.querySelectorAll('.ph-btn');
  if (periodBtn) periodBtn.dataset.cover = 'cl.filter.time_range pr.filter.time_range vd.filter.time_range';
  if (carBtn) carBtn.dataset.cover = 'bh.filter.car cl.filter.car pr.section.per_car vd.section.per_car';
  const readAt = none('');
  const sub = h('p', { class: 'subline' }, readAt, none('the first blocks are the car’s whole life; from "charge level" on they follow the range.  '),
    link(state.buildUrl('statistics', { query: { cars: 'all' } }), 'every car'));

  /* ---------- now */
  const nowSlot = h('div');
  function paintNow(live = {}) {
    const soc = live.soc ?? bat.soc_pct, usable = live.usable ?? bat.usable_soc_pct, kwh = live.kwh ?? bat.energy_now_kwh;
    const band = bat.band || {};
    const lim = live.limit ?? band.charge_limit_pct;
    const lo = band.low_pct ?? 20, hi = band.high_pct ?? 80;
    // a 20-cell ruler: '#' charged, '-' the everyday band not reached, '.' outside the band, '|' the limit
    const cells = 20;
    const ruler = Array.from({ length: cells }, (_, i) => {
      const at = ((i + 0.5) / cells) * 100;
      if (lim != null && Math.abs(at - lim) <= 100 / cells / 2 && !(Math.abs(at - 100 / cells - lim) <= 100 / cells / 2 && at > lim)) return ['|', 'warn'];
      if (soc != null && at <= soc) return ['#', 'on'];
      return at >= lo && at <= hi ? ['-', 'band'] : ['.', 'off'];
    });
    replace(nowSlot, kv([
      ['battery', [h('b', null, soc == null ? '—' : pct(soc)), none('  the battery % the car shows')], { cover: 'bh.soc.current' }],
      ['usable', usable != null ? [pct(usable), soc != null && soc - usable >= 0.5 ? none('  a cold pack locks part of it') : null] : none('not reported')],
      ['stored', kwh != null ? [num(kwh, 'energy'), cap.usable_kwh ? none(` of ${format(cap.usable_kwh, 'energy')} usable when full`) : null] : none('not reported'), { cover: 'bh.bar.stored_energy' }],
      ['everyday band', [h('span', { class: 'ruler', role: 'img', 'aria-label': `battery ${pct(soc)}, band ${pct(lo)} to ${pct(hi)}, limit ${pct(lim)}` }, ruler.map(([c, k]) => h('span', { class: `ruler-${k}` }, c))),
        none(`  ${pct(lo)}..${pct(hi)}${hi >= 100 ? ' (lfp: full is fine)' : ''}`)], { cover: 'bh.soc.band' }],
      ['charge limit', lim != null ? [h('span', { class: 'tone--warn' }, '|'), ' ', pct(lim)] : none('not sent')],
      ['range at this level', soc != null && rng.full_now_km != null ? num((rng.full_now_km * soc) / 100, 'range') : '—'],
      ['to the limit', lim != null && kwh != null && cap.usable_kwh ? num(Math.max(0, (cap.usable_kwh * lim) / 100 - kwh), 'energy') : '—'],
      ['buffer', [num(cap.buffer_kwh, 'energy'), none('  below 0%, never usable')]],
    ]));
  }
  /* ---------- the whole-life blocks, each drawn when its answers are in */
  const waitSec = (title, side) => section({ title, side }, loadingLine(`loading ${title}`));
  const capWait = waitSec('capacity and health'), rangeWait = waitSec('full-charge range'), lifeWait = waitSec('lifetime totals', none('whole life')), refWait = waitSec('reference values');
  replace(nowSlot, loadingLine('loading the battery'));
  async function paintTop() {
    await batReady;
    if (!ctx.alive()) return;
    replace(chemEl, bat.chemistry?.value ? `${CHEM[bat.chemistry.value] || bat.chemistry.value}` : 'chemistry not known');
    replace(readAt, `pack read ${formatDateTime(bat.as_of_ms, tz, { now: now(), weekday: false })}.  `);
    paintNow();
    ctx.events('now', (ev) => {
      if (ev.data?.car !== cid) return;
      const d = ev.data.now || {};
      const v = (x) => (x && typeof x === 'object' && 'value' in x ? x.value : undefined);
      const live = Object.fromEntries(Object.entries({ soc: v(d.soc_pct), usable: v(d.usable_soc_pct), kwh: v(d.energy_remaining_kwh), limit: v(d.charge?.limit_pct) }).filter(([, x]) => x != null));
      if (Object.keys(live).length) paintNow(live);
    });

    /* ---------- capacity and health, range */
    const health = cap.health_pct, warranty = cap.warranty_pct;
    const hTone = health == null ? 'accent' : warranty != null && health < warranty ? 'bad' : health < (warranty ?? 70) + 5 ? 'warn' : 'accent';
    const capBlock = kv([
      ['health', health != null ? [textBar(health, { tone: hTone, label: `health ${pct(health, 1)}` }), ' ', h('b', null, pct(health, 1)), none(cap.degradation_pct != null ? `  ${formatNumber(cap.degradation_pct, { maximumFractionDigits: 1 })}% lost` : '')] : none('needs a "when new" value'), { cover: ['bh.bar.health', 'bh.gauge.degradation'] }],
      ['warranty line', warranty != null ? [pct(warranty), none(health != null ? (health >= warranty ? '  above it' : '  below it') : '')] : none('not set')],
      ['usable now', [num(cap.usable_kwh, 'energy'), none(cap.full_pack_kwh != null ? `  full pack ${format(cap.full_pack_kwh, 'energy')} minus the buffer, ${n0(cap.readings)} readings` : '')], { cover: 'bh.capacity.usable_now' }],
      ['usable new', cap.new_kwh != null ? [num(cap.new_kwh, 'energy'), none(cap.new_source ? `  ${NEW_SOURCE[cap.new_source] || cap.new_source}` : '')] : none('not known: type it under reference values'), { cover: 'bh.capacity.usable_new' }],
      ['difference', cap.difference_kwh != null ? num(-cap.difference_kwh, 'energy', { sign: true }) : none('needs a "when new" value'), { cover: 'bh.capacity.difference' }],
      [`per ${unitLabel('range')} ${rangeWord}`, factor != null ? [num(factor, 'efficiency', { digits: 1 }), none(`  from ${n0(fr.readings)} readings`)] : none('not worked out yet'), { cover: 'bh.stat.efficiency' }],
      ['brick spread', bat.brick_spread_v != null ? [h('span', { class: 'n' }, formatNumber(bat.brick_spread_v * 1000, { maximumFractionDigits: 0 }), h('span', { class: 'u' }, ' mV')), none(bat.brick_spread_v < 0.03 ? '  well balanced' : '  over 30 mV')] : none('not reported')],
    ]);
    const rangeBlock = kv([
      ['at 100% now', [num(rng.full_now_km, 'range'), none(`  ${rangeWord}; from energy ${format(rng.full_now_from_energy_km, 'range')}`)], { cover: 'bh.ranges.max_now' }],
      ['at 100% new', rng.full_new_km != null ? [num(rng.full_new_km, 'range'), none(rng.new_source ? `  ${RANGE_SOURCE[rng.new_source] || rng.new_source}` : '')] : none('not known'), { cover: 'bh.ranges.max_new' }],
      ['range lost', rng.lost_km != null ? [num(rng.lost_km, 'range'), rng.full_new_km ? none(`  ${formatNumber((rng.lost_km / rng.full_new_km) * 100, { maximumFractionDigits: 1 })}% of new`) : null] : none('needs a "when new" value'), { cover: 'bh.ranges.lost' }],
      ['now of new', rng.full_new_km && rng.full_now_km ? [textBar(Math.min(100, (rng.full_now_km / rng.full_new_km) * 100)), ' ', pct((rng.full_now_km / rng.full_new_km) * 100, 1)] : none('—')],
    ]);
    const pk = bat.pack || {};
    const packLine = pk.replaced_ms != null ? h('p', { class: 'note', dataset: { pack: 'replaced' } }, tag('pack', 'muted'), ' ',
      `Pack replaced on ${formatDate(pk.replaced_ms, tz, { weekday: false })} (${pk.source === 'owner' ? 'typed by the owner' : "detected from the pack's readings"}).`) : null;
    const notes = (bat.notes || []).filter((c) => NOTES[c]).map((c) => h('p', { class: 'note' }, /cold|lfp/.test(c) ? tag('warn', 'warn') : tag('info', 'muted'), ' ', NOTES[c]));
    capWait.replaceWith(section({ title: 'capacity and health' }, capBlock,
      h('p', { class: 'note', cover: ['bh.capacity.help', 'bh.gauge.degradation_help'] }, 'health is usable now over usable when new, both from the battery management system (it reports capacity every awake minute; nothing from range). a guide, not a lab test: it moves with temperature and balancing.'),
      packLine, notes));
    rangeWait.replaceWith(section({ title: 'full-charge range' }, rangeBlock, note('the range estimate scales with the car’s energy factor, so it drops in step with capacity. real range depends on speed, weather and climate: see efficiency.')));
  }
  async function paintLife() {
    const [, totals] = await Promise.all([batReady, totalsP]);
    if (!ctx.alive()) return;
    /* ---------- lifetime totals */
    const t = totals || {};
    const mileage = t.odometer_last_km != null && t.odometer_first_km != null ? t.odometer_last_km - t.odometer_first_km : null;
    const inKwh = (life.ac_kwh || 0) + (life.dc_kwh || 0) + (life.regen_kwh || 0);
    const share = (k) => (inKwh ? ((k || 0) / inKwh) * 100 : 0);
    const lifeBlock = h('div', null,
      kv([
        ['odometer', num(t.odometer_last_km, 'odometer'), { cover: 'bh.drive.odometer' }],
        ['charges', [n0(t.charges), none(`  ac ${n0(t.ac?.charges)}, dc ${n0(t.dc?.charges)}  `), link(ctx.link('charges', { period: { preset: 'all' } }), 'charges', { cover: 'bh.charging.link_stats' })], { cover: 'bh.charging.count' }],
        ['driven since logging', num(mileage, 'distance'), { cover: 'bh.drive.mileage' }],
        ['added to the pack', num(t.energy_added_kwh, 'energy'), { cover: 'bh.charging.energy_added' }],
        ['logged in drives', [link(ctx.link('drives', { period: { preset: 'all' } }), format(t.distance_km, 'distance')), none(`  ${n0(t.drives)} drives  `), link(ctx.link('statistics'), 'statistics', { cover: 'bh.drive.link_stats' })], { cover: 'bh.drive.logged' }],
        ['from the grid', [num(t.energy_grid_kwh, 'energy'), t.grid_coverage_pct != null && t.grid_coverage_pct < 100 ? none(`  measured for ${pct(t.grid_coverage_pct)} of it`) : null], { cover: 'bh.charging.energy_used' }],
        ['not logged', t.not_logged_km ? [h('span', { class: t.not_logged_km > 1 ? 'tone--warn' : null }, format(t.not_logged_km, 'distance')), '  ', link(state.buildUrl('health', { car: cid, period: { preset: 'all' } }), 'the gaps')] : num(t.not_logged_km, 'distance'), { cover: 'bh.drive.data_lost' }],
        ['charging efficiency', t.charging_efficiency_pct != null ? pct(t.charging_efficiency_pct, 1) : none('not measured'), { cover: 'bh.charging.efficiency' }],
        ['full cycles', life.equivalent_cycles != null ? [formatNumber(life.equivalent_cycles, { maximumFractionDigits: 1 }), none('  energy charged over usable capacity')] : '—', { cover: 'bh.charging.cycles' }],
      ]),
      h('p', { class: 'note', cover: 'bh.drive.help' }, 'not logged is distance the odometer moved while nothing was recorded; each gap is on health.'),
      h('p', { class: 'note', cover: 'bh.charging.help' }, 'charging efficiency is energy into the pack over energy from the grid, for ac charges where both were measured; dc chargers do not report their grid side, so they are left out.'),
      h('div', { class: 'blk', cover: 'bh.chart.ac_dc' }, h('h3', { class: 'sub-title' }, 'energy into the pack, by source'),
        kv([['ac charging', life.ac_kwh], ['dc fast', life.dc_kwh], ['regen', life.regen_kwh]].map(([l, k], i) => [l, [cellBar(share(k) / 100, 20, ['accent', 'warn', 'text'][i]), ' ', pct(share(k)), none(`  ${format(k, 'energy')}`)]]), { cols: 1 }),
        note(`from the pack’s lifetime counters (${life.basis || 'pack counters'}), so it covers years before logging. out: ${format(life.drive_kwh, 'energy')} driving, ${format(life.standing_kwh, 'energy')} parked.`)));
    lifeWait.replaceWith(section({ title: 'lifetime totals', side: none('whole life') }, lifeBlock));
  }
  async function paintRef() {
    const [, settings] = await Promise.all([batReady, settingsP]);
    if (!ctx.alive()) return;
    /* ---------- reference values */
    const refBlock = (() => {
      const s = settings || {};
      const can = !!ctx.admin && !!settings;
      const rUnit = unitLabel('range');
      const capF = field({ label: 'new kwh', name: 'capacity_new_kwh', type: 'number', value: s.capacity_new_kwh, hint: 'usable capacity when new, 10 to 250. empty: from the earliest readings.', attrs: { min: 10, max: 250, step: 0.1, inputmode: 'decimal', disabled: !can, size: '8' } });
      const rngF = field({ label: `new ${rUnit}`, name: 'full_range_new_km', type: 'number', value: s.full_range_new_km == null ? null : Math.round(convert(s.full_range_new_km, 'range')), hint: 'full-charge range when new. empty: the median full range of the first 30 days.', attrs: { min: 1, step: 1, inputmode: 'numeric', disabled: !can, size: '8' } });
      const warF = field({ label: 'warranty %', name: 'warranty_threshold_pct', type: 'number', value: s.warranty_threshold_pct, hint: 'warranty threshold: the health your warranty promises, often 70.', attrs: { min: 50, max: 100, step: 1, inputmode: 'numeric', disabled: !can, size: '8' } });
      capF.el.dataset.cover = 'bh.setting.capacity_new_override';
      rngF.el.dataset.cover = 'bh.setting.range_new_override';
      let version = s.version;
      const numOrNull = (v) => (v === '' || v == null ? null : Number(v));
      const f = form({ fields: [capF, rngF, warF], cls: 'frm--narrow', submit: 'save reference values', async onSubmit(v) {
        if (!can) return;
        const r = numOrNull(v.full_range_new_km);
        const res = await ctx.api.patch(`${base}/settings`, { version, capacity_new_kwh: numOrNull(v.capacity_new_kwh), full_range_new_km: r == null ? null : Math.round(toMetric(r, 'range') * 10) / 10, warranty_threshold_pct: numOrNull(v.warranty_threshold_pct) });
        version = res.version;
        toast('reference values saved; health and range use them now', { kind: 'ok', next: true });
        ctx.reload();
      } });
      if (!can) f.submitButton.disabled = true;
      return section({ title: 'reference values', side: link(state.buildUrl('settings-cars', { car: cid }), 'settings/cars') },
        h('p', { class: 'note', cover: 'bh.setting.override_help' }, 'typed once for this car and checked when saved: the figures from its papers or first weeks. they set what "when new" means for health and range lost.'),
        f.el,
        !settings ? note('the car settings could not be read, so they cannot be changed here.') : !ctx.admin ? note('only an admin can change these.') : null,
        bat.chemistry ? note(`chemistry: ${CHEM[bat.chemistry.value] || 'not known'}, ${bat.chemistry.source === 'owner' ? 'set by the owner' : 'detected from the pack'}.`) : null);
    })();
    refWait.replaceWith(refBlock);
  }

  /* ---------- slots */
  const capSlot = h('div', null, loadingLine('loading capacity readings'));
  const monthSlot = h('div');
  const clSlot = h('div', null, loadingLine('loading charge level'));
  const prSlot = h('div', null, loadingLine('loading projected range'));
  const vdSlot = h('div', null, loadingLine('loading parked spells'));
  replace(ctx.view, h('div', { class: 'page', dataset: { page: 'battery' } },
    hd, sub,
    h('div', { cover: 'shots.battery-health' },
      h('div', { class: 'cols2' },
        section({ title: 'now', side: none('live') }, nowSlot),
        capWait),
      h('div', { class: 'cols2' }, rangeWait, lifeWait),
      section({ title: 'capacity against the odometer', cover: ['bh.chart.capacity_by_distance.points', 'bh.chart.capacity_by_distance.median'] }, capSlot),
      monthSlot,
      refWait),
    section({ title: 'charge level', cover: ['cl.chart.charge_level', 'shots.charge-level'], side: none('in the range') }, clSlot),
    section({ title: 'projected full-charge range', cover: 'shots.projected-range', side: none('in the range') }, prSlot),
    vdSlot));

  const keyRow = (label, choices, cur, onPick, attrs = {}) => h('div', { class: 'st-switch', ...attrs }, h('span', { class: 'muted' }, `${label}> `),
    h('span', { class: 'keys keys--inline', role: 'group', 'aria-label': label }, choices.map(([v, l, off]) => h('button', { class: 'key', type: 'button', disabled: !!off, title: off || null, 'aria-pressed': String(v === cur), onclick: () => { if (v !== cur) onPick(v); } }, l))));
  async function collect(path, params) {
    const items = []; let trend = null, after;
    for (let k = 0; k < 20; k++) {
      const pg = await ctx.api.get(path, { ...params, limit: 500, ...(after ? { after } : {}) });
      items.push(...(pg.items || [])); trend = trend || pg.trend;
      if (!pg.next) break; after = pg.next;
    }
    return { items, trend };
  }
  const carry = (a, asleep) => { let last = null; return (a || []).map((v, i) => { if (v != null) { last = v; return v; } return asleep[i] ? last : (last = null); }); };
  let first = null;
  async function firstMs() {
    if (first != null) return first;
    try { const pg = await ctx.api.get(`${base}/battery/capacity`, { group: 'month', limit: 1 }); first = pg.items?.[0]?.t_ms ?? now() - 365 * DAY; } catch { first = now() - 365 * DAY; }
    return first;
  }

  /* ---------- capacity scatter and the month table */
  async function paintCapacity() {
    const [pts, med] = await Promise.all([collect(`${base}/battery/capacity`, { group: opt.capGroup, x: opt.capX }), collect(`${base}/battery/capacity`, { group: 'month', x: opt.capX })]);
    await batSettled; // the capacity when new comes with GET battery
    if (!ctx.alive()) return;
    const pk = bat.pack || {};
    const rows = [keyRow('x', [['odometer', 'odometer'], ['time', 'date']], opt.capX, (v) => { opt.capX = v; ctx.setQuery({ cap_x: v === 'time' ? v : null }); paintCapacity(); }),
      keyRow('one dot per', [['charge', 'charge'], ['day', 'day']], opt.capGroup, (v) => { opt.capGroup = v; ctx.setQuery({ cap_group: v === 'day' ? v : null }); paintCapacity(); })];
    paintMonths(med.items);
    const items = pts.items, trend = pts.trend || med.trend;
    if (!items.length) { replace(capSlot, rows, empty('no capacity readings yet', opt.capGroup === 'charge' ? 'a reading needs a finished charge that added at least 10%; try one dot per day' : 'the pack reports capacity while awake; none is stored yet')); return; }
    const xOf = (it) => (opt.capX === 'odometer' ? (it.odometer_km == null ? null : convert(it.odometer_km, 'odometer')) : it.t_ms);
    const P = items.map((it) => ({ x: xOf(it), y: it.usable_kwh, it })).filter((p) => p.x != null && p.y != null);
    const M = med.items.map((it) => ({ x: xOf(it), y: it.usable_kwh })).filter((p) => p.x != null && p.y != null);
    const all = [...P.map((p) => ({ ...p, s: 1 })), ...M.map((p) => ({ ...p, s: 2 }))].sort((a, b) => a.x - b.x);
    const xs = all.map((p) => p.x), y1 = all.map((p) => (p.s === 1 ? p.y : null)), y2 = all.map((p) => (p.s === 2 ? p.y : null));
    const fit = linFit(P);
    const y3 = all.map((p) => (fit ? fit.a + fit.b * p.x : null));
    const newKwh = cap.new_kwh;
    const box = h('div', { class: 'chart is-click' });
    const rd = readout('move over a dot; click a per-charge dot to open that charge');
    const slope = trend?.slope_kwh_per_10000_km, proj = trend?.projection;
    replace(capSlot, rows,
      h('div', { class: 'chart-head' }, h('b', null, 'usable capacity'), h('span', null, 'kWh'), h('span', { class: 'chart-key' },
        h('span', { class: 'chart-key-i chart-key-i--2' }, `. ${opt.capGroup}`), h('span', { class: 'chart-key-i chart-key-i--1' }, '-- monthly median'), h('span', { class: 'chart-key-i chart-key-i--3' }, '- - trend'),
        newKwh != null ? h('span', { class: 'chart-key-i muted' }, `.. new ${format(newKwh, 'energy')}`) : null,
        pk.replaced_ms != null ? h('span', { class: 'chart-key-i muted' }, '| pack replaced; left of it: the previous pack') : null), src(mergedSources(items))),
      box, rd.el,
      note(slope == null ? 'not enough readings for a trend yet.' : slope >= 0 ? 'the pack is not losing capacity at the moment.' : `trend: ${format(Math.abs(slope) / convert(1, 'distance'), 'energy')} less per ${formatNumber(10000)} ${unitLabel('distance')}.`,
        proj ? ` at this rate it reaches the ${pct(proj.warranty_pct)} warranty line at about ${format(proj.odometer_km, 'odometer')}, around ${formatDate(proj.t_ms, tz, { weekday: false })}. a straight line is rough: packs lose more early.` : '',
        ' per charge: energy added over the % it added plus the buffer; per day and the median: the pack’s own reading.'));
    await loadUPlot().catch(() => null);
    if (!ctx.alive() || !window.uPlot) return;
    plot(box, ctx, ({ colors: c }) => ({
      padding: [rpx(6), rpx(8), 0, 0], select: { show: false },
      cursor: { x: true, y: false, drag: { x: false, y: false }, points: { show: false }, dataIdx: (u, si, i) => nearest(u.data[1], i) },
      scales: { x: { time: opt.capX === 'time' }, y: { range: (u, a, b) => { const lo = Math.min(a, newKwh ?? a), hi = Math.max(b, newKwh ?? b); const pad = Math.max(0.5, (hi - lo) * 0.15); return [lo - pad, hi + pad]; } } },
      axes: [opt.capX === 'time' ? timeAxis(tz) : { ...valueAxis((v) => formatNumber(v), { scale: 'x', side: 2, size: 28 }), space: rpx(96) }, valueAxis((v) => formatNumber(v, { maximumFractionDigits: 1 }))],
      series: [{}, { label: 'reading', stroke: c.c2, width: 0, paths: () => null, points: { show: true, space: 0, size: rpx(opt.capGroup === 'day' ? 3 : 5), width: 0, fill: c.c2, stroke: c.c2 } },
        { ...line('median', 0, { width: 2 }), spanGaps: true }, { ...line('trend', 2, { width: 1.5 }), spanGaps: true }],
      hooks: { draw: [(u) => {
        // the day the pack was replaced: readings left of the line are the previous pack's
        const swapX = pk.replaced_ms == null ? null : opt.capX === 'time' ? pk.replaced_ms : P.find((p) => p.it.t_ms >= pk.replaced_ms)?.x;
        if (swapX != null && swapX >= u.scales.x.min && swapX <= u.scales.x.max) {
          const x = Math.round(u.valToPos(swapX, 'x', true)) + 0.5, cx = u.ctx;
          cx.save(); cx.strokeStyle = c.warn || c.muted; cx.lineWidth = 1; cx.beginPath(); cx.moveTo(x, u.bbox.top); cx.lineTo(x, u.bbox.top + u.bbox.height); cx.stroke();
          cx.fillStyle = c.muted; cx.font = `${rpx(10)}px sans-serif`; cx.textAlign = 'right'; cx.fillText('previous pack ', x, u.bbox.top + rpx(12)); cx.restore();
        }
        if (newKwh == null) return;
        const y = Math.round(u.valToPos(newKwh, 'y', true)) + 0.5, cx = u.ctx;
        cx.save(); cx.strokeStyle = c.muted; cx.lineWidth = 1; cx.setLineDash([rpx(2), rpx(3)]);
        cx.beginPath(); cx.moveTo(u.bbox.left, y); cx.lineTo(u.bbox.left + u.bbox.width, y); cx.stroke(); cx.restore();
      }] },
    }), [xs, y1, y2, y3], {
      onCursor: (i) => {
        const p = i == null ? null : all[i];
        rd.show(!p || p.s !== 1 ? null : [['at', formatDate(p.it.t_ms, tz, { weekday: false })], ['odometer', format(p.it.odometer_km, 'odometer')], ['usable', format(p.it.usable_kwh, 'energy')], ['full pack', format(p.it.full_pack_kwh, 'energy')], srcText(p.it) ? ['from', srcText(p.it).replace(/^Data from /, '')] : null, p.it.charge ? ['', 'click to open the charge'] : null].filter(Boolean));
      },
      onBuild: (u) => u.over.addEventListener('click', () => { const i = u.cursor.idx; const it = i != null ? all[i]?.it : null; if (it?.charge) ctx.navigate(state.buildUrl('charge', { car: cid, charge: it.charge })); }),
    });
  }
  // every input the readings came from, SMO first (for the chart's mark)
  function mergedSources(items) {
    const seen = new Map();
    for (const it of items) for (const x of it.sources || []) seen.set(`${x.kind}:${x.site || ''}`, x);
    const order = { smo: 0, tesla_api: 1, import: 2 };
    return { sources: [...seen.values()].sort((a, b) => (order[a.kind] ?? 3) - (order[b.kind] ?? 3)) };
  }
  function paintMonths(items) {
    // health against the same base as the headline figure (usable now over its health), so they agree
    const newKwh = cap.health_pct && cap.usable_kwh ? (cap.usable_kwh * 100) / cap.health_pct : cap.new_kwh;
    const rows = [...items].reverse().map((it) => ({ ...it, health: newKwh && it.usable_kwh != null ? (it.usable_kwh / newKwh) * 100 : null,
      full_km: factor && it.usable_kwh != null ? (it.usable_kwh * 1000) / factor : null }));
    const tb = table(ctx, {
      title: 'by month', rows, csv: false, sort: { key: 'month', dir: 'desc' }, limit: 36,
      note: `the monthly median of the pack’s readings. full range is usable capacity over the ${rangeWord} energy factor now${factor ? ` (${format(factor, 'efficiency', { digits: 1 })})` : ''}.`,
      columns: [
        { key: 'month', label: 'month', sort: true, ph: 'main', value: (r) => r.t_ms, render: (r) => dateFormat(tz, { month: 'short', year: 'numeric' }).format(r.t_ms) },
        { key: 'usable', label: 'usable', num: true, sort: true, ph: 'num', value: (r) => r.usable_kwh, render: (r) => num(r.usable_kwh, 'energy') },
        { key: 'full', label: 'full pack', num: true, render: (r) => num(r.full_pack_kwh, 'energy') },
        { key: 'range', label: 'full range', num: true, sort: true, ph: 'sub', value: (r) => r.full_km, render: (r) => num(r.full_km, 'range') },
        { key: 'health', label: 'health', num: true, sort: true, value: (r) => r.health, render: (r) => (r.health != null ? pct(r.health, 1) : null) },
        { key: 'bar', label: '', grow: true, render: (r) => (r.health != null ? cellBar((r.health - 70) / 30, 20, warranty != null && r.health < warranty ? 'bad' : 'accent') : null) },
        { key: 'odo', label: 'odometer', num: true, render: (r) => num(r.odometer_km, 'odometer') },
        { key: 'from', label: 'data', title: 'where the readings came from: smo, api (the Tesla API), imp (imported)', render: (r) => src(r), value: (r) => srcText(r) },
      ],
      empty: () => empty('no monthly readings yet'),
    });
    replace(monthSlot, tb.el);
  }

  /* ---------- charge level */
  async function paintChargeLevel() {
    const from = period.from_ms ?? (await firstMs());
    const to = period.to_ms ?? now();
    const longSpan = to - from > 62 * DAY;
    const bucket = opt.clBucket && !(opt.clBucket === 'hour' && longSpan) ? opt.clBucket : to - from <= 31 * DAY ? 'hour' : 'day';
    const d = await ctx.api.get(`${base}/battery-level`, { from_ms: from, to_ms: to, bucket, moving: opt.clMoving ? 'true' : 'false', window: opt.clWindow });
    if (!ctx.alive()) return;
    const ctl = h('div', null,
      keyRow('bucket', [['hour', 'hour', longSpan ? 'hour is for ranges up to 62 days' : null], ['day', 'day']], bucket, (v) => { opt.clBucket = v; ctx.setQuery({ cl_bucket: v }); paintChargeLevel(); }, { cover: 'cl.control.bucket_width' }),
      keyRow('moving lines', [[true, 'on'], [false, 'off']], opt.clMoving, (v) => { opt.clMoving = v; ctx.setQuery({ cl_moving: v ? null : '0' }); paintChargeLevel(); }, { cover: 'cl.control.include_moving' }),
      keyRow('window', [['7d', '7 days', opt.clMoving ? null : 'turn the moving lines on'], ['30d', '30 days', opt.clMoving ? null : 'turn the moving lines on']], opt.clWindow, (v) => { opt.clWindow = v; ctx.setQuery({ cl_window: v === '30d' ? v : null }); paintChargeLevel(); }, { cover: 'cl.control.window_width' }));
    const b = d.buckets || [];
    if (!b.some((x) => x.soc_pct != null)) { replace(clSlot, ctl, h('div', { cover: ['cl.chart.battery_level', 'cl.chart.usable_level', 'cl.chart.moving_avg', 'cl.chart.moving_median', 'cl.chart.moving_p075', 'cl.chart.moving_p925', 'cl.chart.legend_stats', 'cl.chart.band'] }, empty('no battery readings in this range', 'pick a longer range'))); return; }
    const ts = b.map((x) => x.t_ms);
    const bms = d.bucket_ms || 3600000;
    const asleepAt = (tt) => (d.spans || []).some((sp) => sp.state === 'asleep' && sp.start_ms <= tt + bms && (sp.end_ms ?? Infinity) >= tt);
    const asl = b.map((x) => x.soc_pct == null && asleepAt(x.t_ms));
    const soc = carry(b.map((x) => x.soc_pct), asl), usable = carry(b.map((x) => x.usable_soc_pct), asl);
    const mv = d.moving || {};
    const limits = [...(d.charge_limit || [])].sort((a, z) => a.t_ms - z.t_ms);
    const lim = ts.map((tt) => { let v = null; for (const l of limits) { if (l.t_ms <= tt + bms) v = l.pct; else break; } return v; });
    const stat = (a) => { const f = (a || []).filter((v) => v != null); return f.length ? { mean: f.reduce((s, v) => s + v, 0) / f.length, min: Math.min(...f), max: Math.max(...f) } : null; };
    const st = d.stats || {};
    if (!d.band) await batSettled;
    if (!ctx.alive()) return;
    const band = d.band || bat.band || {};
    const lo = band.low_pct ?? 20, hi = band.high_pct ?? 80;
    const lines = [
      { k: 'battery', a: soc, cv: 'cl.chart.battery_level', st: { mean: st.mean, min: st.min, max: st.max }, mark: '--' },
      // the usable line stays in the table without readings (its values show as a dash)
      { k: 'usable', a: usable, cv: 'cl.chart.usable_level', st: stat(usable) || {}, mark: '- -' },
      ...(opt.clMoving ? [
        { k: `mean ${opt.clWindow}`, a: mv.mean, cv: 'cl.chart.moving_avg', st: stat(mv.mean), mark: '. .' },
        { k: `median ${opt.clWindow}`, a: mv.median, cv: 'cl.chart.moving_median', st: stat(mv.median), mark: '-.' },
        { k: 'p7.5', a: mv.p7_5, cv: 'cl.chart.moving_p075', st: stat(mv.p7_5), mark: '..' },
        { k: 'p92.5', a: mv.p92_5, cv: 'cl.chart.moving_p925', st: stat(mv.p92_5), mark: '..' },
      ] : []),
      { k: 'limit', a: lim, st: stat(lim), mark: '_|' },
    ];
    const box = h('div', { class: 'chart' });
    const rd = readout('move over the chart for the values at a time');
    const statT = table(ctx, {
      title: null, bare: true, rows: lines.filter((l) => l.st), csv: false, label: 'Charge level lines', wrapCls: 'blk',
      columns: [
        { key: 'k', label: 'line', ph: 'main', grow: true, render: (l) => h('span', { cover: l.cv || null }, h('span', { class: 'muted' }, `${l.mark} `), l.k) },
        { key: 'mean', label: 'average', num: true, ph: 'num', render: (l) => pct(l.st.mean, 1) },
        { key: 'min', label: 'lowest', num: true, ph: 'sub', render: (l) => pct(l.st.min, 1) },
        { key: 'max', label: 'highest', num: true, render: (l) => pct(l.st.max, 1) },
      ],
    });
    statT.el.dataset.cover = 'cl.chart.legend_stats';
    replace(clSlot, ctl,
      h('div', { class: 'chart-head' }, h('b', null, 'charge level'), h('span', null, '%'), h('span', { class: 'chart-key' },
        h('span', { class: 'chart-key-i chart-key-i--1' }, '-- battery'), h('span', { class: 'chart-key-i chart-key-i--2' }, '- - usable'),
        opt.clMoving ? h('span', { class: 'chart-key-i chart-key-i--3' }, '. . moving') : null, h('span', { class: 'chart-key-i tone--warn' }, '_| limit'))),
      box, rd.el,
      kv([
        [`above ${pct(80)}`, [pct(d.share_above_80_pct, 1), none(hi >= 100 ? '  lfp can sit high' : '  long stays high age a nickel pack')]],
        [`below ${pct(20)}`, [pct(d.share_below_20_pct, 1), none('  long stays low are hard on any pack')]],
        ['median level', pct(st.median, 1)],
      ]),
      statT.el,
      h('p', { class: 'note', cover: 'cl.chart.band' }, `shaded band: the everyday range ${pct(lo)}..${pct(hi)}. moving lines use a fixed ${opt.clWindow === '30d' ? '30' : '7'}-day window of daily means ending each day. shares count known time only; unknown time is shaded and breaks the lines.`));
    await loadUPlot().catch(() => null);
    if (!ctx.alive() || !window.uPlot) return;
    const spans = d.spans || [];
    const c0 = colors();
    const series = [{}, line('battery', 0, { width: 2 }), line('usable', 1)];
    if (opt.clMoving) series.push(line('mean', 2), { ...line('median', 2), dash: [rpx(6), rpx(2), rpx(1), rpx(2)] },
      { label: 'p7.5', stroke: c0.muted, width: rpx(1), dash: [rpx(1), rpx(3)], points: { show: false } }, { label: 'p92.5', stroke: c0.muted, width: rpx(1), dash: [rpx(1), rpx(3)], points: { show: false } });
    series.push({ label: 'limit', stroke: c0.warn, width: rpx(1.5), paths: window.uPlot.paths.stepped({ align: 1 }), points: { show: false } });
    const data = [ts, soc, usable];
    if (opt.clMoving) data.push(mv.mean || ts.map(() => null), mv.median || ts.map(() => null), mv.p7_5 || ts.map(() => null), mv.p92_5 || ts.map(() => null));
    data.push(lim);
    plot(box, ctx, ({ colors: c }) => ({
      padding: [rpx(6), rpx(8), 0, 0], select: { show: false }, cursor: { ...cursor(`bat-${cid}`), drag: { x: false, y: false } },
      scales: { x: { time: true, min: from, max: to }, y: { range: [0, 100] } },
      axes: [timeAxis(tz), valueAxis((v) => `${v}%`, { size: 48 })],
      series,
      hooks: { drawClear: [(u) => {
        const cx = u.ctx, { left, top, width, height } = u.bbox;
        cx.save();
        const y0 = Math.round(u.valToPos(hi, 'y', true)), y1 = Math.round(u.valToPos(lo, 'y', true));
        cx.fillStyle = hexA(c.accent, 0.07); cx.fillRect(left, y0, width, y1 - y0);
        for (const s of spans) {
          const a = Math.max(left, u.valToPos(s.start_ms, 'x', true)), e = Math.min(left + width, u.valToPos(s.end_ms ?? now(), 'x', true));
          if (e <= a) continue;
          cx.fillStyle = hexA(c.muted, s.state === 'asleep' ? 0.05 : 0.14); cx.fillRect(a, top, Math.max(1, e - a), height);
        }
        cx.restore();
      }] },
    }), data, { onCursor: (i) => rd.show(i == null ? null : [['at', formatDateTime(ts[i], tz, { now: now(), weekday: false })], ['battery', pct(soc[i], 1)], ['usable', pct(usable[i], 1)],
      opt.clMoving ? ['mean', pct(mv.mean?.[i], 1)] : null, ['limit', pct(lim[i])]].filter(Boolean)) });
  }

  /* ---------- projected range */
  async function paintProjected() {
    const params = { ...ctx.params };
    const span = (period.to_ms ?? now()) - (period.from_ms ?? now() - 365 * DAY);
    const hourTooFine = span > 62 * DAY;
    if (opt.prBucket !== 'auto' && !(opt.prBucket === 'hour' && hourTooFine)) params.bucket = opt.prBucket; else params.max_points = 600;
    const d = await ctx.api.get(`${base}/projected-range`, params);
    if (!ctx.alive()) return;
    const ctl = keyRow('resolution', [['auto', 'auto'], ['hour', 'hour', hourTooFine ? 'hour is for ranges up to 62 days' : null], ['day', 'day'], ['week', 'week']], hourTooFine && opt.prBucket === 'hour' ? 'auto' : opt.prBucket,
      (v) => { opt.prBucket = v; ctx.setQuery({ pr_bucket: v === 'auto' ? null : v }); paintProjected(); }, { cover: 'pr.control.resolution' });
    const v = d.values || {}, ts = d.t_ms || [];
    const allCovers = ['pr.mileage.projected', 'pr.mileage.odometer', 'pr.battery.projected_usable', 'pr.battery.projected_total', 'pr.battery.levels', 'pr.temp.projected', 'pr.temp.outdoor', 'pr.axis.titles', 'pr.control.shared_crosshair'];
    if (!ts.length || !(v.full_range_km || []).some((x) => x != null)) { replace(prSlot, ctl, h('div', { cover: allCovers }, empty('no range readings in this range', 'pick a longer range'))); return; }
    const slept = (d.state || []).map((s) => s === 'asleep');
    const R = (k) => carry((v[k] || []).map((x) => (x == null ? null : convert(x, 'range'))), slept);
    const C = (k, kind) => carry((v[k] || []).map((x) => (x == null ? null : kind ? convert(x, kind) : x)), slept);
    const rU = unitLabel('range');
    const panes = [
      { title: 'against distance driven', cover: ['pr.mileage.projected', 'pr.mileage.odometer', 'pr.axis.titles', 'pr.control.shared_crosshair'], right: unitLabel('odometer'),
        L: [['from battery %', R('full_range_km')], ['from energy', R('full_range_from_energy_km')]], Rt: [['odometer', C('odometer_km', 'odometer')]] },
      { title: 'against battery level', cover: ['pr.battery.projected_usable', 'pr.battery.projected_total', 'pr.battery.levels'], right: '%', pctR: true,
        L: [['from usable %', R('full_range_usable_km')], ['from battery %', R('full_range_km')]], Rt: [['battery % shown', C('soc_pct')], ['pack % (bms)', C('soc_bms_pct')]] },
      { title: 'against temperature', cover: ['pr.temp.projected', 'pr.temp.outdoor'], right: unitLabel('temperature'),
        L: [['from battery %', R('full_range_km')], ['from usable %', R('full_range_usable_km')]], Rt: [['outside', C('outside_c', 'temperature')], ['battery', C('battery_c', 'temperature')]] },
    ];
    const bucketText = d.bucket_ms ? (d.bucket_ms >= 7 * DAY ? 'week' : d.bucket_ms >= DAY ? 'day' : d.bucket_ms >= 3600000 ? `${formatNumber(d.bucket_ms / 3600000, { maximumFractionDigits: 0 })} h` : `${formatNumber(d.bucket_ms / 60000, { maximumFractionDigits: 0 })} min`) : '';
    const blocks = panes.map((p) => ({ p, box: h('div', { class: 'chart chart--pane' }), rd: readout(' ') }));
    replace(prSlot, ctl, blocks.map(({ p, box, rd }) => h('div', { class: 'blk', cover: p.cover },
      h('div', { class: 'chart-head' }, h('b', null, p.title), h('span', null, `left ${rU}, right ${p.right}`), h('span', { class: 'chart-key' },
        h('span', { class: 'chart-key-i chart-key-i--1' }, `-- ${p.L[0][0]}`), h('span', { class: 'chart-key-i chart-key-i--2' }, `- - ${p.L[1][0]}`),
        p.Rt.map(([n], k) => h('span', { class: 'chart-key-i chart-key-i--3' }, `${k ? '.. ' : '. . '}${n}`)))),
      box, rd.el)),
    note(`each point is one ${bucketText}: the range the car would show at 100%, from its range and battery % (or usable %, or energy). the three charts share one cursor; asleep time is shaded light, unknown darker and the lines break there.`));
    await loadUPlot().catch(() => null);
    if (!ctx.alive() || !window.uPlot) return;
    const st = d.state || [];
    for (const { p, box, rd } of blocks) {
      plot(box, ctx, ({ colors: c }) => ({
        padding: [rpx(6), 0, 0, 0], select: { show: false }, cursor: { ...cursor(`pr-${cid}`), drag: { x: false, y: false } },
        scales: { x: { time: true }, y: { range: (u, a, z) => { const pad = Math.max(2, (z - a) * 0.12); return [a - pad, z + pad]; } }, o: p.pctR ? { range: [0, 100] } : { auto: true } },
        axes: [timeAxis(tz), valueAxis((x) => formatNumber(x), { size: 56 }), valueAxis((x) => formatNumber(x), { scale: 'o', side: 1, grid: false, size: 64 })],
        series: [{}, line(p.L[0][0], 0, { width: 2 }), line(p.L[1][0], 1),
          ...p.Rt.map(([n], k) => ({ ...line(n, 2, { scale: 'o', width: 1.25 }), dash: k ? [rpx(1), rpx(2)] : [rpx(2), rpx(3)] }))],
        hooks: { drawClear: [(u) => {
          const cx = u.ctx, { top, height } = u.bbox;
          cx.save();
          for (let i = 0; i < ts.length; i++) {
            if (st[i] !== 'asleep' && st[i] !== 'unknown') continue;
            const x0 = u.valToPos(ts[i], 'x', true), x1 = u.valToPos(ts[i + 1] ?? ts[i] + (d.bucket_ms || 3600000), 'x', true);
            cx.fillStyle = hexA(c.muted, st[i] === 'asleep' ? 0.05 : 0.14); cx.fillRect(x0, top, Math.max(1, x1 - x0), height);
          }
          cx.restore();
        }] },
      }), [ts, p.L[0][1], p.L[1][1], ...p.Rt.map(([, a]) => a)], { onCursor: (i) => rd.show(i == null ? null : [['at', (d.bucket_ms || DAY) < DAY ? formatDateTime(ts[i], tz, { now: now(), weekday: false }) : formatDate(ts[i], tz, { weekday: false })],
        ...p.L.map(([n, a]) => [n, a[i] == null ? '—' : `${formatNumber(a[i], { maximumFractionDigits: 0 })} ${rU}`]), ...p.Rt.map(([n, a]) => [n, a[i] == null ? '—' : `${formatNumber(a[i], { maximumFractionDigits: 1 })} ${p.right}`])]) });
    }
  }

  /* ---------- parked drain */
  async function paintParked() {
    const params = () => ({ ...ctx.params, ...(opt.minH ? { min_hours: opt.minH } : {}) });
    const sumEl = h('div');
    const tripHref = (r) => state.buildUrl('trip', { car: cid, period: { from: r.start_ms, to: r.end_ms ?? now() } });
    const tlHref = (r) => state.buildUrl('timeline', { car: cid, period: { from: startOf(r.start_ms, 'day', tz), to: r.end_ms ?? now() } });
    const standby = (r) => {
      const d = r.duration_ms || 1;
      const a = (r.asleep_ms || 0) / d, u = (r.unknown_ms || 0) / d;
      const n = 10, na = Math.round(a * n), nu = Math.min(n - na, Math.round(u * n));
      return h('span', { title: `${formatDuration(r.asleep_ms || 0)} asleep, ${formatDuration((r.awake_ms || 0) + (r.awake_vehicle_bus_only_ms || 0))} awake${r.unknown_ms ? `, ${formatDuration(r.unknown_ms)} unknown` : ''}` },
        h('span', { class: 'tbar' }, h('span', { class: 'muted' }, 'z'.repeat(na)), h('span', { class: 'tone--accent' }, '#'.repeat(n - na - nu)), h('span', { class: 'tone--bad' }, '?'.repeat(nu))), ' ', pct(a * 100));
    };
    const t = table(ctx, {
      title: 'parked drain', path: `${base}/parked`, params: params(), csvName: 'parked drain', cover: ['vd.table', 'shots.vampire-drain'],
      note: 'each stay parked off a charger at one spot. energy is measured at the pack, so small losses show that whole % steps hide. standby: z asleep, # awake, ? unknown (never counted as asleep).',
      columns: [
        { key: 'start', label: 'parked', ph: 'main', cover: ['vd.col.start', 'vd.link.start_details'], render: (r) => [h('a', { class: 'lnk-plain', href: tripHref(r), title: 'this span as a trip' }, formatDateTime(r.start_ms, tz, { now: now(), weekday: false })), none(` ${r.place?.name || r.address || ''}`)] },
        { key: 'end', label: 'until', cover: 'vd.col.end', render: (r) => (r.end_ms == null ? tag('parked', 'accent') : h('a', { class: 'lnk-plain', href: tlHref(r) }, startOf(r.end_ms, 'day', tz) === startOf(r.start_ms, 'day', tz) ? formatTime(r.end_ms, tz) : formatDateTime(r.end_ms, tz, { now: now(), weekday: false }))) },
        { key: 'len', label: 'length', num: true, ph: 'sub', cover: 'vd.col.period', render: (r) => formatDuration(r.duration_ms) },
        { key: 'standby', label: 'standby', cover: 'vd.col.standby', render: standby },
        { key: 'energy', label: 'energy', num: true, ph: 'num', cover: 'vd.col.energy', render: (r) => [num(r.energy_kwh, 'energy'), r.energy_method && r.energy_method !== 'counter' ? none(` ${METHOD[r.energy_method] || r.energy_method}`) : null] },
        { key: 'power', label: 'avg power', num: true, cover: 'vd.col.avg_power', render: (r) => num(r.avg_power_w, 'power_w') },
        { key: 'soc', label: 'battery', num: true, cover: 'vd.col.soc_diff', render: (r) => (r.soc_start_pct == null || r.soc_end_pct == null ? null : [`-${formatNumber(Math.max(0, r.soc_start_pct - r.soc_end_pct), { maximumFractionDigits: 1 })}%`, none(` ${pct(r.soc_start_pct)}->${pct(r.soc_end_pct)}`)]) },
        { key: 'range', label: 'range lost', num: true, cover: 'vd.col.range_loss', render: (r) => num(r.range_lost_km, 'range', { digits: 1 }) },
        { key: 'rph', label: '/hour', num: true, cover: 'vd.col.range_per_hour', render: (r) => num(r.range_lost_per_hour_km, 'range_rate', { digits: 2 }) },
        { key: 'why', label: 'awake by', grow: true, cover: 'vd.flag.cold', render: (r) => [r.cold ? [tag('cold', 'warn'), ' '] : null,
          (r.awake_reasons || []).length ? (r.awake_reasons || []).map((x) => REASONS[x] || x).join(', ') : none(r.awake_ms > 0 ? 'no reason sent' : 'slept')] },
        { key: 'from', label: 'data', title: 'where the data came from: smo, api (the Tesla API), imp (imported)', render: (r) => src(r), value: (r) => srcText(r) },
      ],
      empty: () => empty('no parked spells in this range', opt.minH ? `none was at least ${opt.minH} h; pick any length or a longer range` : 'pick a longer range'),
      onPage: (pg) => { const s = pg.summary; if (s) replace(sumEl, kv([
        ['spells', [n0(s.spells), none(`  ${formatNumber(s.hours ?? 0, { maximumFractionDigits: 0 })} h parked off a charger`)]],
        ['energy lost', num(s.energy_kwh, 'energy')],
        ['per day parked', num(s.kwh_per_day, 'energy')],
        ['cost', s.cost?.amount != null ? [formatMoney(s.cost.amount, s.cost.currency), none(`  at the ${s.cost.basis || 'home price'}`)] : none('set a home price on places')],
      ])); },
    });
    const minRow = keyRow('shortest', [0, 1, 3, 6, 12, 24].map((x) => [x, x ? `${x} h` : 'any']), opt.minH, (x) => { opt.minH = x; ctx.setQuery({ min_h: x || null }); for (const b of minRow.querySelectorAll('.key')) b.setAttribute('aria-pressed', String(b.textContent === (x ? `${x} h` : 'any'))); t.reload(params()); }, { cover: 'vd.filter.min_idle' });
    replace(vdSlot, h('div', null, sumEl, minRow, t.el));
    ctx.events('rows', (ev) => { if (ev.data?.car === cid && (ev.data.kinds || []).some((k) => k === 'parked' || k === 'drive' || k === 'charge')) t.reload(params(), { quiet: true }); });
    await t.ready;
  }

  const guard = (slot, fn) => fn().catch((e) => { if (ctx.alive()) replace(slot, errorBlock(e, { retry: () => guard(slot, fn), home: false })); });
  // GET battery failing fails the page (as before); the other blocks show their own errors
  await Promise.all([paintTop(), paintLife(), paintRef(), guard(capSlot, paintCapacity), guard(clSlot, paintChargeLevel), guard(prSlot, paintProjected), guard(vdSlot, paintParked)]);
  if (!ctx.alive()) return;
  ctx.ready();
}

function nearest(arr, i) {
  if (!arr || arr[i] != null) return i;
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
