// Places (/places): name the spots that matter, give them charging prices, and see what a change does to
// history before it is saved. One page, two views chosen by the address:
//   /places                         the list (search, kind, sort by name, visits or last visit; server CSV),
//                                   a map of every circle, and suggestions from where the cars park most;
//   /places?place=<id>              the editor for a place;
//   /places?new=1[&lat=&lon=&radius=]   the editor for a new place (from a recorded position, a suggestion,
//                                   or a drive, charge or stop that has no place yet).
// Matching is live on the server: a saved change renames every drive, charge and stop inside the circle at
// once; of overlapping circles the nearest centre wins. Costs stay put: charges that ended keep the cost
// they had until someone reprices them (POST /places/{place}/reprice) for a chosen span.
// The editor asks POST /places/preview after each change, so the effect on history is known before saving.

import * as state from '../shared/state.js';
import { h, replace, cssVar } from '../shared/dom.js';
import { format, formatMoney, formatMoneyList, convert, toMetric, unitLabel } from '../shared/units.js';
import { formatNumber } from '../shared/i18n.js';
import * as TI from '../shared/tiers.js';
import { dayKey, dayStart, addDays } from '../shared/time.js';
import { icon } from '../core/icons.js';
import { pageHead, panel, metric, note } from '../core/page.js';
import { dataTable } from '../core/table.js';
import { emptyState } from '../core/states.js';
import { createMap, mapFallback, boundsOf } from '../core/map.js';
import { field } from '../core/form.js';
import { openDialog, confirmDialog } from '../core/dialog.js';
import { flash } from '../core/flash.js';
import { rpx } from '../core/display.js';
import { dur, ago, dateTime, DASH } from '../core/fmt.js';

export const id = 'places';
export const css = true;
export const title = (ctx) => (ctx.query?.new ? 'New place' : ctx.query?.place ? 'Edit place' : 'Places');

const enc = encodeURIComponent;
const KINDS = [
  ['home', 'Home', 'home'], ['work', 'Work', 'flag'], ['charger', 'Charger', 'bolt'],
  ['service', 'Service centre', 'settings'], ['other', 'Other', 'pin'],
];
const KIND_LABEL = Object.fromEntries(KINDS.map(([k, l]) => [k, l]));
const KIND_ICON = Object.fromEntries(KINDS.map(([k, , i]) => [k, i]));
const KIND_COLOR = { home: '--live', work: '--live-2', charger: '--c-charge', service: '--warm', other: '--text-2' };
const CURRENCIES = ['USD', 'CAD', 'EUR', 'GBP', 'NOK', 'SEK', 'DKK', 'CHF', 'AUD', 'NZD', 'JPY', 'CNY'];
const DAYS = ['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun'];
const DAY_NAME = { mon: 'Mon', tue: 'Tue', wed: 'Wed', thu: 'Thu', fri: 'Fri', sat: 'Sat', sun: 'Sun' };
const DAY_CHOICES = [['all', 'Every day'], ['mon-fri', 'Weekdays (Mon–Fri)'], ['sat,sun', 'Weekends (Sat, Sun)'], ...DAYS.map((d) => [d, `${DAY_NAME[d]} only`])];
const R_MIN = 10, R_MAX = 5000, R_NEW = 100;
const HM_RX = /^(([01]\d|2[0-3]):[0-5]\d|24:00)$/;

const listUrl = (query = null) => state.buildUrl('places', { query });
const editUrl = (pid) => state.buildUrl('places', { query: { place: pid } });
const newUrl = (q = {}) => state.buildUrl('places', { query: { new: 1, ...q } });
const kindLabel = (k) => KIND_LABEL[k] || 'Other';
// a place's counts are all time, so the lists they open show all time too
const ALL_TIME = { preset: 'all' };
const chargesHref = (pid) => state.buildUrl('charges', { period: ALL_TIME, query: { cars: 'all', places: pid } });
const drivesHref = (carId, pid) => state.buildUrl('drives', { car: carId, period: ALL_TIME, query: { places: pid } });

/** Drives per car at each place (GET /places?car= for every car): Map place id -> [{ car, name, drives }], most first. */
async function drivesByCar(ctx, cars) {
  const out = new Map();
  const lists = await Promise.all(cars.map((c) => ctx.api.list('places', { car: c.id, sort: 'drives' }, { max: 1000 }).all()
    .then((rows) => [c, rows]).catch(() => [c, []])));
  for (const [c, rows] of lists) {
    for (const p of rows) {
      const n = p.counts?.drives || 0;
      if (!n) continue;
      if (!out.has(p.id)) out.set(p.id, []);
      out.get(p.id).push({ car: c.id, name: c.name, drives: n });
    }
  }
  for (const v of out.values()) v.sort((a, b) => b.drives - a.drives || a.name.localeCompare(b.name));
  return out;
}
const fix5 = (v) => (v == null ? DASH : Number(v).toFixed(5));
const posText = (lat, lon) => `${fix5(lat)}, ${fix5(lon)}`;
/** A first name for a suggested spot: its label, else its street, else its town. */
const suggName = (s) => (s.address?.short || s.label || s.address?.locality || '').replace(/^near /i, '').slice(0, 64) || null;

/* ------------------------------------------------------------------ geometry (metres on the earth) */

const RAD = Math.PI / 180;
function distM(aLat, aLon, bLat, bLon) {
  const x = Math.sin(((bLat - aLat) * RAD) / 2) ** 2 + Math.cos(aLat * RAD) * Math.cos(bLat * RAD) * Math.sin(((bLon - aLon) * RAD) / 2) ** 2;
  return 2 * 6371008.8 * Math.asin(Math.min(1, Math.sqrt(x)));
}
const mPerDegLat = 111320;
const mPerDegLon = (lat) => 111320 * Math.max(0.01, Math.cos(lat * RAD));
/** A circle as a closed ring of [lon, lat]. */
function ring(lat, lon, r, n = 96) {
  const dy = r / mPerDegLat, dx = r / mPerDegLon(lat);
  const out = [];
  for (let i = 0; i <= n; i++) { const a = (i / n) * 2 * Math.PI; out.push([lon + dx * Math.cos(a), lat + dy * Math.sin(a)]); }
  return out;
}
const circleBounds = (lat, lon, r) => boundsOf([[lon - r / mPerDegLon(lat), lat - r / mPerDegLat], [lon + r / mPerDegLon(lat), lat + r / mPerDegLat]]);
const circleFeature = (p, extra = {}) => ({ type: 'Feature', properties: { id: p.id, kind: p.kind, ...extra }, geometry: { type: 'Polygon', coordinates: [ring(p.lat, p.lon, p.radius_m)] } });

/* ------------------------------------------------------------------ words for prices and effects */

const priceEnergy = (v, cur) => format({ amount: v, currency: cur }, 'price_energy');
const priceTime = (v, cur) => format({ amount: v, currency: cur }, 'price_time');
function daysLabel(days) {
  if (!days || days === 'all') return 'Every day';
  const hit = DAY_CHOICES.find(([v]) => v === days);
  if (hit) return hit[1].replace(/ only$/, '');
  return String(days).split(',').map((part) => part.trim().split('-').map((d) => DAY_NAME[d] || d).join('–')).join(', ');
}
/** One line for a place's price: '$0.31/kWh · 2 time-of-use rates · $1.00 a session', or null. */
function priceParts(price) {
  if (!price) return [];
  const c = price.currency;
  const out = [];
  if (price.per_kwh != null) out.push(priceEnergy(price.per_kwh, c));
  if (price.tiers?.steps?.length) out.push(TI.tiersSummary(price.tiers, c));
  if (price.per_minute != null) out.push(priceTime(price.per_minute, c));
  const n = (price.rates || []).length;
  if (n) out.push(`${n} time-of-use ${n === 1 ? 'rate' : 'rates'}`);
  if (price.session_fee != null) out.push(`${formatMoney(price.session_fee, c)} a session`);
  return out;
}
const plural = (n, one, many) => `${formatNumber(n)} ${n === 1 ? one : many}`;

/** The counts of a preview or a save, as a short list of figures. */
function effectFigures(e, { deleting = false } = {}) {
  if (!e) return note('What this would change could not be worked out just now.');
  return h('div', { class: 'pl-effect-figs' },
    deleting ? null : metric('Drives gain the name', e.drives_named, 'count', { size: 'sm', explain: 'a start or end inside the circle' }),
    metric(deleting ? 'Drives lose the name' : 'Drives lose a name', e.drives_unnamed, 'count', { size: 'sm', explain: deleting ? 'they show their street address again' : 'now outside, or a nearer centre wins' }),
    deleting ? null : metric('Charges gain the name', e.charges_named, 'count', { size: 'sm', explain: 'and this place’s price for new costs' }),
    metric(deleting ? 'Charges lose the name' : 'Charges lose a name', e.charges_unnamed ?? 0, 'count', { size: 'sm', explain: 'their cost stays as it is' }),
    deleting ? null : metric('Past charges a reprice would change', e.charges_repriced, 'count', { size: 'sm', explain: 'only if you reprice them; nothing changes by itself' }));
}
const CHANGE_WORD = { named: 'gains the name', unnamed: 'loses the name', repriced: 'would cost differently' };
/** A few of the drives and charges a change touches, each a link with its date, car and change (example_rows);
 *  bare ids only when the server sent no example_rows. done: the change was made (a reprice that ran). */
function examplesLine(ctx, e, cars = [], { done = false } = {}) {
  const rows = Array.isArray(e?.example_rows) ? e.example_rows.slice(0, 6) : [];
  const byId = Object.fromEntries(cars.map((c) => [c.id, c]));
  if (rows.length) {
    return h('div', { class: 'pl-examples' },
      h('p', { class: 'faint' }, done ? 'For example:' : 'For example (open one to see it as it is now):'),
      h('ul', { class: 'pl-example-list' }, rows.map((x) => {
        const c = byId[x.car];
        const isDrive = x.kind === 'drive';
        const href = isDrive ? state.buildUrl('drive', { car: x.car, drive: x.id }) : state.buildUrl('charge', { car: x.car, charge: x.id });
        const when = x.start_ms != null ? dateTime(x.start_ms, c?.time_zone || ctx.tz) : 'time not known';
        const change = done && x.change === 'repriced' ? 'repriced' : CHANGE_WORD[x.change] || x.change;
        return h('li', null,
          icon(isDrive ? 'route' : 'bolt', 'ico--sm'),
          h('a', { class: 'link', href }, `${isDrive ? 'Drive' : 'Charge'} ${when}`),
          h('span', { class: 'faint' }, ` · ${c?.name || x.car} · ${change}`));
      })));
  }
  const ids = (e?.examples || []).slice(0, 5);
  if (!ids.length) return null;
  return h('p', { class: 'pl-examples' }, h('span', { class: 'faint' }, 'Example drive and charge ids: '), ids.map((x, i) => [i ? ', ' : '', h('span', { class: 'mono' }, x)]));
}

