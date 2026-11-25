// Trip (/cars/{car}/trip?from=&to=): any stretch of time as one trip. The window is the shared period;
// shortcuts set it to the last three drives, a day, this month or a saved trip, and dragging across a
// chart narrows it. The figures come from GET /cars/{car}/trip (distance, time split, speeds, energy,
// cost), the route from GET /tracks (one line per drive, markers for the start, the end, charges and
// overnight stops), the strip from /activity, battery, range and elevation from /series, and the drives
// and charges from their own lists for the same window. A second car (?vs=) is shown side by side with
// the same window. Saved trips (GET/POST/PATCH/DELETE /trips) belong to the account.

import * as state from '../shared/state.js';
import { h, replace, cssVar, steady } from '../shared/dom.js';
import { format, formatMoney, convert, unitLabel, getUnits, rangeLabel } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, formatDateRange, zoneName, dayKey, addDays, dayRange, startOf, DAY } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { downloadCsv, csvFilename, column, timeColumn } from '../shared/csv.js';
import { trackBounds } from '../shared/tracks.js';
import { icon } from '../core/icons.js';
import { spotText } from '../core/fmt.js';
import { pageHead, panel, metric, csvButton } from '../core/page.js';
import { dataTable } from '../core/table.js';
import { emptyState } from '../core/states.js';
import { activityStrip, KINDS as STRIP_KINDS } from '../core/strip.js';
import { createMap, routePin, boundsOf, padClear, mapFallback } from '../core/map.js';
import { loadUPlot, mountPlot, timeAxis, valueAxis, colors, areaFill, hexA } from '../core/chart.js';
import { openPeriodPicker } from '../core/period.js';
import { openCarSwitcher, soldParts, soldOptions } from '../core/carswitch.js';
import { openDialog, confirmDialog } from '../core/dialog.js';
import { flash } from '../core/flash.js';
import { isAdmin } from '../core/prefs.js';
import { rpx } from '../core/display.js';
import { srcText, srcTitle } from '../core/source.js';

export const id = 'trip';
export const css = true;
export const title = () => 'Trip';

const enc = encodeURIComponent;
const iso = (ms) => (ms == null ? '' : new Date(ms).toISOString());
const pct = (v) => format(v, 'percent');
const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
const placeLabel = (p) => p?.place?.name || p?.label || p?.address?.short || spotText(p) || '—';
const pointHref = (p, lat, lon) => (p?.place?.id ? state.buildUrl('places', { query: { place: p.place.id } })
  : lat != null && lon != null ? state.buildUrl('places', { query: { lat: lat.toFixed(5), lon: lon.toFixed(5), new: 1 } }) : null);
const COST_SOURCE = { tariff: 'place price', manual: 'typed by hand', free_supercharging: 'free Supercharging', teslamate: 'imported' };

// the time split: API key, label, the token it is drawn in (always next to its label)
const SPLIT = [
  ['driving', 'Driving', 'driving'],
  ['charging_ac', 'Charging AC', 'ac'],
  ['charging_dc', 'Charging DC (fast)', 'dc'],
  ['charging_unknown', 'Charging (kind not known)', 'unknown'],
  ['parked_awake', 'Parked, awake', 'awake'],
  ['asleep', 'Asleep', 'asleep'],
  ['not_recorded', 'Not recorded (no data)', 'none'],
];

