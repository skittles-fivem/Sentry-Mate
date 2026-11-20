// Places (/places): the preset bank. Named places are the presets of the radio: the list on top (search,
// kind, sorted and paged by the server, GET /places) and the tuner under it, which edits one place or a new
// one: name and kind, the circle on the map (click to move it), which place wins where circles overlap,
// prices (per kWh or per minute, time-of-day rates, a session fee), the effect on history before saving
// (POST /places/preview) and repricing past charges (POST /places/{id}/reprice). ?place=<id> opens a place,
// ?new=1&lat=&lon= starts one at a spot (every place name in the app links here). Only admins change places.

import * as state from '../shared/state.js';
import { h, replace, cssVar } from '../shared/dom.js';
import { format, convert, toMetric, unitLabel } from '../shared/units.js';
import { formatDate, dayKey, dayStart, addDays } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon, hud, plate, btn, linkBtn, seg, tag, callout } from '../core/ui.js';
import { controls, slot, select, search, stat, rack, plural } from '../core/kit.js';
import { tracks } from '../core/tracks.js';
import { createMap, pin, pad, mapFallback } from '../core/map.js';
import { rpx } from '../core/display.js';
import { emptyState } from '../core/states.js';
import { field } from '../core/form.js';
import { confirmDialog, toast } from '../core/layer.js';
import { isAdmin } from '../core/prefs.js';
import { money } from '../core/fmt.js';

export const id = 'places';
export const css = true;

const enc = encodeURIComponent;
const KINDS = [['home', 'Home', 'home'], ['work', 'Work', 'flag'], ['charger', 'Charger', 'bolt'], ['service', 'Service centre', 'sliders'], ['other', 'Other', 'pin']];
const KIND = Object.fromEntries(KINDS.map(([k, l, i]) => [k, { word: l, icon: i }]));
const R_MIN = 10, R_MAX = 5000, R_NEW = 100;
const DAYS = [['mon-sun', 'Every day'], ['mon-fri', 'Mon to Fri'], ['sat-sun', 'Sat and Sun']];
const CURRENCIES = ['USD', 'EUR', 'GBP', 'CAD', 'AUD', 'CHF', 'NOK', 'SEK', 'DKK', 'JPY', 'NZD'];
const listUrl = (query = null) => state.buildUrl('places', { query });
const editUrl = (pid) => state.buildUrl('places', { query: { place: pid } });
function distM(a, b, c, d) {
  const R = 6371000, r = Math.PI / 180;
  const x = (d - b) * r * Math.cos(((a + c) / 2) * r), y = (c - a) * r;
  return Math.sqrt(x * x + y * y) * R;
}
function circle(lat, lon, m) {
  const pts = [];
  for (let i = 0; i <= 64; i++) {
    const a = (i / 64) * 2 * Math.PI;
    pts.push([lon + ((m * Math.sin(a)) / (111320 * Math.cos((lat * Math.PI) / 180))), lat + (m * Math.cos(a)) / 110540]);
  }
  return { type: 'Feature', properties: {}, geometry: { type: 'Polygon', coordinates: [pts] } };
}

