// Charges (/cars/{car}/charges, or /charges?cars=all for every car): the charging log and the charging
// habits. One control deck drives the session list (GET .../charges, sorted and paged by the server, with
// its totals as trip counters) and the charges with missing data. The statistics below (GET
// .../charging-stats, /charging-stats/places, the per-car /dc-curve) follow the period, the cars, the places
// and the shortest charge: costs and totals, the AC and DC split, the habits (a battery-level heat grid and
// the start-to-end bar of every session on one shared cursor, the level bands, the week grid, back-to-back
// runs), where the energy came from (a map and two lists) and the fast-charging curve.
// AC or DC comes from the charge port's own signal; a session without one is Unknown, never DC. DC
// efficiency is unknown (the grid side of a fast charger is not measured). Money is per currency and never
// added across currencies. "rows" events load everything again.

import * as state from '../shared/state.js';
import { h, replace, cssVar } from '../shared/dom.js';
import { column } from '../shared/csv.js';
import { format, formatMoney, unitLabel } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, buckets, bucketKey } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon, hud, plate, btn, seg, tag, note } from '../core/ui.js';
import { controls, slot, periodBtn, select, search, counters, csvBtn, band, plural, eq, placeLabel, placeHref, sourceTag } from '../core/kit.js';
import { tracks } from '../core/tracks.js';
import { loadUPlot, mountPlot, timeAxis, valueAxis, tone, alpha, well } from '../core/chart.js';
import { createMap, mapFallback, boundsOf, pad } from '../core/map.js';
import { rpx } from '../core/display.js';
import { emptyState } from '../core/states.js';
import { popover, toast } from '../core/layer.js';
import { openPeriod } from '../core/period.js';
import { isAdmin } from '../core/prefs.js';

export const id = 'charges';
export const css = true;

const enc = encodeURIComponent;
const KIND = { ac: 'AC', dc: 'DC', unknown: 'Unknown' };
const KIND_HELP = { ac: 'AC: the car\'s own charger turned mains power into DC; grid energy is measured.', dc: 'DC: a fast charger fed the pack directly; its grid side is not measured.', unknown: 'Unknown: the charge port did not say AC or DC (often imported history). Never counted as DC.' };
const KIND_TONE = { ac: 'grn', dc: 'org', unknown: null };
const COST_SOURCE = { tariff: 'place price', manual: 'typed by hand', free_supercharging: 'free Supercharging', teslamate: 'imported' };
const PLACE_KINDS = [['', 'Any kind'], ['home', 'Home'], ['work', 'Work'], ['charger', 'Public charger'], ['service', 'Service'], ['other', 'Other'], ['none', 'Not a saved place']];
const DAYS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
const iso = (ms) => (ms == null ? '' : new Date(ms).toISOString());
const pct = (v) => format(v, 'percent');
const moneyList = (list) => (list || []).filter((x) => x.amount != null);
const moneyText = (list) => (moneyList(list).length ? moneyList(list).map((x) => formatMoney(x.amount, x.currency)).join(' + ') : '—');
const priceText = (list) => ((list || []).filter((x) => x.value != null).length ? list.filter((x) => x.value != null).map((x) => format({ amount: x.value, currency: x.currency }, 'price_energy')).join(' + ') : '—');
const where = (c) => ({ place: c.place, label: c.label, address: c.address?.short, lat: c.position?.lat ?? c.lat, lon: c.position?.lon ?? c.lon });