export async function mount(ctx) {
  const { car, tz } = ctx;
  const cid = car.id;
  const now = ctx.api.now();
  const ideal = getUnits().range === 'ideal';
  const rk = ideal ? 'ideal_range_km' : 'range_km';
  const vsId = ctx.query.vs && ctx.query.vs !== cid ? String(ctx.query.vs) : null;

  /* ---------- the window: the shared period; "all" starts at this car's first drive or charge */
  let from = ctx.period?.from_ms ?? null;
  // the window ends now at the latest; one that starts after now keeps its own end (an empty trip, as on the other pages)
  const to = from != null && from >= now ? Math.max(ctx.period?.to_ms ?? 0, from + 1) : Math.min(ctx.period?.to_ms ?? now, now);
  if (from == null) {
    // "all" needs the car's first drive or charge: when neither can be read the page says so (not the last week as if it were all)
    let d0, c0;
    try {
      [d0, c0] = await Promise.all([
        ctx.api.page(`cars/${enc(cid)}/drives`, { sort: 'start', dir: 'asc' }, { limit: 1 }),
        ctx.api.page(`cars/${enc(cid)}/charges`, { sort: 'start', dir: 'asc' }, { limit: 1 }),
      ]);
    } catch (e) { ctx.fail(e); return; }
    if (!ctx.alive()) return;
    const starts = [d0.items?.[0]?.start_ms, c0.items?.[0]?.start_ms].filter((v) => v != null);
    from = starts.length ? Math.min(...starts) : to - 7 * DAY;
  }
  const win = { from_ms: from, to_ms: to };
  const span = Math.max(1, to - from);

  let trip, series, actItems, last3, saved, fleet, vsTrip = null, vsCar = null;
  const lost = {};   // the side reads that failed, by name: each is said where its data would be, never shown as "none"
  const or = (name, p, v) => p.catch((e) => { lost[name] = e; return v; });
  try {
    [trip, series, actItems, last3, saved, fleet] = await Promise.all([
      ctx.api.get(`cars/${enc(cid)}/trip`, win),
      or('series', ctx.api.get(`cars/${enc(cid)}/series`, { fields: `soc_pct,${rk},elevation_m,lat,lon`, ...win, max_points: 1500 }), null),
      or('activity', collect(ctx.api.list(`cars/${enc(cid)}/activity`, win, { limit: 1000 }), 20000), []),
      or('last3', ctx.api.page(`cars/${enc(cid)}/drives`, { sort: 'start', dir: 'desc' }, { limit: 3 }), { items: [] }),
      or('saved', ctx.api.page('trips', { car: cid }, { limit: 100 }), { items: [] }),
      or('cars', ctx.api.page('cars', { sort: 'name' }, { limit: 100 }), { items: [car], next: null }),
    ]);
    if (vsId) {
      [vsCar, vsTrip] = await Promise.all([
        ctx.api.get(`cars/${enc(vsId)}`).catch(() => null),
        ctx.api.get(`cars/${enc(vsId)}/trip`, win).catch(() => null),
      ]);
    }
  } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;

  const nDrives = trip.drives?.length ?? 0;
  const nCharges = trip.charges?.length ?? 0;
  const savedItems = saved.items || [];
  const match = savedItems.find((t) => t.from_ms === from && t.to_ms === (ctx.period?.to_ms ?? null)) || savedItems.find((t) => Math.abs(t.from_ms - from) < 60000 && Math.abs(t.to_ms - to) < 60000);
  const rangeText = formatDateRange(from, Math.max(from, to - 1), tz, { time: true, weekday: true });
  ctx.setTitle(match ? `Trip ${match.name}` : `Trip ${formatDateRange(from, Math.max(from, to - 1), tz, { year: false })}`);

  /* ---------- window and cars */
  const periodLink = (period) => state.buildUrl('trip', { car: cid, period, query: vsId ? { vs: vsId } : null });
  const periodBtn = h('button', { class: 'btn', type: 'button', onclick: (e) => openPeriodPicker(e.currentTarget), cover: 'trip.filter.time_range', title: 'The trip is this window: drives and charges belong to it by their start. Days are the car’s days.' },
    icon('calendar', 'ico--sm'), ctx.period?.preset === 'custom' ? 'Custom window' : ctx.period?.label || 'Window', icon('chev-d', 'ico--sm'));
  const l3 = last3.items || [];
  const l3From = l3.length ? l3[l3.length - 1].start_ms : null;
  const lastThree = l3From != null
    ? h('a', { class: 'btn btn--sm', href: periodLink({ from: l3From, to: null }), cover: 'trip.link.last_three_drives', title: `From the start of the ${l3.length === 3 ? 'third newest' : 'oldest'} drive (${formatDate(l3From, tz, { year: false })}, ${formatTime(l3From, tz)}) up to now` }, icon('route', 'ico--sm'), l3.length === 3 ? 'Last three drives' : `Last ${l3.length === 1 ? 'drive' : `${l3.length} drives`}`)
    : h('button', { class: 'btn btn--sm', type: 'button', disabled: true, cover: 'trip.link.last_three_drives', title: lost.last3 ? `The drives could not be loaded: ${lost.last3.message}` : 'This car has no drives yet' }, icon('route', 'ico--sm'), 'Last three drives');
  const today = dayKey(now, tz);
  const shortcuts = h('div', { class: 'trip-shortcuts', role: 'group', 'aria-label': 'Window shortcuts' },
    lastThree,
    h('a', { class: 'btn btn--sm', href: periodLink({ from: dayRange(today, today, tz).from, to: null }) }, 'Today'),
    h('a', { class: 'btn btn--sm', href: periodLink(dayRange(addDays(today, -1), addDays(today, -1), tz)) }, 'Yesterday'),
    h('a', { class: 'btn btn--sm', href: periodLink({ from: startOf(now, 'month', tz), to: null }), title: 'The same month as on Statistics' }, 'This month'));

  // saved trips: open one, save this window, rename or remove the one shown
  const savedSel = h('select', { class: 'select', 'aria-label': 'Open a saved trip', disabled: !savedItems.length },
    h('option', { value: '' }, savedItems.length ? `Saved trips (${formatNumber(savedItems.length)})` : lost.saved ? 'Saved trips could not be loaded' : 'No saved trips yet'),
    savedItems.map((t) => h('option', { value: t.id, selected: match?.id === t.id }, `${t.name} · ${formatDateRange(t.from_ms, Math.max(t.from_ms, t.to_ms - 1), tz, { year: true })}`)));
  savedSel.addEventListener('change', () => {
    const t = savedItems.find((x) => x.id === savedSel.value);
    if (t) ctx.navigate(periodLink({ from: t.from_ms, to: t.to_ms }));
  });
  const nameDialog = async (titleText, initial, onSave) => {
    const input = h('input', { class: 'input', id: 'trip-name', value: initial, maxlength: '80', required: true, autocomplete: 'off' });
    const err = h('p', { class: 'form-error', role: 'alert', hidden: true });
    const body = h('div', { class: 'field' }, h('label', { class: 'form-label', for: 'trip-name' }, 'Name'), input,
      h('p', { class: 'form-hint' }, `${rangeText} (${zoneName(from, tz)}). Only you see your saved trips.`), err);
    return openDialog({ title: titleText, body, actions: [{ label: 'Cancel', value: false }, { label: 'Save', value: true, kind: 'primary', onClick: async () => {
      const name = input.value.trim();
      if (!name) { err.hidden = false; err.textContent = 'Give the trip a name.'; return false; }
      try { await onSave(name); return true; } catch (e) {
        err.hidden = false; err.textContent = e?.fields?.name || e?.fields?.to_ms || e?.message || 'Could not save the trip.'; return false;
      }
    } }] });
  };
  const saveBtn = h('button', { class: 'btn btn--sm btn--primary', type: 'button', onclick: async () => {
    const ok = await nameDialog('Save this trip', '', (name) => ctx.api.post('trips', { car: cid, from_ms: from, to_ms: to, name }));
    if (ok && ctx.alive()) { flash('Trip saved. Open it again from Saved trips.', { next: true }); ctx.navigate(periodLink({ from, to })); }
  } }, icon('plus', 'ico--sm'), 'Save as a trip');
  const renameBtn = match ? h('button', { class: 'btn btn--sm', type: 'button', onclick: async () => {
    const ok = await nameDialog('Rename the trip', match.name, (name) => ctx.api.patch(`trips/${enc(match.id)}`, { name }));
    if (ok && ctx.alive()) { flash('Trip renamed.'); ctx.reload(); }
  } }, 'Rename') : null;
  const removeBtn = match ? h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: async () => {
    if (!(await confirmDialog(`Remove the saved trip “${match.name}”? The drives and charges stay; only the saved window goes.`, { title: 'Remove saved trip?', ok: 'Remove', danger: true }))) return;
    try { await ctx.api.del(`trips/${enc(match.id)}`); flash('Saved trip removed.'); ctx.reload(); } catch (e) { flash(e.message || 'Could not remove the trip.', { kind: 'error' }); }
  } }, 'Remove') : null;

  // a second car for side by side; the list is searched when there are more cars than it shows
  const others = (fleet.items || []).filter((c) => c.id !== cid);
  const vsSel = h('select', { class: 'select', id: 'trip-vs', disabled: !others.length && !fleet.next },
    h('option', { value: '' }, others.length || fleet.next ? 'No second car' : lost.cars ? 'The other cars could not be loaded' : 'Only one car'),
    // sold cars last, in a "Sold cars" group (a sold car already compared stays in place)
    (() => { const { main, sold } = soldParts(others, [vsId]); const mk = (c) => h('option', { value: c.id, selected: c.id === vsId }, c.name); return [main.map(mk), soldOptions(sold, mk)]; })(),
    vsId && !others.some((c) => c.id === vsId) && vsCar ? h('option', { value: vsId, selected: true }, vsCar.name) : null);
  vsSel.addEventListener('change', () => { ctx.setQuery({ vs: vsSel.value || null }); ctx.reload(); });
  const vsSearch = fleet.next ? h('input', { class: 'input input--sm', type: 'search', placeholder: 'Find a car', 'aria-label': 'Find a car to compare' }) : null;
  if (vsSearch) {
    let t = 0;
    vsSearch.addEventListener('input', () => {
      clearTimeout(t);
      t = setTimeout(async () => {
        const pg = await ctx.api.page('cars', { sort: 'name', search: vsSearch.value.trim() || null }, { limit: 100 }).catch(() => null);
        if (!pg || !ctx.alive()) return;
        const { main, sold } = soldParts((pg.items || []).filter((c) => c.id !== cid), [vsId], { search: !!vsSearch.value.trim() });
        const mk = (c) => h('option', { value: c.id, selected: c.id === vsId }, c.name);
        replace(vsSel, h('option', { value: '' }, 'No second car'), main.map(mk), soldOptions(sold, mk));
      }, 300);
    });
    ctx.onCleanup(() => clearTimeout(t));
  }
  const vsBox = h('span', { class: 'trip-vs', cover: 'trip.control.per_car', title: 'Show a second car beside this one, with the same window' },
    h('label', { class: 'form-label', for: 'trip-vs' }, 'Compare with'), vsSearch, vsSel);

  const windowPanel = h('section', { class: 'panel panel-pad trip-window', 'aria-label': 'Window and cars' },
    h('div', { class: 'trip-window-row' }, periodBtn, shortcuts, h('span', { class: 'grow' }), vsBox),
    h('div', { class: 'trip-window-row' },
      h('span', { class: 'trip-window-text' }, icon('clock', 'ico--sm'), `${rangeText} ${zoneName(from, tz)}`, h('span', { class: 'faint' }, ` · ${formatDuration(span)} · drag across a chart to narrow the window; Back returns to the wider one`)),
      h('span', { class: 'grow' }), savedSel, renameBtn, removeBtn, saveBtn));

  /* ---------- trip figures */
  const noDrive = nDrives === 0;
  const whyDrive = 'No drive in this window';
  const cost = trip.cost || {};
  const costList = (cost.by_currency || []).filter((x) => x.amount != null);
  const per100 = (cost.per_100km || []).filter((x) => x.amount != null);
  const tm = trip.time_ms || {};
  const dist = trip.distance || {};
  // an odometer reading the car's own inputs lacked, filled from an import (the drives list marks its rows the same way)
  const odoFill = trip.value_sources?.odometer_km?.length ? srcText({ sources: trip.value_sources.odometer_km }) : '';
  const en = trip.energy_kwh || {};
  const costWhy = nCharges === 0 ? 'No charge in this window' : 'No charge has a price yet';
  const moneyEl = (list, kind) => (list.length ? h('span', { class: 'money-stack' }, list.map((x) => h('span', null, kind ? format({ amount: x.amount, currency: x.currency }, kind) : formatMoney(x.amount, x.currency), list.length > 1 ? h('small', { class: 'cur' }, ` ${x.currency}`) : null))) : null);
  const figs = panel({ title: 'Trip figures', icon: 'stats', cls: 'trip-figures', cover: 'trip.empty_states', side: csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'trip', dayKey(from, tz), 'figures'), figureRows(trip, car), FIG_COLS) }) },
    h('div', { class: 'trip-fig-grid' },
      metric('Distance', dist.odometer_km, 'distance', { size: 'lg', cover: 'trip.stat.mileage', why: noDrive ? whyDrive : 'No odometer reading', explain: !noDrive && dist.drives_km != null ? `by odometer${odoFill ? ` (a reading filled in: ${odoFill.charAt(0).toLowerCase()}${odoFill.slice(1)})` : ''}; the ${formatNumber(nDrives)} ${nDrives === 1 ? 'drive adds' : 'drives add'} up to ${format(dist.drives_km, 'distance')}` : 'by odometer' }),
      metric('Average speed, moving', trip.speed_kph?.moving, 'speed', { cover: 'trip.stat.speed_moving', why: whyDrive, explain: noDrive ? 'distance over the time driving' : `distance over ${formatDuration(tm.driving || 0)} of driving` }),
      metric('Average speed, with fast charging', trip.speed_kph?.incl_dc, 'speed', { cover: 'trip.stat.speed_incl_dc', why: whyDrive, explain: tm.charging_dc ? `the ${formatDuration(tm.charging_dc)} at DC chargers counted as travel time` : 'no DC charging in this window, so the same as moving' }),
      metric('Net consumption', trip.wh_per_km?.net, 'efficiency', { cover: 'trip.stat.consumption_net', why: noDrive ? whyDrive : 'Too little distance', explain: noDrive ? 'energy at the pack per distance, regen taken off' : `${format(en.drive_net, 'energy')} the pack gave while driving, regen taken off` }),
      metric('Gross consumption', trip.wh_per_km?.gross, 'efficiency', { cover: 'trip.stat.consumption_gross', why: noDrive ? whyDrive : 'Too little distance', explain: `${format(en.gross, 'energy')} used in all, parked time included, from the pack’s own counters; the same method for any window length` }),
      metric('Charging cost', moneyEl(costList), null, { cover: 'trip.stat.charging_cost', why: costWhy, explain: nCharges ? `${formatNumber(cost.priced ?? 0)} of ${formatNumber(nCharges)} ${nCharges === 1 ? 'charge' : 'charges'} priced${cost.unpriced ? `; ${formatNumber(cost.unpriced)} without a price are not counted` : ''}; per currency, never added across` : 'charges that started in the window' }),
      metric('Cost per distance', moneyEl(per100, 'money_per_distance'), null, { cover: 'trip.stat.cost_per_100', why: noDrive ? whyDrive : costList.length ? 'Too little distance' : costWhy, explain: 'the charging cost over the distance driven' })),
    noDrive ? h('p', { class: 'note' }, 'Nothing was driven in this window. Pick a wider window or a shortcut above; a blank figure says why it is blank.') : null);

  /* ---------- time spent */
  const splitTotal = SPLIT.reduce((s, [k]) => s + (tm[k] || 0), 0);
  const splitBar = h('div', { class: 'trip-split', role: 'img', 'aria-label': SPLIT.map(([k, l]) => `${l} ${formatDuration(tm[k] || 0)}`).join(', ') },
    SPLIT.filter(([k]) => tm[k] > 0).map(([k, l, tone]) => h('i', { dataset: { tone }, vars: { '--w': `${((tm[k] / Math.max(1, splitTotal)) * 100).toFixed(3)}%` }, title: `${l}: ${formatDuration(tm[k])}` })));
  const timePanel = panel({ title: 'Time spent', icon: 'clock', cover: 'trip.chart.time_spent', side: csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'trip', dayKey(from, tz), 'time'), SPLIT.map(([k, l]) => ({ part: l, minutes: Math.round((tm[k] || 0) / 60000), share: splitTotal ? Math.round(((tm[k] || 0) / splitTotal) * 1000) / 10 : null })), [column('Part', 'part'), column('Minutes', 'minutes'), column('Share (%)', 'share')]) }) },
    splitBar,
    h('ul', { class: 'trip-split-legend' }, SPLIT.map(([k, l, tone]) => h('li', null, h('i', { dataset: { tone }, 'aria-hidden': 'true' }), h('span', null, l),
      h('b', null, formatDuration(tm[k] || 0)), h('span', { class: 'faint' }, splitTotal ? `${formatNumber(((tm[k] || 0) / splitTotal) * 100, { maximumFractionDigits: 0 })} %` : '—')))),
    h('p', { class: 'note' }, 'Parked time is split into awake and asleep; time with no log (a paused recording, a failed check) is “not recorded” and is never counted as sleep.'));

  /* ---------- energy used and put back */
  const eRows = [
    ['gross', 'Used in all', en.gross, 'everything, parked time included'],
    ['net', 'Used driving', en.drive_net, 'while driving, regen taken off'],
    ['ac', 'Added AC', en.added_ac, 'into the pack, home and public AC'],
    ['dc', 'Added DC', en.added_dc, 'into the pack at fast chargers'],
    ['unknown', 'Added (kind not known)', en.added_unknown, 'into the pack, charges that did not say AC or DC'],
  ];
  const eMax = Math.max(1e-6, ...eRows.map((r) => r[2] || 0));
  const energyPanel = panel({ title: 'Energy', icon: 'bolt', cover: 'trip.chart.energy', side: csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'trip', dayKey(from, tz), 'energy'), eRows.map(([, l, v]) => ({ what: l, kwh: v })), [column('What', 'what'), column('Energy', 'kwh', 'energy')]) }) },
    h('ul', { class: 'hbars trip-energy' }, eRows.map(([k, l, v, ex]) => h('li', { dataset: { tone: k } },
      h('span', { class: 'hbar-label' }, h('b', null, l), h('small', null, ex)),
      h('span', { class: 'hbar' }, h('i', { vars: { '--w': `${((v || 0) / eMax) * 100}%` } })),
      h('span', { class: 'hbar-val' }, format(v, 'energy'))))),
    h('p', { class: 'note' }, 'Measured at the pack. Energy added counts charges that started in the window; the grid side is on Charges.'));

  /* ---------- second car */
  const comparePanel = vsId ? compareBlock(ctx, car, trip, vsId, vsCar, vsTrip, win, tz) : null;

  /* ---------- the map */
  const mapEl = h('div', { class: 'map', role: 'img', 'aria-label': `Map of the trip from ${rangeText}` });
  const mapLegend = h('div', { class: 'glass-card trip-map-legend' },
    h('b', null, `${formatNumber(nDrives)} ${nDrives === 1 ? 'drive' : 'drives'} · ${formatNumber(nCharges)} ${nCharges === 1 ? 'charge' : 'charges'}`),
    h('span', null, h('span', { class: 'route-pin route-pin--a', 'aria-hidden': 'true' }, 'A'), 'Start'),
    h('span', null, h('span', { class: 'route-pin route-pin--b', 'aria-hidden': 'true' }, 'B'), 'End'),
    h('span', null, h('i', { class: 'trip-dot', dataset: { kind: 'charge' } }), 'Charge (click to open)'),
    h('span', null, h('i', { class: 'trip-dot', dataset: { kind: 'overnight' } }), 'Overnight stop'),
    vsCar && vsTrip ? h('span', null, h('i', { class: 'trip-line', dataset: { car: 'b' } }), vsCar.name) : null);
  const readout = h('div', { class: 'glass-card trip-readout', hidden: true, 'aria-hidden': 'true' });
  const mapPanel = h('section', { class: 'panel map-panel trip-map', 'aria-label': 'Route map', cover: 'trip.map.route' },
    mapEl, h('div', { class: 'map-overlay map-overlay--tl' }, mapLegend), h('div', { class: 'map-overlay map-overlay--bl' }, readout));

  /* ---------- activity strip */
  const ACT = Object.fromEntries(Object.entries(STRIP_KINDS).map(([k, v]) => [k, v.label]));
  const stripPanel = panel({ title: 'What the car did', icon: 'timeline', cover: 'trip.chart.states',
    side: h('span', { class: 'row' }, csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'trip', dayKey(from, tz), 'activity'), actItems.map((a) => ({ ...a, state: ACT[a.kind] || a.kind, minutes: a.end_ms != null ? Math.round((a.end_ms - a.start_ms) / 60000) : null })), [timeColumn('Start', 'start_ms', tz), timeColumn('End', 'end_ms', tz), column('State', 'state'), column('Minutes', 'minutes'), { label: 'Data from', value: (a) => srcText(a) }]) }),
      h('a', { class: 'panel-link', href: state.buildUrl('timeline', { car: cid, period: { from, to: ctx.period?.to_ms ?? null } }) }, 'Timeline', icon('chev-r', 'ico--sm'))) },
    lost.activity ? h('p', { class: 'note warn', role: 'status' }, `What the car did could not be loaded: ${lost.activity.message}`) : activityStrip(actItems, from, to, { tz, now, size: 'lg' }),
    h('p', { class: 'note' }, 'Asleep is hatched blue: the car went quiet. Grey hatching is unknown: no log, a paused recording or a log that failed its check.'));

  /* ---------- battery and elevation */
  const battEl = h('div', { class: 'chart trip-chart--batt' });
  const elevEl = h('div', { class: 'chart trip-chart--elev' });
  const battVal = h('span', { class: 'val' });
  const elevVal = h('span', { class: 'val' });
  const chartTime = h('span', { class: 'chart-time' });
  const seriesCsv = () => downloadCsv(csvFilename(car.name, 'trip', dayKey(from, tz), 'battery-elevation'),
    (series?.t_ms || []).map((t, i) => ({ t, state: series.state?.[i] ?? null, soc: series.values.soc_pct?.[i] ?? null, range: series.values[rk]?.[i] ?? null, elev: series.values.elevation_m?.[i] ?? null })),
    [timeColumn('Time', 't', tz), column('State', 'state'), column('Battery', 'soc', 'percent'), column(rangeLabel(), 'range', 'range'), column('Elevation', 'elev', 'length')]);
  const chartsPanel = h('section', { class: 'panel trip-charts', 'aria-label': 'Battery and elevation', cover: 'trip.control.zoom' },
    h('div', { class: 'panel-head' }, h('h2', { class: 'panel-title' }, icon('battery'), 'Battery and elevation'),
      h('span', { class: 'row', title: series ? srcText(series) || null : null }, steady(chartTime, (series?.t_ms || []).map((v) => `${formatDate(v, tz, { year: false })}, ${formatTime(v, tz)}`)), h('span', { class: 'faint' }, 'Drag across a chart to make that span the trip'), csvButton({ onClick: seriesCsv }))),
    h('div', { class: 'chart-block', dataset: { series: 'batt' }, cover: ['trip.chart.battery.soc', 'trip.chart.battery.range'] },
      h('div', { class: 'chart-label' }, h('span', { class: 'name' }, 'Battery', h('span', { class: 'chart-unit' }, ' · % and ', unitLabel('range'))),
        h('span', { class: 'key' }, h('i', { vars: { '--c': 'var(--c-soc)' } }), 'Battery %'),
        h('span', { class: 'key' }, h('i', { vars: { '--c': 'var(--live)' } }), `${rangeLabel()} (right)`), battVal),
      battEl),
    h('div', { class: 'chart-block', dataset: { series: 'elev' }, cover: 'trip.chart.elevation' },
      h('div', { class: 'chart-label' }, h('span', { class: 'name' }, 'Elevation', h('span', { class: 'chart-unit' }, ` · ${unitLabel('length')}`)),
        h('span', { class: 'key' }, h('i', { vars: { '--c': 'var(--c-elev)' } }), 'Above sea level'), elevVal),
      elevEl),
    h('p', { class: 'note' }, 'Shaded blue: asleep (the values hold). Grey: unknown, no log; the lines break there instead of guessing.'));

  /* ---------- drives and charges */
  const listParams = { from_ms: from, to_ms: to };
  const tripDrives = dataTable(ctx, {
    title: 'Drives', icon: 'route', path: `cars/${enc(cid)}/drives`, params: listParams, limit: 50, csvName: 'trip-drives', cls: 'trip-table',
    cover: 'trip.drives.table', sort: { key: 'start', dir: 'asc' },
    note: 'Every drive that started in the window, oldest first. Consumption is left blank, with the reason, where it would not be reliable.',
    rowTitle: srcTitle,
    rowHref: (d) => state.buildUrl('drive', { car: cid, drive: d.id }),
    columns: [
      { key: 'start', label: 'Start', sort: 'start', firstDir: 'asc', cover: ['trip.drives.date', 'trip.drives.link.details'], csv: (d) => iso(d.start_ms),
        render: (d) => h('span', { class: 'cell-text' }, h('a', { class: 'main', href: state.buildUrl('drive', { car: cid, drive: d.id }), title: 'Open this drive' }, `${formatDate(d.start_ms, tz, { year: false })}, ${formatTime(d.start_ms, tz)}`),
          h('span', { class: 'sub' }, d.flags?.in_progress ? 'driving now' : `to ${formatTime(d.end_ms, tz)}`)) },
      { key: 'from', label: 'From', cls: 'col-place', cover: ['trip.drives.start', 'trip.drives.link.geofence_start'], csv: (d) => placeLabel(d.start), render: (d) => placeCell(d.start, d.start?.lat, d.start?.lon) },
      { key: 'to', label: 'To', cls: 'col-place', cover: ['trip.drives.destination', 'trip.drives.link.geofence_end'], csv: (d) => placeLabel(d.end), render: (d) => (d.flags?.in_progress ? h('span', { class: 'cell-text' }, h('span', { class: 'main faint' }, 'driving now')) : placeCell(d.end, d.end?.lat, d.end?.lon)) },
      { key: 'duration', label: 'Duration', num: true, sort: 'duration', cover: ['trip.drives.duration', 'trip.drives.link.duration_hint'], csv: (d) => (d.duration_ms == null ? null : Math.round(d.duration_ms / 60000)),
        render: (d) => h('span', { class: 'cell-text cell-num', title: `${hm(d.duration_ms)} (hours:minutes), ${formatTime(d.start_ms, tz)} to ${d.end_ms ? formatTime(d.end_ms, tz) : 'now'}` },
          h('span', { class: 'main' }, formatDuration(d.duration_ms))) },
      { key: 'distance', label: 'Distance', num: true, sort: 'distance', cover: 'trip.drives.distance', csv: column('Distance', 'distance_km', 'distance'), render: (d) => format(d.distance_km, 'distance') },
      { key: 'soc', label: 'Battery', num: true, cover: ['trip.drives.soc_start', 'trip.drives.soc_end'], title: `Battery at the start → at the end, with the ${rangeLabel().toLowerCase()} below`,
        render: (d) => socCell(d.start_soc_pct, d.end_soc_pct, d.start_range_km, d.end_range_km) },
      { key: 'wh', label: 'Consumption', num: true, sort: 'wh_per_km', cover: 'trip.drives.consumption', csv: column('Consumption', 'wh_per_km', 'efficiency'), render: consumptionCell },
    ],
    empty: () => emptyState({ icon: 'route', title: 'No drive in this window', text: 'Drives belong to the window by their start. Widen the window, or use Last three drives above.' }),
  });
  const admin = isAdmin();
  const tripCharges = dataTable(ctx, {
    title: 'Charges', icon: 'bolt', path: `cars/${enc(cid)}/charges`, params: listParams, limit: 50, csvName: 'trip-charges', cls: 'trip-table trip-table--charges',
    cover: 'trip.charges.table', sort: { key: 'start', dir: 'asc' },
    note: 'Every charge that started in the window, oldest first.',
    noteMore: `Energy added is measured at the pack; from the grid is measured on AC only (a fast charger’s grid side is not). Hover a row to see where its data came from. ${admin ? 'Click a cost to set or change it.' : 'An admin can set costs.'}`,
    rowTitle: srcTitle,
    rowHref: (c) => state.buildUrl('charge', { car: cid, charge: c.id }),
    columns: [
      { key: 'start', label: 'Start', sort: 'start', firstDir: 'asc', cover: ['trip.charges.date', 'trip.charges.link.details'], csv: (c) => iso(c.start_ms),
        render: (c) => h('span', { class: 'cell-text' }, h('a', { class: 'main', href: state.buildUrl('charge', { car: cid, charge: c.id }), title: 'Open this charge' }, `${formatDate(c.start_ms, tz, { year: false })}, ${formatTime(c.start_ms, tz)}`),
          h('span', { class: 'sub' }, c.end_ms ? `${c.kind === 'dc' ? 'DC' : c.kind === 'ac' ? 'AC' : 'AC or DC unknown'}, to ${formatTime(c.end_ms, tz)}` : 'charging now')) },
      { key: 'place', label: 'Place', cls: 'col-place', cover: ['trip.charges.location', 'trip.charges.link.geofence'], csv: (c) => placeLabel(c), render: (c) => placeCell(c, c.position?.lat, c.position?.lon) },
      { key: 'duration', label: 'Charging', num: true, sort: 'duration', cover: 'trip.charges.duration', csv: (c) => (c.charging_ms == null ? null : Math.round(c.charging_ms / 60000)),
        render: (c) => h('span', { class: 'nowrap', title: `Time actually charging; waiting while plugged in is left out${c.plugged_ms != null ? `. Plugged in ${formatDuration(c.plugged_ms)}` : ''}` }, formatDuration(c.charging_ms)) },
      { key: 'cost', label: 'Cost', num: true, sort: 'cost', cover: ['trip.charges.cost', 'trip.charges.link.cost'], csv: (c) => (c.cost ? `${c.cost.amount} ${c.cost.currency}` : ''), render: (c) => costCell(c, cid, admin) },
      { key: 'added', label: 'Added', num: true, sort: 'energy', cover: ['trip.charges.energy_added', 'trip.charges.range_added'], title: `Energy into the pack, then the ${rangeLabel().toLowerCase()} it added`, csv: column('Energy added', 'energy_added_kwh', 'energy'),
        render: (c) => h('span', { class: 'cell-text cell-num' }, h('span', { class: 'main' }, format(c.energy_added_kwh, 'energy')), c.range_added_km != null ? h('span', { class: 'sub' }, `+${format(c.range_added_km, 'range')}`) : null) },
      { key: 'grid', label: 'From grid', num: true, cover: 'trip.charges.energy_used', title: 'Energy drawn, losses included (AC only)', csv: column('Energy from grid', 'energy_grid_kwh', 'energy'),
        render: (c) => (c.energy_grid_kwh != null ? format(c.energy_grid_kwh, 'energy') : h('span', { class: 'cell-text cell-num', title: c.kind === 'dc' ? 'A fast charger’s grid side is not measured.' : 'No meter reading for this charge.' },
          h('span', { class: 'main faint' }, '—'), h('span', { class: 'sub' }, c.kind === 'dc' ? 'DC: not metered' : 'no reading'))) },
      { key: 'power', label: 'Avg power', num: true, cover: 'trip.charges.power_avg', title: 'While charging, waiting time left out; the peak in each value’s title', csv: column('Average power', 'power_avg_kw', 'power'),
        render: (c) => h('span', { class: 'nowrap', title: c.power_max_kw != null ? `Peak ${format(c.power_max_kw, 'power')}` : null }, format(c.power_avg_kw, 'power')) },
      { key: 'soc', label: 'Battery', num: true, sort: 'end_soc', cover: ['trip.charges.soc_start', 'trip.charges.soc_end'], title: `Battery at the start → at the end, with the ${rangeLabel().toLowerCase()} below (sorts by the end)`,
        render: (c) => socCell(c.start_soc_pct, c.end_soc_pct, c.start_range_km, c.end_range_km) },
    ],
    empty: () => emptyState({ icon: 'bolt', title: 'No charge in this window', text: 'Charges belong to the window by their start. A charge that began before the window is on the trip before it.' }),
  });

  const titleText = match ? match.name : nDrives ? tripTitle(trip, tz) : 'Trip';
  replace(ctx.view, h('div', { class: 'trip-page', cover: 'shots.trip' },
    pageHead({
      eyebrow: `${car.name} · ${match ? 'Saved trip' : 'Trip'}`,
      title: titleText,
      sub: [`${formatDateRange(from, Math.max(from, to - 1), tz, { year: true })}`, format(dist.odometer_km ?? dist.drives_km, 'distance'), `${formatNumber(nDrives)} ${nDrives === 1 ? 'drive' : 'drives'}`, `${formatNumber(nCharges)} ${nCharges === 1 ? 'charge' : 'charges'}`, vsCar ? `beside ${vsCar.name}` : null],
      actions: [h('button', { class: 'btn', type: 'button', onclick: openCarSwitcher }, icon('car'), 'Switch car'),
        h('a', { class: 'btn', href: state.buildUrl('drives', { car: cid, period: { from, to: ctx.period?.to_ms ?? null } }) }, icon('route'), 'These drives')],
    }),
    windowPanel,
    h('div', { class: 'trip-layout' }, mapPanel, figs),
    comparePanel,
    h('div', { class: 'trip-pair' }, timePanel, energyPanel),
    stripPanel,
    chartsPanel,
    tripDrives.el,
    tripCharges.el));
  await Promise.all([tripDrives.ready, tripCharges.ready]);
  if (!ctx.alive()) return;
  // a table wider than its panel (a narrow window) says so above it, instead of hiding columns at the edge
  for (const t of [tripDrives, tripCharges]) {
    const box = t.el.querySelector('.tbl-scroll');
    if (!box || typeof ResizeObserver !== 'function') continue;
    const hint = h('p', { class: 'trip-scroll-hint', hidden: true }, icon('chev-r', 'ico--sm'), 'More columns to the right: scroll the table sideways');
    box.before(hint);
    // measured now, before ready, so the hint is there with the first paint (not a line that pushes the table later)
    const fits = () => { hint.hidden = box.scrollWidth <= box.clientWidth + 1; };
    fits();
    const ro = new ResizeObserver(fits);
    ro.observe(box);
    ctx.onCleanup(() => ro.disconnect());
  }
  if (!ctx.alive()) return;
  ctx.ready();

  // a drive or charge that starts or ends: everything again (only when the window reaches now)
  if (to >= now - 60000) {
    let t = 0;
    ctx.events('rows', (ev) => {
      if (ev.data?.car !== cid && ev.data?.car !== vsId) return;
      if (!(ev.data.kinds || []).some((k) => k === 'drives' || k === 'charges')) return;
      if (t) return;   // (words that keep coming, as in a rebuild, never put it off)
      t = setTimeout(() => { t = 0; if (ctx.alive()) ctx.refresh(); }, 1500);
    });
    ctx.onCleanup(() => clearTimeout(t));
  }

  /* ---------- the map, after the first paint */
  let m = null, hoverMarker = null;
  (async () => {
    const cars = [cid, vsCar && vsTrip ? vsId : null].filter(Boolean).join(','); // a second car that did not load cannot hide the first car's route
    const n = nDrives + (vsTrip?.drives?.length ?? 0);
    let tracks = null, tracksErr = null;
    try { tracks = await ctx.api.get('tracks', { cars, ...win, max_points: clamp(Math.max(12000, n * 6), 12000, 200000) }); } catch (e) { tracks = null; tracksErr = e; }
    if (!ctx.alive()) return;
    const lines = (tracks?.drives || []).map((d) => ({ type: 'Feature', properties: { car: d.car === cid ? 'a' : 'b', drive: d.drive },
      geometry: { type: 'LineString', coordinates: d.lon.map((lo, i) => [lo, d.lat[i]]).filter((p) => p[0] != null && p[1] != null) } })).filter((f) => f.geometry.coordinates.length > 1);
    const marks = (trip.markers || []).filter((k) => k.lat != null && k.lon != null);
    const all = [...lines.flatMap((f) => f.geometry.coordinates), ...marks.map((k) => [k.lon, k.lat])];
    // framed without stray fixes (a jump to another state and back)
    const fit = trackBounds(tracks?.drives || [], marks.map((k) => [k.lon, k.lat])) || boundsOf(all);
    if (!all.length) { mapEl.append(mapFallback(tracksErr ? `The route could not be loaded: ${tracksErr.message}` : nDrives ? 'No positions were recorded for these drives.' : 'Nothing was driven in this window, so there is no route.')); return; }
    try {
      m = await createMap(mapEl, ctx, { bounds: fit });
      if (!m) return;
      await m.ready;
      if (!ctx.alive()) return;
      m.map.addSource('trip-routes', { type: 'geojson', data: { type: 'FeatureCollection', features: lines } });
      const line = { type: 'line', source: 'trip-routes', layout: { 'line-cap': 'round', 'line-join': 'round' } };
      const carColour = ['match', ['get', 'car'], 'b', cssVar('--c-soc'), cssVar('--live')];
      m.addLayer({ ...line, id: 'trip-glow', paint: { 'line-color': carColour, 'line-opacity': 0.2 } }, { 'line-width': () => rpx(12), 'line-blur': () => rpx(6) });
      m.addLayer({ ...line, id: 'trip-casing', paint: { 'line-color': cssVar('--map-casing'), 'line-opacity': 0.85 } }, { 'line-width': () => rpx(7) });
      m.addLayer({ ...line, id: 'trip-line', paint: { 'line-color': carColour } }, { 'line-width': () => rpx(3.6) });
      m.map.addSource('trip-marks', { type: 'geojson', data: { type: 'FeatureCollection', features: marks.filter((k) => k.kind === 'charge' || k.kind === 'overnight').map((k) => ({ type: 'Feature', properties: { kind: k.kind, charge: k.charge || '', t: k.t_ms }, geometry: { type: 'Point', coordinates: [k.lon, k.lat] } })) } });
      m.addLayer({ id: 'trip-marks', type: 'circle', source: 'trip-marks', paint: { 'circle-color': ['match', ['get', 'kind'], 'charge', cssVar('--warm'), cssVar('--st-asleep')], 'circle-stroke-color': cssVar('--map-casing') } },
        { 'circle-radius': () => rpx(6), 'circle-stroke-width': () => rpx(2) });
      m.map.on('click', 'trip-marks', (e) => { const f = e.features?.[0]; if (f?.properties?.charge) ctx.navigate(state.buildUrl('charge', { car: cid, charge: f.properties.charge })); });
      m.map.on('mouseenter', 'trip-marks', () => { m.map.getCanvas().style.cursor = 'pointer'; });
      m.map.on('mouseleave', 'trip-marks', () => { m.map.getCanvas().style.cursor = ''; });
      const s = marks.find((k) => k.kind === 'start'), e = [...marks].reverse().find((k) => k.kind === 'end');
      if (s) routePin(m, [s.lon, s.lat], 'A');
      if (e) routePin(m, [e.lon, e.lat], 'B');
      m.frame(() => {
        const pad = padClear(mapEl, mapLegend, { left: 40, top: 24, right: 60, bottom: 56, beside: true });
        // the route and its End pin stay above the Map / Satellite switch and the credit, at every Interface size
        const foot = mapEl.querySelector('.maplibregl-ctrl-bottom-right')?.offsetHeight || 0;
        pad.bottom = Math.min(Math.max(pad.bottom, foot + rpx(16)), Math.max(0, mapEl.clientHeight * 0.8 - pad.top));
        m.map.fitBounds(fit, { padding: pad, maxZoom: 15, duration: 0 });
      });
    } catch (err) { console.warn(err); replace(mapEl, mapFallback()); }
  })();
  function hoverAt(i) {
    const lat = series?.values.lat?.[i], lon = series?.values.lon?.[i];
    if (i == null || lat == null || lon == null || !m) { readout.hidden = true; hoverMarker?.remove(); hoverMarker = null; return; }
    if (!hoverMarker) hoverMarker = new m.ml.Marker({ element: h('div', { class: 'car-marker car-marker--live trip-hover', 'aria-hidden': 'true' }) }).setLngLat([lon, lat]).addTo(m.map);
    else hoverMarker.setLngLat([lon, lat]);
    readout.hidden = false;
    replace(readout, h('b', null, `${formatDate(series.t_ms[i], tz, { year: false })}, ${formatTime(series.t_ms[i], tz)}`), h('span', null, `Battery ${pct(series.values.soc_pct?.[i])}`));
  }

  /* ---------- the charts */
  if (!series || !series.t_ms?.length) {
    battEl.append(h('p', { class: 'chart-empty' }, lost.series ? `The battery readings could not be loaded: ${lost.series.message}` : 'No battery readings in this window.'));
    elevEl.append(h('p', { class: 'chart-empty' }, lost.series ? 'The elevation readings could not be loaded.' : 'No elevation readings in this window.'));
    return;
  }
  await loadUPlot();
  if (!ctx.alive()) return;
  const xs = series.t_ms.map((t) => t / 1000);
  // asleep is not unknown: the car does not move and the battery holds while it sleeps, so lines go on
  // through asleep buckets; they break only where nothing is known
  const carry = (a) => { let last = null; return (a || []).map((v, i) => { if (v != null) { last = v; return v; } return series.state?.[i] === 'asleep' ? last : (last = null); }); };
  const soc = carry(series.values.soc_pct);
  const range = carry(series.values[rk]).map((v) => (v == null ? null : convert(v, 'range')));
  const elev = carry(series.values.elevation_m).map((v) => (v == null ? null : convert(v, 'length')));
  const known = (a) => a.some((v) => v != null);
  if (!known(elev)) elevEl.append(h('p', { class: 'chart-empty' }, 'This car sent no elevation in this window.'));
  const bands = (u) => {
    const c = u.ctx, { top, height } = u.bbox;
    const asleep = cssVar('--st-asleep'), unknown = cssVar('--st-unknown');
    c.save();
    for (let i = 0; i < xs.length; i++) {
      const st = series.state?.[i];
      if (st !== 'asleep' && st !== 'unknown') continue;
      const x0 = u.valToPos(xs[i], 'x', true), x1 = u.valToPos(xs[i + 1] ?? xs[i] + (series.bucket_ms || 60000) / 1000, 'x', true);
      c.fillStyle = hexA(st === 'asleep' ? asleep : unknown, st === 'asleep' ? 0.13 : 0.22);
      c.fillRect(x0, top, Math.max(1, x1 - x0), height);
    }
    c.restore();
  };
  const sync = `trip-${cid}`;
  const idle = () => {
    const mm = (a) => { const f = a.filter((v) => v != null); return f.length ? [Math.min(...f), Math.max(...f)] : null; };
    const s = mm(soc), e = mm(elev);
    battVal.textContent = s ? `${pct(s[0])} – ${pct(s[1])}` : 'no data';
    elevVal.textContent = e ? `${formatNumber(e[0], { maximumFractionDigits: 0 })} – ${formatNumber(e[1], { maximumFractionDigits: 0 })} ${unitLabel('length')}` : 'no data';
    chartTime.textContent = '';
  };
  idle();
  const onCursor = (i) => {
    if (i == null) { idle(); hoverAt(null); return; }
    chartTime.textContent = `${formatDate(series.t_ms[i], tz, { year: false })}, ${formatTime(series.t_ms[i], tz)}`;
    battVal.textContent = `${pct(series.values.soc_pct?.[i])} · ${format(series.values[rk]?.[i], 'range')}${series.state?.[i] === 'asleep' ? ' (asleep)' : series.state?.[i] === 'unknown' ? ' (unknown)' : ''}`;
    elevVal.textContent = format(series.values.elevation_m?.[i], 'length');
    hoverAt(i);
  };
  let zoomed = false;
  const zoomTo = (a, b) => {
    // both charts share one selection, so this runs once per chart: only the first one navigates
    if (b - a < 60 || zoomed) return;
    zoomed = true;
    // the wider window goes into this entry's URL first (a page without from/to would fall back to the
    // tab's stored period, which by then is the narrow one), then one new entry for the narrow window
    if (!ctx.state.periodFromUrl) {
      const p = ctx.period;
      const wide = ctx.state.period || (p?.preset && p.preset !== 'custom' ? { preset: p.preset } : { from, to: p?.to_ms ?? null });
      try { globalThis.history?.replaceState(null, '', periodLink(wide)); } catch { /* no history: fine */ }
    }
    ctx.navigate(periodLink({ from: Math.round(a * 1000), to: Math.round(b * 1000) }));
  };
  const make = (kind) => (width, height) => {
    const c = colors();
    const batt = kind === 'batt';
    return {
      width, height, padding: [rpx(10), rpx(batt ? 4 : 14), 0, 0], legend: { show: false },
      cursor: { sync: { key: sync, setSeries: false }, drag: { x: true, y: false, setScale: false }, points: { size: rpx(8), width: rpx(2), stroke: c.cursor } },
      select: { show: true },
      scales: batt ? { x: { time: true, min: from / 1000, max: to / 1000 }, pct: { range: [0, 100] }, dist: { auto: true, range: (u, a, b) => [0, Math.max(10, (b || 0) * 1.1)] } }
        : { x: { time: true, min: from / 1000, max: to / 1000 }, y: { auto: true } },
      axes: batt ? [timeAxis(tz, { show: false, spanMs: span }), { ...valueAxis({ size: 48 }), scale: 'pct' }, { ...valueAxis({ size: 52 }), scale: 'dist', side: 1, grid: { show: false } }]
        : [timeAxis(tz, { spanMs: span }), valueAxis({ size: 48 })],
      series: batt ? [{},
        { label: 'Battery', scale: 'pct', stroke: cssVar('--c-soc'), width: rpx(2), fill: areaFill(cssVar('--c-soc'), 0.2, 0.01), points: { show: false } },
        { label: 'Range', scale: 'dist', stroke: c.live, width: rpx(1.5), points: { show: false } }]
        : [{}, { label: 'Elevation', stroke: cssVar('--c-elev'), width: rpx(2), fill: areaFill(cssVar('--c-elev'), 0.22, 0.01), points: { show: false } }],
      hooks: {
        drawClear: [bands],
        setSelect: [(u) => {
          if (u.select.width < rpx(6)) return;
          const a = u.posToVal(u.select.left, 'x'), b = u.posToVal(u.select.left + u.select.width, 'x');
          u.setSelect({ left: 0, top: 0, width: 0, height: 0 }, false);
          zoomTo(a, b);
        }],
        setCursor: batt ? [(u) => onCursor(u.cursor.idx == null || u.cursor.left < 0 ? null : u.cursor.idx)] : [],
      },
    };
  };
  mountPlot(battEl, make('batt'), [xs, soc, range], ctx, null, 'Battery level and range along the trip');
  mountPlot(elevEl, make('elev'), [xs, elev], ctx, null, 'Height above sea level along the trip');
}

