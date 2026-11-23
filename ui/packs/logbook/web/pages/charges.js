// Sentry Mate, "Logbook" UI pack: Charges (/cars/{car}/charges, or /charges?cars=all|ids for several cars).
//
// Find charging sessions and understand charging habits and costs, for the shared period. Data:
// GET /cars/{car}/charges or /charges?cars= (the list: filters, sort, cursor paging, totals, CSV),
// /places (the place pick list), /cars (the car pick list), /charges?flags=gap,in_progress (charges with
// missing data) and each one's detail for where its gap lies, /charging-stats (costs, AC and DC, habits),
// /charging-stats/places (map and tables), /cars/{car}/charging-stats/dc-curve (the fast-charging curve),
// /cars/{car}/settings (the pack's chemistry, for the daily band), PUT and DELETE .../cost and
// POST /cars/{car}/rebuild for admins.
//
// AC or DC comes from the charge port's own signal; a session without one is Unknown, never DC. DC
// efficiency is unknown (a fast charger's grid side is not measured), never a made-up 100 %. Money stays
// per currency. Filters live in the address, so a link or Back shows the same view; the period is the
// frame's. The charts are SVG in percentages of boxes sized in rem, so they stay sharp at any size.

import { h, svg, replace, cssVar } from '../shared/dom.js';
import { format, parts, formatMoney, formatMoneyList, unitLabel, rangeLabel } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, buckets, bucketKey, periodLabel } from '../shared/time.js';
import { formatNumber, tn } from '../shared/i18n.js';
import { downloadCsv, csvFilename, column, timeColumn } from '../shared/csv.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { costSourceLabel } from '../shared/invoices.js';
import { pageHead, q, figure, icon, empty, errorView, loadingBlock, flag, button, sep } from '../core/ui.js';
import { dataTable } from '../core/table.js';
import { makeMap, boundsOf } from '../core/map.js';
import { nearNow } from '../shared/lazy.js';
import { IMAGERY_HALO } from '../shared/maptiles.js';
import { openDialog, confirmDialog, popover } from '../core/dialog.js';
import { choice, soldPickRows } from '../core/form.js';
import { placeLink } from '../core/journal.js';
import { SOURCE_FILTER, sourceFilter, SOURCE_WHY, rowTitle } from '../core/sources.js';
import { zoneLabel } from '../core/format.js';
import { display } from '../core/display.js';
import { onTheme } from '../core/prefs.js';
import { remount } from '../core/router.js';
import { setPrice } from '../shared/prices.js';

const PRICE_CLS = { btn: 'button button-small', primary: '', ghost: 'button-quiet', plain: 'button-quiet', input: 'field-input', select: 'field-select' };

export const id = 'charges';
export const css = true;
export const title = () => 'Charges';

const KIND = { ac: 'AC', dc: 'DC', unknown: 'Unknown' };
const KIND_WHY = {
  ac: 'AC: the car’s own charger turned mains power into DC. The grid energy is metered, so losses and efficiency are known.',
  dc: 'DC: a fast charger fed the pack directly. Its grid side is not metered, so the efficiency is unknown.',
  unknown: 'Unknown: the charge port did not say AC or DC (often imported history). It is never counted as DC.',
};
const COST_SOURCE = { tariff: 'place price', manual: 'typed in', free_supercharging: 'free Supercharging', teslamate: 'imported', tesla_invoice: 'Tesla invoice', teslafi: 'from TeslaFi' };
const PLACE_KINDS = [['', 'Any kind of place'], ['home', 'Home'], ['work', 'Work'], ['charger', 'Public charger'], ['service', 'Service centre'], ['other', 'Other saved place'], ['none', 'Not a saved place']];
const TYPES = [['', 'All'], ['ac', 'AC'], ['dc', 'DC'], ['unknown', 'Unknown']];
const SOURCES = SOURCE_FILTER;
const FLAGS = [['', 'Every charge'], ['gap', 'With missing data'], ['in_progress', 'Under way'], ['gap,in_progress', 'Missing data or under way']];
const CURRENCIES = ['USD', 'CAD', 'EUR', 'GBP', 'NOK', 'SEK', 'DKK', 'CHF', 'AUD', 'NZD', 'JPY', 'CNY'];
const DAYS = ['Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday', 'Sunday'];
const oneOf = (list, v, dflt = '') => (list.some(([k]) => k === v) ? v : dflt);
const idList = (v) => (v ? String(v).split(',').map((x) => x.trim()).filter(Boolean) : []);
const posNum = (v) => { const n = parseFloat(v); return Number.isFinite(n) && n >= 0 ? n : null; };
const pct = (v) => format(v, 'percent');
const placeName = (c) => (c.place && c.place.name) || (c.address && (c.address.short || c.address.locality)) || 'An unnamed spot';
const pointOf = (c) => ({ place: c.place, lat: c.position && c.position.lat, lon: c.position && c.position.lon });
// Several currencies are never added; each keeps its code, since a symbol such as $ alone is ambiguous.
const mixed = (list) => new Set(list.map((x) => x.currency)).size > 1;
const moneyList = (list) => { const l = (list || []).filter((x) => x && x.amount != null); return !l.length ? '—' : mixed(l) ? l.map((x) => `${formatMoney(x.amount, x.currency)} ${x.currency}`).join(' + ') : formatMoneyList(l); };
const priceList = (list) => { const l = Array.isArray(list) ? list.filter((x) => x && x.value != null) : []; return l.map((x) => format({ amount: x.value, currency: x.currency }, 'price_energy') + (mixed(l) ? ` ${x.currency}` : '')).join(' + '); };
const csvButton = (label, onclick, cover = null) => h('button', { type: 'button', class: 'button button-quiet button-small ch-csv', onclick, cover, 'aria-label': `Download CSV: ${label}` }, icon('download'), 'CSV');
const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
const p3 = (v) => `${v.toFixed(3)}%`;

// The filters of the address, and the API queries they make: base (the period and the cars), the list's and
// the statistics'. prefetch() warms the first reads with the same ones when a link here is under the pointer.
const filtersOf = (q) => ({
  places: idList(q.places), place_kind: oneOf(PLACE_KINDS, q.place_kind), kind: oneOf(TYPES, q.kind), q: q.q || '',
  min_cost: posNum(q.min_cost), unpriced: q.unpriced === '1', min_dur: posNum(q.min_dur),
  source: sourceFilter(q.source), flags: oneOf(FLAGS, q.flags), heat: q.heat === 'end' ? 'end' : 'start',
});
const baseQuery = (ctx, carsParam) => ({ ...ctx.params, cars: carsParam });
const listQuery = (ctx, f, carsParam) => ({
  ...baseQuery(ctx, carsParam), places: f.places.length ? f.places.join(',') : undefined, place_kind: f.place_kind || undefined, kind: f.kind || undefined, q: f.q || undefined,
  min_cost: f.min_cost ?? undefined, include_unpriced: f.min_cost != null && f.unpriced ? 'true' : undefined,
  min_duration_min: f.min_dur ?? undefined, source: f.source || undefined, flags: f.flags || undefined,
});
const statsQuery = (ctx, f, carsParam) => ({ ...baseQuery(ctx, carsParam), places: f.places.length ? f.places.join(',') : undefined, min_duration_min: f.min_dur ?? undefined });
const fleetRead = (ctx) => ctx.api.list('cars', { sort: 'name' }, { limit: 200 });
// the charging places' query, and one car's fast-charging curve
const placesQuery = (sp, cars) => ({ cars, from_ms: sp.from_ms, to_ms: sp.to_ms, places: sp.places, min_duration_min: sp.min_duration_min });
const curveRead = (ctx, cid, f) => ctx.api.get(api.apiPath`cars/${cid}/charging-stats/dc-curve`, { from_ms: ctx.params.from_ms, to_ms: ctx.params.to_ms, places: f.places.length ? f.places.join(',') : undefined });
const placesRead = (ctx) => ctx.api.list('places', { sort: 'name' }, { limit: 200 });
// one car's charges (without a car, the cars the fleet has decide the queries)
export async function prefetch(ctx) {
  const [, places] = await Promise.all([fleetRead(ctx).all(), placesRead(ctx).all()]);
  if (!ctx.car) return;
  const f = filtersOf(ctx.query), known = new Set((places || []).map((p) => p.id));
  f.places = f.places.filter((x) => known.has(x));
  const cid = ctx.car.id, listPath = api.apiPath`cars/${cid}/charges`, sp = statsQuery(ctx, f, undefined);
  await Promise.all([
    ctx.api.pager(listPath, { ...listQuery(ctx, f, undefined), sort: 'start', dir: 'desc' }, { limit: 50 }).more(), // the list, newest first
    ctx.api.page(listPath, { ...baseQuery(ctx, undefined), flags: 'gap,in_progress' }, { limit: 20 }),
    ctx.api.get(api.apiPath`cars/${cid}/charging-stats`, sp), ctx.api.list(listPath, sp, { limit: 500 }).all(),
    ctx.api.get(api.apiPath`cars/${cid}/settings`),
    // the places tables (8 a page), the places map, and the curve
    ctx.api.pager('charging-stats/places', { ...placesQuery(sp, cid), sort: 'energy' }, { limit: 8 }).more(),
    ctx.api.pager('charging-stats/places', { ...placesQuery(sp, cid), sort: 'cost' }, { limit: 8 }).more(),
    ctx.api.list('charging-stats/places', { ...placesQuery(sp, cid), sort: 'energy' }, { limit: 200 }).all(),
    curveRead(ctx, cid, f),
  ]);
}

