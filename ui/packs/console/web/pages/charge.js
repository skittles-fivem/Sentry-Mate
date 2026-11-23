// Charge (/cars/{car}/charges/{charge}): one charging session. A header line, a stat block (time, energy
// into the pack and from the supply, efficiency, battery and range at the start and the end), where it was
// on a small flat map, then the session on one time axis: power and battery on one uPlot with two y-axes,
// and stacked panes under it (range, temperatures, battery heating, the supply: volts, amps and the pilot,
// phases) sharing one cursor; drag across a pane to zoom them all. Then power against battery % with this
// car's usual fast-charging curve, the cost with its parts and an editor (PUT / DELETE .../cost, admin), and
// the samples on demand.

import * as state from '../shared/state.js';
import { h, replace, cover } from '../shared/dom.js';
import { format, formatMoney, unitLabel, convert, getUnits } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, zoneName, dayRange, dayKey, DAY } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import * as TI from '../shared/tiers.js';
import { head, section, kv, note, link } from '../core/ui.js';
import { num, tag } from '../core/fmt.js';
import { safeMap } from '../core/map.js';
import { loadUPlot, plot, line, timeAxis, valueAxis, cursor, readout } from '../core/chart.js';
import { table } from '../core/table.js';
import { field, form } from '../core/form.js';
import { empty } from '../core/states.js';
import { rpx } from '../core/display.js';
import { toast } from '../core/toast.js';
import { placeLink } from './timeline.js';
import { KIND, KIND_TONE } from './charges.js';
import { src, est as estTag, kindsOf, apiOnly, isEstimated, EST_WHY } from '../core/source.js';
import { appliedInvoice, unusedInvoices, invoiceSourceLabel, feeLabel, chargeInvoicePdfUrl } from '../shared/invoices.js';

export const id = 'charge';
export const title = () => 'Charge';

const enc = encodeURIComponent;
const CURRENCIES = ['USD', 'CAD', 'EUR', 'GBP', 'NOK', 'SEK', 'DKK', 'CHF', 'AUD', 'NZD', 'JPY', 'CNY'];
const COST_SOURCE = { tariff: 'from the place price', manual: 'typed by hand', free_supercharging: 'free supercharging', teslamate: 'imported', tesla_invoice: 'from tesla invoice', teslafi: 'from teslafi' };
const LIMIT = { station: 'the station pilot', cable: 'the cable', car: 'the car’s maximum', setting: 'the set current' };
const FIELDS = ['power_kw', 'ac_line_kw', 'soc_pct', 'usable_soc_pct', 'range_km', 'battery_c', 'outside_c', 'heater_kw',
  'ac_volts_v', 'ac_amps_a', 'pilot_a', 'phases', 'limit_source', 'dc_volts_v', 'dc_amps_a'];
const none = (t) => h('span', { class: 'muted' }, t);
const pct = (v) => format(v, 'percent');
// a rate with the decimals it was typed with (2 to 5)
const rateText = (v, kind, currency) => format(v, kind, { currency, digits: Math.max(2, Math.min(5, (String(+Number(v).toFixed(5)).split('.')[1] || '').length)) });

