// Charges (/cars/{car}/charges, or /charges?cars=all|ids for several cars): find charging sessions and
// understand charging habits and costs. One filter bar drives the list (GET .../charges, sorted and
// paged by the server, CSV) and its totals; the stats sections (GET .../charging-stats, /places and the
// per-car /dc-curve) follow the period, the cars, the places and the shortest charge.
// AC or DC comes from the charge port's own signal; a session without one is Unknown, never DC. DC
// efficiency is unknown (the grid side of a fast charger is not measured), never a fake 100 %. Money
// is per currency and never added across currencies.
// Live: a charge that starts or ends ("rows" events) loads everything again.

import * as state from '../shared/state.js';
import { h, replace, setVars, cssVar } from '../shared/dom.js';
import { downloadCsv, csvFilename, column } from '../shared/csv.js';
import { format, formatMoney, formatMoneyList, unitLabel, rangeLabel } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, buckets, bucketKey } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon } from '../core/icons.js';
import { spotText } from '../core/fmt.js';
import { pageHead, metric, csvButton } from '../core/page.js';
import { dataTable } from '../core/table.js';
import { emptyState } from '../core/states.js';
import { loadUPlot, mountPlot, timeAxis, valueAxis, colors, hexA, chartFont } from '../core/chart.js';
import { createMap, mapFallback, boundsOf } from '../core/map.js';
import { openPeriodPicker } from '../core/period.js';
import { placePopover } from '../core/popover.js';
import { isAdmin } from '../core/prefs.js';
import { flash } from '../core/flash.js';
import { rpx } from '../core/display.js';
import { srcMark, sourceOptions, filterWord } from '../core/source.js';

export const id = 'charges';
export const css = true;

const enc = encodeURIComponent;
const KIND = { ac: 'AC', dc: 'DC', unknown: 'Unknown' };
const KIND_HELP = {
  ac: 'AC: the car’s own charger turned mains power into DC. The grid energy is measured, so losses and efficiency are known.',
  dc: 'DC: a fast charger fed the pack directly. Its grid side is not measured, so efficiency is unknown.',
  unknown: 'Unknown: the charge port did not say AC or DC for this session (often imported history). It is never counted as DC.',
};
const COST_SOURCE = { tariff: 'place price', manual: 'typed by hand', free_supercharging: 'free Supercharging', teslamate: 'imported' };
const PLACE_KINDS = [['', 'Any kind of place'], ['home', 'Home'], ['work', 'Work'], ['charger', 'Public charger'], ['service', 'Service'], ['other', 'Other place'], ['none', 'Not a saved place']];
// device pixels per CSS pixel of a uPlot canvas
const pxr = (u) => (u.width ? u.ctx.canvas.width / u.width : globalThis.devicePixelRatio || 1);
const iso = (ms) => (ms == null ? '' : new Date(ms).toISOString());
const pct = (v) => format(v, 'percent');
const placeLabel = (c) => c?.place?.name || c?.label || c?.address?.short || spotText(c) || '—';
const placeHref = (c) => (c?.place?.id ? state.buildUrl('places', { query: { place: c.place.id } })
  : c?.position?.lat != null ? state.buildUrl('places', { query: { lat: c.position.lat.toFixed(5), lon: c.position.lon.toFixed(5), new: 1 } }) : null);
// several currencies, one per line (never added together)
// with more than one currency each line names its currency code, since several share the $ sign
const stack = (items) => (items.length ? h('span', { class: 'money-stack' }, items.map(([t, cur]) => h('span', null, t, items.length > 1 && cur ? h('small', { class: 'cur' }, ` ${cur}`) : null))) : null);
const moneyStack = (list) => stack((list || []).filter((x) => x.amount != null).map((x) => [formatMoney(x.amount, x.currency), x.currency]));
const priceStack = (list) => stack((list || []).filter((x) => x.value != null).map((x) => [format({ amount: x.value, currency: x.currency }, 'price_energy'), x.currency]));