export async function mount(ctx) {
  const admin = isAdmin();
  const q = ctx.query;
  const f = { search: q.search || '', kind: q.kind || '' };

  /* ---------- the list */
  const missing = h('div', { hidden: !q.missing, cover: 'web.geofences.form.not-found' },
    callout('warn', 'That place does not exist', `There is no place "${q.missing || ''}". It was deleted, or the link is old. Every place there is is listed below.`));
  const emptyBox = h('div', { hidden: true, cover: 'web.geofences.list.empty-state' },
    hud({ pad: false }, emptyState({ icon: 'pin', title: 'No presets yet', text: 'Without places, drives and charges show street addresses and charges have no price. Add your home first: the tuner below starts where a car is now.' })));
  const list = tracks(ctx, {
    title: 'Presets', icon: 'pin', path: 'places', params: { search: f.search || null, kind: f.kind || null }, limit: 50, csvName: 'places', sort: { key: 'name', dir: 'asc' },
    note: 'Sorted by name until you pick another column. Counts cover the cars your account sees. Edit opens the place in the tuner below.',
    cover: 'web.geofences.list.order',
    rowCls: (p) => (p.id === q.place ? 'is-picked' : null),
    onPage: (pg) => { emptyBox.hidden = !!(pg.items?.length || f.search || f.kind); },
    columns: [
      { key: 'name', label: 'Name', sort: 'name', firstDir: 'asc', lead: true, cover: 'web.geofences.list.col-name',
        render: (p) => h('span', { class: 'cell' }, h('a', { class: 'cell-main pl-name', href: editUrl(p.id) }, icon(KIND[p.kind]?.icon || 'pin', 'i--sm'), h('span', null, p.name)), h('span', { class: 'cell-sub' }, `${KIND[p.kind]?.word || 'Other'}${p.address?.short ? ` · ${p.address.short}` : ''}`)) },
      { key: 'circle', label: 'Circle', num: true, cover: ['web.geofences.list.col-position', 'web.geofences.list.col-radius'], csv: (p) => `${p.lat},${p.lon},${p.radius_m}`,
        render: (p) => h('span', { class: 'cell' }, h('span', null, `${p.lat.toFixed(4)}, ${p.lon.toFixed(4)}`), h('span', { class: 'cell-sub' }, `${format(p.radius_m, 'length')} radius`)) },
      { key: 'history', label: 'History', num: true, sort: 'visits', firstDir: 'desc', csv: (p) => p.counts?.visits,
        render: (p) => h('span', { class: 'cell' }, h('span', null, plural(p.counts?.visits ?? 0, 'stop')), h('span', { class: 'cell-sub' }, `${plural(p.counts?.drives ?? 0, 'drive')} · ${plural(p.counts?.charges ?? 0, 'charge')}`)) },
      { key: 'price', label: 'Price', num: true, csv: (p) => (p.price ? `${p.price.per_kwh ?? ''} ${p.price.currency}` : ''),
        render: (p) => (p.price ? h('span', { class: 'cell' }, h('span', null, p.price.per_kwh != null ? format({ amount: p.price.per_kwh, currency: p.price.currency }, 'price_energy') : format({ amount: p.price.per_minute, currency: p.price.currency }, 'price_time')),
          p.price.rates?.length ? h('span', { class: 'cell-sub' }, plural(p.price.rates.length, 'rate')) : null) : h('span', { class: 'faint' }, 'no price')) },
      { key: 'last', label: 'Last visit', num: true, render: (p) => (p.last_visit_ms ? formatDate(p.last_visit_ms, p.time_zone || ctx.tz, { year: true, weekday: false }) : '—'), csv: (p) => p.last_visit_ms },
      { key: 'act', label: '', csv: false, cover: ['web.geofences.list.edit', 'web.geofences.list.delete'],
        render: (p) => h('span', { class: 'pl-acts' }, linkBtn(admin ? 'Edit' : 'View', editUrl(p.id), { sm: true, cover: 'web.geofences.list.edit' }),
          admin ? btn(null, () => removePlace(p), { sm: true, kind: 'ghost', icon: 'x', label: `Delete ${p.name}`, cover: 'web.geofences.list.delete' }) : null) },
    ],
    empty: () => emptyState({ icon: 'search', title: 'No place matches', text: 'Clear the search or pick every kind.' }),
  });
  const setF = (patch) => { Object.assign(f, patch); ctx.setQuery({ search: f.search || null, kind: f.kind || null }); list.reload({ search: f.search || null, kind: f.kind || null }); };

  async function removePlace(p) {
    let eff = null;
    try { eff = await ctx.api.post('places/preview', { place: p.id, delete: true }); } catch { eff = null; }
    const ok = await confirmDialog(`${eff ? `${plural(eff.drives_unnamed || 0, 'drive')} and ${plural(eff.charges_unnamed || 0, 'charge')} lose the name ${p.name} and show their address again. ` : ''}Costs already worked out stay.`, { title: `Delete ${p.name}?`, ok: 'Delete', danger: true });
    if (!ok || !ctx.alive()) return;
    try { await ctx.api.del(`places/${enc(p.id)}`, { params: { version: p.version } }); toast(`Deleted ${p.name}.`); if (q.place === p.id) ctx.navigate(listUrl()); else list.reload(); } catch (e) {
      toast(e.code === 'stale_version' ? `${p.name} was changed by someone else meanwhile; nothing was deleted.` : e.message, { kind: 'error' });
    }
  }

  /* ---------- the tuner (editor) */
  const [cars, others] = await Promise.all([
    ctx.api.list('cars', { sort: 'last_activity', dir: 'desc' }, { max: 50 }).all().catch(() => []),
    ctx.api.list('places', { sort: 'name' }, { max: 1000 }).all().catch(() => []),
  ]);
  if (!ctx.alive()) return;
  let cur = null;
  if (q.place) {
    try { cur = await ctx.api.get(`places/${enc(q.place)}`); } catch (e) {
      if (e.status === 404) { ctx.navigate(listUrl({ missing: q.place }), { replace: true }); return; }
      ctx.fail(e); return;
    }
    if (!ctx.alive()) return;
  }
  let start = cur;
  if (!start) {
    const qLat = parseFloat(q.lat), qLon = parseFloat(q.lon);
    if (Number.isFinite(qLat) && Number.isFinite(qLon)) start = { lat: qLat, lon: qLon };
    else {
      let now = null;
      if (cars[0]) { try { now = await ctx.api.get(`cars/${enc(cars[0].id)}/now`); } catch { now = null; } }
      const la = now?.location?.lat?.value, lo = now?.location?.lon?.value;
      start = la != null ? { lat: la, lon: lo, from: cars[0].name } : others[0] ? { lat: others[0].lat, lon: others[0].lon } : { lat: 0, lon: 0 };
    }
    if (!ctx.alive()) return;
  }
  const lock = !admin;
  const d = { lat: start.lat, lon: start.lon, radius_m: cur?.radius_m ?? R_NEW, name: cur?.name || '', kind: cur?.kind || (others.some((p) => p.kind === 'home') ? 'other' : 'home') };
  const pr = cur?.price || null;
  let mode = pr ? (pr.per_minute != null && pr.per_kwh == null ? 'minute' : 'kwh') : 'none';
  let rates = (pr?.rates || []).map((r) => ({ ...r }));
  let dirty = false;

  const L = unitLabel('length');
  const fName = field({ label: 'Name', name: 'name', value: d.name, required: true, hint: 'Shown on every drive, charge and stop inside the circle.', attrs: { maxlength: '80', disabled: lock, autocomplete: 'off' } });
  const fKind = field({ label: 'Kind', name: 'kind', type: 'select', value: d.kind, options: KINDS.map(([k, l]) => [k, l]), attrs: { disabled: lock } });
  const fLat = field({ label: 'Latitude', name: 'lat', type: 'number', value: d.lat.toFixed(5), attrs: { step: 'any', min: '-90', max: '90', disabled: lock } });
  const fLon = field({ label: 'Longitude', name: 'lon', type: 'number', value: d.lon.toFixed(5), attrs: { step: 'any', min: '-180', max: '180', disabled: lock } });
  const fRad = field({ label: `Radius (${L})`, name: 'radius_m', type: 'number', value: String(Math.round(convert(d.radius_m, 'length'))), hint: `${format(R_MIN, 'length')} to ${format(R_MAX, 'length')}`,
    attrs: { step: '1', min: String(Math.ceil(convert(R_MIN, 'length'))), max: String(Math.floor(convert(R_MAX, 'length'))), disabled: lock } });
  const fCur = field({ label: 'Currency', name: 'currency', type: 'select', value: pr?.currency || 'USD', options: [...new Set([pr?.currency, ...CURRENCIES].filter(Boolean))].map((c) => [c, c]), attrs: { disabled: lock } });
  const fKwh = field({ label: 'Per kWh', name: 'per_kwh', type: 'number', value: pr?.per_kwh ?? '', attrs: { step: 'any', disabled: lock } });
  const fMin = field({ label: 'Per minute', name: 'per_minute', type: 'number', value: pr?.per_minute ?? '', attrs: { step: 'any', disabled: lock } });
  const fFee = field({ label: 'Session fee', name: 'session_fee', type: 'number', value: pr?.session_fee ?? '', hint: 'Added once per charge.', attrs: { step: 'any', min: '0', disabled: lock } });
  const formMsg = h('div', { class: 'form-msg', role: 'alert', hidden: true });
  const num = (fl) => (fl.input.value.trim() === '' ? null : Number(fl.input.value));

  function validate(quiet = false) {
    const errs = {};
    if (!fName.input.value.trim()) errs.name = 'Give the place a name.';
    const la = num(fLat), lo = num(fLon), r = num(fRad);
    if (la == null || la < -90 || la > 90) errs.lat = 'A latitude is from -90 to 90.';
    if (lo == null || lo < -180 || lo > 180) errs.lon = 'A longitude is from -180 to 180.';
    const rm = r == null ? null : toMetric(r, 'length');
    if (rm == null || rm < R_MIN - 0.5 || rm > R_MAX + 0.5) errs.radius_m = `The radius is from ${format(R_MIN, 'length')} to ${format(R_MAX, 'length')}.`;
    if (mode === 'kwh' && num(fKwh) == null && !rates.length) errs.per_kwh = 'Give a price per kWh, or add a rate.';
    if (mode === 'minute' && num(fMin) == null && !rates.length) errs.per_minute = 'Give a price per minute, or add a rate.';
    if (num(fFee) != null && num(fFee) < 0) errs.session_fee = 'A session fee cannot be below 0.';
    if (!quiet) for (const fl of [fName, fLat, fLon, fRad, fKwh, fMin, fFee]) fl.setError(errs[fl.name] || null);
    return errs;
  }
  function draft() {
    const b = { name: fName.input.value.trim(), kind: fKind.input.value, lat: num(fLat), lon: num(fLon), radius_m: Math.round(toMetric(num(fRad) ?? 0, 'length')) };
    b.price = mode === 'none' ? null : {
      currency: fCur.input.value, per_kwh: mode === 'kwh' ? num(fKwh) : null, per_minute: mode === 'minute' ? num(fMin) : null, session_fee: num(fFee),
      rates: rates.map((r) => ({ name: r.name || 'Rate', days: r.days, from: r.from, to: r.to, per_kwh: mode === 'kwh' ? (r.per_kwh ?? null) : null, per_minute: mode === 'minute' ? (r.per_minute ?? null) : null })),
    };
    return b;
  }

  /* prices */
  const priceBox = h('div', { class: 'pl-price' });
  const modeSeg = seg([['none', 'No price'], ['kwh', 'Per kWh'], ['minute', 'Per minute']], mode, (v) => { mode = v; changed(); paintPrice(); }, { label: 'Priced by', cover: 'web.geofences.form.billing-type' });
  const rateRow = (r, i) => {
    const inp = (key, type, attrs) => { const el = h('input', { class: 'input', type, value: r[key] ?? '', disabled: lock, ...attrs }); el.addEventListener('input', () => { r[key] = type === 'number' ? (el.value === '' ? null : Number(el.value)) : el.value; changed(); }); return el; };
    const daysSel = select(DAYS, r.days || 'mon-sun', (v) => { r.days = v; changed(); }, { label: 'Days' });
    daysSel.disabled = lock;
    return h('div', { class: 'pl-rate' },
      inp('name', 'text', { 'aria-label': 'Rate name', placeholder: 'Name', maxlength: '40' }), daysSel,
      inp('from', 'time', { 'aria-label': 'From' }), inp('to', 'time', { 'aria-label': 'To' }),
      inp(mode === 'minute' ? 'per_minute' : 'per_kwh', 'number', { 'aria-label': mode === 'minute' ? 'Price per minute' : 'Price per kWh', step: 'any' }),
      btn(null, () => { rates.splice(i, 1); changed(); paintPrice(); }, { icon: 'x', sm: true, kind: 'ghost', label: 'Remove this rate', disabled: lock }));
  };
  function paintPrice() {
    fKwh.el.hidden = mode !== 'kwh'; fMin.el.hidden = mode !== 'minute';
    const priced = mode !== 'none';
    replace(priceBox,
      priced ? h('div', { class: 'pl-grid', cover: 'web.geofences.form.cost-per-unit' }, fCur.el, fKwh.el, fMin.el, h('div', { cover: 'web.geofences.form.session-fee' }, fFee.el))
        : h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', { cover: ['web.geofences.form.cost-per-unit', 'web.geofences.form.session-fee'] }, 'No price: charges here get no cost unless one is typed on the charge. Pick per kWh or per minute to give a price, a session fee and rates.')),
      h('div', { class: 'pl-rates', cover: 'projects.agile' },
        h('p', { class: 'scope-title' }, 'Rates by time of day'),
        priced && rates.length ? rates.map(rateRow) : null,
        h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, priced ? 'A rate wins over the base price while it runs, in the place\'s own time zone; a charge across rates is split by the minute.' : 'Rates by time of day need a price first.')),
        priced && !lock ? btn('Add a rate', () => { rates.push({ name: '', days: 'mon-fri', from: '00:00', to: '07:00', per_kwh: null, per_minute: null }); changed(); paintPrice(); }, { icon: 'plus', sm: true }) : null));
  }
  paintPrice();

  /* effect on history */
  const effBody = h('div', { 'aria-live': 'polite' });
  let effT = 0, effGen = 0;
  const previewSoon = () => { clearTimeout(effT); effT = setTimeout(previewNow, 450); };
  ctx.onCleanup(() => clearTimeout(effT));
  async function previewNow() {
    const my = ++effGen;
    if (Object.keys(validate(true)).length) { replace(effBody, h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, 'Fix the marked fields; then this shows what saving would change.'))); return; }
    const body = draft();
    if (cur) body.id = cur.id;
    let e;
    try { e = await ctx.api.post('places/preview', body); } catch (err) {
      if (my !== effGen || !ctx.alive() || err.name === 'AbortError') return;
      replace(effBody, h('p', { class: 'note' }, icon('warn', 'i--sm'), h('span', null, err.fields ? Object.values(err.fields).join(' ') : 'What saving would change could not be worked out just now.')));
      return;
    }
    if (my !== effGen || !ctx.alive()) return;
    replace(effBody, rack([
      stat('Drives named', e.drives_named ?? 0, 'count', { how: 'start or end inside the circle' }),
      stat('Drives unnamed', e.drives_unnamed ?? 0, 'count', { how: 'lose this name' }),
      stat('Charges named', e.charges_named ?? 0, 'count'),
      stat('Charges unnamed', e.charges_unnamed ?? 0, 'count'),
      stat('Taken from others', (e.drives_taken ?? 0) + (e.charges_taken ?? 0), 'count', { how: e.taken_from?.length ? `from ${e.taken_from.map((x) => x.name || x).join(', ')}` : 'no other place loses rows' }),
      stat('Would reprice', e.charges_repriced ?? 0, 'count', { how: 'past charges, only if you choose to' }),
    ]));
  }

  /* overlaps */
  const overlapBox = h('div');
  function paintOverlaps() {
    const near = others.filter((p) => p.id !== cur?.id).map((p) => ({ p, dm: distM(d.lat, d.lon, p.lat, p.lon) })).filter((x) => x.dm < x.p.radius_m + d.radius_m).sort((a, b) => a.dm - b.dm);
    replace(overlapBox, !near.length ? h('p', { class: 'note' }, icon('check', 'i--sm'), h('span', null, 'This circle overlaps no other place.'))
      : h('ul', { class: 'pl-overlaps' }, near.slice(0, 8).map(({ p, dm }) => h('li', null, h('a', { href: editUrl(p.id) }, p.name),
        h('span', { class: 'faint' }, ` · ${format(p.radius_m, 'length')} radius, centres ${format(dm, 'length')} apart`), dm <= p.radius_m ? [' ', tag('holds this centre', 'org')] : null))));
  }

  /* the map */
  const mapEl = h('div', { class: 'map pl-map', role: 'img', 'aria-label': 'The circle of the place on the map. Click to move its centre.' });
  let m = null, centre = null;
  function paintMap() {
    if (!m) return;
    m.map.getSource('pl-circle')?.setData(circle(d.lat, d.lon, d.radius_m));
    m.map.getSource('pl-others')?.setData({ type: 'FeatureCollection', features: others.filter((p) => p.id !== cur?.id && distM(d.lat, d.lon, p.lat, p.lon) < 20000).map((p) => circle(p.lat, p.lon, p.radius_m)) });
    centre?.setLngLat([d.lon, d.lat]);
  }
  function changed() {
    dirty = true;
    const la = num(fLat), lo = num(fLon), r = num(fRad);
    if (la != null && la >= -90 && la <= 90) d.lat = la;
    if (lo != null && lo >= -180 && lo <= 180) d.lon = lo;
    if (r != null) { const rm = toMetric(r, 'length'); if (rm >= R_MIN && rm <= R_MAX) d.radius_m = rm; }
    paintMap(); paintOverlaps(); previewSoon();
  }
  for (const fl of [fName, fKind, fLat, fLon, fRad, fCur, fKwh, fMin, fFee]) fl.input.addEventListener(fl.input.tagName === 'SELECT' ? 'change' : 'input', changed);

  /* save, reprice */
  async function save() {
    formMsg.hidden = true;
    if (Object.keys(validate()).length) { formMsg.hidden = false; replace(formMsg, icon('warn', 'i--sm'), h('span', null, 'Some fields need a look; they are marked.')); return; }
    const body = draft();
    let res;
    try { res = cur ? await ctx.api.patch(`places/${enc(cur.id)}`, { ...body, version: cur.version }) : await ctx.api.post('places', body); } catch (e) {
      if (e.fields) for (const fl of [fName, fLat, fLon, fRad, fKwh, fMin, fFee]) fl.setError(e.fields[fl.name] || null);
      formMsg.hidden = false;
      replace(formMsg, icon('warn', 'i--sm'), h('span', null, e.code === 'stale_version' ? 'Someone else saved this place meanwhile, so nothing was saved. Load the page again to see their version.' : e.message || 'The place could not be saved.'));
      return;
    }
    dirty = false;
    const eff = res.effect || {};
    toast(`${cur ? 'Saved' : 'Added'} ${res.name}.${eff.drives_named || eff.charges_named ? ` ${plural(eff.drives_named || 0, 'drive')} and ${plural(eff.charges_named || 0, 'charge')} show its name.` : ''}${eff.charges_repriced ? ` ${plural(eff.charges_repriced, 'past charge')} could be repriced below.` : ''}`, { next: true });
    ctx.navigate(editUrl(res.id));
  }
  const repOut = h('div', { 'aria-live': 'polite' });
  const tzP = cur?.time_zone || ctx.tz;
  const today = dayKey(ctx.api.now(), tzP);
  const fromIn = h('input', { class: 'input', type: 'date', 'aria-label': 'First day', max: today, disabled: lock });
  const toIn = h('input', { class: 'input', type: 'date', 'aria-label': 'Last day', max: today, disabled: lock });
  const onlyIn = h('input', { type: 'checkbox', disabled: lock });
  async function reprice(dry) {
    if (!cur?.price) { replace(repOut, h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, 'Save a price first: a reprice uses the saved price.'))); return; }
    if (dirty) { replace(repOut, h('p', { class: 'note' }, icon('warn', 'i--sm'), h('span', null, 'Save your changes first: a reprice uses the saved price.'))); return; }
    const sp = { ...(fromIn.value ? { from_ms: dayStart(fromIn.value, tzP) } : {}), ...(toIn.value ? { to_ms: dayStart(addDays(toIn.value, 1), tzP) } : {}), only_without_cost: onlyIn.checked };
    let r;
    try { r = await ctx.api.post(`places/${enc(cur.id)}/reprice`, { ...sp, dry_run: true }); } catch (e) { replace(repOut, h('p', { class: 'note' }, icon('warn', 'i--sm'), h('span', null, e.message))); return; }
    const show = (x, done) => replace(repOut, rack([
      stat('Charges in the span', x.charges, 'count'), stat(done ? 'Repriced' : 'Would change', x.repriced, 'count', { how: `${formatNumber(x.unchanged ?? 0)} keep the same cost` }),
      stat('Cost before', x.cost_before?.length ? money(x.cost_before) : null, null), stat(done ? 'Cost now' : 'Cost after', x.cost_after?.length ? money(x.cost_after) : null, null)]));
    show(r, false);
    if (dry || !r.repriced) return;
    if (!(await confirmDialog(`Reprice ${plural(r.repriced, 'past charge')} at ${cur.name} with the saved price? Their old costs are replaced. A cost typed by hand is never touched.`, { title: 'Reprice past charges?', ok: 'Reprice' }))) return;
    try { const done = await ctx.api.post(`places/${enc(cur.id)}/reprice`, { ...sp, dry_run: false }); show(done, true); toast(`Repriced ${plural(done.repriced, 'charge')}.`); } catch (e) { toast(e.message, { kind: 'error' }); }
  }

  const tuner = h('form', { class: 'stack pl-tuner', novalidate: true, cover: 'web.geofences.form.validation', onsubmit: (e) => { e.preventDefault(); save(); } },
    h('div', { class: 'band' }, h('span', { class: 'band-no' }, cur ? 'ED' : 'NEW'),
      h('h2', { class: 'band-title', cover: 'web.geofences.form.breadcrumb' }, h('a', { href: listUrl() }, 'Places'), ' / ', cur ? cur.name : 'New place'), h('span', { class: 'band-rule', 'aria-hidden': 'true' })),
    lock ? callout('info', 'View only', 'Only an admin can add, change or delete places.') : null,
    h('div', { class: 'cols' },
      hud({ title: 'The place', icon: 'pin', tone: 'cyan' },
        h('div', { class: 'pl-grid' }, h('div', { cover: 'web.geofences.form.name' }, fName.el), fKind.el, fLat.el, fLon.el, fRad.el),
        h('p', { class: 'note pl-line', cover: 'web.geofences.form.defaults' }, icon('info', 'i--sm'), h('span', null, cur ? `Created ${cur.created_ms ? formatDate(cur.created_ms, tzP, { year: true }) : 'before the import'}; ${plural(cur.counts?.visits ?? 0, 'stop')} here so far.` : `A new place starts ${start.from ? `where ${start.from} is now` : q.lat ? 'at the spot you came from' : 'at the first place'}, with a ${format(R_NEW, 'length')} circle. Click the map to move it.`)),
        h('p', { class: 'note pl-line', cover: 'web.geofences.form.entry-from-dashboards' }, icon('info', 'i--sm'), h('span', null, 'Every place name on a drive, charge, timeline entry or stop opens this tuner; a bare address starts a new place at that spot.'))),
      hud({ title: 'On the map', icon: 'globe', pad: false, cover: 'web.geofences.form.position-map' }, mapEl)),
    h('div', { class: 'cols' },
      hud({ title: 'Which place wins', icon: 'pin', cover: 'web.geofences.form.match-rule' },
        h('p', { class: 'note pl-line' }, icon('info', 'i--sm'), h('span', null, 'A point is inside a place when it is no farther from the centre than the radius. Inside several circles, the place whose centre is nearest wins (on a tie, the older place).')), overlapBox),
      hud({ title: 'Effect on history', icon: 'timeline', cover: 'web.geofences.form.relabel-history' },
        h('p', { class: 'note pl-line' }, icon('info', 'i--sm'), h('span', null, 'Names follow the place as it is now: saving renames every drive, charge and stop inside the circle, history before the place existed too. Nothing below has changed yet.')), effBody)),
    hud({ title: 'Prices', icon: 'bolt', side: modeSeg.el },
      priceBox,
      h('p', { class: 'note pl-line', cover: 'web.geofences.form.negative-price' }, icon('info', 'i--sm'), h('span', null, 'A price below zero is allowed: some tariffs pay you to charge at times. A session fee cannot be below zero.')),
      h('p', { class: 'note pl-line', cover: 'web.geofences.form.auto-cost' }, icon('info', 'i--sm'), h('span', null, 'A charge that ends here gets its cost from this price by itself: energy from the grid where it was measured, else energy into the pack. A cost typed on a charge always wins.'))),
    hud({ title: 'Reprice past charges', icon: 'clock', cover: 'web.geofences.form.retro-costs' },
      h('p', { class: 'note pl-line' }, icon('info', 'i--sm'), h('span', null, cur?.price ? `Past charges at ${cur.name} keep the cost they had. Pick a span to price them with the saved price; see first what would change.` : 'Once a price is saved, past charges here can be priced for a span you choose.')),
      h('div', { class: 'row pl-rep' }, h('label', { class: 'field' }, h('span', { class: 'field-label' }, 'First day'), fromIn), h('label', { class: 'field' }, h('span', { class: 'field-label' }, 'Last day'), toIn),
        h('label', { class: 'check pl-only' }, onlyIn, h('span', null, 'Only charges without a cost')),
        btn('See what changes', () => reprice(true), { sm: true, disabled: lock }), btn('Reprice', () => reprice(false), { sm: true, kind: 'go', disabled: lock })),
      repOut),
    formMsg,
    h('div', { class: 'pl-bar' },
      h('span', { cover: 'web.geofences.form.save' }, btn(cur ? 'Save the place' : 'Add the place', null, { kind: 'go', type: 'submit', icon: 'check', disabled: lock })),
      linkBtn('Leave without saving', listUrl(), { kind: 'ghost', icon: 'arrow-l', cover: 'web.geofences.form.back' }),
      cur && admin ? btn('Delete', () => removePlace(cur), { kind: 'danger', icon: 'x' }) : null));

  replace(ctx.view, h('div', { class: 'page pl' },
    plate({ kicker: h('span', { cover: 'web.geofences.list.breadcrumb' }, h('a', { href: '/' }, 'All cars'), ' / Places'), title: 'Places', aim: ctx.info.aim,
      tools: admin ? linkBtn('New place', listUrl({ new: 1 }), { kind: 'go', icon: 'plus', cover: 'web.geofences.list.create' }) : btn('New place', null, { icon: 'lock', disabled: true, cover: 'web.geofences.list.create' }) }),
    missing,
    hud({ title: 'What places do', icon: 'info', cover: 'faq.geofence-purpose' }, h('div', { class: 'cols--3 cols pl-intro' },
      h('div', null, h('h3', null, 'Your own names'), h('p', null, 'A place gives a spot one name. Every drive, charge and stop inside its circle shows it, history from before the place existed too.')),
      h('div', null, h('h3', null, 'Charging prices'), h('p', null, 'A price per kWh or per minute, rates by time of day and a session fee. A charge that ends there gets its cost by itself.')),
      h('div', null, h('h3', null, 'One place per point'), h('p', null, 'Where circles overlap, the nearest centre wins. The tuner shows overlaps and what a change renames before you save.')))),
    controls({ label: 'Find a place' },
      slot('Find', search(f.search, (v) => setF({ search: v }), { placeholder: 'Name or address', ctx }), { wide: true }),
      slot('Kind', select([['', 'Every kind'], ...KINDS.map(([k, l]) => [k, l])], f.kind, (v) => setF({ kind: v }), { label: 'Kind' }))),
    emptyBox, list.el, tuner));

  paintOverlaps();
  try { await list.ready; } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  previewNow();
  ctx.ready();
  if (q.place || q.new) ctx.view.querySelector('.pl-tuner')?.scrollIntoView({ block: 'start' });
  const beforeUnload = (e) => { if (dirty) { e.preventDefault(); e.returnValue = ''; } };
  ctx.on(window, 'beforeunload', beforeUnload);

  try {
    m = await createMap(mapEl, ctx, { center: [d.lon, d.lat], zoom: 16 });
    if (!m) return;
    await m.ready;
    if (!ctx.alive()) return;
    m.map.addSource('pl-others', { type: 'geojson', data: { type: 'FeatureCollection', features: [] } });
    m.map.addLayer({ id: 'pl-others-fill', type: 'fill', source: 'pl-others', paint: { 'fill-color': cssVar('--cyan'), 'fill-opacity': 0.1 } });
    m.addLayer({ id: 'pl-others-line', type: 'line', source: 'pl-others', paint: { 'line-color': cssVar('--cyan'), 'line-opacity': 0.6 } }, { 'line-width': () => rpx(1.5) });
    m.map.addSource('pl-circle', { type: 'geojson', data: circle(d.lat, d.lon, d.radius_m) });
    m.map.addLayer({ id: 'pl-fill', type: 'fill', source: 'pl-circle', paint: { 'fill-color': cssVar('--mag'), 'fill-opacity': 0.18 } });
    m.addLayer({ id: 'pl-line', type: 'line', source: 'pl-circle', paint: { 'line-color': cssVar('--mag') } }, { 'line-width': () => rpx(2.5) });
    centre = pin(m, [d.lon, d.lat], { tone: 'mag' });
    paintMap();
    if (!lock) m.map.on('click', (e) => { fLat.input.value = e.lngLat.lat.toFixed(5); fLon.input.value = e.lngLat.lng.toFixed(5); changed(); });
    m.frame(() => { const b = circle(d.lat, d.lon, d.radius_m * 1.6).geometry.coordinates[0]; m.map.fitBounds([[Math.min(...b.map((p) => p[0])), Math.min(...b.map((p) => p[1]))], [Math.max(...b.map((p) => p[0])), Math.max(...b.map((p) => p[1]))]], { padding: pad(24), duration: 0, maxZoom: 18 }); });
  } catch (e) { console.warn(e); replace(mapEl, mapFallback()); }
}