export async function mount(ctx) {
  const { car, tz } = ctx;
  const cid = car.id, chid = ctx.state.charge;
  const base = `cars/${enc(cid)}/charges/${enc(chid)}`;
  let c;
  try { c = await ctx.api.get(base); } catch (e) {
    if (e.status === 404) e.message = `There is no charge with this number for ${car.name}. It may have been rebuilt under a new number; find it on charges.`;
    ctx.fail(e, e.status === 404 ? { retry: null } : {}); return;
  }
  if (!ctx.alive()) return;
  const live = !!c.flags?.in_progress || c.end_ms == null;
  const t0 = c.start_ms, t1 = c.end_ms ?? ctx.api.now();
  const dc = c.kind === 'dc', ac = c.kind === 'ac';
  const imported = kindsOf(c).length > 0 && kindsOf(c).every((k) => k === 'import');
  const [curve, place, before, typical] = await Promise.all([
    ctx.api.get(`${base}/curve`, { fields: FIELDS.join(','), max_points: 4000 }).catch(() => null),
    c.place?.id ? ctx.api.get(`places/${enc(c.place.id)}`).catch(() => null) : null,
    ctx.api.get(`cars/${enc(cid)}/timeline`, { to_ms: t0, kinds: 'drive', limit: 1 }).catch(() => null),
    dc ? ctx.api.get(`cars/${enc(cid)}/charging-stats/dc-curve`, { from_ms: t0 - 365 * DAY, to_ms: t1 + 1 }).catch(() => null) : null,
  ]);
  if (!ctx.alive()) return;
  const zone = zoneName(t0, tz);
  const when = `${formatDate(t0, tz, { weekday: false, year: false })} ${formatTime(t0, tz)}`;
  ctx.setTitle(`Charge ${when}`);
  if (live) ctx.events('rows', (ev) => { if (ev.data?.car === cid && (ev.data.kinds || []).includes('charges')) ctx.reload(); });

  /* ---------- the header, the path line */
  const hd = head(ctx, { title: 'Charge', period: false, extra: h('span', { class: 'ph-extra muted', cover: 'cd.context.session' },
    `${formatDate(t0, tz, { weekday: false })} ${formatTime(t0, tz)} - ${live ? 'now' : formatTime(t1, tz)} ${zone}`) });
  const carBtn = hd.querySelector('.ph-btn');
  if (carBtn) cover(carBtn, 'cd.filter.car', 'web.charge-cost.car');
  const driveBefore = before?.items?.[0]?.ref?.drive || null;
  const day = dayKey(t0, tz);
  const nav = h('span', { class: 'drive-nav' },
    c.prev ? link(state.buildUrl('charge', { car: cid, charge: c.prev }), '< previous') : none('< previous'), '  ',
    c.next ? link(state.buildUrl('charge', { car: cid, charge: c.next }), 'next >') : none('next >'), '  ',
    driveBefore ? link(state.buildUrl('drive', { car: cid, drive: driveBefore }), 'drive before') : null, driveBefore ? '  ' : null,
    link(state.buildUrl('timeline', { car: cid, period: dayRange(day, day, tz) }), 'that day'));
  const subline = h('p', { class: 'subline', cover: 'web.charge-cost.breadcrumb' },
    h('a', { class: 'lnk-plain', href: ctx.link('charges'), cover: 'web.charge-cost.back', title: 'back to the charges' }, '< charges'), '  ',
    placeLink(c) || '—', ' ', tag(KIND[c.kind] || '?', KIND_TONE[c.kind] || 'muted'),
    c.fast_charger ? none(` ${[c.fast_charger.type === 'supercharger' ? 'supercharger' : c.fast_charger.brand || 'fast charger', c.fast_charger.class].filter(Boolean).join(' ').toLowerCase()}`) : null,
    live ? [' ', tag('charging now', 'warn')] : null, c.has_gaps ? [' ', tag(`gap ${formatDuration(c.flags?.gap_ms || 0)}`, 'bad')] : null, nav);
  const srcLine = h('p', { class: 'src-line' }, src(c, { full: true }), isEstimated(c) && apiOnly(c) ? none(`. ${EST_WHY}`) : null);

  /* ---------- figures */
  const why = live ? 'still charging' : imported ? 'not in imported history' : apiOnly(c) ? 'not from the tesla api' : 'not recorded';
  const m = (v, kind, w = why) => (v == null ? none(w) : num(v, kind));
  const gridKwh = c.energy_grid_kwh;
  const socAdded = c.start_soc_pct != null && c.end_soc_pct != null ? c.end_soc_pct - c.start_soc_pct : null;
  const rangeAdded = c.range_added_km ?? (c.start_range_km != null && c.end_range_km != null ? c.end_range_km - c.start_range_km : null);
  const price = place?.price || place?.tariff || null;
  const figures = kv([
    ['when', h('span', { cover: 'web.charge-cost.dates' }, `${formatTime(t0, tz)} - ${live ? 'now' : formatTime(t1, tz)} ${zone}`)],
    ['charging', h('span', { cover: 'web.charge-cost.duration' }, formatDuration(c.charging_ms), c.plugged_ms != null ? none(` plugged ${formatDuration(c.plugged_ms)}`) : null), { cover: 'cd.stat.duration' }],
    ['place', h('span', { cover: 'web.charge-cost.location' }, placeLink(c) || '—', c.place && c.address?.short ? none(` ${c.address.short}`) : null), { cover: 'cd.stat.location' }],
    ['added', h('span', { cover: 'web.charge-cost.energy' }, m(c.energy_added_kwh, 'energy'), estTag(c), none(' into the pack')), { cover: 'cd.stat.energy_added' }],
    [ac ? 'drawn' : dc ? 'delivered' : 'drawn', ac ? [m(gridKwh, 'energy'), gridKwh != null ? none(` from the line${c.losses_kwh != null ? `, ${format(c.losses_kwh, 'energy')} lost` : ''}`) : null]
      : dc ? [m(c.dc_inlet_kwh, 'energy'), none(c.fast_charger?.billed_kwh != null ? ` at the inlet; billed ${format(c.fast_charger.billed_kwh, 'energy')}` : ' at the inlet; the grid side is not sent')] : none('unknown whether ac or dc'), { cover: 'cd.stat.energy_used' }],
    ['efficiency', c.efficiency_pct != null ? [pct(c.efficiency_pct), none(' added over drawn')] : none(dc ? 'unknown for dc: a fast charger’s grid side is not measured' : c.efficiency_note || why), { cover: 'cd.stat.efficiency' }],
    ['battery', h('span', { class: 'nowrap' }, h('span', { cover: 'cd.stat.soc_start' }, pct(c.start_soc_pct)), h('span', { class: 'arrow' }, ' -> '), h('span', { cover: 'cd.stat.soc_end' }, pct(c.end_soc_pct)),
      h('span', { class: 'muted', cover: 'cd.stat.soc_added' }, socAdded != null ? `  +${format(socAdded, 'percent')}` : ''))],
    ['range', h('span', null, h('span', { cover: 'cd.stat.range_start' }, num(c.start_range_km, 'range')), h('span', { class: 'arrow' }, ' -> '), h('span', { cover: 'cd.stat.range_end' }, num(c.end_range_km, 'range')),
      h('span', { class: 'muted', cover: 'cd.stat.range_added' }, rangeAdded != null ? `  +${format(rangeAdded, 'range')}` : ''))],
    ['power', [m(c.power_avg_kw, 'power'), c.power_max_kw != null ? none(` avg, peak ${format(c.power_max_kw, 'power')}`) : none(' avg')], { cover: 'cd.stat.avg_power' }],
    ['outside', [m(c.outside_avg_c, 'temperature'), c.battery_avg_c != null ? none(` avg, battery ${format(c.battery_avg_c, 'temperature')}`) : null], { cover: 'cd.stat.avg_temp' }],
    ['odometer', num(c.odometer_km, 'odometer')],
    ['range added', [m(c.range_per_hour_km, 'range_rate'), none(' per hour of charging')]],
  ]);
  const mapEl = h('div', { class: 'map map--sm', role: 'img', 'aria-label': 'Map of where it charged' });

  /* ---------- through the charge */
  const t = curve?.t_ms || [];
  const col = (k) => curve?.values?.[k] || [];
  const V = (k, kind) => col(k).map((v) => (v == null || typeof v !== 'number' ? null : kind ? convert(v, kind) : v));
  const has = (a) => a.some((v) => v != null);
  const range = getUnits().range === 'ideal' ? 'ideal' : 'rated';
  const PANES = [
    { key: 'main', name: 'power and battery', unit: `kW / %`, cover: ['cd.chart.power', 'cd.chart.soc'], two: true, h: 'chart',
      s: [['power', V('power_kw'), 'kW'], ['battery', V('soc_pct'), '%', true], ...(has(V('ac_line_kw')) ? [['line', V('ac_line_kw'), 'kW']] : [])] },
    { key: 'range', name: `${range} range`, unit: unitLabel('range'), cover: 'cd.chart.range', s: [['range', V('range_km', 'range'), unitLabel('range')]] },
    { key: 'temps', name: 'temperatures', unit: unitLabel('temperature'), cover: 'cd.chart.outdoor_temp', s: [['outside', V('outside_c', 'temperature'), unitLabel('temperature')], ['battery', V('battery_c', 'temperature'), unitLabel('temperature')]] },
    { key: 'heat', name: 'battery heating', unit: 'kW', min0: true, cover: 'cd.chart.battery_heater', s: [['heater', V('heater_kw'), 'kW']] },
    { key: 'volts', name: dc ? 'charger voltage' : 'line voltage', unit: 'V', cover: 'cd.chart.ac_voltage', s: [['volts', V(dc ? 'dc_volts_v' : 'ac_volts_v'), 'V']] },
    { key: 'amps', name: dc ? 'charger current' : 'current', unit: 'A', min0: true, cover: ['cd.chart.ac_current', 'cd.chart.pilot_current'], s: [['drawn', V(dc ? 'dc_amps_a' : 'ac_amps_a'), 'A'], ...(dc ? [] : [['pilot', V('pilot_a'), 'A']])] },
    { key: 'phases', name: 'phases in use', unit: 'count', range: [0, 3.5], cover: 'cd.chart.phases', s: [['phases', V('phases'), '']] },
  ];
  const blocks = PANES.filter((p) => p.s.some(([, a]) => has(a))).map((p) => ({ ...p, el: h('div', { class: ['chart', p.h !== 'chart' && 'chart--pane'] }) }));
  const missing = PANES.filter((p) => !blocks.some((b) => b.key === p.key));
  const read = readout('move over a pane for values; drag across it to zoom');
  const selLine = h('p', { class: 'sel-line', cover: 'cd.filter.time_range' });
  const plots = [];
  let zoom = null;
  const lim = col('limit_source');
  const paintSel = () => {
    if (!zoom) {
      replace(selLine, none('the whole charge: '), formatDuration(c.charging_ms), none('  added '), m(c.energy_added_kwh, 'energy'), none('  avg '), m(c.power_avg_kw, 'power'),
        lim.some((x) => x) ? [none('  limited by '), LIMIT[lim.filter(Boolean).pop()] || lim.filter(Boolean).pop()] : null, none('   drag across a pane to zoom'));
      return;
    }
    const [a, b] = zoom;
    const idx = t.map((x, i) => (x >= a && x <= b ? i : -1)).filter((i) => i >= 0);
    const pw = col('power_kw');
    let kwh = 0;
    for (let k = 1; k < idx.length; k++) { const v = pw[idx[k]]; const dt = (t[idx[k]] - t[idx[k - 1]]) / 3.6e6; if (v != null && dt < 0.1) kwh += v * dt; }
    const soc = idx.map((i) => col('soc_pct')[i]).filter((v) => v != null);
    const p = idx.map((i) => pw[i]).filter((v) => v != null);
    replace(selLine, none('selected '), `${formatTime(a, tz)} - ${formatTime(b, tz)}`, none('  time '), formatDuration(b - a), none('  added '), p.length ? num(kwh, 'energy') : '—',
      none('  avg '), p.length ? num(p.reduce((s, v) => s + v, 0) / p.length, 'power') : '—', none('  battery '), soc.length > 1 ? format(soc[soc.length - 1] - soc[0], 'percent', { sign: true }) : '—', '  ',
      h('button', { class: 'btn btn--line tone--accent', type: 'button', onclick: () => setZoom(null) }, '[all]'));
  };
  const setZoom = (z) => { zoom = z; for (const p of plots) p.u?.setScale('x', { min: z ? z[0] : t[0], max: z ? z[1] : t[t.length - 1] }); paintSel(); };
  paintSel();
  const keyMarks = ['--', '- -', '. .'];
  const panesEl = h('div', { class: 'panes' }, blocks.map((b) => h('div', { class: 'pane', cover: b.cover },
    h('div', { class: 'chart-head' }, h('b', null, b.name), h('span', null, b.unit),
      h('span', { class: 'chart-key' }, b.s.length > 1 ? b.s.map(([n], k) => h('span', { class: `chart-key-i chart-key-i--${k + 1}` }, `${keyMarks[k]} ${n}`)) : null)),
    b.el)),
  missing.length ? h('p', { class: 'note', cover: missing.flatMap((p) => [].concat(p.cover)) }, `not sent for this charge: ${missing.map((p) => p.name).join(', ')}`) : null);
  const onCursor = (i) => {
    if (i == null) { read.show(null); return; }
    read.show([['at', formatTime(t[i], tz, { seconds: true })], ...blocks.flatMap((b) => b.s.map(([n, a, u]) => [n, a[i] == null ? '—' : `${formatNumber(a[i], { maximumFractionDigits: 1 })}${u ? ` ${u}` : ''}`]))]);
  };

  /* ---------- power against battery % */
  const pts = [];
  t.forEach((ms, i) => { const s = col('soc_pct')[i], p = col('power_kw')[i]; if (s != null && p != null && p > 0.05) pts.push([s, p]); });
  pts.sort((a, b) => a[0] - b[0]);
  const bySoc = new Map();
  for (const [s, p] of pts) { const k = Math.round(s); const e = bySoc.get(k) || [0, 0]; e[0] += p; e[1]++; bySoc.set(k, e); }
  const usual = new Map((typical?.median || []).map((x) => [x.soc_pct, x.power_kw]));
  const curveEl = h('div', { class: 'chart' });
  const curveRead = readout(dc ? 'one dot per sample; the line is the mean at each percent, dashed is this car’s usual fast charge' : 'one dot per sample; the line is the mean at each percent');

  /* ---------- cost */
  const cost = c.cost;
  const bd = c.cost_breakdown || {};
  const manual = c.manual_cost;
  const cur = cost?.currency || price?.currency || 'USD';
  const parts = (bd.parts || []).map((p) => ({ ...p }));
  if (bd.session_fee) parts.push({ rate: 'session fee', cost: bd.session_fee });
  const inv = invoiceRows(c, cid, chid);
  const costNow = kv([
    ['cost', h('span', null, cost ? h('b', null, formatMoney(cost.amount, cost.currency)) : none('no cost yet'), cost ? none(` ${COST_SOURCE[cost.source] || cost.source}`) : null,
      cost?.source === 'manual' ? [' ', tag('typed', 'warn')] : null), { cover: ['cd.stat.cost', 'web.charge-cost.current-cost'] }],
    ['per kwh', c.price_per_kwh?.value != null ? [format(c.price_per_kwh.value, 'price_energy', { currency: cost?.currency }), none(` of the ${c.price_per_kwh.basis === 'grid' ? 'energy drawn' : 'energy added'}`)]
      : none(c.place ? `${c.place.name} has no price yet: add one on places, or type the cost below` : 'not a saved place with a price: type the cost below')],
    ...inv.rows,
    bd.cycle ? ['tiered', h('span', null, TI.splitText(TI.stepParts(bd), cur), none(`; the cycle from ${formatDate(bd.cycle.start_ms, c.place?.time_zone || tz, { weekday: false })} had ${format(bd.cycle.before_kwh, 'energy')} before it`))] : null,
  ].filter(Boolean), { cols: 1 });
  const breakdown = table(ctx, {
    title: 'how the cost was made', rows: parts, csv: false,
    columns: [
      { key: 'rate', label: 'rate', ph: 'main', render: (p) => String(p.from_kwh != null ? `${p.rate}: ${TI.stepLabel(p.from_kwh, p.to_kwh)}` : p.rate || '').toLowerCase() },
      { key: 'per_kwh', label: '/kwh', num: true, ph: 'sub', render: (p) => (p.per_kwh != null ? rateText(p.per_kwh, 'price_energy', cur) : null) },
      { key: 'per_minute', label: '/min', num: true, ph: 'sub', render: (p) => (p.per_minute != null ? rateText(p.per_minute, 'price_time', cur) : null) },
      { key: 'kwh', label: 'energy', num: true, render: (p) => (p.kwh != null ? num(p.kwh, 'energy') : null) },
      { key: 'minutes', label: 'time', num: true, render: (p) => (p.minutes != null ? formatDuration(p.minutes * 60000) : null) },
      { key: 'cost', label: 'cost', num: true, ph: 'num', render: (p) => formatMoney(p.cost, cur) },
    ],
    empty: () => empty(cost?.source === 'free_supercharging' ? 'free supercharging: nothing to pay' : cost ? 'one amount, no rates' : 'no cost to break down yet'),
  });
  const editor = costEditor(ctx, { c, base, manual, cur, gridKwh, dc });

  /* ---------- samples on demand */
  const samplesSlot = h('div');
  const samplesBtn = h('button', { class: 'btn', type: 'button', onclick: () => {
    const step = Math.max(1, Math.ceil(t.length / 600));
    const rows = [];
    for (let i = 0; i < t.length; i += step) rows.push(i);
    const cn = (key, label, a, u, ph = null) => ({ key, label, num: true, ph, render: (i) => (a[i] == null ? null : `${formatNumber(a[i], { maximumFractionDigits: 1 })}${u ? ` ${u}` : ''}`) });
    const tb = table(ctx, { bare: true, rows, label: 'Samples', csv: false, columns: [
      { key: 't', label: 'time', ph: 'main', render: (i) => formatTime(t[i], tz, { seconds: true }) },
      cn('power', 'power', V('power_kw'), 'kW', 'num'), cn('soc', 'battery', V('soc_pct'), '%', 'sub'), cn('range', 'range', V('range_km', 'range'), unitLabel('range')),
      cn('volts', 'volts', V(dc ? 'dc_volts_v' : 'ac_volts_v'), 'V'), cn('amps', 'amps', V(dc ? 'dc_amps_a' : 'ac_amps_a'), 'A'), cn('bat', 'battery temp', V('battery_c', 'temperature'), unitLabel('temperature')),
    ] });
    replace(samplesSlot, note(`${formatNumber(rows.length)} of ${formatNumber(t.length)} samples${step > 1 ? `, one in ${step}` : ''}.`), tb.el);
    samplesBtn.hidden = true;
  } }, 'show the samples');

  replace(ctx.view, h('div', { class: 'page', dataset: { page: 'charge' }, cover: 'shots.charge-details' },
    hd, subline, srcLine,
    h('div', { class: 'cols2' },
      section({ title: 'figures', side: live ? h('span', { class: 'tone--warn' }, 'so far') : null }, figures),
      section({ title: 'where', cover: ['cd.map.location', 'web.charge-cost.map'] }, mapEl,
        c.position?.lat != null ? note(`${c.address?.short || 'a spot'}${c.position.accuracy_m != null ? `, within ${format(c.position.accuracy_m, 'length')}` : ''}`) : null)),
    section({ title: 'through the charge', cover: 'cd.chart.timeline', side: curve ? src(curve) : null }, t.length ? [selLine, panesEl, read.el] : empty('no samples recorded for this charge', imported ? 'imported history has the totals only' : null)),
    section({ title: 'power against battery', cover: ['cd.chart.curve_points', 'cd.chart.curve_avg'] },
      h('div', { class: 'chart-head' }, h('b', null, 'power into the pack'), h('span', null, 'kW by %'),
        h('span', { class: 'chart-key' }, h('span', { class: 'chart-key-i chart-key-i--2' }, '. samples'), h('span', { class: 'chart-key-i chart-key-i--1' }, '-- mean'), dc ? h('span', { class: 'chart-key-i chart-key-i--3' }, '- - usual') : null)),
      pts.length > 1 ? [curveEl, curveRead.el] : empty('too few points to draw')),
    section({ title: 'cost', id: 'cost' },
      h('div', { class: 'cols2' }, h('div', null, costNow, inv.slots, breakdown.el,
        cost?.source === 'tariff' ? note(`each rate of the place price for the time it applied, in ${zone}${bd.gap_ms ? `; ${formatDuration(bd.gap_ms)} without data is priced at the rate around it` : ''}.`) : null), editor)),
    section({ title: 'samples' }, t.length ? [samplesBtn, samplesSlot] : empty('no samples')),
    h('div', { hidden: true, cover: 'web.charge-cost.not-found' }, empty('no such charge'))));

  if (ctx.query.edit === 'cost') requestAnimationFrame(() => { document.getElementById('cost')?.scrollIntoView({ block: 'start' }); editor.querySelector('input[name="amount"]')?.focus({ preventScroll: true }); });

  // charts first (the ready signal waits for them), then the map
  if (t.length || pts.length > 1) {
    try { await loadUPlot(); } catch { replace(panesEl, h('p', { class: 'chart-note' }, '-- the charts could not be drawn --')); }
  }
  if (!ctx.alive()) return;
  if (window.uPlot && t.length) {
    const lastKey = blocks[blocks.length - 1]?.key;
    for (const b of blocks) {
      const xAxis = b.key === lastKey ? timeAxis(tz) : { ...timeAxis(tz), size: 0, gap: 0, values: (u, sp) => sp.map(() => '') };
      const yScale = b.range ? { range: () => b.range } : b.min0 ? { range: (u, a, cc) => [0, Math.max(1, (cc || 0) * 1.1)] } : { auto: true };
      plots.push(plot(b.el, ctx, () => ({
        padding: [rpx(6), 0, b.key === lastKey ? 0 : rpx(8), 0],
        cursor: { ...cursor(`charge-${chid}`), drag: { x: true, y: false, setScale: false } },
        select: { show: true },
        scales: { x: { time: true, min: zoom ? zoom[0] : t[0], max: zoom ? zoom[1] : t[t.length - 1] }, y: yScale, ...(b.two ? { y2: { range: () => [0, 100] } } : {}) },
        axes: [xAxis, valueAxis((v) => formatNumber(v), { size: 52 }),
          b.two ? valueAxis((v) => `${v}%`, { scale: 'y2', side: 1, grid: false, size: 52 }) : { show: true, side: 1, size: rpx(52), grid: { show: false }, ticks: { show: false }, values: () => [] }],
        series: [{}, ...b.s.map(([n, , , onY2], k) => line(n, k, { scale: b.two && onY2 ? 'y2' : 'y', width: k ? 1.5 : 2 }))],
        hooks: { setSelect: [(u) => { if (u.select.width > 2) { const a = u.posToVal(u.select.left, 'x'), e = u.posToVal(u.select.left + u.select.width, 'x'); u.setSelect({ width: 0, height: 0 }, false); setZoom([a, e]); } }] },
      }), [t, ...b.s.map(([, a]) => a)], { onCursor }));
    }
  }
  if (window.uPlot && pts.length > 1) {
    const cx = pts.map((p) => p[0]);
    const mean = cx.map((s) => { const e = bySoc.get(Math.round(s)); return e ? e[0] / e[1] : null; });
    const us = cx.map((s) => (dc ? usual.get(Math.round(s)) ?? null : null));
    plot(curveEl, ctx, ({ colors: cl }) => ({
      padding: [rpx(6), 0, 0, 0],
      cursor: { x: true, y: false, points: { show: false }, drag: { x: false, y: false } },
      scales: { x: { time: false, auto: true }, y: { range: (u, a, b) => [0, Math.max(1, (b || 0) * 1.1)] } },
      axes: [{ ...valueAxis((v) => `${formatNumber(v)}%`, { scale: 'x', side: 2, size: 28 }), space: rpx(64) }, valueAxis((v) => formatNumber(v), { size: 48 })],
      series: [{}, { stroke: cl.c2, width: 0, paths: () => null, points: { show: true, size: rpx(4), width: 0, fill: cl.c2, stroke: cl.c2 } },
        { ...line('mean', 0, { width: 2 }), spanGaps: true }, { ...line('usual', 2, { width: 1.5 }), spanGaps: true }],
    }), [cx, pts.map((p) => p[1]), mean, us], {
      onCursor: (i) => curveRead.show(i == null ? null : [['battery', pct(cx[i])], ['power', format(pts[i][1], 'power')], ['mean', mean[i] != null ? format(mean[i], 'power') : '—'], dc ? ['usual', us[i] != null ? format(us[i], 'power') : '—'] : null].filter(Boolean)),
    });
  }

  if (c.position?.lat != null) {
    const mp = await safeMap(mapEl, ctx, { center: [c.position.lon, c.position.lat], zoom: 15 });
    if (mp && ctx.alive()) {
      if (place?.radius_m) mp.circle('geo', place.lon ?? c.position.lon, place.lat ?? c.position.lat, place.radius_m, { token: '--accent' });
      mp.marker([c.position.lon, c.position.lat], 'map-pin map-pin--warn');
      mp.frame(() => mp.map.jumpTo({ center: [c.position.lon, c.position.lat], zoom: 15 }));
    }
  } else mapEl.append(h('p', { class: 'map-note' }, '-- no position recorded for this charge --'));
  if (!ctx.alive()) return;
  ctx.ready();
}