/* ------------------------------------------------------------------ helpers */

async function collect(iter, max) {
  const out = [];
  for await (const x of iter) { out.push(x); if (out.length >= max) break; }
  return out;
}

const hm = (ms) => {
  if (ms == null) return '—';
  const m = Math.round(ms / 60000);
  return `${Math.floor(m / 60)}:${String(m % 60).padStart(2, '0')}`;
};

// a title from the places: the first start to the farthest-looking end
function tripTitle(trip, tz) {
  const days = trip.from_ms != null ? formatDateRange(trip.from_ms, Math.max(trip.from_ms, (trip.to_ms ?? trip.from_ms) - 1), tz, { year: false }) : '';
  return days ? `Trip ${days}` : 'Trip';
}

function placeCell(p, lat, lon) {
  const href = pointHref(p, lat, lon);
  const name = placeLabel(p);
  const saved = !!p?.place?.id;
  return h('span', { class: 'cell-text' },
    href ? h('a', { class: 'main place-link', href, title: saved ? `Edit the place ${name}` : `${name}: save this spot as a place` }, name) : h('span', { class: 'main' }, name),
    saved && p.address?.short && p.address.short !== name ? h('span', { class: 'sub' }, p.address.short) : null);
}

// the battery at the start and the end in one cell: "63% → 61%" over "322 → 315 km"
function socCell(a, b, ra, rb) {
  const rng = ra != null || rb != null ? `${ra != null ? formatNumber(convert(ra, 'range'), { maximumFractionDigits: 0 }) : '—'} → ${format(rb, 'range')}` : '';
  return h('span', { class: 'cell-text cell-num', title: `Start ${pct(a)}${ra != null ? ` (${format(ra, 'range')})` : ''}, end ${pct(b)}${rb != null ? ` (${format(rb, 'range')})` : ''}` },
    h('span', { class: 'main' }, `${pct(a)} → ${pct(b)}`), h('span', { class: 'sub' }, rng));
}

