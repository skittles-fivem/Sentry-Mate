// Charge detail (/cars/{car}/charges/{charge}): one charging session in full. The figures (time, energy
// into the pack and from the supply, efficiency, battery % and range), where it was (a small map), the
// session at 1 Hz on one time axis (GET .../curve: battery, power, range, temperatures, battery heating,
// and the supply: line or charger volts and amps, the station's pilot current, phases), power against
// battery % over this car's usual fast-charging curve, and the cost with its parts and an editor
// (PUT/DELETE .../cost, admin) that survives rebuilds.

import * as state from '../shared/state.js';
import { h, replace, cssVar, cover } from '../shared/dom.js';
import { format, convert, unitLabel, getUnits, formatMoney } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, zoneName, dayRange, dayKey, DAY } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { downloadCsv, csvFilename, column, timeColumn } from '../shared/csv.js';
import { icon } from '../core/icons.js';
import { spotText } from '../core/fmt.js';
import { pageHead, panel, metric, tag, csvButton, kv } from '../core/page.js';
import { dataTable } from '../core/table.js';
import { field, form } from '../core/form.js';
import { flash } from '../core/flash.js';
import { emptyState } from '../core/states.js';
import { createMap, carMarker, mapFallback } from '../core/map.js';
import { loadUPlot, mountPlot, timeAxis, valueAxis, colors, areaFill, hexA, chartFont } from '../core/chart.js';
import { rpx } from '../core/display.js';
import { srcMark, isImported, apiOnly, hasKind } from '../core/source.js';
import { costFromWords, feeLabel, invoiceSourceLabel, chargeInvoicePdfUrl } from '../shared/invoices.js';

export const id = 'charge';
export const css = true;
export const title = () => 'Charge';

// whether this tab started on a charge page: then "Back" has no page of ours to go back to
const landedHere = (() => { try { return performance.getEntriesByType('navigation')[0]?.name === location.href; } catch { return false; } })();
const landedUrl = location.href;

const CURRENCIES = ['USD', 'CAD', 'EUR', 'GBP', 'NOK', 'SEK', 'DKK', 'CHF', 'AUD', 'NZD', 'JPY', 'CNY'];
const COST_SOURCE = {
  tariff: 'from the place price', manual: 'entered by hand', free_supercharging: 'free Supercharging on this car',
  teslamate: 'imported from TeslaMate', tesla_invoice: costFromWords('tesla_invoice'), teslafi: costFromWords('teslafi'),
};
const LIMIT_WORDS = {
  station: "the charging station's pilot signal", cable: 'the cable', car: "the car's own maximum", setting: 'the current set on the screen',
};
const KIND_WORDS = { ac: 'AC charge', dc: 'Fast charge (DC)' };
const FIELDS = ['power_kw', 'ac_line_kw', 'soc_pct', 'usable_soc_pct', 'range_km', 'battery_c', 'outside_c', 'heater_kw',
  'ac_volts_v', 'ac_amps_a', 'pilot_a', 'phases', 'limit_source', 'dc_volts_v', 'dc_amps_a', 'volts_v', 'amps_a'];

const placeLabel = (c) => c.place?.name || c.address?.short || spotText(c) || 'Unnamed spot';
const placeHref = (c) => (c.place?.id ? state.buildUrl('places', { query: { place: c.place.id } })
  : c.position?.lat != null ? state.buildUrl('places', { query: { lat: c.position.lat.toFixed(5), lon: c.position.lon.toFixed(5), new: 1 } }) : null);
const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
const viewerZone = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone; } catch { return null; } })();

// a rate with the decimals it was entered with (2 to 5), so $0.25000/min is not rounded to $0.25 or $0.250
function rateText(v, kind, currency) {
  const dec = (String(+Number(v).toFixed(5)).split('.')[1] || '').length;
  return format(v, kind, { currency, digits: clamp(dec, 2, 5) });
}

// the first drive after t. The timeline is newest first, so within a window the last row of the last page
// is the oldest; the window widens (3 days, a month, a year, then up to now) until a drive turns up.
async function driveAfterOf(ctx, cid, t) {
  const now = ctx.api.now();
  try {
    for (const span of [3 * DAY, 30 * DAY, 365 * DAY, Infinity]) {
      const to = Math.min(t + span, now + 1);
      if (to <= t) return null;
      let after = null, last = null;
      for (let i = 0; i < 50; i++) {
        const res = await ctx.api.get(`cars/${encodeURIComponent(cid)}/timeline`, { from_ms: t, to_ms: to, kinds: 'drive', limit: 500, ...(after ? { after } : {}) });
        const items = res?.items || [];
        if (items.length) last = items[items.length - 1];
        after = res?.next || null;
        if (!after || !items.length) break;
      }
      if (last) return last.ref?.drive || null;
      if (to > now) return null;
    }
  } catch { /* no button */ }
  return null;
}

