// Sentry Mate, "Pebble" UI pack: Charges (/cars/{car}/charges, or /charges for every car), charging sessions as
// tiles grouped by day, then what the charging habits and costs look like.
//
// A pill row picks AC or DC and Filters opens a sheet (place, words in the address, lowest cost, shortest charging
// time). Then the totals of the listed charges, the charges with missing data, one tile per session under friendly
// day headers (each with a coral bar from its start to its end level), and the statistics: costs and totals, the AC
// and DC split, when in the week sessions start or end, each session from its start to its end level, the levels
// when plugging in and when charging stops, the charging places on a map and the fast-charging curve.
// Data: GET /api/v1/cars/{car}/charges (paged, totals), /charging-stats, /charging-stats/places, /charging-stats/dc-curve.

import { h } from '../shared/dom.js';
import { format, formatMoney, unitLabel } from '../shared/units.js';
import { formatTime, formatDate, formatDuration } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { tile, num, chip, btn, icon, note } from '../core/ui.js';
import { emptyTile } from '../core/states.js';
import { createSheet } from '../core/sheet.js';
import { field, segmented } from '../core/form.js';
import { mapTile, boundsOf } from '../core/map.js';
import { xyChart } from '../core/chart.js';
import { byDay, dayHead, socBar, filterBar, moreButton, statTile, pills, money, splitBar, sectionHead, dur } from '../core/bits.js';
import { srcPill, estPill, SOURCE_OPTIONS, SOURCE_WORDS, sourceParam } from '../core/source.js';