function consumptionCell(d) {
  let why = null;
  if (d.flags?.in_progress) why = 'still driving';
  else if (d.wh_per_km == null) why = 'not measured';
  else if ((d.distance_km ?? 0) < 1) why = 'too short';
  else if (d.flags?.gap_ms > 60000) why = `${formatDuration(d.flags.gap_ms)} without data`;
  if (why) return h('span', { class: 'cell-text cell-num', title: why === 'too short' ? 'Too short to be reliable' : null }, h('span', { class: 'main faint' }, '—'), h('span', { class: 'sub' }, why));
  const est = d.energy_basis === 'estimated';
  return h('span', { class: 'cell-text cell-num', title: est ? 'Estimated: from the Tesla API\'s power readings (or an import), not measured at the pack' : 'Energy measured at the pack' }, h('span', { class: 'main' }, format(d.wh_per_km, 'efficiency')),
    h('span', { class: 'sub' }, `${est ? '≈ ' : ''}${format(d.energy_kwh, 'energy')}`));
}

function costCell(c, cid, admin) {
  const edit = state.buildUrl('charge', { car: cid, charge: c.id, query: { edit: 'cost' } });
  // unpriced is a quiet "—"; the edit pen shows on hover or focus only; where the cost came from is the title
  const txt = c.cost ? formatMoney(c.cost.amount, c.cost.currency) : '—';
  const why = c.cost ? `Cost: ${COST_SOURCE[c.cost.source] || c.cost.source}` : c.place ? 'No price: the place has no price' : 'No price: not a saved place';
  return h('span', { class: 'cell-text cell-num', title: why },
    admin ? h('a', { class: ['main', 'cost-link', !c.cost && 'is-missing'], href: edit, title: c.cost ? 'Change the cost of this charge' : 'Set a cost for this charge', 'aria-label': c.cost ? `${txt}, change the cost` : 'No price, set a cost' }, txt, icon('settings', 'ico--sm cost-pen'))
      : h('span', { class: ['main', !c.cost && 'faint'] }, txt));
}

