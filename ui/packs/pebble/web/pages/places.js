// Sentry Mate, "Pebble" UI pack: Places (/places), the places that matter and their charging prices.
//
// A map tile with every saved place, a search and sort pills, and a tile per place (name, kind, address, circle,
// how often it was used, its price). Tapping a tile, or a link from another page (?place=<id>, or ?new=lat,lon to
// name a spot), opens the edit sheet: name, kind, position on a small map (tap to move), circle, the price (per kWh
// or per minute, a session fee, rates by time of day), what saving would change in the history, and after saving
// the choice to apply the price to past charges. Data: GET/POST/PATCH/DELETE /api/v1/places, POST /places/preview,
// POST /places/{place}/reprice.

import { h } from '../shared/dom.js';
import { format, convert, toMetric, unitLabel, formatMoneyList } from '../shared/units.js';
import { formatDate } from '../shared/time.js';
import { formatNumber, tn } from "../shared/i18n.js";
import * as api from '../shared/api.js';
import { tile, chip, btn, icon, note } from '../core/ui.js';
import { emptyTile, errorWords } from '../core/states.js';
import { createSheet, confirmSheet } from '../core/sheet.js';
import { field, segmented } from '../core/form.js';
import { mapTile, boundsOf, coordsText } from '../core/map.js';
import { pills, moreButton } from '../core/bits.js';
import { openPriceEditor, pricesOf, priceOnDay, priceLine, fromWord, dayIn } from '../shared/prices.js';

const PRICE_CLS = { btn: 'btn', primary: 'btn-primary', ghost: 'btn-ghost', plain: 'btn-soft', input: 'input', select: 'input' };

