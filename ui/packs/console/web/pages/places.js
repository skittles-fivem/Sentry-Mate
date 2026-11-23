// Places (/places): named places with their circle, visits and charging price. The list is a table
// (GET /places, sorted and paged by the server); selecting a row opens its edit form and a small map
// (?place=<id>, or ?new=1&lat=&lon= for a new one). At 2560 px and wider the list and the form sit side by
// side (40 / 60). The form is 'key: [input]' lines; after each change POST /places/preview says what saving
// would rename and reprice, before anything is saved. Of overlapping circles the nearest centre wins.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { format, convert, toMetric, unitLabel } from '../shared/units.js';
import { formatNumber } from '../shared/i18n.js';
import * as TI from '../shared/tiers.js';
import { dayKey, dayStart, addDays } from '../shared/time.js';
import { head, section, kv, note, link } from '../core/ui.js';
import { num, tag, date } from '../core/fmt.js';
import { table } from '../core/table.js';
import { safeMap } from '../core/map.js';
import { empty } from '../core/states.js';
import { field } from '../core/form.js';
import { confirmDialog } from '../core/dialog.js';
import { toast } from '../core/toast.js';
import { isAdmin } from '../core/prefs.js';

export const id = 'places';
export const title = (ctx) => (ctx.query?.new ? 'New place' : ctx.query?.place ? 'Edit place' : 'Places');

const enc = encodeURIComponent;
const KINDS = [['home', 'home'], ['work', 'work'], ['charger', 'charger'], ['service', 'service centre'], ['other', 'other']];
const CURRENCIES = ['USD', 'CAD', 'EUR', 'GBP', 'NOK', 'SEK', 'DKK', 'CHF', 'AUD', 'NZD', 'JPY', 'CNY'];
const DAY_CHOICES = [['all', 'every day'], ['mon-fri', 'mon-fri'], ['sat,sun', 'sat, sun'], ['mon', 'mon'], ['tue', 'tue'], ['wed', 'wed'], ['thu', 'thu'], ['fri', 'fri'], ['sat', 'sat'], ['sun', 'sun']];
const R_MIN = 10, R_MAX = 5000, R_NEW = 100;
const HM = /^(([01]\d|2[0-3]):[0-5]\d|24:00)$/;
const EDITOR_ITEMS = ['web.geofences.form.breadcrumb', 'web.geofences.form.defaults', 'web.geofences.form.position-map', 'web.geofences.form.match-rule',
  'web.geofences.form.name', 'web.geofences.form.validation', 'web.geofences.form.back', 'web.geofences.form.save', 'web.geofences.form.billing-type',
  'web.geofences.form.cost-per-unit', 'web.geofences.form.session-fee', 'web.geofences.form.negative-price', 'web.geofences.form.auto-cost', 'projects.agile',
  'web.geofences.form.relabel-history', 'web.geofences.form.retro-costs', 'web.geofences.form.entry-from-dashboards'];
const listUrl = (query = null) => state.buildUrl('places', { query });
const editUrl = (pid) => state.buildUrl('places', { query: { place: pid } });
const priceText = (p) => {
  if (!p) return null;
  const parts = [];
  if (p.per_kwh != null) parts.push(`${format(p.per_kwh, 'money', { currency: p.currency })}/kWh`);
  if (p.tiers?.steps?.length) parts.push(`${p.tiers.steps.length} kWh step${p.tiers.steps.length === 1 ? '' : 's'}`);
  if (p.per_minute != null) parts.push(`${format(p.per_minute, 'money', { currency: p.currency })}/min`);
  if (p.session_fee) parts.push(`+${format(p.session_fee, 'money', { currency: p.currency })}`);
  if (p.rates?.length) parts.push(`${p.rates.length} rate${p.rates.length === 1 ? '' : 's'}`);
  return parts.join(' ') || 'priced';
};
const distM = (a, b, c, d) => { const R = 6371000, r = Math.PI / 180; const x = (d - b) * r * Math.cos(((a + c) / 2) * r), y = (c - a) * r; return Math.sqrt(x * x + y * y) * R; };