export async function mount(ctx) {
  const { car, tz } = ctx;
  const cid = car.id, chid = ctx.state.charge;
  const base = `cars/${encodeURIComponent(cid)}/charges/${encodeURIComponent(chid)}`;
  let c;
  try { c = await ctx.api.get(base); } catch (e) {
    if (e.status === 404) {
      e.message = `There is no charge with this number for ${car.name}. It may have been rebuilt under a new number, or it belongs to a car your account may not see; find it on Charges.`;
    }
    ctx.fail(e, e.status === 404 ? { retry: null } : {});
    return;
  }
  if (!ctx.alive()) return;
  const live = !!c.flags?.in_progress;
  const t0 = c.start_ms, t1 = c.end_ms ?? ctx.api.now();
  const dc = c.kind === 'dc', ac = c.kind === 'ac';
  // where the data came from: SMO measures; the Tesla API alone and an import only estimate the energy
  const apiAlone = apiOnly(c);
  const imported = isImported(c) && !hasKind(c, 'smo');
  const est = c.energy_basis === 'estimated' || imported || apiAlone;
  const estWord = apiAlone ? 'estimated from the Tesla API\'s readings' : 'estimated (imported)';
  const notIn = apiAlone ? 'Not from the Tesla API' : 'Not in imported history';

  const [curve, place, around, typical] = await Promise.all([
    ctx.api.get(`${base}/curve`, { fields: FIELDS.join(','), max_points: 4000 }).catch(() => null),
    c.place?.id ? ctx.api.get(`places/${encodeURIComponent(c.place.id)}`).catch(() => null) : null,
    Promise.all([
      // the timeline is newest first: the first row before the charge is the drive before, however long the car stood
      ctx.api.get(`cars/${encodeURIComponent(cid)}/timeline`, { to_ms: t0, kinds: 'drive', limit: 1 }).catch(() => null),
      live ? null : driveAfterOf(ctx, cid, t1),
    ]),
    dc ? ctx.api.get(`cars/${encodeURIComponent(cid)}/charging-stats/dc-curve`, { from_ms: t0 - 365 * DAY, to_ms: t1 + 1 }).catch(() => null) : null,
    // uPlot loads with the data, so the charts are drawn with the page (their space never opens up later)
    loadUPlot(),
  ]);
  if (!ctx.alive()) return;

  const day = dayKey(t0, tz);
  const when = `${formatDate(t0, tz, { year: false })}, ${formatTime(t0, tz)}`;
  ctx.setTitle(`Charge ${when}`);
  ctx.setCrumbs([{ label: 'All cars', href: '/' }, { label: car.name, href: ctx.link('car') }, { label: 'Charges', href: ctx.link('charges') }, { label: when }]);
  if (live) ctx.events('rows', (ev) => { if (ev.data?.car === cid && (ev.data.kinds || []).includes('charges')) ctx.refresh(); });

  const driveBefore = around[0]?.items?.[0]?.ref?.drive || null;
  const driveAfter = around[1] || null;
  const backHref = ctx.link('charges');
  const back = h('a', {
    class: 'back', href: backHref, cover: 'web.charge-cost.back',
    onclick: (e) => { if (!(landedHere && location.href === landedUrl) && history.length > 1) { e.preventDefault(); history.back(); } },
  }, icon('arrow-l', 'ico--sm'), 'Back');
  const trail = h('nav', { class: 'charge-trail', 'aria-label': 'This charge', cover: ['web.charge-cost.breadcrumb', 'cd.filter.car'] },
    h('a', { href: '/' }, 'All cars'), h('span', { 'aria-hidden': 'true' }, '›'),
    h('a', { href: ctx.link('car'), cover: 'web.charge-cost.car' }, car.name), h('span', { 'aria-hidden': 'true' }, '›'),
    h('a', { href: backHref }, 'Charges'));
  const zone = zoneName(t0, tz);
  const otherZone = viewerZone && viewerZone !== tz
    ? `${formatTime(t0, viewerZone)} – ${live ? 'now' : formatTime(t1, viewerZone)} ${zoneName(t0, viewerZone)} where you are` : null;

  /* ---------- the figures */
  const why = live ? 'Still charging' : imported || apiAlone ? notIn : 'Not recorded';
  const gridKwh = c.energy_grid_kwh;
  const drawn = ac ? metric('Energy drawn', gridKwh, 'energy', { cover: 'cd.stat.energy_used', explain: est ? estWord : 'from the supply, AC line volts × amps', why: imported || apiAlone ? notIn : why })
    : dc ? metric('Delivered by the charger', c.dc_inlet_kwh, 'energy', { cover: 'cd.stat.energy_used', explain: c.fast_charger?.billed_kwh != null ? `at the inlet; the charger billed ${format(c.fast_charger.billed_kwh, 'energy')}` : "at the inlet (the charger's volts × amps); the grid side is not sent", why })
      : metric('Energy drawn', null, 'energy', { cover: 'cd.stat.energy_used', why: 'Unknown whether AC or DC' });
  const eff = metric('Efficiency', c.efficiency_pct, 'percent', {
    cover: 'cd.stat.efficiency', digits: 1,
    explain: c.efficiency_pct != null ? `${format(c.losses_kwh, 'energy')} lost on the way${est ? ' (estimated)' : ''}${c.efficiency_pct > 100 ? '; above 100 % means a bad reading' : ''}` : null,
    why: dc ? "DC: the charger's own losses are not on the CAN bus" : why,
  });
  const avgTemp = metric('Outside', c.outside_avg_c, 'temperature', { cover: 'cd.stat.avg_temp', explain: c.battery_avg_c != null ? `average; battery ${format(c.battery_avg_c, 'temperature')}` : 'average of the session', why });
  const socFrom = clamp(c.start_soc_pct ?? 0, 0, 100), socTo = clamp(c.end_soc_pct ?? 0, 0, 100);
  const socAdded = c.end_soc_pct != null && c.start_soc_pct != null ? c.end_soc_pct - c.start_soc_pct : null;
  const rangeName = getUnits().range === 'ideal' ? 'Rated range (charges keep rated range only)' : 'Rated range';
  const stats = panel({ title: 'Figures', icon: 'stats', cls: 'charge-figures-panel', side: live ? h('span', { class: 'pill pill--warm' }, icon('bolt'), 'so far') : null },
    h('div', { class: 'charge-figures' },
      metric('Charging time', c.charging_ms, 'duration', { cover: 'cd.stat.duration', explain: c.plugged_ms != null ? `plugged in for ${formatDuration(c.plugged_ms)}` : 'first to last charging second' }),
      metric('Energy added', c.energy_added_kwh, 'energy', { cover: 'cd.stat.energy_added', explain: est ? estWord : "into the pack, the pack's volts × amps", why }),
      drawn, eff,
      metric('Average power', c.power_avg_kw, 'power', { cover: 'cd.stat.avg_power', explain: c.power_max_kw != null ? `peak ${format(c.power_max_kw, 'power')}, into the pack` : 'into the pack', why }),
      avgTemp,
      metric('Range per hour', c.range_per_hour_km, 'range_rate', { explain: 'range added per hour of charging', why })),
    h('div', { class: 'charge-soc' },
      h('div', { cover: ['cd.stat.soc_start', 'cd.stat.range_start'] }, h('span', { class: 'lbl' }, 'Start'), h('b', null, format(c.start_soc_pct, 'percent')), h('span', null, format(c.start_range_km, 'range'))),
      h('div', { class: 'charge-soc-bar', role: 'img', 'aria-label': `Battery from ${format(c.start_soc_pct, 'percent')} to ${format(c.end_soc_pct, 'percent')}` },
        h('span', { class: 'bar bar--charge', vars: { '--from': `${socFrom}%`, '--w': `${Math.max(0, socTo - socFrom)}%` } }, h('i')),
        h('span', { class: 'charge-soc-added', cover: ['cd.stat.soc_added', 'cd.stat.range_added'] },
          h('b', null, format(socAdded, 'percent', { sign: true })), ' · ', format(c.range_added_km, 'range', { sign: true }),
          c.energy_added_kwh && socAdded > 0 ? h('span', { class: 'faint' }, ` · ${format(c.energy_added_kwh / socAdded, 'energy')} per %`) : null)),
      h('div', { class: 'r', cover: ['cd.stat.soc_end', 'cd.stat.range_end'] }, h('span', { class: 'lbl' }, live ? 'Now' : 'End'), h('b', null, format(c.end_soc_pct, 'percent')), h('span', null, format(c.end_range_km, 'range')))),
    h('p', { class: 'note' }, `Battery % as the car's screen shows it; range is ${rangeName.toLowerCase()}.`),
    h('dl', { class: 'charge-facts' },
      h('dt', null, 'Odometer'), h('dd', null, format(c.odometer_km, 'odometer')),
      c.fast_charger ? [h('dt', null, 'Charger'), h('dd', null, [c.fast_charger.brand, c.fast_charger.type === 'supercharger' ? 'Supercharger' : c.fast_charger.type, c.fast_charger.class].filter(Boolean).join(' '))] : null,
      c.flags?.gap_ms ? [h('dt', null, 'Missing'), h('dd', { class: 'warn' }, `${formatDuration(c.flags.gap_ms)} without data`)] : null,
      [h('dt', null, 'Data from'), h('dd', null, srcMark(c, { estimated: true }))]));

  /* ---------- where */
  const mapEl = h('div', { class: 'map', role: 'img', 'aria-label': `Map of where the car charged: ${placeLabel(c)}` });
  const pHref = placeHref(c);
  const placeCard = h('div', { class: 'glass-card place-card charge-place', cover: ['cd.stat.location', 'web.charge-cost.location'] },
    h('span', { class: 'eyebrow' }, c.place?.kind ? `${c.place.kind[0].toUpperCase()}${c.place.kind.slice(1)}` : 'Not a saved place'),
    h('b', null, pHref ? h('a', { href: pHref }, placeLabel(c)) : placeLabel(c)),
    c.address?.short && c.place ? h('span', null, c.address.short) : null,
    h('span', { class: 'faint' }, c.position?.accuracy_m != null ? `GPS within ${format(c.position.accuracy_m, 'length')}` : c.position?.lat != null ? spotText(c) : 'No GPS fix'),
    pHref ? h('a', { class: 'panel-link', href: pHref }, c.place ? 'Edit this place' : 'Save as a place', icon('chev-r', 'ico--sm')) : null);
  const mapPanel = h('section', { class: 'panel map-panel charge-map', 'aria-label': 'Where it charged', cover: ['cd.map.location', 'web.charge-cost.map'] },
    mapEl, h('div', { class: 'map-overlay map-overlay--tl' }, placeCard));

  /* ---------- the charts */
  const selBox = h('div', { class: 'selection', cover: 'cd.filter.time_range' });
  const timeEl = h('span', { class: 'chart-time' });
  const resetBtn = h('button', { class: 'btn btn--sm', type: 'button', hidden: true }, icon('reset', 'ico--sm'), 'Whole session');
  const chargeHost = h('div', { class: 'charge-charts' });
  const supplyHost = h('div', { class: 'charge-charts' });
  const curveHost = h('div', { class: 'curve-chart' });
  const t = curve?.t_ms || [];
  const col = (f) => curve?.values?.[f] || [];
  const rowsCsv = () => t.map((ms, i) => Object.fromEntries([['t', ms], ...FIELDS.map((f) => [f, col(f)[i] ?? null])]));
  const curveCols = [timeColumn('Time', 't', tz), column('Battery', 'soc_pct', 'percent'), column('Usable', 'usable_soc_pct', 'percent'),
    column('Power into the pack', 'power_kw', 'power'), column('AC line power', 'ac_line_kw', 'power'), column('Rated range', 'range_km', 'range'),
    column('Battery temperature', 'battery_c', 'temperature'), column('Outside', 'outside_c', 'temperature'), column('Battery heating', 'heater_kw', 'power'),
    column('AC line voltage', 'ac_volts_v', 'voltage'), column('AC line current', 'ac_amps_a', 'current'), column('Pilot current', 'pilot_a', 'current'),
    column('Phases', 'phases'), column('Current set by', 'limit_source'), column('Charger voltage (DC)', 'dc_volts_v', 'voltage'),
    column('Charger current (DC)', 'dc_amps_a', 'current'), column('Pack voltage', 'volts_v', 'voltage'), column('Pack current', 'amps_a', 'current')];
  const csvCurve = (part) => csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'charge', day, part), rowsCsv(), curveCols) });
  const stepNote = curve?.step_ms > 1000 ? `One point every ${formatDuration(curve.step_ms)}${imported ? ' (imported history)' : apiAlone ? ' (Tesla API readings)' : ' (a long charge)'}.` : 'One point per second.';
  const gapNote = (c.gaps || []).length ? h('p', { class: 'note warn' }, icon('warn', 'ico--sm'), `${c.gaps.length === 1 ? 'One stretch' : `${c.gaps.length} stretches`} without data, shaded grey: nothing is drawn across ${c.gaps.length === 1 ? 'it' : 'them'}.`) : null;

  const chargePanel = h('section', { class: 'panel charts-panel', 'aria-label': 'Through the charge', cover: 'cd.chart.timeline' },
    h('div', { class: 'panel-head' }, h('h2', { class: 'panel-title' }, icon('signals'), 'Through the charge'),
      h('span', { class: 'row' }, timeEl, h('span', { class: 'faint zoom-hint' }, 'Drag across a chart to zoom; double-click for the whole session'), resetBtn, csvCurve('curve'))),
    selBox, gapNote, chargeHost, h('p', { class: 'note' }, stepNote));
  const limitSeen = [...new Set(col('limit_source').filter((v) => v != null))];
  const supplyPanel = h('section', { class: 'panel charts-panel', 'aria-label': 'The supply' },
    h('div', { class: 'panel-head' }, h('h2', { class: 'panel-title' }, icon('plug'), dc ? 'The charger' : 'The supply'), h('span', { class: 'row' }, csvCurve('supply'))),
    h('p', { class: 'note' }, ac ? `What the car took from the AC supply.${limitSeen.length ? ` The current was set by ${limitSeen.map((k) => LIMIT_WORDS[k] || k).join(', then ')}.` : ' The car did not say what set the current.'}`
      : dc ? "A fast charger turns AC into DC itself, so the car sees the charger's DC volts and amps, not the grid." : 'This charge does not say whether it was AC or DC.'),
    supplyHost);
  const curveVal = h('span', { class: 'val' });
  const curvePanel = h('section', { class: 'panel charts-panel', 'aria-label': 'Power against battery', cover: ['cd.chart.curve_points', 'cd.chart.curve_avg'] },
    h('div', { class: 'panel-head' }, h('h2', { class: 'panel-title' }, icon('bolt'), 'Power against battery %'), h('span', { class: 'row' }, curveVal, csvButton({ onClick: () => curveCsv() }))),
    h('div', { class: 'chart-label curve-keys' },
      h('span', { class: 'key key--dot' }, h('i', { vars: { '--c': 'var(--c-charge)' } }), 'each sample'),
      h('span', { class: 'key' }, h('i', { vars: { '--c': 'var(--warm)' } }), 'this session, mean per %'),
      dc ? h('span', { class: 'key key--dash' }, h('i', { vars: { '--c': 'var(--text-2)' } }), `this car's usual fast charge (median${typical?.sessions?.length ? ` of ${typical.sessions.length}` : ''})`) : null),
    curveHost,
    h('p', { class: 'note' }, dc ? 'Power into the pack. A fast charge slows down as the battery fills; a cold pack lowers the whole curve.'
      : "Power into the pack. AC charging is limited by the supply, so the line stays flat until the end; the usual-curve comparison is for fast charges only."));

  /* ---------- cost */
  const cost = c.cost;
  const bd = c.cost_breakdown || { parts: [] };
  const manual = c.manual_cost || (cost?.source === 'manual' ? { mode: bd.mode, amount: cost.amount, currency: cost.currency } : null);
  const price = place?.price || null;
  const isManual = cost?.source === 'manual' && manual;
  const manualMin = (bd.parts || [])[0]?.minutes ?? (t1 - t0) / 60000;
  const manualBasis = manual?.energy_basis || c.price_per_kwh?.basis || (gridKwh != null ? 'grid' : 'pack');
  const manualKwh = (bd.parts || [])[0]?.kwh ?? (manualBasis === 'grid' && gridKwh != null ? gridKwh : c.energy_added_kwh);
  const basisWords = (b) => (b === 'grid' ? 'energy drawn' : 'energy added to the pack');
  const manualWhy = !isManual ? null
    : manual.mode === 'per_minute' ? ` · ${rateText(manual.amount, 'price_time', cost.currency)} × ${formatNumber(manualMin, { maximumFractionDigits: 1 })} min`
      : manual.mode === 'per_kwh' ? ` · ${rateText(manual.amount, 'price_energy', cost.currency)} × ${format(manualKwh, 'energy')} (${basisWords(manualBasis)})`
        : ' · one amount, as entered';
  const costHead = h('div', { class: 'cost-now', cover: ['cd.stat.cost', 'web.charge-cost.current-cost'] },
    h('span', { class: 'metric-label' }, 'Cost'),
    h('span', { class: 'cost-amount' }, cost ? formatMoney(cost.amount, cost.currency) : 'No cost yet'),
    h('span', { class: 'explain' }, cost ? [COST_SOURCE[cost.source] || cost.source,
      isManual ? manualWhy : c.price_per_kwh?.value != null ? ` · ${format(c.price_per_kwh.value, 'price_energy', { currency: cost.currency })} over the ${c.price_per_kwh.basis === 'grid' ? 'energy drawn' : 'energy added to the pack'}` : '']
      : price ? 'The place has a price, but it gave no figure for this charge.' : c.place ? `${c.place.name} has no price yet: add one on Places, or enter the cost below.` : 'Not at a saved place with a price: enter the cost below, or save the spot as a place with a price.'),
    cost?.source === 'manual' ? tag('Entered by hand', 'warm') : null);
  const invoices = invoiceFacts(c);
  const parts = (bd.parts || []).map((p) => ({ ...p }));
  // an entered cost shows the rate as it was entered, with what it was multiplied by
  if (isManual && parts.length === 1) {
    const p = parts[0];
    if (manual.mode === 'per_minute') Object.assign(p, { per_minute: manual.amount, minutes: manualMin, per_kwh: null, rate: 'Entered per minute' });
    else if (manual.mode === 'per_kwh') Object.assign(p, { per_kwh: manual.amount, kwh: manualKwh, per_minute: null, minutes: null, rate: `Entered per kWh (${basisWords(manualBasis)})` });
    else Object.assign(p, { per_kwh: null, per_minute: null, minutes: null, rate: 'Entered as one amount' });
  }
  if (bd.session_fee) parts.push({ rate: 'Session fee', per_kwh: null, per_minute: null, kwh: null, minutes: null, cost: bd.session_fee });
  const cur = cost?.currency || price?.currency || 'USD';
  const breakdown = dataTable(ctx, {
    title: 'How the cost was made', icon: 'stats', rows: parts, cls: 'cost-table',
    columns: [
      { key: 'rate', label: 'Rate', render: (p) => p.rate, csv: (p) => p.rate },
      { key: 'per_kwh', label: 'Per kWh', num: true, render: (p) => (p.per_kwh != null ? rateText(p.per_kwh, 'price_energy', cur) : '—'), csv: (p) => p.per_kwh },
      { key: 'per_minute', label: 'Per minute', num: true, render: (p) => (p.per_minute != null ? rateText(p.per_minute, 'price_time', cur) : '—'), csv: (p) => p.per_minute },
      { key: 'kwh', label: 'Energy', num: true, render: (p) => format(p.kwh, 'energy'), csv: (p) => p.kwh },
      { key: 'minutes', label: 'Time', num: true, render: (p) => (p.minutes != null ? formatDuration(p.minutes * 60000) : '—'), csv: (p) => p.minutes },
      { key: 'cost', label: `Cost (${cur})`, num: true, render: (p) => formatMoney(p.cost, cur), csv: (p) => p.cost },
    ],
    csv: { filename: csvFilename(car.name, 'charge', day, 'cost'), columns: [{ label: 'Rate', value: (p) => p.rate }, { label: `Per kWh (${cur})`, value: (p) => p.per_kwh }, { label: `Per minute (${cur})`, value: (p) => p.per_minute }, { label: 'Energy (kWh)', value: (p) => p.kwh }, { label: 'Minutes', value: (p) => p.minutes }, { label: `Cost (${cur})`, value: (p) => p.cost }] },
    empty: () => emptyState({ title: 'No parts', text: cost?.source === 'free_supercharging' ? 'Free Supercharging: nothing to pay.' : cost ? 'This cost is one amount with no rates.' : 'There is no cost to break down yet.' }),
    note: cost?.source === 'tesla_invoice' || cost?.source === 'teslafi' ? `The fees on the ${invoiceSourceLabel(cost.source === 'teslafi' ? 'teslafi' : 'tesla')}, as billed.`
      : cost?.source === 'tariff' ? `Each rate of the place price for the time it applied, in the car's time zone (${zone})${bd.gap_ms ? `; ${formatDuration(bd.gap_ms)} without data is priced at the rate around it` : ''}.` : null,
  });

  const facts = kv([
    ['When', h('span', { cover: 'web.charge-cost.dates' }, `${formatDate(t0, tz, { weekday: false })} ${formatTime(t0, tz)} – ${live ? 'now' : formatTime(t1, tz)} ${zone}`, otherZone ? h('span', { class: 'faint block' }, otherZone) : null)],
    ['Charging time', h('span', { cover: 'web.charge-cost.duration' }, formatDuration(c.charging_ms), h('span', { class: 'faint block' }, 'pauses under 3 min count as one charge'))],
    ['Energy', h('span', { cover: 'web.charge-cost.energy' }, `${format(c.energy_added_kwh, 'energy')} into the pack`,
      h('span', { class: 'faint block' }, ac && gridKwh != null ? `${format(gridKwh, 'energy')} drawn, ${format(c.losses_kwh, 'energy')} lost` : dc ? `${format(c.dc_inlet_kwh, 'energy')} delivered; grid energy unknown` : 'energy drawn unknown'))],
    ['Place', price ? `${placeLabel(c)}: ${[price.per_kwh != null ? format(price.per_kwh, 'price_energy', { currency: price.currency }) : null, price.per_minute != null ? format(price.per_minute, 'price_time', { currency: price.currency }) : null, (price.rates || []).length ? `${price.rates.length} time rates` : null, price.session_fee ? `${formatMoney(price.session_fee, price.currency)} per session` : null].filter(Boolean).join(', ') || 'priced'}` : `${placeLabel(c)}: no price`],
  ]);

  const editor = costEditor(ctx, { c, base, manual, cur, gridKwh, dc });

  replace(ctx.view, h('div', { class: 'charge-page', cover: ['shots.charge-details', 'cd.context.session'] },
    pageHead({
      back: null,
      eyebrow: trail,
      title: [placeLabel(c)],
      titleExtra: live ? h('span', { class: 'pill pill--lg pill--warm' }, icon('bolt'), 'Charging now') : h('span', { class: 'tag' }, KIND_WORDS[c.kind] || 'Charge'),
      sub: [`${formatDate(t0, tz, { weekday: 'long', month: 'long' })}, ${formatTime(t0, tz)} – ${live ? 'now' : formatTime(t1, tz)} ${zone}`, format(c.energy_added_kwh, 'energy'), formatDuration(c.charging_ms), srcMark(c, { estimated: true })],
      actions: [
        back,
        c.prev ? h('a', { class: 'btn btn--icon', href: state.buildUrl('charge', { car: cid, charge: c.prev }), 'aria-label': 'Previous charge', title: 'Previous charge' }, icon('chev-l')) : null,
        c.next ? h('a', { class: 'btn btn--icon', href: state.buildUrl('charge', { car: cid, charge: c.next }), 'aria-label': 'Next charge', title: 'Next charge' }, icon('chev-r')) : null,
        driveBefore ? h('a', { class: 'btn', href: state.buildUrl('drive', { car: cid, drive: driveBefore }) }, icon('route'), 'Drive before') : null,
        driveAfter ? h('a', { class: 'btn', href: state.buildUrl('drive', { car: cid, drive: driveAfter }) }, icon('route'), 'Drive after') : null,
        h('a', { class: 'btn', href: state.buildUrl('timeline', { car: cid, period: dayRange(day, day, tz) }) }, icon('timeline'), 'That day'),
      ],
    }),
    h('div', { class: 'charge-layout' }, stats, mapPanel),
    chargePanel,
    h('div', { class: 'charge-lower' }, supplyPanel, curvePanel),
    h('section', { class: 'panel panel-pad cost-panel', 'aria-label': 'Cost', id: 'cost' },
      h('div', { class: 'panel-head' }, h('h2', { class: 'panel-title' }, icon('book'), 'Cost')),
      h('div', { class: 'cost-grid' },
        h('div', { class: 'cost-left' }, costHead, invoices, facts, breakdown.el),
        editor)),
    h('div', { hidden: true, cover: 'web.charge-cost.not-found' }, emptyState({ title: 'No such charge', text: 'A charge number that does not exist, or one of a car your account may not see, gives this same answer.' }))));
  await breakdown.ready;
  if (!ctx.alive()) return;
  // ready once the charts are in too (below)
  const shown = () => {
    ctx.ready();
    // ?edit=cost (the cost cell of the charges list): go straight to the cost editor
    // (on the next frame: the router puts a new page at the top once mount returns)
    if (ctx.query?.edit === 'cost') {
      requestAnimationFrame(() => {
        if (!ctx.alive()) return;
        const sec = ctx.view.querySelector('#cost');
        sec?.scrollIntoView({ block: 'start' });
        sec?.querySelector('.cost-edit input, .cost-edit select, .cost-edit button')?.focus({ preventScroll: true });
      });
    }
  };

  /* ---------- the map */
  (async () => {
    const pos = c.position;
    if (pos?.lat == null) { mapEl.append(mapFallback('No position was recorded for this charge.')); return; }
    try {
      const m = await createMap(mapEl, ctx, { center: [pos.lon, pos.lat], zoom: 15 });
      if (!m) return;
      await m.ready;
      if (!ctx.alive()) return;
      carMarker(m, [pos.lon, pos.lat], { tone: 'warm' });
      m.frame(() => m.map.jumpTo({ center: [pos.lon, pos.lat], zoom: 15, padding: { top: Math.min(placeCard.offsetHeight + rpx(24), mapEl.clientHeight * 0.5), left: 0, right: 0, bottom: 0 } }));
    } catch (e) { console.warn(e); replace(mapEl, mapFallback()); }
  })();

  /* ---------- charts */
  if (!t.length) {
    for (const host of [chargeHost, supplyHost, curveHost]) host.append(h('p', { class: 'note' }, 'No curve was recorded for this charge.'));
    for (const id2 of ['cd.chart.soc', 'cd.chart.power', 'cd.chart.range', 'cd.chart.outdoor_temp', 'cd.chart.battery_heater']) cover(chargeHost, id2);
    for (const id2 of ['cd.chart.ac_voltage', 'cd.chart.ac_current', 'cd.chart.pilot_current', 'cd.chart.phases']) cover(supplyHost, id2);
    selBox.append(h('span', { class: 'selection-title' }, 'Nothing to zoom: no curve'));
    shown();
    return;
  }
  const x = t.map((ms) => ms / 1000);
  const V = (f, kind) => col(f).map((v) => (v == null || typeof v !== 'number' ? null : kind ? convert(v, kind) : v));
  const known = (f) => col(f).some((v) => v != null);
  const heat = col('heater_kw').filter((v) => v != null);
  const heatKwh = heat.length ? t.reduce((s, ms, i) => { const v = col('heater_kw')[i]; const dt = i ? (ms - t[i - 1]) / 3.6e6 : 0; return v == null || dt > 0.05 ? s : s + v * dt; }, 0) : null;
  const tUnit = unitLabel('temperature');
  const chargeDefs = [
    { key: 'soc', name: 'Battery', unit: '%', cover: 'cd.chart.soc', digits: 0, series: [{ label: 'Battery', data: V('soc_pct'), color: '--c-soc', area: true }, { label: 'Usable', data: V('usable_soc_pct'), color: '--c-power', dash: true }],
      note: known('usable_soc_pct') ? 'Usable is the part you can drive on; a cold battery locks some of it.' : null },
    { key: 'power', name: 'Charging power', unit: 'kW', cover: 'cd.chart.power', digits: 1, series: [{ label: 'Into the pack', data: V('power_kw'), color: '--c-charge', area: true }, ...(known('ac_line_kw') ? [{ label: 'From the line', data: V('ac_line_kw'), color: '--warm', dash: true }] : [])],
      note: known('ac_line_kw') ? 'The gap between the two lines is what the on-board charger and cables lose.' : dc ? 'Measured at the pack; the charger does not send its own power.' : null },
    { key: 'range', name: rangeName, unit: unitLabel('range'), cover: 'cd.chart.range', digits: 0, series: [{ label: 'Range', data: V('range_km', 'range'), color: '--live' }] },
    { key: 'temps', name: 'Temperatures', unit: tUnit, cover: 'cd.chart.outdoor_temp', digits: 1, series: [{ label: 'Battery', data: V('battery_c', 'temperature'), color: '--warm' }, { label: 'Outside', data: V('outside_c', 'temperature'), color: '--c-power' }],
      note: known('battery_c') ? null : 'Battery temperature is not in imported history.' },
    { key: 'heat', name: 'Battery heating', unit: 'kW', cover: 'cd.chart.battery_heater', digits: 1, zero: true, series: [{ label: 'Heating', data: V('heater_kw'), color: '--warm', area: true }], showX: true,
      note: !heat.length ? 'This car does not send battery heating power (or it is imported history).'
        : heat.some((v) => v > 0) ? `The pack warmed itself to charge faster: about ${format(heatKwh, 'energy')}, taken from the supply.` : 'The pack did not heat itself during this charge (0 kW all the way).' },
  ];
  const supplyDefs = ac ? [
    { key: 'volts', name: 'Line voltage', unit: 'V', cover: 'cd.chart.ac_voltage', digits: 0, series: [{ label: 'Volts', data: V('ac_volts_v'), color: '--c-power' }],
      note: known('ac_volts_v') ? 'Per phase, at the car’s inlet. A dip under load means a long or thin supply cable.' : 'Line voltage was not recorded for this charge.' },
    { key: 'amps', name: 'Current', unit: 'A', cover: ['cd.chart.ac_current', 'cd.chart.pilot_current'], digits: 0, zero: true, series: [{ label: 'Drawn', data: V('ac_amps_a'), color: '--c-charge', area: true }, { label: 'Pilot (allowed)', data: V('pilot_a'), color: '--warm', dash: true, step: true }],
      note: known('pilot_a') ? 'The pilot is the most the station lets the car draw; the car draws less near the end.' : 'The station’s pilot current is not in this record.' },
    { key: 'phases', name: 'Phases in use', unit: 'count', cover: 'cd.chart.phases', digits: 0, range: [0, 3.5], series: [{ label: 'Phases', data: V('phases'), color: '--c-soc', step: true }], showX: true,
      note: known('phases') ? 'Counted from the charger’s phase switches each second: 1 for a home socket, 3 for most European wall boxes.' : 'Phases were not recorded for this charge.' },
  ] : [
    { key: 'volts', name: dc ? 'Charger voltage (DC)' : 'Voltage', unit: 'V', cover: 'cd.chart.ac_voltage', digits: 0, series: [{ label: dc ? 'Charger' : 'Pack', data: V(dc ? 'dc_volts_v' : 'volts_v'), color: '--c-power' }],
      note: dc ? 'The charger’s output at the inlet; there is no AC line on a fast charge.' : null },
    { key: 'amps', name: dc ? 'Charger current (DC)' : 'Current', unit: 'A', cover: ['cd.chart.ac_current', 'cd.chart.pilot_current'], digits: 0, zero: true, series: [{ label: dc ? 'Charger' : 'Pack', data: V(dc ? 'dc_amps_a' : 'amps_a'), color: '--c-charge', area: true }],
      note: dc ? 'A fast charger has no pilot current: the car asks for the current it wants and the charger delivers it.' : null },
    { key: 'phases', name: 'Phases in use', unit: 'count', cover: 'cd.chart.phases', digits: 0, empty: true, showX: true, series: [],
      note: dc ? 'None: a fast charge is DC, so there are no AC phases.' : 'Not known for this charge.' },
  ];

  const xMin = x[0], xMax = x[x.length - 1];
  let zoom = [xMin, xMax];
  const plots = [];
  const gaps = (c.gaps || []).map((g) => [g.start_ms / 1000, (g.end_ms ?? t1) / 1000]);
  const blocks = [...chargeDefs.map((df) => ({ df, host: chargeHost })), ...supplyDefs.map((df) => ({ df, host: supplyHost }))].map(({ df, host }) => {
    const valEl = h('span', { class: 'val' });
    const target = h('div', { class: 'chart' });
    const block = h('div', { class: 'chart-block', dataset: { series: df.key }, cover: df.cover },
      h('div', { class: 'chart-label' }, h('span', { class: 'name' }, df.name, df.unit !== 'count' ? h('span', { class: 'chart-unit' }, ` · ${df.unit}`) : null),
        df.series.map((s) => h('span', { class: ['key', s.dash && 'key--dash'] }, h('i', { vars: { '--c': `var(${s.color})` } }), s.label)), valEl),
      df.empty ? null : target, df.note ? h('p', { class: 'chart-note' }, df.note) : null);
    host.append(block);
    return { df, target, valEl };
  });
  const idle = () => { for (const b of blocks) b.valEl.textContent = ''; };
  shown();

  function paintSelection() {
    const [a, b] = zoom;
    const idx = x.map((s, i) => (s >= a && s <= b ? i : -1)).filter((i) => i >= 0);
    const pw = idx.map((i) => col('power_kw')[i]).filter((v) => v != null);
    const soc = idx.map((i) => col('soc_pct')[i]).filter((v) => v != null);
    let kwh = 0;
    for (let k = 1; k < idx.length; k++) { const v = col('power_kw')[idx[k]]; const dt = (t[idx[k]] - t[idx[k - 1]]) / 3.6e6; if (v != null && dt < 0.05) kwh += v * dt; }
    const zoomed = a > xMin + 1 || b < xMax - 1;
    resetBtn.hidden = !zoomed;
    // the whole session shows the charge's own figures (as in Figures); a zoomed part is worked out from the curve
    replace(selBox, h('span', { class: 'selection-title' }, zoomed ? `Selection ${formatTime(a * 1000, tz)} – ${formatTime(b * 1000, tz)}` : 'The whole session'),
      metric('Duration', zoomed ? (b - a) * 1000 : c.charging_ms, 'duration', { size: 'sm', explain: zoomed ? null : 'charging time' }),
      zoomed ? metric('Energy added', pw.length ? kwh : null, 'energy', { size: 'sm', explain: 'from the 1 Hz curve, power × time', why: 'No power here' })
        : metric('Energy added', c.energy_added_kwh, 'energy', { size: 'sm', explain: est ? estWord : 'the figure for this charge', why }),
      zoomed ? metric('Average power', pw.length ? pw.reduce((s, v) => s + v, 0) / pw.length : null, 'power', { size: 'sm', explain: 'from the 1 Hz curve', why: 'No power here' })
        : metric('Average power', c.power_avg_kw, 'power', { size: 'sm', explain: c.power_max_kw != null ? `peak ${format(c.power_max_kw, 'power')}` : null, why }),
      zoomed ? metric('Battery', soc.length > 1 ? soc[soc.length - 1] - soc[0] : null, 'percent', { size: 'sm', sign: true, explain: 'from the 1 Hz curve', why: 'No battery % here' })
        : metric('Battery', socAdded, 'percent', { size: 'sm', sign: true, why }));
  }
  const zoomTo = (a, b) => { zoom = [a, b]; for (const p of plots) p.u?.setScale('x', { min: a, max: b }); paintSelection(); };
  resetBtn.addEventListener('click', () => zoomTo(xMin, xMax));
  paintSelection();

  // stretches without data: grey hatching like the rest of the pack's unknown time (a canvas cannot take the
  // CSS gradient of --nodata-fill, so the tile is drawn from --st-unknown; remade when the theme changes)
  let gapTile = null;
  const gapPattern = (cx) => {
    const g = cssVar('--st-unknown') || '#7D889C';
    const n = Math.max(4, Math.round(rpx(7) * (window.devicePixelRatio || 1)));
    if (gapTile?.g !== g || gapTile?.n !== n) {
      const tile = document.createElement('canvas');
      tile.width = tile.height = n;
      const tc = tile.getContext('2d');
      tc.fillStyle = hexA(g, 0.14); tc.fillRect(0, 0, n, n);
      tc.strokeStyle = hexA(g, 0.5); tc.lineWidth = n * 0.3;
      tc.beginPath();
      for (const o of [-n, 0, n]) { tc.moveTo(o, n); tc.lineTo(o + n, 0); }
      tc.stroke();
      gapTile = { g, n, tile };
    }
    return cx.createPattern(gapTile.tile, 'repeat') || hexA(g, 0.25);
  };
  const drawGaps = (u) => {
    if (!gaps.length) return;
    const { left, top, width, height } = u.bbox;
    const cx = u.ctx;
    cx.save(); cx.beginPath(); cx.rect(left, top, width, height); cx.clip();
    cx.fillStyle = gapPattern(cx);
    for (const [a, b] of gaps) { const pa = u.valToPos(a, 'x', true), pb = u.valToPos(b, 'x', true); cx.fillRect(pa, top, Math.max(1, pb - pa), height); }
    cx.restore();
  };
  const first = blocks[0];
  for (const { df, target } of blocks) {
    if (df.empty) continue;
    const make = (width, height) => {
      const cl = colors();
      return {
        width, height, padding: [rpx(10), rpx(14), df.showX ? 0 : rpx(6), 0], legend: { show: false },
        cursor: { sync: { key: `charge-${chid}`, setSeries: false }, drag: { x: true, y: false, setScale: false }, points: { size: rpx(8), width: rpx(2), stroke: cl.cursor } },
        select: { show: true },
        scales: { x: { time: true }, y: { auto: true, range: df.range ? () => df.range : df.zero ? (u, a, b) => [0, Math.max(1, (b || 0) * 1.15)] : undefined } },
        axes: [timeAxis(tz, { show: !!df.showX }), valueAxis({ size: 56, digits: df.digits ?? 0 })],
        series: [{}, ...df.series.map((s) => {
          const cc = cssVar(s.color);
          return { label: s.label, stroke: cc, width: rpx(s.dash ? 1.5 : 2), dash: s.dash ? [rpx(4), rpx(3)] : undefined, fill: s.area ? areaFill(cc) : undefined, paths: s.step ? window.uPlot.paths.stepped({ align: 1 }) : undefined, points: { show: false } };
        })],
        hooks: {
          drawClear: [drawGaps],
          setSelect: [(u) => {
            if (u.select.width < rpx(6)) return;
            const a = u.posToVal(u.select.left, 'x'), b = u.posToVal(u.select.left + u.select.width, 'x');
            u.setSelect({ left: 0, top: 0, width: 0, height: 0 }, false);
            zoomTo(a, b);
          }],
          setCursor: df === first.df ? [(u) => {
            const i = u.cursor.idx;
            if (i == null || u.cursor.left < 0) { timeEl.textContent = ''; idle(); return; }
            timeEl.textContent = formatTime(t[i], tz, { seconds: true });
            for (const b of blocks) {
              if (b.df.empty) continue;
              const ps = b.df.series.map((s) => (s.data[i] == null ? '—' : formatNumber(s.data[i], { maximumFractionDigits: b.df.digits ?? 0 })));
              b.valEl.textContent = ps.join(' / ') + (b.df.unit === 'count' ? '' : ` ${b.df.unit}`);
            }
          }] : [],
        },
      };
    };
    plots.push(mountPlot(target, make, [x, ...df.series.map((s) => s.data)], ctx, (u) => {
      u.over.addEventListener('dblclick', () => zoomTo(xMin, xMax));
      if (zoom[0] > xMin || zoom[1] < xMax) u.setScale('x', { min: zoom[0], max: zoom[1] });
    }));
  }

  /* ---------- power against battery % */
  const pts = [];
  t.forEach((ms, i) => { const s = col('soc_pct')[i], p = col('power_kw')[i]; if (s != null && p != null && p > 0.05) pts.push([s, p, col('battery_c')[i] ?? null]); });
  pts.sort((a, b) => a[0] - b[0]);
  const bySoc = new Map();
  for (const [s, p] of pts) { const k = Math.round(s); const e = bySoc.get(k) || [0, 0]; e[0] += p; e[1] += 1; bySoc.set(k, e); }
  const mean = (s) => { const e = bySoc.get(Math.round(s)); return e ? e[0] / e[1] : null; };
  const med = (typical?.median || []).filter((m) => m.power_kw != null);
  const usual = (s) => {
    if (!med.length) return null;
    const k = Math.round(s);
    const hit = med.find((m) => m.soc_pct === k);
    return hit ? hit.power_kw : null;
  };
  function curveCsv() {
    downloadCsv(csvFilename(car.name, 'charge', day, 'power-curve'), pts.map(([s, p, b]) => ({ soc: s, power: p, battery: b, mean: mean(s), usual: dc ? usual(s) : null })),
      [column('Battery', 'soc', 'percent'), column('Power into the pack', 'power', 'power'), column('Battery temperature', 'battery', 'temperature'), column('This session, mean at this %', 'mean', 'power'), column("This car's usual fast charge", 'usual', 'power')]);
  }
  if (pts.length < 2) { curveHost.append(h('p', { class: 'note' }, 'Too few points to draw.')); return; }
  const cx = pts.map((p) => p[0]);
  const data = [cx, pts.map((p) => p[1]), cx.map(mean), dc ? cx.map(usual) : cx.map(() => null)];
  const curveMake = (width, height) => {
    const cl = colors();
    const dot = cssVar('--c-charge');
    return {
      width, height, padding: [rpx(10), rpx(14), 0, 0], legend: { show: false },
      cursor: { drag: { x: false, y: false }, points: { size: rpx(8), width: rpx(2), stroke: cl.cursor } },
      scales: { x: { time: false, auto: true }, y: { auto: true, range: (u, a, b) => [0, Math.max(1, (b || 0) * 1.12)] } },
      axes: [{ stroke: cl.axis, font: chartFont(), grid: { stroke: cl.grid, width: 1 }, ticks: { show: false }, gap: rpx(6), size: rpx(26), space: rpx(60), incrs: [1, 2, 5, 10, 20, 25, 50], values: (u, v) => v.map((n) => format(n, 'percent')) },
        valueAxis({ size: 56, digits: 0 })],
      series: [{},
        { label: 'Sample', stroke: dot, fill: hexA(dot, 0.5), width: 0, paths: () => null, points: { show: true, size: rpx(3.5), width: 0, fill: hexA(dot, 0.55), stroke: hexA(dot, 0.55) } },
        { label: 'Mean', stroke: cssVar('--warm'), width: rpx(2.2), points: { show: false }, spanGaps: true },
        { label: 'Usual', stroke: cssVar('--text-2'), width: rpx(1.6), dash: [rpx(5), rpx(4)], points: { show: false }, spanGaps: true }],
      hooks: {
        setCursor: [(u) => {
          const i = u.cursor.idx;
          if (i == null || u.cursor.left < 0) { curveVal.textContent = ''; return; }
          const s = format(cx[i], 'percent'), m = mean(cx[i]), us = dc ? usual(cx[i]) : null;
          curveVal.textContent = `${s}: ${format(data[1][i], 'power')}${m != null ? ` · mean ${format(m, 'power')}` : ''}${us != null ? ` · usual ${format(us, 'power')}` : ''}`;
        }],
      },
    };
  };
  mountPlot(curveHost, curveMake, data, ctx);
}