/* ---------- the invoices of a charge (Supercharger invoices: Tesla's, or a cost TeslaFi gave) */
// kv rows under the cost: the invoice (its site and fees), an invoice a typed cost won over, and the PDF links.
// A charge without them gets hidden slots, so the items are always on the page.
function invoiceRows(c, car, charge) {
  const list = Array.isArray(c.invoices) ? c.invoices : [];
  const used = appliedInvoice(c);
  const usage = (f) => (f.usage == null ? null : f.unit === 'kwh' ? format(f.usage, 'energy') : f.unit === 'min' ? formatDuration(f.usage * 60000) : null);
  const fees = (i) => (i.fees || []).map((f) => `${feeLabel(f.kind).toLowerCase()} ${formatMoney(f.amount, i.currency)}${usage(f) ? ` for ${usage(f)}` : ''}`).join(', ');
  const word = (i) => invoiceSourceLabel(i.source).toLowerCase();
  const rows = [];
  if (used) rows.push([word(used), [used.site || '—', fees(used) ? none(`; ${fees(used)}`) : null], { cover: 'inv.charge.source' }]);
  for (const i of unusedInvoices(c)) rows.push([word(i), [formatMoney(i.total, i.currency), none(' (an entered cost is used instead)')], { cover: 'inv.charge.source' }]);
  const pdfs = list.filter((i) => i.pdf);
  if (pdfs.length) {
    rows.push(['invoice', h('span', null, pdfs.map((i, n) => [n ? ' ' : null,
      h('a', { class: 'lnk', href: chargeInvoicePdfUrl(car, charge, i.id), download: true, title: i.invoice_name || null }, `invoice (pdf)${pdfs.length > 1 ? ` ${n + 1}` : ''}`)])), { cover: 'inv.charge.pdf' }]);
  }
  const slots = [!rows.some((r) => r[2].cover === 'inv.charge.source') ? h('span', { hidden: true, cover: 'inv.charge.source' }) : null,
    !pdfs.length ? h('span', { hidden: true, cover: 'inv.charge.pdf' }) : null];
  return { rows, slots };
}

