// Battery (/cars/{car}/battery): the pack as an instrument cluster. Now: a fuel gauge of the charge with
// the everyday band and the charge limit. Health: a dial of health against new with the warranty line, the
// capacities and the range at 100 %. Lifetime: an odometer, trip counters and where the energy came from.
// Reference values (admin, PATCH .../settings). The capacity over the car's life (GET .../battery/capacity).
// Then, for the period: charge level (GET .../battery-level), projected full-charge range on three synced
// charts (GET .../projected-range) and the parked drain (GET .../parked). Asleep is not unknown: asleep time
// is shaded blue and lines go on through it; unknown time is grey and lines break there. "now" events move
// the Now panel; "rows" events reload the parked list.

import * as state from '../shared/state.js';
import { h, replace, cssVar } from '../shared/dom.js';
import { column, timeColumn } from '../shared/csv.js';
import { format, formatMoney, convert, toMetric, unitLabel, getUnits } from '../shared/units.js';
import { formatDate, formatDateTime, formatTime, formatDuration, startOf, DAY } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon, hud, plate, seg, note, tag, kv, bar, linkBtn } from '../core/ui.js';
import { controls, slot, periodBtn, carBtn, select, counters, csvBtn, band, plural } from '../core/kit.js';
import { srcChip, sourceLabel } from '../core/sources.js';
import { dial, fuel, odo } from '../core/gauges.js';
import { tracks } from '../core/tracks.js';
import { loadUPlot, mountPlot, scope, timeAxis, valueAxis, tone, alpha, well } from '../core/chart.js';
import { rpx } from '../core/display.js';
import { emptyState, errorState } from '../core/states.js';
import { field, form } from '../core/form.js';
import { toast } from '../core/layer.js';
import { val } from '../core/fmt.js';

export const id = 'battery';
export const css = true;

const enc = encodeURIComponent;
const pct = (v, digits = 0) => format(v, 'percent', { digits });
const NEW_SOURCE = { owner: 'typed by the owner', early_bms: 'the pack\'s readings in its first 30 days', imported_charges: 'the earliest imported charges', pack_code: 'the pack-size code (a hint, not a measurement)' };
const RANGE_SOURCE = { owner: 'typed by the owner', first_30_days: 'the first 30 days', pack_code: 'the pack-size code (a hint)' };
const NOTES = {
  new_value_typed: 'The "when new" values are the ones typed under Reference values.',
  new_value_from_early_readings: 'Nothing was typed, so "when new" is the median of the earliest pack readings. Type the real figure if you know it.',
  new_value_from_imported_charges: 'The car was logged late, so "when new" comes from the earliest imported charges.',
  new_value_from_pack_code: 'There are no early readings, so "when new" is only the pack-size code. Type the real figure for a true health figure.',
  cold_battery_locks_part: 'The pack is cold: part of the energy is locked until it warms up, so usable is below battery %.',
  lfp_charge_to_100_weekly: 'LFP pack: charge to 100 % about once a week so the car can recalibrate its battery %.',
  imported_history_estimated: 'Part of the history is imported; figures from it are estimated.',
  no_history: 'There is no history for this car yet.',
};
const CHEM = { lfp: 'LFP (iron phosphate)', nca_nmc: 'NCA or NMC (nickel)' };
const REASONS = { sentry: 'Sentry', keep_climate_on: 'Climate on', keep_climate_dog: 'Dog mode', keep_climate_camp: 'Camp mode', user_present: 'Someone near', preconditioning_cabin: 'Cabin warm-up', preconditioning_battery: 'Pack warm-up', charging: 'Charging', keep_warm: 'Keeping warm', door_open: 'Door open', accessory_power: 'Accessory power' };
const METHOD = { counter: 'pack counters', bms_energy: 'pack energy reading', range: 'range change (estimated)' };