export async function mount(ctx) {
  const admin = isAdmin();
  const q = ctx.query;
  const editing = q.place || q.new;

  const hd = head(ctx, { title: editing ? (q.new ? 'New place' : 'Edit place') : 'Places', period: false, car: false });
  const trail = h('p', { class: 'subline', cover: editing ? ['web.geofences.list.breadcrumb', 'web.geofences.form.breadcrumb'] : 'web.geofences.list.breadcrumb' },
    link('/', 'fleet'), h('span', { class: 'sep' }, ' / '), editing ? [link(listUrl(), 'places', { cover: 'web.geofences.form.back' }), h('span', { class: 'sep' }, ' / '), q.new ? 'new' : 'edit'] : 'places');
  const newBtn = admin ? h('a', { class: 'btn btn--primary', href: listUrl({ new: 1 }), cover: ['web.geofences.list.create', ...EDITOR_ITEMS] }, '+ new place')
    : h('button', { class: 'btn', type: 'button', disabled: true, title: 'only an admin can add, change or delete places', cover: ['web.geofences.list.create', ...EDITOR_ITEMS] }, '+ new place');

  /* ---------- the list */
  const searchIn = h('input', { class: 'in', type: 'search', value: q.q || '', placeholder: 'name or address', 'aria-label': 'Find a place' });
  const kindSel = h('select', { class: 'in', 'aria-label': 'Kind' }, h('option', { value: '' }, 'kind: any'), KINDS.map(([k, l]) => h('option', { value: k, selected: q.kind === k }, `kind: ${l}`)));
  const params = () => ({ q: searchIn.value.trim() || null, kind: kindSel.value || null });
  const sel = q.place || null;
  const list = table(ctx, {
    title: 'places', path: 'places', params: params(), limit: 50, csvName: 'places', sort: { key: 'name', dir: 'asc' },
    note: 'Sorted by name until you pick another column. Counts cover the cars your account sees. Select a row to edit it.', noteCover: 'web.geofences.list.order',
    rowHref: (p) => editUrl(p.id),
    columns: [
      { key: 'name', label: 'name', sort: 'name', firstDir: 'asc', ph: 'main', cover: 'web.geofences.list.col-name', render: (p) => h('a', { class: ['lnk-plain', p.id === sel && 'tone--accent'], href: editUrl(p.id) }, p.name) },
      { key: 'kind', label: 'kind', ph: 'sub', render: (p) => (KINDS.find((k) => k[0] === p.kind) || [0, p.kind])[1] },
      { key: 'radius', label: 'radius', num: true, cover: 'web.geofences.list.col-radius', render: (p) => num(p.radius_m, 'length') },
      { key: 'pos', label: 'centre', cover: 'web.geofences.list.col-position', render: (p) => h('span', { class: 'muted', title: p.address?.short || null }, `${p.lat.toFixed(4)}, ${p.lon.toFixed(4)}`) },
      { key: 'visits', label: 'visits', num: true, sort: 'visits', firstDir: 'desc', ph: 'num', render: (p) => formatNumber(p.counts?.visits ?? 0) },
      { key: 'charges', label: 'charges', num: true, render: (p) => formatNumber(p.counts?.charges ?? 0) },
      { key: 'price', label: 'price', ph: 'sub', render: (p) => priceText(p.price) || h('span', { class: 'muted' }, 'free') },
      { key: 'last', label: 'last visit', sort: 'last_visit', firstDir: 'desc', render: (p) => (p.last_visit_ms ? date(p.last_visit_ms, p.time_zone || ctx.tz) : null) },
      { key: 'act', label: '', cover: ['web.geofences.list.edit', 'web.geofences.list.delete'], render: (p) => h('span', { class: 'pin' },
        h('a', { class: 'lnk', href: editUrl(p.id), cover: 'web.geofences.list.edit' }, admin ? 'edit' : 'view'),
        admin ? h('button', { class: 'btn btn--line muted', type: 'button', 'aria-label': `Delete ${p.name}`, onclick: () => removePlace(p) }, 'del') : null) },
    ],
    empty: () => (searchIn.value.trim() || kindSel.value ? empty('no place matches', 'clear the search or pick any kind') : h('div', { cover: 'web.geofences.list.empty-state' }, empty('no places yet', admin ? 'add one with + new place, or from a drive or charge that has no place yet' : 'an admin can add places'))),
  });
  // the empty-state item is on the list page whether or not it is empty now
  const emptyMark = h('span', { hidden: true, cover: 'web.geofences.list.empty-state' });
  let st = 0;
  const reloadList = () => { ctx.setQuery({ q: searchIn.value.trim() || null, kind: kindSel.value || null }); list.reload(params()); };
  searchIn.addEventListener('input', () => { clearTimeout(st); st = setTimeout(reloadList, 300); });
  kindSel.addEventListener('change', reloadList);
  ctx.onCleanup(() => clearTimeout(st));

  async function removePlace(p) {
    let eff = null;
    try { eff = await ctx.api.post('places/preview', { place: p.id, delete: true }); } catch { eff = null; }
    const words = eff ? ` ${formatNumber(eff.drives_unnamed || 0)} drives and ${formatNumber(eff.charges_unnamed || 0)} charges lose its name; they show their address again.` : '';
    const ok = await confirmDialog(`Delete ${p.name}?${words} Costs stay as they are.`, { title: 'delete the place?', ok: 'delete', danger: true });
    if (!ok || !ctx.alive()) return;
    try { await ctx.api.del(`places/${enc(p.id)}`); toast(`Deleted ${p.name}.`, { next: true }); ctx.navigate(listUrl()); } catch (e) { toast(e.message || 'The place could not be deleted.', { kind: 'error' }); }
  }

  const missing = q.missing ? h('p', { class: 'staleline', role: 'status', cover: 'web.geofences.form.not-found' }, tag('not found', 'bad'), ' that place no longer exists; it may have been deleted.')
    : h('span', { hidden: true, cover: 'web.geofences.form.not-found' });
  const intro = h('p', { class: 'note', cover: 'faq.geofence-purpose' }, 'A place is a named circle. Every drive, charge and stop inside it shows the name instead of the street address, lists can be filtered by it, and charging there can have a price.');

  /* ---------- the form */
  let editor = null, editorReady = Promise.resolve();
  if (editing) {
    editor = h('div', { class: 'pl-edit' });
    editorReady = buildEditor(editor).catch((e) => { if (ctx.alive()) replace(editor, h('p', { class: 'errbox-line' }, tag('error', 'bad'), ' ', e.message || 'the place could not be loaded')); });
  }

  const listCol = h('div', { class: 'pl-list' }, h('div', { class: 'optline' }, h('span', { class: 'optline-k' }, 'find:'), searchIn, kindSel, newBtn), list.el, emptyMark);
  const mapAll = h('div', { class: 'map map--sm', role: 'img', 'aria-label': 'Map of every place' });
  replace(ctx.view, h('div', { class: 'page', dataset: { page: 'places' } },
    hd, trail, missing, intro,
    editing ? h('div', { class: 'split split--edit' }, listCol, editor) : [listCol, section({ title: 'every circle' }, mapAll)]));

  try { await Promise.all([list.ready, editorReady]); } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  if (!editing) {
    const all = await ctx.api.list('places', { sort: 'name' }, { limit: 500 }).all({ max: 2000 }).catch(() => []);
    if (!ctx.alive()) return;
    if (all.length) {
      const m = await safeMap(mapAll, ctx, { center: [all[0].lon, all[0].lat], zoom: 10 });
      if (m && ctx.alive()) {
        m.points('places', all.map((p) => ({ lon: p.lon, lat: p.lat, props: { id: p.id } })), { size: 9 });
        m.map.on('click', 'places', (e) => { const pid = e.features?.[0]?.properties?.id; if (pid) ctx.navigate(editUrl(pid)); });
        m.fit(all.map((p) => [p.lon, p.lat]), { pad: 24, maxZoom: 14 });
      }
    } else mapAll.append(h('p', { class: 'map-note' }, '-- no places yet --'));
  }
  if (!ctx.alive()) return;
  ctx.ready();

  /* ================================================================ the editor */
  async function buildEditor(host) {
    const isNew = !!q.new;
    let cur = null;
    if (!isNew) {
      try { cur = await ctx.api.get(`places/${enc(q.place)}`); } catch (e) {
        if (e.status === 404) { state.navigate(listUrl({ missing: q.place }), { replace: true }); return; }
        throw e;
      }
    }
    const others = await ctx.api.list('places', { sort: 'name' }, { limit: 500 }).all({ max: 2000 }).catch(() => []);
    if (!ctx.alive()) return;
    const lock = !admin;
    const lat0 = Number.isFinite(+q.lat) ? +q.lat : others[0]?.lat ?? 0, lon0 = Number.isFinite(+q.lon) ? +q.lon : others[0]?.lon ?? 0;
    const p = cur?.price || null;
    const d = {
      name: cur?.name || '', kind: cur?.kind || 'other', lat: cur?.lat ?? lat0, lon: cur?.lon ?? lon0,
      radius_m: cur?.radius_m ?? (Number.isFinite(+q.radius) ? +q.radius : R_NEW), time_zone: cur?.time_zone || '',
      priced: !!p, mode: p?.tiers ? 'tiers' : p ? (p.per_kwh != null && p.per_minute != null ? 'both' : p.per_minute != null ? 'minute' : 'kwh') : 'kwh',
      currency: p?.currency || 'USD', per_kwh: p?.per_kwh ?? null, per_minute: p?.per_minute ?? null, session_fee: p?.session_fee ?? null,
      rates: (p?.rates || []).map((r) => ({ ...r })),
      tiers: TI.copyTiers(p?.tiers) || TI.newTiers(p?.per_kwh ?? null),
    };
    const L = unitLabel('length');
    const numOrNull = (v) => (v === '' || v == null ? null : Number(v));
    const fName = field({ label: 'name', name: 'name', value: d.name, required: true, attrs: { maxlength: '64', autocomplete: 'off', disabled: lock, placeholder: 'home, gym car park' } });
    const fKind = field({ label: 'kind', name: 'kind', type: 'select', value: d.kind, options: KINDS, attrs: { disabled: lock } });
    const fRadius = field({ label: `radius ${L}`, name: 'radius_m', type: 'number', value: Math.round(convert(d.radius_m, 'length')), required: true,
      hint: `${format(R_MIN, 'length')} to ${format(R_MAX, 'length')}`, attrs: { min: String(Math.ceil(convert(R_MIN, 'length'))), max: String(Math.floor(convert(R_MAX, 'length'))), step: '1', disabled: lock } });
    const fLat = field({ label: 'latitude', name: 'lat', type: 'number', value: d.lat.toFixed(6), attrs: { step: 'any', min: '-90', max: '90', disabled: lock } });
    const fLon = field({ label: 'longitude', name: 'lon', type: 'number', value: d.lon.toFixed(6), attrs: { step: 'any', min: '-180', max: '180', disabled: lock } });
    let zones = [];
    try { zones = Intl.supportedValuesOf('timeZone'); } catch { zones = []; }
    if (d.time_zone && !zones.includes(d.time_zone)) zones = [d.time_zone, ...zones];
    const fZone = field({ label: 'time zone', name: 'time_zone', type: 'select', value: d.time_zone, options: [...(isNew ? [['', 'automatic: the nearest place’s']] : []), ...zones.map((z) => [z, z])], attrs: { disabled: lock } });
    const fCur = field({ label: 'currency', name: 'currency', type: 'select', value: d.currency, path: 'price.currency', options: CURRENCIES.map((c) => [c, c]), attrs: { disabled: lock } });
    const fKwh = field({ label: 'per kWh', name: 'per_kwh', type: 'number', value: d.per_kwh, path: 'price.per_kwh', attrs: { step: '0.0001', disabled: lock, placeholder: '0.31' } });
    const fMin = field({ label: 'per minute', name: 'per_minute', type: 'number', value: d.per_minute, path: 'price.per_minute', attrs: { step: '0.0001', disabled: lock, placeholder: '0.20' } });
    const fFee = field({ label: 'session fee', name: 'session_fee', type: 'number', value: d.session_fee, path: 'price.session_fee', attrs: { step: '0.01', min: '0', max: '1000', disabled: lock, placeholder: '0.00' } });
    const fields = { name: fName, kind: fKind, radius_m: fRadius, lat: fLat, lon: fLon, time_zone: fZone, 'price.currency': fCur, 'price.per_kwh': fKwh, 'price.per_minute': fMin, 'price.session_fee': fFee };
    let dirty = false;
    const leave = (e) => { if (dirty && !window.confirm('Leave without saving your changes?')) e.preventDefault(); };
    const formMsg = h('p', { class: 'frm-msg', hidden: true, role: 'alert' });

    const keySet = (opts, get, set) => h('span', { class: 'keys' }, opts.map(([v, l]) => h('button', { class: 'key', type: 'button', disabled: lock, 'aria-pressed': String(get() === v),
      onclick: (e) => { set(v); for (const b of e.currentTarget.parentElement.children) b.setAttribute('aria-pressed', String(b === e.currentTarget)); paintPrice(); changed(); } }, l)));
    const pricedKeys = keySet([[false, 'free or not tracked'], [true, 'priced']], () => d.priced, (v) => { d.priced = v; });
    const modeKeys = keySet([['kwh', 'per kWh'], ['minute', 'per minute'], ['both', 'both'], ['tiers', 'tiered (per kWh steps)']], () => d.mode, (v) => { d.mode = v; });
    // tiered prices: steps by the kWh used in a billing cycle
    const tiersBox = h('div', { class: 'lines' });
    const tiersMsg = h('p', { class: 'frm-msg', hidden: true, role: 'alert' });
    const fCycle = field({ label: 'cycle from', name: 'cycle_day', hint: 'the billing cycle starts at midnight at the place', type: 'select', value: String(d.tiers.cycle_day || 1), path: 'price.tiers.cycle_day', options: TI.cycleChoices().map(([v, l]) => [v, l.toLowerCase()]), attrs: { disabled: lock } });
    fCycle.input.addEventListener('change', () => { d.tiers.cycle_day = Number(fCycle.input.value); paintRule(); changed(); });
    const fBase = field({ label: 'house use', name: 'base_kwh', hint: 'kWh the household uses per cycle, counted first', type: 'number', value: d.tiers.base_kwh || null, path: 'price.tiers.base_kwh', attrs: { min: '0', step: '1', placeholder: '0', disabled: lock } });
    fBase.input.addEventListener('input', () => { d.tiers.base_kwh = numOrNull(fBase.input.value) ?? 0; paintRule(); changed(); });
    function paintTiers() {
      const st = d.tiers.steps;
      replace(tiersBox, st.map((s, i) => {
        const { from, to } = TI.stepRange(st, i);
        const inp = (key, attrs) => { const el = h('input', { class: 'in in--cell', type: 'number', disabled: lock || attrs.fixed, 'aria-label': `${key === 'from_kwh' ? 'from kWh' : 'price per kWh'} of step ${i + 1}`, size: '7', ...attrs, fixed: undefined }); el.addEventListener('input', () => { s[key] = numOrNull(el.value); if (key === 'from_kwh') paintTierWords(); paintRule(); changed(); }); return el; };
        return h('div', { class: 'pl-rate pl-tier' }, h('span', { class: 'muted pl-tier-words' }, `${i + 1}. ${TI.stepLabel(from, to).toLowerCase()}`),
          h('span', { class: 'muted' }, 'from'), inp('from_kwh', { step: '1', min: '0', value: i === 0 ? 0 : s.from_kwh ?? '', fixed: i === 0 }), h('span', { class: 'muted' }, 'kWh at'),
          inp('per_kwh', { step: '0.0001', value: s.per_kwh ?? '', placeholder: '/kWh' }),
          lock || st.length <= 1 ? null : h('button', { class: 'btn btn--line muted', type: 'button', 'aria-label': `remove step ${i + 1}`, onclick: () => { st.splice(i, 1); st[0].from_kwh = 0; paintTiers(); paintRule(); changed(); } }, '[x]'));
      }));
    }
    function paintTierWords() {
      const st = d.tiers.steps;
      tiersBox.querySelectorAll('.pl-tier-words').forEach((el, i) => { const { from, to } = TI.stepRange(st, i); el.textContent = `${i + 1}. ${TI.stepLabel(from, to).toLowerCase()}`; });
    }
    const tiersWrap = h('div', null, h('h3', { class: 'sub-title' }, 'steps per billing cycle', ' ', lock ? null : h('button', { class: 'btn btn--line tone--accent', type: 'button', onclick: () => {
      const st = d.tiers.steps;
      if (st.length >= TI.MAX_TIERS) return;
      st.push({ from_kwh: (Number(st[st.length - 1]?.from_kwh) || 0) + 1000, per_kwh: null });
      paintTiers(); paintRule(); changed();
    } }, '[add a step]')),
      tiersBox, tiersMsg,
      fCycle.el, fBase.el,
      note(`Each step’s price applies from its kWh on, counted from the cycle start: this place’s charging, every car, in time order. ${TI.BASE_USE_NOTE}`));
    const ratesBox = h('div', { class: 'lines' });
    const ruleEl = h('div', { class: 'mono-box pl-rule', cover: 'web.geofences.form.auto-cost' });
    const priceBody = h('div', { class: 'frm' });
    function paintRates() {
      replace(ratesBox, d.rates.length ? d.rates.map((r, i) => {
        const inp = (key, attrs) => { const el = h('input', { class: 'in in--cell', disabled: lock, 'aria-label': `${key} of rate ${i + 1}`, ...attrs }); el.addEventListener('input', () => { r[key] = attrs.type === 'number' ? numOrNull(el.value) : el.value; changed(); }); return el; };
        const days = h('select', { class: 'in in--cell', disabled: lock, 'aria-label': `days of rate ${i + 1}` }, [...DAY_CHOICES, ...(DAY_CHOICES.some(([v]) => v === r.days) ? [] : [[r.days, r.days]])].map(([v, l]) => h('option', { value: v, selected: v === (r.days || 'all') }, l)));
        days.addEventListener('change', () => { r.days = days.value; changed(); });
        return h('div', { class: 'pl-rate' }, inp('name', { type: 'text', value: r.name || '', maxlength: '40', size: '10' }), days,
          inp('from', { type: 'text', value: r.from || '00:00', size: '5', inputmode: 'numeric', class: 'in in--cell in--time' }), h('span', { class: 'muted' }, '-'), inp('to', { type: 'text', value: r.to || '24:00', size: '5', inputmode: 'numeric', class: 'in in--cell in--time' }),
          d.mode !== 'minute' ? inp('per_kwh', { type: 'number', step: '0.0001', value: r.per_kwh ?? '', placeholder: '/kWh', size: '6' }) : null,
          d.mode !== 'kwh' && d.mode !== 'tiers' ? inp('per_minute', { type: 'number', step: '0.0001', value: r.per_minute ?? '', placeholder: '/min', size: '6' }) : null,
          lock ? null : h('button', { class: 'btn btn--line muted', type: 'button', 'aria-label': `remove rate ${i + 1}`, onclick: () => { d.rates.splice(i, 1); paintRates(); changed(); } }, '[x]'));
      }) : h('p', { class: 'muted' }, '-- no time-of-use rates: one price all day --'));
    }
    function paintRule() {
      const c = d.currency, part = [];
      if (d.mode === 'tiers') part.push('kWh in each step x that step’s price');
      else if (d.mode !== 'minute') part.push(`kWh x ${d.per_kwh != null ? format(d.per_kwh, 'money', { currency: c }) : 'price per kWh'}`);
      if (d.mode !== 'kwh' && d.mode !== 'tiers') part.push(`minutes x ${d.per_minute != null ? format(d.per_minute, 'money', { currency: c }) : 'price per minute'}`);
      part.push(d.session_fee != null ? `${format(d.session_fee, 'money', { currency: c })} fee` : 'session fee');
      const ex = d.mode === 'tiers' ? TI.workedExample(d.tiers, d.session_fee) : null;
      const exOk = ex && ex.parts.length && ex.parts.every((x) => x.per_kwh != null);
      replace(ruleEl, h('p', null, `cost = ${part.join(' + ')}`),
        ex ? h('p', null, exOk ? `e.g. ${formatNumber(ex.kwh)} kWh from ${formatNumber(ex.before)} kWh into the cycle: ${TI.splitText(ex.parts, c)}${d.session_fee ? ` + ${format(d.session_fee, 'money', { currency: c })}` : ''} = ${format(ex.total, 'money', { currency: c })}` : 'e.g. -- give every step a price --') : null,
        ex && d.rates.length ? h('p', { class: 'muted' }, 'time-of-use rates win their hours; their kWh still count toward the steps') : null);
    }
    function paintPrice() {
      fKwh.el.hidden = d.mode === 'minute' || d.mode === 'tiers';
      fMin.el.hidden = d.mode === 'kwh' || d.mode === 'tiers';
      tiersWrap.hidden = d.mode !== 'tiers';
      if (d.mode === 'tiers') paintTiers();
      priceBody.hidden = !d.priced;
      paintRates(); paintRule();
    }
    replace(priceBody,
      h('div', { class: 'fld', cover: 'web.geofences.form.billing-type' }, h('span', { class: 'fld-label' }, 'priced by:'), h('div', { class: 'fld-box' }, modeKeys)),
      h('div', { cover: 'web.geofences.form.cost-per-unit' }, fCur.el, fKwh.el, fMin.el),
      h('div', { cover: 'web.geofences.form.session-fee' }, fFee.el),
      tiersWrap,
      h('p', { class: 'note', cover: 'web.geofences.form.negative-price' }, 'A price below zero is allowed: some tariffs pay you to charge at times of surplus. The session fee cannot be below zero.'),
      h('div', { cover: 'projects.agile' }, h('h3', { class: 'sub-title' }, 'time-of-use rates', ' ', lock ? null : h('button', { class: 'btn btn--line tone--accent', type: 'button', onclick: () => { d.rates.push({ name: d.rates.length ? `rate ${d.rates.length + 1}` : 'off-peak', days: 'all', from: '00:00', to: '07:00', per_kwh: null, per_minute: null }); paintRates(); changed(); } }, '[add a rate]')),
        ratesBox, note('Rates follow the clock of the place’s time zone. Outside every rate the price above (or the steps) applies. Hourly tariffs are typed here as rates; nothing is fetched from outside.')),
      ruleEl,
      note('A charge that ends inside the circle gets this cost by itself, worked out in 15 minute steps at the rate of each step. A cost typed on a charge by hand always wins.'));
    paintPrice();

    // read the inputs into d
    const readInputs = () => {
      d.name = fName.input.value; d.kind = fKind.input.value; d.time_zone = fZone.input.value;
      d.radius_m = fRadius.input.value === '' ? NaN : toMetric(Number(fRadius.input.value), 'length');
      d.lat = fLat.input.value === '' ? NaN : Number(fLat.input.value); d.lon = fLon.input.value === '' ? NaN : Number(fLon.input.value);
      d.currency = fCur.input.value; d.per_kwh = numOrNull(fKwh.input.value); d.per_minute = numOrNull(fMin.input.value); d.session_fee = numOrNull(fFee.input.value);
    };
    for (const f of Object.values(fields)) { f.input.addEventListener('input', () => { readInputs(); paintRule(); changed(); }); f.input.addEventListener('change', () => { readInputs(); paintRule(); changed(); }); }

    function validate() {
      const e = {};
      const nm = d.name.trim();
      if (!nm) e.name = 'Give the place a name.'; else if (nm.length > 64) e.name = 'At most 64 characters.';
      if (!Number.isFinite(d.lat) || Math.abs(d.lat) > 90) e.lat = 'A latitude from -90 to 90.';
      if (!Number.isFinite(d.lon) || Math.abs(d.lon) > 180) e.lon = 'A longitude from -180 to 180.';
      if (!Number.isFinite(d.radius_m) || d.radius_m < R_MIN || d.radius_m > R_MAX) e.radius_m = `From ${format(R_MIN, 'length')} to ${format(R_MAX, 'length')}.`;
      if (d.priced) {
        if (d.mode === 'tiers') Object.assign(e, TI.tierErrors(d.tiers));
        for (const [k, v] of [['price.per_kwh', d.mode !== 'minute' && d.mode !== 'tiers' ? d.per_kwh : null], ['price.per_minute', d.mode !== 'kwh' && d.mode !== 'tiers' ? d.per_minute : null]]) if (v != null && (!Number.isFinite(v) || v < -10 || v > 100)) e[k] = 'From -10 to 100.';
        if (d.session_fee != null && (!Number.isFinite(d.session_fee) || d.session_fee < 0 || d.session_fee > 1000)) e['price.session_fee'] = 'From 0 to 1,000.';
        if (d.mode !== 'tiers' && (d.mode === 'minute' ? d.per_minute : d.per_kwh) == null && d.session_fee == null && !d.rates.length) e['price.per_kwh'] = 'Type a price, a rate or a session fee, or pick free.';
        d.rates.forEach((r, i) => { if (!(r.name || '').trim() || !HM.test(r.from || '') || !HM.test(r.to || '') || r.from === r.to) e.rates = `Rate ${i + 1}: a name and times as HH:MM (24:00 ends the day), start and end apart.`; });
      }
      return e;
    }
    const showErrors = (e) => {
      for (const [k, f] of Object.entries(fields)) f.setError(e[k] || null);
      const tierMsgs = Object.entries(e).filter(([k]) => k.startsWith('price.tiers')).map(([, v]) => v);
      tiersMsg.hidden = !tierMsgs.length; replace(tiersMsg, [...new Set(tierMsgs)].join(' '));
      const rest = Object.entries(e).filter(([k]) => !fields[k] && !k.startsWith('price.tiers')).map(([, v]) => v);
      formMsg.hidden = !rest.length; replace(formMsg, rest.join(' '));
    };
    const draftBody = () => {
      const b = { name: d.name.trim(), kind: d.kind, lat: d.lat, lon: d.lon, radius_m: Math.round(d.radius_m), price: null };
      if (d.time_zone) b.time_zone = d.time_zone;
      if (d.priced) {
        const k = d.mode !== 'minute', m = d.mode !== 'kwh' && d.mode !== 'tiers', tiered = d.mode === 'tiers';
        b.price = { currency: d.currency, per_kwh: k && !tiered ? d.per_kwh : null, per_minute: m ? d.per_minute : null, session_fee: d.session_fee, tiers: tiered ? TI.tiersBody(d.tiers) : null,
          rates: d.rates.map((r) => ({ name: r.name.trim(), days: r.days || 'all', from: r.from, to: r.to, per_kwh: k ? r.per_kwh ?? null : null, per_minute: m ? r.per_minute ?? null : null })) };
      }
      return b;
    };

    /* the effect on history and overlaps */
    const effBody = h('div', { 'aria-live': 'polite' }, h('p', { class: 'muted' }, 'working out what saving would change'));
    const overlapBox = h('div');
    let effT = 0, effGen = 0;
    ctx.onCleanup(() => clearTimeout(effT));
    async function preview() {
      const my = ++effGen;
      if (Object.keys(validate()).length) { replace(effBody, h('p', { class: 'muted' }, 'fix the marked fields first; then this shows what saving would change')); return; }
      const body = draftBody();
      if (cur) body.id = cur.id;
      let e;
      try { e = await ctx.api.post('places/preview', body); } catch (err) { if (my === effGen && ctx.alive()) replace(effBody, h('p', { class: 'muted' }, err.fields ? Object.values(err.fields).join(' ') : 'what saving would change could not be worked out just now')); return; }
      if (my !== effGen || !ctx.alive()) return;
      replace(effBody, kv([
        ['drives named', formatNumber(e.drives_named || 0)], ['drives unnamed', formatNumber(e.drives_unnamed || 0)],
        ['charges named', formatNumber(e.charges_named || 0)], ['charges unnamed', formatNumber(e.charges_unnamed || 0)],
        ['taken from', e.taken_from?.length ? e.taken_from.map((x) => x.name || x).join(', ') : h('span', { class: 'muted' }, 'no other place')],
        ['would reprice', [formatNumber(e.charges_repriced || 0), h('span', { class: 'muted' }, ' past charges, only when you reprice them')]],
      ], { cols: 1 }));
    }
    function paintOverlaps() {
      const near = others.filter((o) => o.id !== cur?.id).map((o) => ({ o, dm: distM(d.lat, d.lon, o.lat, o.lon) })).filter((x) => Number.isFinite(x.dm) && x.dm < x.o.radius_m + d.radius_m).sort((a, b) => a.dm - b.dm);
      replace(overlapBox, near.length ? h('div', { class: 'lines' }, near.slice(0, 8).map(({ o, dm }) => h('div', { class: 'ln ln--rule' }, h('span', { class: 'ln-t' }, tag('overlap', 'warn')),
        h('span', { class: 'ln-what' }, h('a', { class: 'lnk-plain', href: editUrl(o.id) }, o.name), h('span', { class: 'muted' }, `  ${format(o.radius_m, 'length')} radius, centres ${format(dm, 'length')} apart`)), h('span', { class: 'ln-num' }))))
        : h('p', null, tag('ok', 'ok'), ' this circle overlaps no other place'));
    }
    let setCircle = null, centre = null;
    function changed() {
      dirty = true;
      clearTimeout(effT); effT = setTimeout(preview, 450);
      paintOverlaps();
      if (setCircle && Number.isFinite(d.lat) && Number.isFinite(d.lon) && Number.isFinite(d.radius_m)) { setCircle(d.lon, d.lat, d.radius_m); centre?.setLngLat([d.lon, d.lat]); }
    }

    async function save() {
      if (lock) return;
      readInputs();
      const errs = validate();
      showErrors(errs);
      if (Object.keys(errs).length) return;
      const body = draftBody();
      if (!isNew) body.version = cur.version;
      saveBtn.disabled = true;
      let res;
      try { res = isNew ? await ctx.api.post('places', body) : await ctx.api.patch(`places/${enc(cur.id)}`, body); } catch (e) {
        saveBtn.disabled = false;
        if (!ctx.alive()) return;
        if (e.code === 'stale_version') { formMsg.hidden = false; replace(formMsg, 'Someone else saved this place meanwhile, so nothing was saved. ', h('button', { class: 'btn btn--line tone--accent', type: 'button', onclick: () => { dirty = false; ctx.reload(); } }, '[load theirs]')); return; }
        if (e.status === 404) { dirty = false; state.navigate(listUrl({ missing: cur.id }), { replace: true }); return; }
        if (e.fields) { showErrors(e.fields); return; }
        formMsg.hidden = false; formMsg.textContent = e.message || 'The place could not be saved. Try again.';
        return;
      }
      if (!ctx.alive()) return;
      dirty = false;
      const eff = res.effect || {};
      toast(`${isNew ? 'Added' : 'Saved'} ${res.name}.${eff.drives_named || eff.charges_named ? ` ${formatNumber(eff.drives_named || 0)} drives and ${formatNumber(eff.charges_named || 0)} charges show its name.` : ''}${eff.charges_repriced ? ' Past charges keep their cost until you reprice them.' : ''}`, { next: true });
      ctx.navigate(editUrl(res.id));
    }
    const saveBtn = h('button', { class: 'btn btn--primary', type: 'submit', disabled: lock }, isNew ? 'add the place' : 'save');
    const delBtn = cur && admin ? h('button', { class: 'btn btn--danger', type: 'button', onclick: () => removePlace(cur) }, 'delete') : null;

    /* reprice past charges */
    let reprice = null;
    if (cur) {
      const tz = cur.time_zone || ctx.tz;
      const today = dayKey(ctx.api.now(), tz);
      const fromIn = h('input', { class: 'in in--date', type: 'date', max: today, 'aria-label': 'first day', disabled: lock });
      const toIn = h('input', { class: 'in in--date', type: 'date', max: today, 'aria-label': 'last day', disabled: lock });
      const out = h('div', { 'aria-live': 'polite' });
      const span = () => ({ ...(fromIn.value ? { from_ms: dayStart(fromIn.value, tz) } : {}), ...(toIn.value ? { to_ms: dayStart(addDays(toIn.value, 1), tz) } : {}) });
      const run = async (dry) => {
        if (dirty) { replace(out, h('p', { class: 'tone--warn' }, 'save your changes first: a reprice uses the saved price')); return; }
        let r;
        try { r = await ctx.api.post(`places/${enc(cur.id)}/reprice`, { ...span(), dry_run: dry }); } catch (e) { replace(out, h('p', { class: 'tone--bad' }, e.message || 'that did not work')); return; }
        if (!ctx.alive()) return;
        const money = (l) => (l?.length ? l.map((x) => format(x.amount, 'money', { currency: x.currency })).join(' + ') : '—');
        replace(out, kv([['charges in span', formatNumber(r.charges ?? 0)], [dry ? 'would change' : 'repriced', formatNumber(r.repriced ?? 0)], ['cost before', money(r.cost_before)], [dry ? 'cost after' : 'cost now', money(r.cost_after)]], { cols: 1 }));
        if (!dry) toast(`Repriced ${formatNumber(r.repriced ?? 0)} charges at ${cur.name}.`);
      };
      reprice = section({ title: 'reprice past charges', id: 'reprice', cover: 'web.geofences.form.retro-costs' },
        cur.price ? [h('div', { class: 'optline' }, h('span', { class: 'optline-k' }, 'from'), fromIn, h('span', { class: 'optline-k' }, 'to'), toIn,
          h('button', { class: 'btn', type: 'button', disabled: lock, onclick: () => run(true) }, 'check'),
          h('button', { class: 'btn btn--primary', type: 'button', disabled: lock, onclick: async () => { if (await confirmDialog('Replace the cost of past charges here with the saved price? A cost typed by hand is never touched.', { title: 'reprice?', ok: 'reprice' })) run(false); } }, 'reprice')),
        note('Empty days mean from the first charge or up to now.'), out]
          : note(`${cur.name} has no saved price, so there is nothing to reprice with. Give it a price and save first.`));
    }

    const mapEl = h('div', { class: 'map map--sm', role: 'img', 'aria-label': 'The circle on the map' });
    const formEl = h('form', { class: 'frm', novalidate: true, cover: 'web.geofences.form.validation', onsubmit: (e) => { e.preventDefault(); save(); } },
      h('div', { cover: 'web.geofences.form.name' }, fName.el), fKind.el, fRadius.el, fLat.el, fLon.el, fZone.el,
      h('div', { class: 'fld' }, h('span', { class: 'fld-label' }, 'charging:'), h('div', { class: 'fld-box' }, pricedKeys)),
      priceBody, formMsg,
      h('div', { class: 'acts', cover: 'web.geofences.form.save' }, saveBtn, h('a', { class: 'btn', href: listUrl(), onclick: leave, cover: 'web.geofences.form.back' }, 'leave without saving'), delBtn));
    replace(host,
      section({ title: isNew ? 'new place' : cur.name, side: cur ? h('span', { class: 'muted' }, `${formatNumber(cur.counts?.visits ?? 0)} visits, ${formatNumber(cur.counts?.charges ?? 0)} charges`) : null },
        h('p', { class: 'note', cover: 'web.geofences.form.entry-from-dashboards' }, 'Places can also be made from a drive, charge or stop that has no place yet: its +place link opens this form at that spot.'),
        isNew ? h('p', { class: 'note', cover: 'web.geofences.form.defaults' }, `New places start with a ${format(R_NEW, 'length')} radius at the spot given, kind other, no price and the time zone of the nearest place.`) : h('span', { hidden: true, cover: 'web.geofences.form.defaults' }),
        cur?.address?.short ? h('p', { class: 'pl-addr' }, h('span', { class: 'muted' }, 'centre at '), cur.address.short) : null,
        mapEl, h('p', { class: 'note', cover: 'web.geofences.form.position-map' }, lock ? 'The circle of the place.' : 'Click the map to move the centre there.'),
        formEl),
      section({ title: 'effect on history', cover: 'web.geofences.form.relabel-history' },
        note('Saving renames every drive, charge and stop inside the circle, also those recorded before the place existed. Nothing has changed yet.'), effBody),
      section({ title: 'which place wins', cover: 'web.geofences.form.match-rule' },
        note('A point is inside a place when it is no farther from the centre than the radius. Inside several circles, the place with the nearest centre wins (on a tie, the older one).'), overlapBox),
      reprice);
    paintOverlaps();
    preview();
    const m = await safeMap(mapEl, ctx, { center: [d.lon, d.lat], zoom: 15 });
    if (m && ctx.alive()) {
      setCircle = m.circle('circle', d.lon, d.lat, d.radius_m);
      centre = m.marker([d.lon, d.lat], 'map-pin');
      const rr = d.radius_m;
      m.frame(() => m.map.fitBounds([[d.lon - rr / 50000, d.lat - rr / 80000], [d.lon + rr / 50000, d.lat + rr / 80000]], { duration: 0, maxZoom: 17 }));
      if (!lock) m.map.on('click', (e) => { fLat.input.value = e.lngLat.lat.toFixed(6); fLon.input.value = e.lngLat.lng.toFixed(6); readInputs(); changed(); });
    }
    dirty = false;
  }
}