export const id = 'places';
export const css = true;
export const shape = [['full', 'h1'], ['full', 'h2'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1']];
export const title = () => 'Places';

const KINDS = [['home', 'Home'], ['work', 'Work'], ['charger', 'Charger'], ['service', 'Service'], ['other', 'Other']];
const KIND_TONE = { home: 'coral', work: 'sky', charger: 'teal', service: 'sun', other: 'muted' };
const SORTS = [['name', 'Name'], ['visits', 'Most visited'], ['last_visit', 'Last visit'], ['created', 'Newest']];

function priceText(p) {
  if (!p) return 'No price';
  const cur = p.currency || '';
  const parts = [];
  if (p.per_kwh != null) parts.push(`${format(p.per_kwh, 'price_energy', { currency: cur })}`);
  if (p.per_minute != null) parts.push(`${format(p.per_minute, 'price_time', { currency: cur })}`);
  if (p.session_fee != null) parts.push(`${format(p.session_fee, 'money', { currency: cur })} a session`);
  if (p.rates?.length) parts.push(`${p.rates.length} time-of-day rate${p.rates.length > 1 ? 's' : ''}`);
  return parts.join(' · ') || 'No price';
}

export async function mount(ctx) {
  const admin = ctx.admin;
  let sort = SORTS.some(([k]) => k === ctx.query.sort) ? ctx.query.sort : 'name';
  let qtext = '';
  let pager = null;

  /* ------------------------------------------------------------ the list */
  const search = h('input', { type: 'search', placeholder: 'Find a place', 'aria-label': 'Find a place', autocomplete: 'off' });
  let timer = 0;
  search.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(() => { qtext = search.value.trim(); load(); }, 250); });
  ctx.onCleanup(() => clearTimeout(timer));
  const sortPills = pills({ label: 'Order', value: sort, options: SORTS, cover: 'web.geofences.list.order', onchange: (v) => { sort = v; ctx.setQuery({ sort: v === 'name' ? null : v }); load(); } });
  const bar = h('div', { class: 'page-bar w-full' },
    h('span', { class: 'crumb', cover: 'web.geofences.list.breadcrumb' }, icon('pin'), 'Places'),
    h('label', { class: 'search-pill' }, icon('search'), search), sortPills);
  const newBtn = admin ? btn('New place', { kind: 'primary', icon: 'plus', cover: 'web.geofences.list.create', onclick: (e) => openEditor(null, null, e.currentTarget) })
    : h('span', { class: 'chip tone-muted', cover: 'web.geofences.list.create' }, 'Only an admin can add places');
  ctx.head({ actions: [newBtn] });
  const intro = tile({ w: 'full', cls: 'places-intro', cover: 'faq.geofence-purpose' },
    h('p', { class: 'lede' }, 'A place gives a spot a name and, for chargers, a price.'),
    note('Drives, charges and stops inside a place’s circle show its name, and charges there get its price. Where circles overlap, the nearest centre wins.'));
  const mapBox = h('div', { class: 'map' });
  const mapT = tile({ w: 'full', title: 'Every place', icon: 'map', cls: 'map-tile' }, mapBox);
  const listStart = h('div', { class: 'tl-list-start w-full' });
  const more = moreButton(async () => { const b = pager.items.length; await pager.more(); drawFrom(b); return !pager.done; }, { text: 'Show more places' });
  const tiles = [];
  function placeTile(p) {
    return tile({ w: 1, cls: ['item', 'place-tile'], href: `?place=${encodeURIComponent(p.id)}`, label: `Edit ${p.name}`, cover: ['web.geofences.list.edit'] },
      h('div', { class: 'item-top' },
        h('span', { class: ['item-ic', `tone-${KIND_TONE[p.kind] || 'muted'}`], 'aria-hidden': 'true' }, icon(p.kind === 'charger' ? 'bolt' : p.kind === 'home' ? 'park' : 'pin')),
        h('div', { class: 'item-text' }, h('p', { class: 'item-title', cover: 'web.geofences.list.col-name' }, p.name), h('p', { class: 'item-sub' }, p.address?.short || coordsText(p.lon, p.lat)))),
      h('p', { class: 'item-sub', cover: ['web.geofences.list.col-position', 'web.geofences.list.col-radius'] }, `${coordsText(p.lon, p.lat)} · circle ${format(p.radius_m, 'length')}`),
      h('p', { class: 'item-sub' }, `${tn(p.counts?.visits || 0, "{n} stop", "{n} stops")} · ${tn(p.counts?.charges || 0, "{n} charge", "{n} charges")}${p.last_visit_ms ? ` · last ${formatDate(p.last_visit_ms, p.time_zone, { now: api.now() })}` : ''}`),
      h('div', { class: 'item-flags tile-foot' }, chip(KINDS.find(([k]) => k === p.kind)?.[1] || 'Other', { tone: KIND_TONE[p.kind] || 'muted' }), p.price ? chip(priceText(p.price), { tone: 'teal', icon: 'coin' }) : null));
  }
  function drawFrom(start) {
    for (const p of pager.items.slice(start)) { const t = placeTile(p); tiles.push(t); ctx.view.insertBefore(t, more); }
    more.querySelector('button').hidden = pager.done;
  }
  async function load() {
    pager = ctx.api.pager('places', { sort, search: qtext || undefined }, { limit: 40 });
    await pager.more();
    if (!ctx.alive()) return;
    for (const t of tiles.splice(0)) t.remove();
    ctx.view.querySelector('.places-empty')?.remove();
    if (!pager.items.length) {
      const e = emptyTile({ icon: 'pin', title: qtext ? `No place matches “${qtext}”` : 'No places yet', text: qtext ? 'Try part of the name.' : 'Name the spots that matter, like home and work. Every list then shows the names.', cover: ['web.geofences.list.empty-state', 'web.geofences.list.col-name', 'web.geofences.list.col-position', 'web.geofences.list.col-radius', 'web.geofences.list.edit'],
        action: admin && !qtext ? btn('New place', { kind: 'primary', icon: 'plus', onclick: (ev) => openEditor(null, null, ev.currentTarget) }) : null });
      e.classList.add('places-empty');
      ctx.view.insertBefore(e, more);
      more.querySelector('button').hidden = true;
    } else drawFrom(0);
  }
  const emptyMark = h('span', { class: 'sr-only', cover: 'web.geofences.list.empty-state' }, 'A place list with nothing in it says how to add the first place.');

  /* ------------------------------------------------------------ the editor sheet */
  const sheet = createSheet({ title: 'Place', cls: 'place-sheet', onClose: () => { if (ctx.query.place || ctx.query.new) ctx.setQuery({ place: null, new: null }); } });
  ctx.onCleanup(() => sheet.el.remove());
  let cur = null; // the place being edited, or null for a new one
  let sheetMap = null;
  const f = {
    crumb: h('p', { class: 'crumb sheet-crumb', cover: 'web.geofences.form.breadcrumb' }),
    name: field({ label: 'Name', name: 'name', attrs: { maxlength: 80, autocomplete: 'off', required: true }, cover: 'web.geofences.form.name' }),
    kind: field({ label: 'Kind', name: 'kind', type: 'select', options: KINDS, value: 'other' }),
    lat: field({ label: 'Latitude', name: 'lat', type: 'number', attrs: { step: 'any', inputmode: 'decimal' } }),
    lon: field({ label: 'Longitude', name: 'lon', type: 'number', attrs: { step: 'any', inputmode: 'decimal' } }),
    radius: field({ label: `Circle (${unitLabel('length')})`, name: 'radius', type: 'number', attrs: { step: '1', inputmode: 'numeric' }, hint: 'From 10 to 5000 m. A circle a little bigger than the car park works best.' }),
    billing: segmented({ label: 'Charging price', name: 'billing', value: 'none', options: [['none', 'No price'], ['kwh', 'Per kWh'], ['minute', 'Per minute']], cover: 'web.geofences.form.billing-type', onchange: () => showPrice() }),
    unit: field({ label: 'Price per unit', name: 'unit', type: 'number', attrs: { step: 'any', inputmode: 'decimal' }, cover: ['web.geofences.form.cost-per-unit', 'web.geofences.form.negative-price'], hint: 'A price below zero is allowed: some tariffs pay you to charge at times.' }),
    fee: field({ label: 'Session fee', name: 'fee', type: 'number', attrs: { step: 'any', inputmode: 'decimal', min: '0' }, cover: 'web.geofences.form.session-fee', hint: 'Added once to each charge here. Leave empty for none.' }),
    currency: field({ label: 'Currency', name: 'currency', attrs: { maxlength: 3, autocomplete: 'off' }, hint: 'Three letters, for example USD or EUR.' }),
  };
  const ratesBox = h('div', { class: 'rates', cover: 'projects.agile' });
  const mapEl = h('div', { class: 'map map-short' });
  const effect = h('div', { class: 'effect', 'aria-live': 'polite', cover: 'web.geofences.form.relabel-history' });
  const reprice = h('div', { class: 'effect', cover: 'web.geofences.form.retro-costs' });
  const err = h('p', { class: 'form-error', role: 'alert', hidden: true, cover: 'web.geofences.form.validation' });
  const missing = h('div', { class: 'place-missing', hidden: true, cover: 'web.geofences.form.not-found' });
  const saveBtn = btn('Save', { kind: 'primary', icon: 'check', cover: 'web.geofences.form.save', onclick: () => save() });
  const delBtn = btn('Delete', { kind: 'danger', icon: 'trash', cover: 'web.geofences.list.delete', onclick: () => remove() });
  const backBtn = btn('Back to places', { kind: 'soft', icon: 'chevronLeft', cover: 'web.geofences.form.back', onclick: () => sheet.close() });
  const checkBtn = btn('What would change?', { kind: 'soft', icon: 'search', onclick: () => preview() });
  // prices by date: each from its first day on, edited in the place price editor
  const datedBox = h('div', { class: 'dated' });
  function paintDated() {
    const dated = pricesOf(cur);
    const nowEntry = cur ? priceOnDay(dated, dayIn(Date.now(), cur.time_zone)) : null;
    datedBox.replaceChildren(
      h('span', { class: 'label' }, 'Prices by date'),
      dated.length ? h('ul', { class: 'dated-list' }, dated.map((x) => h('li', { class: x === nowEntry ? 'is-now' : null },
        h('b', null, fromWord(x.from)), h('span', null, priceLine(x.price)),
        h('span', { class: 'hint' }, [x.source === 'invoice' ? 'from invoices' : null, x === nowEntry ? 'in force today' : null].filter(Boolean).join(' · '))))) : null,
      h('p', { class: 'hint' }, cur ? 'A charge is costed at the price of its own day. The price below is the one in force today; a price you type wins over one from invoices.' : 'Save the place first; then it can have prices from a date on.'),
      cur && admin ? btn(dated.length ? 'Add or change prices by date' : 'Add prices by date', { kind: 'soft', icon: 'calendar', onclick: async () => {
        const saved = await openPriceEditor({ client: ctx.api, place: cur, cls: PRICE_CLS });
        if (saved) { ctx.toast('Prices saved.'); sheet.close(); load(); }
      } }) : null);
  }
  const priceBox = h('div', { class: 'form price-box' }, f.unit, f.fee, f.currency, h('p', { class: 'hint', cover: 'web.geofences.form.auto-cost' }, 'Charges that start here from now on get this price by themselves. Past charges keep theirs until you apply the price to them.'), ratesBox);
  const formEl = h('div', { class: 'form', cover: ['web.geofences.form.entry-from-dashboards', 'web.geofences.form.defaults'] },
    f.crumb, missing, f.name, f.kind,
    h('div', { class: 'field', cover: 'web.geofences.form.position-map' }, h('span', { class: 'label' }, 'Where'), mapEl, h('p', { class: 'hint' }, 'Tap the map to move the centre.')),
    h('div', { class: 'two' }, f.lat, f.lon), f.radius,
    h('p', { class: 'hint', cover: 'web.geofences.form.match-rule' }, 'A drive or charge belongs to this place when it starts or ends inside the circle. Where circles overlap, the nearest centre wins.'),
    datedBox, f.billing, priceBox, err,
    h('div', { class: 'sheet-actions' }, backBtn, checkBtn, admin ? delBtn : null, admin ? saveBtn : null),
    effect, reprice);
  sheet.body.append(formEl);
  const val = (fl) => fl.control.value.trim();

  let rates = [];
  function drawRates() {
    ratesBox.replaceChildren(h('span', { class: 'label' }, 'Rates by time of day'),
      rates.length ? h('ul', { class: 'rate-list' }, rates.map((r, i) => h('li', { class: 'rate' },
        h('span', { class: 'rate-text' }, h('span', { class: 'row-name' }, r.name || 'Rate'), h('span', { class: 'row-sub' }, `${r.days} ${r.from}–${r.to}: ${r.per_kwh != null ? `${r.per_kwh} per kWh` : `${r.per_minute} per minute`}`)),
        btn(`Remove ${r.name || 'rate'}`, { kind: 'ghost', icon: 'close', iconOnly: true, onclick: () => { rates.splice(i, 1); drawRates(); } })))) : h('p', { class: 'hint' }, 'None: the one price above holds all day.'),
      btn('Add a rate', { kind: 'soft', icon: 'plus', onclick: addRate }));
  }
  function addRate() {
    const nm = field({ label: 'Name', name: 'rname', value: 'Night' });
    const pr = field({ label: 'Price per kWh', name: 'rprice', type: 'number', attrs: { step: 'any' } });
    const days = field({ label: 'Days', name: 'rdays', type: 'select', options: [['all', 'Every day'], ['mon-fri', 'Mon to Fri'], ['sat,sun', 'Sat and Sun']], value: 'all' });
    const from = field({ label: 'From', name: 'rfrom', type: 'time', value: '00:00' });
    const to = field({ label: 'To', name: 'rto', type: 'time', value: '06:00' });
    const box = h('div', { class: 'rate-new form' }, nm, pr, days, h('div', { class: 'two' }, from, to),
      h('div', { class: 'form-actions' }, btn('Add', { kind: 'primary', onclick: () => {
        if (val(pr) === '') { pr.querySelector('.input').setAttribute('aria-invalid', 'true'); return; }
        rates.push({ name: val(nm), per_kwh: Number(val(pr)), per_minute: null, days: days.control.value, from: val(from) || '00:00', to: val(to) || '24:00' });
        drawRates();
      } }), btn('Cancel', { kind: 'ghost', onclick: drawRates })));
    ratesBox.replaceChildren(h('span', { class: 'label' }, 'New rate'), box);
  }
  function showPrice() {
    const b = f.billing.querySelector('[aria-checked="true"]')?.dataset.value || 'none';
    priceBox.hidden = b === 'none';
    f.unit.querySelector('.label').textContent = b === 'minute' ? 'Price per minute' : 'Price per kWh';
  }

  /** Open the editor: a saved place, or a new one (at lngLat when given). */
  async function openEditor(p, lngLat, opener) {
    cur = p;
    missing.hidden = true;
    err.hidden = true;
    effect.replaceChildren(); reprice.replaceChildren();
    sheet.titleEl.textContent = p ? `Edit ${p.name}` : 'New place';
    f.crumb.replaceChildren('Places', icon('chevronRight'), p ? p.name : 'New place');
    const lat = p ? p.lat : lngLat ? lngLat[1] : null, lon = p ? p.lon : lngLat ? lngLat[0] : null;
    f.name.control.value = p ? p.name : '';
    f.kind.control.value = p ? p.kind : 'other';
    f.lat.control.value = lat != null ? String(lat) : '';
    f.lon.control.value = lon != null ? String(lon) : '';
    f.radius.control.value = String(Math.round(convert(p ? p.radius_m : 100, 'length')));
    const pr = p?.price;
    f.billing.set(!pr ? 'none' : pr.per_minute != null && pr.per_kwh == null ? 'minute' : 'kwh');
    f.unit.control.value = pr ? String(pr.per_kwh ?? pr.per_minute ?? '') : '';
    f.fee.control.value = pr?.session_fee != null ? String(pr.session_fee) : '';
    f.currency.control.value = pr?.currency || 'USD';
    rates = (pr?.rates || []).map((r) => ({ ...r }));
    drawRates(); showPrice(); paintDated();
    delBtn.hidden = !p;
    for (const x of [f.name, f.kind, f.lat, f.lon, f.radius, f.unit, f.fee, f.currency]) x.control.disabled = !admin;
    sheet.open(opener);
    if (p && admin && p.price) offerReprice(p);
    await drawSheetMap();
  }
  async function drawSheetMap() {
    const lat = Number(val(f.lat)), lon = Number(val(f.lon));
    const ok = val(f.lat) !== '' && Number.isFinite(lat) && Number.isFinite(lon);
    if (!sheetMap) {
      sheetMap = await mapTile(ctx, mapEl, { center: ok ? [lon, lat] : [-98, 39], zoom: ok ? 15 : 3 });
      if (!sheetMap || !sheetMap.map) return;
      sheetMap.map.on('click', (e) => {
        if (!admin) return;
        f.lat.control.value = e.lngLat.lat.toFixed(5); f.lon.control.value = e.lngLat.lng.toFixed(5);
        drawSheetMap();
      });
    }
    if (!sheetMap.map) return;
    sheetMap.map.resize();
    if (ok) {
      const r = toMetric(Number(val(f.radius)) || 100, 'length');
      await sheetMap.circle('circle', [lon, lat], r);
      sheetMap.map.jumpTo({ center: [lon, lat], zoom: r > 800 ? 13 : 15.5 });
    }
  }
  for (const x of [f.lat, f.lon, f.radius]) x.control.addEventListener('change', () => drawSheetMap());

  function draft() {
    const b = f.billing.querySelector('[aria-checked="true"]')?.dataset.value || 'none';
    const unit = val(f.unit) === '' ? null : Number(val(f.unit));
    const fee = val(f.fee) === '' ? null : Number(val(f.fee));
    return {
      name: val(f.name), kind: f.kind.control.value, lat: Number(val(f.lat)), lon: Number(val(f.lon)),
      radius_m: Math.round(toMetric(Number(val(f.radius)) || 100, 'length')),
      price: b === 'none' ? null : { currency: val(f.currency).toUpperCase() || 'USD', per_kwh: b === 'kwh' ? unit : null, per_minute: b === 'minute' ? unit : null, session_fee: fee, rates },
    };
  }
  function check(d) {
    const bad = [];
    if (!d.name) bad.push('Give the place a name.');
    if (!Number.isFinite(d.lat) || !Number.isFinite(d.lon) || val(f.lat) === '') bad.push('Pick where it is: tap the map or type the position.');
    if (d.radius_m < 10 || d.radius_m > 5000) bad.push('The circle must be from 10 to 5000 m.');
    if (d.price && d.price.per_kwh == null && d.price.per_minute == null && d.price.session_fee == null && !rates.length) bad.push('Type a price, or choose No price.');
    return bad;
  }
  function showErr(text) { err.textContent = text; err.hidden = !text; }
  function effectWords(e) {
    if (!e) return [];
    const out = [`${formatNumber(e.drives_named || 0)} drives and ${formatNumber(e.charges_named || 0)} charges would show this name`];
    if (e.drives_unnamed || e.charges_unnamed) out.push(`${formatNumber(e.drives_unnamed || 0)} drives and ${formatNumber(e.charges_unnamed || 0)} charges would lose a name`);
    if (e.taken_from?.length) out.push(`some are taken from ${e.taken_from.map((t) => t.place.name).join(', ')}`);
    if (e.charges_repriced) out.push(`${formatNumber(e.charges_repriced)} past charges would get a new cost if you apply the price`);
    return out;
  }
  async function preview() {
    const d = draft();
    const bad = check(d);
    if (bad.length) { showErr(bad.join(' ')); return; }
    showErr('');
    try {
      const e = await ctx.api.post('places/preview', cur ? { ...d, id: cur.id } : d);
      effect.replaceChildren(h('p', { class: 'effect-title' }, 'What saving changes in the history'), h('ul', { class: 'effect-list' }, effectWords(e).map((t) => h('li', null, `${t}.`))));
    } catch (e2) { showErr(e2.message || errorWords(e2).title); }
  }
  async function save() {
    const d = draft();
    const bad = check(d);
    if (bad.length) { showErr(bad.join(' ')); return; }
    showErr('');
    saveBtn.disabled = true;
    try {
      const res = cur ? await ctx.api.patch(api.apiPath`places/${cur.id}`, { ...d, version: cur.version }) : await ctx.api.post('places', d);
      const saved = res.place || res;
      cur = saved;
      sheet.titleEl.textContent = `Edit ${saved.name}`;
      effect.replaceChildren(h('p', { class: 'effect-title' }, 'Saved'), h('ul', { class: 'effect-list' }, effectWords(res.effect).map((t) => h('li', null, `${t}.`))));
      ctx.toast(`${saved.name} is saved.`);
      if (saved.price) offerReprice(saved);
      await load();
      drawMap();
    } catch (e) {
      const fields = e.fields || {};
      showErr(Object.values(fields).join(' ') || e.message || 'Not saved.');
    } finally { saveBtn.disabled = false; }
  }
  async function offerReprice(p) {
    reprice.replaceChildren(h('p', { class: 'effect-title' }, 'Past charges here'), note('Past charges keep the cost they had. Apply today’s price to them to see their cost with this price.'),
      btn('Check past charges', { kind: 'soft', icon: 'coin', onclick: async () => {
        try {
          const r = await ctx.api.post(api.apiPath`places/${p.id}/reprice`, { dry_run: true });
          reprice.replaceChildren(h('p', { class: 'effect-title' }, 'Past charges here'),
            h('p', null, `${formatNumber(r.repriced)} of ${formatNumber(r.charges)} charges would change: ${formatMoneyList(r.cost_before)} becomes ${formatMoneyList(r.cost_after)}.`),
            r.repriced ? btn('Apply the price to them', { kind: 'primary', onclick: async () => {
              const done = await ctx.api.post(api.apiPath`places/${p.id}/reprice`, { dry_run: false });
              ctx.toast(`${formatNumber(done.repriced)} charges have the new cost.`);
              reprice.replaceChildren(note(`${formatNumber(done.repriced)} charges have the new cost.`));
            } }) : note('Nothing to change.'));
        } catch (e) { reprice.append(note(e.message || 'The check did not work.')); }
      } }));
  }
  async function remove() {
    if (!cur) return;
    let words = '';
    try { const e = await ctx.api.post('places/preview', { place: cur.id, delete: true }); words = ` ${formatNumber(e.drives_unnamed || 0)} drives and ${formatNumber(e.charges_unnamed || 0)} charges lose its name.`; } catch { /* still ask */ }
    const ok = await confirmSheet({ title: `Delete ${cur.name}?`, text: `The place goes away at once.${words} Past costs stay as they are.`, yes: 'Delete', danger: true });
    if (!ok) return;
    try {
      await ctx.api.del(api.apiPath`places/${cur.id}`, { params: { version: cur.version } });
      ctx.toast(`${cur.name} is deleted.`);
      sheet.close();
      await load(); drawMap();
    } catch (e) { showErr(e.message || 'Not deleted.'); }
  }

  /** Open what the address asks for: ?place=<id> or ?new=lat,lon. */
  async function fromQuery(q) {
    if (q.place) {
      try { const p = await ctx.api.get(api.apiPath`places/${q.place}`); await openEditor(p, null); } catch (e) {
        if (e.status !== 404) throw e;
        await openEditor(null, null);
        sheet.titleEl.textContent = 'Place not found';
        missing.hidden = false;
        missing.replaceChildren(h('p', { class: 'lede' }, 'There is no such place.'), note('It may have been deleted. You can add a new one here, or go back to the list.'));
      }
    } else if (q.new) {
      const [la, lo] = String(q.new).split(',').map(Number);
      if (Number.isFinite(la) && Number.isFinite(lo)) await openEditor(null, [lo, la]);
    } else if (sheet.isOpen) sheet.close();
  }

  /* ------------------------------------------------------------ the map of every place */
  let bigMap = null;
  async function drawMap() {
    const all = await ctx.api.list('places', { sort: 'name' }).all({ max: 500 }).catch(() => []);
    if (!ctx.alive()) return;
    const pts = all.map((p) => [p.lon, p.lat]);
    if (!pts.length) { if (!bigMap) mapBox.append(h('p', { class: 'map-note' }, 'No place to show yet.')); return; }
    if (!bigMap) { bigMap = await mapTile(ctx, mapBox, { bounds: boundsOf(pts), padding: 40 }); if (!bigMap) return; }
    if (!bigMap.map) return;
    for (const mk of bigMap.markers || []) mk.remove();
    bigMap.markers = all.map((p) => bigMap.pin([p.lon, p.lat], { tone: KIND_TONE[p.kind] || 'muted', title: p.name, onclick: (e) => openEditor(p, null, e.currentTarget) }));
  }

  ctx.view.replaceChildren(bar, intro, mapT, emptyMark, listStart, more);
  await Promise.all([load(), drawMap()]);
  await fromQuery(ctx.query);
  await bigMap?.ready;
  if (!ctx.alive()) return {};
  ctx.ready();
  return { onQuery: (q) => fromQuery(q) };
}