// a colour between two #rrggbb tokens
function mixHex(a, b, t) {
  const p = (c) => { const m = /^#([0-9a-f]{6})$/i.exec(String(c).trim()); return m ? parseInt(m[1], 16) : null; };
  const x = p(a), y = p(b);
  if (x == null || y == null) return t < 0.5 ? a : b;
  const ch = (n, s) => (n >> s) & 255;
  const m = (s) => Math.round(ch(x, s) + (ch(y, s) - ch(x, s)) * t);
  return `rgb(${m(16)}, ${m(8)}, ${m(0)})`;
}
// battery temperature to a colour: cold blue, mild teal, warm amber
const TEMP_STOPS = [10, 25, 40];
function tempColor(c, pal) {
  if (c == null) return pal.unknown;
  if (c <= TEMP_STOPS[0]) return pal.cold;
  if (c <= TEMP_STOPS[1]) return mixHex(pal.cold, pal.mild, (c - TEMP_STOPS[0]) / (TEMP_STOPS[1] - TEMP_STOPS[0]));
  if (c <= TEMP_STOPS[2]) return mixHex(pal.mild, pal.warm, (c - TEMP_STOPS[1]) / (TEMP_STOPS[2] - TEMP_STOPS[1]));
  return pal.warm;
}

export async function mount(ctx) {
  const { tz, period } = ctx;
  const fixedCar = ctx.car;
  const q = ctx.query;
  const now = () => ctx.api.now();

  /* ---------- which cars */
  let fleet = [];
  try { fleet = (await ctx.api.page('cars', { sort: 'name' }, { limit: 200 })).items || []; } catch { fleet = fixedCar ? [fixedCar] : []; }
  if (!ctx.alive()) return;
  const carById = new Map(fleet.map((c) => [c.id, c]));
  if (fixedCar) carById.set(fixedCar.id, fixedCar);
  const chosen = fixedCar ? [fixedCar.id] : (!q.cars || q.cars === 'all' ? 'all' : q.cars.split(',').filter(Boolean));
  const viewCars = chosen === 'all' ? fleet.map((c) => c.id) : chosen;
  const multi = !fixedCar;
  const carsParam = fixedCar ? null : (chosen === 'all' ? 'all' : chosen.join(','));
  const carName = (cid) => carById.get(cid)?.name || cid;
  const scopeName = fixedCar ? fixedCar.name : chosen === 'all' ? (fleet.length === 1 ? fleet[0].name : `All ${formatNumber(fleet.length)} cars`) : chosen.length === 1 ? carName(chosen[0]) : `${formatNumber(chosen.length)} cars`;
  const listPath = fixedCar ? `cars/${enc(fixedCar.id)}/charges` : 'charges';
  const statsPath = fixedCar ? `cars/${enc(fixedCar.id)}/charging-stats` : 'charging-stats';
  const fileBase = fixedCar ? fixedCar.name : scopeName;

  /* ---------- filters (in the address) */
  const f = {
    places: q.places || '', place_kind: q.place_kind || '', kind: q.kind || '', q: q.q || '', min_cost: q.min_cost || '',
    unpriced: q.unpriced === '1', min_dur: q.min_dur || '', source: filterWord(q.source), flags: q.flags || '',
  };
  const base = () => ({ ...ctx.params, cars: carsParam });
  const listParams = () => ({
    ...base(), places: f.places || null, place_kind: f.place_kind || null, kind: f.kind || null, q: f.q || null,
    min_cost: f.min_cost !== '' ? f.min_cost : null, include_unpriced: f.min_cost !== '' && f.unpriced ? 'true' : null,
    min_duration_min: f.min_dur || null, source: f.source || null, flags: f.flags || null,
  });
  const statsParams = () => ({ ...base(), places: f.places || null, min_duration_min: f.min_dur || null });
  const queryOf = () => ({
    places: f.places || null, place_kind: f.place_kind || null, kind: f.kind || null, q: f.q || null, min_cost: f.min_cost || null,
    unpriced: f.min_cost && f.unpriced ? '1' : null, min_dur: f.min_dur || null, source: f.source || null, flags: f.flags || null,
  });

  const root = h('div', { class: 'charges-page', cover: ['shots.charges', 'shots.charging-stats'] });
  replace(ctx.view, root);

  // places for the place filter (the most visited first)
  let places = [];
  try { places = (await ctx.api.page('places', { sort: 'visits' }, { limit: 200 })).items || []; } catch { places = []; }
  if (!ctx.alive()) return;
  const twins = new Set(places.map((p) => p.name).filter((n, i, a) => a.indexOf(n) !== i));
  const placeSub = (p) => [p.counts?.charges != null ? `${formatNumber(p.counts.charges)} ${p.counts.charges === 1 ? 'charge' : 'charges'}` : null,
    twins.has(p.name) && p.lat != null ? `${p.lat.toFixed(3)}, ${p.lon.toFixed(3)}` : null].filter(Boolean).join(' · ');

  /* ---------- a pick list in a popover (cars, places) */
  const pops = [];
  function pickList({ label, hint, items, selected, allText, onApply, instant = false, cover: cv, title }) {
    const sel = new Set(selected);
    const text = h('span', { class: 'pick-text' });
    const btn = h('button', { class: 'btn pick-btn', type: 'button', 'aria-haspopup': 'dialog', 'aria-expanded': 'false', title, cover: cv },
      icon(label === 'Cars' ? 'car' : 'pin', 'ico--sm'), text, icon('chev-d', 'ico--sm'));
    const list = h('div', { class: 'pick-list', role: 'group', 'aria-label': label });
    const find = h('input', { class: 'input', type: 'search', placeholder: `Find ${label.toLowerCase()}`, 'aria-label': `Find ${label.toLowerCase()}` });
    const applyBtn = h('button', { class: 'btn btn--sm btn--primary', type: 'button' }, 'Show');
    const pop = h('div', { class: 'popover pick-pop', popover: 'auto', role: 'dialog', 'aria-label': label },
      h('p', { class: 'pick-hint' }, hint), items.length > 8 ? find : null, list,
      h('div', { class: 'pick-actions' },
        h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: () => { sel.clear(); paint(); if (instant) onApply([...sel]); } }, allText),
        instant ? h('button', { class: 'btn btn--sm', type: 'button', onclick: () => pop.hidePopover() }, 'Done') : applyBtn));
    const paintText = () => {
      const names = items.filter((x) => sel.has(x.id)).map((x) => x.label);
      text.textContent = names.length ? (names.length > 2 ? `${formatNumber(names.length)} ${label.toLowerCase()}` : names.join(', ')) : allText;
      btn.classList.toggle('is-on', names.length > 0);
    };
    const paint = () => {
      const needle = find.value.trim().toLowerCase();
      const shown = items.filter((x) => !needle || x.label.toLowerCase().includes(needle));
      replace(list, shown.length ? shown.map((x) => h('label', { class: 'check' },
        h('input', { type: 'checkbox', checked: sel.has(x.id), onchange: (e) => { if (e.target.checked) sel.add(x.id); else sel.delete(x.id); paintText(); if (instant) onApply([...sel]); } }),
        h('span', null, x.label), x.sub ? h('span', { class: 'faint pick-sub' }, x.sub) : null))
        : h('p', { class: 'note' }, items.length ? `Nothing is called that.` : `There is nothing to pick yet.`));
      paintText();
    };
    find.addEventListener('input', paint);
    applyBtn.addEventListener('click', () => { pop.hidePopover(); onApply([...sel]); });
    btn.addEventListener('click', () => {
      if (pop.matches(':popover-open')) { pop.hidePopover(); return; }
      pop.showPopover(); placePopover(pop, btn, { align: 'start' });
    });
    pop.addEventListener('toggle', (e) => btn.setAttribute('aria-expanded', String(e.newState === 'open')));
    paint();
    pops.push(pop);
    return btn;
  }
  ctx.onCleanup(() => { for (const p of pops) { if (p.matches(':popover-open')) p.hidePopover(); p.remove(); } });

  // cars: one opens /cars/{car}/charges, several or all open /charges?cars=
  const goCars = (ids) => {
    const period0 = state.current().period;
    const query = queryOf();
    const all = !ids.length || (fleet.length > 1 && ids.length === fleet.length);
    ctx.navigate(ids.length === 1 ? state.buildUrl('charges', { car: ids[0], period: period0, query })
      : state.buildUrl('charges', { period: period0, query: { ...query, cars: all ? 'all' : ids.join(',') } }));
  };
  const carBtn = pickList({
    label: 'Cars', allText: 'Every car', items: fleet.map((c) => ({ id: c.id, label: c.name, sub: [c.model, c.year].filter(Boolean).join(' · ') })),
    selected: chosen === 'all' ? [] : chosen, onApply: goCars, cover: ['charges.filter.car', 'cs.filter.car'],
    hint: 'Tick one car, several, or none for every car. Every figure, chart and list on this page then covers the cars you picked, in one view.',
    title: 'Which cars this page covers',
  });
  const placeBtn = pickList({
    label: 'Places', allText: 'Any place', items: places.map((p) => ({ id: p.id, label: p.name, sub: placeSub(p) })),
    selected: f.places ? f.places.split(',') : [], instant: true, onApply: (ids) => { f.places = ids.join(','); apply(true); },
    cover: ['charges.filter.geofence', 'cs.filter.geofence'],
    hint: 'Keep only charges at the ticked saved places. A charge belongs to the place whose circle holds it (the nearest centre wins). This also narrows the statistics below.',
    title: 'Charges at saved places only',
  });

  const kindSel = h('select', { class: 'select', 'aria-label': 'Kind of place' }, PLACE_KINDS.map(([v, l]) => h('option', { value: v, selected: f.place_kind === v }, l)));
  const kindSeg = h('span', { class: 'seg', role: 'group', 'aria-label': 'AC or DC', cover: 'charges.filter.charge_type' },
    [['', 'AC and DC'], ['ac', 'AC'], ['dc', 'DC'], ['unknown', 'Unknown']].map(([v, l]) => h('button', {
      class: 'seg-btn', type: 'button', 'aria-pressed': String(f.kind === v), title: v ? KIND_HELP[v] : 'Every charge',
      onclick: (e) => { f.kind = v; for (const b of kindSeg.children) b.setAttribute('aria-pressed', String(b === e.currentTarget)); apply(true); },
    }, l)));
  const textIn = h('input', { class: 'input', type: 'search', value: f.q, placeholder: 'Place or address', 'aria-label': 'Text in the place or address',
    title: 'Keeps charges whose saved place name, address or town contains this text.' });
  const costIn = h('input', { class: 'input input--sm', type: 'number', min: '0', step: '0.01', value: f.min_cost, 'aria-label': 'Lowest cost' });
  const unpricedIn = h('input', { type: 'checkbox', checked: f.unpriced, 'aria-label': 'Keep charges without a price' });
  const durIn = h('input', { class: 'input input--sm', type: 'number', min: '0', step: '1', value: f.min_dur, 'aria-label': 'Shortest charging time (minutes)' });
  const sourceSel = h('select', { class: 'select', 'aria-label': 'Source' }, sourceOptions(f.source).map(([v, l]) => h('option', { value: v, selected: f.source === v }, l)));
  const flagSel = h('select', { class: 'select', 'aria-label': 'Missing data' }, [['', 'Every charge'], ['gap', 'With missing data'], ['in_progress', 'Under way'], ['gap,in_progress', 'Missing data or under way']].map(([v, l]) => h('option', { value: v, selected: f.flags === v }, l)));

  let timer = 0;
  function apply(nowNow = false) {
    clearTimeout(timer);
    timer = setTimeout(() => {
      Object.assign(f, { place_kind: kindSel.value, q: textIn.value.trim(), min_cost: costIn.value, unpriced: unpricedIn.checked, min_dur: durIn.value, source: sourceSel.value, flags: flagSel.value });
      ctx.setQuery(queryOf());
      table.reload(listParams());
      loadStats();
    }, nowNow ? 0 : 350);
  }
  ctx.onCleanup(() => clearTimeout(timer));
  for (const el of [kindSel, sourceSel, flagSel, unpricedIn]) el.addEventListener('change', () => apply(true));
  for (const el of [textIn, costIn, durIn]) el.addEventListener('input', () => apply());

  const periodBtn = (cv) => h('button', { class: 'btn', type: 'button', onclick: (e) => openPeriodPicker(e.currentTarget), cover: cv, title: 'The period of the whole page. Charges belong to it by their start.' },
    icon('calendar', 'ico--sm'), period.preset === 'custom' ? 'Custom period' : period.label, icon('chev-d', 'ico--sm'));
  const filters = h('div', { class: 'filter-bar' },
    periodBtn(['charges.filter.time_range', 'cs.filter.time_range']),
    carBtn, placeBtn, kindSel, kindSeg,
    h('label', { class: 'field field--search', cover: 'charges.filter.location' }, icon('search'), textIn),
    h('span', { class: 'filter', cover: 'charges.filter.min_cost' },
      h('label', { class: 'filter' }, h('span', null, 'Cost at least'), costIn, h('span', { class: 'faint' }, 'in its currency')),
      h('label', { class: 'check filter-check', title: 'With a lowest cost set, also keep charges that have no price yet' }, unpricedIn, h('span', null, 'and unpriced'))),
    h('label', { class: 'filter', cover: ['charges.filter.min_duration', 'cs.filter.min_duration'], title: 'Charging time only; waiting while plugged in does not count. This also applies to the statistics below.' },
      h('span', null, 'Charging at least'), durIn, h('span', { class: 'faint' }, 'min')),
    sourceSel, flagSel);

  /* ---------- summary tiles (the list's totals) */
  const summary = h('section', { class: 'panel charges-summary', 'aria-label': 'Totals of the listed charges' });
  const paintSummary = (t) => {
    t = t || {};
    const n = t.count ?? 0;
    const none = !n;
    const priced = n - (t.unpriced ?? 0);
    const sess = (k) => `${formatNumber(k)} ${k === 1 ? 'session' : 'sessions'}`;
    replace(summary, h('div', { class: 'charges-metrics' },
      metric('Sessions', n, 'count', { explain: none ? 'match the filters' : `${formatNumber(priced)} priced, ${formatNumber(t.unpriced ?? 0)} without a price` }),
      metric('Energy added', none ? null : t.energy_added_kwh, 'energy', { cover: 'charges.stat.energy_added', why: 'No charge matches', explain: `into the pack, over ${sess(n)}` }),
      metric('From the grid', none || !t.energy_grid_known ? null : t.energy_grid_kwh, 'energy', {
        cover: 'charges.stat.energy_used', why: none ? 'No charge matches' : 'No AC charge with a meter reading',
        explain: none ? 'AC, measured' : `AC, measured on ${sess(t.energy_grid_known)} of ${formatNumber(n)}${t.dc_inlet_kwh ? `; DC at the inlet ${format(t.dc_inlet_kwh, 'energy')}` : ''}`,
      }),
      metric('Cost', none ? null : moneyStack(t.cost_by_currency), null, {
        cover: 'charges.stat.cost', why: none ? 'No charge matches' : 'No charge has a price',
        explain: none ? 'per currency' : `per currency, never added across; ${formatNumber(priced)} of ${sess(n)} priced`,
      }),
      metric('Average charging time', none ? null : t.avg_duration_ms, 'duration', { cover: 'charges.stat.avg_duration', why: 'No charge matches', explain: `per session, time actually charging (not plugged-in waiting)` })));
  };

  /* ---------- the session list */
  const kindCell = (c) => h('span', { class: 'cell-text' },
    h('span', { class: ['kind-pill', `kind-pill--${c.kind || 'unknown'}`], title: KIND_HELP[c.kind || 'unknown'] }, KIND[c.kind] || 'Unknown'),
    c.fast_charger ? h('span', { class: 'sub' }, [c.fast_charger.type === 'supercharger' ? 'Supercharger' : c.fast_charger.brand || 'Fast charger', c.fast_charger.class].filter(Boolean).join(' ')) : null);
  const costCell = (c) => {
    const edit = state.buildUrl('charge', { car: c.car, charge: c.id, query: { edit: 'cost' } });
    const txt = c.cost ? formatMoney(c.cost.amount, c.cost.currency) : 'No price';
    return h('span', { class: 'cell-text cell-num' },
      isAdmin() ? h('a', { class: ['main', 'cost-link', !c.cost && 'is-missing'], href: edit, title: c.cost ? 'Change the cost of this charge' : 'Set a cost for this charge' }, txt, ' ', icon('settings', 'ico--sm'))
        : h('span', { class: ['main', !c.cost && 'faint'] }, txt),
      h('span', { class: 'sub' }, c.cost ? COST_SOURCE[c.cost.source] || c.cost.source : c.place ? 'the place has no price' : 'not a saved place'));
  };
  const effCell = (c) => {
    if (c.efficiency_pct != null) {
      const bar = h('span', { class: 'eff-bar' }, h('i'));
      setVars(bar, { '--w': `${Math.max(4, Math.min(100, c.efficiency_pct)).toFixed(1)}%` });
      return h('span', { class: 'eff', title: `Energy into the pack over energy from the grid. Lost: ${format(c.losses_kwh, 'energy')}` }, bar, h('span', { class: 'eff-val' }, pct(c.efficiency_pct)));
    }
    return h('span', { class: 'faint nowrap', title: c.kind === 'dc' ? 'A fast charger’s grid side is not measured, so the efficiency is unknown.' : 'No grid meter reading for this charge.' }, c.kind === 'dc' ? 'unknown (DC)' : '—');
  };
  const columns = [
    multi ? { key: 'car', label: 'Car', render: (c) => h('span', { class: 'nowrap' }, carName(c.car)), csv: (c) => carName(c.car) } : null,
    { key: 'start', label: 'Start', sort: 'start', firstDir: 'desc', cover: ['charges.table.date', 'charges.link.view_details'], render: (c) => h('span', { class: 'cell-text' },
      h('a', { class: 'main', href: state.buildUrl('charge', { car: c.car, charge: c.id }) }, `${formatDate(c.start_ms, c.time_zone || tz, { year: false })}, ${formatTime(c.start_ms, c.time_zone || tz)}`),
      h('span', { class: 'sub sub--src' }, c.end_ms ? `to ${formatTime(c.end_ms, c.time_zone || tz)}` : 'charging now', srcMark(c, { short: true, estimated: true }))),
      csv: (c) => iso(c.start_ms) },
    { key: 'place', label: 'Place', cls: 'col-place', cover: ['charges.table.location', 'charges.link.geofence'], render: (c) => {
      const href = placeHref(c);
      return h('span', { class: 'cell-text' },
        href ? h('a', { class: 'main place-link', href, title: c.place ? `Edit the place ${c.place.name}` : 'Make this spot a saved place' }, placeLabel(c)) : h('span', { class: 'main' }, placeLabel(c)),
        h('span', { class: 'sub' }, c.place ? c.address?.short || '' : 'not a saved place yet'));
    }, csv: (c) => placeLabel(c) },
    { key: 'kind', label: 'Type', cover: 'charges.table.type', title: 'From the charge port’s own signal', render: kindCell, csv: (c) => KIND[c.kind] || 'Unknown' },
    { key: 'duration', label: 'Charging', num: true, sort: 'duration', cover: 'charges.table.duration', title: 'Time charging, then time plugged in', render: (c) => h('span', { class: 'cell-text cell-num' },
      h('span', { class: 'main' }, formatDuration(c.charging_ms)), h('span', { class: 'sub' }, c.plugged_ms != null ? `plugged ${formatDuration(c.plugged_ms)}` : '')),
      csv: (c) => (c.charging_ms == null ? null : Math.round(c.charging_ms / 60000)) },
    { key: 'added', label: 'Added', num: true, sort: 'energy', cover: 'charges.table.energy_added', title: 'Energy into the pack', render: (c) => format(c.energy_added_kwh, 'energy'), csv: column('Energy added', 'energy_added_kwh', 'energy') },
    { key: 'grid', label: 'From grid', num: true, cover: 'charges.table.energy_used', title: 'AC: measured at the car’s charger input. DC: what the charger delivered at the inlet.', render: (c) => (c.energy_grid_kwh != null ? format(c.energy_grid_kwh, 'energy')
      : c.dc_inlet_kwh != null ? h('span', { class: 'cell-text cell-num' }, h('span', { class: 'main faint' }, 'not measured'), h('span', { class: 'sub' }, `${format(c.dc_inlet_kwh, 'energy')} at the inlet`)) : h('span', { class: 'faint' }, '—')),
      csv: column('Energy from grid', 'energy_grid_kwh', 'energy') },
    { key: 'eff', label: 'Efficiency', cover: 'charges.table.efficiency', title: 'Energy into the pack over energy from the grid (AC only)', render: effCell, csv: (c) => c.efficiency_pct },
    { key: 'cost', label: 'Cost', num: true, sort: 'cost', cover: ['charges.table.cost', 'charges.link.set_cost'], render: costCell, csv: (c) => (c.cost ? `${c.cost.amount} ${c.cost.currency}` : '') },
    { key: 'ppk', label: 'Per kWh', num: true, cover: 'charges.table.cost_per_kwh', title: 'Cost over the grid energy (AC) or over the energy added (DC)', render: (c) => (c.price_per_kwh?.value != null
      ? h('span', { class: 'cell-text cell-num' }, h('span', { class: 'main' }, format({ amount: c.price_per_kwh.value, currency: c.cost?.currency }, 'price_energy')), h('span', { class: 'sub' }, c.price_per_kwh.basis === 'grid' ? 'of grid energy' : 'of energy added'))
      : h('span', { class: 'faint' }, '—')), csv: (c) => c.price_per_kwh?.value },
    { key: 'power', label: 'Power', num: true, sort: 'power_max', cover: 'charges.table.avg_power', title: 'Average while charging (waiting time left out), then the peak', render: (c) => h('span', { class: 'cell-text cell-num' },
      h('span', { class: 'main' }, format(c.power_avg_kw, 'power')), h('span', { class: 'sub' }, c.power_max_kw != null ? `peak ${format(c.power_max_kw, 'power')}` : '')),
      csv: column('Average power', 'power_avg_kw', 'power') },
    { key: 'range', label: 'Range added', num: true, cover: 'charges.table.range_added', title: `${rangeLabel()} added`, render: (c) => format(c.range_added_km, 'range'), csv: column('Range added', 'range_added_km', 'range') },
    { key: 'rate', label: 'Per hour', num: true, cover: 'charges.table.charge_rate', title: 'Range added per hour of charging', render: (c) => format(c.range_per_hour_km, 'range_rate'), csv: column('Range per hour', 'range_per_hour_km', 'range_rate') },
    { key: 'soc', label: 'Battery', sort: 'end_soc', cover: ['charges.table.start_soc', 'charges.table.end_soc'], title: 'Battery level at the start and the end', render: (c) => {
      const bar = h('span', { class: 'soc-bar', 'aria-hidden': 'true' }, h('i'));
      setVars(bar, { '--a': `${Math.max(0, Math.min(100, c.start_soc_pct ?? 0))}%`, '--b': `${Math.max(0, Math.min(100, c.end_soc_pct ?? c.start_soc_pct ?? 0))}%` });
      return h('span', { class: 'soc-cell' }, bar, h('span', { class: 'nowrap' }, `${pct(c.start_soc_pct)} → ${pct(c.end_soc_pct)}`));
    }, csv: (c) => `${c.start_soc_pct ?? ''} -> ${c.end_soc_pct ?? ''}` },
    { key: 'temp', label: 'Temps', num: true, cover: 'charges.table.temp', title: 'Average outside, then battery temperature', render: (c) => h('span', { class: 'cell-text cell-num' },
      h('span', { class: 'main' }, format(c.outside_avg_c, 'temperature')), h('span', { class: 'sub' }, c.battery_avg_c != null ? `battery ${format(c.battery_avg_c, 'temperature')}` : 'battery —')),
      csv: column('Outside', 'outside_avg_c', 'temperature') },
    { key: 'odo', label: 'Odometer', num: true, cover: 'charges.table.odometer', render: (c) => format(c.odometer_km, 'odometer'), csv: column('Odometer', 'odometer_km', 'odometer') },
    { key: 'flags', label: '', csv: false, render: (c) => h('span', { class: 'flag-icons' },
      c.has_gaps ? h('span', { class: 'flag-ico', title: `Data missing for ${formatDuration(c.flags?.gap_ms || 0)} inside this charge` }, icon('warn', 'ico--sm')) : null,
      c.flags?.in_progress || c.end_ms == null ? h('span', { class: 'tag tag--live' }, 'live') : null) },
  ].filter(Boolean);
  const table = dataTable(ctx, {
    title: 'Charging sessions', icon: 'bolt', path: listPath, params: listParams(), limit: 25, columns, sort: { key: 'start', dir: 'desc' }, csvName: 'charges', cls: 'charges-table',
    cover: 'charges.table', noteCover: 'charges.text.list_help',
    note: 'Narrow the list with the period, cars, places, AC or DC, text, a lowest cost and a shortest charging time; the totals above count exactly the listed charges. Charging time leaves out time plugged in but not charging, so average power is not diluted. Energy added is measured at the pack (estimated for a charge with Tesla API data alone, or imported; the line under each start says where its data came from); DC efficiency is unknown because a fast charger’s grid side is not measured. Open a row for its curve and cost.',
    rowHref: (c) => state.buildUrl('charge', { car: c.car, charge: c.id }),
    onPage: (pg) => { if (pg.totals) paintSummary(pg.totals); },
    empty: () => {
      const why = f.q ? `the text "${f.q}"` : f.places ? 'the place filter' : f.place_kind ? 'the kind of place' : f.kind ? `the ${KIND[f.kind]} filter` : f.min_cost ? 'the lowest cost' : f.min_dur ? 'the shortest charging time' : f.flags ? 'the missing-data filter' : f.source ? 'the source filter' : null;
      return emptyState({ icon: 'bolt', title: 'No charges', text: why ? `No charge in ${period.label.toLowerCase()} passes ${why}. Clear it to see every charge.` : `${scopeName} did not charge in this period. Pick a longer period.` });
    },
  });

  /* ---------- charges with missing data */
  const incomplete = dataTable(ctx, {
    title: 'Charges with missing data', icon: 'warn', path: listPath, params: { ...base(), flags: 'gap,in_progress' }, limit: 20, csvName: 'charges-missing-data', cls: 'missing-table',
    noteCover: 'charges.text.incomplete_help',
    note: 'Charges that overlap a gap in the logs (no log, dropped frames, a log that failed its check) or that are still under way. Their figures cover what was recorded. Late logs fill them in by themselves; an admin can rebuild the period from the stored logs. Nothing needs repairing by hand.',
    cover: 'charges.incomplete.table',
    side: isAdmin() ? h('button', { class: 'btn btn--sm', type: 'button', onclick: rebuild, title: 'Read the stored logs again for this period and rebuild charges, drives, states and stops' }, icon('reset', 'ico--sm'), 'Rebuild the period') : null,
    columns: [
      multi ? { key: 'car', label: 'Car', render: (c) => carName(c.car), csv: (c) => carName(c.car) } : null,
      { key: 'id', label: 'Charge', cover: 'charges.incomplete.col.id', render: (c) => h('a', { class: 'main mono', href: state.buildUrl('charge', { car: c.car, charge: c.id }) }, c.id), csv: (c) => c.id },
      { key: 'start', label: 'Start', cover: 'charges.incomplete.col.start_date', render: (c) => `${formatDate(c.start_ms, c.time_zone || tz, { year: false })}, ${formatTime(c.start_ms, c.time_zone || tz)}`, csv: (c) => iso(c.start_ms) },
      { key: 'end', label: 'Last recorded', cover: 'charges.incomplete.col.end_date', render: (c) => (c.end_ms ? formatTime(c.end_ms, c.time_zone || tz) : 'still going'), csv: (c) => iso(c.end_ms) },
      { key: 'why', label: 'What is missing', render: (c) => (c.flags?.in_progress || c.end_ms == null ? 'Under way; it ends when the next logs arrive' : `No data for ${formatDuration(c.flags?.gap_ms || 0)} inside it; open it to see where`) },
      { key: 'added', label: 'Added', num: true, cover: 'charges.incomplete.col.energy_added', render: (c) => format(c.energy_added_kwh, 'energy'), csv: column('Energy added', 'energy_added_kwh', 'energy') },
      { key: 'grid', label: 'From grid', num: true, cover: 'charges.incomplete.col.energy_used', render: (c) => (c.energy_grid_kwh != null ? format(c.energy_grid_kwh, 'energy') : h('span', { class: 'faint' }, c.kind === 'dc' ? 'not measured (DC)' : '—')), csv: column('Energy from grid', 'energy_grid_kwh', 'energy') },
      { key: 'soc_a', label: 'Start %', num: true, cover: 'charges.incomplete.col.start_soc', render: (c) => pct(c.start_soc_pct), csv: (c) => c.start_soc_pct },
      { key: 'soc_b', label: 'End %', num: true, cover: 'charges.incomplete.col.end_soc', render: (c) => pct(c.end_soc_pct), csv: (c) => c.end_soc_pct },
      { key: 'duration', label: 'Charging', num: true, cover: 'charges.incomplete.col.duration', render: (c) => formatDuration(c.charging_ms), csv: (c) => (c.charging_ms == null ? null : Math.round(c.charging_ms / 60000)) },
    ].filter(Boolean),
    empty: () => emptyState({ icon: 'check', title: 'No charge is missing data', text: 'Every charge in this period has all its data, and none is under way.' }),
  });
  async function rebuild() {
    const ids = fixedCar ? [fixedCar.id] : [...new Set(incomplete.rows().map((c) => c.car))];
    if (!ids.length) { flash('No charge in this period is missing data, so there is nothing to rebuild.', { kind: 'info' }); return; }
    try {
      let logs = 0;
      for (const cid of ids) logs += (await ctx.api.post(`cars/${enc(cid)}/rebuild`, ctx.params))?.logs_read ?? 0;
      flash(`Rebuilt from ${formatNumber(logs)} stored logs.`);
      ctx.reload();
    } catch (e) { flash(`Could not rebuild: ${e.message}`, { kind: 'error' }); }
  }

  /* ---------- statistics panels */
  const scopeLine = h('p', { class: 'stats-scope note' });
  const totalsPanel = h('section', { class: 'panel panel-pad', 'aria-label': 'Costs and totals' });
  const splitPanel = h('section', { class: 'panel panel-pad split-panel', 'aria-label': 'AC and DC split' });
  const habitsPanel = h('section', { class: 'panel panel-pad habits', id: 'habits', 'aria-label': 'Charging habits' });
  const placesPanel = h('section', { class: 'panel panel-pad places-panel', 'aria-label': 'Charging places' });
  const curvePanel = h('section', { class: 'panel panel-pad curve-panel', 'aria-label': 'Fast-charging curve' });
  const head = (ic, title, ...side) => h('div', { class: 'panel-head' }, h('h2', { class: 'panel-title' }, icon(ic), title), h('span', { class: 'panel-side' }, side));
  const csvOf = (name, rows, cols, cv) => csvButton({ cover: cv, onClick: () => downloadCsv(csvFilename(fileBase, name), typeof rows === 'function' ? rows() : rows, cols) });

  let statsGen = 0;
  let chemistry = null; // cars -> 'lfp' | 'nca_nmc' | null, loaded once
  async function loadChemistry() {
    if (chemistry) return chemistry;
    chemistry = new Map();
    const ids = viewCars.slice(0, 12);
    await Promise.all(ids.map(async (cid) => {
      try { const s = await ctx.api.get(`cars/${enc(cid)}/settings`); const v = s?.battery_chemistry; chemistry.set(cid, v?.setting && v.setting !== 'auto' ? v.setting : v?.detected || null); } catch { chemistry.set(cid, null); }
    }));
    return chemistry;
  }

  async function loadStats() {
    const my = ++statsGen;
    const sp = statsParams();
    const left = [f.places ? 'the place filter' : null, f.min_dur ? `charges of at least ${f.min_dur} min` : null].filter(Boolean);
    scopeLine.textContent = `Statistics for ${scopeName}, ${period.label.toLowerCase()}${left.length ? `, with ${left.join(' and ')}` : ''}. Of the list filters, only the period, cars, places and shortest charging time apply here.`;
    let s, rows;
    try {
      [s, rows] = await Promise.all([
        ctx.api.get(statsPath, sp),
        ctx.api.list(listPath, sp, { limit: 500 }).all({ max: 5000 }),
        loadChemistry(),
      ]);
    } catch (e) {
      if (my !== statsGen || !ctx.alive()) return;
      const why = `The statistics could not be loaded: ${e.message}`;
      replace(totalsPanel, head('stats', 'Costs and totals'), h('p', { class: 'note warn' }, why));
      replace(splitPanel, head('plug', 'AC and DC'), h('p', { class: 'note warn' }, why));
      replace(habitsPanel, head('battery', 'Charging habits'), h('p', { class: 'note warn' }, why));
      // places and the curve do not need that answer: they load on their own
      await Promise.all([paintPlaces(sp), paintCurve([], my)]);
      return;
    }
    if (my !== statsGen || !ctx.alive()) return;
    paintTotals(s);
    paintSplit(s);
    await paintHabits(s, rows);
    if (my !== statsGen || !ctx.alive()) return;
    await Promise.all([paintPlaces(sp), paintCurve(rows, my)]);
  }

  /* costs and totals */
  function paintTotals(s) {
    const ppk = s.price_per_kwh || {};
    const c100 = s.cost_per_100km || {};
    const codesP = (list) => (list || []).filter((x) => x.value != null).map((x) => `${x.value} ${x.currency}/kWh`).join('; ');
    const codes = (list) => (list || []).map((x) => `${x.amount} ${x.currency}`).join('; ');
    const row = { count: s.count, energy_added_kwh: s.energy_added_kwh, total_cost: codes(s.cost_by_currency), supercharger_cost: codes(s.supercharger_cost),
      price_all: codesP(ppk.all), price_ac: codesP(ppk.ac), price_dc: codesP(ppk.dc), price_basis: ppk.basis, priced: ppk.priced, cost_per_100: (c100.value || []).map((x) => `${x.amount} ${x.currency}`).join('; '), cost_per_100_method: c100.method, distance_km: c100.distance_km };
    replace(totalsPanel,
      head('stats', 'Costs and totals', csvOf('charging-totals', [row], [column('Sessions', 'count'), column('Energy added', 'energy_added_kwh', 'energy'), column('Total cost', 'total_cost'),
        column('Supercharger cost', 'supercharger_cost'), column('Price per kWh', 'price_all'), column('Price per kWh AC', 'price_ac'), column('Price per kWh DC', 'price_dc'),
        column('Price basis', 'price_basis'), column('Priced sessions', 'priced'), column('Cost per 100 km', 'cost_per_100'), column('Method', 'cost_per_100_method'), column('Distance driven', 'distance_km', 'distance')])),
      h('div', { class: 'totals-grid' },
        metric('Sessions', s.count ?? 0, 'count', { cover: 'cs.stat.count', explain: `${formatNumber(ppk.priced ?? 0)} with a price` }),
        metric('Energy added', s.count ? s.energy_added_kwh : null, 'energy', { cover: 'cs.stat.energy_added', why: 'No charge', explain: 'into the pack' }),
        metric('Total cost', moneyStack(s.cost_by_currency), null, { cover: 'cs.stat.total_cost', why: 'No charge has a price', explain: 'per currency, never added across' }),
        metric('Supercharger cost', moneyStack(s.supercharger_cost), null, { cover: 'cs.stat.supercharger_cost', why: 'No priced Supercharger session', explain: 'sessions the charger itself names as a Tesla Supercharger' }),
        metric('Price per kWh', priceStack(ppk.all), null, { cover: 'cs.stat.cost_per_kwh', why: 'No priced charge', explain: 'all priced sessions: cost over grid energy (AC) or billed energy (DC)' }),
        metric('AC price per kWh', priceStack(ppk.ac), null, { cover: 'cs.stat.cost_per_kwh_ac', why: 'No priced AC charge', explain: 'cost over the grid energy, measured' }),
        metric('DC price per kWh', priceStack(ppk.dc), null, { cover: 'cs.stat.cost_per_kwh_dc', why: 'No priced DC charge', explain: 'cost over the energy the charger billed' }),
        metric(`Cost per 100 ${unitLabel('distance')}`, stack((c100.value || []).map((x) => [format({ amount: x.amount, currency: x.currency }, 'money_per_distance'), x.currency])), null, { cover: 'cs.stat.cost_per_100', why: c100.distance_km ? 'No charge has a price' : 'No driving in the period',
          explain: `${c100.method || 'charging cost over distance driven'}${c100.distance_km != null ? ` (${format(c100.distance_km, 'distance')})` : ''}` })),
      h('p', { class: 'note' }, 'Unpriced charges are left out of the prices, not counted as free. Prices come from each place’s tariff, a cost typed on the charge, or imported history; a free Supercharging period counts as zero.'));
  }

  /* AC and DC split */
  function paintSplit(s) {
    const bk = s.by_kind || {};
    const kinds = ['ac', 'dc', 'unknown'];
    const sum = (k) => kinds.reduce((a, x) => a + (bk[x]?.[k] || 0), 0);
    const bar = (k, kind, cv, label) => {
      const total = sum(k);
      const el = h('div', { class: 'split', cover: cv },
        h('div', { class: 'split-head' }, h('span', { class: 'list-title' }, label), h('b', null, k === 'kwh' ? format(total, 'energy') : formatDuration(total))),
        h('div', { class: 'split-bar', role: 'img', 'aria-label': `${label}: ${kinds.map((x) => `${KIND[x]} ${total ? Math.round(((bk[x]?.[k] || 0) / total) * 100) : 0} %`).join(', ')}` },
          kinds.map((x) => { const i = h('i', { class: `split-${x}` }); setVars(i, { '--w': `${total ? ((bk[x]?.[k] || 0) / total) * 100 : 0}%` }); return i; })),
        h('ul', { class: 'split-legend' }, kinds.map((x) => h('li', null, h('span', { class: ['swatch', `split-${x}`] }),
          h('span', null, KIND[x]), h('b', null, k === 'kwh' ? format(bk[x]?.kwh || 0, 'energy') : formatDuration(bk[x]?.ms || 0)),
          h('span', { class: 'faint' }, total ? `${formatNumber(((bk[x]?.[k] || 0) / total) * 100, { maximumFractionDigits: 0 })} %` : '—')))));
      return el;
    };
    replace(splitPanel,
      head('plug', 'AC and DC', csvOf('charging-ac-dc', kinds.map((x) => ({ kind: KIND[x], kwh: bk[x]?.kwh || 0, min: Math.round((bk[x]?.ms || 0) / 60000) })),
        [column('Type', 'kind'), column('Energy added', 'kwh', 'energy'), column('Charging time (min)', 'min')])),
      bar('kwh', 'energy', 'cs.chart.ac_dc_energy', 'Energy added'),
      bar('ms', 'time', 'cs.chart.ac_dc_duration', 'Time charging'),
      h('p', { class: 'note' }, 'From the charge port’s own AC or DC signal. Sessions without one are Unknown, not DC.'));
  }

  /* charging habits */
  let heatSide = q.heat === 'end' ? 'end' : 'start';
  async function paintHabits(s, rowsIn) {
    const rows = rowsIn.filter((c) => c.start_soc_pct != null).sort((a, b) => a.start_ms - b.start_ms);
    const lfpSet = new Set(viewCars.filter((cid) => chemistry.get(cid) === 'lfp'));
    const allLfp = viewCars.length > 0 && viewCars.every((cid) => lfpSet.has(cid));
    const someLfp = lfpSet.size > 0 && !allLfp;
    const upper = allLfp ? 100 : 80;
    const guideText = allLfp ? '20 % to 100 %: an LFP pack is happy to charge full' : someLfp ? `20 % to 80 % (NCA or NMC packs); ${[...lfpSet].map(carName).join(', ')} ${lfpSet.size === 1 ? 'has' : 'have'} an LFP pack, which may go to 100 %` : '20 % to 80 %, the usual daily band for an NCA or NMC pack';

    // the battery-level map over time: columns are days, weeks or months; rows are 10 % bands
    const from = ctx.params.from_ms ?? (rows[0]?.start_ms ?? now());
    const to = Math.min(ctx.params.to_ms ?? now(), now() + 60000);
    const spanDays = (to - from) / 86400000;
    const unit = spanDays <= 62 ? 'day' : spanDays <= 730 ? 'week' : 'month';
    let cols = [];
    try { cols = buckets(from, to, unit, tz, { max: 3000 }); } catch { cols = []; }
    const colIndex = new Map(cols.map((b, i) => [b.key, i]));
    const bandOf = (v) => Math.max(0, Math.min(9, Math.floor(v / 10)));
    const grid = (side) => {
      const g = Array.from({ length: 10 }, () => new Array(cols.length).fill(0));
      for (const c of rows) {
        const v = side === 'start' ? c.start_soc_pct : c.end_soc_pct;
        const t = side === 'start' ? c.start_ms : c.end_ms ?? c.start_ms;
        if (v == null) continue;
        const i = colIndex.get(bucketKey(t, unit, tz));
        if (i != null) g[bandOf(v)][i]++;
      }
      return g;
    };
    const socMap = h('div', { class: 'socmap', role: 'img' });
    const socAxis = h('div', { class: 'socmap-axis', 'aria-hidden': 'true' });
    const socRead = h('span', { class: 'chart-read' });
    const hl = h('i', { class: 'socmap-hl', hidden: true });
    let curGrid = null;
    const colLabel = (b) => (unit === 'month' ? formatDate(b.start_ms + 86400000, tz, { weekday: false, year: 'auto' }).replace(/\s?\d{1,2},?/, '') : formatDate(b.start_ms + 3600000, tz, { weekday: false, year: false }));
    const idleRead = () => replace(socRead, rows.length ? `${formatNumber(rows.length)} sessions; darker means more sessions ${heatSide === 'start' ? 'started' : 'ended'} at that level` : 'no session in the period');
    function paintSocMap() {
      curGrid = grid(heatSide);
      const max = Math.max(1, ...curGrid.flat());
      setVars(socMap, { '--n': String(Math.max(1, cols.length)) });
      socMap.setAttribute('aria-label', `Battery level at the ${heatSide} of each session, by ${unit}`);
      const cells = [];
      for (let r = 9; r >= 0; r--) {
        cells.push(h('span', { class: 'socmap-y' }, `${r * 10}–${r * 10 + 10} %`));
        for (let i = 0; i < cols.length; i++) {
          const n = curGrid[r][i];
          cells.push(h('i', { class: ['socmap-cell', n && 'has', heatSide === 'end' && 'is-end', r * 10 >= upper && 'over', r < 2 && 'under'], dataset: { col: i }, vars: { '--a': (n / max).toFixed(3) } }));
        }
      }
      hl.hidden = true;
      replace(socMap, cells, hl);
      const step = Math.max(1, Math.ceil(cols.length / 8));
      setVars(socAxis, { '--n': String(Math.max(1, cols.length)) });
      replace(socAxis, h('span'), cols.map((b, i) => (i % step === 0 ? h('span', { class: 'socmap-x', vars: { '--col': String(i + 2), '--span': String(Math.min(step, cols.length - i)) } }, colLabel(b)) : null)));
      idleRead();
    }
    const showCol = (i, fromChart = false) => {
      if (i == null || !cols[i]) { hl.hidden = true; idleRead(); return; }
      hl.hidden = false;
      setVars(hl, { '--col': String(i + 2) });
      const n = curGrid.reduce((a, r) => a + r[i], 0);
      const b = cols[i];
      replace(socRead, `${unit === 'day' ? formatDate(b.start_ms + 3600000, tz, { year: false }) : `${unit === 'week' ? 'Week of ' : ''}${formatDate(b.start_ms + 3600000, tz, { weekday: false, year: false })}`} · `,
        h('b', null, `${formatNumber(n)} ${n === 1 ? 'session' : 'sessions'}`), n ? ` ${heatSide === 'start' ? 'started' : 'ended'} at ${curGrid.map((r, k) => (r[i] ? `${k * 10}–${k * 10 + 10} %` : null)).filter(Boolean).reverse().join(', ')}` : '');
      if (!fromChart && delta?.u) {
        const u = delta.u;
        const mid = (b.start_ms + b.end_ms) / 2000;
        // the session nearest the middle of this column, so both charts point at the same moment
        let best = -1, bd = Infinity;
        u.data[0].forEach((x, k) => { const d = Math.abs(x - mid); if (d < bd) { bd = d; best = k; } });
        if (best >= 0 && u.data[0][best] * 1000 >= b.start_ms && u.data[0][best] * 1000 < b.end_ms) u.setCursor({ left: u.valToPos(u.data[0][best], 'x'), top: u.valToPos(50, 'y') });
      }
    };
    socMap.addEventListener('mousemove', (e) => { const c = e.target.closest('.socmap-cell'); if (c) showCol(+c.dataset.col); });
    socMap.addEventListener('mouseleave', () => { showCol(null); delta?.u?.setCursor({ left: -10, top: -10 }); });

    const sideSeg = h('span', { class: 'seg', role: 'group', 'aria-label': 'Start or end level' },
      [['start', 'Where sessions start'], ['end', 'Where they end']].map(([v, l]) => h('button', { class: 'seg-btn', type: 'button', 'aria-pressed': String(heatSide === v),
        onclick: (e) => { heatSide = v; ctx.setQuery({ heat: v === 'end' ? 'end' : null }); for (const b of sideSeg.children) b.setAttribute('aria-pressed', String(b === e.currentTarget)); paintSocMap(); paintWeek(); } }, l)));

    // the start-to-end band of every session
    const deltaEl = h('div', { class: 'delta-chart' });
    const deltaRead = h('span', { class: 'chart-read' });
    const idleDelta = () => replace(deltaRead, rows.length ? `${formatNumber(rows.length)} sessions; shaded: ${guideText}` : 'no session in the period');
    idleDelta();
    let delta = null;

    // when in the week sessions start or end (the car's local time)
    const week = h('div', { class: 'weekmap', role: 'img' });
    const DAYS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
    function paintWeek() {
      const g = s.heatmap?.[heatSide] || [];
      const max = Math.max(1, ...g.flat());
      week.setAttribute('aria-label', `When sessions ${heatSide}, by weekday and hour`);
      const cells = [h('span')];
      // one label per three hours, over the three columns it names
      for (let hr = 0; hr < 24; hr += 3) cells.push(h('span', { class: 'weekmap-x' }, formatTime(Date.UTC(2026, 0, 5, hr), 'UTC').replace(':00', '')));
      for (let d = 0; d < 7; d++) {
        cells.push(h('span', { class: 'weekmap-y' }, DAYS[d]));
        for (let hr = 0; hr < 24; hr++) {
          const n = g[d]?.[hr] || 0;
          cells.push(h('i', { class: ['weekmap-cell', n && 'has', heatSide === 'end' && 'is-end'], title: `${DAYS[d]} ${String(hr).padStart(2, '0')}:00–${String(hr + 1).padStart(2, '0')}:00: ${n} ${n === 1 ? 'session' : 'sessions'}`, vars: { '--a': (n / max).toFixed(3) } }));
        }
      }
      replace(week, cells);
    }

    // start and end bands, with a bar per band
    const bandList = (list, side) => {
      const max = Math.max(1, ...(list || []).map((b) => b.count));
      return h('ul', { class: 'hbars band-list' }, (list || []).slice().reverse().map((b) => {
        const lo = parseInt(b.band, 10);
        const tone = side === 'end' ? (lo >= upper ? 'is-high' : null) : (lo < 10 ? 'is-danger' : lo < 20 ? 'is-high' : null);
        const bar = h('i'); setVars(bar, { '--w': `${(b.count / max) * 100}%` });
        return h('li', { class: tone }, h('span', { class: 'hbar-label' }, `${b.band.replace('-', '–')} %`), h('span', { class: 'hbar' }, bar), h('span', { class: 'hbar-val' }, `${formatNumber(b.count)} ${b.count === 1 ? 'session' : 'sessions'}`));
      }));
    };
    const bandCols = [column('Band (%)', 'band'), column('Sessions', 'count')];
    const runsTable = dataTable(ctx, {
      title: 'Back-to-back charging', icon: 'reset', rows: s.runs || [], csvName: 'charging-runs', cls: 'runs-table', label: 'Runs of back-to-back charges',
      note: 'Two or more charges, each starting within a day of the one before: top-ups that belong together. They are listed here so one long charge split in parts is not read as a habit.',
      sort: { key: 'start', dir: 'desc' },
      columns: [
        multi ? { key: 'car', label: 'Car', sort: true, value: (r) => carName(r.car), render: (r) => carName(r.car), csv: (r) => carName(r.car) } : null,
        { key: 'start', label: 'First start', sort: true, value: (r) => r.start_ms, render: (r) => `${formatDate(r.start_ms, tz, { year: false })}, ${formatTime(r.start_ms, tz)}`, csv: (r) => iso(r.start_ms) },
        { key: 'charges', label: 'Charges', num: true, sort: true, render: (r) => formatNumber(r.charges) },
        { key: 'min_start_soc_pct', label: 'Lowest start', num: true, sort: true, render: (r) => pct(r.min_start_soc_pct) },
        { key: 'max_end_soc_pct', label: 'Highest end', num: true, sort: true, render: (r) => pct(r.max_end_soc_pct) },
      ].filter(Boolean),
      empty: () => emptyState({ icon: 'check', title: 'No back-to-back charges', text: 'Every charge in this period stands on its own.' }),
    });

    replace(habitsPanel,
      h('div', { class: 'panel-head' }, h('h2', { class: 'panel-title' }, icon('battery'), 'Charging habits'),
        h('span', { class: 'panel-side' }, sideSeg,
          h('button', { class: 'btn btn--sm', type: 'button', onclick: (e) => openPeriodPicker(e.currentTarget), cover: ['cs.filter.heatmap_window', 'cs.filter.charge_delta_window'], title: 'These charts follow the page’s period. Change it here or in the top bar.' },
            icon('calendar', 'ico--sm'), `Follows the period: ${period.label}`))),
      h('div', { class: 'habit-figures' },
        metric('Ended above 80 %', s.share_above_80_pct, 'percent', { why: 'No charge', explain: allLfp ? 'of the sessions; fine for an LFP pack' : 'of the sessions; keep it for trips with an NCA or NMC pack' }),
        metric('Started below 20 %', s.share_below_20_pct, 'percent', { why: 'No charge', explain: 'of the sessions' }),
        metric('Daily band', allLfp ? '20–100 %' : '20–80 %', null, { explain: guideText, cls: 'band-metric' })),
      h('div', { class: 'habit-block', cover: ['cs.chart.heatmap', 'cs.control.shared_crosshair'] },
        h('div', { class: 'chart-head' }, h('span', { class: 'chart-title' }, `Battery level at the start or end of each session (pick above), by ${unit}`),
          h('span', { class: 'chart-side' }, socRead, csvOf(`charging-levels-by-${unit}`, () => cols.map((b, i) => { const o = { period: b.key }; for (let r = 0; r < 10; r++) o[`${r * 10}-${r * 10 + 10}`] = curGrid[r][i]; return o; }),
            [column(unit[0].toUpperCase() + unit.slice(1), 'period'), ...Array.from({ length: 10 }, (_, r) => column(`${heatSide} ${r * 10}-${r * 10 + 10} %`, `${r * 10}-${r * 10 + 10}`))]))),
        socMap, socAxis),
      h('div', { class: 'habit-block', cover: ['cs.chart.charge_delta', 'cs.chart.charge_delta.band'] },
        h('div', { class: 'chart-head' }, h('span', { class: 'chart-title' }, 'Each session, from its start level to its end level'),
          h('span', { class: 'chart-side' }, deltaRead, csvOf('charging-start-end', rows, [column('Car', (c) => carName(c.car)), column('Charge', 'id'), column('Start', (c) => iso(c.start_ms)),
            column('Type', (c) => KIND[c.kind] || 'Unknown'), column('Start level', 'start_soc_pct', 'percent'), column('End level', 'end_soc_pct', 'percent')]))),
        h('div', { class: 'legend' }, ['ac', 'dc', 'unknown'].map((k) => h('span', { class: 'legend-chip legend-chip--static' }, h('i', { class: ['legend-swatch', `split-${k}`] }), KIND[k])),
          h('span', { class: 'legend-chip legend-chip--static' }, h('i', { class: 'legend-swatch legend-band' }), 'daily band')),
        deltaEl,
        h('p', { class: 'note' }, 'Hover either chart: the other one points at the same moment. Click a bar to open that charge.')),
      h('div', { class: 'habit-lists' },
        h('div', { cover: ['cs.table.discharge_stats', 'cs.table.discharge_stats.soc', 'cs.table.discharge_stats.count'] },
          h('div', { class: 'list-head' }, h('h3', { class: 'list-title' }, 'Level when plugging in'), csvOf('charging-start-bands', s.start_bands || [], bandCols)),
          bandList(s.start_bands, 'start'),
          h('p', { class: 'note', cover: 'cs.text.discharge_stats_help' }, 'How low the battery is when charging starts. Every session counts once, at its starting level; back-to-back top-ups are listed below as well, so you can see which starts were really low. Below 10 % is red, below 20 % amber.')),
        h('div', { cover: ['cs.table.charge_stats', 'cs.table.charge_stats.soc', 'cs.table.charge_stats.count'] },
          h('div', { class: 'list-head' }, h('h3', { class: 'list-title' }, 'Level when charging stops'), csvOf('charging-end-bands', s.end_bands || [], bandCols)),
          bandList(s.end_bands, 'end'),
          h('p', { class: 'note' }, `Where sessions end. Amber bands are above the daily band (${upper} %).`)),
        h('div', null,
          h('div', { class: 'list-head' }, h('h3', { class: 'list-title' }, 'When in the week'), csvOf(`charging-${heatSide}-week-hours`, () => (s.heatmap?.[heatSide] || []).map((r, d) => ({ day: DAYS[d], ...Object.fromEntries(r.map((n, hr) => [`h${hr}`, n])) })),
            [column('Day', 'day'), ...Array.from({ length: 24 }, (_, hr) => column(`${String(hr).padStart(2, '0')}:00`, `h${hr}`))])),
          week,
          h('p', { class: 'note' }, 'Sessions by the hour they start (or end), in each car’s own time.'))),
      runsTable.el);
    paintSocMap();
    paintWeek();

    // the delta chart (uPlot): x = start time, a bar from the start to the end level per session
    await loadUPlot();
    if (!ctx.alive()) return;
    deltaEl.hidden = !rows.length;
    if (!rows.length) return;
    const xs = rows.map((c) => c.start_ms / 1000);
    const lo = rows.map((c) => c.start_soc_pct);
    const hi = rows.map((c) => c.end_soc_pct ?? c.start_soc_pct);
    const spanMs = (to - from);
    const make = (width, height) => {
      const c = colors();
      const pal = { ac: cssVar('--c-charge'), dc: cssVar('--c-power'), unknown: cssVar('--st-unknown'), band: cssVar('--live') };
      return {
        width, height, padding: [rpx(10), rpx(14), 0, 0], legend: { show: false },
        cursor: { sync: { key: 'charges-habits', setSeries: false }, y: false, points: { show: false }, drag: { x: false, y: false } },
        scales: { x: { time: true, range: () => [from / 1000, to / 1000] }, y: { range: () => [0, 100] } },
        axes: [timeAxis(tz, { spanMs }), { ...valueAxis({ size: 44 }), values: (u, v) => v.map((n) => `${n} %`), splits: () => [0, 20, 40, 60, 80, 100] }],
        series: [{}, { stroke: 'transparent', width: 0, points: { show: false } }, { stroke: 'transparent', width: 0, points: { show: false } }],
        hooks: {
          drawAxes: [(u) => {
            const g = u.ctx; const { left, width: w } = u.bbox;
            const y20 = u.valToPos(20, 'y', true), yU = u.valToPos(upper, 'y', true);
            g.save();
            g.fillStyle = hexA(pal.band, 0.08); g.fillRect(left, yU, w, y20 - yU);
            g.strokeStyle = hexA(pal.band, 0.55); g.lineWidth = Math.max(1, rpx(1) * pxr(u)); g.setLineDash([rpx(4) * pxr(u), rpx(4) * pxr(u)]);
            for (const y of someLfp ? [y20, yU, u.valToPos(100, 'y', true)] : [y20, yU]) { g.beginPath(); g.moveTo(left, y); g.lineTo(left + w, y); g.stroke(); }
            g.setLineDash([]); g.fillStyle = c.axis; g.font = chartFont(10).replace(/(\d+)px/, (_, n) => `${Math.round(n * pxr(u))}px`); g.textAlign = 'left';
            g.fillText(`${upper} %`, left + rpx(4) * pxr(u), yU - rpx(4) * pxr(u));
            g.restore();
          }],
          draw: [(u) => {
            const g = u.ctx;
            const bw = Math.max(rpx(2.5), Math.min(rpx(9), (u.bbox.width / pxr(u) / Math.max(1, rows.length)) * 0.6)) * pxr(u);
            g.save();
            rows.forEach((r, i) => {
              const x = u.valToPos(xs[i], 'x', true);
              const y1 = u.valToPos(lo[i], 'y', true), y2 = u.valToPos(hi[i], 'y', true);
              g.fillStyle = hexA(pal[r.kind] || pal.unknown, u.cursor.idx === i ? 1 : 0.78);
              g.fillRect(x - bw / 2, Math.min(y1, y2), bw, Math.max(rpx(2) * pxr(u), Math.abs(y1 - y2)));
            });
            g.restore();
          }],
          setCursor: [(u) => {
            const i = u.cursor.idx;
            if (i == null || u.cursor.left < 0) { idleDelta(); return; }
            const r = rows[i];
            replace(deltaRead, `${multi ? `${carName(r.car)} · ` : ''}${formatDate(r.start_ms, r.time_zone || tz, { year: false })}, ${formatTime(r.start_ms, r.time_zone || tz)} · `, h('b', null, `${pct(r.start_soc_pct)} → ${pct(r.end_soc_pct)}`), ` · ${KIND[r.kind] || 'Unknown'} · ${placeLabel(r)}`);
            const k = colIndex.get(bucketKey(r.start_ms, unit, tz));
            showCol(k ?? null, true);
            u.redraw(false, false);
          }],
        },
      };
    };
    delta = mountPlot(deltaEl, make, [xs, lo, hi], ctx, (u) => {
      u.over.classList.add('is-clickable');
      u.over.addEventListener('click', () => { const i = u.cursor.idx; if (i != null && rows[i]) ctx.navigate(state.buildUrl('charge', { car: rows[i].car, charge: rows[i].id })); });
      u.over.addEventListener('mouseleave', () => showCol(null, true));
    });
  }

  /* charging places: a map and two paged tables */
  async function paintPlaces(sp) {
    const pp = { cars: fixedCar ? fixedCar.id : sp.cars, from_ms: sp.from_ms, to_ms: sp.to_ms, places: sp.places, min_duration_min: sp.min_duration_min };
    const placeCell = (p) => (p.place ? h('a', { class: 'main place-link', href: state.buildUrl('places', { query: { place: p.place } }), title: `Edit the place ${p.label}` }, p.label)
      : h('a', { class: 'main place-link', href: state.buildUrl('places', { query: { lat: p.lat.toFixed(5), lon: p.lon.toFixed(5), new: 1 } }), title: 'Make this spot a saved place' }, p.label));
    const placeCol = (cv) => ({ key: 'label', label: 'Place', cover: cv, render: (p) => h('span', { class: 'cell-text' }, placeCell(p),
      h('span', { class: 'sub' }, [p.place ? null : 'not a saved place', multi && p.cars?.length ? p.cars.map(carName).join(', ') : null].filter(Boolean).join(' · '))), csv: (p) => p.label });
    const sessionsCol = { key: 'sessions', label: 'Sessions', num: true, render: (p) => formatNumber(p.sessions) };
    const byEnergy = dataTable(ctx, {
      title: 'By energy', icon: 'bolt', path: 'charging-stats/places', params: { ...pp, sort: 'energy' }, limit: 10, csvName: 'charging-places-energy', cls: 'places-table', cover: 'cs.table.top_energy',
      columns: [placeCol('cs.table.top_energy.location'), sessionsCol, { key: 'energy', label: 'Added', num: true, cover: 'cs.table.top_energy.charged', render: (p) => format(p.energy_added_kwh, 'energy') }],
      empty: () => emptyState({ icon: 'pin', title: 'No charging place', text: 'No charge in this period.' }),
    });
    const byCost = dataTable(ctx, {
      title: 'By cost', icon: 'stats', path: 'charging-stats/places', params: { ...pp, sort: 'cost' }, limit: 10, csvName: 'charging-places-cost', cls: 'places-table', cover: 'cs.table.top_cost',
      columns: [placeCol('cs.table.top_cost.location'), sessionsCol,
        { key: 'cost', label: 'Cost', num: true, cover: 'cs.table.top_cost.cost', render: (p) => (p.cost_by_currency?.length ? moneyStack(p.cost_by_currency) : h('span', { class: 'faint' }, 'no price')) },
        { key: 'ppk', label: 'Per kWh', num: true, render: (p) => (p.price_per_kwh != null && p.cost_by_currency?.length === 1 ? format({ amount: p.price_per_kwh, currency: p.cost_by_currency[0].currency }, 'price_energy') : '—') }],
      empty: () => emptyState({ icon: 'pin', title: 'No charging place', text: 'No charge in this period.' }),
    });
    const mapEl = h('div', { class: 'map places-map' });
    const mapRead = h('span', { class: 'chart-read' });
    let pts = [];
    const mapBox = h('div', { class: 'places-map-wrap', cover: 'cs.map.energy_by_location' },
      h('div', { class: 'chart-head' }, h('span', { class: 'chart-title' }, 'Energy by place'), h('span', { class: 'chart-side' }, mapRead,
        csvOf('charging-places-map', () => pts, [column('Place', 'label'), column('Place id', 'place'), column('Latitude', 'lat'), column('Longitude', 'lon'), column('Sessions', 'sessions'), column('Energy added', 'energy_added_kwh', 'energy')]))),
      mapEl, h('p', { class: 'note' }, 'Grouped by saved place, else by position, never by address text, so one spot is never split in two. Bigger circles took more energy; click one for its charges.'));
    replace(placesPanel, head('pin', 'Charging places'), h('div', { class: 'places-grid' }, mapBox, h('div', { class: 'places-tables' }, byEnergy.el, byCost.el)));
    try { pts = (await ctx.api.page('charging-stats/places', { ...pp, sort: 'energy' }, { limit: 200 })).items || []; } catch { pts = []; }
    if (!ctx.alive()) return;
    replace(mapRead, pts.length ? `${formatNumber(pts.length)} places${pts.length >= 200 ? ' (the 200 with most energy)' : ''}` : 'no place');
    const mapping = (async () => {
      if (!pts.length) { mapEl.append(h('div', { class: 'map-fallback' }, 'No charge in this period, so there is nothing to map.')); return; }
      let m;
      try { m = await createMap(mapEl, ctx, { bounds: boundsOf(pts.map((p) => [p.lon, p.lat])), zoom: 10 }); } catch { replace(mapEl, mapFallback()); return; }
      if (!m) return;
      await m.ready;
      const max = Math.max(1, ...pts.map((p) => p.energy_added_kwh || 0));
      m.map.addSource('cplaces', { type: 'geojson', data: { type: 'FeatureCollection', features: pts.map((p, i) => ({ type: 'Feature', id: i, properties: { i, r: 4 + 14 * Math.sqrt((p.energy_added_kwh || 0) / max) }, geometry: { type: 'Point', coordinates: [p.lon, p.lat] } })) } });
      m.addLayer({ id: 'cplaces', type: 'circle', source: 'cplaces', paint: { 'circle-color': cssVar('--c-charge'), 'circle-opacity': 0.55, 'circle-stroke-color': cssVar('--c-charge') } },
        { 'circle-radius': () => ['*', ['get', 'r'], rpx(1)], 'circle-stroke-width': () => rpx(1.5) });
      m.map.on('mousemove', 'cplaces', (e) => {
        m.map.getCanvas().classList.add('is-clickable');
        const p = pts[e.features?.[0]?.properties?.i];
        if (p) replace(mapRead, h('b', null, p.label), ` · ${format(p.energy_added_kwh, 'energy')} · ${formatNumber(p.sessions)} ${p.sessions === 1 ? 'session' : 'sessions'}`);
      });
      m.map.on('mouseleave', 'cplaces', () => { m.map.getCanvas().classList.remove('is-clickable'); replace(mapRead, `${formatNumber(pts.length)} places`); });
      m.map.on('click', 'cplaces', (e) => {
        const p = pts[e.features?.[0]?.properties?.i];
        if (!p) return;
        if (p.place) { f.places = p.place; ctx.setQuery(queryOf()); ctx.reload(); } else ctx.navigate(state.buildUrl('places', { query: { lat: p.lat.toFixed(5), lon: p.lon.toFixed(5), new: 1 } }));
      });
      m.frame(() => { if (pts.length === 1) m.map.jumpTo({ center: [pts[0].lon, pts[0].lat], zoom: 13 }); else m.map.fitBounds(boundsOf(pts.map((p) => [p.lon, p.lat])), { padding: rpx(36), duration: 0, maxZoom: 14 }); });
    })();
    await Promise.all([byEnergy.ready, byCost.ready, mapping]);
  }

  /* fast-charging curve (one car at a time) */
  let curveCar = q.curve_car && viewCars.includes(q.curve_car) ? q.curve_car : null;
  async function paintCurve(rows, my) {
    const dcCount = new Map();
    for (const c of rows) if (c.kind === 'dc') dcCount.set(c.car, (dcCount.get(c.car) || 0) + 1);
    if (!curveCar || !viewCars.includes(curveCar)) curveCar = fixedCar ? fixedCar.id : ([...dcCount.entries()].sort((a, b) => b[1] - a[1])[0]?.[0] || viewCars[0]);
    const carSel = multi && viewCars.length > 1 ? h('select', { class: 'select select--sm', 'aria-label': 'Car for the curve' },
      viewCars.map((cid) => h('option', { value: cid, selected: cid === curveCar }, `${carName(cid)} (${formatNumber(dcCount.get(cid) || 0)} DC)`))) : null;
    carSel?.addEventListener('change', () => { curveCar = carSel.value; ctx.setQuery({ curve_car: curveCar }); paintCurve(rows, statsGen); });
    const plotEl = h('div', { class: 'curve-chart' });
    const read = h('span', { class: 'chart-read' });
    let data = { median: [], sessions: [] };
    const pal = () => ({ cold: cssVar('--c-power'), mild: cssVar('--live'), warm: cssVar('--warm'), unknown: cssVar('--st-unknown') });
    const legend = h('div', { class: 'curve-legend' },
      h('span', { class: 'legend-chip legend-chip--static' }, h('i', { class: 'legend-swatch legend-median' }), 'median of every session'),
      h('span', { class: 'temp-scale' }, h('span', null, `battery ${format(TEMP_STOPS[0], 'temperature', { digits: 0 })}`), h('i', { class: 'temp-ramp' }), h('span', null, format(TEMP_STOPS[2], 'temperature', { digits: 0 }))));
    replace(curvePanel,
      head('bolt', 'Fast-charging curve', carSel, csvOf('dc-curve', () => [
        ...data.median.map((p) => ({ kind: 'median', charge: '', label: '', start: '', ...p, battery_c: null })),
        ...data.sessions.flatMap((s) => s.points.map((p) => ({ kind: 'session', charge: s.charge, label: s.label, start: iso(s.start_ms), ...p }))),
      ], [column('Row', 'kind'), column('Charge', 'charge'), column('Place', 'label'), column('Start', 'start'), column('Battery level', 'soc_pct', 'percent'), column('Power', 'power_kw', 'power'), column('Battery temperature', 'battery_c', 'temperature')], 'cs.chart.dc_curve_sessions')),
      h('div', { class: 'chart-head' }, h('span', { class: 'chart-title' }, `Power against battery level, every DC session${multi ? ` of ${carName(curveCar)}` : ''}`), h('span', { class: 'chart-side' }, read)),
      legend, h('div', { class: 'curve-wrap', cover: ['cs.chart.dc_curve_median', 'cs.link.dc_curve_details'] }, plotEl),
      h('p', { class: 'note' }, 'One dot per battery percent of each session, coloured by battery temperature (a cold pack charges slower). The line is this car’s median power at each percent. Hover a dot to see its session, click it to open the charge.'));
    try { data = await ctx.api.get(`cars/${enc(curveCar)}/charging-stats/dc-curve`, { from_ms: ctx.params.from_ms, to_ms: ctx.params.to_ms, places: f.places || null }); } catch (e) { replace(plotEl, h('p', { class: 'note warn' }, `The curve could not be loaded: ${e.message}`)); return; }
    if (my !== statsGen || !ctx.alive()) return;
    const sessions = data.sessions || [];
    const idle = () => replace(read, sessions.length ? `${formatNumber(sessions.length)} DC ${sessions.length === 1 ? 'session' : 'sessions'}${data.median?.length ? `; median peak ${format(Math.max(...data.median.map((p) => p.power_kw)), 'power')}` : ''}` : 'no DC session');
    idle();
    if (!sessions.length) { replace(plotEl, h('div', { class: 'map-fallback' }, `${carName(curveCar)} did not fast-charge in this period.`)); return; }
    await loadUPlot();
    if (!ctx.alive()) return;
    const tzc = data.time_zone || tz;
    const maxP = Math.max(10, ...sessions.flatMap((s) => s.points.map((p) => p.power_kw || 0)));
    let dots = [], hover = null;
    const make = (width, height) => {
      const c = colors();
      const p = pal();
      const med = cssVar('--text');
      return {
        width, height, padding: [rpx(10), rpx(14), 0, 0], legend: { show: false },
        cursor: { x: false, y: false, points: { show: false }, drag: { x: false, y: false } },
        scales: { x: { time: false, range: () => [0, 100] }, y: { range: () => [0, maxP * 1.08] } },
        axes: [{ ...valueAxis({ size: 26 }), values: (u, v) => v.map((n) => `${n} %`), space: rpx(60) }, { ...valueAxis({ size: 48 }), values: (u, v) => v.map((n) => `${formatNumber(n)} kW`) }],
        series: [{}, { stroke: med, width: rpx(2.5), points: { show: true, size: rpx(6), stroke: med, fill: med }, spanGaps: false }],
        hooks: {
          drawAxes: [(u) => {
            const g = u.ctx; const pr = pxr(u); const r = rpx(2.6) * pr;
            dots = [];
            g.save();
            for (const s of sessions) {
              const on = hover && hover.s === s;
              if (on) {
                g.strokeStyle = hexA(c.text, 0.8); g.lineWidth = rpx(1.5) * pr; g.beginPath();
                s.points.forEach((pt, k) => { const x = u.valToPos(pt.soc_pct, 'x', true), y = u.valToPos(pt.power_kw, 'y', true); if (k) g.lineTo(x, y); else g.moveTo(x, y); });
                g.stroke();
              }
              for (const pt of s.points) {
                const x = u.valToPos(pt.soc_pct, 'x', true), y = u.valToPos(pt.power_kw, 'y', true);
                g.globalAlpha = on ? 1 : 0.7; g.fillStyle = tempColor(pt.battery_c, p);
                g.beginPath(); g.arc(x, y, on ? r * 1.4 : r, 0, Math.PI * 2); g.fill(); g.globalAlpha = 1;
                dots.push({ x: x / pr, y: y / pr, s, pt });
              }
            }
            g.restore();
          }],
        },
      };
    };
    const medX = (data.median || []).map((p) => p.soc_pct);
    const medY = (data.median || []).map((p) => p.power_kw);
    const plot = mountPlot(plotEl, make, [medX, medY], ctx, (u) => {
      u.over.addEventListener('mousemove', (e) => {
        const x = e.offsetX + u.bbox.left / pxr(u), y = e.offsetY + u.bbox.top / pxr(u);
        let best = null, bd = rpx(10) ** 2;
        for (const d of dots) { const dd = (d.x - x) ** 2 + (d.y - y) ** 2; if (dd < bd) { bd = dd; best = d; } }
        const changed = (best?.s || null) !== (hover?.s || null);
        hover = best;
        u.over.classList.toggle('is-clickable', !!best);
        if (best) replace(read, `${best.s.label} · ${formatDate(best.s.start_ms, tzc, { year: false })}, ${formatTime(best.s.start_ms, tzc)} · `, h('b', null, `${pct(best.pt.soc_pct)}: ${format(best.pt.power_kw, 'power')}`), ` · battery ${format(best.pt.battery_c, 'temperature')}`);
        else idle();
        if (changed) u.redraw(false, false);
      });
      u.over.addEventListener('mouseleave', () => { if (hover) { hover = null; u.redraw(false, false); } idle(); });
      u.over.addEventListener('click', () => { if (hover) ctx.navigate(state.buildUrl('charge', { car: curveCar, charge: hover.s.charge })); });
    });
    return plot;
  }

  // a charge started or ended: load everything again (a moment later, so bursts load once)
  let rowsTimer = 0;
  ctx.events('rows', (ev) => {
    if (ev.data?.car && !viewCars.includes(ev.data.car)) return;
    if (ev.data?.kinds && !ev.data.kinds.includes('charges')) return;
    clearTimeout(rowsTimer);
    rowsTimer = setTimeout(() => { if (!ctx.alive()) return; table.reload(listParams()); incomplete.reload(); loadStats(); }, 2000);
  });
  ctx.onCleanup(() => clearTimeout(rowsTimer));

  replace(root,
    pageHead({ eyebrow: scopeName, title: 'Charges', sub: [period.label, multi ? `days in ${tz.replace(/_/g, ' ')} time; each charge in its car’s time` : `times in ${tz.replace(/_/g, ' ')}`] }),
    filters, summary, table.el, incomplete.el,
    h('div', { class: 'stats-head' }, h('h2', { class: 'section-title' }, 'Charging statistics'), scopeLine),
    h('div', { class: 'stats-grid' }, totalsPanel, splitPanel), habitsPanel, placesPanel, curvePanel, ...pops);
  paintSummary(null);

  try { await Promise.all([table.ready, incomplete.ready, loadStats()]); } catch (e) { ctx.fail(e); return; }
  ctx.ready();
}