/* ---------- Supercharger invoices matched to the charge: the source, the site, the fees and the PDF */
function feeText(f, cur) {
  const usage = f.unit === 'kwh' ? format(f.usage, 'energy') : f.unit === 'min' ? formatDuration(f.usage * 60000) : null;
  const rate = f.rate == null ? null : rateText(f.rate, f.unit === 'min' ? 'price_time' : 'price_energy', cur);
  return `${feeLabel(f.kind)} ${formatMoney(f.amount, cur)}${usage && rate ? ` (${usage} × ${rate})` : ''}`;
}
function invoiceFacts(c) {
  const list = Array.isArray(c.invoices) ? c.invoices : [];
  const pdfs = list.filter((i) => i.pdf);
  // always on the page (hidden when the charge has no invoice), so the layout and the coverage marks stay put
  return h('div', { class: 'cost-invoice', hidden: !list.length, cover: 'inv.charge.source' },
    list.map((i) => h('p', { class: 'cost-invoice-line' },
      h('span', { class: 'cost-invoice-what' }, i.applied
        ? `${invoiceSourceLabel(i.source)} · ${i.site}`
        : `${invoiceSourceLabel(i.source)}: ${formatMoney(i.total, i.currency)} (an entered cost is used instead) · ${i.site}`),
      h('span', { class: 'faint block' }, (i.fees || []).map((f) => feeText(f, i.currency)).join(' · ') || 'No fees listed'))),
    pdfs.length ? h('p', { class: 'cost-invoice-pdf' }, pdfs.map((i, n) => h('a', {
      class: 'btn btn--sm', href: chargeInvoicePdfUrl(c.car, c.id, i.id), download: i.invoice_name || '',
      cover: n === 0 ? 'inv.charge.pdf' : null,
    }, icon('download', 'ico--sm'), pdfs.length > 1 ? `Invoice (PDF) ${n + 1}` : 'Invoice (PDF)')))
      : h('span', { hidden: true, cover: 'inv.charge.pdf' }, list.length ? 'The invoice (PDF) is not downloaded yet.' : ''),
    list.length && !pdfs.length && list.some((i) => i.source === 'tesla') ? h('p', { class: 'note' }, 'The invoice (PDF) is not downloaded yet.') : null);
}

