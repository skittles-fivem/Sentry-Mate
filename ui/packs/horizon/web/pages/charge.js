// Charge (/cars/{car}/charges/{charge}): one charging session, read like a charger's display. The plate has
// the way back, the charges before and after and the drives around it. The session panel shows the battery
// as a fuel gauge from start to end, the charge computer (time, energy in and from the supply, efficiency,
// power, temperatures, range) and a map of where it was. Then the session at 1 Hz (GET .../curve) on synced
// traces with drag to zoom (battery, power, range, temperatures, pack heating), the supply (line volts and
// amps, the station's pilot current, phases; a fast charger's own volts and amps), power against battery %
// over this car's usual fast charge (GET .../charging-stats/dc-curve), and the cost with its parts and an
// editor (PUT and DELETE .../cost, admin) that survives rebuilds.

import * as state from '../shared/state.js';
import { h, replace, cssVar, cover } from '../shared/dom.js';
import { column, timeColumn } from '../shared/csv.js';
import { format, formatMoney, getUnits } from '../shared/units.js';
import { formatDate, formatTime, formatDuration, zoneName, dayKey, dayRange, DAY } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon, hud, plate, btn, linkBtn, tag, kv, note } from '../core/ui.js';
import { counters, rack, stat, csvBtn, placeLink, placeLabel } from '../core/kit.js';
import { srcChip, estTag, kindsOf, estimated } from '../core/sources.js';
import { fuel } from '../core/gauges.js';
import { tracks } from '../core/tracks.js';
import { loadUPlot, mountPlot, scope, valueAxis, tone, alpha, chartFont, well } from '../core/chart.js';
import { createMap, pin, mapFallback } from '../core/map.js';
import { rpx } from '../core/display.js';
import { emptyState } from '../core/states.js';
import { field, form } from '../core/form.js';
import { toast } from '../core/layer.js';

export const id = 'charge';
export const css = true;
export const title = () => 'Charge';

const enc = encodeURIComponent;
const CURRENCIES = ['USD', 'CAD', 'EUR', 'GBP', 'NOK', 'SEK', 'DKK', 'CHF', 'AUD', 'NZD', 'JPY', 'CNY'];
const COST_SOURCE = { tariff: 'from the place price', manual: 'typed by hand', free_supercharging: 'free Supercharging on this car', teslamate: 'imported from TeslaMate' };
const LIMIT = { station: 'the station\'s pilot signal', cable: 'the cable', car: 'the car\'s own maximum', setting: 'the current set on the screen' };
const FIELDS = ['power_kw', 'ac_line_kw', 'soc_pct', 'usable_soc_pct', 'range_km', 'battery_c', 'outside_c', 'heater_kw', 'ac_volts_v', 'ac_amps_a', 'pilot_a', 'phases', 'limit_source', 'dc_volts_v', 'dc_amps_a', 'volts_v', 'amps_a'];
const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
const rateText = (v, kind, currency) => format(v, kind, { currency, digits: clamp((String(+Number(v).toFixed(5)).split('.')[1] || '').length, 2, 5) });