/* ------------------------------------------------------------------ the page */

export async function mount(ctx) {
  const q = ctx.query || {};
  if (q.place || q.new) return mountEditor(ctx, q);
  return mountList(ctx, q);
}

/* ================================================================== the list */

async function mountList(ctx, q) {
  const admin = ctx.admin;
  const f = { search: q.search || '', kind: KIND_LABEL[q.kind] ? q.kind : '' };
  ctx.setCrumbs([{ label: 'All cars', href: '/' }, { label: 'Places' }]);
  ctx.setTitle('Places');

  const [cars, sugg] = await Promise.all([
    ctx.api.list('cars', { sort: 'name' }, { max: 500 }).all().catch(() => []),
    ctx.api.get('places/suggestions', { limit: 8 }).catch(() => null),
  ]);
  if (!ctx.alive()) return;
  const carName = Object.fromEntries(cars.map((c) => [c.id, c.name]));
  let byCar = await drivesByCar(ctx, cars);
  if (!ctx.alive()) return;

  const countEl = h('span', null, 'Saved places');
  const newBtn = admin
    ? h('a', { class: 'btn btn--primary', href: newUrl(), cover: ['web.geofences.list.create', ...EDITOR_ITEMS] }, icon('plus', 'ico--sm'), 'New place')
    : h('button', { class: 'btn', type: 'button', disabled: true, title: 'Only an admin can add, change or delete places.', cover: ['web.geofences.list.create', ...EDITOR_ITEMS] }, icon('lock', 'ico--sm'), 'New place');
  const trail = h('span', { class: 'pl-trail', cover: 'web.geofences.list.breadcrumb' }, h('a', { href: '/' }, 'All cars'), h('span', { class: 'faint', 'aria-hidden': 'true' }, ' / '), 'Places');

  const head = pageHead({
    eyebrow: trail, title: 'Places',
    sub: [countEl, 'Matched live: a change renames history at once', admin ? null : 'View only: an admin makes changes'],
    actions: [newBtn],
  });

  const intro = h('section', { class: 'panel panel-pad pl-intro', 'aria-label': 'What places do', cover: 'faq.geofence-purpose' },
    h('div', { class: 'pl-intro-item' }, icon('pin'), h('div', null, h('h3', null, 'Your own names'),
      h('p', null, 'A street address can be wrong, or change from one parking spot to the next. A place gives a spot one name of your choosing. Every drive, charge and stop inside its circle shows that name, including history recorded before the place existed.'))),
    h('div', { class: 'pl-intro-item' }, icon('bolt'), h('div', null, h('h3', null, 'Charging prices'),
      h('p', null, 'A place can have a price per kWh or per minute, time-of-use rates and a session fee. A charge that ends there gets its cost from that price by itself.'))),
    h('div', { class: 'pl-intro-item' }, icon('info'), h('div', null, h('h3', null, 'One place per point'),
      h('p', null, 'Where circles overlap, the place with the nearest centre wins. The editor shows which places overlap and what a change would rename before you save.'))));

  // a stale or mistyped editor link lands here with ?missing=<id>
  const missingBox = h('div', { class: 'pl-banner', role: 'status', hidden: !q.missing, cover: 'web.geofences.form.not-found' },
    icon('warn'),
    h('p', null, q.missing ? `There is no place “${q.missing}”. It was deleted, or the link is old or mistyped. Every place there is is listed below.` : 'That place does not exist any more.'),
    h('button', { class: 'btn btn--ghost btn--sm', type: 'button', onclick: () => { missingBox.hidden = true; ctx.setQuery({ missing: null }); } }, icon('x', 'ico--sm'), 'Dismiss'));

  // the empty state for a server with no places at all (kept in the page, shown when that is so)
  const emptyBox = h('section', { class: 'panel pl-empty', hidden: true, cover: 'web.geofences.list.empty-state' },
    emptyState({
      icon: 'pin', title: 'No places yet',
      text: 'Without places, drives and charges show street addresses and charges have no price. Add your home first: start from where a car is now, or from a suggestion below (where the cars park most).',
      action: admin ? h('div', { class: 'row' }, h('a', { class: 'btn btn--primary', href: newUrl() }, icon('plus', 'ico--sm'), 'Start from where a car is now'))
        : h('p', { class: 'faint' }, 'An admin can add places.'),
    }));

  /* ---------- filters */
  const searchIn = h('input', { class: 'input', type: 'search', value: f.search, placeholder: 'Name or address', 'aria-label': 'Search places by name or address' });
  const kindSeg = h('span', { class: 'seg', role: 'group', 'aria-label': 'Kind of place' },
    [['', 'All kinds'], ...KINDS.map(([k, l]) => [k, l])].map(([v, l]) => h('button', {
      class: 'seg-btn', type: 'button', 'aria-pressed': String(f.kind === v),
      onclick: (e) => { f.kind = v; for (const b of kindSeg.children) b.setAttribute('aria-pressed', String(b === e.currentTarget)); apply(); },
    }, l)));
  const filters = h('div', { class: 'filter-bar' }, h('label', { class: 'field field--search' }, icon('search'), searchIn), kindSeg);
  let timer = 0;
  searchIn.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(() => { f.search = searchIn.value.trim(); apply(); }, 300); });
  ctx.onCleanup(() => clearTimeout(timer));
  const params = () => ({ search: f.search || null, kind: f.kind || null });
  function apply() {
    ctx.setQuery({ search: f.search || null, kind: f.kind || null });
    table.setParams(params());
    loadMap();
  }

  /* ---------- the table */
  const priceCell = (p) => {
    const parts = priceParts(p.price);
    return parts.length ? h('span', { class: 'cell-text' }, h('span', null, parts[0]), parts.length > 1 ? h('span', { class: 'sub' }, parts.slice(1).join(' · ')) : null)
      : h('span', { class: 'faint' }, 'No price');
  };
  // charges (every car), drives (by the cars that drove there) and stops, all time; each count opens its list
  const historyCell = (p) => {
    const nc = p.counts?.charges || 0, nd = p.counts?.drives || 0, nv = p.counts?.visits || 0;
    const per = byCar.get(p.id) || [];
    const charges = nc ? h('a', { class: 'link', href: chargesHref(p.id), title: 'Every charge here, every car, all time' }, plural(nc, 'charge', 'charges'))
      : h('span', { class: 'faint' }, 'No charges');
    let drives;
    if (!nd) drives = h('span', { class: 'faint' }, 'No drives');
    else if (per.length === 1) drives = h('a', { class: 'link', href: drivesHref(per[0].car, p.id), title: `${per[0].name}’s drives that start or end here, all time` }, plural(nd, 'drive', 'drives'), h('span', { class: 'faint' }, ` (${per[0].name})`));
    else if (per.length) {
      drives = h('span', null, `${plural(nd, 'drive', 'drives')}: `, per.slice(0, 3).map((x, i) => [i ? h('span', { class: 'faint' }, ', ') : null,
        h('a', { class: 'link', href: drivesHref(x.car, p.id), title: `${x.name}’s drives that start or end here, all time` }, `${x.name} ${formatNumber(x.drives)}`)]),
        per.length > 3 ? h('span', { class: 'faint' }, ` +${per.length - 3} more in the editor`) : null);
    } else drives = h('span', null, plural(nd, 'drive', 'drives'));
    return h('span', { class: 'pl-hist' }, charges, drives, h('span', { class: nv ? 'sub' : 'sub faint' }, nv ? plural(nv, 'stop', 'stops') : 'No stops'));
  };
  const table = dataTable(ctx, {
    title: 'Saved places', icon: 'pin', path: 'places', params: params(), limit: 50, csvName: 'places',
    sort: { key: 'name', dir: 'asc' },
    note: 'Sorted by name, A to Z, until you pick another column. Counts cover the cars your account sees. Open a place to edit it.',
    noteCover: 'web.geofences.list.order',
    columns: [
      { key: 'name', label: 'Name', sort: 'name', firstDir: 'asc', cover: 'web.geofences.list.col-name', cls: 'col-name',
        title: 'The name, its kind and the street address of the centre',
        render: (p) => h('span', { class: 'cell-text' }, h('a', { class: 'pl-name', href: editUrl(p.id) }, p.name),
          h('span', { class: 'sub' }, h('span', { class: ['pl-kind', `pl-kind--${p.kind}`] }, icon(KIND_ICON[p.kind] || 'pin', 'ico--sm'), kindLabel(p.kind)),
            (p.address?.short || p.address?.locality) ? ` · ${p.address.short || p.address.locality}` : null)) },
      { key: 'circle', label: 'Circle', cover: ['web.geofences.list.col-position', 'web.geofences.list.col-radius'],
        title: `The radius (${unitLabel('length')}) and the centre (latitude, longitude)`,
        render: (p) => h('span', { class: 'cell-text' }, h('span', null, `${format(p.radius_m, 'length')} radius`), h('span', { class: 'sub mono' }, posText(p.lat, p.lon))) },
      { key: 'history', label: 'History, all time', sort: 'visits', firstDir: 'desc', cls: 'col-hist',
        title: 'Charges (every car), drives that start or end here (by car) and stops inside the circle, all time. Sorting puts the most stops first.',
        render: historyCell },
      { key: 'last', label: 'Last visit', sort: 'last_visit', firstDir: 'desc', render: (p) => (p.last_visit_ms ? h('span', { class: 'cell-text' }, h('span', null, ago(p.last_visit_ms)), h('span', { class: 'sub' }, dateTime(p.last_visit_ms, p.time_zone || ctx.tz))) : h('span', { class: 'faint' }, 'Not visited')) },
      { key: 'price', label: 'Price', title: 'The charging price here; a charge that ends here gets its cost from it', render: priceCell },
      { key: 'actions', label: 'Change', cls: 'col-actions pl-sticky', cover: ['web.geofences.list.edit', 'web.geofences.list.delete'],
        render: (p) => h('span', { class: 'pl-actions' },
          h('a', { class: 'btn btn--sm', href: editUrl(p.id), cover: 'web.geofences.list.edit' }, admin ? 'Edit' : 'View'),
          admin ? h('button', { class: 'btn btn--sm btn--ghost btn--danger', type: 'button', cover: ['web.geofences.list.delete', 'web.geofences.form.relabel-history'], 'aria-label': `Delete ${p.name}`, onclick: () => removePlace(ctx, p, () => reloadAll(), cars) }, icon('x', 'ico--sm'), 'Delete') : null) },
    ],
    rowHref: (p) => editUrl(p.id),
    empty: () => (f.search || f.kind
      ? emptyState({ icon: 'search', title: 'No place matches', text: `No saved place${f.kind ? ` of the kind ${kindLabel(f.kind)}` : ''}${f.search ? ` has “${f.search}” in its name or address` : ''}. Clear the search or pick All kinds.` })
      : emptyState({ icon: 'pin', title: 'No places yet', text: 'Add one with New place, or name a suggestion below.' })),
    onPage: (pg) => {
      if (pg.total != null) countEl.textContent = f.search || f.kind ? `${plural(pg.total, 'place matches', 'places match')}` : plural(pg.total, 'saved place', 'saved places');
      emptyBox.hidden = !(pg.total === 0 && !f.search && !f.kind);
    },
  });

  /* ---------- the map of every circle (follows the filters) */
  const mapEl = h('div', { class: 'map' });
  // a phone has no pointer to hover with: a tap opens the circle's place
  const circleHint = () => (document.documentElement.dataset.phone ? 'Tap a circle to edit its place.' : 'Point at a circle for its name; click it to edit.');
  const mapCard = h('div', { class: 'glass-card pl-map-card' }, h('strong', null, 'Every circle'), h('span', { class: 'faint' }, circleHint()));
  const mapPanel = h('section', { class: 'panel map-panel pl-map-panel', 'aria-label': 'Map of the places' }, mapEl, h('div', { class: 'map-overlay map-overlay--tl' }, mapCard));
  let m = null, mapRows = [], mapBusy = null;
  async function loadMap() {
    let rows;
    try { rows = await ctx.api.list('places', params(), { max: 1000 }).all(); } catch { rows = []; }
    if (!ctx.alive()) return;
    mapRows = rows;
    if (!m) {
      if (mapBusy) return;
      mapBusy = (async () => {
        const first = rows[0];
        try { m = await createMap(mapEl, ctx, { center: first ? [first.lon, first.lat] : [0, 20], zoom: first ? 11 : 1.5 }); } catch { replace(mapEl, mapFallback()); return; }
        if (!m) return;
        await m.ready;
        if (!ctx.alive()) return;
        m.map.addSource('places', { type: 'geojson', data: { type: 'FeatureCollection', features: [] } });
        m.map.addSource('centres', { type: 'geojson', data: { type: 'FeatureCollection', features: [] } });
        const kindColor = ['match', ['get', 'kind'], ...KINDS.flatMap(([k]) => [k, cssVar(KIND_COLOR[k])]), cssVar('--text-2')];
        m.addLayer({ id: 'places-fill', type: 'fill', source: 'places', paint: { 'fill-color': kindColor, 'fill-opacity': 0.18 } });
        m.addLayer({ id: 'places-line', type: 'line', source: 'places', paint: { 'line-color': kindColor, 'line-opacity': 0.9 } }, { 'line-width': () => rpx(1.5) });
        m.addLayer({ id: 'places-dot', type: 'circle', source: 'centres', paint: { 'circle-color': kindColor, 'circle-stroke-color': cssVar('--map-casing') } },
          { 'circle-radius': () => rpx(4), 'circle-stroke-width': () => rpx(1.5) });
        const pick = (e) => e.features?.[0]?.properties?.id;
        for (const layer of ['places-fill', 'places-dot']) {
          m.map.on('click', layer, (e) => { const pid = pick(e); if (pid) ctx.navigate(editUrl(pid)); });
          m.map.on('mousemove', layer, (e) => {
            m.map.getCanvas().style.cursor = 'pointer';
            const p = mapRows.find((x) => x.id === pick(e));
            if (p) replace(mapCard, h('strong', null, p.name), h('span', { class: 'faint' }, `${kindLabel(p.kind)} · ${format(p.radius_m, 'length')} radius · click to edit`));
          });
          m.map.on('mouseleave', layer, () => { m.map.getCanvas().style.cursor = ''; paintCard(); });
        }
        paintMap();
      })();
      await mapBusy;
      mapBusy = null;
      return;
    }
    paintMap();
  }
  function paintCard() {
    replace(mapCard, h('strong', null, mapRows.length ? plural(mapRows.length, 'circle', 'circles') : 'No circle to show'),
      h('span', { class: 'faint' }, mapRows.length ? circleHint() : 'Nothing matches the filters.'));
  }
  function paintMap() {
    if (!m || !m.map.getSource('places')) return;
    m.map.getSource('places').setData({ type: 'FeatureCollection', features: mapRows.map((p) => circleFeature(p)) });
    m.map.getSource('centres').setData({ type: 'FeatureCollection', features: mapRows.map((p) => ({ type: 'Feature', properties: { id: p.id, kind: p.kind }, geometry: { type: 'Point', coordinates: [p.lon, p.lat] } })) });
    paintCard();
    if (mapRows.length) {
      const b = boundsOf(mapRows.flatMap((p) => circleBounds(p.lat, p.lon, p.radius_m)));
      m.frame(() => m.map.fitBounds(b, { padding: { top: rpx(90), left: rpx(40), right: rpx(60), bottom: rpx(40) }, maxZoom: 16, duration: 0 }));
    }
  }

  /* ---------- suggestions */
  const sRows = sugg?.items || [];
  const suggTable = dataTable(ctx, {
    title: 'Suggestions', icon: 'park', rows: sRows, csvName: 'place-suggestions', cls: 'pl-sugg',
    note: 'Spots with no saved place where the cars stopped most. Naming one renames those stops at once.',
    sort: { key: 'visits', dir: 'desc' },
    columns: [
      { key: 'label', label: 'Where', sort: true, value: (s) => s.label || '', render: (s) => h('span', { class: 'cell-text' }, h('span', null, s.label || posText(s.lat, s.lon)), h('span', { class: 'sub' }, [s.address?.locality, s.address?.region].filter(Boolean).join(', '))),
        csv: (s) => s.label },
      { key: 'position', label: 'Centre (lat, lon)', render: (s) => h('span', { class: 'mono nowrap' }, posText(s.lat, s.lon)), csv: (s) => `${s.lat}, ${s.lon}` },
      { key: 'visits', label: 'Stops', num: true, sort: true, render: (s) => formatNumber(s.visits) },
      { key: 'dwell_ms', label: 'Time parked', num: true, sort: true, render: (s) => dur(s.dwell_ms), csv: (s) => Math.round((s.dwell_ms || 0) / 60000) + ' min' },
      { key: 'cars', label: 'Cars', render: (s) => (s.cars || []).map((c) => carName[c] || c).join(', '), csv: (s) => (s.cars || []).map((c) => carName[c] || c).join('; ') },
      { key: 'name', label: '', csv: false, cover: 'web.geofences.form.entry-from-dashboards',
        render: (s) => (admin ? h('a', { class: 'btn btn--sm', href: newUrl({ lat: s.lat.toFixed(5), lon: s.lon.toFixed(5), radius: s.radius_m || R_NEW, origin: 'suggestion', name: suggName(s), kind: KIND_LABEL[s.kind] ? s.kind : null }) }, icon('plus', 'ico--sm'), 'Name it') : h('span', { class: 'faint' }, 'Admin only')) },
    ],
    empty: () => emptyState({ icon: 'check', title: 'No suggestions', text: 'Every spot where the cars stop often already lies inside a saved place.' }),
  });

  replace(ctx.view, h('div', { class: 'places-page' },
    head, missingBox, intro, filters, emptyBox, table.el,
    h('div', { class: 'pl-lower' }, mapPanel, suggTable.el)));

  const reloadAll = () => { table.reload(params()); loadMap(); };
  const refreshCounts = async () => { const b = await drivesByCar(ctx, cars); if (ctx.alive()) byCar = b; };
  let liveT = 0;
  ctx.events('rows', () => { clearTimeout(liveT); liveT = setTimeout(async () => {
    await refreshCounts();
    if (!ctx.alive()) return;
    table.reload(params());
    const s = await ctx.api.get('places/suggestions', { limit: 8 }).catch(() => null);
    if (s && ctx.alive()) suggTable.setRows(s.items || []);
  }, 1000); });
  ctx.onCleanup(() => clearTimeout(liveT));

  await table.ready;
  if (!ctx.alive()) return;
  ctx.ready();
  loadMap();
}