/* ---------- the cost editor: a total, a price per kWh or per minute, the currency and the energy basis */
function costEditor(ctx, { c, base, manual, cur, gridKwh, dc }) {
  const can = !!ctx.admin;
  const minutes = (c.charging_ms || 0) / 60000;
  const startMode = manual?.mode && manual.mode !== 'tariff' ? manual.mode : 'total';
  const startAmount = manual ? manual.amount : c.cost ? c.cost.amount : null;
  const mode = field({ label: 'The amount is', name: 'mode', type: 'select', value: startMode, path: 'mode',
    options: [['total', 'The total for this charge'], ['per_kwh', 'A price per kWh'], ['per_minute', 'A price per minute of charging']] });
  const amount = field({ label: 'Amount', name: 'amount', type: 'number', value: startAmount ?? '', required: true, path: 'amount',
    hint: 'Rates may have up to 5 decimals (0.34500); only the total shown is rounded. Below zero is refused: enter a credit as 0 and note it.',
    attrs: { min: '0', max: '100000', step: startMode === 'total' ? '0.01' : '0.00001', inputmode: 'decimal' } });
  const currency = field({ label: 'Currency', name: 'currency', type: 'select', value: manual?.currency || cur, path: 'currency',
    options: [...new Set([manual?.currency || cur, ...CURRENCIES])].map((k) => [k, k]) });
  const basis = field({ label: 'Per kWh of', name: 'energy_basis', type: 'select', path: 'energy_basis',
    value: manual?.energy_basis || (gridKwh != null ? 'grid' : 'pack'),
    options: [...(gridKwh != null ? [['grid', `Energy drawn from the supply (${format(gridKwh, 'energy')})`]] : []), ['pack', `Energy added to the pack (${format(c.energy_added_kwh, 'energy')})`]],
    hint: dc ? 'On a fast charge the grid energy is unknown, so a price per kWh is over the energy added to the pack.' : null });
  const preview = h('p', { class: 'cost-preview', 'aria-live': 'polite' });
  const sync = () => {
    const m = mode.value();
    amount.input.step = m === 'total' ? '0.01' : '0.00001';
    basis.el.hidden = m !== 'per_kwh';
    const a = amount.value();
    const kwh = basis.value() === 'grid' && gridKwh != null ? gridKwh : c.energy_added_kwh;
    const total = a == null || !Number.isFinite(a) ? null : m === 'total' ? a : m === 'per_kwh' ? a * (kwh || 0) : a * minutes;
    replace(preview, total == null ? 'Type an amount to see the total.'
      : m === 'total' ? `${formatMoney(total, currency.value())} for the whole charge${kwh ? ` (${format(total / kwh, 'price_energy', { currency: currency.value() })})` : ''}.`
        : m === 'per_kwh' ? `${format(kwh, 'energy')} × ${format(a, 'price_energy', { currency: currency.value(), digits: 5 })} = ${formatMoney(total, currency.value())}.`
          : `${formatNumber(minutes, { maximumFractionDigits: 1 })} min × ${format(a, 'price_time', { currency: currency.value(), digits: 5 })} = ${formatMoney(total, currency.value())}.`);
  };
  for (const f of [mode, amount, currency, basis]) { f.input.addEventListener('input', sync); f.input.addEventListener('change', sync); }
  const removeBtn = h('button', { class: 'btn btn--danger btn--sm', type: 'button', hidden: !manual, onclick: async () => {
    removeBtn.disabled = true;
    try {
      await ctx.api.del(`${base}/cost`);
      flash('Entered cost removed: the place price applies again.', { next: true });
      ctx.reload();
    } catch (err) { f.setMessage(err?.message || 'The cost could not be removed. Try again.'); removeBtn.disabled = false; }
  } }, icon('reset', 'ico--sm'), 'Remove the entered cost');
  const f = form({
    fields: [mode, amount, currency, basis], submit: manual ? 'Save the new cost' : 'Save this cost', cls: 'cost-form',
    extra: preview,
    secondary: removeBtn,
    async onSubmit(v) {
      const body = { mode: v.mode, amount: v.amount, currency: v.currency };
      if (v.mode === 'per_kwh') body.energy_basis = v.energy_basis;
      await ctx.api.put(`${base}/cost`, body);
      flash('Cost saved. It stays with this charge when charges are rebuilt.', { next: true });
      ctx.reload();
    },
  });
  cover(f.el, 'cd.link.set_cost', 'web.charge-cost.entry');
  cover(mode.el, 'web.charge-cost.mode');
  cover(amount.el, 'web.charge-cost.amount', 'web.charge-cost.rate-precision');
  cover(f.el.querySelector('.form-actions'), 'web.charge-cost.save', 'web.charge-cost.clear-and-negative');
  cover(f.el.querySelector('.form-message'), 'web.charge-cost.error');
  sync();
  const fs = h('fieldset', { class: 'cost-fieldset', disabled: !can }, f.el);
  return h('div', { class: 'cost-edit' },
    h('h3', { class: 'cost-edit-title' }, manual ? 'Change the entered cost' : 'Enter the cost'),
    h('p', { class: 'note' }, can ? 'For a receipt that differs from the place price. A cost entered here is kept when charges are rebuilt and is never repriced; remove it to go back to the place price.'
      : 'Only an admin can enter or change a cost.'),
    fs);
}