export async function mount(ctx) {
  const fixedCar = ctx.car;
  const tz = ctx.tz;
  const admin = !!(ctx.me && ctx.me.role === 'admin');

  /* ---------------------------------------------------------------- which cars */

  let fleet = [];
  try { fleet = await fleetRead(ctx).all({ max: 2000 }); } catch (err) { if (ctx.signal.aborted) throw err; fleet = fixedCar ? [fixedCar] : []; }
  const byId = new Map(fleet.map((c) => [c.id, c]));
  if (fixedCar) byId.set(fixedCar.id, fixedCar);
  const asked = idList(ctx.query.cars).filter((x) => byId.has(x));
  const all = !fixedCar && (!asked.length || ctx.query.cars === 'all');
  const viewCars = fixedCar ? [fixedCar.id] : all ? fleet.map((c) => c.id) : asked;
  const multi = !fixedCar && viewCars.length !== 1;
  const carsParam = fixedCar ? undefined : all ? 'all' : asked.join(',');
  const carName = (cid) => (byId.get(cid) && byId.get(cid).name) || cid;
  const scopeName = fixedCar ? fixedCar.name : all ? (fleet.length === 1 ? fleet[0].name : `All ${formatNumber(fleet.length)} cars`) : asked.length === 1 ? carName(asked[0]) : `${formatNumber(asked.length)} cars`;
  const listPath = fixedCar ? api.apiPath`cars/${fixedCar.id}/charges` : 'charges';
  const statsPath = fixedCar ? api.apiPath`cars/${fixedCar.id}/charging-stats` : 'charging-stats';
  const zoneOf = (c) => (c && c.time_zone) || tz;

  /* ---------------------------------------------------------------- filters */

  const f = filtersOf(ctx.query);
  let places = [], placesFailed = false;
  try { places = await placesRead(ctx).all({ max: 2000 }); } catch (err) { if (ctx.signal.aborted) throw err; placesFailed = true; }
  const placeById = new Map(places.map((p) => [p.id, p]));
  f.places = f.places.filter((x) => placeById.has(x));

  const base = () => baseQuery(ctx, carsParam);
  const listParams = () => listQuery(ctx, f, carsParam);
  const statsParams = () => statsQuery(ctx, f, carsParam);
  const filtered = () => !!(f.places.length || f.place_kind || f.kind || f.q || f.min_cost != null || f.min_dur != null || f.source || f.flags);
  const queryOf = () => ({
    places: f.places.length ? f.places.join(',') : null, place_kind: f.place_kind || null, kind: f.kind || null, q: f.q || null,
    min_cost: f.min_cost ?? null, unpriced: f.min_cost != null && f.unpriced ? '1' : null, min_dur: f.min_dur ?? null,
    source: f.source || null, flags: f.flags || null,
  });

  // Cars: one opens /cars/{car}/charges; several or all open /charges?cars=.
  const carPick = pickList(ctx, {
    id: 'charges-cars', none: 'Every car', noun: ['car', 'cars'],
    items: fleet.map((c) => ({ id: c.id, label: c.name, sold: state.isSold(c), sub: [[c.model, c.year].filter(Boolean).join(' '), state.isSold(c) ? 'Sold' : null].filter(Boolean).join(' · ') })),
    chosen: fixedCar ? [fixedCar.id] : all ? [] : asked, apply: 'Show these cars',
    onapply: (ids) => {
      const query = queryOf();
      const every = !ids.length || (fleet.length > 1 && ids.length === fleet.length);
      ctx.go(ids.length === 1 ? state.buildUrl('charges', { car: ids[0], period: ctx.state.period, query })
        : state.buildUrl('charges', { period: ctx.state.period, query: { ...query, cars: every ? 'all' : ids.join(',') } }));
    },
  });
  const placePick = pickList(ctx, {
    id: 'charges-places', none: 'Any place', noun: ['place', 'places'], failed: placesFailed, emptyText: 'No places saved yet',
    items: places.map((p) => ({ id: p.id, label: p.name, sub: p.counts ? tn(p.counts.charges || 0, '{n} charge', '{n} charges') : '' })),
    chosen: f.places, instant: true,
    onapply: (ids) => { f.places = ids; changed(); },
  });
  const kindSel = h('select', { class: 'field-select', id: 'charges-pkind' }, PLACE_KINDS.map(([k, t]) => h('option', { value: k, selected: k === f.place_kind }, t)));
  const typeChoice = choice({ label: 'AC or DC', name: 'charges-kind', value: f.kind, options: TYPES, onchange: (v) => { f.kind = v; changed(); } });
  const qIn = h('input', { class: 'field-input', type: 'search', id: 'charges-q', value: f.q, placeholder: 'A place, street or town', autocomplete: 'off' });
  const costIn = h('input', { class: 'field-input ch-num', type: 'number', id: 'charges-cost', min: '0', step: '0.01', inputmode: 'decimal', value: f.min_cost ?? '' });
  const unpricedIn = h('input', { type: 'checkbox', id: 'charges-unpriced', checked: f.unpriced, class: 'ch-check' });
  const durIn = h('input', { class: 'field-input ch-num', type: 'number', id: 'charges-dur', min: '0', step: '1', inputmode: 'numeric', value: f.min_dur ?? '' });
  const srcSel = h('select', { class: 'field-select', id: 'charges-source' }, SOURCES.map(([k, t]) => h('option', { value: k, selected: k === f.source }, t)));
  const flagSel = h('select', { class: 'field-select', id: 'charges-flags' }, FLAGS.map(([k, t]) => h('option', { value: k, selected: k === f.flags }, t)));
  let timer = 0;
  ctx.onCleanup(() => clearTimeout(timer));
  const later = (fn) => { clearTimeout(timer); timer = setTimeout(fn, 300); };
  qIn.addEventListener('input', () => later(() => { f.q = qIn.value.trim(); changed(); }));
  costIn.addEventListener('input', () => later(() => { f.min_cost = costIn.value === '' ? null : posNum(costIn.value); changed(); }));
  durIn.addEventListener('input', () => later(() => { f.min_dur = durIn.value === '' ? null : posNum(durIn.value); changed(); }));
  unpricedIn.addEventListener('change', () => { f.unpriced = unpricedIn.checked; if (f.min_cost != null) changed(); else ctx.setQuery(queryOf()); });
  kindSel.addEventListener('change', () => { f.place_kind = kindSel.value; changed(); });
  srcSel.addEventListener('change', () => { f.source = srcSel.value; changed(); });
  flagSel.addEventListener('change', () => { f.flags = flagSel.value; changed(); });
  const clearBtn = button('Clear the filters', {
    iconName: 'reset', cls: 'button-small',
    onclick: () => {
      Object.assign(f, { places: [], place_kind: '', kind: '', q: '', min_cost: null, unpriced: false, min_dur: null, source: '', flags: '' });
      qIn.value = ''; costIn.value = ''; durIn.value = ''; unpricedIn.checked = false; kindSel.value = ''; srcSel.value = ''; flagSel.value = '';
      typeChoice.set(''); placePick.set([]);
      changed();
    },
  });
  clearBtn.hidden = !filtered();
  function changed() {
    ctx.setQuery(queryOf());
    clearBtn.hidden = !filtered();
    replace(totals);
    table.reload(listParams()).catch(() => {});
    // The statistics follow only the period, the cars, the places and the shortest charging time.
    const key = JSON.stringify(statsParams());
    if (key !== statsKey) loadStats().catch(() => {});
  }

  const periodText = h('span', { class: 'ch-period-text' });
  const periodLine = h('p', { class: 'ch-period', cover: ['charges.filter.time_range', 'cs.filter.time_range'] },
    h('span', { class: 'field-label' }, 'Period'), periodText,
    button('Change the period', { cls: 'button-small', iconName: 'calendar', onclick: () => document.querySelector('.period-trigger')?.click() }));
  const drawPeriod = () => {
    replace(periodText, periodLabel(ctx.period, tz, api.now()), sep(),
      multi ? `days in ${zoneLabel(tz)}; each charge’s times in its car’s own zone` : `times in ${zoneLabel(tz)}`, sep(), 'a charge belongs to the period by its start');
  };
  drawPeriod();

  const fieldOf = (labelFor, text, control, opts = {}) => h('div', { class: ['field', opts.cls], cover: opts.cover },
    h(labelFor ? 'label' : 'span', { class: 'field-label', for: labelFor || null }, text), control, opts.hint ? h('p', { class: 'field-hint' }, opts.hint) : null);
  const filters = h('section', { class: 'ch-filters', 'aria-label': 'Filters' },
    periodLine,
    h('div', { class: 'ch-filter-row' },
      fieldOf('charges-cars', 'Cars', carPick, { cls: 'ch-f-cars', cover: ['charges.filter.car', 'cs.filter.car'], hint: 'One, several or every car, in one view.' }),
      fieldOf('charges-places', 'Places', placePick, { cls: 'ch-f-places', cover: ['charges.filter.geofence', 'cs.filter.geofence'], hint: 'Charges at the ticked saved places; also narrows the statistics.' }),
      fieldOf('charges-pkind', 'Kind of place', kindSel, { cls: 'ch-f-pkind' }),
      h('div', { class: 'field ch-f-search field-search', cover: 'charges.filter.location' },
        h('label', { class: 'field-label', for: 'charges-q' }, 'Find'),
        h('div', { class: 'field-control' }, icon('search', 'icon field-icon'), qIn),
        h('p', { class: 'field-hint' }, 'Matches the place name, street and town.'))),
    h('div', { class: 'ch-filter-row ch-filter-narrow' },
      h('div', { class: 'field', cover: 'charges.filter.charge_type' }, typeChoice, h('p', { class: 'field-hint' }, 'From the charge port’s own signal.')),
      h('div', { class: 'field ch-f-cost', cover: 'charges.filter.min_cost' },
        h('label', { class: 'field-label', for: 'charges-cost' }, 'Cost at least'),
        h('div', { class: 'ch-inline' }, costIn, h('span', { class: 'ch-unit' }, 'in its own currency')),
        h('label', { class: 'ch-check-label', for: 'charges-unpriced' }, unpricedIn, 'Keep charges with no price yet')),
      fieldOf('charges-dur', 'Charging at least', h('div', { class: 'ch-inline' }, durIn, h('span', { class: 'ch-unit' }, 'min')),
        { cover: ['charges.filter.min_duration', 'cs.filter.min_duration'], hint: 'Plugged-in waiting not counted.' }),
      fieldOf('charges-source', 'Source', srcSel, { cls: 'ch-f-small' }),
      fieldOf('charges-flags', 'Show', flagSel, { cls: 'ch-f-small' }),
      h('div', { class: 'ch-clear' }, clearBtn)));

  /* ---------------------------------------------------------------- totals and the list */

  const totals = h('div', { class: 'figs ch-totals' });
  function drawTotals(t) {
    t = t || {};
    const n = t.count || 0;
    const priced = n - (t.unpriced || 0);
    const why = filtered() ? 'no charge matches' : 'no charge';
    replace(totals,
      h('div', { class: 'fig ch-totals-caption' }, h('p', { class: 'eyebrow' }, filtered() ? 'The charges listed' : 'For the period'), h('p', { class: 'ch-totals-span' }, periodLabel(ctx.period, tz, api.now()))),
      figure('Sessions', formatNumber(n), n ? `${formatNumber(priced)} with a price, ${formatNumber(t.unpriced || 0)} without` : filtered() ? 'that match the filters' : 'in the period'),
      figure('Energy added', q(n ? t.energy_added_kwh : null, 'energy', { why }), n ? `into the pack, over ${tn(n, '{n} session', '{n} sessions')}` : null, { cover: 'charges.stat.energy_added' }),
      figure('From the grid', q(n && t.energy_grid_known ? t.energy_grid_kwh : null, 'energy', { why: n ? 'no AC charge was metered' : why }),
        n ? `AC, metered, on ${formatNumber(t.energy_grid_known || 0)} of ${tn(n, '{n} session', '{n} sessions')}${t.dc_inlet_kwh ? `; DC at the inlet ${format(t.dc_inlet_kwh, 'energy')}` : ''}` : null, { cover: 'charges.stat.energy_used' }),
      figure('Cost', n && (t.cost_by_currency || []).length ? moneyList(t.cost_by_currency) : q(null, 'money', { why: n ? 'no charge has a price' : why }),
        n ? `per currency, never added across; ${formatNumber(priced)} of ${tn(n, '{n} session', '{n} sessions')} priced` : null, { cover: 'charges.stat.cost' }),
      figure('Average charging time', n ? formatDuration(t.avg_duration_ms || 0) : q(null, 'duration', { why }), n ? `per session, over ${tn(n, '{n} session', '{n} sessions')}; plugged-in waiting left out` : null, { cover: 'charges.stat.avg_duration' }));
  }

  const kindCell = (c) => h('span', null,
    h('span', { class: ['ch-kind', `ch-kind-${c.kind || 'unknown'}`], title: KIND_WHY[c.kind || 'unknown'] }, KIND[c.kind] || 'Unknown'),
    c.fast_charger ? (() => { const t = [c.fast_charger.type === 'supercharger' ? 'Supercharger' : c.fast_charger.brand || 'Fast charger', c.fast_charger.class].filter(Boolean).join(' '); return h('span', { class: 'cell-sub ch-nowrap', title: t }, t); })() : null);
  // The cost, or "—" when unpriced; where the price came from goes in the title. The set-cost icon shows on row
  // hover or focus (always on touch screens), so it does not repeat down the list.
  const costCell = (c) => {
    const text = c.cost ? formatMoney(c.cost.amount, c.cost.currency) : '—';
    // the hover title names where the cost came from (Place price, From Tesla invoice, ...)
    const why = c.cost ? costSourceLabel(c.cost.source) || `Cost from the ${COST_SOURCE[c.cost.source] || c.cost.source}` : c.place ? 'No price: the place has no price' : 'No price: not at a saved place';
    const main = admin
      ? h('button', { type: 'button', class: ['ch-cost-button', { 'is-missing': !c.cost }], title: `${why}. ${c.cost ? 'Change the cost of this charge' : 'Set a cost for this charge'}`, 'aria-label': c.cost ? `${text}, change the cost` : 'No price, set a cost', onclick: () => costDialog(c) }, text, icon('settings'))
      : h('a', { class: ['ch-cost-link', { 'is-missing': !c.cost }], href: state.buildUrl('charge', { car: c.car, charge: c.id, period: ctx.state.period, query: { edit: 'cost' } }), title: `${why}. Open the charge to see how its cost was worked out` }, text);
    return h('span', { cover: 'inv.charges.cost-source' }, main,
      c.price_per_kwh && c.price_per_kwh.value != null
        ? h('span', { class: 'cell-sub ch-ppk ch-nowrap', title: c.price_per_kwh.basis === 'grid' ? 'Per kWh: cost over the energy from the grid (metered)' : 'Per kWh: cost over the energy added to the pack' }, format({ amount: c.price_per_kwh.value, currency: c.cost && c.cost.currency }, 'price_energy'))
        : null);
  };
  // Efficiency under the grid energy; when it is not known, nothing under it and the reason in the cell's title
  // (the server's efficiency_note when it gives one, such as energies measured over different spans).
  const effSub = (c) => (c.efficiency_pct != null
    ? h('span', { class: 'cell-sub', title: c.losses_kwh != null ? `${format(c.losses_kwh, 'energy')} lost between the grid and the pack` : null }, `${format(c.efficiency_pct, 'percent', { digits: 0 })} efficient`)
    : null);
  const effWhy = (c) => (c.efficiency_pct != null ? null
    : c.efficiency_note || (c.kind === 'dc' ? KIND_WHY.dc : 'Efficiency unknown: this charge has no reading from the car’s charger input.'));
  const columns = [
    multi ? { key: 'car', label: 'Car', cls: 'c-car', cell: (c) => carName(c.car), value: (c) => carName(c.car) } : null,
    { key: 'start', label: 'Start', sort: 'start', th: true, cls: 'c-start', firstDir: 'desc', cell: (c) => h('span', null,
      h('a', { class: 'cell-main ch-start', href: state.buildUrl('charge', { car: c.car, charge: c.id, period: ctx.state.period }) },
        `${formatDate(c.start_ms, zoneOf(c), { weekday: false, now: api.now() })}, ${formatTime(c.start_ms, zoneOf(c))}`),
      h('span', { class: 'cell-sub' }, c.end_ms == null ? 'charging now' : `to ${formatTime(c.end_ms, zoneOf(c))}`,
        c.has_gaps ? [' ', flag('Gap', { kind: 'warn', title: `No data for ${formatDuration((c.flags && c.flags.gap_ms) || 0)} inside this charge` })] : null)) },
    { key: 'place', label: 'Place', cls: 'c-place', cell: (c) => {
      const href = placeLink(pointOf(c));
      return h('span', null,
        href ? h('a', { class: 'ch-place', href, title: c.place ? `Edit ${c.place.name} in Places` : 'Save this spot as a place' }, placeName(c)) : h('span', { class: 'ch-place' }, placeName(c)),
        c.place && c.address && c.address.short ? h('span', { class: 'cell-sub' }, c.address.short) : null);
    } },
    { key: 'kind', label: 'Type', cls: 'c-kind', cell: kindCell },
    { key: 'duration', label: 'Charging', sort: 'duration', num: true, cls: 'c-dur', hint: 'Time charging, then time plugged in', cell: (c) => h('span', null,
      formatDuration(c.charging_ms || 0), h('span', { class: 'cell-sub ch-nowrap', title: c.plugged_ms != null ? `Plugged in ${formatDuration(c.plugged_ms)}` : null }, c.plugged_ms != null ? `plugged ${formatDuration(c.plugged_ms)}` : '')) },
    { key: 'energy', label: 'Added', sort: 'energy', num: true, cls: 'c-added', hint: 'Energy into the pack, and the range it added', cell: (c) => h('span', { title: c.range_per_hour_km != null ? `${format(c.range_per_hour_km, 'range')} of range an hour` : null },
      q(c.energy_added_kwh, 'energy', { why: 'no reading' }),
      h('span', { class: 'cell-sub ch-nowrap' }, c.range_added_km != null ? ['+', format(c.range_added_km, 'range')] : '')) },
    { key: 'grid', label: 'From grid', num: true, cls: 'c-grid', hint: 'AC: metered at the car’s charger input', cell: (c) => h('span', { title: effWhy(c) },
      c.energy_grid_kwh != null ? q(c.energy_grid_kwh, 'energy') : h('span', { class: 'ch-quiet' }, '—'),
      effSub(c)) },
    { key: 'cost', label: 'Cost', sort: 'cost', num: true, cls: 'c-cost', cell: costCell },
    { key: 'power', label: 'Power', sort: 'power_max', num: true, cls: 'c-power', hint: 'Average while charging (waiting left out); the peak is in the cell’s title', cell: (c) => h('span', { title: c.power_max_kw != null ? `Peak ${format(c.power_max_kw, 'power')}` : null },
      q(c.power_avg_kw, 'power')) },
    { key: 'rate', label: 'Per hour', num: true, cls: 'c-rate', hint: 'Range added per hour of charging', cell: (c) => h('span', null,
      q(c.range_per_hour_km, 'range'), h('span', { class: 'cell-sub' }, 'an hour')) },
    { key: 'soc', label: 'Battery', sort: 'end_soc', num: true, cls: 'c-soc', firstDir: 'desc', hint: 'Battery level at the start and the end', cell: (c) => h('span', { class: 'ch-soc' },
      h('span', null, pct(c.start_soc_pct), ' → ', pct(c.end_soc_pct)),
      socBar(c.start_soc_pct, c.end_soc_pct)) },
    { key: 'temp', label: 'Outside', num: true, cls: 'c-temp', hint: 'Average outside temperature; the battery’s is in the cell’s title', cell: (c) => h('span', { title: c.battery_avg_c != null ? `Battery ${format(c.battery_avg_c, 'temperature', { digits: 0 })}` : null },
      q(c.outside_avg_c, 'temperature', { digits: 0 })) },
    { key: 'odo', label: 'Odometer', num: true, cls: 'c-odo', cell: (c) => q(c.odometer_km, 'odometer') },
  ].filter(Boolean);
  const emptyWhy = () => {
    const bits = [];
    if (f.places.length) bits.push(`at ${placePick.summary()}`);
    if (f.place_kind) bits.push(`at a place of the kind “${PLACE_KINDS.find(([k]) => k === f.place_kind)[1]}”`);
    if (f.kind) bits.push(`of type ${KIND[f.kind]}`);
    if (f.q) bits.push(`matching “${f.q}”`);
    if (f.min_cost != null) bits.push(`costing at least ${formatNumber(f.min_cost)}${f.unpriced ? ' (or unpriced)' : ''}`);
    if (f.min_dur != null) bits.push(`charging ${formatNumber(f.min_dur)} min or longer`);
    if (f.source) bits.push(SOURCE_WHY[f.source]);
    if (f.flags) bits.push(f.flags === 'gap' ? 'with missing data' : f.flags === 'in_progress' ? 'under way' : 'with missing data or under way');
    return bits;
  };
  const table = dataTable(ctx, {
    path: listPath, params: listParams(), limit: 50, columns, sort: { key: 'start', dir: 'desc' }, itemName: ['charge', 'charges'],
    cls: 'ch-list', label: 'Charging sessions', cover: ['charges.table', 'charges.table.date', 'charges.link.view_details', 'charges.table.location', 'charges.link.geofence', 'charges.table.type',
      'charges.table.duration', 'charges.table.cost', 'charges.link.set_cost', 'charges.table.cost_per_kwh', 'charges.table.energy_added', 'charges.table.energy_used', 'charges.table.efficiency',
      'charges.table.temp', 'charges.table.avg_power', 'charges.table.charge_rate', 'charges.table.range_added', 'charges.table.start_soc', 'charges.table.end_soc', 'charges.table.odometer'],
    caption: 'Open a charge for its curve and cost. The CSV holds every listed charge.',
    rowAttrs: (c) => ({ class: ['ch-row', { 'is-going': c.end_ms == null }] }),
    rowTitle: (c) => rowTitle(c),
    onRows: (got, page) => { if (page && page.totals && table.items.length === got.length) drawTotals(page.totals); },
    empty: () => {
      const bits = emptyWhy();
      const span = `this period (${periodLabel(ctx.period, tz, api.now())})`;
      return empty({
        title: bits.length ? `No charge in ${span} ${bits.join(', ')}.` : `${scopeName} did not charge in ${span}.`,
        text: bits.length ? 'Loosen a filter or clear the filters; a longer period in the period picker may help too.' : 'Choose a longer period in the period picker at the top.',
        action: bits.length ? button('Clear the filters', { kind: 'primary', onclick: () => clearBtn.click() }) : null,
      });
    },
  });

  // The cost of one charge (admins): a total, per kWh or per minute; or back to the place's price.
  async function costDialog(c) {
    const cur = (c.cost && c.cost.currency) || (byId.get(c.car) && byId.get(c.car).currency) || 'USD';
    const mode = h('select', { class: 'field-select', id: 'cost-mode' }, [['total', 'A total for the session'], ['per_kwh', 'A price per kWh'], ['per_minute', 'A price per minute']].map(([k, t]) => h('option', { value: k }, t)));
    const amount = h('input', { class: 'field-input', type: 'number', id: 'cost-amount', min: '0', step: '0.01', inputmode: 'decimal', value: c.cost && c.cost.source === 'manual' ? String(c.cost.amount) : '' });
    const currency = h('select', { class: 'field-select', id: 'cost-currency' }, [...new Set([cur, ...CURRENCIES])].map((k) => h('option', { value: k, selected: k === cur }, k)));
    const basis = h('select', { class: 'field-select', id: 'cost-basis' }, [['grid', 'Energy from the grid (AC, metered)'], ['pack', 'Energy added to the pack']].map(([k, t]) => h('option', { value: k, selected: k === (c.kind === 'ac' ? 'grid' : 'pack') }, t)));
    const basisField = h('div', { class: 'field', hidden: true }, h('label', { class: 'field-label', for: 'cost-basis' }, 'Per kWh of'), basis);
    mode.addEventListener('change', () => { basisField.hidden = mode.value !== 'per_kwh'; });
    const err = h('p', { class: 'field-error', role: 'alert' });
    const body = h('div', { class: 'ch-cost-form' },
      h('p', { class: 'dialog-text' }, `${placeName(c)}, ${formatDate(c.start_ms, zoneOf(c), { now: api.now() })} ${formatTime(c.start_ms, zoneOf(c))}: ${format(c.energy_added_kwh, 'energy')} added in ${formatDuration(c.charging_ms || 0)}. `,
        c.cost ? `Now ${formatMoney(c.cost.amount, c.cost.currency)} (${COST_SOURCE[c.cost.source] || c.cost.source}).` : 'It has no price yet.'),
      h('div', { class: 'field' }, h('label', { class: 'field-label', for: 'cost-mode' }, 'What you paid'), mode),
      h('div', { class: 'ch-inline ch-cost-amount' },
        h('div', { class: 'field' }, h('label', { class: 'field-label', for: 'cost-amount' }, 'Amount'), amount),
        h('div', { class: 'field' }, h('label', { class: 'field-label', for: 'cost-currency' }, 'Currency'), currency)),
      basisField,
      h('p', { class: 'field-hint' }, 'A cost typed here is kept when the charge is rebuilt from the logs.'), err);
    const save = async () => {
      const n = parseFloat(amount.value);
      if (!Number.isFinite(n) || n < 0) { err.textContent = 'Type an amount of 0 or more.'; return false; }
      try {
        await ctx.api.put(api.apiPath`cars/${c.car}/charges/${c.id}/cost`, { mode: mode.value, amount: n, currency: currency.value, energy_basis: mode.value === 'per_kwh' ? basis.value : undefined });
      } catch (e) { err.textContent = `Not saved: ${e.message}`; return false; }
      ctx.flash('Cost saved.');
      return true;
    };
    const actions = [{ label: 'Cancel', value: null, kind: 'quiet' }];
    if (c.cost && c.cost.source === 'manual') actions.push({ label: 'Use the place price again', value: 'reset', kind: 'quiet', onclick: async () => {
      try { await ctx.api.del(api.apiPath`cars/${c.car}/charges/${c.id}/cost`); ctx.flash('The place price applies again.'); return true; } catch (e) { err.textContent = `Not changed: ${e.message}`; return false; }
    } });
    actions.push({ label: 'Save the cost', value: 'saved', kind: 'primary', onclick: save });
    const v = await openDialog({ title: c.cost ? 'Change the cost' : 'Set a cost', body, actions, signal: ctx.signal, initialFocus: '#cost-amount' });
    if (v && ctx.alive()) reloadAll().catch(() => {});
  }

  /* ---------------------------------------------------------------- charges with missing data */

  const missing = h('section', { class: 'block ch-missing', 'aria-labelledby': 'ch-missing-title', cover: ['charges.incomplete.table', 'charges.incomplete.col.id', 'charges.incomplete.col.start_date', 'charges.incomplete.col.end_date', 'charges.incomplete.col.energy_added', 'charges.incomplete.col.energy_used', 'charges.incomplete.col.start_soc', 'charges.incomplete.col.end_soc', 'charges.incomplete.col.duration'] });
  async function drawMissing() {
    const params = { ...base(), flags: 'gap,in_progress' };
    if (!missing.firstChild) replace(missing, loadingBlock(3));
    let pg;
    try { pg = await ctx.api.page(listPath, params, { limit: 20 }); } catch (err) { if (!ctx.signal.aborted) replace(missing, errorView(err, { eyebrow: 'Charges with missing data', retry: () => drawMissing() })); return; }
    const list = pg.items || [];
    // Where each gap lies, from the charge itself.
    const gaps = new Map();
    await Promise.all(list.filter((c) => c.has_gaps).map(async (c) => {
      try { const d = await ctx.api.get(api.apiPath`cars/${c.car}/charges/${c.id}`); gaps.set(c.id, d.gaps || []); } catch { gaps.set(c.id, null); }
    }));
    if (!ctx.alive()) return;
    const cars = [...new Set(list.map((c) => c.car))];
    const rebuild = admin && list.length ? button(`Rebuild ${cars.length > 1 ? 'these cars’' : 'these'} charges from the stored logs`, {
      iconName: 'reset', cls: 'button-small',
      onclick: async () => {
        if (!(await confirmDialog({ title: 'Rebuild charges?', text: `Sentry Mate reads the stored logs again for this period and rebuilds ${cars.length > 1 ? `the charges, drives, states and stops of ${tn(cars.length, '{n} car', '{n} cars')}` : 'its charges, drives, states and stops'}. Costs and purposes you typed are kept.`, yes: 'Rebuild' }))) return;
        try {
          let logs = 0;
          for (const cid of cars) logs += ((await ctx.api.post(api.apiPath`cars/${cid}/rebuild`, { from_ms: ctx.params.from_ms ?? list[list.length - 1].start_ms, to_ms: ctx.params.to_ms ?? api.now() })) || {}).logs_read || 0;
          ctx.flash(`Rebuilt from ${tn(logs, '{n} stored log', '{n} stored logs')}.`);
          reloadAll().catch(() => {});
        } catch (err) { ctx.flash(`The rebuild did not start: ${err.message}`, { kind: 'error' }); }
      },
    }) : null;
    const where = (c) => {
      const z = zoneOf(c);
      if (c.end_ms == null || (c.flags && c.flags.in_progress)) return 'Under way: the figures grow as logs arrive.';
      const g = gaps.get(c.id);
      if (!g || !g.length) return `${formatDuration((c.flags && c.flags.gap_ms) || 0)} without data inside it.`;
      return g.map((x) => `${{ unknown: 'No data', dropped: 'Frames lost', chain: 'A log missing' }[x.kind] || 'No data'} ${x.end_ms == null ? `from ${formatTime(x.start_ms, z, { seconds: true })} to the end` : `${formatTime(x.start_ms, z, { seconds: true })}–${formatTime(x.end_ms, z, { seconds: true })}`}`).join('; ') + '.';
    };
    const lastSecond = (c) => (c.end_ms == null ? 'still going' : formatTime(c.end_ms, zoneOf(c), { seconds: true }));
    const csvCols = [{ label: 'Car', value: (c) => carName(c.car) }, { label: 'Charge', value: (c) => c.id }, timeColumn('Start', 'start_ms', tz), timeColumn('Last recorded', 'end_ms', tz), { label: 'What is missing', value: where },
      column('Energy added', 'energy_added_kwh', 'energy'), column('Energy from grid', 'energy_grid_kwh', 'energy'), column('Start battery', 'start_soc_pct', 'percent'), column('End battery', 'end_soc_pct', 'percent'), { label: 'Charging (min)', value: (c) => Math.round((c.charging_ms || 0) / 60000) }];
    replace(missing,
      h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'ch-missing-title' }, 'Charges with missing data', h('span', { class: 'label-note' }, list.length ? (pg.next ? `the first ${formatNumber(list.length)}` : tn(list.length, '{n} charge', '{n} charges')) : 'none')),
        h('div', { class: 'block-actions' }, list.length ? csvButton('charges with missing data', () => downloadCsv(csvFilename(scopeName, 'charges missing data', periodLabel(ctx.period, tz, api.now())), list, csvCols)) : null, rebuild)),
      h('p', { class: 'note ch-missing-note', cover: 'charges.text.incomplete_help' }, 'A charge is listed here when part of it has no data (no log arrived, frames were lost, a log failed its check or is missing from the chain) or when it is still under way. Its figures cover only the time with data; a late log or a rebuild from the stored logs fills them in. Nothing needs repairing by hand.'),
      list.length ? h('div', { class: 'ledger-scroll' }, h('table', { class: 'ledger-table ch-missing-table' },
        h('thead', null, h('tr', null, [multi ? 'Car' : null, 'Charge', 'Start', 'Last recorded', 'Where data is missing', 'Added', 'From grid', 'Battery', 'Charging'].filter(Boolean).map((t, i, a) => h('th', { scope: 'col', class: { num: i >= a.length - 4 } }, t)))),
        h('tbody', null, list.map((c) => h('tr', null,
          multi ? h('td', null, carName(c.car)) : null,
          h('th', { scope: 'row' }, h('a', { class: 'cell-main', href: state.buildUrl('charge', { car: c.car, charge: c.id, period: ctx.state.period }) }, placeName(c)), h('span', { class: 'cell-sub mono' }, `Charge ${c.id}`)),
          h('td', null, `${formatDate(c.start_ms, zoneOf(c), { weekday: false, now: api.now() })}, ${formatTime(c.start_ms, zoneOf(c))}`),
          h('td', null, lastSecond(c)),
          h('td', { class: 'ch-where' }, where(c)),
          h('td', { class: 'num' }, q(c.energy_added_kwh, 'energy')),
          h('td', { class: 'num' }, c.energy_grid_kwh != null ? q(c.energy_grid_kwh, 'energy') : h('span', { class: 'ch-quiet' }, c.kind === 'dc' ? 'not metered (DC)' : '—')),
          h('td', { class: 'num' }, pct(c.start_soc_pct), ' → ', pct(c.end_soc_pct)),
          h('td', { class: 'num' }, formatDuration(c.charging_ms || 0)))))))
        : h('p', { class: 'note-serif' }, 'Every charge in this period has all its data, and none is under way.'));
  }

  /* ---------------------------------------------------------------- statistics */

  const statsNote = h('p', { class: 'note ch-stats-note' });
  const costsBox = h('section', { class: 'block ch-costs', 'aria-labelledby': 'ch-costs-title' });
  const splitBox = h('section', { class: 'block ch-split', 'aria-labelledby': 'ch-split-title' });
  const habitsBox = h('section', { class: 'block ch-habits', 'aria-labelledby': 'ch-habits-title' });
  const placesBox = h('section', { class: 'block ch-places', 'aria-labelledby': 'ch-places-title' });
  const curveBox = h('section', { class: 'block ch-curve', 'aria-labelledby': 'ch-curve-title' });
  const statsHead = h('header', { class: 'ch-stats-head' }, h('h2', { class: 'ch-stats-title' }, 'Charging statistics'), statsNote);

  let chemistry = null;
  async function loadChemistry() {
    if (chemistry) return chemistry;
    chemistry = new Map();
    await Promise.all(viewCars.slice(0, 12).map(async (cid) => {
      try { const s = await ctx.api.get(api.apiPath`cars/${cid}/settings`); const b = s && s.battery_chemistry; chemistry.set(cid, b ? (b.setting && b.setting !== 'auto' ? b.setting : b.detected) : null); } catch { chemistry.set(cid, null); }
    }));
    return chemistry;
  }

  let statsGen = 0, statsKey = '';
  async function loadStats() {
    const my = ++statsGen;
    const sp = statsParams();
    statsKey = JSON.stringify(sp);
    const left = [f.places.length ? `at ${placePick.summary()}` : null, f.min_dur != null ? `charging ${formatNumber(f.min_dur)} min or longer` : null].filter(Boolean);
    statsNote.textContent = `For ${scopeName}, ${periodLabel(ctx.period, tz, api.now())}${left.length ? `, charges ${left.join(' and ')}` : ''}. Of the filters above, only the period, the cars, the places and the shortest charging time apply here; every chart follows the period.`;
    for (const b of [costsBox, splitBox, habitsBox]) if (!b.firstChild) replace(b, loadingBlock(3));
    let s, rows;
    try {
      [s, rows] = await Promise.all([ctx.api.get(statsPath, sp), ctx.api.list(listPath, sp, { limit: 500 }).all({ max: 5000 }), loadChemistry()]);
    } catch (err) {
      if (my !== statsGen || ctx.signal.aborted) return;
      for (const b of [costsBox, splitBox, habitsBox]) replace(b, errorView(err, { eyebrow: 'Charging statistics', retry: () => loadStats() }));
      return;
    }
    if (my !== statsGen || !ctx.alive()) return;
    drawCosts(s);
    drawSplit(s);
    drawHabits(s, rows);
    await Promise.all([drawPlaces(sp, my), drawCurve(rows, my)]);
  }

  const blockHead = (idName, text, note, ...actions) => h('div', { class: 'block-head' },
    h('h2', { class: 'label', id: idName }, text, note ? h('span', { class: 'label-note' }, note) : null),
    actions.length ? h('div', { class: 'block-actions' }, actions) : null);
  const file = (what) => csvFilename(scopeName, what, periodLabel(ctx.period, tz, api.now()));

  function drawCosts(s) {
    const ppk = s.price_per_kwh || {};
    const c100 = s.cost_per_100km || {};
    const c100list = (c100.value || []).filter((x) => x.amount != null);
    const c100text = c100list.length ? c100list.map((x) => format({ amount: x.amount, currency: x.currency }, 'money_per_distance')).join(' + ') : null;
    const c100node = c100list.length ? c100list.map((x, i) => { const pp = parts({ amount: x.amount, currency: x.currency }, 'money_per_distance'); const q1 = h('span', { class: 'q' }, pp.number, mixed(c100list) ? ` ${x.currency}` : ''); return i ? [document.createTextNode(' + '), q1] : q1; }).flat() : null;
    const row = {
      count: s.count, energy: s.energy_added_kwh, total: moneyList(s.cost_by_currency), sc: moneyList(s.supercharger_cost), all: priceList(ppk.all), ac: priceList(ppk.ac), dc: priceList(ppk.dc),
      basis: ppk.basis, priced: ppk.priced, c100: c100text || '', method: c100.method, dist: c100.distance_km,
    };
    const priced = ppk.priced ?? 0;
    replace(costsBox,
      blockHead('ch-costs-title', 'Costs and totals', null, csvButton('costs and totals', () => downloadCsv(file('charging costs'), [row], [
        column('Sessions', 'count'), column('Energy added', 'energy', 'energy'), column('Total cost', 'total'), column('Supercharger cost', 'sc'), column('Price per kWh', 'all'), column('AC price per kWh', 'ac'),
        column('DC price per kWh', 'dc'), column('Price basis', 'basis'), column('Priced sessions', 'priced'), column(`Cost per 100 ${unitLabel('distance')}`, 'c100'), column('Method', 'method'), column('Distance driven', 'dist', 'distance')]))),
      h('div', { class: 'figs ch-cost-figs' },
        figure('Sessions', formatNumber(s.count || 0), `${formatNumber(priced)} with a price`, { cover: 'cs.stat.count' }),
        figure('Energy added', q(s.count ? s.energy_added_kwh : null, 'energy', { why: 'no charge' }), 'into the pack', { cover: 'cs.stat.energy_added' }),
        figure('Total cost', (s.cost_by_currency || []).length ? moneyList(s.cost_by_currency) : q(null, 'money', { why: 'no charge has a price' }), 'per currency, never added across', { cover: 'cs.stat.total_cost' }),
        figure('Supercharger cost', (s.supercharger_cost || []).length ? moneyList(s.supercharger_cost) : q(null, 'money', { why: 'no priced Supercharger session' }), 'sessions the charger itself names as a Supercharger', { cover: 'cs.stat.supercharger_cost' }),
        figure('Price per kWh', priceList(ppk.all) || q(null, 'money', { why: 'no priced charge' }), 'every priced session: cost over grid energy (AC) or billed energy (DC)', { cover: 'cs.stat.cost_per_kwh' }),
        figure('AC price per kWh', priceList(ppk.ac) || q(null, 'money', { why: 'no priced AC charge' }), 'cost over the grid energy, metered', { cover: 'cs.stat.cost_per_kwh_ac' }),
        figure('DC price per kWh', priceList(ppk.dc) || q(null, 'money', { why: 'no priced DC charge' }), 'cost over the energy the charger billed', { cover: 'cs.stat.cost_per_kwh_dc' }),
        figure(`Cost per 100 ${unitLabel('distance')}`, c100node || q(null, 'money', { why: c100.distance_km ? 'no charge has a price' : 'no driving in the period' }),
          `${c100.method || 'charging cost over the distance driven'}${c100.distance_km != null ? `, ${format(c100.distance_km, 'distance')} driven` : ''}`, { cover: 'cs.stat.cost_per_100' })),
      h('p', { class: 'note ch-after-figs' }, 'Charges with no price are left out of the prices, not counted as free. A price comes from the place’s tariff, a cost typed on the charge, or imported history; free Supercharging counts as zero.'));
  }

  function drawSplit(s) {
    const bk = s.by_kind || {};
    const kinds = ['ac', 'dc', 'unknown'];
    const bar = (k, label, cover) => {
      const total = kinds.reduce((a, x) => a + ((bk[x] && bk[x][k]) || 0), 0);
      const val = (x) => (bk[x] && bk[x][k]) || 0;
      const text = (v) => (k === 'kwh' ? format(v, 'energy') : formatDuration(v));
      let at = 0;
      return h('div', { class: 'ch-split-part', cover },
        h('div', { class: 'ch-split-head' }, h('p', { class: 'ch-split-name' }, label), h('p', { class: 'ch-split-total' }, total ? text(total) : q(null, k === 'kwh' ? 'energy' : 'duration', { why: 'no charge' }))),
        svg('svg', { class: 'ch-split-svg', width: '100%', height: '100%', preserveAspectRatio: 'none', role: 'img', 'aria-label': `${label}: ${kinds.map((x) => `${KIND[x]} ${text(val(x))}`).join(', ')}` },
          svg('rect', { class: 'ch-split-track', x: 0, y: 0, width: '100%', height: '100%' }),
          total ? kinds.map((x) => { const w = (val(x) / total) * 100; const r = svg('rect', { class: `ch-fill-${x}`, x: p3(at), y: 0, width: p3(w), height: '100%' }); at += w; return r; }) : null),
        h('ul', { class: 'ch-split-key' }, kinds.map((x) => h('li', null,
          h('span', { class: ['ch-swatch', `ch-swatch-${x}`], 'aria-hidden': 'true' }), h('span', { class: 'ch-split-kind', title: KIND_WHY[x] }, KIND[x]),
          h('span', { class: 'ch-split-val' }, text(val(x))), h('span', { class: 'ch-split-share' }, total ? format((val(x) / total) * 100, 'percent', { digits: 0 }) : '—')))));
    };
    replace(splitBox,
      blockHead('ch-split-title', 'AC and DC', null, csvButton('AC and DC', () => downloadCsv(file('charging ac dc'), kinds.map((x) => ({ kind: KIND[x], kwh: val2(bk, x, 'kwh'), min: Math.round(val2(bk, x, 'ms') / 60000) })),
        [column('Type', 'kind'), column('Energy added', 'kwh', 'energy'), column('Charging time (min)', 'min')]))),
      h('div', { class: 'ch-split-grid' }, bar('kwh', 'Energy added', 'cs.chart.ac_dc_energy'), bar('ms', 'Time charging', 'cs.chart.ac_dc_duration')),
      h('p', { class: 'note ch-after-figs' }, 'From the charge port’s own AC or DC signal. A session without one is Unknown, never DC. Time charging leaves out plugged-in waiting.'));
  }
  const val2 = (bk, x, k) => (bk[x] && bk[x][k]) || 0;

  /* charging habits: levels over time, each session's band, start and end bands, the week, runs */
  function drawHabits(s, rowsIn) {
    const rows = rowsIn.filter((c) => c.start_soc_pct != null).sort((a, b) => a.start_ms - b.start_ms);
    const lfp = viewCars.filter((cid) => chemistry.get(cid) === 'lfp');
    const allLfp = viewCars.length > 0 && lfp.length === viewCars.length;
    const someLfp = lfp.length > 0 && !allLfp;
    const upper = allLfp ? 100 : 80;
    const bandText = allLfp ? '20 % to 100 %: an LFP pack is happy to charge full' : someLfp ? `20 % to 80 % for NCA or NMC packs; ${lfp.map(carName).join(', ')} ${lfp.length === 1 ? 'has' : 'have'} an LFP pack, which may go to 100 %` : '20 % to 80 %, the usual daily band for an NCA or NMC pack';
    const now = api.now();
    const from = ctx.params.from_ms ?? (rows[0] ? rows[0].start_ms : now - 30 * 86400000);
    const to = Math.min(ctx.params.to_ms ?? now, now + 60000);
    const spanDays = (to - from) / 86400000;
    const unit = spanDays <= 62 ? 'day' : spanDays <= 730 ? 'week' : 'month';
    let cols = [];
    try { cols = buckets(from, to, unit, tz, { max: 3000 }); } catch { cols = []; }
    const colIndex = new Map(cols.map((b, i) => [b.key, i]));
    const xOf = (ms) => clamp(((ms - from) / Math.max(1, to - from)) * 100, 0, 100);
    const colLabel = (b, long = false) => (unit === 'month' ? formatDate(b.start_ms + 86400000, tz, { weekday: false, year: long ? true : 'auto' }).replace(/\s?\d{1,2},?(?=\s|$)/, '')
      : `${unit === 'week' && long ? 'Week of ' : ''}${formatDate(b.start_ms + 3600000, tz, { weekday: long && unit === 'day', year: false })}`);

    // The level map: columns are days, weeks or months; rows are 10 % bands.
    const grid = (side) => {
      const g = Array.from({ length: 10 }, () => new Array(cols.length).fill(0));
      for (const c of rows) {
        const v = side === 'start' ? c.start_soc_pct : c.end_soc_pct;
        const t = side === 'start' ? c.start_ms : c.end_ms ?? c.start_ms;
        if (v == null) continue;
        const i = colIndex.get(bucketKey(t, unit, tz));
        if (i != null) g[clamp(Math.floor(v / 10), 0, 9)][i]++;
      }
      return g;
    };
    let cur = grid(f.heat);
    const levelMap = h('div', { class: 'ch-levels', role: 'img', vars: { '--n': String(Math.max(1, cols.length)) } });
    const levelAxis = h('div', { class: 'ch-levels-axis', 'aria-hidden': 'true' });
    const readout = h('p', { class: 'readout ch-readout', 'aria-live': 'polite' });
    const idle = () => replace(readout, h('span', { class: 'readout-idle' }, rows.length ? `${tn(rows.length, '{n} session', '{n} sessions')}. Point at either chart: the other marks the same ${unit}.` : 'No session in the period.'));
    function drawLevels() {
      cur = grid(f.heat);
      const max = Math.max(1, ...cur.flat());
      levelMap.setAttribute('aria-label', `How many sessions ${f.heat === 'start' ? 'started' : 'ended'} in each 10 % band of battery, by ${unit}`);
      const cells = [];
      for (let r = 9; r >= 0; r--) {
        cells.push(h('span', { class: 'ch-levels-y' }, `${r * 10}–${r * 10 + 10} %`));
        for (let i = 0; i < cols.length; i++) {
          const n = cur[r][i];
          cells.push(h('span', { class: ['ch-cell', { 'has-n': n, 'is-end': f.heat === 'end', 'is-over': r * 10 >= upper, 'is-under': r < 2 }], dataset: { col: i }, vars: { '--a': (n / max).toFixed(3) } }));
        }
      }
      replace(levelMap, cells);
      const step = Math.max(1, Math.ceil(cols.length / 8));
      replace(levelAxis, cols.map((b, i) => (i % step === 0 ? h('span', { class: 'ch-levels-x', vars: { '--c': String(i + 2), '--s': String(Math.min(step, cols.length - i)) } }, colLabel(b)) : null)));
    }
    let marked = null;
    function mark(i, from_ = 'levels') {
      for (const el of levelMap.querySelectorAll('.is-marked')) el.classList.remove('is-marked');
      if (i == null || !cols[i]) { marked = null; deltaMark.setAttribute('width', '0'); idle(); return; }
      marked = i;
      for (const el of levelMap.querySelectorAll(`[data-col="${i}"]`)) el.classList.add('is-marked');
      const b = cols[i];
      const a = xOf(b.start_ms), z = xOf(b.end_ms);
      deltaMark.setAttribute('x', p3(a)); deltaMark.setAttribute('width', p3(Math.max(0.15, z - a)));
      if (from_ === 'levels') {
        const n = cur.reduce((acc, r) => acc + r[i], 0);
        const where = cur.map((r, k) => (r[i] ? `${r[i]} at ${k * 10}–${k * 10 + 10} %` : null)).filter(Boolean).reverse();
        replace(readout, h('span', { class: 'readout-time' }, colLabel(b, true)), ' ', h('strong', null, tn(n, '{n} session', '{n} sessions')),
          n ? ` ${f.heat === 'start' ? 'started' : 'ended'}: ${where.join(', ')}` : ` ${f.heat === 'start' ? 'started' : 'ended'}`);
      }
    }
    levelMap.addEventListener('mousemove', (e) => { const c = e.target.closest('[data-col]'); if (c && +c.dataset.col !== marked) mark(+c.dataset.col); });
    levelMap.addEventListener('mouseleave', () => { mark(null); hoverDelta(null); });
    const sideChoice = choice({ label: 'Battery level at', name: 'charges-heat', value: f.heat, options: [['start', 'The start'], ['end', 'The end']], onchange: (v) => { f.heat = v; ctx.setQuery({ heat: v === 'end' ? 'end' : null }); drawLevels(); drawWeek(); sideTitles(); } });
    const levelTitle = h('figcaption', { class: 'label' });
    const weekTitle = h('h3', { class: 'label' });
    const sideTitles = () => {
      levelTitle.textContent = `Battery level at the ${f.heat === 'end' ? 'end' : 'start'} of each session, by ${unit}`;
      weekTitle.textContent = `When sessions ${f.heat === 'end' ? 'end' : 'start'} in the week`;
    };
    sideTitles();

    // Each session from its start level to its end level, on the same time axis.
    const deltaMark = svg('rect', { class: 'ch-delta-mark', x: 0, y: 0, width: 0, height: '100%' });
    const deltaHover = svg('g', { class: 'ch-delta-hover' });
    const kindRows = rows.map((c, i) => ({ c, i, x: xOf(c.start_ms) }));
    const bw = clamp(60 / Math.max(1, rows.length), 0.18, 1.2);
    const delta = svg('svg', { class: 'ch-delta-svg', width: '100%', height: '100%', role: 'img', 'aria-label': `Each session’s battery band from start to end, against the daily band of ${bandText}` },
      svg('rect', { class: 'ch-band', x: 0, y: p3(100 - upper), width: '100%', height: p3(upper - 20) }),
      [0, 20, 40, 60, 80, 100].map((v) => svg('line', { class: ['ch-grid', { 'is-band': v === 20 || v === upper }], x1: 0, x2: '100%', y1: p3(100 - v), y2: p3(100 - v) })),
      someLfp ? svg('line', { class: 'ch-grid is-band is-lfp', x1: 0, x2: '100%', y1: '0%', y2: '0%' }) : null,
      deltaMark,
      kindRows.map(({ c, x }) => svg('rect', { class: `ch-fill-${c.kind || 'unknown'} ch-delta-bar`, x: p3(clamp(x - bw / 2, 0, 100 - bw)), width: p3(bw),
        y: p3(100 - Math.max(c.start_soc_pct, c.end_soc_pct ?? c.start_soc_pct)), height: p3(Math.max(0.6, Math.abs((c.end_soc_pct ?? c.start_soc_pct) - c.start_soc_pct))) })),
      deltaHover);
    const deltaY = h('div', { class: 'ch-delta-y', 'aria-hidden': 'true' }, [0, 20, 40, 60, 80, 100].map((v) => h('span', { vars: { '--y': p3(100 - v) } }, `${v} %`)));
    const deltaX = h('div', { class: 'ch-delta-x', 'aria-hidden': 'true' }, (() => {
      const step = Math.max(1, Math.ceil(cols.length / 8));
      return cols.map((b, i) => { if (i % step) return null; const x = xOf(b.start_ms); return h('span', { class: { 'is-end': x > 90 }, vars: { '--x': p3(x) } }, colLabel(b)); });
    })());
    let hovered = null;
    function hoverDelta(k) {
      hovered = k;
      replace(deltaHover);
      if (k == null) return;
      const { c, x } = kindRows[k];
      deltaHover.append(svg('line', { class: 'ch-delta-cross', x1: p3(x), x2: p3(x), y1: 0, y2: '100%' }));
    }
    const deltaWrap = h('div', { class: 'ch-delta-plot' }, delta);
    deltaWrap.addEventListener('mousemove', (e) => {
      if (!kindRows.length) return;
      const r = deltaWrap.getBoundingClientRect();
      const x = ((e.clientX - r.left) / Math.max(1, r.width)) * 100;
      let best = 0, bd = Infinity;
      for (const k of kindRows) { const d = Math.abs(k.x - x); if (d < bd) { bd = d; best = k.i; } }
      if (best === hovered) return;
      hoverDelta(best);
      const c = rows[best];
      const i = colIndex.get(bucketKey(c.start_ms, unit, tz));
      mark(i ?? null, 'delta');
      replace(readout, h('span', { class: 'readout-time' }, `${multi ? `${carName(c.car)}, ` : ''}${formatDate(c.start_ms, zoneOf(c), { weekday: false, now: api.now() })}, ${formatTime(c.start_ms, zoneOf(c))}`), ' ',
        h('strong', null, pct(c.start_soc_pct), ' → ', pct(c.end_soc_pct)), ` · ${KIND[c.kind] || 'Unknown'} at ${placeName(c)}. Click to open it.`);
    });
    deltaWrap.addEventListener('mouseleave', () => { hoverDelta(null); mark(null); });
    deltaWrap.addEventListener('click', () => { if (hovered != null && rows[hovered]) ctx.go(state.buildUrl('charge', { car: rows[hovered].car, charge: rows[hovered].id, period: ctx.state.period })); });

    // When in the week: weekday by hour, in local time.
    const week = h('div', { class: 'ch-week', role: 'img' });
    function drawWeek() {
      const g = (s.heatmap && s.heatmap[f.heat]) || [];
      const max = Math.max(1, ...g.flat());
      week.setAttribute('aria-label', `When in the week sessions ${f.heat}, by weekday and hour`);
      const cells = [h('span')];
      // one label per six hours, over the six columns it names
      for (let hr = 0; hr < 24; hr += 6) cells.push(h('span', { class: 'ch-week-x' }, formatTime(Date.UTC(2026, 0, 5, hr), 'UTC')));
      for (let d = 0; d < 7; d++) {
        cells.push(h('span', { class: 'ch-week-y' }, DAYS[d].slice(0, 3)));
        for (let hr = 0; hr < 24; hr++) {
          const n = (g[d] && g[d][hr]) || 0;
          cells.push(h('span', { class: ['ch-cell', { 'has-n': n, 'is-end': f.heat === 'end' }], vars: { '--a': (n / max).toFixed(3) }, title: `${DAYS[d]} ${String(hr).padStart(2, '0')}:00–${String(hr + 1).padStart(2, '0')}:00: ${tn(n, '{n} session', '{n} sessions')}` }));
        }
      }
      replace(week, cells);
    }

    const bandList = (list, side) => {
      const max = Math.max(1, ...(list || []).map((b) => b.count || 0));
      return h('ul', { class: 'ch-bands' }, (list || []).slice().reverse().map((b) => {
        const lo = parseInt(b.band, 10);
        const tone = side === 'end' ? (lo >= upper ? 'is-high' : null) : lo < 10 ? 'is-low' : lo < 20 ? 'is-high' : null;
        return h('li', { class: ['ch-band-row', tone] },
          h('span', { class: 'ch-band-name' }, `${String(b.band).replace('-', '–')} %`),
          svg('svg', { class: 'ch-band-svg', width: '100%', height: '100%', preserveAspectRatio: 'none', 'aria-hidden': 'true' }, svg('rect', { class: 'ch-band-fill', x: 0, y: 0, width: p3(((b.count || 0) / max) * 100), height: '100%' })),
          h('span', { class: 'ch-band-count' }, tn(b.count || 0, '{n} session', '{n} sessions')));
      }));
    };
    const bandCols = [{ label: 'Band (%)', value: (b) => b.band }, column('Sessions', 'count')];
    const allRuns = [...(s.runs || [])].sort((x, y) => y.start_ms - x.start_ms);
    const runs = dataTable(ctx, {
      rows: allRuns.slice(0, 8), itemName: ['run shown', 'runs shown'], label: 'Runs of back-to-back charges', cls: 'ch-runs', sort: { key: 'start', dir: 'desc' },
      csv: false,
      columns: [
        multi ? { key: 'car', label: 'Car', sort: true, value: (r) => carName(r.car), cell: (r) => carName(r.car) } : null,
        { key: 'start', label: 'First start', sort: true, th: true, value: (r) => r.start_ms, cell: (r) => `${formatDate(r.start_ms, zoneOf(byId.get(r.car)), { weekday: false, now: api.now() })}, ${formatTime(r.start_ms, zoneOf(byId.get(r.car)))}` },
        { key: 'charges', label: 'Charges', num: true, sort: true, cell: (r) => formatNumber(r.charges) },
        { key: 'min_start_soc_pct', label: 'Lowest start', num: true, sort: true, cell: (r) => pct(r.min_start_soc_pct) },
        { key: 'max_end_soc_pct', label: 'Highest end', num: true, sort: true, cell: (r) => pct(r.max_end_soc_pct) },
      ].filter(Boolean),
      empty: () => empty({ title: 'No back-to-back charges in this period.', text: 'Every charge stood on its own: none started within a day of the one before.' }),
    });
    const levelCsv = () => downloadCsv(file(`charging levels by ${unit}`), cols.map((b, i) => { const o = { key: b.key }; for (let r = 0; r < 10; r++) o[`b${r}`] = cur[r][i]; return o; }),
      [{ label: unit[0].toUpperCase() + unit.slice(1), value: (o) => o.key }, ...Array.from({ length: 10 }, (_, r) => ({ label: `${f.heat === 'start' ? 'Started' : 'Ended'} at ${r * 10}-${r * 10 + 10} %`, value: (o) => o[`b${r}`] }))]);
    const deltaCsv = () => downloadCsv(file('charging start to end'), rows, [{ label: 'Car', value: (c) => carName(c.car) }, { label: 'Charge', value: (c) => c.id }, timeColumn('Start', 'start_ms', tz),
      { label: 'Type', value: (c) => KIND[c.kind] || 'Unknown' }, column('Start battery', 'start_soc_pct', 'percent'), column('End battery', 'end_soc_pct', 'percent'), { label: 'Place', value: placeName }]);
    const weekCsv = () => downloadCsv(file(`charging ${f.heat} by hour`), ((s.heatmap && s.heatmap[f.heat]) || []).map((r, d) => ({ day: DAYS[d], r })),
      [{ label: 'Day', value: (o) => o.day }, ...Array.from({ length: 24 }, (_, hr) => ({ label: `${String(hr).padStart(2, '0')}:00`, value: (o) => o.r[hr] || 0 }))]);

    replace(habitsBox,
      blockHead('ch-habits-title', 'Charging habits', `follows the period: ${periodLabel(ctx.period, tz, api.now())}`),
      h('div', { class: 'ch-habits-top' },
        h('div', { class: 'figs ch-habit-figs' },
          figure('Ended above 80 %', q(s.count ? s.share_above_80_pct : null, 'percent', { why: 'no charge' }), allLfp ? 'of the sessions; fine for an LFP pack' : 'of the sessions; best kept for trips with an NCA or NMC pack'),
          figure('Started below 20 %', q(s.count ? s.share_below_20_pct : null, 'percent', { why: 'no charge' }), 'of the sessions'),
          figure('Daily band', allLfp ? '20–100 %' : '20–80 %', bandText)),
        h('div', { class: 'ch-habit-controls' },
          h('div', { class: 'field', cover: ['cs.filter.heatmap_window', 'cs.filter.charge_delta_window'] },
            h('span', { class: 'field-label' }, 'Window'),
            h('p', { class: 'ch-window' }, periodLabel(ctx.period, tz, api.now()), ', by ', unit),
            button('Change the period', { cls: 'button-small', iconName: 'calendar', onclick: () => document.querySelector('.period-trigger')?.click() }),
            h('p', { class: 'field-hint' }, 'Both charts and every list here follow the page’s period; there is no separate window to keep in step.')),
          h('div', { class: 'field' }, sideChoice, h('p', { class: 'field-hint' }, 'For the level map and the week.')))),
      h('div', { class: 'ch-pair', cover: 'cs.control.shared_crosshair' },
        readout,
        h('figure', { class: 'ch-part', cover: 'cs.chart.heatmap' },
          h('div', { class: 'ch-part-head' }, levelTitle, csvButton(`battery levels by ${unit}`, levelCsv)),
          h('div', { class: 'ch-levels-wrap' }, levelMap, levelAxis),
          h('p', { class: 'note' }, `Darker means more sessions in that band and ${unit}. The bands below 20 % and above ${upper} % are outside the daily band.`)),
        h('figure', { class: 'ch-part', cover: ['cs.chart.charge_delta', 'cs.chart.charge_delta.band'] },
          h('div', { class: 'ch-part-head' }, h('figcaption', { class: 'label' }, 'Each session, from its start level to its end level'), csvButton('each session from start to end', deltaCsv)),
          h('ul', { class: 'ch-key' }, ['ac', 'dc', 'unknown'].map((k) => h('li', null, h('span', { class: ['ch-swatch', `ch-swatch-${k}`], 'aria-hidden': 'true' }), KIND[k])),
            h('li', null, h('span', { class: 'ch-swatch ch-swatch-band', 'aria-hidden': 'true' }), `daily band, ${bandText}`)),
          h('div', { class: 'ch-delta' }, deltaY, deltaWrap, deltaX),
          rows.length ? null : h('p', { class: 'note-serif' }, 'No session in the period, so there is nothing to draw.'))),
      h('div', { class: 'ch-habit-lists' },
        h('div', { class: 'ch-part', cover: ['cs.table.discharge_stats', 'cs.table.discharge_stats.soc', 'cs.table.discharge_stats.count'] },
          h('div', { class: 'ch-part-head' }, h('h3', { class: 'label' }, 'Level when plugging in'), csvButton('level when plugging in', () => downloadCsv(file('charging start bands'), s.start_bands || [], bandCols))),
          bandList(s.start_bands, 'start'),
          h('p', { class: 'note', cover: 'cs.text.discharge_stats_help' }, 'How low the battery was when each session started: how far the car is run down before charging. Each session counts once, at its starting level. Under 10 % is red, under 20 % amber.')),
        h('div', { class: 'ch-part', cover: ['cs.table.charge_stats', 'cs.table.charge_stats.soc', 'cs.table.charge_stats.count'] },
          h('div', { class: 'ch-part-head' }, h('h3', { class: 'label' }, 'Level when charging stops'), csvButton('level when charging stops', () => downloadCsv(file('charging end bands'), s.end_bands || [], bandCols))),
          bandList(s.end_bands, 'end'),
          h('p', { class: 'note' }, `Where sessions ended. Amber bands are above the daily band (${upper} %).`)),
        h('div', { class: 'ch-part' },
          h('div', { class: 'ch-part-head' }, weekTitle, csvButton('when in the week', weekCsv)),
          week,
          h('p', { class: 'note' }, multi ? 'By the hour, in each car’s own local time.' : `By the hour, in ${zoneLabel(tz)}.`))),
      h('div', { class: 'ch-part ch-runs-part' },
        h('div', { class: 'ch-part-head' }, h('h3', { class: 'label' }, 'Back-to-back charging', h('span', { class: 'label-note' }, tn(allRuns.length, '{n} run', '{n} runs'))), allRuns.length ? csvButton('back-to-back charging', () => downloadCsv(file('back to back charges'), allRuns, [{ label: 'Car', value: (r) => carName(r.car) }, timeColumn('First start', 'start_ms', tz), column('Charges', 'charges'), column('Lowest start', 'min_start_soc_pct', 'percent'), column('Highest end', 'max_end_soc_pct', 'percent')])) : null),
        h('p', { class: 'note' }, 'Two or more charges, each starting within a day of the one before: top-ups that belong together, so one long charge split in parts is not read as a habit.'),
        runs.el,
        allRuns.length > 8 ? h('div', { class: 'ledger-foot' }, button(`Show all ${formatNumber(allRuns.length)} runs`, { onclick: (e) => { runs.setRows(allRuns); e.currentTarget.parentElement.remove(); } })) : null));
    runs.load();
    drawLevels();
    drawWeek();
    idle();
  }

  /* charging places: a map and two paged tables */
  let placesUi = null;
  async function drawPlaces(sp, my) {
    const pp = placesQuery(sp, fixedCar ? fixedCar.id : sp.cars);
    const placeCell = (p) => {
      const href = placeLink({ place: p.place ? { id: p.place } : null, lat: p.lat, lon: p.lon });
      return h('span', null, href ? h('a', { class: 'ch-place', href, title: p.place ? `Edit ${p.label} in Places` : 'Save this spot as a place' }, p.label) : p.label,
        multi && p.cars && p.cars.length ? h('span', { class: 'cell-sub' }, p.cars.map(carName).join(', ')) : null);
    };
    const sessions = { key: 'sessions', label: 'Sessions', num: true, cell: (p) => formatNumber(p.sessions) };
    // an admin sets the place's price from here; a spot that is no saved place is saved first
    const setCol = { key: 'set_price', label: '', cell: (p) => (admin ? h('button', { type: 'button', class: 'button button-quiet button-small set-price',
      title: p.place ? `Set the price at ${p.label}` : 'Save this spot as a place, then set its price',
      onclick: async () => {
        try {
          const saved = await setPrice({ client: api, placeId: p.place, spot: p.place ? null : { name: p.label, lat: p.lat, lon: p.lon }, cls: PRICE_CLS });
          if (saved) { ctx.flash(`Prices at ${saved.name} saved.`); remount(); }
        } catch (e) { ctx.flash(`Could not set the price: ${e.message}`, { kind: 'error' }); }
      } }, p.place ? 'Set price' : 'Save place and set price') : null) };
    const ppkCell = (p) => (p.price_per_kwh != null && p.cost_by_currency && p.cost_by_currency.length === 1 ? format({ amount: p.price_per_kwh, currency: p.cost_by_currency[0].currency }, 'price_energy') : h('span', { class: 'ch-quiet' }, '—'));
    const none = () => empty({ title: 'No charging place in this period.', text: 'Choose a longer period, or clear the place filter.' });
    // Built once; a refresh loads the tables again and moves the map's circles, with the old ones on screen
    // until the new answer is in (nothing blanks out and comes back).
    if (!placesUi) {
      const byEnergy = dataTable(ctx, {
        path: 'charging-stats/places', params: { ...pp, sort: 'energy' }, limit: 8, itemName: ['place', 'places'], label: 'Charging places by energy', cls: 'ch-place-table', cover: 'cs.table.top_energy',
        columns: [{ key: 'label', label: 'Place', th: true, cover: 'cs.table.top_energy.location', cell: placeCell }, sessions,
          { key: 'energy', label: 'Added', num: true, cover: 'cs.table.top_energy.charged', cell: (p) => q(p.energy_added_kwh, 'energy') }, setCol],
        empty: none,
      });
      const byCost = dataTable(ctx, {
        path: 'charging-stats/places', params: { ...pp, sort: 'cost' }, limit: 8, itemName: ['place', 'places'], label: 'Charging places by cost', cls: 'ch-place-table', cover: 'cs.table.top_cost',
        columns: [{ key: 'label', label: 'Place', th: true, cover: 'cs.table.top_cost.location', cell: placeCell }, sessions,
          { key: 'cost', label: 'Cost', num: true, cover: 'cs.table.top_cost.cost', cell: (p) => (p.cost_by_currency && p.cost_by_currency.length ? moneyList(p.cost_by_currency) : h('span', { class: 'ch-quiet' }, 'no price')) },
          { key: 'ppk', label: 'Per kWh', num: true, cell: ppkCell }, setCol],
        empty: none,
      });
      const mapEl = h('div', { class: 'map ch-map', role: 'region', 'aria-label': 'Map of the charging places' });
      const mapRead = h('p', { class: 'readout ch-map-read', 'aria-live': 'polite' });
      placesUi = { byEnergy, byCost, mapEl, mapRead, m: null, pts: [], first: true };
      replace(placesBox,
        blockHead('ch-places-title', 'Charging places', 'grouped by saved place, else by position', csvButton('charging places map', () => downloadCsv(file('charging places'), placesUi.pts, [
          { label: 'Place', value: (p) => p.label }, { label: 'Place id', value: (p) => p.place || '' }, column('Latitude', 'lat'), column('Longitude', 'lon'), column('Sessions', 'sessions'), column('Energy added', 'energy_added_kwh', 'energy'),
          { label: 'Cost', value: (p) => moneyList(p.cost_by_currency || []) }]), 'cs.map.energy_by_location')),
        h('div', { class: 'ch-places-grid' },
          h('div', { class: 'ch-map-part', cover: 'cs.map.energy_by_location' }, mapRead, mapEl,
            h('p', { class: 'note' }, 'Bigger circles took more energy. A place is a saved place or one spot, never an address text, so one spot is never split in two. Click a saved place to list its charges.')),
          h('div', { class: 'ch-place-tables' },
            h('div', { class: 'ch-part' }, h('h3', { class: 'label' }, 'By energy added'), byEnergy.el),
            h('div', { class: 'ch-part' }, h('h3', { class: 'label' }, 'By cost'), byCost.el))));
    }
    const ui = placesUi;
    const first = ui.first;
    ui.first = false;
    const loads = first
      ? [ui.byEnergy.load().catch(() => {}), ui.byCost.load().catch(() => {})]
      : [ui.byEnergy.reload({ ...pp, sort: 'energy' }).catch(() => {}), ui.byCost.reload({ ...pp, sort: 'cost' }).catch(() => {})];
    let pts;
    try { pts = await ctx.api.list('charging-stats/places', { ...pp, sort: 'energy' }, { limit: 200 }).all({ max: 400 }); } catch { pts = []; }
    if (my !== statsGen || !ctx.alive()) return;
    ui.pts = pts;
    const idleMap = () => replace(ui.mapRead, h('span', { class: 'readout-idle' }, ui.pts.length ? `${tn(ui.pts.length, '{n} place', '{n} places')}${ui.pts.length >= 400 ? ' (the 400 with the most energy)' : ''}; point at a circle for its figures.` : 'No charge in this period, so there is nothing to map.'));
    idleMap();
    const features = () => {
      const max = Math.max(1, ...ui.pts.map((p) => p.energy_added_kwh || 0));
      return { type: 'FeatureCollection', features: ui.pts.map((p, i) => ({ type: 'Feature', properties: { i, r: 4 + 13 * Math.sqrt((p.energy_added_kwh || 0) / max) }, geometry: { type: 'Point', coordinates: [p.lon, p.lat] } })) };
    };
    if (ui.m) ui.m.map.getSource('ch-places')?.setData(features());
    else if (pts.length) {
      // a map below the fold is made when it comes near (core/map.js): the stats wait for it only when it is near now
      const drawn = drawMap();
      if (nearNow(ui.mapEl)) loads.push(drawn);
      else drawn.catch((err) => { if (!ctx.signal.aborted) replace(ui.mapEl, h('p', { class: 'map-note' }, `The map did not load: ${err.message}`)); });
    }
    await Promise.all(loads);

    async function drawMap() {
      const m = await makeMap(ctx, ui.mapEl, { bounds: boundsOf(ui.pts.map((p) => [p.lon, p.lat])), padding: 44, maxZoom: 14, lazy: true });
      if (!m) return;
      await m.loaded;
      if (ctx.signal.aborted) return;
      ui.m = m;
      m.map.addSource('ch-places', { type: 'geojson', data: features() });
      const paint = () => ({ 'circle-color': cssVar('--ochre'), 'circle-opacity': 0.42, 'circle-stroke-color': m.imagery() ? IMAGERY_HALO.casing : cssVar('--ochre'), 'circle-stroke-width': (m.imagery() ? 2.5 : 1.5) * display.scale, 'circle-radius': ['*', ['get', 'r'], display.scale] });
      m.map.addLayer({ id: 'ch-places', type: 'circle', source: 'ch-places', paint: paint() });
      const restyle = () => { if (m.map.getLayer('ch-places')) for (const [k, v] of Object.entries(paint())) m.map.setPaintProperty('ch-places', k, v); };
      const stopTheme = onTheme(restyle);
      const stopSize = display.onChange((c) => { if (c.rem) restyle(); });
      const stopTiles = m.onTiles(restyle);
      ctx.onCleanup(() => { stopTheme(); stopSize(); stopTiles(); });
      m.map.on('mousemove', 'ch-places', (e) => {
        const p = ui.pts[e.features && e.features[0] && e.features[0].properties.i];
        m.map.getCanvas().classList.add('is-pointing');
        if (p) replace(ui.mapRead, h('strong', null, p.label), ' · ', format(p.energy_added_kwh, 'energy'), ' in ', tn(p.sessions, '{n} session', '{n} sessions'), (p.cost_by_currency || []).length ? ` · ${moneyList(p.cost_by_currency)}` : '');
      });
      m.map.on('mouseleave', 'ch-places', () => { m.map.getCanvas().classList.remove('is-pointing'); idleMap(); });
      m.map.on('click', 'ch-places', (e) => {
        const p = ui.pts[e.features && e.features[0] && e.features[0].properties.i];
        if (!p) return;
        if (p.place) { f.places = [p.place]; placePick.set(f.places); changed(); window.scrollTo({ top: 0 }); } else { const href = placeLink({ lat: p.lat, lon: p.lon }); if (href) ctx.go(href); }
      });
      await Promise.race([m.ready, new Promise((r) => setTimeout(r, 4000))]);
    }
  }

  /* the fast-charging curve, one car at a time */
  let curveCar = ctx.query.curve_car && viewCars.includes(ctx.query.curve_car) ? ctx.query.curve_car : null;
  async function drawCurve(rows, my) {
    const dcCount = new Map();
    for (const c of rows) if (c.kind === 'dc') dcCount.set(c.car, (dcCount.get(c.car) || 0) + 1);
    if (!curveCar || !viewCars.includes(curveCar)) curveCar = fixedCar ? fixedCar.id : ([...dcCount.entries()].sort((a, b) => b[1] - a[1])[0] || [viewCars[0]])[0];
    const carSel = viewCars.length > 1 ? h('select', { class: 'field-select ch-curve-car', 'aria-label': 'The car whose curve is shown' },
      viewCars.map((cid) => h('option', { value: cid, selected: cid === curveCar }, `${carName(cid)} (${tn(dcCount.get(cid) || 0, '{n} DC session', '{n} DC sessions')})`))) : null;
    carSel?.addEventListener('change', () => { curveCar = carSel.value; ctx.setQuery({ curve_car: curveCar }); drawCurve(rows, statsGen).catch(() => {}); });
    // The answer first, then the block is drawn once: the curve on screen stays until the new one is in.
    if (!curveBox.firstChild) replace(curveBox, loadingBlock(4));
    let data = { median: [], sessions: [] }, fail = null;
    try { data = await curveRead(ctx, curveCar, f); } catch (err) {
      if (ctx.signal.aborted) return;
      fail = err;
    }
    if (my !== statsGen || !ctx.alive()) return;
    const plot = h('div', { class: 'ch-curve-plot' });
    const read = h('p', { class: 'readout ch-readout', 'aria-live': 'polite' });
    const csv = () => downloadCsv(file(`fast charging curve ${carName(curveCar)}`), [
      ...(data.median || []).map((p) => ({ row: 'median', ...p })),
      ...(data.sessions || []).flatMap((s) => s.points.map((p) => ({ row: 'session', charge: s.charge, label: s.label, start_ms: s.start_ms, ...p }))),
    ], [{ label: 'Row', value: (r) => r.row }, { label: 'Charge', value: (r) => r.charge || '' }, { label: 'Place', value: (r) => r.label || '' }, timeColumn('Start', 'start_ms', data.time_zone || tz),
      column('Battery level', 'soc_pct', 'percent'), column('Power', 'power_kw', 'power'), column('Battery temperature', 'battery_c', 'temperature')]);
    replace(curveBox,
      blockHead('ch-curve-title', 'Fast-charging curve', viewCars.length > 1 ? null : `${carName(curveCar)}, every DC session in the period`, carSel, csvButton('fast-charging curve', csv, 'cs.chart.dc_curve_sessions')),
      h('ul', { class: 'ch-key' },
        h('li', null, h('span', { class: 'ch-swatch ch-swatch-median', 'aria-hidden': 'true' }), 'median of every session at each battery %'),
        h('li', { class: 'ch-temp-key' }, 'battery temperature ', ['t0', 't1', 't2', 't3', 't4'].map((t, i) => h('span', { class: 'ch-temp-step' }, h('span', { class: ['ch-swatch', `ch-${t}`], 'aria-hidden': 'true' }), ['under ', '', '', '', 'over '][i], format([10, 15, 25, 35, 40][i], 'temperature', { digits: 0 }))))),
      read, plot,
      h('p', { class: 'note' }, 'One dot per battery percent of each session, coloured by the battery’s temperature: a cold pack takes power slower. The line is this car’s median power at each percent. Point at a dot to see its session; click it to open the charge.'));
    if (fail) { replace(plot, errorView(fail, { eyebrow: 'Fast-charging curve', retry: () => drawCurve(rows, statsGen) })); return; }
    const sessions = data.sessions || [];
    const median = data.median || [];
    const zc = data.time_zone || tz;
    const idleRead = () => replace(read, h('span', { class: 'readout-idle' }, sessions.length ? `${tn(sessions.length, '{n} DC session', '{n} DC sessions')} of ${carName(curveCar)}${median.length ? `; the median peaks at ${format(Math.max(...median.map((p) => p.power_kw || 0)), 'power')}` : ''}.` : ''));
    idleRead();
    if (!sessions.length) { replace(plot, empty({ title: `${carName(curveCar)} did not fast-charge in this period.`, text: 'Choose a longer period, or another car above.' })); return; }
    const maxP = Math.max(10, ...sessions.flatMap((s) => s.points.map((p) => p.power_kw || 0)), ...median.map((p) => p.power_kw || 0));
    const top = Math.ceil((maxP * 1.05) / 50) * 50;
    const yOf = (kw) => 100 - clamp((kw / top) * 100, 0, 100);
    const tClass = (c) => (c == null ? 'ch-tn' : c < 10 ? 'ch-t0' : c < 20 ? 'ch-t1' : c < 30 ? 'ch-t2' : c < 40 ? 'ch-t3' : 'ch-t4');
    const segs = (pts, cls) => pts.slice(1).map((p, i) => svg('line', { class: cls, x1: p3(pts[i].soc_pct), y1: p3(yOf(pts[i].power_kw)), x2: p3(p.soc_pct), y2: p3(yOf(p.power_kw)) }));
    const hoverG = svg('g', { class: 'ch-curve-hover' });
    const dots = svg('g', { class: 'ch-dots' }, sessions.map((s, si) => s.points.map((p, pi) => svg('circle', { class: ['ch-dot', tClass(p.battery_c)], cx: p3(p.soc_pct), cy: p3(yOf(p.power_kw)), dataset: { s: si, p: pi } }))));
    const yTicks = Array.from({ length: 5 }, (_, i) => (top / 4) * i);
    const graph = svg('svg', { class: 'ch-curve-svg', width: '100%', height: '100%', role: 'img', 'aria-label': `Charging power against battery level for ${tn(sessions.length, '{n} DC session', '{n} DC sessions')}, with the median` },
      yTicks.map((v) => svg('line', { class: 'ch-grid', x1: 0, x2: '100%', y1: p3(yOf(v)), y2: p3(yOf(v)) })),
      [20, 40, 60, 80].map((v) => svg('line', { class: 'ch-grid ch-grid-v', x1: p3(v), x2: p3(v), y1: 0, y2: '100%' })),
      segs(median, 'ch-median'), dots, hoverG);
    const wrap = h('div', { class: 'ch-curve-area', cover: ['cs.chart.dc_curve_median', 'cs.link.dc_curve_details'] }, graph);
    let on = null;
    const show = (si, pi) => {
      if (on && on.si === si && on.pi === pi) return;
      on = { si, pi };
      const s = sessions[si], p = s.points[pi];
      replace(hoverG, segs(s.points, 'ch-session-line'), svg('circle', { class: ['ch-dot', 'is-on', tClass(p.battery_c)], cx: p3(p.soc_pct), cy: p3(yOf(p.power_kw)) }));
      replace(read, h('span', { class: 'readout-time' }, `${s.label}, ${formatDate(s.start_ms, zc, { weekday: false, now: api.now() })} ${formatTime(s.start_ms, zc)}`), ' ',
        h('strong', null, `${pct(p.soc_pct)}: ${format(p.power_kw, 'power')}`), `, battery ${format(p.battery_c, 'temperature', { digits: 0 })}. Click to open it.`);
    };
    wrap.addEventListener('mouseover', (e) => { const d = e.target.closest('circle[data-s]'); if (d) show(+d.dataset.s, +d.dataset.p); });
    wrap.addEventListener('mouseleave', () => { on = null; replace(hoverG); idleRead(); });
    wrap.addEventListener('click', () => { if (on) ctx.go(state.buildUrl('charge', { car: curveCar, charge: sessions[on.si].charge, period: ctx.state.period })); });
    replace(plot,
      h('div', { class: 'ch-curve-y', 'aria-hidden': 'true' }, yTicks.map((v) => h('span', { vars: { '--y': p3(yOf(v)) } }, format(v, 'power', { digits: 0 })))),
      wrap,
      h('div', { class: 'ch-curve-x', 'aria-hidden': 'true' }, [0, 20, 40, 60, 80, 100].map((v) => h('span', { vars: { '--x': p3(v) } }, `${v} %`))));
  }

  /* ---------------------------------------------------------------- the page */

  ctx.main.append(
    pageHead({
      eyebrow: scopeName, title: 'Charges',
      deck: 'Find charging sessions and understand charging habits and costs. Open a row for its curve and how its cost was worked out.',
    }),
    filters, totals,
    h('section', { class: 'ch-sessions', 'aria-label': 'Charging sessions', cover: 'shots.charges' },
      h('p', { class: 'note ch-list-help', cover: 'charges.text.list_help',
        title: `The totals above count exactly the charges listed. Charging time leaves out time plugged in but not charging, so average power is not diluted. Energy added is measured at the pack; from the grid is metered for AC only, so DC efficiency is shown as unknown. Range added is ${rangeLabel().toLowerCase()}, as chosen in Display and units. Point at a row for where its data came from.` },
        'The totals count exactly the charges listed. ', ctx.me && ctx.me.role === 'admin' ? 'Click a cost to see or change it.' : 'Click a cost to see how it was worked out.'),
      table.el),
    missing,
    h('div', { class: 'ch-stats', cover: 'shots.charging-stats' }, statsHead, h('div', { class: 'ch-stats-row' }, costsBox, splitBox), habitsBox, placesBox, curveBox));

  async function reloadAll(c = ctx) {
    drawPeriod();
    await Promise.all([table.reload(listParams()), drawMissing(), loadStats()]);
    c.ready?.();
  }

  // A charge started or ended, or a rebuild: load again a moment later (a burst loads once).
  let rowsTimer = 0;
  ctx.events(['rows'], (ev) => {
    const d = ev.data || {};
    if (d.car && !viewCars.includes(d.car)) return;
    if (d.kinds && !d.kinds.includes('charges')) return;
    clearTimeout(rowsTimer);
    rowsTimer = setTimeout(() => { if (ctx.alive()) reloadAll().catch(() => {}); }, 2000);
  });
  ctx.onCleanup(() => clearTimeout(rowsTimer));

  await Promise.all([table.load(), drawMissing(), loadStats()]);
  ctx.ready();
  return {
    async update(c) {
      await reloadAll(c);
    },
  };
}