// the figures as rows, for the CSV and the side-by-side table
const FIGS = [
  ['Distance by odometer', (t) => t.distance?.odometer_km, 'distance'],
  ['Distance of the drives', (t) => t.distance?.drives_km, 'distance'],
  ['Drives', (t) => t.drives?.length ?? null, 'count'],
  ['Charges', (t) => t.charges?.length ?? null, 'count'],
  ['Driving', (t) => t.time_ms?.driving, 'duration'],
  ['Charging AC', (t) => t.time_ms?.charging_ac, 'duration'],
  ['Charging DC', (t) => t.time_ms?.charging_dc, 'duration'],
  ['Charging (kind not known)', (t) => t.time_ms?.charging_unknown, 'duration'],
  ['Parked, awake', (t) => t.time_ms?.parked_awake, 'duration'],
  ['Asleep', (t) => t.time_ms?.asleep, 'duration'],
  ['Not recorded', (t) => t.time_ms?.not_recorded, 'duration'],
  ['Average speed, moving', (t) => t.speed_kph?.moving, 'speed'],
  ['Average speed, with fast charging', (t) => t.speed_kph?.incl_dc, 'speed'],
  ['Energy used driving', (t) => t.energy_kwh?.drive_net, 'energy'],
  ['Energy used in all', (t) => t.energy_kwh?.gross, 'energy'],
  ['Energy added AC', (t) => t.energy_kwh?.added_ac, 'energy'],
  ['Energy added DC', (t) => t.energy_kwh?.added_dc, 'energy'],
  ['Energy added (kind not known)', (t) => t.energy_kwh?.added_unknown, 'energy'],
  ['Net consumption', (t) => t.wh_per_km?.net, 'efficiency'],
  ['Gross consumption', (t) => t.wh_per_km?.gross, 'efficiency'],
  ['Charging cost', (t) => t.cost?.by_currency || [], 'money'],
  ['Cost per distance', (t) => t.cost?.per_100km || [], 'money_per_distance'],
];
const FIG_COLS = [column('Car', 'car'), column('Figure', 'figure'), column('Value', 'value'), column('Unit', 'unit')];
function figText(v, kind) {
  if (kind === 'money' || kind === 'money_per_distance') return v.length ? v.map((x) => (kind === 'money' ? formatMoney(x.amount, x.currency) : format({ amount: x.amount, currency: x.currency }, kind))).join(' + ') : '—';
  if (kind === 'duration') return v == null ? '—' : formatDuration(v);
  if (kind === 'count') return v == null ? '—' : formatNumber(v);
  return format(v, kind);
}
function figureRows(t, car) {
  return FIGS.map(([label, get, kind]) => {
    const v = get(t);
    if (kind === 'money' || kind === 'money_per_distance') return { car: car.name, figure: label, value: v.map((x) => `${kind === 'money' ? x.amount : Number(convert(x.amount, kind).toFixed(2))} ${x.currency}`).join(' + '), unit: kind === 'money' ? 'currency' : unitLabel('money_per_distance') };
    if (kind === 'duration') return { car: car.name, figure: label, value: v == null ? null : Math.round(v / 60000), unit: 'min' };
    return { car: car.name, figure: label, value: v == null ? null : kind === 'count' ? v : Number(convert(v, kind).toFixed(kind === 'energy' ? 3 : 1)), unit: kind === 'count' ? '' : unitLabel(kind) };
  });
}

