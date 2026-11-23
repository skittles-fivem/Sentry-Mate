// Sentry Mate, "Pebble" UI pack: Charge (/cars/{car}/charges/{charge}), one charging session in full.
//
// A place tile with the map and the level ring, number tiles (energy added and drawn, cost, time, power, efficiency,
// temperature, battery and range from start to end), the power and battery chart and the supply chart sharing one
// cursor and zoom, the power curve against battery level (with this car's usual fast charge), and the cost: what it
// is, how it was made, and an editor for an admin to enter a receipt's amount (total, per kWh or per minute).
// Data: GET /api/v1/cars/{car}/charges/{charge}, /curve, /places/{place}, /charging-stats/dc-curve; PUT and DELETE
// .../charges/{charge}/cost.

import { h } from '../shared/dom.js';
import { format, formatMoney } from '../shared/units.js';
import * as TI from '../shared/tiers.js';
import { formatTime, formatDate, formatDuration, zoneName, dayKey, dayRange } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { tile, num, chip, btn, linkBtn, icon, note, ring } from '../core/ui.js';
import { emptyTile } from '../core/states.js';
import { field, form, segmented } from '../core/form.js';
import { mapTile } from '../core/map.js';
import { timeChart, xyChart } from '../core/chart.js';
import { statTile, facts, socBar, money } from '../core/bits.js';
import { srcPill, estPill, isImported, isEstimated, apiOnly, EST_WHY } from '../core/source.js';
import { appliedInvoice, unusedInvoices, invoiceSourceLabel, feeLabel, chargeInvoicePdfUrl } from '../shared/invoices.js';

export const id = 'charge';
export const css = true;
export const title = () => 'Charge';