/* ------------------------------------------------------------------ the battery bar of a row */

function socBar(a, b) {
  if (a == null && b == null) return null;
  const lo = clamp(Math.min(a ?? b, b ?? a), 0, 100), hi = clamp(Math.max(a ?? b, b ?? a), 0, 100);
  return svg('svg', { class: 'ch-socbar', width: '100%', height: '100%', preserveAspectRatio: 'none', 'aria-hidden': 'true' },
    svg('rect', { class: 'ch-socbar-track', x: 0, y: 0, width: '100%', height: '100%' }),
    svg('rect', { class: 'ch-socbar-had', x: 0, y: 0, width: p3(lo), height: '100%' }),
    svg('rect', { class: 'ch-socbar-added', x: p3(lo), y: 0, width: p3(Math.max(0.8, hi - lo)), height: '100%' }));
}

/* ------------------------------------------------------------------ a pick list in a small menu */

/**
 * A button that says what is chosen, and under it a search box and a ticked list (cars or places).
 * instant: each tick applies (after a short pause); else an Apply button. el.set(ids), el.summary().
 */
function pickList(ctx, { id, items, chosen, none, noun, onapply, instant = false, apply = 'Apply', failed = false, emptyText = 'Nothing to pick' }) {
  let sel = new Set(chosen);
  let soldOpen = false;
  const byId = new Map(items.map((x) => [x.id, x]));
  const trigger = h('button', { type: 'button', class: 'field-select ch-pick-trigger', id, disabled: failed || !items.length });
  const search = h('input', { class: 'field-input ch-pick-search', type: 'search', placeholder: `Find a ${noun[0]}`, 'aria-label': `Find a ${noun[0]}`, autocomplete: 'off' });
  const list = h('ul', { class: 'ch-pick-list' });
  const more = h('p', { class: 'ch-pick-more' });
  let timer = 0;
  ctx.onCleanup(() => clearTimeout(timer));
  const fire = () => { if (!instant) return; clearTimeout(timer); timer = setTimeout(() => onapply([...sel]), 350); };
  const names = () => [...sel].map((x) => byId.get(x)?.label).filter(Boolean);
  const summary = () => { const n = names(); return n.length <= 2 ? n.join(' or ') : `${n[0]}, ${n[1]} or ${tn(n.length - 2, `{n} other ${noun[0]}`, `{n} other ${noun[1]}`)}`; };
  function text() {
    const n = names();
    trigger.textContent = failed ? `The ${noun[1]} could not be loaded` : !items.length ? emptyText : !n.length ? none : n.length === 1 ? n[0] : `${n[0]} and ${formatNumber(n.length - 1)} more`;
  }
  function draw() {
    const s = search.value.trim().toLowerCase();
    const found = items.filter((x) => !s || x.label.toLowerCase().includes(s));
    found.sort((a, b) => (sel.has(b.id) - sel.has(a.id)) || a.label.localeCompare(b.label));
    // Sold cars (items with sold: true) wait behind their own toggle unless ticked or searched for.
    const { rows, cut } = soldPickRows(found, {
      chosen: (x) => sel.has(x.id), searching: !!s, open: soldOpen, ontoggle: () => { soldOpen = !soldOpen; draw(); }, max: 60,
      row: (x) => {
        const box = h('input', { type: 'checkbox', id: `${id}-${x.id}`, checked: sel.has(x.id) });
        box.addEventListener('change', () => { if (box.checked) sel.add(x.id); else sel.delete(x.id); text(); fire(); });
        return h('li', null, h('label', { class: 'ch-pick-item', for: `${id}-${x.id}` }, box, h('span', { class: 'ch-pick-name' }, x.label), h('span', { class: 'ch-pick-sub' }, x.sub || '')));
      },
    });
    replace(list, rows);
    more.textContent = !found.length ? `No ${noun[0]} has that name.` : cut ? `${formatNumber(cut)} more; type to narrow the list.` : '';
  }
  search.addEventListener('input', draw);
  const panel = h('div', { class: 'pop ch-pick-pop', id: `${id}-menu`, role: 'group', 'aria-label': noun[1], tabindex: '-1' },
    items.length > 6 ? search : null, list, more,
    h('div', { class: 'ch-pick-foot' },
      button(instant ? 'Clear' : none, { cls: 'button-small', onclick: () => { sel.clear(); draw(); text(); if (instant) fire(); else { pop.close({ refocus: true }); onapply([]); } } }),
      button(instant ? 'Done' : apply, { kind: 'primary', cls: 'button-small', onclick: () => { pop.close({ refocus: true }); if (!instant) onapply([...sel]); } })));
  const wrap = h('div', { class: 'pop-wrap ch-pick-wrap' }, trigger, panel);
  const pop = popover(trigger, panel, { onOpen: draw, focus: '.ch-pick-search' });
  text();
  wrap.set = (ids) => { sel = new Set(ids); text(); };
  wrap.summary = summary;
  return wrap;
}