export async function mount(ctx) {
  const { tz, period } = ctx;
  const fixed = ctx.car;
  const q = ctx.query;
  const now = () => ctx.api.now();

  /* ---------- cars and places */
  const [fleet, places] = await Promise.all([
    ctx.api.page('cars', { sort: 'name' }, { limit: 200 }).then((p) => p.items || []).catch(() => (fixed ? [fixed] : [])),
    ctx.api.page('places', { sort: 'visits' }, { limit: 200 }).then((p) => p.items || []).catch(() => []),
  ]);
  if (!ctx.alive()) return;
  const carById = new Map(fleet.map((c) => [c.id, c]));
  if (fixed) carById.set(fixed.id, fixed);
  const viewCars = fixed ? [fixed.id] : fleet.map((c) => c.id);
  const multi = !fixed;
  const carName = (cid) => carById.get(cid)?.name || cid;
  const scopeName = fixed ? fixed.name : `All ${plural(fleet.length, 'car')}`;
  const listPath = fixed ? `cars/${enc(fixed.id)}/charges` : 'charges';
  const statsPath = fixed ? `cars/${enc(fixed.id)}/charging-stats` : 'charging-stats';
  const carsParam = fixed ? null : 'all';
  const zoneOf = (c) => c.time_zone || carById.get(c.car)?.time_zone || tz;

  /* ---------- filters (kept in the address) */
  const f = { places: q.places || '', place_kind: q.place_kind || '', kind: q.kind || '', q: q.q || '', min_cost: q.min_cost || '', unpriced: q.unpriced === '1', min_dur: q.min_dur || '', source: q.source || '', flags: q.flags || '' };
  const base = () => ({ ...ctx.params, cars: carsParam });
  const listParams = () => ({ ...base(), places: f.places || null, place_kind: f.place_kind || null, kind: f.kind || null, q: f.q || null, min_cost: f.min_cost !== '' ? f.min_cost : null,
    include_unpriced: f.min_cost !== '' && f.unpriced ? 'true' : null, min_duration_min: f.min_dur || null, source: f.source || null, flags: f.flags || null });
  const statsParams = () => ({ ...base(), places: f.places || null, min_duration_min: f.min_dur || null });
  const queryOf = () => ({ places: f.places || null, place_kind: f.place_kind || null, kind: f.kind || null, q: f.q || null, min_cost: f.min_cost || null, unpriced: f.min_cost && f.unpriced ? '1' : null, min_dur: f.min_dur || null, source: f.source || null, flags: f.flags || null });
  let tmr = 0;
  const set = (patch, wait = 0, stats = true) => {
    clearTimeout(tmr);
    tmr = setTimeout(() => {
      Object.assign(f, patch);
      ctx.setQuery(queryOf());
      list.reload(listParams());
      if (stats) loadStats();
    }, wait);
  };
  ctx.onCleanup(() => clearTimeout(tmr));

  const carSel = select([['', `Every car (${fleet.length})`], ...fleet.map((c) => [c.id, c.name])], fixed ? fixed.id : '', (v) => {
    const p = state.current().period;
    ctx.navigate(v ? state.buildUrl('charges', { car: v, period: p, query: queryOf() }) : state.buildUrl('charges', { period: p, query: { ...queryOf(), cars: 'all' } }));
  }, { label: 'Cars' });
  const chosen = new Set(f.places ? f.places.split(',') : []);
  const placeText = h('span', { class: 'ell' });
  const paintPlaceText = () => { const n = places.filter((p) => chosen.has(p.id)).map((p) => p.name); placeText.textContent = n.length ? (n.length > 2 ? plural(n.length, 'place') : n.join(', ')) : 'Any place'; };
  paintPlaceText();
  const placePop = popover({ cls: 'cs-pop', label: 'Saved places', fill: (el) => replace(el, h('div', { class: 'cs-pop-in' },
    note('Keep only charges at the ticked places. This also narrows the statistics below.'),
    places.length ? h('div', { class: 'cs-pop-list' }, places.map((p) => h('label', { class: 'check' },
      h('input', { type: 'checkbox', checked: chosen.has(p.id), onchange: (e) => { if (e.target.checked) chosen.add(p.id); else chosen.delete(p.id); paintPlaceText(); set({ places: [...chosen].join(',') }, 300); } }),
      h('span', null, p.name, p.counts?.charges != null ? h('span', { class: 'faint' }, ` · ${plural(p.counts.charges, 'charge')}`) : null))))
      : note('There is no saved place yet.'),
    h('div', { class: 'row' }, btn('Any place', () => { chosen.clear(); paintPlaceText(); set({ places: '' }); placePop.hide(); }, { sm: true, kind: 'ghost' }), btn('Done', () => placePop.hide(), { sm: true })))) });
  ctx.onCleanup(() => { placePop.hide(); placePop.el.remove(); });
  const placeBtn = h('button', { class: 'btn ctl-btn', type: 'button', 'aria-haspopup': 'dialog', 'aria-expanded': 'false', onclick: (e) => placePop.toggle(e.currentTarget) }, icon('pin', 'i--sm'), placeText, icon('chev-d', 'i--sm'));
  const kindSeg = seg([['', 'AC and DC'], ['ac', 'AC'], ['dc', 'DC'], ['unknown', 'Unknown']], f.kind, (v) => set({ kind: v }, 0, false), { label: 'AC or DC' });
  const numIn = (value, label, step, onv) => { const i = h('input', { class: 'input ctl-num', type: 'number', min: '0', step, value, 'aria-label': label }); i.addEventListener('input', () => onv(i.value)); return i; };
  const unpricedIn = h('input', { type: 'checkbox', checked: f.unpriced, onchange: (e) => set({ unpriced: e.target.checked }, 0, false) });
  const deck = controls({ note: 'Charges belong to the period they started in. Charging time leaves out time plugged in but not charging. The statistics further down follow the period, cars, places and shortest charge.' },
    slot('Period', periodBtn(ctx, ['charges.filter.time_range', 'cs.filter.time_range'])),
    slot('Cars', carSel, { cover: ['charges.filter.car', 'cs.filter.car'] }),
    slot('Places', placeBtn, { cover: ['charges.filter.geofence', 'cs.filter.geofence'] }),
    slot('Kind of place', select(PLACE_KINDS, f.place_kind, (v) => set({ place_kind: v }, 0, false), { label: 'Kind of place' })),
    slot('AC or DC', kindSeg.el, { cover: 'charges.filter.charge_type' }),
    slot('Text', search(f.q, (v) => set({ q: v }, 0, false), { placeholder: 'Place or address', ctx }), { wide: true, cover: 'charges.filter.location' }),
    slot('Cost at least', h('span', { class: 'ctl-in' }, numIn(f.min_cost, 'Lowest cost, in its currency', '0.01', (v) => set({ min_cost: v }, 350, false)), h('label', { class: 'check cs-check' }, unpricedIn, h('span', null, 'and unpriced'))), { cover: 'charges.filter.min_cost' }),
    slot('Charging at least (min)', numIn(f.min_dur, 'Shortest charging time in minutes', '1', (v) => set({ min_dur: v }, 350)), { cover: ['charges.filter.min_duration', 'cs.filter.min_duration'] }),
    slot('Source', select([['', 'Every source'], ['can', 'CAN Bridge'], ['teslamate', 'Imported']], f.source, (v) => set({ source: v }, 0, false), { label: 'Source' })),
    slot('Missing data', select([['', 'Every charge'], ['gap', 'With missing data'], ['in_progress', 'Under way'], ['gap,in_progress', 'Missing or under way']], f.flags, (v) => set({ flags: v }, 0, false), { label: 'Missing data' })));

  /* ---------- the totals of the list */
  const sumEl = h('div');
  const paintSummary = (t) => {
    t = t || {};
    const n = t.count ?? 0;
    const priced = n - (t.unpriced ?? 0);
    replace(sumEl, counters([
      { label: 'Sessions', value: n, kind: 'count', how: n ? `${formatNumber(priced)} priced` : 'none match' },
      { label: 'Energy added', value: n ? t.energy_added_kwh : null, kind: 'energy', cover: 'charges.stat.energy_added', how: 'into the pack' },
      { label: 'From the grid', value: n && t.energy_grid_known ? t.energy_grid_kwh : null, kind: 'energy', cover: 'charges.stat.energy_used', how: n ? `AC, measured on ${plural(t.energy_grid_known || 0, 'session')}` : 'AC, measured' },
      { label: 'Cost', text: n ? moneyText(t.cost_by_currency) : '—', cover: 'charges.stat.cost', how: 'per currency, never added across' },
      { label: 'Average charging', text: n && t.avg_duration_ms != null ? formatDuration(t.avg_duration_ms) : '—', cover: 'charges.stat.avg_duration', how: 'time charging, not waiting' },
    ]));
  };

  /* ---------- the session list */
  const placeCell = (c) => { const p = where(c); const href = placeHref(p); return h('span', { class: 'cell' }, href ? h('a', { class: 'cell-main plc', href }, placeLabel(p)) : h('span', { class: 'cell-main' }, placeLabel(p)), h('span', { class: 'cell-sub' }, c.place ? c.address?.short || '' : 'not a saved place yet')); };
  const columns = [
    multi ? { key: 'car', label: 'Car', render: (c) => carName(c.car), csv: (c) => carName(c.car) } : null,
    { key: 'start', label: 'Start', sort: 'start', firstDir: 'desc', lead: true, cover: ['charges.table.date', 'charges.link.view_details'], render: (c, i) => h('span', { class: 'cell' },
      h('a', { class: 'cell-main', href: state.buildUrl('charge', { car: c.car, charge: c.id }) }, h('span', { class: 'trk-no' }, `${String(i + 1).padStart(2, '0')} `), `${formatDate(c.start_ms, zoneOf(c), { year: false })}, ${formatTime(c.start_ms, zoneOf(c))}`),
      h('span', { class: 'cell-sub' }, c.end_ms ? `to ${formatTime(c.end_ms, zoneOf(c))}` : 'charging now')), csv: (c) => iso(c.start_ms) },
    { key: 'place', label: 'Place', cls: 'is-wide', cover: ['charges.table.location', 'charges.link.geofence'], render: placeCell, csv: (c) => placeLabel(where(c)) },
    { key: 'kind', label: 'Type', cover: 'charges.table.type', render: (c) => h('span', { class: 'cell' }, h('span', { class: ['tag', KIND_TONE[c.kind || 'unknown'] && `tag--${KIND_TONE[c.kind || 'unknown']}`], title: KIND_HELP[c.kind || 'unknown'] }, KIND[c.kind] || 'Unknown'),
      c.fast_charger ? h('span', { class: 'cell-sub' }, [c.fast_charger.type === 'supercharger' ? 'Supercharger' : c.fast_charger.brand || 'Fast charger', c.fast_charger.class].filter(Boolean).join(' ')) : null), csv: (c) => KIND[c.kind] || 'Unknown' },
    { key: 'duration', label: 'Charging', num: true, sort: 'duration', cover: 'charges.table.duration', render: (c) => h('span', { class: 'cell' }, formatDuration(c.charging_ms), c.plugged_ms != null ? h('span', { class: 'cell-sub' }, `plugged ${formatDuration(c.plugged_ms)}`) : null), csv: (c) => (c.charging_ms == null ? '' : Math.round(c.charging_ms / 60000)) },
    { key: 'added', label: 'Added', num: true, sort: 'energy', cover: 'charges.table.energy_added', render: (c) => format(c.energy_added_kwh, 'energy'), csv: column('Energy added', 'energy_added_kwh', 'energy') },
    { key: 'grid', label: 'From grid', num: true, cover: 'charges.table.energy_used', render: (c) => (c.energy_grid_kwh != null ? format(c.energy_grid_kwh, 'energy') : c.dc_inlet_kwh != null ? h('span', { class: 'cell' }, h('span', { class: 'faint' }, 'not measured'), h('span', { class: 'cell-sub' }, `${format(c.dc_inlet_kwh, 'energy')} at the inlet`)) : '—'), csv: column('Energy from grid', 'energy_grid_kwh', 'energy') },
    { key: 'eff', label: 'Efficiency', num: true, cover: 'charges.table.efficiency', render: (c) => (c.efficiency_pct != null ? h('span', { class: 'cs-eff', title: `Lost ${format(c.losses_kwh, 'energy')}` }, h('span', null, pct(c.efficiency_pct)), h('span', { class: 'cs-eff-bar', 'aria-hidden': 'true' }, h('i', { vars: { '--w': `${Math.max(4, Math.min(100, c.efficiency_pct)).toFixed(1)}%` } }))) : h('span', { class: 'faint' }, c.kind === 'dc' ? 'unknown (DC)' : '—')), csv: (c) => c.efficiency_pct },
    { key: 'cost', label: 'Cost', num: true, sort: 'cost', cover: ['charges.table.cost', 'charges.link.set_cost'], render: (c) => {
      const txt = c.cost ? formatMoney(c.cost.amount, c.cost.currency) : 'No price';
      return h('span', { class: 'cell' }, isAdmin() ? h('a', { class: ['plc', !c.cost && 'cs-none'], href: state.buildUrl('charge', { car: c.car, charge: c.id, query: { edit: 'cost' } }), title: c.cost ? 'Change the cost' : 'Set a cost' }, txt) : h('span', { class: !c.cost ? 'faint' : null }, txt),
        h('span', { class: 'cell-sub' }, c.cost ? COST_SOURCE[c.cost.source] || c.cost.source : c.place ? 'the place has no price' : 'not a saved place'));
    }, csv: (c) => (c.cost ? `${c.cost.amount} ${c.cost.currency}` : '') },
    { key: 'ppk', label: 'Per kWh', num: true, cover: 'charges.table.cost_per_kwh', render: (c) => (c.price_per_kwh?.value != null ? h('span', { class: 'cell' }, format({ amount: c.price_per_kwh.value, currency: c.cost?.currency }, 'price_energy'), h('span', { class: 'cell-sub' }, c.price_per_kwh.basis === 'grid' ? 'of grid energy' : 'of energy added')) : '—'), csv: (c) => c.price_per_kwh?.value },
    { key: 'power', label: 'Power', num: true, sort: 'power_max', cover: 'charges.table.avg_power', render: (c) => h('span', { class: 'cell' }, format(c.power_avg_kw, 'power'), c.power_max_kw != null ? h('span', { class: 'cell-sub' }, `peak ${format(c.power_max_kw, 'power')}`) : null), csv: column('Average power', 'power_avg_kw', 'power') },
    { key: 'range', label: 'Range added', num: true, cover: 'charges.table.range_added', render: (c) => format(c.range_added_km, 'range'), csv: column('Range added', 'range_added_km', 'range') },
    { key: 'rate', label: 'Per hour', num: true, cover: 'charges.table.charge_rate', title: 'Range added per hour of charging', render: (c) => format(c.range_per_hour_km, 'range_rate'), csv: column('Range per hour', 'range_per_hour_km', 'range_rate') },
    { key: 'soc', label: 'Battery', sort: 'end_soc', cover: ['charges.table.start_soc', 'charges.table.end_soc'], render: (c) => h('span', { class: 'cs-soc' },
      h('span', { class: 'nowrap' }, `${pct(c.start_soc_pct)} → ${pct(c.end_soc_pct)}`),
      h('span', { class: 'cs-soc-bar', 'aria-hidden': 'true', vars: { '--a': `${Math.max(0, Math.min(100, c.start_soc_pct ?? 0))}%`, '--b': `${Math.max(0, Math.min(100, c.end_soc_pct ?? c.start_soc_pct ?? 0))}%` } }, h('i'))), csv: (c) => `${c.start_soc_pct ?? ''} -> ${c.end_soc_pct ?? ''}` },
    { key: 'temp', label: 'Temps', num: true, cover: 'charges.table.temp', render: (c) => h('span', { class: 'cell' }, format(c.outside_avg_c, 'temperature'), h('span', { class: 'cell-sub' }, c.battery_avg_c != null ? `pack ${format(c.battery_avg_c, 'temperature')}` : 'pack —')), csv: column('Outside', 'outside_avg_c', 'temperature') },
    { key: 'odo', label: 'Odometer', num: true, cover: 'charges.table.odometer', render: (c) => format(c.odometer_km, 'odometer'), csv: column('Odometer', 'odometer_km', 'odometer') },
    { key: 'flags', label: '', csv: false, render: (c) => h('span', { class: 'flagline' }, c.has_gaps ? h('span', { class: 'tag tag--org', title: 'Data missing inside this charge' }, 'Gap') : null, c.flags?.in_progress || c.end_ms == null ? tag('Live', 'grn') : null, sourceTag(c.source)) },
  ].filter(Boolean);
  const list = tracks(ctx, {
    title: 'Charging sessions', icon: 'bolt', path: listPath, params: listParams(), limit: 25, columns, sort: { key: 'start', dir: 'desc' }, csvName: 'charges', cover: 'charges.table', noteCover: 'charges.text.list_help', cls: 'cs-list',
    note: 'The counters above add up exactly the listed charges. Energy added is measured at the pack; DC efficiency is unknown because a fast charger\'s grid side is not measured. Open a row for its curve and cost.',
    rowHref: (c) => state.buildUrl('charge', { car: c.car, charge: c.id }),
    onPage: (pg) => { if (pg.totals) paintSummary(pg.totals); },
    empty: () => emptyState({ icon: 'bolt', title: 'No charges', text: Object.values(queryOf()).some(Boolean) ? `No charge in ${period.label.toLowerCase()} passes every filter that is on. Clear some to see more.` : `${scopeName} did not charge in this period. Pick a longer period.` }),
  });

  /* ---------- charges with missing data */
  const missing = tracks(ctx, {
    title: 'Charges with missing data', icon: 'warn', tone: 'orange', path: listPath, params: { ...base(), flags: 'gap,in_progress' }, limit: 20, csvName: 'charges-missing-data', cover: 'charges.incomplete.table', noteCover: 'charges.text.incomplete_help', dense: true,
    note: 'Charges that overlap a gap in the logs, or that are still under way. Their figures cover what was recorded. Late logs fill them in by themselves; an admin can rebuild the period from the stored logs.',
    side: isAdmin() ? btn('Rebuild the period', rebuild, { sm: true, kind: 'ghost', icon: 'arrow-r' }) : null,
    rowHref: (c) => state.buildUrl('charge', { car: c.car, charge: c.id }),
    columns: [
      multi ? { key: 'car', label: 'Car', render: (c) => carName(c.car) } : null,
      { key: 'id', label: 'Charge', lead: true, cover: 'charges.incomplete.col.id', render: (c) => h('a', { class: 'cell-main mono', href: state.buildUrl('charge', { car: c.car, charge: c.id }) }, c.id), csv: (c) => c.id },
      { key: 'start', label: 'Start', cover: 'charges.incomplete.col.start_date', render: (c) => `${formatDate(c.start_ms, zoneOf(c), { year: false })}, ${formatTime(c.start_ms, zoneOf(c))}`, csv: (c) => iso(c.start_ms) },
      { key: 'end', label: 'Last recorded', cover: 'charges.incomplete.col.end_date', render: (c) => (c.end_ms ? formatTime(c.end_ms, zoneOf(c)) : 'still going'), csv: (c) => iso(c.end_ms) },
      { key: 'added', label: 'Added', num: true, cover: 'charges.incomplete.col.energy_added', render: (c) => format(c.energy_added_kwh, 'energy') },
      { key: 'grid', label: 'From grid', num: true, cover: 'charges.incomplete.col.energy_used', render: (c) => (c.energy_grid_kwh != null ? format(c.energy_grid_kwh, 'energy') : h('span', { class: 'faint' }, c.kind === 'dc' ? 'not measured' : '—')) },
      { key: 'soc_a', label: 'Start %', num: true, cover: 'charges.incomplete.col.start_soc', render: (c) => pct(c.start_soc_pct) },
      { key: 'soc_b', label: 'End %', num: true, cover: 'charges.incomplete.col.end_soc', render: (c) => pct(c.end_soc_pct) },
      { key: 'duration', label: 'Charging', num: true, cover: 'charges.incomplete.col.duration', render: (c) => formatDuration(c.charging_ms) },
    ].filter(Boolean),
    empty: () => emptyState({ icon: 'check', title: 'No charge is missing data', text: 'Every charge in this period has all its data, and none is under way.' }),
  });
  async function rebuild() {
    const ids = fixed ? [fixed.id] : [...new Set(missing.rows().map((c) => c.car))];
    if (!ids.length) { toast('No charge in this period is missing data, so there is nothing to rebuild.', { kind: 'info' }); return; }
    try { let logs = 0; for (const cid of ids) logs += (await ctx.api.post(`cars/${enc(cid)}/rebuild`, ctx.params))?.logs_read ?? 0; toast(`Rebuilt from ${plural(logs, 'stored log')}.`); ctx.reload(); } catch (e) { toast(`Could not rebuild: ${e.message}`, { kind: 'error' }); }
  }

  /* ---------- statistics */
  const scopeLine = h('span', { class: 'trk-count' });
  const totalsEl = h('div');
  const splitEl = h('div');
  const habitsEl = h('div', { class: 'stack' });
  const placesEl = h('div', { class: 'stack' });
  const curveEl = h('div', { class: 'stack' });
  const curveSide = h('span', { class: 'row' });
  let gen = 0;
  async function loadStats() {
    const my = ++gen;
    const sp = statsParams();
    scopeLine.textContent = `${scopeName}, ${period.label.toLowerCase()}${f.places ? ', at the chosen places' : ''}${f.min_dur ? `, charges of at least ${f.min_dur} min` : ''}`;
    let s, rows;
    try { [s, rows] = await Promise.all([ctx.api.get(statsPath, sp), ctx.api.list(listPath, sp, { limit: 500 }).all({ max: 5000 })]); } catch (e) {
      if (my !== gen || !ctx.alive()) return;
      for (const el of [totalsEl, splitEl, habitsEl]) replace(el, h('p', { class: 'note note--warn' }, icon('warn', 'i--sm'), h('span', null, `The statistics could not be loaded: ${e.message}`)));
      await Promise.all([paintPlaces(sp, my), paintCurve([], my)]);
      return;
    }
    if (my !== gen || !ctx.alive()) return;
    paintTotals(s); paintSplit(s);
    await paintHabits(s, rows, my);
    if (my !== gen || !ctx.alive()) return;
    await Promise.all([paintPlaces(sp, my), paintCurve(rows, my)]);
  }

  function paintTotals(s) {
    const ppk = s.price_per_kwh || {}, c100 = s.cost_per_100km || {};
    replace(totalsEl, counters([
      { label: 'Sessions', value: s.count ?? 0, kind: 'count', cover: 'cs.stat.count', how: `${formatNumber(ppk.priced ?? 0)} with a price` },
      { label: 'Energy added', value: s.count ? s.energy_added_kwh : null, kind: 'energy', cover: 'cs.stat.energy_added', how: 'into the pack' },
      { label: 'Total cost', text: moneyText(s.cost_by_currency), cover: 'cs.stat.total_cost', how: 'per currency, never added across' },
      { label: 'Superchargers', text: moneyText(s.supercharger_cost), cover: 'cs.stat.supercharger_cost', how: 'sessions the charger names as Tesla' },
      { label: 'Per kWh', text: priceText(ppk.all), cover: 'cs.stat.cost_per_kwh', how: 'every priced session' },
      { label: 'AC per kWh', text: priceText(ppk.ac), cover: 'cs.stat.cost_per_kwh_ac', how: 'over the grid energy' },
      { label: 'DC per kWh', text: priceText(ppk.dc), cover: 'cs.stat.cost_per_kwh_dc', how: 'over the energy billed' },
      { label: `Per 100 ${unitLabel('distance')}`, text: (c100.value || []).length ? c100.value.map((x) => format({ amount: x.amount, currency: x.currency }, 'money_per_distance')).join(' + ') : '—', cover: 'cs.stat.cost_per_100', how: c100.distance_km != null ? `over ${format(c100.distance_km, 'distance')} driven` : 'charging cost over distance' },
    ]), note('Unpriced charges are left out of the prices, not counted as free. Prices come from each place\'s price, a cost typed on the charge, or imported history; free Supercharging counts as zero.'));
  }

  function paintSplit(s) {
    const bk = s.by_kind || {};
    const kinds = ['ac', 'dc', 'unknown'];
    const one = (k, label, cv) => {
      const total = kinds.reduce((a, x) => a + (bk[x]?.[k] || 0), 0);
      const sh = (x) => (total ? ((bk[x]?.[k] || 0) / total) * 100 : 0);
      return h('div', { class: 'cs-split', cover: cv },
        h('div', { class: 'cs-split-head' }, h('span', { class: 'stat-label' }, h('span', null, label)), h('b', null, k === 'kwh' ? format(total, 'energy') : formatDuration(total))),
        h('div', { class: 'cs-split-bar', role: 'img', 'aria-label': `${label}: ${kinds.map((x) => `${KIND[x]} ${Math.round(sh(x))} %`).join(', ')}` }, kinds.map((x) => h('i', { class: `cs-k-${x}`, vars: { '--w': `${sh(x).toFixed(2)}%` } }))),
        h('div', { class: 'cs-split-key' }, kinds.map((x) => h('span', null, h('i', { class: ['cs-sw', `cs-k-${x}`] }), `${KIND[x]} `, h('b', null, `${formatNumber(sh(x), { maximumFractionDigits: 0 })} %`), h('span', { class: 'faint' }, ` · ${k === 'kwh' ? format(bk[x]?.kwh || 0, 'energy') : formatDuration(bk[x]?.ms || 0)}`)))));
    };
    replace(splitEl, one('kwh', 'Energy added', 'cs.chart.ac_dc_energy'), one('ms', 'Time charging', 'cs.chart.ac_dc_duration'), note('From the charge port\'s own AC or DC signal. Sessions without one are Unknown, not DC.'));
  }

  /* habits */
  let heatSide = q.heat === 'end' ? 'end' : 'start';
  async function paintHabits(s, rowsIn, my) {
    const rows = rowsIn.filter((c) => c.start_soc_pct != null).sort((a, b) => a.start_ms - b.start_ms);
    const from = ctx.params.from_ms ?? rows[0]?.start_ms ?? now();
    const to = Math.min(ctx.params.to_ms ?? now(), now() + 60000);
    const days = (to - from) / 86400000;
    const unit = days <= 62 ? 'day' : days <= 730 ? 'week' : 'month';
    let cols = [];
    try { cols = buckets(from, to, unit, tz, { max: 3000 }); } catch { cols = []; }
    const colIndex = new Map(cols.map((b, i) => [b.key, i]));
    const gridOf = (side) => {
      const g = Array.from({ length: 10 }, () => new Array(cols.length).fill(0));
      for (const c of rows) {
        const v = side === 'start' ? c.start_soc_pct : c.end_soc_pct;
        if (v == null) continue;
        const i = colIndex.get(bucketKey(side === 'start' ? c.start_ms : c.end_ms ?? c.start_ms, unit, tz));
        if (i != null) g[Math.max(0, Math.min(9, Math.floor(v / 10)))][i]++;
      }
      return g;
    };
    const heat = h('div', { class: 'cs-heat', role: 'img' });
    const heatRead = h('span', { class: 'scope-read' });
    const hl = h('i', { class: 'cs-heat-hl', hidden: true });
    let grid = null, delta = null;
    const colText = (b) => (unit === 'month' ? formatDate(b.start_ms + 86400000, tz, { weekday: false }).replace(/\s?\d{1,2},?/, '') : formatDate(b.start_ms + 3600000, tz, { weekday: false, year: false }));
    function paintHeat() {
      grid = gridOf(heatSide);
      const max = Math.max(1, ...grid.flat());
      heat.style.setProperty('--n', String(Math.max(1, cols.length)));
      heat.setAttribute('aria-label', `Battery level at the ${heatSide} of each session, by ${unit}`);
      const cells = [];
      for (let r = 9; r >= 0; r--) {
        cells.push(h('span', { class: 'cs-heat-y' }, `${r * 10}–${r * 10 + 10} %`));
        for (let i = 0; i < cols.length; i++) { const n = grid[r][i]; cells.push(h('i', { class: ['cs-heat-c', n && 'has', heatSide === 'end' && 'is-end'], dataset: { col: i }, vars: { '--a': (n / max).toFixed(3) } })); }
      }
      const step = Math.max(1, Math.ceil(cols.length / 5));
      cells.push(h('span'), ...cols.map((b, i) => (i % step === 0 ? h('span', { class: 'cs-heat-x', vars: { '--span': String(Math.min(step, cols.length - i)) } }, colText(b)) : null)));
      hl.hidden = true;
      replace(heat, cells, hl);
      heatRead.textContent = rows.length ? `${plural(rows.length, 'session')}; brighter: more sessions ${heatSide === 'start' ? 'started' : 'ended'} at that level` : 'no session in the period';
    }
    const showCol = (i, fromChart = false) => {
      if (i == null || !cols[i]) { hl.hidden = true; return; }
      hl.hidden = false;
      hl.style.setProperty('--col', String(i + 2));
      const n = grid.reduce((a, r) => a + r[i], 0);
      heatRead.textContent = `${unit === 'week' ? 'Week of ' : ''}${colText(cols[i])} · ${plural(n, 'session')}`;
      if (!fromChart && delta?.u) {
        const mid = (cols[i].start_ms + cols[i].end_ms) / 2000;
        const xs = delta.u.data[0];
        let best = -1, bd = Infinity;
        xs.forEach((x, k) => { const d = Math.abs(x - mid); if (d < bd) { bd = d; best = k; } });
        if (best >= 0 && xs[best] * 1000 >= cols[i].start_ms && xs[best] * 1000 < cols[i].end_ms) delta.u.setCursor({ left: delta.u.valToPos(xs[best], 'x'), top: delta.u.valToPos(50, 'y') });
      }
    };
    heat.addEventListener('mousemove', (e) => { const c = e.target.closest('.cs-heat-c'); if (c) showCol(+c.dataset.col); });
    heat.addEventListener('mouseleave', () => { showCol(null); delta?.u?.setCursor({ left: -10, top: -10 }); });
    const sideSeg = seg([['start', 'Where sessions start'], ['end', 'Where they end']], heatSide, (v) => { heatSide = v; ctx.setQuery({ heat: v === 'end' ? 'end' : null }); paintHeat(); paintWeek(); }, { label: 'Start or end level' });

    const week = h('div', { class: 'cs-week', role: 'img' });
    function paintWeek() {
      const g = s.heatmap?.[heatSide] || [];
      const max = Math.max(1, ...g.flat());
      week.setAttribute('aria-label', `When sessions ${heatSide}, by weekday and hour`);
      const cells = [h('span')];
      for (let hr = 0; hr < 24; hr += 6) cells.push(h('span', { class: 'cs-week-x' }, `${String(hr).padStart(2, '0')}:00`));
      for (let d = 0; d < 7; d++) {
        cells.push(h('span', { class: 'cs-week-y' }, DAYS[d]));
        for (let hr = 0; hr < 24; hr++) { const n = g[d]?.[hr] || 0; cells.push(h('i', { class: ['cs-heat-c', n && 'has', heatSide === 'end' && 'is-end'], title: `${DAYS[d]} ${String(hr).padStart(2, '0')}:00: ${plural(n, 'session')}`, vars: { '--a': (n / max).toFixed(3) } })); }
      }
      replace(week, cells);
    }
    const bandList = (list, side) => {
      const items = (list || []).slice().reverse();
      const max = Math.max(1, ...items.map((b) => b.count));
      return eq(items.map((b) => { const lo = parseInt(b.band, 10); return { label: `${String(b.band).replace('-', '–')} %`, value: b.count / max, text: plural(b.count, 'session'), tone: side === 'end' ? (lo >= 80 ? 'org' : null) : lo < 20 ? 'org' : null }; }), { empty: 'No session in the period.' });
    };
    const deltaBox = well('well--lg', 'Each session, from its start level to its end level');
    const deltaRead = h('span', { class: 'scope-read' });
    const runs = tracks(ctx, {
      title: 'Back-to-back charging', icon: 'arrow-r', rows: s.runs || [], csvName: 'charging-runs', sort: { key: 'start', dir: 'desc' }, dense: true,
      note: 'Two or more charges, each starting within a day of the one before: top-ups that belong together, so one long charge in parts is not read as a habit.',
      columns: [
        multi ? { key: 'car', label: 'Car', sort: true, value: (r) => carName(r.car), render: (r) => carName(r.car) } : null,
        { key: 'start', label: 'First start', sort: true, lead: true, value: (r) => r.start_ms, render: (r) => `${formatDate(r.start_ms, tz, { year: false })}, ${formatTime(r.start_ms, tz)}`, csv: (r) => iso(r.start_ms) },
        { key: 'charges', label: 'Charges', num: true, sort: true, render: (r) => formatNumber(r.charges) },
        { key: 'min_start_soc_pct', label: 'Lowest start', num: true, sort: true, render: (r) => pct(r.min_start_soc_pct) },
        { key: 'max_end_soc_pct', label: 'Highest end', num: true, sort: true, render: (r) => pct(r.max_end_soc_pct) },
      ].filter(Boolean),
      empty: () => emptyState({ icon: 'check', title: 'No back-to-back charges', text: 'Every charge in this period stands on its own.' }),
    });
    replace(habitsEl,
      h('div', { class: 'row cs-habit-ctl' }, sideSeg.el,
        btn(`Follows the period: ${period.label}`, (e) => openPeriod(e.currentTarget), { sm: true, kind: 'ghost', icon: 'calendar', cover: ['cs.filter.heatmap_window', 'cs.filter.charge_delta_window'] })),
      counters([
        { label: 'Ended above 80 %', value: s.share_above_80_pct, kind: 'percent', how: 'of the sessions' },
        { label: 'Started below 20 %', value: s.share_below_20_pct, kind: 'percent', how: 'of the sessions' },
        { label: 'Daily band', text: '20–80 %', how: 'the usual band for a nickel pack; LFP may go to 100 %' },
      ]),
      h('div', { class: 'cs-block', cover: ['cs.chart.heatmap', 'cs.control.shared_crosshair'] },
        h('div', { class: 'scope-head' }, h('span', { class: 'scope-title' }, `Battery level per ${unit}`), h('span'), h('span', { class: 'row' }, heatRead,
          csvBtn({ name: [scopeName, `charging-levels-${unit}`], rows: () => cols.map((b, i) => Object.fromEntries([['period', b.key], ...grid.map((r, k) => [`b${k}`, r[i]])])), columns: [column('Period', 'period'), ...Array.from({ length: 10 }, (_, k) => column(`${k * 10}-${k * 10 + 10} %`, `b${k}`))] }))),
        heat),
      h('div', { class: 'cs-block', cover: ['cs.chart.charge_delta', 'cs.chart.charge_delta.band'] },
        h('div', { class: 'scope-head' }, h('span', { class: 'scope-title' }, 'Each session, start level to end level'),
          h('span', { class: 'keys' }, ['ac', 'dc', 'unknown'].map((k) => h('span', { class: 'key' }, h('i', { class: ['key-sw', `cs-k-${k}`] }), KIND[k])), h('span', { class: 'key' }, h('i', { class: 'key-sw cs-k-band' }), 'daily band 20–80 %')),
          h('span', { class: 'row' }, deltaRead, csvBtn({ name: [scopeName, 'charging-start-end'], rows, columns: [column('Car', (c) => carName(c.car)), column('Charge', 'id'), column('Start', (c) => iso(c.start_ms)), column('Type', (c) => KIND[c.kind] || 'Unknown'), column('Start level', 'start_soc_pct', 'percent'), column('End level', 'end_soc_pct', 'percent')] }))),
        rows.length ? deltaBox : note('No session in the period.'),
        note('Point at either chart: the other one points at the same moment. Click a bar to open that charge.')),
      h('div', { class: 'cols--3 cols cs-lists' },
        h('div', { class: 'stack', cover: ['cs.table.discharge_stats', 'cs.table.discharge_stats.soc', 'cs.table.discharge_stats.count'] },
          h('div', { class: 'cs-list-h' }, h('h3', { class: 'cs-h3' }, 'Level when plugging in'), csvBtn({ name: [scopeName, 'charging-start-bands'], rows: s.start_bands || [], columns: [column('Band (%)', 'band'), column('Sessions', 'count')] })),
          bandList(s.start_bands, 'start'),
          h('p', { class: 'note', cover: 'cs.text.discharge_stats_help' }, icon('info', 'i--sm'), h('span', null, 'How low the battery is when charging starts. Every session counts once at its starting level; below 20 % is orange.'))),
        h('div', { class: 'stack', cover: ['cs.table.charge_stats', 'cs.table.charge_stats.soc', 'cs.table.charge_stats.count'] },
          h('div', { class: 'cs-list-h' }, h('h3', { class: 'cs-h3' }, 'Level when charging stops'), csvBtn({ name: [scopeName, 'charging-end-bands'], rows: s.end_bands || [], columns: [column('Band (%)', 'band'), column('Sessions', 'count')] })),
          bandList(s.end_bands, 'end'),
          note('Where sessions end. Orange bands are above the daily band.')),
        h('div', { class: 'stack' },
          h('div', { class: 'cs-list-h' }, h('h3', { class: 'cs-h3' }, 'When in the week'), csvBtn({ name: [scopeName, `charging-${heatSide}-week`], rows: () => (s.heatmap?.[heatSide] || []).map((r, d) => ({ day: DAYS[d], ...Object.fromEntries(r.map((n, hr) => [`h${hr}`, n])) })), columns: [column('Day', 'day'), ...Array.from({ length: 24 }, (_, hr) => column(`${String(hr).padStart(2, '0')}:00`, `h${hr}`))] })),
          week, note('By the hour sessions start (or end), in each car\'s own time.'))),
      runs.el);
    paintHeat(); paintWeek();
    if (!rows.length) return;
    await loadUPlot();
    if (!ctx.alive() || my !== gen) return;
    const xs = rows.map((c) => c.start_ms / 1000), lo = rows.map((c) => c.start_soc_pct), hi = rows.map((c) => c.end_soc_pct ?? c.start_soc_pct);
    delta = mountPlot(deltaBox, (width, height) => {
      const pal = { ac: cssVar('--grn'), dc: cssVar('--org'), unknown: cssVar('--st-unknown') }, bandC = cssVar('--cyan');
      return {
        width, height, padding: [rpx(10), rpx(16), 0, 0], legend: { show: false },
        cursor: { sync: { key: `cs-${fixed?.id || 'all'}`, setSeries: false }, y: false, points: { show: false }, drag: { x: false, y: false } },
        scales: { x: { time: true, range: () => [from / 1000, to / 1000] }, y: { range: () => [0, 100] } },
        axes: [timeAxis(tz, { spanMs: to - from }), { ...valueAxis({ size: 44 }), values: (u, v) => v.map((n) => `${n} %`), splits: () => [0, 20, 40, 60, 80, 100] }],
        series: [{}, { stroke: 'transparent', points: { show: false } }, { stroke: 'transparent', points: { show: false } }],
        hooks: {
          drawAxes: [(u) => {
            const g = u.ctx, { left, width: w } = u.bbox;
            const y20 = Math.round(u.valToPos(20, 'y', true)), y80 = Math.round(u.valToPos(80, 'y', true));
            g.save(); g.fillStyle = alpha(bandC, 0.08); g.fillRect(left, y80, w, y20 - y80);
            g.strokeStyle = alpha(bandC, 0.55); g.lineWidth = 1; g.setLineDash([rpx(4), rpx(4)]);
            for (const y of [y20, y80]) { g.beginPath(); g.moveTo(left, y + 0.5); g.lineTo(left + w, y + 0.5); g.stroke(); }
            g.restore();
          }],
          draw: [(u) => {
            const g = u.ctx, pr = u.ctx.canvas.width / Math.max(1, u.width);
            const bw = Math.max(rpx(2.5), Math.min(rpx(9), (u.bbox.width / pr / Math.max(1, rows.length)) * 0.6)) * pr;
            g.save();
            rows.forEach((r, i) => {
              const x = u.valToPos(xs[i], 'x', true), y1 = u.valToPos(lo[i], 'y', true), y2 = u.valToPos(hi[i], 'y', true);
              g.fillStyle = alpha(pal[r.kind] || pal.unknown, u.cursor.idx === i ? 1 : 0.8);
              g.fillRect(Math.round(x - bw / 2), Math.round(Math.min(y1, y2)), Math.round(bw), Math.max(Math.round(rpx(2) * pr), Math.round(Math.abs(y1 - y2))));
            });
            g.restore();
          }],
          setCursor: [(u) => {
            const i = u.cursor.idx;
            if (i == null || u.cursor.left < 0) { deltaRead.textContent = ''; return; }
            const r = rows[i];
            deltaRead.textContent = `${multi ? `${carName(r.car)} · ` : ''}${formatDate(r.start_ms, zoneOf(r), { year: false })}, ${formatTime(r.start_ms, zoneOf(r))} · ${pct(r.start_soc_pct)} → ${pct(r.end_soc_pct)} · ${KIND[r.kind] || 'Unknown'} · ${placeLabel(where(r))}`;
            showCol(colIndex.get(bucketKey(r.start_ms, unit, tz)) ?? null, true);
            u.redraw(false, false);
          }],
        },
      };
    }, [xs, lo, hi], ctx, (u) => {
      u.over.addEventListener('click', () => { const i = u.cursor.idx; if (i != null && rows[i]) ctx.navigate(state.buildUrl('charge', { car: rows[i].car, charge: rows[i].id })); });
      u.over.addEventListener('mouseleave', () => showCol(null, true));
    });
  }

  /* places */
  async function paintPlaces(sp, my) {
    const pp = { cars: fixed ? fixed.id : sp.cars, from_ms: sp.from_ms, to_ms: sp.to_ms, places: sp.places, min_duration_min: sp.min_duration_min };
    const placeCol = (cv) => ({ key: 'label', label: 'Place', lead: true, cover: cv, render: (p) => h('span', { class: 'cell' }, h('a', { class: 'cell-main plc', href: placeHref({ place: p.place ? { id: p.place } : null, lat: p.lat, lon: p.lon }) }, p.label), p.place ? null : h('span', { class: 'cell-sub' }, 'not a saved place')), csv: (p) => p.label });
    const byEnergy = tracks(ctx, { title: 'By energy', icon: 'bolt', path: 'charging-stats/places', params: { ...pp, sort: 'energy' }, limit: 8, csvName: 'charging-places-energy', cover: 'cs.table.top_energy', dense: true,
      columns: [placeCol('cs.table.top_energy.location'), { key: 'sessions', label: 'Sessions', num: true, render: (p) => formatNumber(p.sessions) }, { key: 'energy', label: 'Added', num: true, cover: 'cs.table.top_energy.charged', render: (p) => format(p.energy_added_kwh, 'energy') }],
      empty: () => emptyState({ icon: 'pin', title: 'No charging place', text: 'No charge in this period.' }) });
    const byCost = tracks(ctx, { title: 'By cost', icon: 'stats', path: 'charging-stats/places', params: { ...pp, sort: 'cost' }, limit: 8, csvName: 'charging-places-cost', cover: 'cs.table.top_cost', dense: true,
      columns: [placeCol('cs.table.top_cost.location'), { key: 'sessions', label: 'Sessions', num: true, render: (p) => formatNumber(p.sessions) }, { key: 'cost', label: 'Cost', num: true, cover: 'cs.table.top_cost.cost', render: (p) => (moneyList(p.cost_by_currency).length ? moneyText(p.cost_by_currency) : h('span', { class: 'faint' }, 'no price')) }],
      empty: () => emptyState({ icon: 'pin', title: 'No charging place', text: 'No charge in this period.' }) });
    const mapEl = h('div', { class: 'map cs-map' });
    const mapRead = h('span', { class: 'scope-read' });
    let pts = [];
    replace(placesEl,
      h('div', { class: 'cs-mapbox', cover: 'cs.map.energy_by_location' },
        h('div', { class: 'scope-head' }, h('span', { class: 'scope-title' }, 'Energy by place'), h('span'), h('span', { class: 'row' }, mapRead, csvBtn({ name: [scopeName, 'charging-places-map'], rows: () => pts, columns: [column('Place', 'label'), column('Latitude', 'lat'), column('Longitude', 'lon'), column('Sessions', 'sessions'), column('Energy added', 'energy_added_kwh', 'energy')] }))),
        mapEl, note('Grouped by saved place, else by position, so one spot is never split in two. Bigger circles took more energy; click one for its charges.')),
      h('div', { class: 'cols' }, byEnergy.el, byCost.el));
    try { pts = (await ctx.api.page('charging-stats/places', { ...pp, sort: 'energy' }, { limit: 200 })).items || []; } catch { pts = []; }
    if (!ctx.alive() || my !== gen) return;
    mapRead.textContent = pts.length ? plural(pts.length, 'place') : 'no place';
    const mapping = (async () => {
      if (!pts.length) { mapEl.append(mapFallback('No charge in this period, so there is nothing to map.')); return; }
      let m;
      try { m = await createMap(mapEl, ctx, { bounds: boundsOf(pts.map((p) => [p.lon, p.lat])), zoom: 10 }); } catch { replace(mapEl, mapFallback()); return; }
      if (!m) return;
      await m.ready;
      if (!ctx.alive()) return;
      const max = Math.max(1, ...pts.map((p) => p.energy_added_kwh || 0));
      m.map.addSource('cs-places', { type: 'geojson', data: { type: 'FeatureCollection', features: pts.map((p, i) => ({ type: 'Feature', id: i, properties: { i, r: 4 + 14 * Math.sqrt((p.energy_added_kwh || 0) / max) }, geometry: { type: 'Point', coordinates: [p.lon, p.lat] } })) } });
      m.addLayer({ id: 'cs-places', type: 'circle', source: 'cs-places', paint: { 'circle-color': cssVar('--grn'), 'circle-opacity': 0.5, 'circle-stroke-color': cssVar('--grn') } }, { 'circle-radius': () => ['*', ['get', 'r'], rpx(1)], 'circle-stroke-width': () => rpx(1.5) });
      m.map.on('mousemove', 'cs-places', (e) => { const p = pts[e.features?.[0]?.properties?.i]; if (p) mapRead.textContent = `${p.label} · ${format(p.energy_added_kwh, 'energy')} · ${plural(p.sessions, 'session')}`; });
      m.map.on('mouseleave', 'cs-places', () => { mapRead.textContent = plural(pts.length, 'place'); });
      m.map.on('click', 'cs-places', (e) => { const p = pts[e.features?.[0]?.properties?.i]; if (!p) return; if (p.place) { f.places = p.place; ctx.setQuery(queryOf()); ctx.reload(); } else ctx.navigate(placeHref({ lat: p.lat, lon: p.lon })); });
      m.frame(() => { if (pts.length === 1) m.map.jumpTo({ center: [pts[0].lon, pts[0].lat], zoom: 13 }); else m.map.fitBounds(boundsOf(pts.map((p) => [p.lon, p.lat])), { padding: pad(36), duration: 0, maxZoom: 14 }); });
    })();
    await Promise.all([byEnergy.ready, byCost.ready, mapping]);
  }

  /* the fast-charging curve, one car at a time */
  let curveCar = q.curve_car && viewCars.includes(q.curve_car) ? q.curve_car : null;
  async function paintCurve(rows, my) {
    const dcCount = new Map();
    for (const c of rows) if (c.kind === 'dc') dcCount.set(c.car, (dcCount.get(c.car) || 0) + 1);
    if (!curveCar || !viewCars.includes(curveCar)) curveCar = fixed ? fixed.id : [...dcCount.entries()].sort((a, b) => b[1] - a[1])[0]?.[0] || viewCars[0];
    if (!curveCar) { replace(curveEl, note('There is no car yet.')); return; }
    let data = { median: [], sessions: [] };
    const read = h('span', { class: 'scope-read' });
    const box = well('well--lg', 'Power against battery level, every DC session');
    replace(curveSide,
      multi && viewCars.length > 1 ? select(viewCars.map((cid) => [cid, `${carName(cid)} (${formatNumber(dcCount.get(cid) || 0)} DC)`]), curveCar, (v) => { curveCar = v; ctx.setQuery({ curve_car: v }); paintCurve(rows, gen); }, { label: 'Car for the curve' }) : null,
      csvBtn({ name: [carName(curveCar), 'dc-curve'], cover: 'cs.chart.dc_curve_sessions', rows: () => [...(data.median || []).map((p) => ({ row: 'median', ...p })), ...(data.sessions || []).flatMap((s2) => s2.points.map((p) => ({ row: 'session', charge: s2.charge, label: s2.label, start: iso(s2.start_ms), ...p })))],
        columns: [column('Row', 'row'), column('Charge', 'charge'), column('Place', 'label'), column('Start', 'start'), column('Battery level', 'soc_pct', 'percent'), column('Power', 'power_kw', 'power'), column('Battery temperature', 'battery_c', 'temperature')] }));
    replace(curveEl,
      h('div', { class: 'scope-head' }, h('span', { class: 'scope-title' }, `Power against battery level${multi ? ` · ${carName(curveCar)}` : ''}`),
        h('span', { class: 'keys' }, h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': 'var(--ink)' } }), 'median of every session'), h('span', { class: 'key cs-temp' }, `battery ${format(10, 'temperature', { digits: 0 })}`, h('i', { class: 'cs-ramp' }), format(40, 'temperature', { digits: 0 }))), read),
      h('div', { cover: ['cs.chart.dc_curve_median', 'cs.link.dc_curve_details'] }, box),
      note('One dot per battery percent of each session, coloured by battery temperature (a cold pack charges slower). The line is the median power at each percent. Point at a dot for its session; click to open the charge.'));
    try { data = await ctx.api.get(`cars/${enc(curveCar)}/charging-stats/dc-curve`, { from_ms: ctx.params.from_ms, to_ms: ctx.params.to_ms, places: f.places || null }); } catch (e) { replace(box, mapFallback(`The curve could not be loaded: ${e.message}`)); return; }
    if (my !== gen || !ctx.alive()) return;
    const sessions = data.sessions || [];
    const idle = () => { read.textContent = sessions.length ? `${plural(sessions.length, 'DC session')}${data.median?.length ? `; median peak ${format(Math.max(...data.median.map((p) => p.power_kw)), 'power')}` : ''}` : 'no DC session'; };
    idle();
    if (!sessions.length) { box.append(mapFallback(`${carName(curveCar)} did not fast-charge in this period.`)); return; }
    await loadUPlot();
    if (!ctx.alive()) return;
    const maxP = Math.max(10, ...sessions.flatMap((s2) => s2.points.map((p) => p.power_kw || 0)));
    const tzc = data.time_zone || tz;
    let dots = [], hover = null;
    let tc = null;
    const tempColor = (bc) => (bc == null ? tc.none : bc <= 15 ? tc.cold : bc <= 30 ? tc.mild : tc.warm);
    mountPlot(box, (width, height) => {
      const c = tone(), med = cssVar('--ink');
      tc = { cold: cssVar('--cyan'), mild: cssVar('--violet'), warm: cssVar('--org'), none: cssVar('--st-unknown') };
      return {
        width, height, padding: [rpx(10), rpx(28), 0, rpx(8)], legend: { show: false },
        cursor: { x: false, y: false, points: { show: false }, drag: { x: false, y: false } },
        scales: { x: { time: false, range: () => [0, 100] }, y: { range: () => [0, maxP * 1.08] } },
        axes: [{ ...valueAxis({ size: 26 }), side: 2, grid: { show: false }, space: rpx(72), values: (u, v) => v.map((n) => `${n} %`) }, { ...valueAxis({ size: 48 }), values: (u, v) => v.map((n) => `${formatNumber(n)} kW`) }],
        series: [{}, { stroke: med, width: rpx(2.5), points: { show: true, size: rpx(5), stroke: med, fill: med }, spanGaps: false }],
        hooks: { drawAxes: [(u) => {
          const g = u.ctx, pr = g.canvas.width / Math.max(1, u.width), r = rpx(2.6) * pr;
          dots = [];
          g.save();
          for (const s2 of sessions) {
            const on = hover && hover.s === s2;
            if (on) { g.strokeStyle = alpha(c.ink, 0.8); g.lineWidth = rpx(1.5) * pr; g.beginPath(); s2.points.forEach((pt, k) => { const x = u.valToPos(pt.soc_pct, 'x', true), y = u.valToPos(pt.power_kw, 'y', true); if (k) g.lineTo(x, y); else g.moveTo(x, y); }); g.stroke(); }
            for (const pt of s2.points) {
              const x = u.valToPos(pt.soc_pct, 'x', true), y = u.valToPos(pt.power_kw, 'y', true);
              g.globalAlpha = on ? 1 : 0.7; g.fillStyle = tempColor(pt.battery_c);
              g.beginPath(); g.arc(x, y, on ? r * 1.4 : r, 0, Math.PI * 2); g.fill(); g.globalAlpha = 1;
              dots.push({ x: x / pr, y: y / pr, s: s2, pt });
            }
          }
          g.restore();
        }] },
      };
    }, [(data.median || []).map((p) => p.soc_pct), (data.median || []).map((p) => p.power_kw)], ctx, (u) => {
      const pr = () => u.ctx.canvas.width / Math.max(1, u.width);
      u.over.addEventListener('mousemove', (e) => {
        const x = e.offsetX + u.bbox.left / pr(), y = e.offsetY + u.bbox.top / pr();
        let best = null, bd = rpx(10) ** 2;
        for (const d of dots) { const dd = (d.x - x) ** 2 + (d.y - y) ** 2; if (dd < bd) { bd = dd; best = d; } }
        const changed = (best?.s || null) !== (hover?.s || null);
        hover = best;
        u.over.style.cursor = best ? 'pointer' : '';
        if (best) read.textContent = `${best.s.label} · ${formatDate(best.s.start_ms, tzc, { year: false })}, ${formatTime(best.s.start_ms, tzc)} · ${pct(best.pt.soc_pct)}: ${format(best.pt.power_kw, 'power')} · battery ${format(best.pt.battery_c, 'temperature')}`; else idle();
        if (changed) u.redraw(false, false);
      });
      u.over.addEventListener('mouseleave', () => { if (hover) { hover = null; u.redraw(false, false); } idle(); });
      u.over.addEventListener('click', () => { if (hover) ctx.navigate(state.buildUrl('charge', { car: curveCar, charge: hover.s.charge })); });
    });
  }

  let rowsT = 0;
  ctx.events('rows', (ev) => {
    if (ev.data?.car && !viewCars.includes(ev.data.car)) return;
    if (ev.data?.kinds && !ev.data.kinds.includes('charges')) return;
    clearTimeout(rowsT);
    rowsT = setTimeout(() => { if (!ctx.alive()) return; list.reload(listParams(), { quiet: true }); missing.reload(null, { quiet: true }); loadStats(); }, 2000);
  });
  ctx.onCleanup(() => clearTimeout(rowsT));

  replace(ctx.view, h('div', { class: 'page cs', cover: ['shots.charges', 'shots.charging-stats'] },
    plate({ kicker: `Charging · ${scopeName}`, title: 'Charges', aim: ctx.info.aim, extra: h('p', { class: 'cs-zone' }, multi ? 'Each charge in its own car\'s time.' : `Times in ${tz.replace(/_/g, ' ')}.`) }),
    deck,
    hud({ title: 'On this log', icon: 'stats', cls: 'cs-sum' }, sumEl),
    list.el, missing.el,
    band(1, 'Charging statistics', scopeLine),
    h('div', { class: 'cols cols--wide cs-pair' },
      hud({ title: 'Costs and totals', icon: 'stats', tone: 'orange' }, totalsEl),
      hud({ title: 'AC and DC', icon: 'plug' }, splitEl)),
    hud({ title: 'Charging habits', icon: 'battery', tone: 'cyan', id: 'habits' }, habitsEl),
    hud({ title: 'Charging places', icon: 'pin' }, placesEl),
    hud({ title: 'Fast-charging curve', icon: 'bolt', tone: 'orange', side: curveSide }, curveEl)));
  paintSummary(null);

  try { await Promise.all([list.ready, missing.ready, loadStats()]); } catch (e) { ctx.fail(e); return; }
  if (ctx.alive()) ctx.ready();
}