const FIELDS = 'power_kw,ac_line_kw,soc_pct,usable_soc_pct,range_km,battery_c,outside_c,heater_kw,ac_volts_v,ac_amps_a,pilot_a,phases,dc_volts_v,dc_amps_a';
const KIND_WORDS = { ac: 'AC charge', dc: 'Fast charge (DC)', unknown: 'Charge' };
const COST_SOURCE = { tariff: 'From the place price', manual: 'Entered by hand', free_supercharging: 'Free Supercharging', teslamate: 'Imported', tesla_invoice: 'From Tesla invoice', teslafi: 'From TeslaFi' };
const CURRENCIES = ['USD', 'EUR', 'GBP', 'CAD', 'AUD', 'NZD', 'CHF', 'SEK', 'NOK', 'DKK', 'JPY', 'CNY'];
const has = (l) => (l || []).some((x) => x != null);
const placeName = (c) => c.place?.name || c.address?.short || 'Unnamed spot';
const placeHref = (c) => (c.place?.id ? state.buildUrl('places', { query: { place: c.place.id } })
  : c.position?.lat != null ? state.buildUrl('places', { query: { new: `${c.position.lat.toFixed(5)},${c.position.lon.toFixed(5)}` } }) : null);

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz;
  const base = api.apiPath`cars/${car.id}/charges/${ctx.chargeId}`;
  const back = linkBtn(`${car.name}’s charges`, state.buildUrl('charges', { car: car.id, period: ctx.state.period }), { kind: 'soft', icon: 'chevronLeft', cover: 'web.charge-cost.back' });
  let c;
  try { c = await ctx.api.get(base); } catch (err) {
    if (err && err.status === 404) {
      ctx.head({ title: 'No such charge' });
      ctx.view.replaceChildren(emptyTile({ icon: 'bolt', title: 'There is no charge with this number', text: 'It may have been rebuilt under a new number, or the link is wrong. Find it on Charges.', action: back, cover: 'web.charge-cost.not-found' }));
      ctx.ready();
      return {};
    }
    throw err;
  }
  if (!ctx.alive()) return {};
  const live = !!c.flags?.in_progress || c.end_ms == null;
  const t0 = c.start_ms, t1 = c.end_ms ?? api.now();
  const dc = c.kind === 'dc', ac = c.kind === 'ac';
  const imported = isImported(c) && !c.sources?.some((x) => x.kind !== 'import');
  const estimated = isEstimated(c) || imported;
  const keep = (p) => p.catch(() => null);
  const [curve, place, typical] = await Promise.all([
    keep(ctx.api.get(`${base}/curve`, { fields: FIELDS, max_points: 2000 })),
    c.place?.id ? keep(ctx.api.get(api.apiPath`places/${c.place.id}`)) : null,
    dc ? keep(ctx.api.get(api.apiPath`cars/${car.id}/charging-stats/dc-curve`, { from_ms: t0 - 365 * 864e5, to_ms: t1 + 1 })) : null,
  ]);
  if (!ctx.alive()) return {};

  const when = `${formatDate(t0, tz, { now: api.now() })}, ${formatTime(t0, tz)}`;
  ctx.title(`Charge ${when}`);
  const step = (cid, text, ic, rel) => (cid ? linkBtn(text, state.buildUrl('charge', { car: car.id, charge: cid }), { kind: 'soft', icon: ic, cls: `step-${rel}` })
    : h('span', { class: 'btn btn-soft is-off', 'aria-disabled': 'true', title: rel === 'prev' ? 'The first charge recorded' : 'The newest charge' }, icon(ic), h('span', { class: 'btn-text' }, text)));
  ctx.head({ title: placeName(c), actions: [step(c.prev, 'Earlier', 'chevronLeft', 'prev'), step(c.next, 'Later', 'chevronRight', 'next')] });
  if (live) ctx.events(['rows'], (ev) => { if (ev.data?.car === car.id && (ev.data.kinds || []).includes('charges')) ctx.reload(); });

  /* ------------------------------------------------------------ the line under the header */
  const zone = zoneName(t0, tz);
  const day = dayKey(t0, tz);
  const ctxBar = h('div', { class: 'page-bar w-full', cover: ['cd.context.session', 'shots.charge-details', 'web.charge-cost.breadcrumb'] },
    back,
    h('a', { class: 'fpill fpill-ic', href: state.buildUrl('car', { car: car.id }), cover: ['cd.filter.car', 'web.charge-cost.car'] }, icon('car'), car.name),
    h('a', { class: 'fpill fpill-ic', href: state.buildUrl('timeline', { car: car.id, period: dayRange(day, day, tz) }), cover: 'cd.filter.time_range', title: 'The whole day on the timeline' }, icon('calendar'),
      `${formatDate(t0, tz, { weekday: 'long', month: 'long', now: api.now() })}, ${formatTime(t0, tz)} – ${live ? 'now' : formatTime(t1, tz)} ${zone}`),
    chip(KIND_WORDS[c.kind] || 'Charge', { tone: dc ? 'sky' : ac ? 'teal' : 'muted' }),
    live ? chip('Charging now', { tone: 'teal' }) : null,
    c.has_gaps ? chip('Some data missing', { tone: 'bad' }) : null,
    srcPill(c), estPill(c));

  /* ------------------------------------------------------------ where, and the level */
  const pHref = placeHref(c);
  const mapBox = h('div', { class: 'map' });
  const whereT = tile({ w: 2, title: 'Where', icon: 'pin', cls: ['map-tile', 'where-tile'], cover: ['cd.map.location', 'web.charge-cost.map'],
    side: pHref ? h('a', { class: 'tile-link', href: pHref, cover: ['cd.stat.location', 'web.charge-cost.location'] }, c.place ? 'Edit this place' : 'Save as a place', icon('chevronRight')) : h('span', { cover: ['cd.stat.location', 'web.charge-cost.location'] }, 'No position') },
  mapBox, h('p', { class: 'map-foot' }, h('span', null, h('b', null, placeName(c)), c.place && c.address?.short ? ` · ${c.address.short}` : c.place ? '' : ' · not a saved place'),
    h('span', null, c.fast_charger ? [c.fast_charger.brand, c.fast_charger.type === 'supercharger' ? 'Supercharger' : c.fast_charger.type, c.fast_charger.class].filter(Boolean).join(' ') : format(c.odometer_km, 'odometer'))));
  const socAdded = c.end_soc_pct != null && c.start_soc_pct != null ? c.end_soc_pct - c.start_soc_pct : null;
  const levelT = tile({ w: 2, title: 'Battery', icon: 'battery', cls: 'level-tile' },
    h('div', { class: 'level-top' },
      ring(c.end_soc_pct, { label: live ? 'Now' : 'End', size: 'l', tone: 'coral' }),
      h('div', { class: 'level-nums' },
        h('div', { cover: ['cd.stat.soc_start', 'cd.stat.range_start'] }, h('span', { class: 'hero-k' }, 'Start'), num(c.start_soc_pct, 'percent', { size: 'm' }), h('span', { class: 'tile-line' }, format(c.start_range_km, 'range'))),
        h('div', { cover: ['cd.stat.soc_added', 'cd.stat.range_added'] }, h('span', { class: 'hero-k' }, 'Added'), num(socAdded, 'percent', { size: 'm', why: 'not known' }), h('span', { class: 'tile-line' }, `+${format(c.range_added_km, 'range')}`)),
        h('div', { cover: ['cd.stat.soc_end', 'cd.stat.range_end'] }, h('span', { class: 'hero-k' }, live ? 'Now' : 'End'), num(c.end_soc_pct, 'percent', { size: 'm' }), h('span', { class: 'tile-line' }, format(c.end_range_km, 'range'))))),
    socBar(c.start_soc_pct, c.end_soc_pct, { tone: 'coral' }),
    isEstimated(c) && apiOnly(c) ? note(EST_WHY) : null,
    note(`Battery % as the car’s screen shows it; range is rated range.${c.energy_added_kwh && socAdded > 0 ? ` About ${format(c.energy_added_kwh / socAdded, 'energy')} per %.` : ''}`));

  /* ------------------------------------------------------------ figures */
  const why = live ? 'still charging' : imported ? 'not in imported history' : 'not recorded';
  const cost = c.cost;
  const figs = [
    statTile({ label: 'Energy added', value: c.energy_added_kwh, kind: 'energy', icon: 'bolt', cover: 'cd.stat.energy_added', why, sub: estimated ? (apiOnly(c) ? 'estimated from Tesla API readings' : 'estimated (imported)') : 'into the pack' }),
    ac ? statTile({ label: 'Energy drawn', value: c.energy_grid_kwh, kind: 'energy', icon: 'plug', cover: 'cd.stat.energy_used', why, sub: c.losses_kwh != null ? `${format(c.losses_kwh, 'energy')} lost on the way` : 'from the supply' })
      : statTile({ label: dc ? 'Delivered' : 'Energy drawn', value: dc ? c.dc_inlet_kwh : null, kind: 'energy', icon: 'plug', cover: 'cd.stat.energy_used', why: dc ? why : 'AC or DC not known', sub: dc ? (c.fast_charger?.billed_kwh != null ? `billed ${format(c.fast_charger.billed_kwh, 'energy')}` : 'at the inlet') : null }),
    tile({ w: 1, title: 'Cost', icon: 'coin', cls: 'tile-stat', cover: ['cd.stat.cost', 'web.charge-cost.current-cost'], href: null },
      cost ? money([cost]) : num(null, 'number', { why: c.place ? 'the place has no price' : 'not a priced place' }),
      h('p', { class: 'tile-line' }, cost ? COST_SOURCE[cost.source] || cost.source : ctx.admin ? 'enter it below' : 'no cost yet')),
    statTile({ label: 'Charging time', value: c.charging_ms, kind: 'duration', icon: 'clock', cover: 'cd.stat.duration', sub: c.plugged_ms != null ? `plugged in ${formatDuration(c.plugged_ms)}` : null }),
    statTile({ label: 'Average power', value: c.power_avg_kw, kind: 'power', icon: 'gauge', cover: 'cd.stat.avg_power', why, sub: c.power_max_kw != null ? `peak ${format(c.power_max_kw, 'power')}` : null }),
    statTile({ label: 'Efficiency', value: c.efficiency_pct, kind: 'percent', icon: 'leaf', cover: 'cd.stat.efficiency', why: dc ? 'DC: the charger’s losses are not sent' : c.efficiency_note || why, sub: c.efficiency_pct != null ? 'into the pack over drawn' : null }),
    statTile({ label: 'Outside', value: c.outside_avg_c, kind: 'temperature', icon: 'thermo', cover: 'cd.stat.avg_temp', why, sub: c.battery_avg_c != null ? `battery ${format(c.battery_avg_c, 'temperature')}` : 'average' }),
    statTile({ label: 'Range per hour', value: c.range_per_hour_km, kind: 'range_rate', icon: 'route', why, sub: `${format(c.odometer_km, 'odometer')} on the clock` }),
  ];

  /* ------------------------------------------------------------ charts */
  const t = curve?.t_ms || [];
  const V = curve?.values || {};
  const sync = `charge-${c.id}-${Date.now()}`;
  const gaps = (c.gaps || []).map((g) => ({ state: 'unknown', start_ms: g.start_ms, end_ms: g.end_ms }));
  const cPower = tile({ w: 2, title: 'Power and battery', icon: 'bolt', cls: 'chart-tile', cover: ['cd.chart.timeline', 'cd.chart.power', 'cd.chart.soc'] });
  const cRange = tile({ w: 2, title: 'Range, heater and outside', icon: 'thermo', cls: 'chart-tile', cover: ['cd.chart.range', 'cd.chart.battery_heater', 'cd.chart.outdoor_temp'] });
  const cSupply = tile({ w: 2, title: dc ? 'The charger' : 'The supply', icon: 'plug', cls: 'chart-tile', cover: ['cd.chart.ac_voltage', 'cd.chart.ac_current', 'cd.chart.pilot_current', 'cd.chart.phases'] });
  const cCurve = tile({ w: 2, title: 'Power against battery %', icon: 'gauge', cls: 'chart-tile', cover: ['cd.chart.curve_points', 'cd.chart.curve_avg'] });

  /* ------------------------------------------------------------ cost */
  const costT = tile({ w: 2, title: 'Cost', icon: 'coin', id: 'cost', cls: 'cost-tile', cover: ['cd.link.set_cost', 'web.charge-cost.entry'] });
  const bd = c.cost_breakdown || { parts: [] };
  const price = place?.price || null;
  const cur = cost?.currency || price?.currency || 'USD';
  const manual = c.manual_cost || null;
  costT.append(
    h('div', { class: 'cost-now' }, money(cost ? [cost] : [], { size: 'l', why: 'no cost yet' }),
      h('p', { class: 'tile-line' }, cost ? `${COST_SOURCE[cost.source] || cost.source}${c.price_per_kwh?.value != null ? ` · ${format({ amount: c.price_per_kwh.value, currency: cost.currency }, 'price_energy')} over the ${c.price_per_kwh.basis === 'grid' ? 'energy drawn' : 'energy added'}` : ''}`
        : c.place ? `${c.place.name} has no price yet: add one on Places, or enter the cost here.` : 'Not at a saved place with a price: enter the cost here.')),
    invoiceLines(c, car.id, ctx.chargeId),
    facts([
      ['When', h('span', { cover: 'web.charge-cost.dates' }, `${formatDate(t0, tz, { weekday: false })} ${formatTime(t0, tz)} – ${live ? 'now' : formatTime(t1, tz)} ${zone}`)],
      ['Charging', h('span', { cover: 'web.charge-cost.duration' }, formatDuration(c.charging_ms))],
      ['Energy', h('span', { cover: 'web.charge-cost.energy' }, `${format(c.energy_added_kwh, 'energy')} added${c.energy_grid_kwh != null ? `, ${format(c.energy_grid_kwh, 'energy')} drawn` : ''}`)],
      ...(bd.parts || []).map((p) => [p.from_kwh != null ? `${p.rate}: ${TI.stepLabel(p.from_kwh, p.to_kwh)}` : p.rate || 'Rate', `${p.per_kwh != null ? `${format({ amount: p.per_kwh, currency: cur }, 'price_energy')} × ${format(p.kwh, 'energy')}` : p.per_minute != null ? `${format({ amount: p.per_minute, currency: cur }, 'price_time')} × ${formatNumber(p.minutes, { maximumFractionDigits: 1 })} min` : 'one amount'} = ${formatMoney(p.cost, cur)}`]),
      bd.session_fee ? ['Session fee', formatMoney(bd.session_fee, cur)] : null,
      bd.cycle ? ['Tiered price', `${TI.splitText(TI.stepParts(bd), cur)}; the billing cycle from ${formatDate(bd.cycle.start_ms, place?.time_zone || tz, { weekday: false })} had ${format(bd.cycle.before_kwh, 'energy')} before this charge`] : null,
    ].filter(Boolean)),
    costEditor(ctx, { c, base, manual, cur }),
    h('p', { class: 'note', cover: 'web.charge-cost.not-found' }, 'A link to a charge that was rebuilt under a new number says so and points back to Charges.'));

  ctx.view.replaceChildren(ctxBar, whereT, levelT, ...figs, cPower, cRange, cSupply, cCurve, costT);

  /* draw: map, then charts */
  const work = [];
  if (c.position?.lat != null) {
    work.push((async () => {
      const m = await mapTile(ctx, mapBox, { center: [c.position.lon, c.position.lat], zoom: 15 });
      if (!m || !m.map) return;
      m.pin([c.position.lon, c.position.lat], { tone: 'teal', title: placeName(c) });
      await m.ready;
    })());
  } else mapBox.append(h('p', { class: 'map-note' }, 'No GPS position for this charge.'));

  if (!t.length) {
    for (const x of [cPower, cRange, cSupply, cCurve]) x.append(note(live ? 'No readings yet.' : 'No readings were recorded through this charge.'));
  } else {
    const common = { tz, t_ms: t, sync, spans: gaps };
    work.push(timeChart(ctx, cPower, { ...common, label: 'Power and battery', series: [
      { key: 'power', name: 'Power', kind: 'power', values: V.power_kw, tone: 'c1', fill: true, floor: 0 },
      { key: 'soc', name: 'Battery', kind: 'percent', values: V.soc_pct, tone: 'c2', dashed: true },
    ] }));
    work.push(timeChart(ctx, cRange, { ...common, label: 'Range, heater and outside', series: [
      { key: 'range', name: 'Range', kind: 'range', values: V.range_km, tone: 'c1', axis: 'left' },
      { key: 'heat', name: 'Heater', kind: 'power', values: V.heater_kw, tone: 'c2', dashed: true, axis: 'right', floor: 0 },
    ] }));
    cRange.append(note(`Outside it was ${format(V.outside_c?.find((x) => x != null), 'temperature')} at the start and ${format([...(V.outside_c || [])].reverse().find((x) => x != null), 'temperature')} at the end.${has(V.heater_kw) && V.heater_kw.some((x) => x > 0) ? ' The battery heater ran.' : ' The battery heater did not run.'}`));
    if (ac && (has(V.ac_volts_v) || has(V.ac_amps_a))) {
      work.push(timeChart(ctx, cSupply, { ...common, label: 'The supply', series: [
        { key: 'v', name: 'Voltage', kind: 'voltage', values: V.ac_volts_v, tone: 'c1', axis: 'left' },
        { key: 'a', name: 'Current', kind: 'current', values: V.ac_amps_a, tone: 'c2', dashed: true, axis: 'right', floor: 0 },
        ...(has(V.pilot_a) ? [{ key: 'p', name: 'Pilot', kind: 'current', values: V.pilot_a, tone: 'c3', axis: 'right' }] : []),
      ] }));
      const ph = [...new Set((V.phases || []).filter((x) => x != null))];
      cSupply.append(note(ph.length ? `${ph.join(', then ')} phase${ph.length === 1 && ph[0] === 1 ? '' : 's'}.` : 'The car did not say how many phases.', has(V.pilot_a) ? '' : ' No pilot current was sent.'));
    } else if (dc && (has(V.dc_volts_v) || has(V.dc_amps_a))) {
      work.push(timeChart(ctx, cSupply, { ...common, label: 'The charger', series: [
        { key: 'v', name: 'Voltage', kind: 'voltage', values: V.dc_volts_v, tone: 'c1', axis: 'left' },
        { key: 'a', name: 'Current', kind: 'current', values: V.dc_amps_a, tone: 'c2', dashed: true, axis: 'right', floor: 0 },
      ] }));
      cSupply.append(note('A fast charger turns AC into DC itself, so the car sees its DC volts and amps, not the grid, the phases or a pilot current.'));
    } else cSupply.append(note(dc ? 'A fast charger turns AC into DC itself; this one sent no volts or amps, and there are no phases or pilot current.' : 'No supply voltage, current, phases or pilot current were sent for this charge.'));

    // power against battery %: each reading, this session's mean per %, the usual fast charge
    const pts = [];
    t.forEach((_, i) => { const s = V.soc_pct?.[i], p = V.power_kw?.[i]; if (s != null && p != null && p > 0.05) pts.push({ x: s, y: p }); });
    const bySoc = new Map();
    for (const p of pts) { const k = Math.round(p.x); const e = bySoc.get(k) || [0, 0]; e[0] += p.y; e[1] += 1; bySoc.set(k, e); }
    const xs = [...bySoc.keys()].sort((a, b) => a - b);
    const med = new Map((typical?.median || []).map((m) => [m.soc_pct, m.power_kw]));
    if (pts.length > 1) {
      work.push(xyChart(ctx, cCurve, { label: 'Power against battery level', xs, xKind: 'percent', yKind: 'power', dots: pts.length > 1500 ? pts.filter((_, i) => i % Math.ceil(pts.length / 1500) === 0) : pts, dotsName: 'Readings',
        series: [{ name: 'Mean per %', kind: 'power', values: xs.map((k) => bySoc.get(k)[0] / bySoc.get(k)[1]), tone: 'c3' },
          ...(dc && med.size ? [{ name: 'Usual fast charge', kind: 'power', values: xs.map((k) => med.get(k) ?? null), tone: 'c2', dashed: true }] : [])] }));
      cCurve.append(note(dc ? 'A fast charge slows as the battery fills; a cold pack lowers the whole curve.' : 'AC charging is held by the supply, so the line stays flat until the end.'));
    } else cCurve.append(note('Too few readings to draw.'));
  }
  await Promise.all(work);
  if (!ctx.alive()) return {};
  ctx.ready();
  if (ctx.query.edit === 'cost') { costT.scrollIntoView({ block: 'start' }); costT.querySelector('input, select')?.focus({ preventScroll: true }); }
  return {};
}