function compareBlock(ctx, car, trip, vsId, vsCar, vsTrip, win, tz) {
  const name = vsCar?.name || 'Second car';
  const clear = h('a', { class: 'btn btn--sm', href: state.buildUrl('trip', { car: car.id, period: ctx.state.period, query: null }) }, icon('x', 'ico--sm'), 'Stop comparing');
  if (!vsCar || !vsTrip) {
    return panel({ title: 'Side by side', icon: 'car', cls: 'trip-compare', side: clear },
      h('p', { class: 'note warn' }, 'The second car could not be loaded: it may be hidden from this account or removed. Pick another car above.'));
  }
  const otherTz = vsCar.time_zone && vsCar.time_zone !== tz;
  const rows = FIGS.map(([label, get, kind]) => ({ label, kind, a: get(trip), b: get(vsTrip) }));
  const better = (r) => {
    if (!['efficiency'].includes(r.kind) || r.a == null || r.b == null || r.a === r.b) return [null, null];
    return r.a < r.b ? ['is-better', null] : [null, 'is-better'];
  };
  return panel({ title: 'Side by side', icon: 'car', cls: 'trip-compare',
    side: h('span', { class: 'row' }, csvButton({ onClick: () => downloadCsv(csvFilename(car.name, name, 'trip-compare'), [...figureRows(trip, car), ...figureRows(vsTrip, vsCar)], FIG_COLS) }), clear) },
    h('div', { class: 'tbl-scroll' }, h('table', { class: 'tbl trip-compare-tbl', 'aria-label': `${car.name} and ${name} over the same window` },
      h('thead', null, h('tr', null, h('th', { scope: 'col' }, 'Figure'), h('th', { scope: 'col', class: 'is-num' }, car.name),
        h('th', { scope: 'col', class: 'is-num' }, h('a', { href: state.buildUrl('trip', { car: vsId, period: ctx.state.period, query: { vs: car.id } }), title: `Open the trip page of ${name}` }, name)))),
      h('tbody', null, rows.map((r) => { const [ca, cb] = better(r); return h('tr', null, h('th', { scope: 'row' }, r.label), h('td', { class: ['is-num', ca] }, figText(r.a, r.kind)), h('td', { class: ['is-num', cb] }, figText(r.b, r.kind))); })))),
    h('p', { class: 'note' }, `The same window for both cars${otherTz ? `, in ${car.name}’s time zone (${name} keeps its own days elsewhere)` : ''}. The lower consumption is marked. The second car’s routes are on the map in its own colour.`));
}