export async function mount(ctx) {
  const { car, tz, period } = ctx;
  const cid = car.id, base = `cars/${enc(cid)}`, q = ctx.query;
  const now = () => ctx.api.now();
  const opt = {
    capX: q.cap_x === 'odometer' ? 'odometer' : 'time',
    capGroup: q.cap_group === 'day' ? 'day' : 'charge',
    clBucket: q.cl_bucket === 'hour' || q.cl_bucket === 'day' ? q.cl_bucket : null,
    clMoving: q.cl_moving !== '0',
    clWindow: q.cl_window === '30d' ? '30d' : '7d',
    prBucket: ['hour', 'day', 'week'].includes(q.pr_bucket) ? q.pr_bucket : 'auto',
    minH: [0, 1, 3, 6, 12, 24].includes(Number(q.min_h)) ? Number(q.min_h) : 0,
  };
  const [bat, totals, settings, eff] = await Promise.all([
    ctx.api.get(`${base}/battery`),
    ctx.api.get(`${base}/totals`).catch(() => null),
    ctx.api.get(`${base}/settings`).catch(() => null),
    ctx.api.get(`${base}/efficiency`).catch(() => null),
  ]);
  if (!ctx.alive()) return;
  const cap = bat.capacity || {}, life = bat.lifetime || {};
  const fr = eff?.factor || {};
  const toIdeal = getUnits().range === 'ideal' && fr.rated_wh_per_km > 0 && fr.ideal_wh_per_km > 0 ? fr.rated_wh_per_km / fr.ideal_wh_per_km : null;
  const rangeWord = toIdeal ? 'ideal' : 'rated';
  const rk = (km) => (km == null ? km : toIdeal ? km * toIdeal : km);
  const rng = { ...(bat.range || {}) };
  for (const k of ['full_now_km', 'full_now_from_energy_km', 'full_new_km', 'lost_km']) rng[k] = rk(rng[k]);

  /* ---------- Now: the fuel gauge */
  const nowEl = h('div');
  function paintNow(live = {}) {
    const soc = live.soc ?? bat.soc_pct, usable = live.usable ?? bat.usable_soc_pct, kwh = live.kwh ?? bat.energy_now_kwh;
    const b = bat.band || {};
    const lim = live.limit ?? b.charge_limit_pct;
    const lo = b.low_pct ?? 20, hi = b.high_pct ?? 80;
    const locked = soc != null && usable != null && soc - usable >= 0.5;
    replace(nowEl,
      h('div', { class: 'bt-now' },
        h('div', { class: 'bt-soc', cover: 'bh.soc.current' },
          h('span', { class: 'stat-label' }, h('span', null, 'Battery')),
          h('span', { class: 'bt-soc-v' }, soc == null ? '—' : formatNumber(soc, { maximumFractionDigits: 0 }), h('span', { class: 'bt-soc-u' }, '%')),
          h('span', { class: 'stat-how' }, soc == null ? 'No reading yet.' : 'as the car shows it')),
        h('div', { class: 'bt-fuel', cover: 'bh.soc.band' },
          fuel({ fraction: soc == null ? null : soc / 100, cells: 20, low: 0.2, ends: ['0', '100 %'], label: 'Battery' }),
          h('div', { class: 'bt-band', 'aria-hidden': 'true', vars: { '--lo': `${lo}%`, '--hi': `${hi}%`, '--lim': `${lim ?? 0}%` } },
            h('i', { class: 'bt-band-run' }), lim != null ? h('i', { class: 'bt-band-lim' }) : null),
          h('p', { class: 'bt-band-key' },
            h('span', null, h('i', { class: 'bt-k bt-k--band' }), `Everyday ${pct(lo)} to ${pct(hi)}${hi >= 100 ? ' (LFP: full is fine)' : ''}`),
            lim != null ? h('span', null, h('i', { class: 'bt-k bt-k--lim' }), `Charge limit ${pct(lim)}`) : h('span', { class: 'faint' }, 'No charge limit sent yet')))),
      counters([
        { label: 'Usable', value: usable, kind: 'percent', how: locked ? 'a cold pack locks part of it' : 'what the car can use now' },
        { label: 'Stored energy', value: kwh, kind: 'energy', cover: 'bh.bar.stored_energy', how: cap.usable_kwh ? `of ${format(cap.usable_kwh, 'energy')} when full` : 'usable energy now' },
        { label: 'Range now', value: soc != null && rng.full_now_km != null ? (rng.full_now_km * soc) / 100 : null, kind: 'range', how: `${rangeWord}, full range times battery %` },
        { label: 'To the limit', value: lim != null && kwh != null && cap.usable_kwh ? Math.max(0, (cap.usable_kwh * lim) / 100 - kwh) : null, kind: 'energy', how: 'energy still to add' },
        { label: 'Buffer', value: cap.buffer_kwh, kind: 'energy', how: 'below 0 %, never usable' },
      ]));
  }
  paintNow();
  ctx.events('now', (ev) => {
    if (ev.data?.car !== cid) return;
    const d = ev.data.now || {};
    const live = Object.fromEntries(Object.entries({ soc: val(d.soc_pct), usable: val(d.usable_soc_pct), kwh: val(d.energy_remaining_kwh), limit: val(d.charge?.limit_pct) }).filter(([, x]) => x != null));
    if (Object.keys(live).length) paintNow(live);
  });

  /* ---------- health: the dial */
  const health = cap.health_pct, warranty = cap.warranty_pct;
  const factor = eff?.factor ? (toIdeal ? fr.ideal_wh_per_km : fr.rated_wh_per_km) : null;
  const notesEl = (bat.notes || []).filter((c) => NOTES[c]).map((c) => h('p', { class: ['note', /cold|lfp/.test(c) && 'note--warn'] }, icon(/cold|lfp/.test(c) ? 'warn' : 'info', 'i--sm'), h('span', null, NOTES[c])));
  const healthHud = hud({ title: 'Capacity and health', icon: 'battery', tone: 'cyan', cover: 'shots.battery-health' },
    h('div', { class: 'bt-health' },
      h('div', { class: 'bt-dial', cover: ['bh.bar.health', 'bh.gauge.degradation'] },
        dial({ value: health, max: 100, kind: 'percent', label: 'Health', ticks: 10, warm: health != null && warranty != null && health < warranty, why: 'Not known' }),
        h('p', { class: 'bt-dial-sub' }, cap.degradation_pct == null ? 'Lost: not known' : `${format(cap.degradation_pct, 'percent', { digits: 1 })} lost since new`),
        warranty != null ? h('p', { class: 'bt-dial-sub faint' }, `Warranty line at ${pct(warranty)}`) : null),
      h('div', { class: 'stack' },
        counters([
          { label: 'Usable now', value: cap.usable_kwh, kind: 'energy', cover: 'bh.capacity.usable_now', how: cap.full_pack_kwh != null && cap.buffer_kwh != null ? `full pack ${format(cap.full_pack_kwh, 'energy')} less the buffer` : `${plural(cap.readings || 0, 'pack reading')}` },
          { label: 'Usable when new', value: cap.new_kwh, kind: 'energy', cover: 'bh.capacity.usable_new', how: cap.new_source ? NEW_SOURCE[cap.new_source] || cap.new_source : 'type it under Reference values' },
          { label: 'Difference', text: cap.difference_kwh == null ? '—' : format(-cap.difference_kwh, 'energy', { sign: true }), cover: 'bh.capacity.difference', how: 'now less when new' },
          { label: `Per ${unitLabel('range')} of range`, value: factor, kind: 'efficiency', cover: 'bh.stat.efficiency', how: eff?.factor ? `${rangeWord}, from ${plural(eff.factor.readings || 0, 'reading')}` : 'not worked out yet' },
          { label: 'Cell spread', text: bat.brick_spread_v == null ? '—' : `${formatNumber(bat.brick_spread_v * 1000, { maximumFractionDigits: 0 })} mV`, how: 'under 30 mV is well balanced' },
        ]),
        h('div', { class: 'bt-range' },
          counters([
            { label: 'At 100 % now', value: rng.full_now_km, kind: 'range', cover: 'bh.ranges.max_now', how: `from energy ${format(rng.full_now_from_energy_km, 'range')}` },
            { label: 'At 100 % new', value: rng.full_new_km, kind: 'range', cover: 'bh.ranges.max_new', how: rng.new_source ? RANGE_SOURCE[rng.new_source] || rng.new_source : 'type it under Reference values' },
            { label: 'Range lost', value: rng.lost_km, kind: 'range', cover: 'bh.ranges.lost', how: rng.lost_km != null && rng.full_new_km ? `${format((rng.lost_km / rng.full_new_km) * 100, 'percent', { digits: 1 })} of new` : 'new less now' },
          ]),
          bar(rng.full_new_km && rng.full_now_km ? rng.full_now_km / rng.full_new_km : 0, { label: 'Range now against range when new' })))),
    h('div', { class: 'bt-help', cover: ['bh.capacity.help', 'bh.gauge.degradation_help'] },
      note('Health is usable now as a share of usable when new. Both come from the battery management system, which reports the pack\'s capacity every awake minute; nothing is worked out from range. It moves a little with temperature and with how long since the pack last balanced.'),
      notesEl));

  /* ---------- lifetime */
  const t = totals || {};
  const mileage = t.odometer_last_km != null && t.odometer_first_km != null ? t.odometer_last_km - t.odometer_first_km : null;
  const inKwh = (life.ac_kwh || 0) + (life.dc_kwh || 0) + (life.regen_kwh || 0);
  const parts = [['AC charging', life.ac_kwh, 'ac'], ['DC fast charging', life.dc_kwh, 'dc'], ['Regen braking', life.regen_kwh, 'regen']];
  const share = (k) => (inKwh ? ((k || 0) / inKwh) * 100 : 0);
  const lifeHud = hud({ title: 'Lifetime', icon: 'gauge', tone: 'orange', side: csvBtn({ name: [car.name, 'battery-lifetime'], rows: () => [...parts, ['Driving (out)', life.drive_kwh], ['Parked (out)', life.standing_kwh]].map(([s, k]) => ({ source: s, kwh: k })), columns: [column('Source', 'source'), column('Energy', 'kwh', 'energy')] }) },
    h('div', { class: 'bt-odo', cover: 'bh.drive.odometer' }, h('span', { class: 'stat-label' }, h('span', null, 'Odometer')), odo(t.odometer_last_km == null ? '—' : formatNumber(convert(t.odometer_last_km, 'odometer'), { maximumFractionDigits: 0, useGrouping: false })), h('span', { class: 'bt-odo-u' }, unitLabel('odometer'))),
    h('div', { class: 'cols bt-life' },
      h('div', { class: 'stack' },
        h('h3', { class: 'bt-sub' }, icon('route', 'i--sm'), h('span', null, 'Driving'), h('a', { class: 'plc', href: ctx.link('statistics'), cover: 'bh.drive.link_stats' }, 'Statistics')),
        kv([
          [h('span', { cover: 'bh.drive.mileage' }, 'Driven since logging began'), format(mileage, 'distance')],
          [h('span', { cover: 'bh.drive.logged' }, 'Logged in drives'), h('a', { class: 'plc', href: ctx.link('drives', { period: { preset: 'all' } }) }, `${format(t.distance_km, 'distance')} · ${plural(t.drives || 0, 'drive')}`)],
          [h('span', { cover: 'bh.drive.data_lost' }, 'Not logged'), t.not_logged_km ? h('a', { class: ['plc', t.not_logged_km > 1 && 'warn'], href: state.buildUrl('health', { car: cid, period: { preset: 'all' } }) }, `${format(t.not_logged_km, 'distance')} · see the gaps`) : format(t.not_logged_km, 'distance')],
        ]),
        h('p', { class: 'note', cover: 'bh.drive.help' }, icon('info', 'i--sm'), h('span', null, 'Not logged is distance the odometer moved while nothing was recorded. Each gap is listed on Data health.'))),
      h('div', { class: 'stack' },
        h('h3', { class: 'bt-sub' }, icon('bolt', 'i--sm'), h('span', null, 'Charging'), h('a', { class: 'plc', href: ctx.link('charges', { period: { preset: 'all' } }), cover: 'bh.charging.link_stats' }, 'Charges')),
        kv([
          [h('span', { cover: 'bh.charging.count' }, 'Sessions'), `${formatNumber(t.charges ?? 0)} (AC ${formatNumber(t.ac?.charges ?? 0)}, DC ${formatNumber(t.dc?.charges ?? 0)})`],
          [h('span', { cover: 'bh.charging.energy_added' }, 'Added to the pack'), format(t.energy_added_kwh, 'energy')],
          [h('span', { cover: 'bh.charging.energy_used' }, 'From the grid'), `${format(t.energy_grid_kwh, 'energy')}${t.grid_coverage_pct != null && t.grid_coverage_pct < 100 ? ` (measured for ${pct(t.grid_coverage_pct)})` : ''}`],
          [h('span', { cover: 'bh.charging.efficiency' }, 'Charging efficiency'), t.charging_efficiency_pct == null ? 'not measured' : pct(t.charging_efficiency_pct, 1)],
          [h('span', { cover: 'bh.charging.cycles' }, 'Full cycles'), life.equivalent_cycles == null ? '—' : formatNumber(life.equivalent_cycles, { maximumFractionDigits: 1 })],
        ]),
        h('p', { class: 'note', cover: 'bh.charging.help' }, icon('info', 'i--sm'), h('span', null, 'Efficiency is energy into the pack over energy from the grid, for AC charges where both were measured; DC chargers do not send their grid side, so they are left out. A full cycle is one usable pack of charging.')))),
    h('div', { class: 'bt-acdc', cover: 'bh.chart.ac_dc' },
      h('span', { class: 'stat-label' }, h('span', null, 'Energy into the pack, by source')),
      h('div', { class: 'bt-acdc-bar', role: 'img', 'aria-label': parts.map(([l, k]) => `${l} ${formatNumber(share(k), { maximumFractionDigits: 0 })} %`).join(', ') },
        parts.map(([l, k, c]) => h('i', { class: `bt-p-${c}`, vars: { '--w': `${share(k).toFixed(2)}%` }, title: `${l}: ${format(k, 'energy')}` }))),
      h('div', { class: 'bt-acdc-key' }, parts.map(([l, k, c]) => h('span', null, h('i', { class: ['bt-k', `bt-p-${c}`] }), `${l} `, h('b', null, `${formatNumber(share(k), { maximumFractionDigits: 0 })} %`), h('span', { class: 'faint' }, ` · ${format(k, 'energy')}`)))),
      note(`From the pack's own lifetime counters, so years before logging count too. Out of the pack: ${format(life.drive_kwh, 'energy')} driving, ${format(life.standing_kwh, 'energy')} while parked.`)));

  /* ---------- reference values */
  const can = !!ctx.admin && !!settings;
  const s = settings || {};
  const capF = field({ label: 'Usable capacity when new (kWh)', name: 'capacity_new_kwh', type: 'number', value: s.capacity_new_kwh, path: 'capacity_new_kwh', hint: 'Leave empty to use the earliest readings. 10 to 250 kWh.', attrs: { min: 10, max: 250, step: 0.1, inputmode: 'decimal', disabled: !can } });
  const rngF = field({ label: `Full-charge range when new (${unitLabel('range')})`, name: 'full_range_new_km', type: 'number', value: s.full_range_new_km == null ? null : Math.round(convert(s.full_range_new_km, 'range')), path: 'full_range_new_km', hint: 'Leave empty to use the first 30 days.', attrs: { min: 1, step: 1, inputmode: 'numeric', disabled: !can } });
  const warF = field({ label: 'Warranty threshold (% health)', name: 'warranty_threshold_pct', type: 'number', value: s.warranty_threshold_pct, path: 'warranty_threshold_pct', hint: 'The health the battery warranty promises, often 70 %.', attrs: { min: 50, max: 100, step: 1, inputmode: 'numeric', disabled: !can } });
  let version = s.version;
  const refForm = form({ fields: [capF, rngF, warF], submit: 'Save reference values', async onSubmit(v) {
    if (!can) return;
    const num = (x) => (x === '' || x == null ? null : Number(x));
    const rn = num(v.full_range_new_km);
    const r = await ctx.api.patch(`${base}/settings`, { version, capacity_new_kwh: num(v.capacity_new_kwh), full_range_new_km: rn == null ? null : Math.round(toMetric(rn, 'range') * 10) / 10, warranty_threshold_pct: num(v.warranty_threshold_pct) });
    version = r.version;
    toast('Reference values saved. Health and range use them now.', { next: true });
    ctx.reload();
  } });
  if (!can) refForm.submitButton.disabled = true;
  capF.el.setAttribute('data-cover', 'bh.setting.capacity_new_override');
  rngF.el.setAttribute('data-cover', 'bh.setting.range_new_override');
  const refHud = hud({ title: 'Reference values', icon: 'sliders', cls: 'bt-ref', side: linkBtn('Car settings', state.buildUrl('settings-cars', { car: cid }), { sm: true, kind: 'ghost', icon: 'car' }) },
    h('p', { class: 'note', cover: 'bh.setting.override_help' }, icon('info', 'i--sm'), h('span', null, 'Typed once for this car and checked when saved. Use the figures from the car\'s papers or its first weeks: they set what "when new" means for health and range lost.')),
    refForm.el,
    !settings ? h('p', { class: 'note note--warn' }, icon('warn', 'i--sm'), h('span', null, 'The car settings could not be read, so they cannot be changed here.')) : !ctx.admin ? note('Only an admin can change these.') : null,
    bat.chemistry ? note(`Chemistry: ${CHEM[bat.chemistry.value] || 'not known'}, ${bat.chemistry.source === 'owner' ? 'set by the owner' : 'detected from the pack'}. Change it in Car settings.`) : null);

  /* ---------- capacity over the car's life */
  const capSide = h('span', { class: 'row' });
  const capEl = h('div', { class: 'stack' });
  const capHud = hud({ title: 'Capacity over the car\'s life', icon: 'stats', side: capSide, cover: ['bh.chart.capacity_by_distance.points', 'bh.chart.capacity_by_distance.median'] }, capEl);
  async function collect(path, params) {
    const items = []; let trend = null, after = null;
    for (let k = 0; k < 20; k++) {
      const pg = await ctx.api.get(path, { ...params, limit: 500, ...(after ? { after } : {}) });
      items.push(...(pg.items || [])); trend = trend || pg.trend;
      if (!pg.next) break; after = pg.next;
    }
    return { items, trend };
  }
  async function paintCapacity() {
    const sX = seg([['time', 'By date'], ['odometer', 'By distance']], opt.capX, (v) => { opt.capX = v; ctx.setQuery({ cap_x: v === 'odometer' ? v : null }); paintCapacity(); }, { label: 'Capacity against' });
    const sG = seg([['charge', 'Per charge'], ['day', 'Per day']], opt.capGroup, (v) => { opt.capGroup = v; ctx.setQuery({ cap_group: v === 'day' ? v : null }); paintCapacity(); }, { label: 'One reading' });
    const [pts, med] = await Promise.all([collect(`${base}/battery/capacity`, { group: opt.capGroup, x: opt.capX }), ctx.api.get(`${base}/battery/capacity`, { group: 'month', x: opt.capX, limit: 500 })]);
    if (!ctx.alive()) return;
    const items = pts.items, trend = pts.trend || med.trend;
    const capSrc = [...new Map(items.flatMap((it) => it.sources || []).map((x) => [`${x.kind}:${x.site || ''}`, x])).values()];
    replace(capSide, capSrc.length ? srcChip({ sources: capSrc }) : null, sX.el, sG.el, csvBtn({ name: [car.name, 'battery-capacity', opt.capGroup], rows: items, columns: [timeColumn('Time', 't_ms', tz), column('Odometer', 'odometer_km', 'odometer'), column('Full pack', 'full_pack_kwh', 'energy'), column('Usable', 'usable_kwh', 'energy'), column('Method', 'method'), column('Charge', 'charge'), { label: 'Source', value: (r) => sourceLabel(r) }] }));
    if (!items.length) { replace(capEl, emptyState({ icon: 'battery', title: 'No capacity readings yet', text: opt.capGroup === 'charge' ? 'A reading needs a finished charge that added at least 10 %. Try Per day for the pack\'s own readings.' : 'The pack reports its capacity while the car is awake; none is stored yet.' })); return; }
    const xOf = (it) => (opt.capX === 'odometer' ? (it.odometer_km == null ? null : convert(it.odometer_km, 'odometer')) : it.t_ms / 1000);
    const P = items.map((it) => ({ x: xOf(it), y: it.usable_kwh, it })).filter((p) => p.x != null && p.y != null);
    const M = (med.items || []).map((it) => ({ x: xOf(it), y: it.usable_kwh })).filter((p) => p.x != null && p.y != null);
    const all = [...P.map((p) => ({ ...p, s: 1 })), ...M.map((p) => ({ ...p, s: 2 }))].sort((a, b) => a.x - b.x);
    const xs = all.map((p) => p.x), y1 = all.map((p) => (p.s === 1 ? p.y : null)), y2 = all.map((p) => (p.s === 2 ? p.y : null));
    const read = h('span', { class: 'scope-read' });
    const box = well('well--lg', 'Usable capacity over the car\'s life');
    const slope = trend?.slope_kwh_per_10000_km, proj = trend?.projection;
    replace(capEl,
      h('div', { class: 'scope-head' }, h('span', { class: 'scope-title' }, 'Usable · kWh'),
        h('span', { class: 'keys' }, h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': 'var(--mag)' } }), opt.capGroup === 'charge' ? 'each charge' : 'each day'), h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': 'var(--cyan)' } }), 'monthly median'),
          cap.new_kwh != null ? h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': 'var(--ink-3)' } }), `when new ${format(cap.new_kwh, 'energy')}`) : null), read),
      box,
      note(slope == null ? 'Not enough readings for a trend yet.' : slope >= 0 ? 'The pack is not losing capacity at the moment.' : `Trend: ${format(Math.abs(slope), 'energy')} less per ${formatNumber(10000)} km driven.`,
        proj ? ` At this rate it reaches the ${pct(proj.warranty_pct)} warranty line at about ${format(proj.odometer_km, 'odometer')}, around ${formatDate(proj.t_ms, tz, { weekday: false, month: 'long' })}. A straight line is a rough guide.` : '',
        ' Per charge: energy added over the % it added, plus the buffer. Per day and the median: the pack\'s own reading. Click a per-charge point to open its charge.'));
    await loadUPlot();
    if (!ctx.alive()) return;
    const make = (width, height) => {
      const c = tone(), mag = cssVar('--mag'), cy = cssVar('--cyan');
      return {
        width, height, padding: [rpx(10), rpx(16), 0, 0], legend: { show: false },
        cursor: { drag: { x: false, y: false }, points: { size: rpx(9), width: rpx(2), stroke: c.hub }, dataIdx: (u, si, i) => { const a = u.data[si]; if (a[i] != null) return i; for (let k = 1; k < a.length; k++) { if (a[i - k] != null) return i - k; if (a[i + k] != null) return i + k; } return i; } },
        scales: { x: { time: opt.capX === 'time' }, y: { range: (u, a, b) => { const lo = Math.min(a, cap.new_kwh ?? a), hi2 = Math.max(b, cap.new_kwh ?? b); const p = Math.max(0.5, (hi2 - lo) * 0.15); return [lo - p, hi2 + p]; } } },
        axes: [opt.capX === 'time' ? timeAxis(tz, { spanMs: (xs[xs.length - 1] - xs[0]) * 1000 + 3 * DAY }) : { ...valueAxis({ size: 26 }), side: 2, grid: { show: false }, space: rpx(90), values: (u, v) => v.map((x) => `${formatNumber(x, { maximumFractionDigits: 0 })} ${unitLabel('odometer')}`) }, valueAxis({ digits: 1 })],
        series: [{},
          { label: 'Reading', stroke: mag, width: 0, paths: () => null, points: { show: true, space: 0, size: rpx(opt.capGroup === 'day' ? 4 : 6), width: 0, fill: alpha(mag, 0.7), stroke: alpha(mag, 0.7) } },
          { label: 'Median', stroke: cy, width: rpx(2.25), spanGaps: true, points: { show: false } }],
        hooks: {
          draw: [(u) => {
            if (cap.new_kwh == null) return;
            const y = Math.round(u.valToPos(cap.new_kwh, 'y', true)) + 0.5, g = u.ctx;
            g.save(); g.strokeStyle = c.axis; g.lineWidth = 1; g.setLineDash([rpx(3), rpx(3)]);
            g.beginPath(); g.moveTo(u.bbox.left, y); g.lineTo(u.bbox.left + u.bbox.width, y); g.stroke(); g.restore();
          }],
          setCursor: [(u) => {
            const i = u.cursor.idxs?.[1];
            if (i == null || u.cursor.left < 0 || y1[i] == null) { read.textContent = ''; return; }
            const it = all[i].it;
            read.textContent = `${formatDate(it.t_ms, tz, { weekday: false })} · ${format(it.odometer_km, 'odometer')} · usable ${format(it.usable_kwh, 'energy')}`;
          }],
        },
      };
    };
    mountPlot(box, make, [xs, y1, y2], ctx, (u) => u.over.addEventListener('click', () => { const i = u.cursor.idxs?.[1]; const it = i != null ? all[i]?.it : null; if (it?.charge) ctx.navigate(state.buildUrl('charge', { car: cid, charge: it.charge })); }));
  }

  /* ---------- charge level in the period */
  const sync = `bt-${cid}`;
  const clSide = h('span', { class: 'row' });
  const clEl = h('div', { class: 'stack' });
  let firstMs = null;
  async function startMs() {
    if (period.from_ms != null) return period.from_ms;
    if (firstMs != null) return firstMs;
    try { const pg = await ctx.api.get(`${base}/battery/capacity`, { group: 'month', limit: 1 }); firstMs = pg.items?.[0]?.t_ms ?? now() - 365 * DAY; } catch { firstMs = now() - 365 * DAY; }
    return firstMs;
  }
  const carry = (a, asleep) => { let last = null; return (a || []).map((v, i) => { if (v != null) { last = v; return v; } return asleep[i] ? last : (last = null); }); };
  const shade = (spans, lo, hi) => (u) => {
    const g = u.ctx, { left, top, width, height } = u.bbox;
    g.save();
    if (lo != null) {
      const y0 = Math.round(u.valToPos(hi, 'y', true)), y1 = Math.round(u.valToPos(lo, 'y', true));
      g.fillStyle = alpha(cssVar('--grn'), 0.08); g.fillRect(left, y0, width, y1 - y0);
    }
    for (const sp of spans || []) {
      const a = Math.max(left, u.valToPos(sp.start_ms / 1000, 'x', true)), b = Math.min(left + width, u.valToPos((sp.end_ms ?? now()) / 1000, 'x', true));
      if (b <= a) continue;
      g.fillStyle = alpha(cssVar(sp.state === 'asleep' ? '--st-asleep' : '--st-unknown'), sp.state === 'asleep' ? 0.14 : 0.28);
      g.fillRect(Math.round(a), top, Math.max(1, Math.round(b - a)), height);
    }
    g.restore();
  };
  async function paintChargeLevel() {
    const from = await startMs(), to = period.to_ms ?? now();
    const long = to - from > 62 * DAY;
    const bucket = opt.clBucket && !(opt.clBucket === 'hour' && long) ? opt.clBucket : to - from <= 31 * DAY ? 'hour' : 'day';
    const d = await ctx.api.get(`${base}/battery-level`, { from_ms: from, to_ms: to, bucket, moving: opt.clMoving ? 'true' : 'false', window: opt.clWindow });
    if (!ctx.alive()) return;
    const sB = seg([['hour', 'Hourly'], ['day', 'Daily']], bucket, (v) => { opt.clBucket = v; ctx.setQuery({ cl_bucket: v }); paintChargeLevel(); }, { label: 'Bucket', cover: 'cl.control.bucket_width' });
    if (long) sB.el.querySelector('[data-value="hour"]').disabled = true;
    const mov = h('label', { class: 'check bt-check', cover: 'cl.control.include_moving' }, h('input', { type: 'checkbox', checked: opt.clMoving, onchange: (e) => { opt.clMoving = e.currentTarget.checked; ctx.setQuery({ cl_moving: opt.clMoving ? null : '0' }); paintChargeLevel(); } }), h('span', null, 'Moving lines'));
    const sW = seg([['7d', '7 days'], ['30d', '30 days']], opt.clWindow, (v) => { opt.clWindow = v; ctx.setQuery({ cl_window: v === '30d' ? v : null }); paintChargeLevel(); }, { label: 'Moving window', cover: 'cl.control.window_width' });
    if (!opt.clMoving) for (const b of sW.el.children) b.disabled = true;
    const b = d.buckets || [], ts = b.map((x) => x.t_ms), bms = d.bucket_ms || 3600000;
    const asleepAt = (tt) => (d.spans || []).some((sp) => sp.state === 'asleep' && sp.start_ms <= tt + bms && (sp.end_ms ?? Infinity) >= tt);
    const slept = b.map((x) => x.soc_pct == null && asleepAt(x.t_ms));
    const mv = d.moving || {};
    const limits = [...(d.charge_limit || [])].sort((a, z) => a.t_ms - z.t_ms);
    const lim = ts.map((tt) => { let v = null; for (const l of limits) { if (l.t_ms <= tt + bms) v = l.pct; else break; } return v; });
    const lines = [
      { key: 'soc', label: 'Battery', color: '--mag', values: carry(b.map((x) => x.soc_pct), slept), cover: 'cl.chart.battery_level', fill: true, width: 2 },
      { key: 'usable', label: 'Usable', color: '--cyan', values: carry(b.map((x) => x.usable_soc_pct), slept), cover: 'cl.chart.usable_level', dash: [4, 3], width: 1.5 },
      ...(opt.clMoving ? [
        { key: 'mean', label: `Moving average (${opt.clWindow})`, color: '--org', values: mv.mean || [], cover: 'cl.chart.moving_avg', width: 1.75 },
        { key: 'median', label: `Moving median (${opt.clWindow})`, color: '--violet', values: mv.median || [], cover: 'cl.chart.moving_median', width: 1.75 },
        { key: 'p7_5', label: '7.5th percentile', color: '--ink-3', values: mv.p7_5 || [], cover: 'cl.chart.moving_p075', dash: [2, 3], width: 1 },
        { key: 'p92_5', label: '92.5th percentile', color: '--ink-3', values: mv.p92_5 || [], cover: 'cl.chart.moving_p925', dash: [2, 3], width: 1 },
      ] : []),
      { key: 'limit', label: 'Charge limit', color: '--yel', values: lim, width: 1.5 },
    ];
    replace(clSide, sB.el, mov, sW.el, csvBtn({ name: [car.name, 'charge-level', bucket], rows: () => ts.map((tt, i) => Object.fromEntries([['t_ms', tt], ...lines.map((l) => [l.key, l.values[i] ?? null])])), columns: [timeColumn('Bucket start', 't_ms', tz), ...lines.map((l) => column(l.label, l.key, 'percent', { digits: 2 }))] }));
    if (!b.some((x) => x.soc_pct != null)) { replace(clEl, emptyState({ icon: 'battery', title: 'No battery readings in this period', text: 'The car sent nothing in this period. Pick a longer period.' })); return; }
    const st = d.stats || {};
    const bnd = d.band || bat.band || {};
    const lo = bnd.low_pct ?? 20, hi = bnd.high_pct ?? 80;
    const statOf = (a) => { const f = a.filter((v) => v != null); return f.length ? { mean: f.reduce((x, y) => x + y, 0) / f.length, min: Math.min(...f), max: Math.max(...f) } : null; };
    const box = well('well--lg', 'Charge level over the period');
    const read = h('span', { class: 'scope-read' });
    let plot = null;
    const chips = lines.map((l, k) => h('button', { class: 'key', type: 'button', 'aria-pressed': 'true', cover: l.cover || null, title: 'Show or hide this line', onclick: (e) => { const on = e.currentTarget.getAttribute('aria-pressed') !== 'true'; e.currentTarget.setAttribute('aria-pressed', String(on)); plot?.u?.setSeries(k + 1, { show: on }); } },
      h('i', { class: 'key-sw', vars: { '--c': `var(${l.color})` } }), l.label));
    replace(clEl,
      h('div', { class: 'scope-head' }, h('span', { class: 'scope-title' }, 'Level · %'), h('span', { class: 'keys' }, chips), read),
      box,
      h('div', { class: 'cols bt-cl-foot' },
        counters([
          { label: 'Above 80 %', value: d.share_above_80_pct, kind: 'percent', how: hi >= 100 ? 'of known time; fine for LFP' : 'of known time; long stays high age a nickel pack' },
          { label: 'Below 20 %', value: d.share_below_20_pct, kind: 'percent', how: 'of known time' },
          { label: 'Median level', value: st.median, kind: 'percent', how: `of the ${bucket === 'hour' ? 'hourly' : 'daily'} levels` },
        ]),
        h('table', { class: 'bt-stats', cover: 'cl.chart.legend_stats', 'aria-label': 'Each line: average, lowest and highest' },
          h('thead', null, h('tr', null, h('th', { scope: 'col' }, 'Line'), h('th', { scope: 'col', class: 'is-num' }, 'Average'), h('th', { scope: 'col', class: 'is-num' }, 'Lowest'), h('th', { scope: 'col', class: 'is-num' }, 'Highest'))),
          h('tbody', null, lines.map((l) => [l, l.key === 'soc' ? { mean: st.mean, min: st.min, max: st.max } : statOf(l.values)]).filter(([, x]) => x).map(([l, x]) => h('tr', null,
            h('th', { scope: 'row' }, h('i', { class: 'key-sw', vars: { '--c': `var(${l.color})` } }), l.label), h('td', { class: 'is-num' }, pct(x.mean, 1)), h('td', { class: 'is-num' }, pct(x.min, 1)), h('td', { class: 'is-num' }, pct(x.max, 1))))))),
      h('p', { class: 'note', cover: 'cl.chart.band' }, icon('info', 'i--sm'), h('span', null, `Green band: the everyday range ${pct(lo)} to ${pct(hi)}. Blue: asleep (the level holds and the lines go on). Grey: unknown, no log (the lines break). Moving lines use a fixed ${opt.clWindow === '30d' ? '30-day' : '7-day'} window of daily means.`)));
    plot = await scope(box, ctx, { tz, t_ms: ts, sync, series: lines.map((l) => ({ label: l.label, values: l.values, color: l.color, fill: l.fill, dash: l.dash, width: l.width })), ranges: { y: [0, 100] },
      hooks: { drawClear: [shade(d.spans, lo, hi)] },
      onCursor: (i) => { read.textContent = i == null ? '' : `${formatDateTime(ts[i], tz, { now: now() })} · ${lines.filter((l) => l.values[i] != null).slice(0, 3).map((l) => `${l.label} ${pct(l.values[i], 1)}`).join(' · ')}`; } });
  }

  /* ---------- projected range */
  const prSide = h('span', { class: 'row' });
  const prEl = h('div', { class: 'stack' });
  async function paintProjected() {
    const from = await startMs(), to = period.to_ms ?? now();
    const params = { ...ctx.params, from_ms: from };
    const hourTooFine = to - from > 62 * DAY;
    if (opt.prBucket !== 'auto' && !(opt.prBucket === 'hour' && hourTooFine)) params.bucket = opt.prBucket; else params.max_points = 600;
    const d = await ctx.api.get(`${base}/projected-range`, params);
    if (!ctx.alive()) return;
    const sR = seg([['auto', 'Auto'], ['hour', 'Hour'], ['day', 'Day'], ['week', 'Week']], hourTooFine && opt.prBucket === 'hour' ? 'auto' : opt.prBucket, (v) => { opt.prBucket = v; ctx.setQuery({ pr_bucket: v === 'auto' ? null : v }); paintProjected(); }, { label: 'Resolution', cover: 'pr.control.resolution' });
    if (hourTooFine) sR.el.querySelector('[data-value="hour"]').disabled = true;
    const v = d.values || {}, ts = d.t_ms || [];
    replace(prSide, sR.el, csvBtn({ name: [car.name, 'projected-range'], rows: () => ts.map((tt, i) => ({ t_ms: tt, ...Object.fromEntries(Object.keys(v).map((k) => [k, v[k][i]])) })),
      columns: [timeColumn('Bucket start', 't_ms', tz), column('From battery %', 'full_range_km', 'range'), column('From energy', 'full_range_from_energy_km', 'range'), column('From usable %', 'full_range_usable_km', 'range'), column('Battery %', 'soc_pct', 'percent'), column('Pack % (BMS)', 'soc_bms_pct', 'percent'), column('Odometer', 'odometer_km', 'odometer'), column('Outside', 'outside_c', 'temperature'), column('Battery temperature', 'battery_c', 'temperature')] }));
    if (!ts.length || !(v.full_range_km || []).some((x) => x != null)) { replace(prEl, emptyState({ icon: 'gauge', title: 'No range readings in this period', text: 'Nothing was logged in this period. Pick a longer period.' })); return; }
    const slept = (d.state || []).map((x) => x === 'asleep');
    const R = (k) => carry((v[k] || []).map((x) => rk(x)), slept);
    const C = (k) => carry(v[k] || [], slept);
    const charts = [
      { title: 'Against distance driven', cover: ['pr.mileage.projected', 'pr.mileage.odometer'], right: { name: 'odometer', kind: 'odometer' },
        series: [{ label: 'From battery %', values: R('full_range_km'), kind: 'range', color: '--mag' }, { label: 'From energy', values: R('full_range_from_energy_km'), kind: 'range', color: '--cyan', dash: [5, 3] }, { label: 'Odometer', values: C('odometer_km'), kind: 'odometer', color: '--ink-3', scale: 'y2', width: 1.25 }] },
      { title: 'Against battery level', cover: ['pr.battery.projected_usable', 'pr.battery.projected_total', 'pr.battery.levels'], right: { name: 'level', kind: 'percent' },
        series: [{ label: 'From usable %', values: R('full_range_usable_km'), kind: 'range', color: '--cyan' }, { label: 'From battery %', values: R('full_range_km'), kind: 'range', color: '--mag', dash: [5, 3] }, { label: 'Battery %', values: C('soc_pct'), color: '--violet', scale: 'y2', width: 1.25 }, { label: 'Pack % (BMS)', values: C('soc_bms_pct'), color: '--ink-3', scale: 'y2', width: 1.25, dash: [3, 3] }] },
      { title: 'Against temperature', cover: ['pr.temp.projected', 'pr.temp.outdoor'], right: { name: 'temperature', kind: 'temperature' },
        series: [{ label: 'From battery %', values: R('full_range_km'), kind: 'range', color: '--mag' }, { label: 'From usable %', values: R('full_range_usable_km'), kind: 'range', color: '--cyan', dash: [5, 3] }, { label: 'Outside', values: C('outside_c'), kind: 'temperature', color: '--org', scale: 'y2', width: 1.25 }, { label: 'Battery', values: C('battery_c'), kind: 'temperature', color: '--grn', scale: 'y2', width: 1.25, dash: [3, 3] }] },
    ];
    const bucketText = d.bucket_ms >= 7 * DAY ? 'week' : d.bucket_ms >= DAY ? 'day' : d.bucket_ms >= 3600000 ? `${formatNumber(d.bucket_ms / 3600000)} h` : `${formatNumber((d.bucket_ms || 60000) / 60000)} min`;
    const blocks = charts.map((c, k) => {
      const box = well('', c.title);
      const read = h('span', { class: 'scope-read' });
      const rUnit = c.right.kind === 'percent' ? '%' : unitLabel(c.right.kind);
      return { c, box, read, el: h('div', { class: 'bt-pr', cover: c.cover },
        h('div', { class: 'scope-head', cover: k === 0 ? ['pr.axis.titles', 'pr.control.shared_crosshair'] : null }, h('span', { class: 'scope-title' }, `${c.title} · left ${unitLabel('range')}, right ${rUnit}`),
          h('span', { class: 'keys' }, c.series.map((s2) => h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': `var(${s2.color})` } }), s2.label))), read), box) };
    });
    replace(prEl, blocks.map((b) => b.el), note(`Each point is one ${bucketText} bucket: the range the car would show at 100 %, worked out from its range and its battery % (or usable %, or energy). From usable % sits lower in the cold, when part of the pack is locked. The three charts share one crosshair.`));
    for (const b of blocks) {
      if (!ctx.alive()) return;
      await scope(b.box, ctx, { tz, t_ms: ts, sync, series: b.c.series, ranges: { y: (u, a, z) => { const p = Math.max(2, (z - a) * 0.12); return [Math.floor(a - p), Math.ceil(z + p)]; }, ...(b.c.right.kind === 'percent' ? { y2: [0, 100] } : {}) }, axes: { y: { size: 48 }, y2: { size: 64 } },
        hooks: { drawClear: [(u) => { const g = u.ctx; const { top, height } = u.bbox; g.save(); (d.state || []).forEach((x, i) => { if (x !== 'asleep' && x !== 'unknown') return; const a = u.valToPos(ts[i] / 1000, 'x', true), z = u.valToPos((ts[i + 1] ?? ts[i] + (d.bucket_ms || 3600000)) / 1000, 'x', true); g.fillStyle = alpha(cssVar(x === 'asleep' ? '--st-asleep' : '--st-unknown'), x === 'asleep' ? 0.14 : 0.28); g.fillRect(Math.round(a), top, Math.max(1, Math.round(z - a)), height); }); g.restore(); }] },
        onCursor: (i) => { b.read.textContent = i == null ? '' : `${(d.bucket_ms || DAY) < DAY ? formatDateTime(ts[i], tz, { now: now() }) : formatDate(ts[i], tz, { weekday: false })} · ${b.c.series.filter((s2) => s2.values[i] != null).map((s2) => `${s2.label} ${format(s2.values[i], s2.kind || 'percent')}`).join(' · ')}`; } });
    }
  }

  /* ---------- parked drain */
  const vdSum = h('div');
  const params = () => ({ ...ctx.params, ...(opt.minH ? { min_hours: opt.minH } : {}) });
  const minSel = select([[0, 'Any length'], [1, 'At least 1 h'], [3, 'At least 3 h'], [6, 'At least 6 h'], [12, 'At least 12 h'], [24, 'At least 24 h']], opt.minH, (v) => { opt.minH = Number(v); ctx.setQuery({ min_h: opt.minH || null }); parked.reload(params()); }, { label: 'Shortest parked spell' });
  minSel.setAttribute('data-cover', 'vd.filter.min_idle');
  const standby = (r) => {
    const dd = r.duration_ms || 1;
    const a = ((r.asleep_ms || 0) / dd) * 100, u = ((r.unknown_ms || 0) / dd) * 100, w = Math.max(0, 100 - a - u);
    return h('span', { class: 'cell' },
      h('span', { class: 'bt-sb', role: 'img', 'aria-label': `Asleep ${Math.round(a)} %, awake ${Math.round(w)} %, unknown ${Math.round(u)} %` }, h('i', { class: 'bt-sb-a', vars: { '--w': `${a.toFixed(1)}%` } }), h('i', { class: 'bt-sb-w', vars: { '--w': `${w.toFixed(1)}%` } }), h('i', { class: 'bt-sb-u', vars: { '--w': `${u.toFixed(1)}%` } })),
      h('span', { class: 'cell-sub' }, `${formatDuration(r.asleep_ms || 0)} asleep · ${formatDuration((r.awake_ms || 0) + (r.awake_vehicle_bus_only_ms || 0))} awake${r.unknown_ms ? ` · ${formatDuration(r.unknown_ms)} unknown` : ''}`));
  };
  const parked = tracks(ctx, {
    title: 'Parked drain', icon: 'moon', path: `${base}/parked`, params: params(), limit: 12, csvName: 'parked-drain', csv: { all: true }, cover: ['vd.table', 'shots.vampire-drain'], dense: true,
    side: minSel,
    note: 'Each stay parked off a charger at one spot. Energy is measured at the pack, so it shows small losses that whole battery % steps hide. Unknown time is never counted as asleep.',
    onPage: (pg) => { if (pg.summary) { const sm = pg.summary; replace(vdSum, counters([
      { label: 'Parked spells', value: sm.spells, kind: 'count', how: `${formatNumber(sm.hours ?? 0, { maximumFractionDigits: 0 })} h parked off a charger` },
      { label: 'Energy lost', value: sm.energy_kwh, kind: 'energy', how: 'measured at the pack' },
      { label: 'Per day parked', value: sm.kwh_per_day, kind: 'energy', how: 'lost per 24 h parked' },
      { label: 'What it cost', text: sm.cost?.amount != null ? formatMoney(sm.cost.amount, sm.cost.currency) : '—', how: sm.cost ? 'at the home price' : 'set a home price in Places' },
    ])); } },
    columns: [
      { key: 'start', label: 'Parked', lead: true, cover: ['vd.col.start', 'vd.link.start_details'], render: (r) => h('span', { class: 'cell' },
        h('a', { class: 'cell-main', href: state.buildUrl('trip', { car: cid, period: { from: r.start_ms, to: r.end_ms ?? now() } }), title: 'Open exactly this span as a trip' }, formatDateTime(r.start_ms, tz, { now: now() })),
        h('span', { class: 'cell-sub' }, r.place?.name || r.address || 'Not a saved place')), csv: (r) => new Date(r.start_ms).toISOString() },
      { key: 'end', label: 'Until', cover: 'vd.col.end', render: (r) => (r.end_ms == null ? tag('Still parked', 'grn') : h('a', { class: 'plc', href: state.buildUrl('timeline', { car: cid, period: { from: startOf(r.start_ms, 'day', tz), to: r.end_ms } }) }, startOf(r.end_ms, 'day', tz) === startOf(r.start_ms, 'day', tz) ? formatTime(r.end_ms, tz) : formatDateTime(r.end_ms, tz, { now: now() }))), csv: (r) => (r.end_ms ? new Date(r.end_ms).toISOString() : '') },
      { key: 'length', label: 'Length', num: true, cover: 'vd.col.period', render: (r) => formatDuration(r.duration_ms), csv: (r) => Math.round((r.duration_ms || 0) / 60000) },
      { key: 'standby', label: 'Asleep · awake', cover: 'vd.col.standby', render: standby, csv: (r) => (r.duration_ms ? Math.round(((r.asleep_ms || 0) / r.duration_ms) * 1000) / 10 : '') },
      { key: 'energy', label: 'Energy lost', num: true, cover: 'vd.col.energy', render: (r) => h('span', { class: 'cell' }, format(r.energy_kwh, 'energy'), r.energy_method && r.energy_method !== 'counter' ? h('span', { class: 'cell-sub' }, METHOD[r.energy_method] || r.energy_method) : null), csv: column('Energy lost', 'energy_kwh', 'energy') },
      { key: 'power', label: 'Average power', num: true, cover: 'vd.col.avg_power', render: (r) => h('span', { class: 'cell' }, format(r.avg_power_w, 'power_w'), r.awake_avg_power_w != null ? h('span', { class: 'cell-sub' }, `awake ${format(r.awake_avg_power_w, 'power_w')}`) : null), csv: (r) => r.avg_power_w },
      { key: 'soc', label: 'Battery', num: true, cover: 'vd.col.soc_diff', render: (r) => h('span', { class: 'cell' }, r.soc_start_pct == null || r.soc_end_pct == null ? '—' : `−${pct(Math.max(0, r.soc_start_pct - r.soc_end_pct), 1)}`, h('span', { class: 'cell-sub' }, `${pct(r.soc_start_pct)} → ${pct(r.soc_end_pct)}`)), csv: (r) => (r.soc_start_pct == null || r.soc_end_pct == null ? '' : r.soc_start_pct - r.soc_end_pct) },
      { key: 'range', label: 'Range lost', num: true, cover: 'vd.col.range_loss', render: (r) => h('span', { class: 'cell' }, format(rk(r.range_lost_km), 'range', { digits: 1 }), h('span', { class: 'cell-sub', cover: 'vd.col.range_per_hour' }, `${format(rk(r.range_lost_per_hour_km), 'range_rate', { digits: 2 })}`)), csv: column('Range lost', 'range_lost_km', 'range') },
      { key: 'src', label: 'Source', render: (r) => srcChip(r), csv: (r) => sourceLabel(r) },
      { key: 'why', label: 'Kept awake by', cover: 'vd.flag.cold', render: (r) => h('span', { class: 'flagline' },
        r.cold ? h('span', { class: 'tag tag--cyan', title: `Coldest pack temperature ${format(r.battery_min_c, 'temperature')}` }, 'Cold pack') : null,
        (r.awake_reasons || []).length ? r.awake_reasons.map((x) => tag(REASONS[x] || x)) : h('span', { class: 'faint' }, r.awake_ms > 0 ? 'no reason sent' : 'slept throughout')), csv: (r) => [r.cold ? 'cold pack' : null, ...(r.awake_reasons || [])].filter(Boolean).join('; ') },
    ],
    empty: () => emptyState({ icon: 'moon', title: 'No parked spells in this period', text: opt.minH ? `None was at least ${opt.minH} h long. Choose Any length, or a longer period.` : 'The car did not stay parked off a charger in this period. Pick a longer period.' }),
  });
  ctx.events('rows', (ev) => { if (ev.data?.car === cid) parked.reload(params(), { quiet: true }); });

  replace(ctx.view, h('div', { class: 'page bt' },
    plate({ kicker: `Energy · ${car.name}`, title: 'Battery', aim: ctx.info.aim,
      extra: h('p', { class: 'bt-chem' }, `${bat.chemistry?.value ? `${CHEM[bat.chemistry.value] || bat.chemistry.value} pack` : 'Chemistry not known'} · pack readings as of ${formatDateTime(bat.as_of_ms, tz, { now: now() })}`),
      tools: [carBtn(ctx, ['bh.filter.car', 'cl.filter.car', 'pr.section.per_car', 'vd.section.per_car'])] }),
    hud({ title: 'Now', icon: 'live', tone: 'cyan', side: h('span', { class: 'trk-count' }, 'live') }, nowEl),
    healthHud,
    lifeHud,
    refHud,
    capHud,
    band(1, 'In the period', h('span', { class: 'band-side' }, periodBtn(ctx, ['cl.filter.time_range', 'pr.filter.time_range', 'vd.filter.time_range']))),
    hud({ title: 'Charge level', icon: 'battery', side: clSide, cover: ['cl.chart.charge_level', 'shots.charge-level'] }, clEl),
    hud({ title: 'Projected full-charge range', icon: 'gauge', side: prSide, cover: 'shots.projected-range' }, prEl),
    hud({ title: 'Parked drain, in sum', icon: 'moon', tone: 'orange' }, vdSum),
    parked.el));

  await loadUPlot().catch(() => null);
  if (!ctx.alive()) return;
  await Promise.all([
    paintCapacity().catch((e) => replace(capEl, errorState(e, { retry: () => paintCapacity(), home: false }))),
    paintChargeLevel().catch((e) => replace(clEl, errorState(e, { retry: () => paintChargeLevel(), home: false }))),
    paintProjected().catch((e) => replace(prEl, errorState(e, { retry: () => paintProjected(), home: false }))),
    parked.ready,
  ]);
  if (ctx.alive()) ctx.ready();
}