/**
 * The charge's Supercharger invoices: where the cost came from with the site and its fees, and a download
 * button per invoice PDF. Both are always in the tile (hidden when there is no invoice), so the item is there.
 */
function invoiceLines(c, car, charge) {
  const list = Array.isArray(c.invoices) ? c.invoices : [];
  const used = appliedInvoice(c);
  const usage = (f) => (f.usage == null ? '' : f.unit === 'kwh' ? ` for ${format(f.usage, 'energy')}` : f.unit === 'min' ? ` for ${formatDuration(f.usage * 60000)}` : '');
  const fees = (i) => (i.fees || []).map((f) => `${feeLabel(f.kind)} ${formatMoney(f.amount, i.currency)}${usage(f)}`).join(' · ');
  const lines = [];
  if (used) lines.push(h('span', null, h('b', null, used.site || invoiceSourceLabel(used.source)), fees(used) ? ` · ${fees(used)}` : ''));
  for (const i of unusedInvoices(c)) lines.push(h('span', null, `${invoiceSourceLabel(i.source)}: ${formatMoney(i.total, i.currency)} (an entered cost is used instead)`));
  const pdfs = list.filter((i) => i.pdf);
  const links = pdfs.map((i, n) => {
    const a = linkBtn(pdfs.length > 1 ? `Invoice ${n + 1} (PDF)` : 'Invoice (PDF)', chargeInvoicePdfUrl(car, charge, i.id), { kind: 'soft', icon: 'download' });
    a.setAttribute('download', '');
    if (i.invoice_name) a.title = i.invoice_name;
    return a;
  });
  return h('div', { class: 'inv-box', hidden: !list.length },
    h('p', { class: 'tile-line inv-lines', cover: 'inv.charge.source' }, lines),
    h('div', { class: 'inv-pdfs', cover: 'inv.charge.pdf', hidden: !links.length }, links));
}