/* ---------- the cost editor: a total, a price per kWh or per minute, the currency and the energy basis */
function costEditor(ctx, { c, base, manual, cur, gridKwh, dc }) {
  const can = !!ctx.admin;
  const minutes = (c.charging_ms || 0) / 60000;
  const startMode = manual?.mode && manual.mode !== 'tariff' ? manual.mode : 'total';
  const startAmount = manual ? manual.amount : c.cost ? c.cost.amount : null;
  const mode = field({ label: 'amount is', name: 'mode', type: 'select', value: startMode, path: 'mode',
    options: [['total', 'the total for this charge'], ['per_kwh', 'a price per kwh'], ['per_minute', 'a price per minute']] });
  const amount = field({ label: 'amount', name: 'amount', type: 'number', value: startAmount ?? '', required: true, path: 'amount',
    hint: 'rates may have up to 5 decimals (0.34500); only the total is rounded. below zero is refused: type a credit as 0.',
    attrs: { min: '0', max: '100000', step: startMode === 'total' ? '0.01' : '0.00001', inputmode: 'decimal' } });
  const currency = field({ label: 'currency', name: 'currency', type: 'select', value: manual?.currency || cur, path: 'currency',
    options: [...new Set([manual?.currency || cur, ...CURRENCIES])].map((k) => [k, k]) });
  const basis = field({ label: 'per kwh of', name: 'energy_basis', type: 'select', path: 'energy_basis', value: manual?.energy_basis || (gridKwh != null ? 'grid' : 'pack'),
    options: [...(gridKwh != null ? [['grid', `energy drawn (${format(gridKwh, 'energy')})`]] : []), ['pack', `energy added (${format(c.energy_added_kwh, 'energy')})`]],
    hint: dc ? 'on a fast charge the grid energy is unknown, so a price per kwh is over the energy added' : null });
  const preview = h('p', { class: 'note', 'aria-live': 'polite' });
  const sync = () => {
    const md = mode.input.value;
    amount.input.step = md === 'total' ? '0.01' : '0.00001';
    basis.el.hidden = md !== 'per_kwh';
    const a = amount.input.value === '' ? null : Number(amount.input.value);
    const kwh = basis.input.value === 'grid' && gridKwh != null ? gridKwh : c.energy_added_kwh;
    const cu = currency.input.value;
    const total = a == null || !Number.isFinite(a) ? null : md === 'total' ? a : md === 'per_kwh' ? a * (kwh || 0) : a * minutes;
    replace(preview, total == null ? 'type an amount to see the total'
      : md === 'total' ? `= ${formatMoney(total, cu)} for the whole charge`
        : md === 'per_kwh' ? `${format(kwh, 'energy')} x ${format(a, 'price_energy', { currency: cu, digits: 5 })} = ${formatMoney(total, cu)}`
          : `${formatNumber(minutes, { maximumFractionDigits: 1 })} min x ${format(a, 'price_time', { currency: cu, digits: 5 })} = ${formatMoney(total, cu)}`);
  };
  for (const fl of [mode, amount, currency, basis]) { fl.input.addEventListener('input', sync); fl.input.addEventListener('change', sync); }
  const removeBtn = h('button', { class: 'btn btn--danger', type: 'button', hidden: !manual, onclick: async () => {
    removeBtn.disabled = true;
    try { await ctx.api.del(`${base}/cost`); toast('Typed cost removed: the place price applies again.', { next: true }); ctx.reload(); } catch (err) { f.setMessage(err?.message || 'The cost could not be removed.'); removeBtn.disabled = false; }
  } }, 'remove the typed cost');
  const f = form({
    fields: [mode, amount, currency, basis], submit: manual ? 'save the new cost' : 'save this cost', secondary: removeBtn,
    async onSubmit(v) {
      if (!can) return;
      const body = { mode: v.mode, amount: Number(v.amount), currency: v.currency };
      if (v.mode === 'per_kwh') body.energy_basis = v.energy_basis;
      await ctx.api.put(`${base}/cost`, body);
      toast('Cost saved. It stays with this charge when charges are rebuilt.', { next: true });
      ctx.reload();
    },
  });
  f.el.insertBefore(preview, f.el.querySelector('.frm-msg'));
  cover(f.el, 'cd.link.set_cost', 'web.charge-cost.entry');
  cover(mode.el, 'web.charge-cost.mode');
  cover(amount.el, 'web.charge-cost.amount', 'web.charge-cost.rate-precision');
  cover(f.el.querySelector('.acts'), 'web.charge-cost.save', 'web.charge-cost.clear-and-negative');
  cover(f.el.querySelector('.frm-msg'), 'web.charge-cost.error');
  sync();
  const fs = h('fieldset', { class: 'fs-plain', disabled: !can }, f.el);
  return h('div', null, h('h3', { class: 'sub-title' }, manual ? 'change the typed cost' : 'type the cost'),
    note(can ? 'for a receipt that differs from the place price. a typed cost is kept when charges are rebuilt; remove it to go back to the place price.' : 'only an admin can type or change a cost.'),
    fs);
}