export async function mount(ctx) {
  const { car, tz } = ctx;
  const cid = car.id, chid = ctx.state.charge;
  const base = `cars/${enc(cid)}/charges/${enc(chid)}`;
  let c;
  try { c = await ctx.api.get(base); } catch (e) {
    if (e.status === 404) e.message = `There is no charge with this number for ${car.name}. It may have been rebuilt under a new number; find it on Charges.`;
    ctx.fail(e, e.status === 404 ? { retry: null } : {});
    return;
  }
  if (!ctx.alive()) return;
  const live = !!c.flags?.in_progress || c.end_ms == null;
  const t0 = c.start_ms, t1 = c.end_ms ?? ctx.api.now();
  const dc = c.kind === 'dc', ac = c.kind === 'ac';
  const kinds = kindsOf(c);
  const imported = kinds.length ? !kinds.includes('smo') && kinds.includes('import') : c.source === 'teslamate';
  const apiOnly = kinds.length === 1 && kinds[0] === 'tesla_api';
  const estWord = estimated(c) || imported ? (apiOnly ? 'estimated from the Tesla API' : 'estimated (imported)') : null;
  const [curve, place, before, after, typical] = await Promise.all([
    ctx.api.get(`${base}/curve`, { fields: FIELDS.join(','), max_points: 4000 }).catch(() => null),
    c.place?.id ? ctx.api.get(`places/${enc(c.place.id)}`).catch(() => null) : null,
    ctx.api.get(`cars/${enc(cid)}/timeline`, { to_ms: t0, kinds: 'drive', limit: 1 }).catch(() => null),
    live ? null : ctx.api.get(`cars/${enc(cid)}/drives`, { from_ms: t1, to_ms: Math.min(t1 + 30 * DAY, ctx.api.now() + 1), sort: 'start', dir: 'asc', limit: 1 }).catch(() => null),
    dc ? ctx.api.get(`cars/${enc(cid)}/charging-stats/dc-curve`, { from_ms: t0 - 365 * DAY, to_ms: t1 + 1 }).catch(() => null) : null,
  ]);
  if (!ctx.alive()) return;
  const day = dayKey(t0, tz);
  const zn = zoneName(t0, tz);
  const when = `${formatDate(t0, tz, { year: false })}, ${formatTime(t0, tz)}`;
  ctx.setTitle(`Charge ${when}`);
  if (live) ctx.events('rows', (ev) => { if (ev.data?.car === cid && (!ev.data.kinds || ev.data.kinds.includes('charges'))) ctx.reload(); });
  const driveBefore = before?.items?.[0]?.ref?.drive || null;
  const driveAfter = after?.items?.[0]?.id || null;

  /* ---------- the plate */
  const trail = h('nav', { class: 'cg-trail', 'aria-label': 'Where this charge sits', cover: ['web.charge-cost.breadcrumb', 'cd.filter.car'] },
    h('a', { href: '/' }, 'All cars'), icon('chev-r', 'i--sm'), h('a', { href: ctx.link('car'), cover: 'web.charge-cost.car' }, car.name), icon('chev-r', 'i--sm'), h('a', { href: ctx.link('charges') }, 'Charges'));
  const tools = [
    linkBtn('Back', ctx.link('charges'), { icon: 'arrow-l', sm: true, kind: 'ghost', cover: 'web.charge-cost.back' }),
    c.prev ? h('a', { class: 'btn btn--sm btn--icon', href: state.buildUrl('charge', { car: cid, charge: c.prev }), 'aria-label': 'The charge before', title: 'The charge before' }, icon('seek-l', 'i--sm')) : null,
    c.next ? h('a', { class: 'btn btn--sm btn--icon', href: state.buildUrl('charge', { car: cid, charge: c.next }), 'aria-label': 'The charge after', title: 'The charge after' }, icon('seek-r', 'i--sm')) : null,
    driveBefore ? linkBtn('Drive before', state.buildUrl('drive', { car: cid, drive: driveBefore }), { icon: 'route', sm: true, kind: 'ghost' }) : null,
    driveAfter ? linkBtn('Drive after', state.buildUrl('drive', { car: cid, drive: driveAfter }), { icon: 'route', sm: true, kind: 'ghost' }) : null,
    linkBtn('That day', state.buildUrl('timeline', { car: cid, period: dayRange(day, day, tz) }), { icon: 'timeline', sm: true, kind: 'ghost' }),
  ];
  const head = plate({ kicker: trail, title: placeLabel({ ...c, address: c.address?.short }), tools,
    extra: h('p', { class: 'cg-sub' },
      live ? tag('Charging now', 'grn') : tag(dc ? 'Fast charge (DC)' : ac ? 'AC charge' : 'Charge', dc ? 'org' : 'cyan'),
      h('span', null, `${formatDate(t0, tz, { weekday: 'long', month: 'long' })}, ${formatTime(t0, tz)} to ${live ? 'now' : formatTime(t1, tz)} ${zn}`),
      h('span', null, format(c.energy_added_kwh, 'energy')), h('span', null, formatDuration(c.charging_ms)),
      srcChip(c, { full: true }), estTag(c)) });

  /* ---------- the charge computer */
  const why = live ? 'Still charging' : imported ? 'Not in imported history' : apiOnly ? 'Not sent by the Tesla API' : 'Not recorded';
  const socA = c.start_soc_pct, socB = c.end_soc_pct;
  const socAdded = socA != null && socB != null ? socB - socA : null;
  const cells = 25;
  const gauge = h('div', { class: 'cg-fuel', role: 'img', 'aria-label': `Battery from ${format(socA, 'percent')} to ${format(socB, 'percent')}` },
    fuel({ fraction: socB == null ? null : socB / 100, cells, low: 0.2, ends: ['0', '100 %'], label: 'Battery at the end' }),
    h('div', { class: 'cg-fuel-from', 'aria-hidden': 'true', vars: { '--a': `${clamp(socA ?? 0, 0, 100)}%` } }));
  const figures = h('div', { class: 'cg-ends' },
    h('div', { class: 'cg-end', cover: ['cd.stat.soc_start', 'cd.stat.range_start'] }, h('span', { class: 'stat-label' }, h('span', null, 'Start')), h('b', null, format(socA, 'percent')), h('span', null, format(c.start_range_km, 'range'))),
    h('div', { class: 'cg-added', cover: ['cd.stat.soc_added', 'cd.stat.range_added'] }, h('b', null, format(socAdded, 'percent', { sign: true })), h('span', null, format(c.range_added_km, 'range', { sign: true })),
      c.energy_added_kwh && socAdded > 0 ? h('span', { class: 'faint' }, `${format(c.energy_added_kwh / socAdded, 'energy')} per %`) : null),
    h('div', { class: 'cg-end cg-end--r', cover: ['cd.stat.soc_end', 'cd.stat.range_end'] }, h('span', { class: 'stat-label' }, h('span', null, live ? 'Now' : 'End')), h('b', null, format(socB, 'percent')), h('span', null, format(c.end_range_km, 'range'))));
  const drawn = ac ? { label: 'From the supply', value: c.energy_grid_kwh, kind: 'energy', how: estWord || 'AC line volts × amps' }
    : dc ? { label: 'From the charger', value: c.dc_inlet_kwh, kind: 'energy', how: c.fast_charger?.billed_kwh != null ? `billed ${format(c.fast_charger.billed_kwh, 'energy')}` : 'at the inlet; grid side not sent' }
      : { label: 'From the supply', value: null, kind: 'energy', how: 'not known whether AC or DC' };
  const computer = hud({ title: 'Charge computer', icon: 'bolt', tone: 'cyan', side: live ? tag('so far', 'grn') : null },
    gauge, figures,
    rack([
      stat('Charging time', c.charging_ms, 'duration', { cover: 'cd.stat.duration', how: c.plugged_ms != null ? `plugged in ${formatDuration(c.plugged_ms)}` : 'first to last charging second' }),
      stat('Energy added', c.energy_added_kwh, 'energy', { cover: 'cd.stat.energy_added', hot: true, how: estWord || 'into the pack', why }),
      stat(drawn.label, drawn.value, drawn.kind, { cover: 'cd.stat.energy_used', how: drawn.how, why }),
      stat('Efficiency', c.efficiency_pct, 'percent', { cover: 'cd.stat.efficiency', digits: 1, how: c.efficiency_pct != null ? `${format(c.losses_kwh, 'energy')} lost on the way` : null, why: dc ? 'DC: the charger\'s losses are not sent' : why }),
      stat('Average power', c.power_avg_kw, 'power', { cover: 'cd.stat.avg_power', how: c.power_max_kw != null ? `peak ${format(c.power_max_kw, 'power')}` : 'into the pack', why }),
      stat('Outside', c.outside_avg_c, 'temperature', { cover: 'cd.stat.avg_temp', how: c.battery_avg_c != null ? `battery ${format(c.battery_avg_c, 'temperature')}` : 'average', why }),
      stat('Range per hour', c.range_per_hour_km, 'range_rate', { how: 'range added per hour charging', why }),
      stat('Odometer', c.odometer_km, 'odometer', { how: c.fast_charger ? [c.fast_charger.brand, c.fast_charger.type === 'supercharger' ? 'Supercharger' : c.fast_charger.type, c.fast_charger.class].filter(Boolean).join(' ') : imported ? 'imported' : apiOnly ? 'from the Tesla API' : 'from SMO' }),
    ], { cls: 'cg-rack' }),
    c.flags?.gap_ms ? h('p', { class: 'note note--warn' }, icon('warn', 'i--sm'), h('span', null, `${formatDuration(c.flags.gap_ms)} without data inside this charge.`)) : null,
    note(`Battery % as the car's screen shows it; range is ${getUnits().range === 'ideal' ? 'rated range (charges keep rated range only)' : 'rated range'}.`));

  /* ---------- where */
  const mapEl = h('div', { class: 'map cg-map', role: 'img', 'aria-label': `Map of where the car charged: ${placeLabel(c)}` });
  const whereHud = hud({ title: 'Where', icon: 'pin', cover: ['cd.map.location', 'web.charge-cost.map'] },
    h('div', { class: 'cg-where', cover: ['cd.stat.location', 'web.charge-cost.location'] },
      h('span', { class: 'stat-label' }, h('span', null, c.place?.kind ? c.place.kind : c.place ? 'Saved place' : 'Not a saved place')),
      h('b', null, placeLink({ ...c, lat: c.position?.lat, lon: c.position?.lon, address: c.address?.short }, { text: placeLabel({ ...c, address: c.address?.short }) })),
      c.place && c.address?.short ? h('span', { class: 'faint' }, c.address.short) : null,
      h('span', { class: 'faint' }, c.position?.accuracy_m != null ? `GPS within ${format(c.position.accuracy_m, 'length')}` : c.position?.lat != null ? '' : 'No GPS fix')),
    mapEl);

  /* ---------- traces */
  const t = curve?.t_ms || [];
  const col = (f) => curve?.values?.[f] || [];
  const known = (f) => col(f).some((v) => v != null);
  const selBox = h('div', { class: 'cg-sel', cover: 'cd.filter.time_range' });
  const timeEl = h('span', { class: 'scope-read' });
  const resetBtn = btn('Whole session', () => zoomTo(xMin, xMax), { sm: true, kind: 'ghost', icon: 'arrow-l' });
  resetBtn.hidden = true;
  const chargeHost = h('div', { class: 'cg-traces' });
  const supplyHost = h('div', { class: 'cg-traces' });
  const curveHost = h('div', { class: 'stack' });
  const csvCurve = csvBtn({ name: [car.name, 'charge', day], rows: () => t.map((ms, i) => Object.fromEntries([['t', ms], ...FIELDS.map((f) => [f, col(f)[i] ?? null])])),
    columns: [timeColumn('Time', 't', tz), column('Battery', 'soc_pct', 'percent'), column('Usable', 'usable_soc_pct', 'percent'), column('Power into the pack', 'power_kw', 'power'), column('AC line power', 'ac_line_kw', 'power'),
      column('Rated range', 'range_km', 'range'), column('Battery temperature', 'battery_c', 'temperature'), column('Outside', 'outside_c', 'temperature'), column('Pack heating', 'heater_kw', 'power'),
      column('AC line voltage', 'ac_volts_v', 'voltage'), column('AC line current', 'ac_amps_a', 'current'), column('Pilot current', 'pilot_a', 'current'), column('Phases', 'phases'), column('Current set by', 'limit_source'),
      column('Charger voltage', 'dc_volts_v', 'voltage'), column('Charger current', 'dc_amps_a', 'current')] });
  const limits = [...new Set(col('limit_source').filter((v) => v != null))];
  const tracesHud = hud({ title: 'Through the charge', icon: 'pulse', cover: 'cd.chart.timeline', side: [curve ? srcChip(curve) : null, timeEl, resetBtn, csvCurve] },
    h('p', { class: 'note cg-hint' }, icon('info', 'i--sm'), h('span', null, 'Drag across a trace to zoom; double-click for the whole session.')),
    selBox,
    (c.gaps || []).length ? h('p', { class: 'note note--warn' }, icon('warn', 'i--sm'), h('span', null, `${c.gaps.length === 1 ? 'One stretch' : `${c.gaps.length} stretches`} without data: nothing is drawn across ${c.gaps.length === 1 ? 'it' : 'them'}.`)) : null,
    chargeHost,
    note(curve?.step_ms > 1000 ? `One point every ${formatDuration(curve.step_ms)}${imported ? ' (imported history)' : apiOnly ? ' (Tesla API readings)' : ' (a long charge)'}.` : 'One point per second.'));
  const supplyHud = hud({ title: dc ? 'The charger' : 'The supply', icon: 'plug' },
    note(ac ? `What the car took from the AC supply.${limits.length ? ` The current was set by ${limits.map((k) => LIMIT[k] || k).join(', then ')}.` : ' The car did not say what set the current.'}`
      : dc ? 'A fast charger turns AC into DC itself, so the car sees the charger\'s DC volts and amps, not the grid.' : 'This charge does not say whether it was AC or DC.'),
    supplyHost);
  const curveRead = h('span', { class: 'scope-read' });
  const curveHud = hud({ title: 'Power against battery %', icon: 'bolt', tone: 'orange', cover: ['cd.chart.curve_points', 'cd.chart.curve_avg'], side: curveRead }, curveHost);

  /* ---------- cost */
  const cost = c.cost;
  const bd = c.cost_breakdown || { parts: [] };
  const manual = c.manual_cost || (cost?.source === 'manual' ? { mode: bd.mode, amount: cost.amount, currency: cost.currency } : null);
  const price = place?.price || null;
  const cur = cost?.currency || price?.currency || 'USD';
  const gridKwh = c.energy_grid_kwh;
  const minutes = (c.charging_ms || 0) / 60000;
  const parts = (bd.parts || []).map((p) => ({ ...p }));
  if (cost?.source === 'manual' && manual && parts.length === 1) {
    const p = parts[0];
    if (manual.mode === 'per_minute') Object.assign(p, { per_minute: manual.amount, minutes: p.minutes ?? minutes, per_kwh: null, rate: 'Typed per minute' });
    else if (manual.mode === 'per_kwh') Object.assign(p, { per_kwh: manual.amount, per_minute: null, minutes: null, rate: 'Typed per kWh' });
    else Object.assign(p, { per_kwh: null, per_minute: null, minutes: null, rate: 'Typed as one amount' });
  }
  if (bd.session_fee) parts.push({ rate: 'Session fee', per_kwh: null, per_minute: null, kwh: null, minutes: null, cost: bd.session_fee });
  const breakdown = tracks(ctx, {
    title: 'How the cost was made', icon: 'stats', rows: parts, csvName: `charge-${day}-cost`, dense: true,
    note: cost?.source === 'tariff' ? `Each rate of the place price for the time it applied, in the car's time zone (${zn})${bd.gap_ms ? `; ${formatDuration(bd.gap_ms)} without data is priced at the rate around it` : ''}.` : null,
    columns: [
      { key: 'rate', label: 'Rate', lead: true, render: (p) => p.rate },
      { key: 'per_kwh', label: 'Per kWh', num: true, render: (p) => (p.per_kwh != null ? rateText(p.per_kwh, 'price_energy', cur) : '—'), csv: (p) => p.per_kwh },
      { key: 'per_minute', label: 'Per minute', num: true, render: (p) => (p.per_minute != null ? rateText(p.per_minute, 'price_time', cur) : '—'), csv: (p) => p.per_minute },
      { key: 'kwh', label: 'Energy', num: true, render: (p) => format(p.kwh, 'energy'), csv: (p) => p.kwh },
      { key: 'minutes', label: 'Time', num: true, render: (p) => (p.minutes != null ? formatDuration(p.minutes * 60000) : '—'), csv: (p) => p.minutes },
      { key: 'cost', label: `Cost (${cur})`, num: true, render: (p) => formatMoney(p.cost, cur), csv: (p) => p.cost },
    ],
    empty: () => emptyState({ icon: 'stats', title: 'No parts', text: cost?.source === 'free_supercharging' ? 'Free Supercharging: nothing to pay.' : cost ? 'This cost is one amount with no rates.' : 'There is no cost to break down yet.' }),
  });
  const meter = h('div', { class: 'cg-meter', cover: ['cd.stat.cost', 'web.charge-cost.current-cost'] },
    h('span', { class: 'cg-meter-cap' }, 'Cost'),
    h('span', { class: ['cg-meter-v', !cost && 'is-none'] }, cost ? formatMoney(cost.amount, cost.currency) : 'NO COST YET'),
    h('span', { class: 'cg-meter-sub' }, cost ? [COST_SOURCE[cost.source] || cost.source, c.price_per_kwh?.value != null ? ` · ${format(c.price_per_kwh.value, 'price_energy', { currency: cost.currency })} of ${c.price_per_kwh.basis === 'grid' ? 'energy drawn' : 'energy added'}` : '']
      : price ? 'The place has a price, but it gave no figure for this charge.' : c.place ? `${c.place.name} has no price yet: add one in Places, or type the cost here.` : 'Not at a saved place with a price: type the cost here.'));
  const facts = kv([
    ['When', h('span', { cover: 'web.charge-cost.dates' }, `${formatDate(t0, tz, { weekday: false })} ${formatTime(t0, tz)} to ${live ? 'now' : formatTime(t1, tz)} ${zn}`)],
    ['Charging time', h('span', { cover: 'web.charge-cost.duration' }, `${formatDuration(c.charging_ms)} (pauses under 3 min count as one charge)`)],
    ['Energy', h('span', { cover: 'web.charge-cost.energy' }, `${format(c.energy_added_kwh, 'energy')} into the pack${ac && gridKwh != null ? `, ${format(gridKwh, 'energy')} drawn` : dc ? `, ${format(c.dc_inlet_kwh, 'energy')} delivered` : ''}`)],
    ['Place price', price ? [price.per_kwh != null ? format(price.per_kwh, 'price_energy', { currency: price.currency }) : null, price.per_minute != null ? format(price.per_minute, 'price_time', { currency: price.currency }) : null, (price.rates || []).length ? `${price.rates.length} time rates` : null, price.session_fee ? `${formatMoney(price.session_fee, price.currency)} per session` : null].filter(Boolean).join(', ') || 'priced' : 'none'],
  ]);

  // the editor: a total, a price per kWh or per minute, the currency and the energy basis
  const can = !!ctx.admin;
  const startMode = manual?.mode && manual.mode !== 'tariff' ? manual.mode : 'total';
  const fMode = field({ label: 'The amount is', name: 'mode', type: 'select', value: startMode, path: 'mode', options: [['total', 'The total for this charge'], ['per_kwh', 'A price per kWh'], ['per_minute', 'A price per minute of charging']] });
  const fAmount = field({ label: 'Amount', name: 'amount', type: 'number', value: manual ? manual.amount : cost ? cost.amount : '', required: true, path: 'amount',
    hint: 'Rates may have up to 5 decimals (0.34500); only the total is rounded. Below zero is refused: type a credit as 0.', attrs: { min: '0', max: '100000', step: startMode === 'total' ? '0.01' : '0.00001', inputmode: 'decimal' } });
  const fCur = field({ label: 'Currency', name: 'currency', type: 'select', value: manual?.currency || cur, path: 'currency', options: [...new Set([manual?.currency || cur, ...CURRENCIES])].map((k) => [k, k]) });
  const fBasis = field({ label: 'Per kWh of', name: 'energy_basis', type: 'select', path: 'energy_basis', value: manual?.energy_basis || (gridKwh != null ? 'grid' : 'pack'),
    options: [...(gridKwh != null ? [['grid', `Energy drawn (${format(gridKwh, 'energy')})`]] : []), ['pack', `Energy added to the pack (${format(c.energy_added_kwh, 'energy')})`]], hint: dc ? 'On a fast charge the grid energy is unknown, so a price per kWh is over the energy added.' : null });
  const preview = h('p', { class: 'cg-preview', 'aria-live': 'polite' });
  const amountOf = () => { const v = fAmount.read(); return v === '' ? null : Number(v); };
  const sync = () => {
    const m = fMode.read();
    fAmount.input.step = m === 'total' ? '0.01' : '0.00001';
    fBasis.el.hidden = m !== 'per_kwh';
    const a = amountOf();
    const kwh = fBasis.read() === 'grid' && gridKwh != null ? gridKwh : c.energy_added_kwh;
    const total = a == null || !Number.isFinite(a) ? null : m === 'total' ? a : m === 'per_kwh' ? a * (kwh || 0) : a * minutes;
    preview.textContent = total == null ? 'Type an amount to see the total.' : m === 'total' ? `${formatMoney(total, fCur.read())} for the whole charge.`
      : m === 'per_kwh' ? `${format(kwh, 'energy')} × ${format(a, 'price_energy', { currency: fCur.read(), digits: 5 })} = ${formatMoney(total, fCur.read())}.`
        : `${formatNumber(minutes, { maximumFractionDigits: 1 })} min × ${format(a, 'price_time', { currency: fCur.read(), digits: 5 })} = ${formatMoney(total, fCur.read())}.`;
  };
  for (const f of [fMode, fAmount, fCur, fBasis]) { f.input.addEventListener('input', sync); f.input.addEventListener('change', sync); }
  const removeBtn = btn('Remove the typed cost', async () => {
    removeBtn.disabled = true;
    try { await ctx.api.del(`${base}/cost`); toast('Typed cost removed: the place price applies again.', { next: true }); ctx.reload(); } catch (err) { costForm.setMessage(err?.message || 'The cost could not be removed. Try again.'); removeBtn.disabled = false; }
  }, { kind: 'danger', sm: true, icon: 'x' });
  removeBtn.hidden = !manual;
  const costForm = form({ fields: [fMode, fAmount, fCur, fBasis], submit: manual ? 'Save the new cost' : 'Save this cost', extra: preview, secondary: removeBtn,
    async onSubmit(v) {
      const body = { mode: v.mode, amount: v.amount === '' ? null : Number(v.amount), currency: v.currency };
      if (v.mode === 'per_kwh') body.energy_basis = v.energy_basis;
      await ctx.api.put(`${base}/cost`, body);
      toast('Cost saved. It stays with this charge when charges are rebuilt.', { next: true });
      ctx.reload();
    } });
  cover(costForm.el, 'cd.link.set_cost', 'web.charge-cost.entry');
  cover(fMode.el, 'web.charge-cost.mode');
  cover(fAmount.el, 'web.charge-cost.amount', 'web.charge-cost.rate-precision');
  cover(costForm.el.querySelector('.form-acts'), 'web.charge-cost.save', 'web.charge-cost.clear-and-negative');
  cover(costForm.el.querySelector('.form-msg'), 'web.charge-cost.error');
  sync();
  const editor = h('div', { class: 'cg-edit' },
    h('h3', { class: 'cg-edit-h' }, manual ? 'Change the typed cost' : 'Type the cost'),
    note(can ? 'For a receipt that differs from the place price. A cost typed here stays when charges are rebuilt and is never repriced; remove it to go back to the place price.' : 'Only an admin can type or change a cost.'),
    h('fieldset', { class: 'cg-fs', disabled: !can }, costForm.el));
  const costHud = hud({ title: 'Cost', icon: 'book', tone: 'orange', id: 'cost' },
    h('div', { class: 'cols cg-cost' }, h('div', { class: 'stack' }, meter, facts), editor));

  replace(ctx.view, h('div', { class: 'page cg', cover: ['shots.charge-details', 'cd.context.session'] },
    head,
    h('div', { class: 'cols cols--wide cg-top' }, computer, whereHud),
    tracesHud,
    h('div', { class: 'cols cg-low' }, supplyHud, curveHud),
    costHud, breakdown.el,
    h('div', { hidden: true, cover: 'web.charge-cost.not-found' }, emptyState({ title: 'No such charge', text: 'A charge number that does not exist, or one of a car your account may not see, gives this same answer.' }))));

  if (ctx.query?.edit === 'cost') requestAnimationFrame(() => { if (!ctx.alive()) return; const sec = ctx.view.querySelector('#cost'); sec?.scrollIntoView({ block: 'start' }); sec?.querySelector('select, input')?.focus({ preventScroll: true }); });

  /* ---------- the map */
  const mapJob = (async () => {
    const pos = c.position;
    if (pos?.lat == null) { mapEl.append(mapFallback('No position was recorded for this charge.')); return; }
    try {
      const m = await createMap(mapEl, ctx, { center: [pos.lon, pos.lat], zoom: 15 });
      if (!m) return;
      await m.ready;
      if (!ctx.alive()) return;
      pin(m, [pos.lon, pos.lat], { tone: 'grn', title: placeLabel(c) });
      m.frame(() => m.map.jumpTo({ center: [pos.lon, pos.lat], zoom: 15 }));
    } catch { replace(mapEl, mapFallback()); }
  })();

  /* ---------- the traces */
  const xMin = t.length ? t[0] / 1000 : 0, xMax = t.length ? t[t.length - 1] / 1000 : 0;
  let zoom = [xMin, xMax];
  const plots = [];
  function paintSel() {
    const [a, b] = zoom;
    const idx = t.map((ms, i) => (ms / 1000 >= a && ms / 1000 <= b ? i : -1)).filter((i) => i >= 0);
    const pw = idx.map((i) => col('power_kw')[i]).filter((v) => v != null);
    const soc = idx.map((i) => col('soc_pct')[i]).filter((v) => v != null);
    let kwh = 0;
    for (let k = 1; k < idx.length; k++) { const v = col('power_kw')[idx[k]]; const dt = (t[idx[k]] - t[idx[k - 1]]) / 3.6e6; if (v != null && dt < 0.05) kwh += v * dt; }
    const z = t.length && (a > xMin + 1 || b < xMax - 1);
    resetBtn.hidden = !z;
    replace(selBox, h('span', { class: 'cg-sel-t' }, z ? `Selection ${formatTime(a * 1000, tz)} to ${formatTime(b * 1000, tz)}` : 'The whole session'),
      counters([
        { label: 'Duration', text: formatDuration(z ? (b - a) * 1000 : c.charging_ms) },
        { label: 'Energy added', value: z ? (pw.length ? kwh : null) : c.energy_added_kwh, kind: 'energy', how: z ? 'power × time from the curve' : 'the figure for this charge' },
        { label: 'Average power', value: z ? (pw.length ? pw.reduce((s, v) => s + v, 0) / pw.length : null) : c.power_avg_kw, kind: 'power', how: z ? 'from the curve' : c.power_max_kw != null ? `peak ${format(c.power_max_kw, 'power')}` : null },
        { label: 'Battery', text: z ? (soc.length > 1 ? format(soc[soc.length - 1] - soc[0], 'percent', { sign: true }) : '—') : format(socAdded, 'percent', { sign: true }) },
      ]));
  }
  const zoomTo = (a, b) => { zoom = [a, b]; for (const p of plots) p?.u?.setScale('x', { min: a, max: b }); paintSel(); };
  paintSel();
  const V = (f) => col(f).map((v) => (typeof v === 'number' ? v : null));
  const heat = col('heater_kw').filter((v) => v != null);
  const heatKwh = heat.length ? t.reduce((s, ms, i) => { const v = col('heater_kw')[i]; const dt = i ? (ms - t[i - 1]) / 3.6e6 : 0; return v == null || dt > 0.05 ? s : s + v * dt; }, 0) : null;
  const chargeDefs = [
    { name: 'Battery · %', cover: 'cd.chart.soc', ranges: { y: [0, 100] }, series: [{ label: 'Battery', values: V('soc_pct'), color: '--mag', fill: true }, { label: 'Usable', values: V('usable_soc_pct'), color: '--cyan', dash: [4, 3], width: 1.5 }],
      note: known('usable_soc_pct') ? 'Usable is the part you can drive on; a cold battery locks some of it.' : null },
    { name: 'Charging power · kW', cover: 'cd.chart.power', series: [{ label: 'Into the pack', values: V('power_kw'), color: '--grn', fill: true }, ...(known('ac_line_kw') ? [{ label: 'From the line', values: V('ac_line_kw'), color: '--org', dash: [4, 3], width: 1.5 }] : [])],
      note: known('ac_line_kw') ? 'The gap between the two lines is what the on-board charger and the cables lose.' : dc ? 'Measured at the pack; the charger does not send its own power.' : null },
    { name: 'Rated range', cover: 'cd.chart.range', series: [{ label: 'Range', values: V('range_km'), kind: 'range', color: '--cyan' }] },
    { name: 'Temperatures', cover: 'cd.chart.outdoor_temp', series: [{ label: 'Battery', values: V('battery_c'), kind: 'temperature', color: '--org' }, { label: 'Outside', values: V('outside_c'), kind: 'temperature', color: '--violet' }], digits: 1,
      note: known('battery_c') ? null : 'Battery temperature is not in imported history.' },
    { name: 'Pack heating · kW', cover: 'cd.chart.battery_heater', ranges: { y: (u, a, b) => [0, Math.max(1, (b || 0) * 1.15)] }, series: [{ label: 'Heating', values: V('heater_kw'), color: '--red', fill: true }], digits: 1,
      note: !heat.length ? 'This car does not send pack heating power (or it is imported history).' : heat.some((v) => v > 0) ? `The pack warmed itself to charge faster: about ${format(heatKwh, 'energy')}, taken from the supply.` : 'The pack did not heat itself during this charge.' },
  ];
  const supplyDefs = ac ? [
    { name: 'Line voltage · V', cover: 'cd.chart.ac_voltage', series: [{ label: 'Volts', values: V('ac_volts_v'), color: '--cyan' }], note: known('ac_volts_v') ? 'Per phase, at the car\'s inlet. A dip under load means a long or thin cable.' : 'Line voltage was not recorded for this charge.' },
    { name: 'Current · A', cover: ['cd.chart.ac_current', 'cd.chart.pilot_current'], ranges: { y: (u, a, b) => [0, Math.max(1, (b || 0) * 1.15)] }, series: [{ label: 'Drawn', values: V('ac_amps_a'), color: '--grn', fill: true }, { label: 'Pilot (allowed)', values: V('pilot_a'), color: '--org', dash: [4, 3], width: 1.5 }],
      note: known('pilot_a') ? 'The pilot is the most the station lets the car draw.' : 'The station\'s pilot current is not in this record.' },
    { name: 'Phases in use', cover: 'cd.chart.phases', ranges: { y: [0, 3.5] }, series: [{ label: 'Phases', values: V('phases'), color: '--violet' }], note: known('phases') ? '1 for a home socket, 3 for most European wall boxes.' : 'Phases were not recorded for this charge.' },
  ] : [
    { name: dc ? 'Charger voltage · V' : 'Voltage · V', cover: 'cd.chart.ac_voltage', series: [{ label: dc ? 'Charger' : 'Pack', values: V(dc ? 'dc_volts_v' : 'volts_v'), color: '--cyan' }], note: dc ? 'The charger\'s output at the inlet; there is no AC line on a fast charge.' : null },
    { name: dc ? 'Charger current · A' : 'Current · A', cover: ['cd.chart.ac_current', 'cd.chart.pilot_current'], ranges: { y: (u, a, b) => [0, Math.max(1, (b || 0) * 1.15)] }, series: [{ label: dc ? 'Charger' : 'Pack', values: V(dc ? 'dc_amps_a' : 'amps_a'), color: '--grn', fill: true }], note: dc ? 'A fast charger has no pilot current: the car asks for the current it wants.' : null },
    { name: 'Phases in use', cover: 'cd.chart.phases', empty: true, series: [], note: dc ? 'None: a fast charge is DC, so there are no AC phases.' : 'Not known for this charge.' },
  ];
  const blocks = [...chargeDefs.map((df) => ({ df, host: chargeHost })), ...supplyDefs.map((df) => ({ df, host: supplyHost }))].map(({ df, host }) => {
    const read = h('span', { class: 'scope-read' });
    const box = well('', df.name);
    host.append(h('div', { class: 'cg-trace', cover: df.cover },
      h('div', { class: 'scope-head' }, h('span', { class: 'scope-title' }, df.name), h('span', { class: 'keys' }, df.series.map((s) => h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': `var(${s.color})` } }), s.label))), read),
      df.empty || !t.length ? null : box, df.note ? note(df.note) : null));
    return { df, box, read };
  });
  if (!t.length) {
    for (const host of [chargeHost, supplyHost, curveHost]) host.append(note('No curve was recorded for this charge.'));
    await Promise.all([breakdown.ready, mapJob]);
    if (ctx.alive()) ctx.ready();
    return;
  }
  const read = (i) => {
    timeEl.textContent = i == null ? '' : formatTime(t[i], tz, { seconds: true });
    for (const b of blocks) if (!b.df.empty) b.read.textContent = i == null ? '' : b.df.series.map((s) => (s.values[i] == null ? '—' : format(s.values[i], s.kind || 'number'))).join(' · ');
  };
  const gaps = (c.gaps || []).map((g) => [g.start_ms / 1000, (g.end_ms ?? t1) / 1000]);
  const drawGaps = (u) => {
    if (!gaps.length) return;
    const g = u.ctx, { left, top, width, height } = u.bbox;
    g.save(); g.beginPath(); g.rect(left, top, width, height); g.clip();
    g.fillStyle = alpha(cssVar('--st-unknown'), 0.3);
    for (const [a, b] of gaps) { const pa = u.valToPos(a, 'x', true), pb = u.valToPos(b, 'x', true); g.fillRect(Math.round(pa), top, Math.max(1, Math.round(pb - pa)), height); }
    g.restore();
  };
  let first = true;
  for (const b of blocks) {
    if (b.df.empty) continue;
    const p = await scope(b.box, ctx, { tz, t_ms: t, sync: `cg-${chid}`, series: b.df.series, ranges: b.df.ranges || {}, axes: { y: { digits: b.df.digits ?? 0 } },
      zoom: (a, z) => zoomTo(a / 1000, z / 1000), onCursor: first ? read : null, hooks: { drawClear: [drawGaps] },
      onBuild: (u) => { u.over.addEventListener('dblclick', () => zoomTo(xMin, xMax)); if (zoom[0] > xMin || zoom[1] < xMax) u.setScale('x', { min: zoom[0], max: zoom[1] }); } });
    first = false;
    if (!ctx.alive()) return;
    plots.push(p);
  }

  /* ---------- power against battery % */
  const pts = [];
  t.forEach((ms, i) => { const s = col('soc_pct')[i], p = col('power_kw')[i]; if (s != null && p != null && p > 0.05) pts.push([s, p]); });
  pts.sort((a, b) => a[0] - b[0]);
  const bySoc = new Map();
  for (const [s, p] of pts) { const k = Math.round(s); const e = bySoc.get(k) || [0, 0]; e[0] += p; e[1] += 1; bySoc.set(k, e); }
  const mean = (s) => { const e = bySoc.get(Math.round(s)); return e ? e[0] / e[1] : null; };
  const med = new Map((typical?.median || []).filter((m) => m.power_kw != null).map((m) => [m.soc_pct, m.power_kw]));
  const usual = (s) => (med.size ? med.get(Math.round(s)) ?? null : null);
  const box = well('well--lg', 'Power against battery %');
  replace(curveHost,
    h('div', { class: 'keys' }, h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': 'var(--grn)' } }), 'each second'), h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': 'var(--mag)' } }), 'this session, mean per %'),
      dc ? h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': 'var(--ink-2)' } }), `this car's usual fast charge${typical?.sessions?.length ? ` (median of ${typical.sessions.length})` : ''}`) : null,
      csvBtn({ name: [car.name, 'charge', day, 'power-curve'], rows: () => pts.map(([s, p]) => ({ soc: s, power: p, mean: mean(s), usual: dc ? usual(s) : null })), columns: [column('Battery', 'soc', 'percent'), column('Power', 'power', 'power'), column('Mean at this %', 'mean', 'power'), column('Usual fast charge', 'usual', 'power')] })),
    pts.length >= 2 ? box : note('Too few points to draw.'),
    note(dc ? 'Power into the pack. A fast charge slows as the battery fills; a cold pack lowers the whole curve.' : 'Power into the pack. AC charging is limited by the supply, so the line stays flat until the end.'));
  if (pts.length >= 2) {
    await loadUPlot();
    if (!ctx.alive()) return;
    const cx = pts.map((p) => p[0]);
    const data = [cx, pts.map((p) => p[1]), cx.map(mean), cx.map((s) => (dc ? usual(s) : null))];
    mountPlot(box, (width, height) => {
      const c2 = tone(), dot = cssVar('--grn');
      return {
        width, height, padding: [rpx(10), rpx(16), 0, 0], legend: { show: false },
        cursor: { drag: { x: false, y: false }, points: { size: rpx(8), width: rpx(2), stroke: c2.hub } },
        scales: { x: { time: false, auto: true }, y: { range: (u, a, b) => [0, Math.max(1, (b || 0) * 1.12)] } },
        axes: [{ stroke: c2.axis, font: chartFont(), grid: { stroke: c2.grid, width: 1 }, ticks: { show: false }, gap: rpx(6), size: rpx(26), space: rpx(60), incrs: [1, 2, 5, 10, 20, 25, 50], values: (u, v) => v.map((n) => format(n, 'percent')) }, valueAxis({ size: 48 })],
        series: [{},
          { label: 'Sample', stroke: dot, width: 0, paths: () => null, points: { show: true, size: rpx(3.5), width: 0, fill: alpha(dot, 0.55), stroke: alpha(dot, 0.55) } },
          { label: 'Mean', stroke: cssVar('--mag'), width: rpx(2.25), points: { show: false }, spanGaps: true },
          { label: 'Usual', stroke: cssVar('--ink-2'), width: rpx(1.5), dash: [rpx(5), rpx(4)], points: { show: false }, spanGaps: true }],
        hooks: { setCursor: [(u) => {
          const i = u.cursor.idx;
          if (i == null || u.cursor.left < 0) { curveRead.textContent = ''; return; }
          const m = mean(cx[i]), us = dc ? usual(cx[i]) : null;
          curveRead.textContent = `${format(cx[i], 'percent')}: ${format(data[1][i], 'power')}${m != null ? ` · mean ${format(m, 'power')}` : ''}${us != null ? ` · usual ${format(us, 'power')}` : ''}`;
        }] },
      };
    }, data, ctx);
  }
  await Promise.all([breakdown.ready, mapJob]);
  if (ctx.alive()) ctx.ready();
}