/** The cost editor: a total, a price per kWh or per minute, the currency and the energy basis. */
function costEditor(ctx, { c, base, manual, cur }) {
  const can = !!ctx.admin;
  const minutes = (c.charging_ms || 0) / 60000;
  const grid = c.energy_grid_kwh;
  let mode = manual?.mode && manual.mode !== 'tariff' ? manual.mode : 'total';
  const modeSeg = segmented({ label: 'The amount is', name: 'mode', value: mode, cover: 'web.charge-cost.mode',
    options: [['total', 'The total'], ['per_kwh', 'Per kWh'], ['per_minute', 'Per minute']], onchange: (v) => { mode = v; sync(); } });
  const amount = field({ label: 'Amount', name: 'amount', type: 'number', value: manual ? manual.amount : c.cost ? c.cost.amount : '', cover: ['web.charge-cost.amount', 'web.charge-cost.rate-precision'],
    hint: 'Rates may have up to 5 decimals (0.34500). Below zero is refused: enter a credit as 0.', attrs: { min: '0', max: '100000', step: '0.00001', inputmode: 'decimal', disabled: !can } });
  const currency = field({ label: 'Currency', name: 'currency', type: 'select', value: manual?.currency || cur, options: [...new Set([manual?.currency || cur, ...CURRENCIES])].map((k) => [k, k]), attrs: { disabled: !can } });
  const basis = field({ label: 'Per kWh of', name: 'energy_basis', type: 'select', value: manual?.energy_basis || (grid != null ? 'grid' : 'pack'), attrs: { disabled: !can },
    options: [...(grid != null ? [['grid', `Energy drawn (${format(grid, 'energy')})`]] : []), ['pack', `Energy added (${format(c.energy_added_kwh, 'energy')})`]] });
  const preview = h('p', { class: 'cost-preview', 'aria-live': 'polite' });
  function sync() {
    basis.hidden = mode !== 'per_kwh';
    const a = amount.control.value === '' ? null : Number(amount.control.value);
    const kwh = basis.control.value === 'grid' && grid != null ? grid : c.energy_added_kwh;
    const k = currency.control.value;
    const total = a == null || !Number.isFinite(a) ? null : mode === 'total' ? a : mode === 'per_kwh' ? a * (kwh || 0) : a * minutes;
    preview.textContent = total == null ? 'Type an amount to see the total.'
      : mode === 'total' ? `${formatMoney(total, k)} for the whole charge.`
        : mode === 'per_kwh' ? `${format(kwh, 'energy')} × ${format({ amount: a, currency: k }, 'price_energy', { digits: 5 })} = ${formatMoney(total, k)}.`
          : `${formatNumber(minutes, { maximumFractionDigits: 1 })} min × ${format({ amount: a, currency: k }, 'price_time', { digits: 5 })} = ${formatMoney(total, k)}.`;
  }
  for (const x of [amount, currency, basis]) { x.control.addEventListener('input', sync); x.control.addEventListener('change', sync); }
  const remove = manual && can ? btn('Remove the entered cost', { kind: 'ghost', icon: 'reset', onclick: async () => {
    try { await ctx.api.del(`${base}/cost`); ctx.toast('Entered cost removed: the place price applies again.', { next: true }); ctx.reload(); } catch (err) { ctx.toast(err.message || 'The cost could not be removed.', { kind: 'error' }); }
  } }) : null;
  const f = form({
    fields: [modeSeg, amount, currency, basis], submitLabel: manual ? 'Save the new cost' : 'Save this cost', extra: preview, secondary: remove, cls: 'cost-form',
    async onsubmit(v) {
      if (!can) return;
      const body = { mode, amount: v.amount, currency: v.currency };
      if (mode === 'per_kwh') body.energy_basis = v.energy_basis;
      await ctx.api.put(`${base}/cost`, body);
      ctx.toast('Cost saved. It stays with this charge when charges are rebuilt.', { next: true });
      ctx.reload();
    },
  });
  f.querySelector('.form-actions')?.setAttribute('data-cover', 'web.charge-cost.save web.charge-cost.clear-and-negative');
  f.querySelector('.form-error')?.setAttribute('data-cover', 'web.charge-cost.error');
  if (!can) f.submitButton.disabled = true;
  sync();
  return h('div', { class: 'cost-edit' },
    h('h3', { class: 'sub-head' }, manual ? 'Change the entered cost' : 'Enter the cost'),
    note(can ? 'For a receipt that differs from the place price. It is kept when charges are rebuilt; remove it to go back to the place price.' : 'Only an admin can enter or change a cost.'),
    f);
}