// what opens the editor carries the editor's items
const EDITOR_ITEMS = [
  'web.geofences.form.breadcrumb', 'web.geofences.form.defaults', 'web.geofences.form.position-map', 'web.geofences.form.match-rule',
  'web.geofences.form.name', 'web.geofences.form.validation', 'web.geofences.form.back', 'web.geofences.form.save',
  'web.geofences.form.billing-type', 'web.geofences.form.cost-per-unit', 'web.geofences.form.session-fee', 'web.geofences.form.negative-price',
  'web.geofences.form.auto-cost', 'projects.agile', 'web.geofences.form.relabel-history', 'web.geofences.form.retro-costs',
  'web.geofences.form.entry-from-dashboards',
];

/** Delete a place: first say what loses the name, then delete with the version that was shown. */
async function removePlace(ctx, p, done, cars = []) {
  let eff = null;
  try { eff = await ctx.api.post('places/preview', { place: p.id, delete: true }); } catch { eff = null; }
  if (!ctx.alive()) return false;
  const ok = await openDialog({
    title: `Delete ${p.name}?`,
    body: [
      h('p', null, `Drives, charges and stops inside the circle go back to their street address (or to another place whose circle holds them). Costs that charges already have stay as they are.`),
      effectFigures(eff, { deleting: true }), examplesLine(ctx, eff, cars),
      p.price ? h('p', { class: 'note' }, 'Its charging price goes too: charges that end here later get no cost from it.') : null,
    ],
    actions: [{ label: 'Keep it', value: false }, { label: 'Delete place', value: true, kind: 'danger' }],
  });
  if (ok !== true) return false;
  try {
    const r = await ctx.api.del(`places/${enc(p.id)}`, { params: { version: p.version } });
    flash(`Deleted ${p.name}. ${plural(r.effect?.drives_unnamed || 0, 'drive', 'drives')} lost the name.`);
    done?.();
    return true;
  } catch (e) {
    if (e.code === 'stale_version') flash(`${p.name} was changed by someone else meanwhile, so nothing was deleted. Look at it again first.`, { kind: 'error' });
    else if (e.status === 404) flash(`${p.name} was already deleted.`, { kind: 'info' });
    else flash(e.message || 'The place could not be deleted.', { kind: 'error' });
    done?.();
    return false;
  }
}

/* ================================================================== the editor */