export const id = 'charges';
export const css = true;
export const shape = [['full', 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [2, 'h2'], [2, 'h2']];
export const title = () => 'Charges';

const KIND = { ac: 'AC', dc: 'DC', unknown: 'Unknown' };
const KIND_TONE = { ac: 'teal', dc: 'sky', unknown: 'muted' };
const COST_SOURCE = { tariff: 'place price', manual: 'typed by hand', free_supercharging: 'free Supercharging', teslamate: 'imported' };
const DAYS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
const pct = (v) => format(v, 'percent');
const placeName = (c) => c?.place?.name || c?.address?.short || 'Unnamed spot';
const placeHref = (c) => (c?.place?.id ? state.buildUrl('places', { query: { place: c.place.id } })
  : c?.position?.lat != null ? state.buildUrl('places', { query: { new: `${c.position.lat.toFixed(5)},${c.position.lon.toFixed(5)}` } }) : null);

/** One charge as a tile: place, time, kind, the main numbers, the level bar, more figures and links. */
function chargeTile(c, ctx, tz, carName) {
  const going = !!c.flags?.in_progress || c.end_ms == null;
  const zone = c.time_zone || tz;
  const href = state.buildUrl('charge', { car: c.car, charge: c.id });
  const costHref = state.buildUrl('charge', { car: c.car, charge: c.id, query: { edit: 'cost' } });
  const pHref = placeHref(c);
  const kind = c.kind || 'unknown';
  return tile({ w: 2, cls: ['item', 'charge-tile'] },
    h('div', { class: 'item-top' },
      h('span', { class: 'item-ic tone-teal', 'aria-hidden': 'true' }, icon('bolt')),
      h('div', { class: 'item-text' },
        h('p', { class: 'item-title', cover: ['charges.table.location', 'charges.link.geofence'] },
          pHref ? h('a', { class: 'place-link', href: pHref, title: c.place ? `Edit ${c.place.name}` : 'Name this place' }, placeName(c)) : placeName(c)),
        h('p', { class: 'item-sub', cover: 'charges.table.date' }, `${carName ? `${carName} · ` : ''}${formatTime(c.start_ms, zone)} – ${going ? 'now' : formatTime(c.end_ms, zone)}`)),
      h('span', { class: 'item-flags' },
        h('span', { cover: 'charges.table.type' }, chip(c.fast_charger?.type === 'supercharger' ? `${KIND[kind]} · Supercharger` : KIND[kind], { tone: KIND_TONE[kind] })),
        going ? chip('Charging now', { tone: 'teal' }) : null,
        c.has_gaps ? chip('Some data missing', { tone: 'bad' }) : null,
        null)),
    h('div', { class: 'item-nums' },
      h('span', { cover: 'charges.table.energy_added' }, num(c.energy_added_kwh, 'energy', { size: 's' })),
      h('span', { cover: 'charges.table.duration' }, num(c.charging_ms, 'duration', { size: 's' })),
      h('span', { cover: ['charges.table.cost', 'charges.link.set_cost'] },
        ctx.admin ? h('a', { class: ['cost-link', { 'is-missing': !c.cost }], href: costHref, title: c.cost ? 'Change the cost' : 'Set a cost' },
          c.cost ? formatMoney(c.cost.amount, c.cost.currency) : 'Set a cost')
          : h('span', { class: ['cost-link', { 'is-missing': !c.cost }] }, c.cost ? formatMoney(c.cost.amount, c.cost.currency) : 'No price'))),
    socBar(c.start_soc_pct, c.end_soc_pct, { cover: ['charges.table.start_soc', 'charges.table.end_soc'] }),
    h('p', { class: 'src-line' }, srcPill(c, { short: true }), estPill(c)),
    h('p', { class: 'item-sub charge-more' },
      h('span', { cover: 'charges.table.energy_used' }, c.energy_grid_kwh != null ? `${format(c.energy_grid_kwh, 'energy')} from the grid` : c.dc_inlet_kwh != null ? `${format(c.dc_inlet_kwh, 'energy')} at the inlet` : 'grid not measured'), ' · ',
      h('span', { cover: 'charges.table.efficiency' }, c.efficiency_pct != null ? `${pct(c.efficiency_pct)} efficient` : kind === 'dc' ? 'efficiency unknown (DC)' : 'efficiency —'), ' · ',
      h('span', { cover: 'charges.table.cost_per_kwh' }, c.price_per_kwh?.value != null ? format({ amount: c.price_per_kwh.value, currency: c.cost?.currency }, 'price_energy') : 'no price per kWh'), ' · ',
      h('span', { cover: 'charges.table.avg_power' }, `avg ${format(c.power_avg_kw, 'power')}, peak ${format(c.power_max_kw, 'power')}`), ' · ',
      h('span', { cover: 'charges.table.range_added' }, `+${format(c.range_added_km, 'range')}`), ' · ',
      h('span', { cover: 'charges.table.charge_rate' }, format(c.range_per_hour_km, 'range_rate')), ' · ',
      h('span', { cover: 'charges.table.temp' }, `${format(c.outside_avg_c, 'temperature')} outside`), ' · ',
      h('span', { cover: 'charges.table.odometer' }, format(c.odometer_km, 'odometer'))),
    h('p', { class: 'item-links tile-foot' },
      h('a', { href, cover: 'charges.link.view_details' }, 'Details'),
      c.cost ? h('span', { class: 'item-sub' }, COST_SOURCE[c.cost.source] || c.cost.source) : null));
}

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz;
  const Q = ctx.query;
  let fleet = [];
  if (!car) { try { fleet = (await ctx.api.page('cars', { sort: 'name' }, { limit: 200 })).items || []; } catch { fleet = []; } }
  const carName = (cid) => fleet.find((c) => c.id === cid)?.name || null;
  const listPath = car ? api.apiPath`cars/${car.id}/charges` : 'charges';
  const statsPath = car ? api.apiPath`cars/${car.id}/charging-stats` : 'charging-stats';
  const base = () => ({ ...ctx.params, cars: car ? undefined : 'all' });
  const f = { kind: ['ac', 'dc', 'unknown'].includes(Q.kind) ? Q.kind : '', place: Q.place || '', q: Q.q || '', min_cost: Number(Q.min_cost) > 0 ? Number(Q.min_cost) : null, min_dur: Number(Q.min_dur) > 0 ? Number(Q.min_dur) : null, source: sourceParam(Q.source) };
  const listParams = () => ({ ...base(), kind: f.kind || undefined, places: f.place || undefined, q: f.q || undefined, min_cost: f.min_cost ?? undefined, min_duration_min: f.min_dur ?? undefined, source: f.source || undefined });
  const statsParams = () => ({ ...base(), places: f.place || undefined, min_duration_min: f.min_dur ?? undefined });
  const write = () => ctx.setQuery({ kind: f.kind || null, place: f.place || null, q: f.q || null, min_cost: f.min_cost, min_dur: f.min_dur, source: f.source || null });
  const places = await ctx.api.page('places', { sort: 'visits' }, { limit: 200 }).then((p) => p.items || []).catch(() => []);
  if (!ctx.alive()) return {};

  /* ------------------------------------------------------------ the filter row and its sheet */
  const kindPills = pills({ label: 'AC or DC', value: f.kind, cover: 'charges.filter.charge_type', options: [['', 'AC and DC'], ['ac', 'AC'], ['dc', 'DC'], ['unknown', 'Unknown']],
    onchange: (v) => { f.kind = v; apply(); } });
  const filtersBtn = btn('Filters', { kind: 'soft', icon: 'filter', cover: ['charges.filter.geofence', 'charges.filter.location', 'charges.filter.min_cost', 'charges.filter.min_duration', 'cs.filter.geofence', 'cs.filter.min_duration'], onclick: (e) => { fill(); sheet.open(e.currentTarget); } });
  const filterNote = h('span', { class: 'filter-note' });
  const bar = filterBar(ctx, { carCover: ['charges.filter.car', 'cs.filter.car'], periodCover: ['charges.filter.time_range', 'cs.filter.time_range'] }, kindPills, filtersBtn, filterNote);
  const drawNote = () => {
    filterNote.textContent = [f.place && `at ${places.find((p) => p.id === f.place)?.name || 'a place'}`, f.q && `“${f.q}”`, f.min_cost && `cost at least ${formatNumber(f.min_cost)}`, f.min_dur && `charging at least ${f.min_dur} min`, f.source && (SOURCE_WORDS[f.source] || f.source)].filter(Boolean).join(' · ');
  };
  const sheet = createSheet({ title: 'Filters', cls: 'filters-sheet' });
  ctx.onCleanup(() => sheet.el.remove());
  function fill() {
    const fPlace = field({ label: 'Saved place', name: 'place', type: 'select', value: f.place, options: [['', 'Any place'], ...places.map((p) => [p.id, p.name])], hint: 'This also narrows the statistics further down.' });
    const fQ = field({ label: 'Words in the place or address', name: 'q', value: f.q, attrs: { autocomplete: 'off' } });
    const fCost = field({ label: 'Lowest cost (in its currency)', name: 'min_cost', type: 'number', value: f.min_cost ?? '', attrs: { min: '0', step: '0.01', inputmode: 'decimal' } });
    const fDur = field({ label: 'Shortest charging time (min)', name: 'min_dur', type: 'number', value: f.min_dur ?? '', hint: 'Time charging only; waiting while plugged in does not count.', attrs: { min: '0', step: '1', inputmode: 'numeric' } });
    const fSrc = segmented({ label: 'Data from', name: 'source', value: f.source === 'import:teslamate' ? 'import' : f.source, options: SOURCE_OPTIONS });
    const n = (el) => { const x = Number(el.control.value); return el.control.value === '' || !(x > 0) ? null : x; };
    sheet.body.replaceChildren(h('div', { class: 'form' }, fPlace, fQ, fCost, fDur, fSrc,
      h('p', { class: 'hint' }, 'SMO is the car’s CAN Bridge, the Tesla API is the Tesla account this server reads, Imported is history from TeslaMate. This narrows the list, not the statistics.'),
      h('div', { class: 'sheet-actions' },
        btn('Clear', { kind: 'soft', onclick: () => { Object.assign(f, { place: '', q: '', min_cost: null, min_dur: null, source: '' }); sheet.close(); apply(); } }),
        btn('Show charges', { kind: 'primary', onclick: () => { f.place = fPlace.control.value; f.q = fQ.control.value.trim(); f.min_cost = n(fCost); f.min_dur = n(fDur); f.source = fSrc.querySelector('[aria-checked="true"]')?.dataset.value || ''; sheet.close(); apply(); } }))));
  }
  fill();
  async function apply() { write(); drawNote(); await Promise.all([loadList(), loadStats()]); ctx.ready(); }

  /* ------------------------------------------------------------ totals, missing data, the list */
  const sumBox = h('div', { class: 'contents' });
  const incBox = h('div', { class: 'contents' });
  const listHead = sectionHead('Sessions', 'Charging time leaves out time plugged in but not charging, so average power is not diluted. DC efficiency is unknown: a fast charger’s grid side is not measured.', { cover: 'charges.text.list_help' });
  const listStart = h('div', { class: 'tl-list-start w-full', cover: ['charges.table', 'shots.charges'] });
  let pager = null, lastDay = null;
  const more = moreButton(async () => { const b = pager.items.length; await pager.more(); drawFrom(b); return !pager.done; }, { text: 'Show earlier charges' });
  function drawFrom(start) {
    for (const g of byDay(pager.items.slice(start), tz)) {
      if (g.key !== lastDay) {
        const day = pager.items.filter((c) => byDay([c], tz)[0].key === g.key);
        const kwh = day.reduce((s, c) => s + (c.energy_added_kwh || 0), 0);
        statsBox.before(dayHead(g.key, tz, `${day.length} ${day.length === 1 ? 'charge' : 'charges'} · ${format(kwh, 'energy')}`));
        lastDay = g.key;
      }
      for (const c of g.items) statsBox.before(chargeTile(c, ctx, tz, car ? null : carName(c.car)));
    }
    more.querySelector('button').hidden = pager.done;
    statsBox.before(more);
  }
  function drawSummary(t) {
    t = t || {};
    const n = t.count ?? 0;
    sumBox.replaceChildren(
      statTile({ label: 'Energy added', value: n ? t.energy_added_kwh : null, kind: 'energy', icon: 'bolt', cover: 'charges.stat.energy_added', why: 'no charge', sub: `${formatNumber(n)} ${n === 1 ? 'session' : 'sessions'}` }),
      statTile({ label: 'From the grid', value: n && t.energy_grid_known ? t.energy_grid_kwh : null, kind: 'energy', icon: 'plug', cover: 'charges.stat.energy_used', why: n ? 'no AC meter reading' : 'no charge', sub: n ? `AC, measured on ${formatNumber(t.energy_grid_known || 0)} of ${formatNumber(n)}` : 'AC, measured' }),
      tile({ w: 1, title: 'Cost', icon: 'coin', cls: 'tile-stat', cover: 'charges.stat.cost' }, money(t.cost_by_currency, { why: n ? 'no price yet' : 'no charge' }),
        h('p', { class: 'tile-line' }, n ? `${formatNumber(n - (t.unpriced || 0))} of ${formatNumber(n)} priced` : 'per currency')),
      statTile({ label: 'Average charging time', value: n ? t.avg_duration_ms : null, kind: 'duration', icon: 'clock', cover: 'charges.stat.avg_duration', why: 'no charge', sub: 'per session, charging only' }));
  }
  function drawIncomplete(res) {
    const list = res?.items || [];
    incBox.replaceChildren(tile({ w: 2, title: 'Charges with missing data', icon: 'warning', cls: 'inc-tile', cover: ['charges.incomplete.table', 'charges.incomplete.col.id', 'charges.incomplete.col.start_date', 'charges.incomplete.col.end_date', 'charges.incomplete.col.energy_added', 'charges.incomplete.col.energy_used', 'charges.incomplete.col.start_soc', 'charges.incomplete.col.end_soc', 'charges.incomplete.col.duration'] },
      h('p', { class: 'note', cover: 'charges.text.incomplete_help' }, 'A charge shows here when a log is missing inside it or it is still going. Its figures cover what was recorded; late logs fill it in by themselves.'),
      list.length ? h('ul', { class: 'inc-list' }, list.map((c) => h('li', null,
        h('a', { class: 'inc-id mono', href: state.buildUrl('charge', { car: c.car, charge: c.id }) }, `#${String(c.id).slice(-5)}`),
        h('span', { class: 'inc-when' }, `${formatDate(c.start_ms, c.time_zone || tz, { now: api.now() })}, ${formatTime(c.start_ms, c.time_zone || tz)} – ${c.end_ms ? formatTime(c.end_ms, c.time_zone || tz) : 'still going'}`),
        h('span', { class: 'inc-n' }, `+${format(c.energy_added_kwh, 'energy')} · grid ${format(c.energy_grid_kwh, 'energy')} · ${pct(c.start_soc_pct)} → ${pct(c.end_soc_pct)} · ${dur(c.charging_ms)}`)))) : h('p', { class: 'lede' }, 'Every charge here has all its data.')));
  }
  async function loadList() {
    pager = ctx.api.pager(listPath, listParams(), { limit: 30 });
    const [, inc] = await Promise.all([pager.more(), ctx.api.page(listPath, { ...base(), flags: 'gap,in_progress' }, { limit: 6 }).catch(() => null)]);
    if (!ctx.alive()) return;
    drawSummary(pager.last?.totals);
    drawIncomplete(inc);
    while (listStart.nextSibling && listStart.nextSibling !== statsBox) listStart.nextSibling.remove();
    lastDay = null;
    if (!pager.items.length) {
      const why = f.kind || f.place || f.q || f.min_cost || f.min_dur;
      statsBox.before(emptyTile({ icon: 'bolt', title: ctx.period?.preset === '7d' ? 'No charges this week' : 'No charges in this period', text: why ? 'No charge matches the filters. Clear them to see every charge.' : 'Pick a longer period above.' }));
    } else drawFrom(0);
  }

  /* ------------------------------------------------------------ statistics */
  const statsBox = h('div', { class: 'contents', cover: 'shots.charging-stats' });
  let heatSide = Q.heat === 'end' ? 'end' : 'start';
  async function loadStats() {
    const sp = statsParams();
    let s, rows;
    try {
      [s, rows] = await Promise.all([ctx.api.get(statsPath, sp), ctx.api.list(listPath, sp, { limit: 500 }).all({ max: 3000 })]);
    } catch (err) {
      if (!ctx.alive()) return;
      statsBox.replaceChildren(sectionHead('Charging statistics'), emptyTile({ icon: 'warning', title: 'The statistics did not load', text: err.message }));
      return;
    }
    if (!ctx.alive()) return;
    rows = rows.filter((c) => c.start_soc_pct != null).sort((a, b) => a.start_ms - b.start_ms);
    const ppk = s.price_per_kwh || {};
    const c100 = s.cost_per_100km || {};
    const bk = s.by_kind || {};
    const kinds = ['ac', 'dc', 'unknown'];
    const moneyTile = (label, list, cover, sub, kind = 'money', key = 'amount', ic = 'coin') => tile({ w: 1, title: label, icon: ic, cls: 'tile-stat', cover }, money(list, { kind, key, why: 'no price' }), h('p', { class: 'tile-line' }, sub));

    // when in the week (Monday first, the car's own time) and the start-to-end band of each session
    const heat = h('div', { class: 'week', role: 'img' });
    const heatRead = h('p', { class: 'tile-line week-read', 'aria-live': 'polite' });
    let cells = [];
    function drawHeat() {
      const g = s.heatmap?.[heatSide] || [];
      const max = Math.max(1, ...g.flat());
      heat.setAttribute('aria-label', `When sessions ${heatSide === 'start' ? 'start' : 'end'}, by weekday and hour`);
      cells = [];
      const kids = [h('span', { class: 'week-corner' })];
      for (let hr = 0; hr < 24; hr += 6) { const l = h('span', { class: 'week-x' }, formatTime(Date.UTC(2026, 0, 5, hr), 'UTC').replace(':00', '')); l.style.setProperty('--c', String(hr + 2)); kids.push(l); }
      for (let d = 0; d < 7; d++) {
        const y = h('span', { class: 'week-y' }, DAYS[d]); y.style.setProperty('--r', String(d + 2)); kids.push(y);
        for (let hr = 0; hr < 24; hr++) {
          const n = g[d]?.[hr] || 0;
          const c = h('span', { class: ['week-cell', { 'has-n': n > 0 }], title: `${DAYS[d]} ${String(hr).padStart(2, '0')}:00: ${n} ${n === 1 ? 'session' : 'sessions'}` });
          c.style.setProperty('--a', (n / max).toFixed(3)); c.style.setProperty('--c', String(hr + 2)); c.style.setProperty('--r', String(d + 2));
          cells.push(c); kids.push(c);
        }
      }
      heat.replaceChildren(...kids);
      heatRead.textContent = `${formatNumber(rows.length)} sessions; darker means more sessions ${heatSide === 'start' ? 'started' : 'ended'} in that hour.`;
    }
    const sideSeg = segmented({ name: 'heat', value: heatSide, options: [['start', 'Starts'], ['end', 'Ends']], onchange: (v) => { heatSide = v; ctx.setQuery({ heat: v === 'end' ? 'end' : null }); drawHeat(); } });
    drawHeat();
    const mark = (c) => {
      for (const el of cells) el.classList.remove('is-hit');
      if (!c) { heatRead.textContent = `${formatNumber(rows.length)} sessions; darker means more sessions ${heatSide === 'start' ? 'started' : 'ended'} in that hour.`; return; }
      const t = heatSide === 'start' ? c.start_ms : c.end_ms ?? c.start_ms;
      const p = new Intl.DateTimeFormat('en-US', { timeZone: c.time_zone || tz, weekday: 'short', hour: 'numeric', hourCycle: 'h23' }).formatToParts(t);
      const wd = DAYS.indexOf(p.find((x) => x.type === 'weekday')?.value), hr = Number(p.find((x) => x.type === 'hour')?.value) % 24;
      if (wd >= 0) cells[wd * 24 + hr]?.classList.add('is-hit');
      heatRead.textContent = `${formatDate(c.start_ms, c.time_zone || tz, { now: api.now() })}, ${formatTime(c.start_ms, c.time_zone || tz)}: ${pct(c.start_soc_pct)} → ${pct(c.end_soc_pct)} at ${placeName(c)}`;
    };
    const shown = rows.slice(-60);
    const delta = h('div', { class: 'deltas', role: 'list', 'aria-label': 'Each session from its start level to its end level' },
      h('span', { class: 'deltas-band', 'aria-hidden': 'true' }),
      shown.map((c) => {
        const lo = Math.max(0, Math.min(100, c.start_soc_pct)), hi = Math.max(lo, Math.min(100, c.end_soc_pct ?? lo));
        const fillEl = h('span', { class: ['delta-fill', `tone-${KIND_TONE[c.kind || 'unknown']}`] });
        fillEl.style.setProperty('--lo', (lo / 100).toFixed(4)); fillEl.style.setProperty('--hi', (hi / 100).toFixed(4));
        return h('a', { class: 'delta', role: 'listitem', href: state.buildUrl('charge', { car: c.car, charge: c.id }), title: `${formatDate(c.start_ms, c.time_zone || tz)}: ${pct(lo)} → ${pct(hi)}`,
          onmouseenter: () => mark(c), onfocus: () => mark(c), onmouseleave: () => mark(null), onblur: () => mark(null) }, fillEl);
      }));

    const bandList = (list, side) => {
      const max = Math.max(1, ...(list || []).map((b) => b.count));
      return h('ol', { class: 'rank' }, (list || []).slice().reverse().filter((b) => b.count > 0 || side === 'start').slice(0, 10).map((b) => {
        const lo = parseInt(b.band, 10);
        const fl = h('span', { class: 'rank-fill' }); fl.style.setProperty('--v', (b.count / max).toFixed(4));
        const tone = side === 'end' ? (lo >= 80 ? 'tone-c4' : 'tone-c1') : (lo < 20 ? 'tone-c4' : 'tone-c2');
        return h('li', { class: ['rank-row', tone] }, h('span', { class: 'rank-name' }, `${b.band.replace('-', '–')} %`), h('span', { class: 'rank-n' }, `${formatNumber(b.count)}×`), h('span', { class: 'rank-bar', 'aria-hidden': 'true' }, fl));
      }));
    };

    const placeBox = h('div', { class: 'map map-short' });
    const pp = { ...sp, cars: car ? car.id : 'all' };
    const [byEnergy, byCost] = await Promise.all([
      ctx.api.page('charging-stats/places', { ...pp, sort: 'energy' }, { limit: 100 }).then((p) => p.items || []).catch(() => []),
      ctx.api.page('charging-stats/places', { ...pp, sort: 'cost' }, { limit: 6 }).then((p) => p.items || []).catch(() => []),
    ]);
    if (!ctx.alive()) return;
    const placeRank = (list, val, cover) => {
      const max = Math.max(1, ...list.map(val));
      return h('ol', { class: 'rank', cover: cover[0] || null }, list.slice(0, 6).map((p) => {
        const fl = h('span', { class: 'rank-fill' }); fl.style.setProperty('--v', (val(p) / max).toFixed(4));
        const href = p.place ? state.buildUrl('places', { query: { place: p.place } }) : state.buildUrl('places', { query: { new: `${p.lat.toFixed(5)},${p.lon.toFixed(5)}` } });
        return h('li', { class: 'rank-row' }, h('a', { class: 'rank-name place-link', href, cover: cover[1] }, p.label),
          h('span', { class: 'rank-n', cover: cover[2] }, cover[2].endsWith('cost') ? (p.cost_by_currency?.length ? p.cost_by_currency.map((x) => formatMoney(x.amount, x.currency)).join(' + ') : 'no price') : format(p.energy_added_kwh, 'energy')),
          h('span', { class: 'rank-bar', 'aria-hidden': 'true' }, fl));
      }));
    };

    const curveT = tile({ w: 2, title: 'Fast-charging curve', icon: 'bolt', cls: 'chart-tile', cover: ['cs.chart.dc_curve_sessions', 'cs.chart.dc_curve_median', 'cs.link.dc_curve_details'] });
    statsBox.replaceChildren(
      sectionHead('Charging statistics', `Only the period, the cars, the place and the shortest charging time apply here.`),
      statTile({ label: 'Sessions', value: s.count ?? 0, kind: 'count', icon: 'bolt', cover: 'cs.stat.count', sub: `${formatNumber(ppk.priced ?? 0)} with a price` }),
      statTile({ label: 'Energy added', value: s.count ? s.energy_added_kwh : null, kind: 'energy', icon: 'battery', cover: 'cs.stat.energy_added', why: 'no charge', sub: 'into the pack' }),
      moneyTile('Total cost', s.cost_by_currency, 'cs.stat.total_cost', 'per currency, never added across'),
      moneyTile('Supercharger cost', s.supercharger_cost, 'cs.stat.supercharger_cost', 'sessions the charger names as a Supercharger'),
      moneyTile('Price per kWh', ppk.all, 'cs.stat.cost_per_kwh', 'every priced session', 'price_energy', 'value', 'coin'),
      moneyTile('AC per kWh', ppk.ac, 'cs.stat.cost_per_kwh_ac', 'over the grid energy, measured', 'price_energy', 'value', 'plug'),
      moneyTile('DC per kWh', ppk.dc, 'cs.stat.cost_per_kwh_dc', 'over the energy the charger billed', 'price_energy', 'value', 'bolt'),
      moneyTile(`Per 100 ${unitLabel('distance')}`, c100.value, 'cs.stat.cost_per_100', c100.distance_km != null ? `over ${format(c100.distance_km, 'distance')} driven` : 'no driving in the period', 'money_per_distance', 'amount', 'route'),
      tile({ w: 2, title: 'AC and DC', icon: 'plug' },
        splitBar({ label: 'Energy added', total: format(kinds.reduce((a, k) => a + (bk[k]?.kwh || 0), 0), 'energy'), cover: 'cs.chart.ac_dc_energy', parts: kinds.map((k) => ({ name: KIND[k], value: bk[k]?.kwh || 0, text: format(bk[k]?.kwh || 0, 'energy'), tone: k === 'ac' ? 'c3' : k === 'dc' ? 'c2' : 'c5' })) }),
        splitBar({ label: 'Time charging', total: formatDuration(kinds.reduce((a, k) => a + (bk[k]?.ms || 0), 0)), cover: 'cs.chart.ac_dc_duration', parts: kinds.map((k) => ({ name: KIND[k], value: bk[k]?.ms || 0, text: formatDuration(bk[k]?.ms || 0), tone: k === 'ac' ? 'c3' : k === 'dc' ? 'c2' : 'c5' })) }),
        note('From the charge port’s own AC or DC signal. A session without one is Unknown, never DC.')),
      tile({ w: 2, title: 'When in the week', icon: 'calendar', side: sideSeg, cover: ['cs.chart.heatmap', 'cs.filter.heatmap_window', 'cs.control.shared_crosshair'] },
        heat, heatRead),
      tile({ w: 2, title: 'Each session, start to end level', icon: 'battery', cover: ['cs.chart.charge_delta', 'cs.chart.charge_delta.band', 'cs.filter.charge_delta_window'] },
        rows.length ? delta : note('No session in this period.'),
        h('ul', { class: 'split-key' }, kinds.map((k) => h('li', null, h('span', { class: ['split-sw', `tone-${KIND_TONE[k]}`], 'aria-hidden': 'true' }), KIND[k])),
          h('li', null, h('span', { class: 'split-sw deltas-sw', 'aria-hidden': 'true' }), 'daily band 20–80 %')),
        note(`The last ${formatNumber(shown.length)} sessions of the period. Point at one to see it in the week above; tap it to open the charge.`)),
      tile({ w: 1, title: 'Level when plugging in', icon: 'battery', cover: ['cs.table.discharge_stats', 'cs.table.discharge_stats.soc', 'cs.table.discharge_stats.count'] },
        bandList(s.start_bands, 'start'), h('p', { class: 'note', cover: 'cs.text.discharge_stats_help' }, 'How low the battery is when charging starts. Below 20 % is in sun yellow.')),
      tile({ w: 1, title: 'Level when charging stops', icon: 'battery', cover: ['cs.table.charge_stats', 'cs.table.charge_stats.soc', 'cs.table.charge_stats.count'] },
        bandList(s.end_bands, 'end'), note(`${pct(s.share_above_80_pct)} ended above 80 %, ${pct(s.share_below_20_pct)} started below 20 %.`)),
      tile({ w: 2, title: 'Charging places', icon: 'pin', cls: 'map-tile', cover: 'cs.map.energy_by_location' }, placeBox,
        h('p', { class: 'map-foot' }, byEnergy.length ? `${formatNumber(byEnergy.length)} places; bigger circles took more energy.` : 'No charge in this period, so there is nothing to map.')),
      tile({ w: 1, title: 'Most energy', icon: 'bolt', cover: 'cs.table.top_energy' }, byEnergy.length ? placeRank(byEnergy, (p) => p.energy_added_kwh || 0, ['', 'cs.table.top_energy.location', 'cs.table.top_energy.charged']) : note('No charging place.')),
      tile({ w: 1, title: 'Most cost', icon: 'coin', cover: 'cs.table.top_cost' }, byCost.length ? placeRank(byCost, (p) => p.cost_by_currency?.[0]?.amount || 0, ['', 'cs.table.top_cost.location', 'cs.table.top_cost.cost']) : note('No charging place.')),
      curveT);

    const work = [];
    if (byEnergy.length) {
      work.push((async () => {
        const m = await mapTile(ctx, placeBox, { bounds: boundsOf(byEnergy.map((p) => [p.lon, p.lat])), padding: 40, maxZoom: 14 });
        if (!m || !m.map) return;
        const max = Math.max(1, ...byEnergy.map((p) => p.energy_added_kwh || 0));
        byEnergy.slice(0, 30).forEach((p, i) => {
          const r = 120 + 900 * Math.sqrt((p.energy_added_kwh || 0) / max);
          m.circle(`cp${i}`, [p.lon, p.lat], r, 'c3');
        });
        await m.ready;
      })());
    }
    // the fast-charging curve of one car (this one, or the one with most DC sessions)
    const dcCar = car ? car.id : (() => { const n = new Map(); for (const c of rows) if (c.kind === 'dc') n.set(c.car, (n.get(c.car) || 0) + 1); return [...n.entries()].sort((a, b) => b[1] - a[1])[0]?.[0] || fleet[0]?.id; })();
    if (dcCar) {
      work.push((async () => {
        const d = await ctx.api.get(api.apiPath`cars/${dcCar}/charging-stats/dc-curve`, { from_ms: ctx.params.from_ms, to_ms: ctx.params.to_ms, places: f.place || undefined }).catch(() => null);
        if (!ctx.alive()) return;
        const sessions = d?.sessions || [];
        if (!sessions.length) { curveT.append(note(`${car ? car.name : carName(dcCar) || 'The car'} did not fast-charge in this period.`)); return; }
        const med = (d.median || []).slice().sort((a, b) => a.soc_pct - b.soc_pct);
        const dots = sessions.flatMap((ss) => ss.points.map((p) => ({ x: p.soc_pct, y: p.power_kw, charge: ss.charge, tone: p.battery_c != null && p.battery_c < 15 ? 'c2' : 'c1' })));
        await xyChart(ctx, curveT, {
          label: 'Power against battery level, every DC session', xs: med.map((p) => p.soc_pct), xKind: 'percent', xRange: [0, 100], yKind: 'power',
          series: [{ name: 'Median', kind: 'power', values: med.map((p) => p.power_kw), tone: 'c3' }],
          dots, dotsName: 'DC readings', onDot: (dt) => ctx.navigate(state.buildUrl('charge', { car: dcCar, charge: dt.charge })),
        });
        curveT.append(note(`${formatNumber(sessions.length)} DC sessions, one dot per battery percent; sky dots had a pack under 15 °C, which charges slower. Tap a dot to open its charge.`));
      })());
    }
    await Promise.all(work);
  }

  drawNote();
  drawSummary(null);
  ctx.view.replaceChildren(bar, sumBox, incBox, listHead, listStart, statsBox);
  await Promise.all([loadList(), loadStats()]);
  if (!ctx.alive()) return {};
  ctx.ready();
  ctx.events('rows', (ev) => { if (ev.data?.kinds && !ev.data.kinds.includes('charges')) return; if (car && ev.data?.car && ev.data.car !== car.id) return; loadList(); });
  return { onQuery: async () => { await Promise.all([loadList(), loadStats()]); } };
}