async function mountEditor(ctx, q) {
  const admin = ctx.admin;
  const isNew = !q.place;
  let cur = null;
  const [placesAll, cars] = await Promise.all([
    ctx.api.list('places', { sort: 'name' }, { max: 1000 }).all().catch(() => []),
    ctx.api.list('cars', { sort: 'last_activity', dir: 'desc' }, { max: 200 }).all().catch(() => []),
  ]);
  if (!ctx.alive()) return;
  if (!isNew) {
    try { cur = await ctx.api.get(`places/${enc(q.place)}`); } catch (e) {
      if (!ctx.alive()) return;
      if (e.status === 404) { state.navigate(listUrl({ missing: q.place }), { replace: true }); return; }
      ctx.fail(e, { retry: () => ctx.reload() });
      return;
    }
    if (!ctx.alive()) return;
  }

  /* ---------- where a new place starts */
  let startNote;
  let start = null;
  const qLat = Number(q.lat), qLon = Number(q.lon);
  if (cur) start = { lat: cur.lat, lon: cur.lon, radius_m: cur.radius_m };
  else if (q.lat != null && q.lon != null && Number.isFinite(qLat) && Number.isFinite(qLon) && Math.abs(qLat) <= 90 && Math.abs(qLon) <= 180) {
    const r = Number(q.radius);
    start = { lat: qLat, lon: qLon, radius_m: Number.isFinite(r) && r >= R_MIN && r <= R_MAX ? Math.round(r) : R_NEW };
    startNote = q.origin === 'suggestion'
      ? `Starts at a suggested spot: the middle of the stops there, with a ${format(start.radius_m, 'length')} circle${q.name ? ' and its address as a first name' : ''}. Change anything before saving.`
      : 'Starts at the spot you came from: the drive, charge or stop that had no place yet.';
  } else {
    // the newest recorded position of any car this account sees
    for (const c of cars.slice(0, 5)) {
      let now = null;
      try { now = await ctx.api.get(`cars/${enc(c.id)}/now`); } catch { now = null; }
      if (!ctx.alive()) return;
      const la = now?.location?.lat?.value, lo = now?.location?.lon?.value;
      if (la != null && lo != null) {
        start = { lat: la, lon: lo, radius_m: R_NEW };
        startNote = `Starts at ${c.name}’s last recorded position (${dateTime(now.location.lat.at_ms, c.time_zone || ctx.tz)}), with a ${format(R_NEW, 'length')} circle. Move it on the map.`;
        break;
      }
    }
    if (!start) {
      const p0 = placesAll[0];
      start = p0 ? { lat: p0.lat, lon: p0.lon, radius_m: R_NEW } : { lat: 0, lon: 0, radius_m: R_NEW };
      startNote = 'No car has recorded a position yet, so the circle starts ' + (p0 ? `at ${p0.name}.` : 'at 0, 0.') + ' Click the map where the place is.';
    }
  }
  const usedCurrency = (() => { const n = {}; for (const p of placesAll) if (p.price?.currency) n[p.price.currency] = (n[p.price.currency] || 0) + 1; return Object.entries(n).sort((a, b) => b[1] - a[1])[0]?.[0] || 'USD'; })();

  /* ---------- the draft */
  const d = {
    name: cur?.name ?? (typeof q.name === 'string' ? q.name.slice(0, 64) : ''), kind: cur?.kind ?? (q.kind && KIND_LABEL[q.kind] ? q.kind : 'other'),
    lat: start.lat, lon: start.lon, radius_m: start.radius_m, time_zone: cur?.time_zone ?? '',
    priced: !!cur?.price,
    currency: cur?.price?.currency || usedCurrency,
    per_kwh: cur?.price?.per_kwh ?? null, per_minute: cur?.price?.per_minute ?? null, session_fee: cur?.price?.session_fee ?? null,
    rates: (cur?.price?.rates || []).map((r) => ({ ...r })),
    tiers: TI.copyTiers(cur?.price?.tiers) || TI.newTiers(cur?.price?.per_kwh ?? null),
  };
  d.mode = cur?.price?.tiers ? 'tiers' : d.per_minute != null && d.per_kwh != null ? 'both' : d.per_minute != null || (d.rates.length && d.rates.every((r) => r.per_minute != null && r.per_kwh == null)) ? 'minute' : 'kwh';
  d.radiusTyped = null;   // the radius in the person's units as typed; null when it came from the map or the server
  let lastR = d.radius_m; // the last valid radius, drawn while the field holds one that is not
  const others = placesAll.filter((p) => p.id !== cur?.id);
  let dirty = false;
  const lock = !admin;

  /* ---------- head, trail, back */
  const nameText = () => d.name.trim() || (isNew ? 'New place' : 'Unnamed place');
  const crumbName = h('span', { class: 'pl-trail-name' }, nameText());
  const trail = h('span', { class: 'pl-trail', cover: 'web.geofences.form.breadcrumb' },
    h('a', { href: '/' }, 'All cars'), h('span', { class: 'faint', 'aria-hidden': 'true' }, ' / '),
    h('a', { href: listUrl() }, 'Places'), h('span', { class: 'faint', 'aria-hidden': 'true' }, ' / '), crumbName);
  const titleEl = h('span', null, nameText());
  const setNames = () => {
    crumbName.textContent = nameText(); titleEl.textContent = nameText();
    ctx.setCrumbs([{ label: 'All cars', href: '/' }, { label: 'Places', href: listUrl() }, { label: nameText() }]);
    ctx.setTitle(isNew ? 'New place' : nameText(), 'Places');
  };
  async function leave(e) {
    e?.preventDefault();
    if (dirty && !(await confirmDialog('Your changes to this place are not saved. Leave and lose them?', { title: 'Leave without saving?', ok: 'Leave', danger: true }))) return;
    dirty = false;
    ctx.navigate(listUrl());
  }
  const backLink = h('a', { class: 'back', href: listUrl(), onclick: leave, cover: 'web.geofences.form.back' }, icon('arrow-l', 'ico--sm'), 'All places');
  ctx.on(window, 'beforeunload', (e) => { if (dirty) { e.preventDefault(); e.returnValue = ''; } });

  const subs = cur
    ? [kindLabel(cur.kind), plural(cur.counts?.drives || 0, 'drive', 'drives'), plural(cur.counts?.charges || 0, 'charge', 'charges'), plural(cur.counts?.visits || 0, 'stop', 'stops'), cur.last_visit_ms ? `last visit ${ago(cur.last_visit_ms)}` : 'not visited yet']
    : ['Not saved yet'];
  const head = h('div', { class: 'pl-head' }, backLink, pageHead({ eyebrow: trail, title: titleEl, sub: subs }));

  /* ---------- fields */
  const L = unitLabel('length');
  const toL = (m) => Math.round(convert(m, 'length'));
  const fName = field({ label: 'Name', name: 'name', value: d.name, required: true, attrs: { maxlength: '64', autocomplete: 'off', placeholder: 'For example Home or Gym car park', disabled: lock },
    hint: 'Shown on every drive, charge and stop inside the circle instead of the street address.' });
  const fKind = field({ label: 'Kind', name: 'kind', type: 'select', value: d.kind, options: KINDS.map(([k, l]) => [k, l]), attrs: { disabled: lock },
    hint: 'Used to filter lists (charges at home, at work, at public chargers).' });
  if (lock) fKind.input.disabled = true;
  const fRadius = field({ label: `Radius (${L})`, name: 'radius_m', type: 'number', value: toL(d.radius_m), required: true,
    attrs: { min: String(Math.ceil(convert(R_MIN, 'length'))), max: String(Math.floor(convert(R_MAX, 'length'))), step: '1', disabled: lock },
    hint: `From ${format(R_MIN, 'length')} to ${format(R_MAX, 'length')}. Big enough for the whole car park, small enough not to swallow the neighbours.` });
  const fLat = field({ label: 'Latitude', name: 'lat', type: 'number', value: d.lat.toFixed(6), required: true, attrs: { min: '-90', max: '90', step: 'any', disabled: lock } });
  const fLon = field({ label: 'Longitude', name: 'lon', type: 'number', value: d.lon.toFixed(6), required: true, attrs: { min: '-180', max: '180', step: 'any', disabled: lock } });
  let zones = [];
  try { zones = Intl.supportedValuesOf('timeZone'); } catch { zones = []; }
  if (d.time_zone && !zones.includes(d.time_zone)) zones = [d.time_zone, ...zones];
  const fZone = field({ label: 'Time zone', name: 'time_zone', type: 'select', value: d.time_zone,
    options: [...(isNew ? [['', 'Automatic: the nearest saved place’s zone']] : []), ...zones.map((z) => [z, z.replace(/_/g, ' ')])],
    hint: 'Time-of-use rates follow the clock of this zone, daylight saving included.' });
  if (lock) fZone.input.disabled = true;
  const addrLine = h('p', { class: 'pl-addr' }, icon('pin', 'ico--sm'), h('span', null, cur?.address?.short || 'Address of the centre: looking it up…'));

  const detailsFields = [fName, fKind, fRadius, fLat, fLon, fZone];

  /* ---------- prices */
  const fCurrency = field({ label: 'Currency', name: 'currency', type: 'select', value: d.currency, options: CURRENCIES.map((c) => [c, c]), path: 'price.currency' });
  const fPerKwh = field({ label: 'Price per kWh', name: 'per_kwh', type: 'number', value: d.per_kwh, path: 'price.per_kwh', attrs: { step: '0.0001', placeholder: 'e.g. 0.31' },
    hint: 'Per kWh of energy the charge took: the grid meter on AC, what the fast charger delivered on DC.' });
  const fPerMin = field({ label: 'Price per minute', name: 'per_minute', type: 'number', value: d.per_minute, path: 'price.per_minute', attrs: { step: '0.0001', placeholder: 'e.g. 0.20' },
    hint: 'Per minute of charging (plugged in but not charging does not count).' });
  const fFee = field({ label: 'Session fee', name: 'session_fee', type: 'number', value: d.session_fee, path: 'price.session_fee', attrs: { min: '0', max: '1000', step: '0.01', placeholder: '0.00' },
    hint: 'Added once to every charge here. Zero or more.' });
  const priceFields = [fCurrency, fPerKwh, fPerMin, fFee];
  if (lock) for (const x of priceFields) x.input.disabled = true;

  const pricedSeg = h('span', { class: 'seg', role: 'group', 'aria-label': 'Does charging here cost money' },
    [[false, 'Free or not tracked'], [true, 'Charging here has a price']].map(([v, l]) => h('button', {
      class: 'seg-btn', type: 'button', 'aria-pressed': String(d.priced === v), disabled: lock,
      onclick: () => { d.priced = v; for (const b of pricedSeg.children) b.setAttribute('aria-pressed', String(b.textContent === l)); paintPrice(); changed(); },
    }, l)));
  const modeSeg = h('span', { class: 'seg', role: 'group', 'aria-label': 'Priced by', cover: 'web.geofences.form.billing-type' },
    [['kwh', 'Energy (per kWh)'], ['minute', 'Time (per minute)'], ['both', 'Both'], ['tiers', 'Tiered (per kWh steps)']].map(([v, l]) => h('button', {
      class: 'seg-btn', type: 'button', 'aria-pressed': String(d.mode === v), disabled: lock, dataset: { v },
      onclick: () => { d.mode = v; for (const b of modeSeg.children) b.setAttribute('aria-pressed', String(b.dataset.v === v)); paintPrice(); changed(); },
    }, l)));
  const priceMsg = h('p', { class: 'form-message', role: 'alert', hidden: true });
  const ratesBox = h('div', { class: 'pl-rates' });
  const ratesErr = h('p', { class: 'form-error', hidden: true });
  const addRateBtn = h('button', { class: 'btn btn--sm', type: 'button', disabled: lock, onclick: () => {
    d.rates.push({ name: d.rates.length ? `Rate ${d.rates.length + 1}` : 'Off-peak', days: 'all', from: '00:00', to: '07:00', per_kwh: null, per_minute: null });
    paintRates(); changed();
    ratesBox.querySelector('.pl-rate:last-child input')?.focus();
  } }, icon('plus', 'ico--sm'), 'Add a time-of-use rate');
  /* tiered prices: steps by the kWh used in a billing cycle */
  const tiersBox = h('div', { class: 'pl-rates pl-tiers' });
  const tiersErr = h('p', { class: 'form-error', hidden: true });
  const addTierBtn = h('button', { class: 'btn btn--sm', type: 'button', disabled: lock, onclick: () => {
    const st = d.tiers.steps;
    if (st.length >= TI.MAX_TIERS) return;
    const top = Number(st[st.length - 1]?.from_kwh) || 0;
    st.push({ from_kwh: st.length === 1 ? 1000 : top + 1000, per_kwh: null });
    paintTiers(); paintRule(); changed();
    tiersBox.querySelector('.pl-tier:last-child input')?.focus();
  } }, icon('plus', 'ico--sm'), 'Add a step');
  const cycleSel = h('select', { class: 'select', 'aria-label': 'Billing cycle starts', disabled: lock, dataset: { path: 'price.tiers.cycle_day' } },
    TI.cycleChoices().map(([v, l]) => h('option', { value: v, selected: Number(v) === (d.tiers.cycle_day || 1) }, l)));
  cycleSel.addEventListener('change', () => { d.tiers.cycle_day = Number(cycleSel.value); paintRule(); changed(); });
  const baseIn = h('input', { class: 'input', type: 'number', min: '0', step: '1', value: d.tiers.base_kwh || '', placeholder: '0', 'aria-label': 'Household use per cycle (kWh)', disabled: lock, dataset: { path: 'price.tiers.base_kwh' } });
  baseIn.addEventListener('input', () => { d.tiers.base_kwh = baseIn.value === '' ? 0 : Number(baseIn.value); baseIn.removeAttribute('aria-invalid'); paintRule(); changed(); });
  const tiersWrap = h('div', { class: 'pl-rates-wrap pl-tiers-wrap' },
    h('div', { class: 'pl-sub-head' }, h('h3', null, 'Steps per billing cycle'), addTierBtn),
    h('p', { class: 'form-hint' }, 'Each step’s price applies from its kWh on, counted from the start of the billing cycle. A charge that crosses a step pays each part at its own price.'),
    tiersBox, tiersErr,
    h('div', { class: 'pl-row pl-row--price' },
      h('label', { class: 'pl-rate-cell' }, h('span', null, 'Billing cycle starts'), cycleSel),
      h('label', { class: 'pl-rate-cell' }, h('span', null, 'Household use per cycle (kWh)'), baseIn)),
    h('p', { class: 'form-hint' }, `Counted: this place’s charging, every car, in time order. ${TI.BASE_USE_NOTE}`));
  function paintTiers() {
    const st = d.tiers.steps;
    replace(tiersBox, st.map((s, i) => {
      const { from, to } = TI.stepRange(st, i);
      const num = (key, attrs, label) => {
        const el = h('input', { class: 'input', type: 'number', 'aria-label': `${label}, step ${i + 1}`, disabled: lock || attrs.readonly, ...attrs, dataset: { path: `price.tiers.steps.${i}.${key}` } });
        el.addEventListener('input', () => { s[key] = el.value === '' ? null : Number(el.value); el.removeAttribute('aria-invalid'); paintRule(); changed(); if (key === 'from_kwh') paintTierLabels(); });
        return el;
      };
      return h('div', { class: 'pl-rate pl-tier' },
        h('span', { class: 'pl-rate-cell pl-tier-range' }, h('span', null, `Step ${i + 1}`), h('strong', { class: 'pl-tier-words' }, TI.stepLabel(from, to))),
        h('label', { class: 'pl-rate-cell pl-rate-num' }, h('span', null, 'From kWh'), num('from_kwh', { min: '0', step: '1', value: i === 0 ? 0 : s.from_kwh ?? '', readonly: i === 0 ? true : undefined }, 'From kWh')),
        h('label', { class: 'pl-rate-cell pl-rate-num' }, h('span', null, `${d.currency}/kWh`), num('per_kwh', { step: '0.0001', value: s.per_kwh ?? '', placeholder: 'e.g. 0.16' }, 'Price per kWh')),
        lock || st.length <= 1 ? h('span', { class: 'pl-rate-x' }) : h('button', { class: 'btn btn--ghost btn--icon btn--sm pl-rate-x', type: 'button', 'aria-label': `Remove step ${i + 1}`, onclick: () => { st.splice(i, 1); if (st[0]) st[0].from_kwh = 0; paintTiers(); paintRule(); changed(); } }, icon('x')));
    }));
    addTierBtn.disabled = lock || st.length >= TI.MAX_TIERS;
  }
  function paintTierLabels() {
    const st = d.tiers.steps;
    tiersBox.querySelectorAll('.pl-tier-words').forEach((el, i) => { const { from, to } = TI.stepRange(st, i); el.textContent = TI.stepLabel(from, to); });
  }
  const zoneWord = () => d.time_zone || 'the place’s own time zone';
  const ratesHint = h('p', { class: 'form-hint' });
  const costRule = h('div', { class: 'pl-rule', cover: 'web.geofences.form.auto-cost' });
  const negNote = h('p', { class: 'pl-hint-line', cover: 'web.geofences.form.negative-price' }, icon('info', 'ico--sm'),
    h('span', null, 'A price below zero is allowed: some tariffs pay you to charge at times of surplus. A cost then comes out negative, or below the session fee. The session fee itself cannot be below zero.'));
  const priceBody = h('div', { class: 'pl-price-body' });

  function paintRates() {
    const showK = d.mode !== 'minute', showM = d.mode !== 'kwh' && d.mode !== 'tiers';
    ratesHint.textContent = `Rates follow the clock in ${zoneWord()}, so daylight saving needs no change. Outside every rate ${d.mode === 'tiers' ? 'the steps apply; a rate’s kWh still count toward the steps' : 'the standard price above applies'}. A rate whose start is later than its end runs past midnight. Where rates overlap, the first in this list wins. A blank price in a rate uses the standard one.`;
    replace(ratesBox, d.rates.map((r, i) => {
      const inp = (key, attrs, label) => {
        const el = h('input', { class: 'input', 'aria-label': `${label}, rate ${i + 1}`, disabled: lock, ...attrs, dataset: { path: `price.rates.${i}.${key}` } });
        el.addEventListener('input', () => {
          const v = el.value;
          r[key] = attrs.type === 'number' ? (v === '' ? null : Number(v)) : v;
          el.removeAttribute('aria-invalid');
          changed();
        });
        return el;
      };
      const daySel = h('select', { class: 'select', 'aria-label': `Days, rate ${i + 1}`, disabled: lock, dataset: { path: `price.rates.${i}.days` } },
        [...DAY_CHOICES, ...(DAY_CHOICES.some(([v]) => v === r.days) ? [] : [[r.days, daysLabel(r.days)]])].map(([v, l]) => h('option', { value: v, selected: v === (r.days || 'all') }, l)));
      daySel.addEventListener('change', () => { r.days = daySel.value; changed(); });
      return h('div', { class: 'pl-rate' },
        h('label', { class: 'pl-rate-cell pl-rate-name' }, h('span', null, 'Name'), inp('name', { type: 'text', value: r.name || '', maxlength: '40' }, 'Name')),
        h('label', { class: 'pl-rate-cell pl-rate-days' }, h('span', null, 'Days'), daySel),
        h('label', { class: 'pl-rate-cell pl-rate-time' }, h('span', null, 'From'), inp('from', { type: 'text', value: r.from || '00:00', inputmode: 'numeric', placeholder: 'HH:MM', pattern: HM_RX.source.slice(1, -1) }, 'From')),
        h('label', { class: 'pl-rate-cell pl-rate-time' }, h('span', null, 'To'), inp('to', { type: 'text', value: r.to || '24:00', inputmode: 'numeric', placeholder: 'HH:MM', pattern: HM_RX.source.slice(1, -1) }, 'To')),
        showK ? h('label', { class: 'pl-rate-cell pl-rate-num' }, h('span', null, `${d.currency}/kWh`), inp('per_kwh', { type: 'number', step: '0.0001', value: r.per_kwh ?? '', placeholder: '—' }, 'Price per kWh')) : null,
        showM ? h('label', { class: 'pl-rate-cell pl-rate-num' }, h('span', null, `${d.currency}/min`), inp('per_minute', { type: 'number', step: '0.0001', value: r.per_minute ?? '', placeholder: '—' }, 'Price per minute')) : null,
        lock ? null : h('button', { class: 'btn btn--ghost btn--icon btn--sm pl-rate-x', type: 'button', 'aria-label': `Remove rate ${r.name || i + 1}`, onclick: () => { d.rates.splice(i, 1); paintRates(); changed(); } }, icon('x')));
    }));
    if (!d.rates.length) ratesBox.append(h('p', { class: 'faint pl-rates-none' }, 'No time-of-use rates: one price all day.'));
  }
  function paintRule() {
    const c = d.currency;
    const part = [];
    if (d.mode === 'tiers') part.push('each step’s kWh × its price');
    else if (d.mode !== 'minute') part.push(`kWh × ${d.per_kwh != null ? priceEnergy(d.per_kwh, c) : 'price per kWh'}`);
    if (d.mode !== 'kwh' && d.mode !== 'tiers') part.push(`minutes × ${d.per_minute != null ? priceTime(d.per_minute, c) : 'price per minute'}`);
    part.push(d.session_fee != null ? `${formatMoney(d.session_fee, c)} fee` : 'session fee');
    replace(costRule, h('h3', null, 'How a charge here is costed'),
      h('p', { class: 'pl-formula' }, part.join('  +  ')),
      d.mode === 'tiers' ? tierExample(c) : null,
      h('p', null, 'When a charge ends inside this circle it gets this cost by itself, worked out in 15-minute steps so each step uses the rate of its time. A cost typed on a charge by hand always wins, and free Supercharging stays free.'),
      h('p', { class: 'faint' }, 'Charges that already ended keep the cost they have. To price them with this place’s price, save first, then use Reprice past charges.'));
  }
  function tierExample(c) {
    const ex = TI.workedExample(d.tiers, d.session_fee);
    if (!ex.parts.length || ex.parts.some((p) => p.per_kwh == null)) return h('p', { class: 'faint' }, 'Give every step a price to see a worked example.');
    const fee = d.session_fee ? `  +  ${formatMoney(d.session_fee, c)} fee` : '';
    return h('div', { class: 'pl-example' },
      h('p', null, `Example: a ${formatNumber(ex.kwh)} kWh charge when the billing cycle is at ${formatNumber(ex.before)} kWh${d.tiers.base_kwh ? `, household use of ${formatNumber(d.tiers.base_kwh)} kWh counted first` : ''}:`),
      h('p', { class: 'pl-formula' }, `${TI.splitText(ex.parts, c)}${fee}  =  ${formatMoney(ex.total, c)}`),
      d.rates.length ? h('p', { class: 'faint' }, 'Time-of-use rates win their hours: a kWh in a rate’s hours costs the rate’s price, and still counts toward the steps.') : null);
  }
  function paintPrice() {
    tiersWrap.hidden = d.mode !== 'tiers';
    if (d.mode === 'tiers') paintTiers();
    fPerKwh.el.hidden = d.mode === 'minute' || d.mode === 'tiers';
    fPerMin.el.hidden = d.mode === 'kwh' || d.mode === 'tiers';
    priceBody.hidden = !d.priced;
    paintRates();
    paintRule();
  }
  replace(priceBody,
    h('div', { class: 'form-row' }, h('span', { class: 'form-label' }, 'Priced by'), modeSeg,
      h('p', { class: 'form-hint' }, 'Energy suits home and most public chargers; time suits chargers that bill by the minute. Both adds the two. Tiered suits a power company whose price per kWh changes after so many kWh a month.')),
    h('div', { class: 'pl-row pl-row--price', cover: 'web.geofences.form.cost-per-unit' }, fCurrency.el, fPerKwh.el, fPerMin.el, h('div', { cover: 'web.geofences.form.session-fee' }, fFee.el)),
    negNote,
    tiersWrap,
    h('div', { class: 'pl-rates-wrap', cover: 'projects.agile' },
      h('div', { class: 'pl-sub-head' }, h('h3', null, 'Time-of-use rates'), addRateBtn),
      ratesHint, ratesBox, ratesErr,
      h('p', { class: 'faint pl-hint-line' }, icon('info', 'ico--sm'), h('span', null, 'Tariffs that change every hour are typed here as rates; Sentry Mate does not fetch prices from outside, so nothing leaves the server.'))),
    costRule);

  /* ---------- the effect on history (asked of the server after each change) */
  const effBody = h('div', { class: 'pl-effect-body', 'aria-live': 'polite' }, h('p', { class: 'faint' }, 'Working out what saving would change…'));
  // all time, like the counts; drives only for the cars that drove here
  const drillLinks = cur ? h('p', { class: 'pl-drill' }, h('span', { class: 'faint' }, 'All time: ')) : null;
  if (cur) {
    const per = (cur.counts?.drives ? (await drivesByCar(ctx, cars)).get(cur.id) : null) || [];
    if (!ctx.alive()) return;
    const dot = () => h('span', { class: 'faint', 'aria-hidden': 'true' }, ' · ');
    drillLinks.append(
      cur.counts?.charges ? h('a', { class: 'link', href: chargesHref(cur.id) }, `${plural(cur.counts.charges, 'charge', 'charges')} here, every car`) : h('span', { class: 'faint' }, 'no charges here yet'),
      ...(per.length ? per.flatMap((x) => [dot(), h('a', { class: 'link', href: drivesHref(x.car, cur.id) }, `${x.name}’s ${plural(x.drives, 'drive', 'drives')} here`)])
        : [dot(), h('span', { class: 'faint' }, 'no drives start or end here yet')]));
  }
  const effPanel = panel({ title: 'Effect on history', icon: 'timeline', cls: 'pl-effect', cover: 'web.geofences.form.relabel-history' },
    note('Names follow the place as it is now: saving renames every drive, charge and stop inside the circle, including those recorded before the place existed. Moving or shrinking it can take the name off some. Nothing below has changed yet.'),
    effBody, drillLinks);
  let effT = 0, effGen = 0;
  function previewSoon() { clearTimeout(effT); effT = setTimeout(previewNow, 450); }
  ctx.onCleanup(() => clearTimeout(effT));
  async function previewNow() {
    const my = ++effGen;
    const errs = validate(true);
    if (Object.keys(errs).length) { replace(effBody, h('p', { class: 'faint' }, 'Fix the marked fields first; then this shows what saving would change.')); return; }
    const body = draftBody();
    if (cur) body.id = cur.id;
    let e;
    try { e = await ctx.api.post('places/preview', body); } catch (err) {
      if (my !== effGen || !ctx.alive() || err.name === 'AbortError') return;
      replace(effBody, h('p', { class: 'faint' }, err.fields ? 'The server did not accept the draft yet: ' + Object.values(err.fields).join(' ') : 'What saving would change could not be worked out just now.'));
      return;
    }
    if (my !== effGen || !ctx.alive()) return;
    const none = !e.drives_named && !e.drives_unnamed && !e.charges_named && !(e.charges_unnamed) && !e.charges_repriced;
    replace(effBody, none
      ? h('p', { class: 'pl-ok' }, icon('check', 'ico--sm'), cur && !dirty ? 'Nothing to save: this is the place as it is now.' : 'Saving changes no name on any drive or charge, and no past charge would cost differently.')
      : [effectFigures(e), examplesLine(ctx, e, cars)]);
  }

  /* ---------- match rule and overlaps */
  const overlapBox = h('div', { class: 'pl-overlaps' });
  const matchPanel = panel({ title: 'Which place wins', icon: 'pin', cls: 'pl-match', cover: 'web.geofences.form.match-rule' },
    h('p', { class: 'pl-rule-line' }, 'A point is inside a place when it is no farther from the centre than the radius. A point inside several circles goes to the place whose centre is nearest (on a tie, the older place). So each drive end, charge and stop has at most one place.'),
    overlapBox);
  function paintOverlaps() {
    const near = others.map((p) => ({ p, dm: distM(d.lat, d.lon, p.lat, p.lon) })).filter((x) => x.dm < x.p.radius_m + d.radius_m).sort((a, b) => a.dm - b.dm);
    if (!near.length) { replace(overlapBox, h('p', { class: 'pl-ok' }, icon('check', 'ico--sm'), 'This circle overlaps no other place.')); return; }
    replace(overlapBox, h('p', { class: 'warn pl-hint-line' }, icon('warn', 'ico--sm'), h('span', null, `Overlaps ${plural(near.length, 'place', 'places')}. In the shared part, each point goes to whichever centre is nearer:`)),
      h('ul', { class: 'pl-overlap-list' }, near.slice(0, 8).map(({ p, dm }) => h('li', null,
        h('a', { class: 'link', href: editUrl(p.id) }, p.name),
        h('span', { class: 'faint' }, ` · ${kindLabel(p.kind)}, ${format(p.radius_m, 'length')} radius, centres ${format(dm, 'length')} apart`),
        dm <= p.radius_m ? h('span', { class: 'tag tag--warm' }, 'holds this centre') : null))));
  }

  /* ---------- the map with the circle */
  const mapEl = h('div', { class: 'map' });
  const mapCard = h('div', { class: 'glass-card pl-map-card' });
  const radiusOk = () => Number.isFinite(d.radius_m) && d.radius_m >= R_MIN && d.radius_m <= R_MAX;
  // the radius as the person typed it (their units), else as metres converted; while it is not valid, the last valid one
  const radiusText = () => (d.radiusTyped != null ? `${formatNumber(d.radiusTyped)} ${L}` : format(d.radius_m, 'length'));
  const paintMapCard = () => replace(mapCard,
    radiusOk() ? h('strong', null, `${radiusText()} radius`)
      : h('strong', { class: 'warn' }, `Radius not valid: shown at ${format(lastR, 'length')}`),
    h('span', { class: 'faint' }, !radiusOk() ? `Type a radius from ${format(R_MIN, 'length')} to ${format(R_MAX, 'length')}.`
      : lock ? 'View only' : 'Drag the centre to move it, the dot on the edge to resize. A click on the map moves the centre there.'));
  const mapPanel = h('section', { class: 'panel map-panel pl-edit-map', 'aria-label': 'The circle on the map', cover: 'web.geofences.form.position-map' },
    mapEl, h('div', { class: 'map-overlay map-overlay--tl' }, mapCard));
  let m = null, centreMk = null, edgeMk = null;
  const drawR = () => (radiusOk() ? d.radius_m : lastR);
  const edgeAt = () => [d.lon + drawR() / mPerDegLon(d.lat), d.lat];
  function paintCircle({ refit = false } = {}) {
    if (radiusOk()) lastR = d.radius_m;
    paintMapCard();
    if (!m || !m.map.getSource('draft')) return;
    m.map.getSource('draft').setData({ type: 'FeatureCollection', features: [circleFeature({ id: 'draft', kind: d.kind, lat: d.lat, lon: d.lon, radius_m: drawR() })] });
    centreMk?.setLngLat([d.lon, d.lat]);
    edgeMk?.setLngLat(edgeAt());
    if (refit) m.frame(() => m.map.fitBounds(circleBounds(d.lat, d.lon, drawR()), { padding: { top: rpx(110), left: rpx(50), right: rpx(70), bottom: rpx(50) }, maxZoom: 18.5, duration: 0 }));
  }
  async function buildMap() {
    try { m = await createMap(mapEl, ctx, { center: [d.lon, d.lat], zoom: 16 }); } catch { replace(mapEl, mapFallback('The map could not be drawn in this browser. Type the centre and radius instead.')); return; }
    if (!m) return;
    await m.ready;
    if (!ctx.alive()) return;
    m.map.addSource('others', { type: 'geojson', data: { type: 'FeatureCollection', features: others.map((p) => circleFeature(p)) } });
    m.map.addSource('draft', { type: 'geojson', data: { type: 'FeatureCollection', features: [] } });
    m.addLayer({ id: 'others-fill', type: 'fill', source: 'others', paint: { 'fill-color': cssVar('--text-2'), 'fill-opacity': 0.08 } });
    m.addLayer({ id: 'others-line', type: 'line', source: 'others', paint: { 'line-color': cssVar('--text-2'), 'line-opacity': 0.6, 'line-dasharray': [2, 2] } }, { 'line-width': () => rpx(1.2) });
    m.addLayer({ id: 'draft-fill', type: 'fill', source: 'draft', paint: { 'fill-color': cssVar('--live'), 'fill-opacity': 0.16 } });
    m.addLayer({ id: 'draft-line', type: 'line', source: 'draft', paint: { 'line-color': cssVar('--live'), 'line-opacity': 0.95 } }, { 'line-width': () => rpx(2.2) });
    const cEl = h('div', { class: 'pl-handle pl-handle--centre', title: lock ? null : 'Drag to move the centre' });
    const eEl = h('div', { class: 'pl-handle pl-handle--edge', title: lock ? null : 'Drag to change the radius' });
    centreMk = new m.ml.Marker({ element: cEl, draggable: !lock }).setLngLat([d.lon, d.lat]).addTo(m.map);
    edgeMk = new m.ml.Marker({ element: eEl, draggable: !lock }).setLngLat(edgeAt()).addTo(m.map);
    centreMk.on('drag', () => { const p = centreMk.getLngLat(); setCentre(p.lat, p.lng, { live: true }); });
    centreMk.on('dragend', () => { const p = centreMk.getLngLat(); setCentre(p.lat, p.lng); });
    edgeMk.on('drag', () => { const p = edgeMk.getLngLat(); setRadius(distM(d.lat, d.lon, p.lat, p.lng), { live: true }); });
    edgeMk.on('dragend', () => { const p = edgeMk.getLngLat(); setRadius(distM(d.lat, d.lon, p.lat, p.lng)); edgeMk.setLngLat(edgeAt()); });
    if (!lock) m.map.on('click', (e) => setCentre(e.lngLat.lat, e.lngLat.lng));
    paintCircle({ refit: true });
  }
  function setCentre(lat, lon, { live = false } = {}) {
    d.lat = Math.round(lat * 1e6) / 1e6; d.lon = Math.round(lon * 1e6) / 1e6;
    fLat.input.value = d.lat.toFixed(6); fLon.input.value = d.lon.toFixed(6);
    fLat.setError(null); fLon.setError(null);
    paintCircle();
    if (!live) { paintOverlaps(); changed(); lookupAddress(); }
  }
  function setRadius(r, { live = false } = {}) {
    d.radius_m = Math.min(R_MAX, Math.max(R_MIN, Math.round(r)));
    d.radiusTyped = null;
    fRadius.input.value = String(toL(d.radius_m));
    fRadius.setError(null);
    paintCircle();
    if (!live) { paintOverlaps(); changed(); }
  }
  let addrT = 0;
  function lookupAddress() {
    clearTimeout(addrT);
    addrT = setTimeout(async () => {
      let r = null;
      try { r = await ctx.api.get('places/at', { lat: d.lat, lon: d.lon }); } catch { r = null; }
      if (!ctx.alive()) return;
      const a = r?.address;
      replace(addrLine, icon('pin', 'ico--sm'), h('span', null, a ? `Centre: ${[a.short, a.locality].filter(Boolean).join(', ')}` : 'No address is known for the centre.'));
    }, 400);
  }
  ctx.onCleanup(() => clearTimeout(addrT));

  /* ---------- reading and checking the draft */
  function priceBody2() {
    if (!d.priced) return null;
    return {
      currency: d.currency,
      per_kwh: d.mode === 'minute' || d.mode === 'tiers' ? null : d.per_kwh,
      per_minute: d.mode === 'kwh' || d.mode === 'tiers' ? null : d.per_minute,
      tiers: d.mode === 'tiers' ? TI.tiersBody(d.tiers) : null,
      session_fee: d.session_fee,
      rates: d.rates.map((r) => ({ name: (r.name || '').trim(), days: r.days || 'all', from: r.from || '00:00', to: r.to || '24:00',
        per_kwh: d.mode === 'minute' ? null : r.per_kwh ?? null, per_minute: d.mode === 'kwh' || d.mode === 'tiers' ? null : r.per_minute ?? null })),
    };
  }
  function draftBody() {
    // only the metres sent are rounded; the field and the map keep what was typed
    const b = { name: d.name.trim(), kind: d.kind, lat: d.lat, lon: d.lon, radius_m: Math.round(d.radius_m), price: priceBody2() };
    if (d.time_zone) b.time_zone = d.time_zone;
    return b;
  }
  /** Field errors by API path; quiet: do not paint them (the preview asks). */
  function validate(quiet = false) {
    const e = {};
    const nm = d.name.trim();
    if (!nm) e.name = 'Give the place a name.';
    else if (nm.length > 64) e.name = 'At most 64 characters.';
    if (!Number.isFinite(d.lat) || Math.abs(d.lat) > 90) e.lat = 'A latitude from −90 to 90.';
    if (!Number.isFinite(d.lon) || Math.abs(d.lon) > 180) e.lon = 'A longitude from −180 to 180.';
    if (!Number.isFinite(d.radius_m) || d.radius_m < R_MIN || d.radius_m > R_MAX) e.radius_m = `From ${format(R_MIN, 'length')} to ${format(R_MAX, 'length')}.`;
    if (d.priced) {
      const k = d.mode !== 'minute' && d.mode !== 'tiers' ? d.per_kwh : null, mm = d.mode !== 'kwh' && d.mode !== 'tiers' ? d.per_minute : null;
      if (d.mode === 'tiers') Object.assign(e, TI.tierErrors(d.tiers));
      for (const [key, v] of [['price.per_kwh', k], ['price.per_minute', mm]]) if (v != null && (!Number.isFinite(v) || v < -10 || v > 100)) e[key] = 'From −10 to 100.';
      if (d.session_fee != null && (!Number.isFinite(d.session_fee) || d.session_fee < 0 || d.session_fee > 1000)) e['price.session_fee'] = 'From 0 to 1,000; a fee cannot be below zero.';
      if (d.mode !== 'tiers' && k == null && mm == null && d.session_fee == null && !d.rates.length) e.price = d.mode === 'minute' ? 'Type a price per minute, a rate or a session fee, or choose “Free or not tracked”.' : 'Type a price per kWh, a rate or a session fee, or choose “Free or not tracked”.';
      d.rates.forEach((r, i) => {
        if (!(r.name || '').trim()) e[`price.rates.${i}.name`] = `Rate ${i + 1} needs a name.`;
        for (const t of ['from', 'to']) if (!HM_RX.test(r[t] || '')) e[`price.rates.${i}.${t}`] = `Rate ${i + 1}: a time as HH:MM (24:00 ends the day).`;
        if (HM_RX.test(r.from || '') && r.from === r.to) e[`price.rates.${i}.to`] = `Rate ${i + 1}: the end must differ from the start.`;
      });
    }
    if (!quiet) showErrors(e);
    return e;
  }
  const allFields = [...detailsFields, ...priceFields];
  function showErrors(e) {
    for (const x of allFields) x.setError(e[x.path] || null);
    priceMsg.hidden = !e.price; priceMsg.textContent = e.price || '';
    for (const el of ratesBox.querySelectorAll('[data-path]')) { if (e[el.dataset.path]) el.setAttribute('aria-invalid', 'true'); else el.removeAttribute('aria-invalid'); }
    const rateMsgs = Object.entries(e).filter(([k]) => k.startsWith('price.rates')).map(([, v]) => v);
    for (const el of tiersWrap.querySelectorAll('[data-path]')) { if (e[el.dataset.path]) el.setAttribute('aria-invalid', 'true'); else el.removeAttribute('aria-invalid'); }
    const tierMsgs = Object.entries(e).filter(([k]) => k.startsWith('price.tiers')).map(([, v]) => v);
    tiersErr.hidden = !tierMsgs.length;
    replace(tiersErr, tierMsgs.length ? [icon('warn'), [...new Set(tierMsgs)].join(' ')] : []);
    ratesErr.hidden = !rateMsgs.length;
    replace(ratesErr, rateMsgs.length ? [icon('warn'), [...new Set(rateMsgs)].join(' ')] : []);
    const other = Object.entries(e).filter(([k]) => !allFields.some((x) => x.path === k) && k !== 'price' && !k.startsWith('price.rates') && !k.startsWith('price.tiers'));
    formMsg.hidden = !other.length && !Object.keys(e).length;
    formMsg.textContent = other.length ? other.map(([k, v]) => `${k}: ${v}`).join(' ') : Object.keys(e).length ? 'Some fields need a change: they are marked.' : '';
    const first = allFields.find((x) => e[x.path]);
    if (first && !first.el.hidden) first.input.focus();
  }

  function changed() {
    dirty = true;
    saveState.textContent = 'Not saved';
    saveState.classList.add('warn');
    previewSoon();
  }

  // inputs -> draft
  fName.input.addEventListener('input', () => { d.name = fName.input.value; setNames(); changed(); });
  fKind.input.addEventListener('change', () => { d.kind = fKind.input.value; changed(); });
  fZone.input.addEventListener('change', () => { d.time_zone = fZone.input.value; paintRates(); changed(); });
  fRadius.input.addEventListener('input', () => {
    const v = fRadius.value();
    d.radiusTyped = v != null && Number.isFinite(v) ? v : null;
    d.radius_m = d.radiusTyped != null ? toMetric(v, 'length') : NaN;
    if (radiusOk()) { fRadius.setError(null); paintOverlaps(); }
    paintCircle();
    changed();
  });
  fRadius.input.addEventListener('change', () => paintCircle({ refit: true }));
  const onPos = () => { d.lat = fLat.value() ?? NaN; d.lon = fLon.value() ?? NaN; if (Number.isFinite(d.lat) && Number.isFinite(d.lon) && Math.abs(d.lat) <= 90 && Math.abs(d.lon) <= 180) { paintCircle(); paintOverlaps(); } changed(); };
  fLat.input.addEventListener('input', onPos); fLon.input.addEventListener('input', onPos);
  for (const x of [fLat, fLon]) x.input.addEventListener('change', () => { if (Number.isFinite(d.lat) && Number.isFinite(d.lon)) { paintCircle({ refit: true }); lookupAddress(); } });
  fCurrency.input.addEventListener('change', () => { d.currency = fCurrency.input.value; paintRates(); paintTiers(); paintRule(); changed(); });
  for (const [x, key] of [[fPerKwh, 'per_kwh'], [fPerMin, 'per_minute'], [fFee, 'session_fee']]) x.input.addEventListener('input', () => { d[key] = x.value(); priceMsg.hidden = true; paintRule(); changed(); });

  /* ---------- saving */
  const formMsg = h('p', { class: 'form-message', role: 'alert', hidden: true });
  const saveState = h('span', { class: 'pl-save-state faint', 'aria-live': 'polite' }, cur ? `Saved, version ${cur.version}` : 'Not saved yet');
  const saveBtn = h('button', { class: 'btn btn--primary', type: 'submit', disabled: lock }, icon('check', 'ico--sm'), isNew ? 'Save place' : 'Save changes');
  const delBtn = cur && admin ? h('button', { class: 'btn btn--ghost btn--danger', type: 'button', onclick: async () => {
    if (await removePlace(ctx, cur, null)) { dirty = false; ctx.navigate(listUrl()); }
  } }, icon('x', 'ico--sm'), 'Delete place') : null;
  const actions = h('div', { class: 'pl-actions-bar', cover: 'web.geofences.form.save' },
    saveBtn, h('a', { class: 'btn btn--ghost', href: listUrl(), onclick: leave, cover: 'web.geofences.form.back' }, 'Leave without saving'), h('span', { class: 'grow' }), saveState, delBtn,
    lock ? h('p', { class: 'faint pl-lock' }, icon('lock', 'ico--sm'), 'View only: an admin can change places.') : null);

  function patchBody() {
    const b = draftBody();
    const out = {};
    for (const k of ['name', 'kind', 'lat', 'lon', 'radius_m']) if (b[k] !== cur[k]) out[k] = b[k];
    if (d.time_zone && d.time_zone !== cur.time_zone) out.time_zone = d.time_zone;
    const norm = (p) => (p ? JSON.stringify({ c: p.currency, k: p.per_kwh ?? null, m: p.per_minute ?? null, f: p.session_fee ?? null, r: (p.rates || []).map((r) => [r.name, r.days, r.from, r.to, r.per_kwh ?? null, r.per_minute ?? null]), t: p.tiers ? TI.tiersBody(p.tiers) : null }) : 'null');
    if (norm(b.price) !== norm(cur.price)) out.price = b.price;
    return out;
  }
  async function save() {
    if (lock) return;
    const errs = validate();
    if (Object.keys(errs).length) return;
    let body = isNew ? draftBody() : patchBody();
    if (!isNew && !Object.keys(body).length) { flash('Nothing to save: no field changed.', { kind: 'info' }); dirty = false; return; }
    if (!isNew) body = { ...body, version: cur.version };
    saveBtn.disabled = true; saveBtn.setAttribute('aria-busy', 'true');
    let res;
    try {
      res = isNew ? await ctx.api.post('places', body) : await ctx.api.patch(`places/${enc(cur.id)}`, body);
    } catch (e) {
      saveBtn.disabled = false; saveBtn.removeAttribute('aria-busy');
      if (!ctx.alive() || e.name === 'AbortError') return;
      if (e.code === 'stale_version') {
        formMsg.hidden = false;
        replace(formMsg, 'Someone else saved this place while you were editing it, so nothing was saved. ',
          h('button', { class: 'btn btn--sm', type: 'button', onclick: () => { dirty = false; ctx.reload(); } }, icon('reset', 'ico--sm'), 'Load their version (yours is lost)'));
        return;
      }
      if (e.status === 404) { dirty = false; state.navigate(listUrl({ missing: cur.id }), { replace: true }); return; }
      if (e.fields) { showErrors(Object.fromEntries(Object.entries(e.fields).map(([k, v]) => [k, String(v)]))); return; }
      formMsg.hidden = false; formMsg.textContent = e.message || 'The place could not be saved. Try again.';
      return;
    }
    if (!ctx.alive()) return;
    dirty = false;
    const eff = res.effect || {};
    // past charges a reprice would change: ask now, before going back to the list
    if (eff.charges_repriced > 0 && res.price) {
      const choice = await openDialog({
        title: 'Reprice past charges too?',
        body: [h('p', null, `${plural(eff.charges_repriced, 'past charge', 'past charges')} here would cost differently with this price. They keep their cost unless you reprice them. A cost typed by hand is never changed.`)],
        actions: [{ label: 'Keep their costs', value: 'keep' }, { label: 'Choose a span first', value: 'span' }, { label: `Reprice all ${formatNumber(eff.charges_repriced)}`, value: 'all', kind: 'primary' }],
      });
      if (choice === 'all') {
        try { const r = await ctx.api.post(`places/${enc(res.id)}/reprice`, {}); flash(`Saved ${res.name} and repriced ${plural(r.repriced, 'charge', 'charges')}.`, { next: true }); ctx.navigate(listUrl()); return; } catch (e) { flash(`Saved ${res.name}, but repricing failed: ${e.message || 'try again from the editor'}.`, { kind: 'error', next: true }); ctx.navigate(editUrl(res.id)); return; }
      }
      if (choice === 'span') { flash(`Saved ${res.name}. Pick the span to reprice below.`, { next: true }); state.navigate(state.buildUrl('places', { query: { place: res.id, reprice: 1 } }), { replace: true }); return; }
    }
    const words = isNew ? `Added ${res.name}.` : `Saved ${res.name}.`;
    const named = (eff.drives_named || 0) + (eff.charges_named || 0);
    flash(`${words} ${named ? `${plural(eff.drives_named || 0, 'drive', 'drives')} and ${plural(eff.charges_named || 0, 'charge', 'charges')} now show its name.` : ''}`.trim(), { next: true });
    ctx.navigate(listUrl());
  }

  /* ---------- reprice past charges (a saved place with a saved price) */
  let repricePanel = null;
  if (cur) {
    const tz = cur.time_zone || ctx.tz;
    const today = dayKey(ctx.api.now(), tz);
    const fromIn = h('input', { class: 'input', type: 'date', 'aria-label': 'First day', max: today, disabled: lock });
    const toIn = h('input', { class: 'input', type: 'date', 'aria-label': 'Last day', max: today, disabled: lock });
    const onlyIn = h('input', { type: 'checkbox', disabled: lock });
    const out = h('div', { class: 'pl-reprice-out', 'aria-live': 'polite' });
    const span = () => {
      const a = fromIn.value ? dayStart(fromIn.value, tz) : null;
      const b = toIn.value ? dayStart(addDays(toIn.value, 1), tz) : null;
      return { ...(a != null ? { from_ms: a } : {}), ...(b != null ? { to_ms: b } : {}), only_without_cost: onlyIn.checked };
    };
    const run = async (dry) => {
      if (dirty) { replace(out, h('p', { class: 'warn' }, 'Save your changes first: a reprice uses the saved price.')); return; }
      if (!cur.price) { replace(out, h('p', { class: 'warn' }, 'This place has no saved price, so there is nothing to reprice with.')); return; }
      const sp = span();
      if (sp.from_ms != null && sp.to_ms != null && sp.from_ms >= sp.to_ms) { replace(out, h('p', { class: 'warn' }, 'The last day must not be before the first.')); return; }
      let r;
      try { r = await ctx.api.post(`places/${enc(cur.id)}/reprice`, { ...sp, dry_run: true }); } catch (e) { replace(out, h('p', { class: 'warn' }, e.message || 'That did not work.')); return; }
      if (!ctx.alive()) return;
      const money = (list) => (list?.length ? formatMoneyList(list) : DASH);
      const show = (res, done) => replace(out,
        h('div', { class: 'pl-effect-figs' },
          metric('Charges in the span', res.charges, 'count', { size: 'sm', explain: 'recorded here, ended, no cost typed by hand' }),
          metric(done ? 'Repriced' : 'Would change', res.repriced, 'count', { size: 'sm', explain: `${formatNumber(res.unchanged)} keep the same cost` }),
          metric('Cost before', money(res.cost_before), null, { size: 'sm', explain: 'of these charges, per currency' }),
          metric(done ? 'Cost now' : 'Cost after', money(res.cost_after), null, { size: 'sm', explain: 'with the saved price, per currency' })),
        examplesLine(ctx, res, cars, { done }));
      show(r, false);
      if (dry || !r.repriced) { if (!dry && !r.repriced) flash('No charge in that span would change.', { kind: 'info' }); return; }
      const ok = await confirmDialog(`Reprice ${plural(r.repriced, 'past charge', 'past charges')} at ${cur.name} with today’s price? Their old costs are replaced. A cost typed by hand is never touched.`, { title: 'Reprice past charges?', ok: `Reprice ${formatNumber(r.repriced)}` });
      if (!ok) return;
      try {
        const done = await ctx.api.post(`places/${enc(cur.id)}/reprice`, { ...sp, dry_run: false });
        if (!ctx.alive()) return;
        show(done, true);
        flash(`Repriced ${plural(done.repriced, 'charge', 'charges')} at ${cur.name}.`);
        previewSoon();
      } catch (e) { flash(e.message || 'The reprice failed.', { kind: 'error' }); }
    };
    repricePanel = !cur.price ? panel({ title: 'Reprice past charges', icon: 'reset', cls: 'pl-reprice', id: 'reprice', cover: 'web.geofences.form.retro-costs' },
      note(`${cur.name} has no saved price, so there is nothing to reprice past charges with. Give it a price and save; then past charges here can be priced for a span you choose.`))
      : panel({ title: 'Reprice past charges', icon: 'reset', cls: 'pl-reprice', id: 'reprice', cover: 'web.geofences.form.retro-costs' },
      note(`Applies the saved price of ${cur.name} to charges here that already ended, for the days you pick (in ${tz.replace(/_/g, ' ')}). Leave a day blank for no limit. Check shows the change first; nothing is changed until you confirm.`),
      h('div', { class: 'pl-row pl-reprice-row' },
        h('label', { class: 'form-row' }, h('span', { class: 'form-label' }, 'First day'), fromIn),
        h('label', { class: 'form-row' }, h('span', { class: 'form-label' }, 'Last day'), toIn),
        h('label', { class: 'check pl-only' }, onlyIn, h('span', null, 'Only charges that have no cost yet'))),
      h('div', { class: 'row' },
        h('button', { class: 'btn', type: 'button', disabled: lock, onclick: () => run(true) }, 'Check'),
        h('button', { class: 'btn btn--primary', type: 'button', disabled: lock, onclick: () => run(false) }, 'Reprice…')),
      out);
  }

  /* ---------- layout */
  const defaultsNote = h('p', { class: 'pl-hint-line pl-defaults', cover: 'web.geofences.form.defaults' }, icon('info', 'ico--sm'),
    h('span', null, cur ? `Created ${cur.created_ms ? dateTime(cur.created_ms, cur.time_zone || ctx.tz) : 'before records began'}. Move the circle on the map or type the centre.` : startNote));
  const entryNote = h('p', { class: 'faint pl-entry', cover: 'web.geofences.form.entry-from-dashboards' },
    cur ? 'A place name on a drive, charge or stop opens this editor.' : 'A drive, charge or stop without a place opens a new place at its spot; a place name opens its editor.');
  const detailsPanel = panel({ title: 'Place', icon: 'pin', cls: 'pl-details' },
    defaultsNote,
    h('div', { cover: 'web.geofences.form.name' }, fName.el),
    h('div', { class: 'pl-row' }, fKind.el, fRadius.el),
    h('div', { class: 'pl-row' }, fLat.el, fLon.el),
    addrLine, fZone.el, entryNote);
  const pricePanel = panel({ title: 'Charging price', icon: 'bolt', cls: 'pl-price' },
    h('div', { class: 'form-row' }, pricedSeg), priceMsg, priceBody);

  const formEl = h('form', { class: 'pl-form', novalidate: true, cover: 'web.geofences.form.validation', onsubmit: (e) => { e.preventDefault(); save(); } },
    h('div', { class: 'pl-editor' },
      h('div', { class: 'pl-col' }, mapPanel, matchPanel, effPanel),
      h('div', { class: 'pl-col' }, detailsPanel, pricePanel, repricePanel)),
    formMsg, actions);
  replace(ctx.view, h('div', { class: 'places-page places-page--edit' }, head, formEl));
  setNames();
  paintPrice();
  paintOverlaps();
  paintMapCard();
  if (!cur?.address) lookupAddress(); else replace(addrLine, icon('pin', 'ico--sm'), h('span', null, `Centre: ${[cur.address.short, cur.address.locality].filter(Boolean).join(', ')}`));
  ctx.ready();
  buildMap();
  previewNow();
  if (q.reprice && repricePanel) requestAnimationFrame(() => repricePanel.scrollIntoView({ block: 'start' }));
  ctx.events('rows', () => previewSoon());
}
