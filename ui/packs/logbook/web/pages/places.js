// Sentry Mate, "Logbook" UI pack: Places (/places).
//
// Name the places that matter, give them charging prices, and see what a change will do before saving.
// Two views of one page, chosen by the address:
//   /places                      the list (search, kind, sort, CSV), suggestions from where the cars park
//   /places?place=p1             the editor of a saved place
//   /places?new=37.75080,-122.43300   a new place started at a recorded point (drives, charges, timeline)
//   /places?new=last             a new place started at the last known position of the car heard last
// Data: GET /places (list, paged, CSV), /places/{place}, /places/suggestions, /cars and /cars/{car}/now
// (the starting point of a new place), POST /places/preview (what saving or deleting would change),
// POST, PATCH and DELETE /places[/{place}] and POST /places/{place}/reprice for admins.
//
// Matching is the server's: a point belongs to the place whose circle holds it, and of overlapping
// circles the nearest centre wins. The editor draws the other places too and says which one wins where.
// Prices are in the place's own time zone (local clock time, so daylight saving needs nothing).

import { h, replace, cssVar } from '../shared/dom.js';
import { format, convert, toMetric, unitLabel, formatMoney, formatMoneyList } from '../shared/units.js';
import { formatDate, formatDateTime, formatDuration, dayKey, dayStart, addDays } from '../shared/time.js';
import { downloadCsv, csvFilename, column, timeColumn } from '../shared/csv.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { pageHead, section, q, figure, icon, empty, errorView, loadingBlock, button, flag } from '../core/ui.js';
import { dataTable } from '../core/table.js';
import { makeMap, coordsText } from '../core/map.js';
import { confirmDialog } from '../core/dialog.js';
import { field, setFieldError, choice, switchField } from '../core/form.js';
import { zoneLabel } from '../core/format.js';
import { onTheme } from '../core/prefs.js';
import { remount } from '../core/router.js';

export const id = 'places';
export const css = true;
export const title = () => 'Places';

const KINDS = [['home', 'Home'], ['work', 'Work'], ['charger', 'Public charger'], ['service', 'Service centre'], ['other', 'Other']];
const KIND_NAME = Object.fromEntries(KINDS);
const CURRENCIES = ['USD', 'CAD', 'EUR', 'GBP', 'NOK', 'SEK', 'DKK', 'CHF', 'AUD', 'NZD', 'JPY', 'CNY', 'MXN', 'PLN', 'CZK', 'HUF', 'ISK', 'KRW', 'TWD', 'HKD', 'SGD'];
const DAY_KEYS = ['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun'];
const DAY_NAMES = ['Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday', 'Sunday'];
const DAY_SETS = [['all', 'Every day'], ['mon-fri', 'Monday to Friday'], ['sat,sun', 'Saturday and Sunday']];
const RADIUS_MIN = 10, RADIUS_MAX = 5000, RADIUS_NEW = 100;
const EDITOR_ITEMS = ['web.geofences.form.breadcrumb', 'web.geofences.form.entry-from-dashboards', 'web.geofences.form.defaults',
  'web.geofences.form.position-map', 'web.geofences.form.match-rule', 'web.geofences.form.name', 'web.geofences.form.validation',
  'web.geofences.form.not-found', 'web.geofences.form.back', 'web.geofences.form.save', 'web.geofences.form.billing-type',
  'web.geofences.form.cost-per-unit', 'web.geofences.form.session-fee', 'web.geofences.form.negative-price',
  'web.geofences.form.auto-cost', 'web.geofences.form.relabel-history', 'web.geofences.form.retro-costs', 'projects.agile'];

const isAdmin = (ctx) => !!ctx.me && ctx.me.role === 'admin';
const listUrl = (query = null) => state.buildUrl('places', { query });
const editUrl = (pid) => state.buildUrl('places', { query: { place: pid } });
const newUrl = (lat, lon) => state.buildUrl('places', { query: { new: lat != null && lon != null ? `${lat.toFixed(5)},${lon.toFixed(5)}` : 'last' } });
const fix5 = (v) => (v == null ? '—' : v.toFixed(5));
const num = (v) => { if (v == null) return null; const s = String(v).trim().replace(',', '.'); if (!s) return null; const n = Number(s); return Number.isFinite(n) ? n : NaN; };
const plural = (n, one, many = `${one}s`) => `${format(n, 'count')} ${n === 1 ? one : many}`;
const sepEl = () => h('span', { class: 'crumb-sep', 'aria-hidden': 'true' }, '/');
const clone = (x) => (x == null ? x : JSON.parse(JSON.stringify(x)));

/** Distance in metres over the earth's surface. */
function metres(lat1, lon1, lat2, lon2) {
  const R = 6371008.8, rad = Math.PI / 180;
  const a = Math.sin(((lat2 - lat1) * rad) / 2) ** 2 + Math.cos(lat1 * rad) * Math.cos(lat2 * rad) * Math.sin(((lon2 - lon1) * rad) / 2) ** 2;
  return 2 * R * Math.asin(Math.min(1, Math.sqrt(a)));
}
/** The point at a distance (m) and bearing (degrees) from a centre: [lon, lat]. */
function offset(lat, lon, dist, bearing) {
  const R = 6371008.8, rad = Math.PI / 180, d = dist / R, b = bearing * rad, p1 = lat * rad, l1 = lon * rad;
  const p2 = Math.asin(Math.sin(p1) * Math.cos(d) + Math.cos(p1) * Math.sin(d) * Math.cos(b));
  const l2 = l1 + Math.atan2(Math.sin(b) * Math.sin(d) * Math.cos(p1), Math.cos(d) - Math.sin(p1) * Math.sin(p2));
  return [((l2 / rad + 540) % 360) - 180, p2 / rad];
}
function circleRing(lat, lon, r, steps = 72) {
  const ring = [];
  for (let i = 0; i <= steps; i++) ring.push(offset(lat, lon, r, (i / steps) * 360));
  return ring;
}
const circleFeature = (p, props = {}) => ({ type: 'Feature', properties: props, geometry: { type: 'Polygon', coordinates: [circleRing(p.lat, p.lon, p.radius_m)] } });
const zoomFor = (lat, r) => Math.max(3, Math.min(18, Math.log2((156543.03 * Math.cos((lat * Math.PI) / 180) * 260) / Math.max(10, r * 2.6))));

/** Words for a price: '$0.31/kWh · $0.50 per session · 2 rates by time'. */
function priceText(p) {
  if (!p) return null;
  const parts = [];
  if (p.per_kwh != null) parts.push(format({ amount: p.per_kwh, currency: p.currency }, 'price_energy'));
  if (p.per_minute != null) parts.push(format({ amount: p.per_minute, currency: p.currency }, 'price_time'));
  if (p.session_fee != null) parts.push(`${formatMoney(p.session_fee, p.currency)} per session`);
  const n = (p.rates || []).length;
  if (n) parts.push(`${n} ${n === 1 ? 'rate' : 'rates'} by time of day`);
  return `${parts.join(' · ')} (${p.currency})`;
}

/** Whether a rate's days ('all', 'mon-fri', 'sat,sun') include day index d (0 = Monday). */
function daysInclude(days, d) {
  if (!days || days === 'all') return true;
  return String(days).split(',').some((part) => {
    const [a, b] = part.trim().split('-').map((x) => DAY_KEYS.indexOf(x.trim()));
    if (a < 0) return false;
    if (b == null || Number.isNaN(b)) return a === d;
    if (b < 0) return false;
    return a <= b ? d >= a && d <= b : d >= a || d <= b;
  });
}
const hm = (s) => { const m = /^(\d{2}):(\d{2})$/.exec(String(s || '')); if (!m) return null; const v = +m[1] * 60 + +m[2]; return +m[1] <= 24 && +m[2] < 60 && v <= 1440 ? v : null; };
/** The rate in force at minute m of day d: the first one in the list that covers it, else null (the base price). */
function rateAt(rates, d, m) {
  for (let i = 0; i < rates.length; i++) {
    const r = rates[i];
    if (!daysInclude(r.days, d)) continue;
    const a = hm(r.from ?? '00:00'), b = hm(r.to ?? '24:00');
    if (a == null || b == null) continue;
    if (a <= b ? m >= a && m < b : m >= a || m < b) return i;
  }
  return -1;
}

export async function mount(ctx) {
  if (ctx.query.place || ctx.query.new != null) return mountEditor(ctx);
  return mountList(ctx);
}

/* ================================================================== the list */

async function mountList(ctx) {
  const admin = isAdmin(ctx);
  const filters = { q: ctx.query.q || '', kind: KIND_NAME[ctx.query.kind] ? ctx.query.kind : '' };
  const params = () => ({ search: filters.q || null, kind: filters.kind || null });
  const [cars, sugg] = await Promise.all([
    ctx.api.list('cars', { sort: 'name' }, { max: 500 }).all().catch(() => []),
    ctx.api.get('places/suggestions', { limit: 8 }).catch((e) => ({ error: e })),
  ]);
  const carName = new Map(cars.map((c) => [c.id, c.name]));
  const firstCar = cars[0] || null;

  const newBtn = admin
    ? h('a', { class: 'button', href: newUrl(), cover: ['web.geofences.list.create', ...EDITOR_ITEMS] }, icon('pin'), 'New place')
    : h('span', { class: 'button is-disabled', 'aria-disabled': 'true', title: 'Only an admin can add, change or delete places.', cover: ['web.geofences.list.create', ...EDITOR_ITEMS] }, icon('pin'), 'New place');
  const head = pageHead({
    eyebrow: h('nav', { class: 'crumbs', 'aria-label': 'Breadcrumb', cover: 'web.geofences.list.breadcrumb' }, h('a', { href: '/' }, 'All cars'), sepEl(), h('span', { 'aria-current': 'page' }, 'Places')),
    title: 'Places',
    deck: ctx.info.purpose,
    aside: h('div', { class: 'pl-head-aside' }, newBtn, admin ? null : h('p', { class: 'note' }, 'Your account can look at places; an admin changes them.')),
  });

  const intro = h('section', { class: 'pl-intro', 'aria-label': 'What places are for', cover: 'faq.geofence-purpose' },
    h('p', { class: 'pl-intro-lead' }, 'A place puts your own name on a spot. Drives, charges, stops and the timeline then show that name instead of a looked-up street address, which helps where the address is wrong or changes from one visit to the next (street parking, a large car park, a garage under a building).'),
    h('ul', { class: 'pl-intro-points' },
      h('li', null, h('strong', null, 'Matching. '), 'A point belongs to a place when it lies inside its circle. Where circles overlap, the place with the nearest centre wins.'),
      h('li', null, h('strong', null, 'Prices. '), 'A place can carry a charging price, with rates by time of day, so charges there get a cost when they end.'),
      h('li', null, h('strong', null, 'Live. '), 'Renaming, moving or removing a place changes every drive, charge and stop at once; the editor shows how many before you save.')));

  /* ---------------------------------------------------------------- filters */
  const qIn = h('input', { id: 'pl-q', type: 'search', class: 'field-input', value: filters.q, placeholder: 'Name or address', autocomplete: 'off' });
  let qTimer = 0;
  ctx.onCleanup(() => clearTimeout(qTimer));
  qIn.addEventListener('input', () => { clearTimeout(qTimer); qTimer = setTimeout(() => { filters.q = qIn.value.trim(); changed(); }, 250); });
  const kindPick = choice({ label: 'Kind', name: 'kind', value: filters.kind, options: [['', 'All'], ['home', 'Home'], ['work', 'Work'], ['charger', 'Chargers'], ['service', 'Service'], ['other', 'Other']], onchange: (v) => { filters.kind = v; changed(); } });
  const clearBtn = button('Clear filters', { iconName: 'close', cls: 'button-small', onclick: () => { filters.q = ''; filters.kind = ''; qIn.value = ''; kindPick.set(''); changed(); } });
  const toolbar = h('div', { class: 'toolbar pl-toolbar', role: 'search' },
    h('div', { class: 'field field-search' }, h('label', { class: 'field-label', for: 'pl-q' }, 'Find'),
      h('div', { class: 'field-control' }, icon('search', 'icon field-icon'), qIn),
      h('p', { class: 'field-hint' }, 'Matches the name and the address of each place.')),
    h('div', { class: 'toolbar-end' }, kindPick, clearBtn));
  const filtered = () => !!(filters.q || filters.kind);
  function changed() {
    clearBtn.hidden = !filtered();
    ctx.setQuery({ q: filters.q || null, kind: filters.kind || null });
    table.reload(params());
  }
  clearBtn.hidden = !filtered();

  /* ---------------------------------------------------------------- the table */
  const use = (p) => {
    const c = p.counts || {};
    const bits = [];
    if (c.drives) bits.push(firstCar && cars.length === 1
      ? h('a', { href: state.buildUrl('drives', { car: firstCar.id, query: { places: p.id, place_side: 'any' } }), title: `Drives of ${firstCar.name} that start or end here` }, plural(c.drives, 'drive'))
      : h('span', null, plural(c.drives, 'drive')));
    if (c.charges) bits.push(h('a', { href: state.buildUrl('charges', { query: { cars: 'all', places: p.id } }), title: 'Charges of every car at this place' }, plural(c.charges, 'charge')));
    if (!bits.length) return h('span', { class: 'cell-sub' }, c.visits ? plural(c.visits, 'stop') : 'Not used yet');
    return h('span', { class: 'pl-use' }, bits.flatMap((b, i) => (i ? [h('span', { class: 'pl-dot', 'aria-hidden': 'true' }, ' · '), b] : [b])), c.visits ? h('span', { class: 'cell-sub' }, plural(c.visits, 'stop')) : null);
  };
  const del = (p) => removePlace(ctx, p, () => table.reload(params()));
  const table = dataTable(ctx, {
    path: 'places', params: params(), limit: 50, itemName: ['place', 'places'],
    sort: { key: 'name', dir: 'asc' },
    caption: 'Sorted by name, A to Z, until you pick another column. Counts cover the cars your account sees.',
    label: 'Saved places',
    cover: ['web.geofences.list.order', 'web.geofences.list.empty-state'],
    columns: [
      { key: 'name', label: 'Name', sort: 'name', firstDir: 'asc', th: true, cover: 'web.geofences.list.col-name',
        cell: (p) => h('div', { class: 'pl-name-cell' },
          h('a', { class: 'cell-main pl-name', href: editUrl(p.id), title: p.name }, p.name),
          h('span', { class: 'cell-sub' }, [p.address && p.address.short, p.address && p.address.locality].filter((x, i, a) => x && a.indexOf(x) === i).join(', ') || 'No address')),
        value: (p) => p.name },
      { key: 'kind', label: 'Kind', cell: (p) => h('span', { class: ['pl-kind', `pl-kind-${p.kind}`] }, KIND_NAME[p.kind] || p.kind) },
      { key: 'position', label: 'Centre (lat, lon)', cover: 'web.geofences.list.col-position',
        cell: (p) => h('span', { class: 'pl-coords', title: coordsText([p.lon, p.lat]) }, h('span', null, fix5(p.lat)), h('span', null, fix5(p.lon))) },
      { key: 'radius', label: `Radius (${unitLabel('length')})`, num: true, cover: 'web.geofences.list.col-radius', cell: (p) => q(p.radius_m, 'length') },
      { key: 'price', label: 'Charging price', cell: (p) => (p.price ? h('span', { class: 'pl-price' }, priceText(p.price)) : h('span', { class: 'cell-sub', title: 'Charges here get no cost unless one is typed on the charge' }, 'No price')) },
      { key: 'use', label: 'Used by', sort: 'visits', firstDir: 'desc', cell: use, hint: 'Drives that start or end here, charges here and stops here' },
      { key: 'last', label: 'Last visit', sort: 'last_visit', firstDir: 'desc',
        cls: 'pl-nowrap', cell: (p) => (p.last_visit_ms ? h('time', { datetime: new Date(p.last_visit_ms).toISOString(), title: `${zoneLabel(p.time_zone)}` }, formatDate(p.last_visit_ms, p.time_zone, { now: api.now() })) : h('span', { class: 'cell-sub' }, 'Never')) },
      { key: 'actions', label: admin ? 'Change' : 'Open', cls: 'pl-actions', cover: ['web.geofences.list.edit', ...(admin ? ['web.geofences.list.delete'] : [])],
        cell: (p) => h('span', { class: 'pl-row-actions' },
          h('a', { class: 'button button-quiet button-small', href: editUrl(p.id), cover: 'web.geofences.list.edit', 'aria-label': `${admin ? 'Edit' : 'Open'} ${p.name}` }, admin ? 'Edit' : 'Open'),
          admin ? h('button', { type: 'button', class: 'button button-quiet button-small pl-del', 'aria-label': `Delete ${p.name}`, cover: 'web.geofences.list.delete', onclick: () => del(p) }, 'Delete') : null) },
    ],
    empty: () => (filtered()
      ? empty({ title: `No place matches${filters.q ? ` “${filters.q}”` : ''}${filters.kind ? ` among ${KIND_NAME[filters.kind].toLowerCase()} places` : ''}.`, text: 'The search looks at names and addresses. Clear the filters to see every place.', action: button('Clear filters', { onclick: () => clearBtn.click() }) })
      : empty({
        title: 'No places yet.',
        text: 'Without places, drives and charges show the street address the map gives. Name your home, work and the chargers you use; the suggestions below show where your cars park most.',
        action: admin ? h('a', { class: 'button', href: newUrl() }, icon('pin'), 'Add the first place') : 'Ask an admin to add places.',
      })),
  });

  /* ---------------------------------------------------------------- suggestions */
  const suggBox = h('div', { class: 'pl-sugg' });
  const suggSection = section({ title: 'Where your cars park, with no name yet', note: 'most visited first', cls: 'pl-sugg-block', id: 'pl-sugg' }, suggBox);
  function drawSuggestions() {
    if (sugg && sugg.error) { replace(suggBox, errorView(sugg.error, { eyebrow: 'Could not load the suggestions' })); return; }
    const rows = (sugg && sugg.items) || [];
    const t = dataTable(ctx, {
      rows, itemName: ['spot', 'spots'], label: 'Suggested places',
      caption: 'Stops outside every saved place, grouped within about 150 m. Name one to see its drives and charges under that name.',
      csv: { filename: csvFilename('place-suggestions'), columns: [column('Address', 'label'), column('Latitude', 'lat'), column('Longitude', 'lon'), column('Stops', 'visits'), { label: 'Time parked (h)', value: (r) => (r.dwell_ms / 3600000).toFixed(1) }, { label: 'Cars', value: (r) => (r.cars || []).map((c) => carName.get(c) || c).join('; ') }, timeColumn('Last visit', 'last_visit_ms', ctx.tz)] },
      columns: [
        { key: 'label', label: 'Spot', th: true, cell: (r) => h('div', { class: 'pl-name-cell' }, h('span', { class: 'cell-main' }, r.label || 'Unnamed spot'), h('span', { class: 'cell-sub' }, [r.address && r.address.locality, r.address && r.address.region].filter(Boolean).join(', '))), value: (r) => r.label },
        { key: 'position', label: 'Centre (lat, lon)', cell: (r) => h('span', { class: 'pl-coords' }, h('span', null, fix5(r.lat)), h('span', null, fix5(r.lon))) },
        { key: 'visits', label: 'Stops', num: true, sort: true, cell: (r) => format(r.visits, 'count'), value: (r) => r.visits },
        { key: 'dwell', label: 'Time parked', num: true, sort: true, cell: (r) => formatDuration(r.dwell_ms), value: (r) => r.dwell_ms },
        { key: 'cars', label: 'Cars', cell: (r) => (r.cars || []).map((c) => carName.get(c) || c).join(', ') },
        { key: 'last', label: 'Last visit', sort: true, cell: (r) => formatDate(r.last_visit_ms, ctx.tz, { now: api.now() }), value: (r) => r.last_visit_ms },
        { key: 'act', label: '', cls: 'pl-actions', cell: (r) => (admin ? h('a', { class: 'button button-quiet button-small', href: newUrl(r.lat, r.lon) }, 'Name this place', icon('arrowRight')) : null) },
      ],
      sort: { key: 'visits', dir: 'desc' },
      empty: () => empty({ title: 'Every frequent stop already has a name.', text: 'Stops outside all saved places show up here once a car has parked at the same spot more than once.' }),
    });
    replace(suggBox, t.el);
    t.load();
  }
  drawSuggestions();

  ctx.main.append(head, h('div', { class: 'pl-list-top' }, intro), toolbar, section({ title: 'Saved places', cls: 'pl-list-block' }, table.el), suggSection);
  await table.load();
  ctx.ready();
  return {};
}

/** Delete a place after saying what loses its name. */
async function removePlace(ctx, p, after) {
  let eff = null;
  try { eff = await ctx.api.post('places/preview', { place: p.id, delete: true }); } catch { eff = null; }
  if (!ctx.alive()) return false;
  const lose = eff ? [eff.drives_unnamed ? plural(eff.drives_unnamed, 'drive') : null, eff.charges_unnamed ? plural(eff.charges_unnamed, 'charge') : null].filter(Boolean) : [];
  const text = [
    eff ? (lose.length ? `${lose.join(' and ')} that show “${p.name}” will show their street address again, or the name of another place that covers them.` : `No drive or charge shows “${p.name}” now, so nothing else changes.`) : 'What loses the name could not be worked out; the drives and charges here will show their street address again.',
    'Charges keep the cost they have. This cannot be undone.',
  ].join(' ');
  const ok = await confirmDialog({ title: `Delete ${p.name}?`, text, yes: 'Delete the place', danger: true });
  if (!ok || !ctx.alive()) return false;
  try {
    await ctx.api.del(api.apiPath`places/${p.id}`, { params: { version: p.version } });
  } catch (err) {
    if (err && err.code === 'stale_version') ctx.flash(`${p.name} was changed by someone else meanwhile, so it was not deleted. Look at it again first.`, { kind: 'error' });
    else if (err && err.status === 404) ctx.flash(`${p.name} was already deleted.`, { kind: 'info' });
    else ctx.flash(`Not deleted: ${(err && err.message) || 'the server did not answer'}.`, { kind: 'error' });
    after?.();
    return false;
  }
  ctx.flash(`Deleted ${p.name}.${lose.length ? ` ${lose.join(' and ')} show their address again.` : ''}`);
  after?.();
  return true;
}

/* ================================================================== the editor */

async function mountEditor(ctx) {
  const admin = isAdmin(ctx);
  const pid = ctx.query.place || null;
  const fromPoint = !pid && /^-?\d+(\.\d+)?,-?\d+(\.\d+)?$/.test(String(ctx.query.new || '')) ? String(ctx.query.new).split(',').map(Number) : null;

  const [places, cars, cur] = await Promise.all([
    ctx.api.list('places', { sort: 'name' }, { max: 2000, limit: 200 }).all().catch(() => []),
    ctx.api.list('cars', { sort: 'last_activity', dir: 'desc' }, { max: 200 }).all().catch(() => []),
    pid ? ctx.api.get(api.apiPath`places/${pid}`).catch((e) => ({ error: e })) : null,
  ]);
  const crumbLast = h('span', { 'aria-current': 'page' });
  const crumbs = h('nav', { class: 'crumbs', 'aria-label': 'Breadcrumb', cover: 'web.geofences.form.breadcrumb' },
    h('a', { href: '/' }, 'All cars'), sepEl(), h('a', { href: listUrl() }, 'Places'), sepEl(), crumbLast);

  if (cur && cur.error) {
    if (cur.error.status !== 404) throw cur.error;
    crumbLast.textContent = 'Not found';
    ctx.title('No such place');
    ctx.main.append(pageHead({ eyebrow: crumbs, title: 'No such place' }),
      empty({
        cover: 'web.geofences.form.not-found',
        title: 'This place does not exist.',
        text: 'It may have been deleted, or the link is old or mistyped. Nothing was changed. Drives and charges that used it show their street address, or the name of another place that covers them.',
        action: [h('a', { class: 'link-arrow', href: listUrl(), cover: 'web.geofences.form.back' }, 'All places', icon('arrowRight')), admin ? h('a', { class: 'button button-quiet', href: newUrl() }, icon('pin'), 'New place') : null],
      }));
    ctx.ready();
    return {};
  }

  /* ---------------------------------------------------------------- the draft */
  let start = null; // where a new place starts, and why
  if (!cur) {
    if (fromPoint) start = { lat: fromPoint[0], lon: fromPoint[1], why: 'point' };
    else {
      for (const c of cars.slice(0, 5)) {
        let now = null;
        try { now = await ctx.api.get(api.apiPath`cars/${c.id}/now`); } catch { now = null; }
        const loc = now && now.location;
        if (loc && loc.lat && loc.lon && loc.lat.value != null && loc.lon.value != null) { start = { lat: loc.lat.value, lon: loc.lon.value, why: 'car', car: c, at: loc.lat.at_ms }; break; }
      }
      if (!start) start = { lat: null, lon: null, why: 'none' };
    }
  }
  const saved = cur ? { name: cur.name, kind: cur.kind, lat: cur.lat, lon: cur.lon, radius_m: cur.radius_m, price: clone(cur.price) } : null;
  const d = saved ? clone(saved) : { name: '', kind: 'other', lat: start.lat, lon: start.lon, radius_m: RADIUS_NEW, price: null };
  const others = places.filter((p) => !cur || p.id !== cur.id);
  const nearest = (lat, lon) => (lat == null ? null : others.reduce((best, p) => { const m = metres(lat, lon, p.lat, p.lon); return !best || m < best.m ? { p, m } : best; }, null));
  const zoneOf = () => (cur ? cur.time_zone : (nearest(d.lat, d.lon) || {}).p?.time_zone || ctx.tz);
  const editable = admin;
  let dirty = false;
  const priceDraft = () => d.price || { currency: (nearestPrice() || {}).currency || 'USD', per_kwh: null, per_minute: null, rates: [], session_fee: null };
  function nearestPrice() { const n = others.filter((p) => p.price).sort((a, b) => metres(d.lat ?? 0, d.lon ?? 0, a.lat, a.lon) - metres(d.lat ?? 0, d.lon ?? 0, b.lat, b.lon))[0]; return n && n.price; }
  let lastPrice = d.price ? clone(d.price) : null; // what the price switch brings back

  const setTitleText = () => {
    const n = d.name.trim();
    crumbLast.textContent = n || (cur ? cur.name : 'New place');
    titleEl.textContent = n || (cur ? cur.name : 'New place');
    ctx.title(n || (cur ? cur.name : 'New place'));
  };
  const titleEl = h('span');
  const status = h('p', { class: 'pl-status', 'aria-live': 'polite' });
  const backTop = h('a', { class: 'link-arrow pl-back', href: listUrl(), cover: 'web.geofences.form.back' }, icon('chevronLeft'), 'All places');
  const head = pageHead({
    eyebrow: crumbs,
    title: titleEl,
    deck: cur ? [KIND_NAME[cur.kind] || cur.kind, ' · ', [cur.address && cur.address.short, cur.address && cur.address.locality].filter(Boolean).join(', ') || coordsText([cur.lon, cur.lat])] : 'A new place: give it a name, move the circle where it belongs, and save.',
    aside: h('div', { class: 'pl-head-aside' }, backTop, status),
  });
  setTitleText();

  /* ---------------------------------------------------------------- fields */
  const ro = editable ? {} : { disabled: true };
  const fName = field({ label: 'Name', name: 'name', value: d.name, hint: 'As it shows on drives, charges, stops and the timeline, instead of the street address. 1 to 64 characters.', attrs: { maxlength: 64, autocomplete: 'off', required: true, ...ro }, cover: 'web.geofences.form.name' });
  const fKind = field({ label: 'Kind', name: 'kind', type: 'select', value: d.kind, options: KINDS, hint: 'Used by filters (charges at home, at public chargers) and reports.', attrs: ro });
  const lu = unitLabel('length');
  const fLat = field({ label: 'Latitude', name: 'lat', type: 'number', value: d.lat == null ? '' : d.lat.toFixed(5), attrs: { step: '0.00001', min: -90, max: 90, inputmode: 'decimal', ...ro } });
  const fLon = field({ label: 'Longitude', name: 'lon', type: 'number', value: d.lon == null ? '' : d.lon.toFixed(5), attrs: { step: '0.00001', min: -180, max: 180, inputmode: 'decimal', ...ro } });
  const radiusIn = (m) => Math.round(convert(m, 'length'));
  const fRadius = field({ label: `Radius (${lu})`, name: 'radius_m', type: 'number', value: radiusIn(d.radius_m), hint: `${format(RADIUS_MIN, 'length')} to ${format(RADIUS_MAX, 'length')}. Drag the ring's handle on the map, or type it.`, attrs: { step: 1, min: radiusIn(RADIUS_MIN), max: radiusIn(RADIUS_MAX), inputmode: 'numeric', ...ro } });
  const zoneNote = h('p', { class: 'field-hint pl-zone' });
  const drawZone = () => replace(zoneNote, cur ? `Time zone: ${zoneLabel(cur.time_zone)}, from where the place is. Prices below follow this clock.` : `Time zone: ${zoneLabel(zoneOf())}, taken from the nearest saved place when you save.`);
  drawZone();

  const startNote = h('p', { class: 'pl-start note', cover: cur ? 'web.geofences.form.entry-from-dashboards' : start.why === 'point' ? ['web.geofences.form.entry-from-dashboards', 'web.geofences.form.defaults'] : 'web.geofences.form.defaults' },
    icon('info'),
    cur ? 'Places on drives, charges and the timeline link here: a point inside a place opens that place, any other point opens a new place started there.'
      : start.why === 'point' ? `Started from a point recorded on a drive, charge or stop: ${coordsText([start.lon, start.lat])}. The circle starts at ${format(RADIUS_NEW, 'length')} and kind Other; move or resize it to fit.`
        : start.why === 'car' ? `Started at ${start.car.name}’s last known position (${formatDateTime(start.at, start.car.time_zone || ctx.tz, { now: api.now() })}), the car heard from last, with a ${format(RADIUS_NEW, 'length')} circle and kind Other. Click the map to put the centre somewhere else.`
          : `No car has sent a position yet, so the place has no centre: click the map or type the latitude and longitude. The circle starts at ${format(RADIUS_NEW, 'length')}.`);

  const fieldsByPath = new Map([['name', fName], ['kind', fKind], ['lat', fLat], ['lon', fLon], ['radius_m', fRadius]]);

  /* ---------------------------------------------------------------- price */
  const priceBody = h('div', { class: 'pl-price-body' });
  const priceSwitch = switchField({
    label: 'Charging here costs money', checked: !!d.price,
    hint: 'Off: charges here get no cost unless one is typed on the charge.',
    onchange: (on) => { if (on) d.price = lastPrice ? clone(lastPrice) : priceDraft(); else { lastPrice = clone(d.price); d.price = null; } touched(); drawPrice(); },
  });
  if (!editable) priceSwitch.input.disabled = true;
  let billing = d.price && d.price.per_minute != null && d.price.per_kwh == null ? 'minute' : d.price && d.price.per_minute != null ? 'both' : 'kwh';
  const priceFields = new Map(); // path -> field element

  function moneyField(label, path, value, hint, attrs = {}) {
    const f = field({ label, name: path, type: 'number', value: value == null ? '' : value, hint, attrs: { step: '0.0001', inputmode: 'decimal', ...ro, ...attrs } });
    priceFields.set(path, f);
    return f;
  }
  function drawPrice() {
    priceFields.clear();
    if (!d.price) {
      replace(priceBody, h('p', { class: 'note' }, 'No price. Charges that end here get no cost; you can still type a cost on any charge.'));
      return;
    }
    const p = d.price;
    const cur$ = p.currency;
    const currency = field({ label: 'Currency', name: 'price.currency', type: 'select', value: cur$, options: [...new Set([cur$, ...CURRENCIES])].map((c) => [c, c]), hint: 'Costs keep this currency; amounts in different currencies are never added.', attrs: ro });
    priceFields.set('price.currency', currency);
    currency.control.addEventListener('change', () => { p.currency = currency.control.value; touched(); drawPrice(); });
    const billingPick = choice({
      label: 'Priced by', name: 'billing', value: billing, cover: 'web.geofences.form.billing-type',
      options: [['kwh', 'Per kWh'], ['minute', 'Per minute'], ['both', 'Both']],
      onchange: (v) => { billing = v; if (v === 'kwh') { p.per_minute = null; p.rates.forEach((r) => { r.per_minute = null; }); } if (v === 'minute') { p.per_kwh = null; p.rates.forEach((r) => { r.per_kwh = null; }); } touched(); drawPrice(); },
    });
    if (!editable) billingPick.querySelectorAll('button').forEach((b) => { b.disabled = true; });
    const showKwh = billing !== 'minute', showMin = billing !== 'kwh';
    const kwh = showKwh ? moneyField(`Price per kWh (${cur$})`, 'price.per_kwh', p.per_kwh, 'Outside the rates below. AC charges use the energy from the grid meter, fast chargers the energy they delivered.') : null;
    const min = showMin ? moneyField(`Price per minute (${cur$})`, 'price.per_minute', p.per_minute, 'Per minute plugged in and charging, outside the rates below.') : null;
    const fee = moneyField(`Session fee (${cur$})`, 'price.session_fee', p.session_fee, 'Added once to every charge here. Zero or more.', { min: 0, step: '0.01' });
    for (const [f, key] of [[kwh, 'per_kwh'], [min, 'per_minute'], [fee, 'session_fee']]) if (f) f.control.addEventListener('input', () => { p[key] = num(f.control.value); touched(); drawNegative(); drawWeek(); });
    const negLine = h('p', { class: 'pl-neg', cover: 'web.geofences.form.negative-price', 'aria-live': 'polite' });
    function drawNegative() {
      const neg = [p.per_kwh, p.per_minute, ...p.rates.flatMap((r) => [r.per_kwh, r.per_minute])].some((v) => typeof v === 'number' && v < 0);
      negLine.classList.toggle('is-on', neg);
      replace(negLine, icon(neg ? 'warning' : 'info'), neg
        ? 'A price below zero is set: charges in that time get a negative cost (you are paid to charge), and totals go down. Check the sign if that is not meant.'
        : 'Prices may be below zero, for tariffs that pay you to charge at times of surplus; the editor then says so. The session fee is never below zero.');
    }

    const rateRows = h('div', { class: 'pl-rates', role: 'list' });
    function drawRates() {
      replace(rateRows, p.rates.length ? p.rates.map((r, i) => rateRow(r, i)) : h('p', { class: 'note' }, 'No rates: the base price applies at every hour.'));
    }
    function rateRow(r, i) {
      const base = `price.rates.${i}`;
      const fN = field({ label: 'Rate', name: `${base}.name`, value: r.name, attrs: { maxlength: 40, ...ro } });
      const opts = DAY_SETS.some(([v]) => v === r.days) ? DAY_SETS : [[r.days, r.days === 'mon-sun' ? 'Every day' : r.days], ...DAY_SETS];
      const fD = field({ label: 'Days', name: `${base}.days`, type: 'select', value: r.days, options: opts, attrs: ro });
      const fF = field({ label: 'From', name: `${base}.from`, value: r.from, attrs: { maxlength: 5, placeholder: '00:00', inputmode: 'numeric', ...ro }, cls: 'pl-hm' });
      const fT = field({ label: 'Until', name: `${base}.to`, value: r.to, attrs: { maxlength: 5, placeholder: '24:00', inputmode: 'numeric', ...ro }, cls: 'pl-hm' });
      const fK = showKwh ? field({ label: `Per kWh (${cur$})`, name: `${base}.per_kwh`, type: 'number', value: r.per_kwh ?? '', attrs: { step: '0.0001', ...ro } }) : null;
      const fM = showMin ? field({ label: `Per minute (${cur$})`, name: `${base}.per_minute`, type: 'number', value: r.per_minute ?? '', attrs: { step: '0.0001', ...ro } }) : null;
      for (const [f, key] of [[fN, 'name'], [fD, 'days'], [fF, 'from'], [fT, 'to'], [fK, 'per_kwh'], [fM, 'per_minute']]) {
        if (!f) continue;
        priceFields.set(`${base}.${key}`, f);
        f.control.addEventListener(f.control.tagName === 'SELECT' ? 'change' : 'input', () => {
          r[key] = key === 'per_kwh' || key === 'per_minute' ? num(f.control.value) : f.control.value.trim();
          touched(); drawWeek(); if (key === 'per_kwh' || key === 'per_minute') drawNegative();
        });
      }
      const rm = editable ? button('Remove', { cls: 'button-small pl-rate-rm', onclick: () => { p.rates.splice(i, 1); touched(); drawRates(); drawWeek(); drawNegative(); } }) : null;
      return h('div', { class: ['pl-rate', `pl-rate-c${i % 6}`, { 'pl-rate-one': !(fK && fM) }], role: 'listitem' }, h('span', { class: 'pl-swatch', 'aria-hidden': 'true' }), fN, fD, fF, fT, fK, fM, rm);
    }
    const addRate = editable && p.rates.length < 24 ? button('Add a rate by time of day', { iconName: 'calendar', cls: 'button-small', onclick: () => {
      p.rates.push({ name: p.rates.length ? `Rate ${p.rates.length + 1}` : 'Off-peak', days: 'all', from: '00:00', to: '07:00', per_kwh: showKwh ? p.per_kwh : null, per_minute: showMin ? p.per_minute : null });
      touched(); drawRates(); drawWeek();
    } }) : null;

    const week = h('div', { class: 'pl-week', role: 'img' });
    const weekLegend = h('ul', { class: 'pl-legend' });
    const weekCsv = h('button', { type: 'button', class: 'button button-quiet button-small', onclick: () => {
      const rows = [];
      for (let dd = 0; dd < 7; dd++) for (let hh = 0; hh < 24; hh++) {
        const i = rateAt(p.rates, dd, hh * 60);
        const r = i >= 0 ? p.rates[i] : null;
        rows.push({ day: DAY_NAMES[dd], hour: `${String(hh).padStart(2, '0')}:00`, rate: r ? r.name : 'Base price', per_kwh: r ? r.per_kwh ?? p.per_kwh : p.per_kwh, per_minute: r ? r.per_minute ?? p.per_minute : p.per_minute });
      }
      downloadCsv(csvFilename('place-prices', d.name || 'new-place'), rows, [column('Day', 'day'), column('Hour starting', 'hour'), column('Rate', 'rate'), { label: `Per kWh (${p.currency})`, value: (r) => r.per_kwh }, { label: `Per minute (${p.currency})`, value: (r) => r.per_minute }]);
    } }, icon('download'), 'Download CSV');
    function drawWeek() {
      const tz = zoneOf();
      const cells = [h('span', { class: 'pl-week-corner' })];
      for (let hh = 0; hh < 24; hh++) cells.push(h('span', { class: 'pl-week-hour' }, hh % 3 === 0 ? String(hh).padStart(2, '0') : ''));
      const words = [];
      for (let dd = 0; dd < 7; dd++) {
        cells.push(h('span', { class: 'pl-week-day' }, DAY_NAMES[dd].slice(0, 3)));
        for (let hh = 0; hh < 24; hh++) {
          // an hour cell takes the rate of its first and last quarter: a split hour shows both halves
          const a = rateAt(p.rates, dd, hh * 60), b = rateAt(p.rates, dd, hh * 60 + 45);
          const cls = (i) => (i >= 0 ? `pl-rate-c${i % 6}` : 'pl-base');
          const name = (i) => (i >= 0 ? p.rates[i].name || `Rate ${i + 1}` : 'Base price');
          cells.push(h('span', { class: ['pl-week-cell', cls(a), a !== b && `pl-split-${cls(b)}`], title: `${DAY_NAMES[dd]} ${String(hh).padStart(2, '0')}:00: ${name(a)}${a !== b ? `, then ${name(b)}` : ''}` }));
        }
        words.push(`${DAY_NAMES[dd]}: ${[...new Set(Array.from({ length: 24 }, (_, hh) => rateAt(p.rates, dd, hh * 60)))].map((i) => (i >= 0 ? p.rates[i].name : 'base price')).join(', ')}`);
      }
      replace(week, cells);
      week.setAttribute('aria-label', `Which price applies each hour of the week, in ${tz}. ${words.join('. ')}.`);
      const priceOf = (kwhV, minV) => [kwhV != null ? format({ amount: kwhV, currency: p.currency }, 'price_energy') : null, minV != null ? format({ amount: minV, currency: p.currency }, 'price_time') : null].filter(Boolean).join(' + ') || 'no price';
      replace(weekLegend,
        h('li', null, h('span', { class: 'pl-swatch pl-base', 'aria-hidden': 'true' }), 'Base price: ', priceOf(p.per_kwh, p.per_minute)),
        p.rates.map((r, i) => h('li', null, h('span', { class: ['pl-swatch', `pl-rate-c${i % 6}`], 'aria-hidden': 'true' }), `${r.name || `Rate ${i + 1}`}: `, priceOf(r.per_kwh ?? (showKwh ? p.per_kwh : null), r.per_minute ?? (showMin ? p.per_minute : null)))));
    }
    drawRates(); drawWeek(); drawNegative();
    replace(priceBody,
      h('div', { class: 'pl-price-grid', cover: 'web.geofences.form.cost-per-unit' }, currency, billingPick, kwh, min, h('div', { cover: 'web.geofences.form.session-fee' }, fee)),
      negLine,
      h('div', { class: 'pl-tou', cover: 'projects.agile' },
        h('div', { class: 'block-head pl-sub-head' }, h('h3', { class: 'label' }, 'Rates by time of day', h('span', { class: 'label-note' }, `local time at the place, ${zoneLabel(zoneOf())}`)), h('div', { class: 'block-actions' }, weekCsv)),
        h('p', { class: 'note' }, 'Times are the clock at the place, so daylight saving changes nothing here. The first rate in the list that covers a moment applies; at other times the base price. A session that crosses rates is priced part by part. Until 24:00 means the end of the day; a rate from 22:00 until 06:00 runs over midnight.'),
        rateRows, addRate, week, weekLegend));
  }
  drawPrice();

  const autoCost = h('div', { class: 'pl-rule', cover: 'web.geofences.form.auto-cost' },
    h('p', { class: 'label' }, 'When a charge here gets its cost'),
    h('p', { class: 'note' }, 'When a charge ends inside this place, it gets its cost from the price saved here at that moment: energy × the price per kWh and minutes × the price per minute, rate by rate, plus the session fee. A cost typed on a charge is never replaced, and free Supercharging stays free. Charges that already ended keep their cost; to apply a new price to them, use Reprice past charges.'));

  /* ---------------------------------------------------------------- map */
  const mapBox = h('div', { class: 'map pl-map', role: 'region', 'aria-label': 'The place on the map: its circle and the other saved places near it' });
  const mapNote = h('p', { class: 'note pl-map-note' }, editable ? 'Drag the centre dot to move the place, drag the ring’s handle to resize it, or click the map to put the centre there. Other saved places are drawn in grey.' : 'Other saved places are drawn in grey.');
  let m = null, centreMarker = null, edgeMarker = null;
  function circleData() { return { type: 'FeatureCollection', features: d.lat == null || d.lon == null ? [] : [circleFeature(d)] }; }
  function othersData() { return { type: 'FeatureCollection', features: others.filter((p) => d.lat == null || metres(d.lat, d.lon, p.lat, p.lon) < 30000).map((p) => circleFeature(p, { name: p.name })) }; }
  function drawCircle() {
    if (!m || !m.map.getSource('pl-circle')) return;
    m.map.getSource('pl-circle').setData(circleData());
    if (d.lat != null && d.lon != null) {
      centreMarker?.setLngLat([d.lon, d.lat]);
      edgeMarker?.setLngLat(offset(d.lat, d.lon, d.radius_m, 90));
    }
  }
  async function drawMap() {
    const has = d.lat != null && d.lon != null;
    m = await makeMap(ctx, mapBox, { center: has ? [d.lon, d.lat] : [-98, 39], zoom: has ? zoomFor(d.lat, d.radius_m) : 3, maxZoom: 18 });
    if (!m) return;
    await m.loaded;
    if (!ctx.alive()) return;
    const map = m.map;
    map.addSource('pl-others', { type: 'geojson', data: othersData() });
    map.addSource('pl-circle', { type: 'geojson', data: circleData() });
    const fills = [['pl-others-fill', 'pl-others', '--ink-3', 0.1], ['pl-circle-fill', 'pl-circle', '--accent', 0.16]];
    for (const [lid, src, tok, op] of fills) map.addLayer({ id: lid, type: 'fill', source: src, paint: { 'fill-color': cssVar(tok), 'fill-opacity': op } });
    m.addLine({ id: 'pl-others-line', source: 'pl-others', paint: { 'line-dasharray': [2, 2] } }, 1.4, '--ink-3');
    m.addLine({ id: 'pl-circle-line', source: 'pl-circle' }, 2.6, '--accent');
    ctx.onCleanup(onTheme(() => { if (!map.isStyleLoaded()) return; for (const [lid, , tok] of fills) if (map.getLayer(lid)) map.setPaintProperty(lid, 'fill-color', cssVar(tok)); }));
    const handle = (cls, label) => h('div', { class: ['pl-handle', cls], role: 'img', 'aria-label': label, title: label });
    const placeMarkers = () => {
      if (centreMarker || d.lat == null) return;
      centreMarker = new m.ml.Marker({ element: handle('pl-handle-centre', editable ? 'Centre of the place: drag to move' : 'Centre of the place'), draggable: editable }).setLngLat([d.lon, d.lat]).addTo(map);
      edgeMarker = editable ? new m.ml.Marker({ element: handle('pl-handle-edge', 'Edge of the circle: drag to resize'), draggable: true }).setLngLat(offset(d.lat, d.lon, d.radius_m, 90)).addTo(map) : null;
      centreMarker.on('drag', () => { const ll = centreMarker.getLngLat(); setCentre(ll.lat, ll.lng, false); });
      centreMarker.on('dragend', () => schedulePreview());
      edgeMarker?.on('drag', () => { const ll = edgeMarker.getLngLat(); setRadius(metres(d.lat, d.lon, ll.lat, ll.lng), false, true); });
      edgeMarker?.on('dragend', () => { edgeMarker.setLngLat(offset(d.lat, d.lon, d.radius_m, 90)); schedulePreview(); });
    };
    placeMarkers();
    if (editable) map.on('click', (e) => { setCentre(e.lngLat.lat, e.lngLat.lng, true); placeMarkers(); });
    if (has) map.fitBounds(boundsOfCircle(), { padding: 40, animate: false, maxZoom: 18 });
  }
  const boundsOfCircle = () => { const r = d.radius_m * 1.6; const s = offset(d.lat, d.lon, r, 180), n = offset(d.lat, d.lon, r, 0), w = offset(d.lat, d.lon, r, 270), e = offset(d.lat, d.lon, r, 90); return [[w[0], s[1]], [e[0], n[1]]]; };

  function setCentre(lat, lon, preview) {
    d.lat = Math.round(lat * 1e6) / 1e6; d.lon = Math.round(lon * 1e6) / 1e6;
    fLat.control.value = d.lat.toFixed(5); fLon.control.value = d.lon.toFixed(5);
    setFieldError(fLat, null); setFieldError(fLon, null);
    touched(preview); drawCircle(); drawMatch(); drawZone();
    if (m && m.map.getSource('pl-others')) m.map.getSource('pl-others').setData(othersData());
  }
  function setRadius(metresV, preview, fromMap = false) {
    d.radius_m = Math.max(RADIUS_MIN, Math.min(RADIUS_MAX, Math.round(metresV)));
    if (fromMap) fRadius.control.value = radiusIn(d.radius_m);
    setFieldError(fRadius, null);
    touched(preview); drawCircle(); drawMatch();
  }
  fName.control.addEventListener('input', () => { d.name = fName.control.value; setFieldError(fName, null); setTitleText(); touched(); });
  fKind.control.addEventListener('change', () => { d.kind = fKind.control.value; touched(); });
  for (const f of [fLat, fLon]) f.control.addEventListener('input', () => {
    const la = num(fLat.control.value), lo = num(fLon.control.value);
    if (la != null && lo != null && Math.abs(la) <= 90 && Math.abs(lo) <= 180) { d.lat = la; d.lon = lo; touched(); drawCircle(); drawMatch(); drawZone(); }
  });
  fRadius.control.addEventListener('input', () => {
    const v = num(fRadius.control.value);
    if (v != null && !Number.isNaN(v)) { const mv = toMetric(v, 'length'); if (mv >= RADIUS_MIN && mv <= RADIUS_MAX) setRadius(mv, true); }
  });

  /* ---------------------------------------------------------------- which place wins */
  const matchBox = h('div', { class: 'pl-match-body' });
  function drawMatch() {
    if (d.lat == null || d.lon == null) { replace(matchBox, h('p', { class: 'note' }, 'Put the centre on the map to see which places overlap.')); return; }
    const near = others.map((p) => ({ p, dist: metres(d.lat, d.lon, p.lat, p.lon) })).filter((x) => x.dist < x.p.radius_m + d.radius_m).sort((a, b) => a.dist - b.dist);
    const name = d.name.trim() || 'this place';
    replace(matchBox,
      h('p', { class: 'note' }, 'A point belongs to a place when it lies inside its circle. Where circles overlap, the place whose centre is nearest wins (on a tie, the older place). So a big circle never swallows a small one inside it: the small one keeps the points nearer its own centre.'),
      near.length
        ? h('ul', { class: 'pl-overlaps' }, near.map(({ p, dist }) => {
          const inside = dist <= d.radius_m || dist <= p.radius_m;
          return h('li', null,
            h('a', { href: editUrl(p.id), class: 'pl-overlap-name' }, p.name),
            h('span', { class: 'cell-sub' }, `${KIND_NAME[p.kind] || p.kind}, centre ${format(dist, 'length')} away, radius ${format(p.radius_m, 'length')}`),
            h('span', { class: 'pl-overlap-rule' }, `Where both circles reach, points nearer the centre of ${p.name} go to ${p.name}, the others to ${name}; the line between is halfway, ${format(dist / 2, 'length')} from each centre.${inside ? ' One centre lies inside the other circle.' : ''}`));
        }))
        : h('p', { class: 'pl-none' }, icon('check'), `No other saved place overlaps this circle: every point inside it goes to ${name}.`));
  }
  drawMatch();

  /* ---------------------------------------------------------------- effect on history */
  const effectBox = h('div', { class: 'pl-effect-body', 'aria-live': 'polite' });
  let previewTimer = 0, previewSeq = 0;
  ctx.onCleanup(() => clearTimeout(previewTimer));
  function schedulePreview() { clearTimeout(previewTimer); previewTimer = setTimeout(runPreview, 450); }
  function draftBody() {
    const b = { name: d.name.trim() || 'Draft', kind: d.kind, lat: d.lat, lon: d.lon, radius_m: d.radius_m, price: d.price ? cleanPrice(d.price) : null };
    if (cur) b.id = cur.id;
    return b;
  }
  const exampleHref = (x) => (x.kind === 'drive' ? state.buildUrl('drive', { car: x.car, drive: x.id }) : state.buildUrl('charge', { car: x.car, charge: x.id }));
  const carNames = new Map(cars.map((c) => [c.id, c]));
  async function runPreview() {
    if (d.lat == null || d.lon == null) { replace(effectBox, h('p', { class: 'note' }, 'Put the centre on the map to see what the place would name.')); return; }
    const seq = ++previewSeq;
    effectBox.classList.add('is-busy');
    let e = null, err = null;
    try { e = await ctx.api.post('places/preview', draftBody()); } catch (x) { err = x; }
    if (!ctx.alive() || seq !== previewSeq) return;
    effectBox.classList.remove('is-busy');
    if (err) {
      replace(effectBox, err.fields ? h('p', { class: 'note pl-warn' }, icon('warning'), 'The preview waits until the fields are valid: ', Object.values(err.fields).join(' ')) : errorView(err, { retry: runPreview, eyebrow: 'Could not work out the effect' }));
      return;
    }
    const nm = d.name.trim() || (cur ? cur.name : 'the new place');
    const none = !e.drives_named && !e.drives_unnamed && !e.charges_named && !e.charges_unnamed;
    const figs = h('div', { class: 'figs pl-effect-figs' },
      figure('Gain the name', h('span', null, plural(e.drives_named, 'drive'), h('br'), plural(e.charges_named, 'charge')), `will show “${nm}”`),
      figure('Lose the name', h('span', null, plural(e.drives_unnamed, 'drive'), h('br'), plural(e.charges_unnamed, 'charge')), cur ? `show “${cur.name}” now, and will not` : 'a new place takes points from overlapping places'),
      figure('Would be repriced', plural(e.charges_repriced, 'charge'), d.price ? 'past charges here whose cost a reprice after saving would change' : 'no price, so nothing to reprice'));
    const rows = e.example_rows || [];
    const WORD = { named: 'gains the name', unnamed: 'loses the name', repriced: 'cost would change on a reprice' };
    const examples = rows.length ? h('div', { class: 'pl-examples' }, h('p', { class: 'label' }, 'For example'),
      h('ul', null, rows.map((x) => { const c = carNames.get(x.car); return h('li', null, h('a', { href: exampleHref(x) }, `${x.kind === 'drive' ? 'Drive' : 'Charge'} of ${formatDate(x.start_ms, (c && c.time_zone) || ctx.tz, { now: api.now() })}`), h('span', { class: 'cell-sub' }, ` ${c ? c.name : x.car}, ${WORD[x.change] || x.change}`)); }))) : null;
    replace(effectBox,
      h('p', { class: 'note' }, dirty || !cur ? (none ? 'Saving would not change the name of any drive or charge.' : `If you save now: ${plural(e.drives_named + e.drives_unnamed, 'drive')} and ${plural(e.charges_named + e.charges_unnamed, 'charge')} change their name. Nothing changes until you save.`) : 'No unsaved changes. Move the circle or change the name or price to see what saving would do.'),
      figs, examples,
      h('p', { class: 'note' }, 'Counts look at drive starts and ends, charges and stops of the cars your account sees. Costs stay as they are when you save; only a reprice changes past charges.'));
  }

  /* ---------------------------------------------------------------- reprice */
  const repriceBox = h('div', { class: 'pl-reprice-body' });
  function drawReprice() {
    if (!cur) { replace(repriceBox, h('p', { class: 'note' }, 'Save the place first; then past charges here can get its price.')); return; }
    if (!cur.price) { replace(repriceBox, h('p', { class: 'note' }, 'This place has no saved price, so there is nothing to apply to past charges.')); return; }
    const tz = cur.time_zone;
    const today = dayKey(api.now(), tz);
    const fFrom = field({ label: 'From', name: 'from', type: 'date', value: '', hint: 'Empty: from the first charge.', attrs: { max: today, ...ro } });
    const fTo = field({ label: 'Until', name: 'to', type: 'date', value: today, hint: 'Including this day.', attrs: { max: today, ...ro } });
    const only = switchField({ label: 'Only charges without a cost', checked: true, hint: 'Off: charges priced before, with an older price, get the price saved now as well. Typed costs are never changed.' });
    if (!editable) only.input.disabled = true;
    const out = h('div', { class: 'pl-reprice-out', 'aria-live': 'polite' });
    const span = () => {
      const a = fFrom.control.value, b = fTo.control.value;
      const sp = { only_without_cost: only.input.checked };
      if (a) sp.from_ms = dayStart(a, tz);
      if (b) sp.to_ms = dayStart(addDays(b, 1), tz);
      return sp;
    };
    const money = (l) => (l && l.length ? formatMoneyList(l) : 'no cost');
    async function check() {
      if (dirty) { replace(out, h('p', { class: 'note pl-warn' }, icon('warning'), 'You have unsaved changes. A reprice uses the price as saved: save first, or leave the changes.')); return; }
      replace(out, loadingBlock(2));
      let r;
      try { r = await ctx.api.post(api.apiPath`places/${cur.id}/reprice`, { ...span(), dry_run: true }); } catch (err) { if (ctx.alive()) replace(out, errorView(err, { retry: check, eyebrow: 'Could not check' })); return; }
      if (!ctx.alive()) return;
      const go = r.repriced && editable ? button(`Reprice ${plural(r.repriced, 'charge')}`, { kind: 'primary', onclick: async () => {
        const ok = await confirmDialog({ title: `Reprice ${plural(r.repriced, 'charge')} at ${cur.name}?`, text: `Their cost becomes what the saved price gives: ${money(r.cost_before)} in all now, ${money(r.cost_after)} after. Charges with a typed cost are left alone.`, yes: 'Reprice', danger: false });
        if (!ok || !ctx.alive()) return;
        try {
          const done = await ctx.api.post(api.apiPath`places/${cur.id}/reprice`, { ...span(), dry_run: false });
          ctx.flash(`Repriced ${plural(done.repriced, 'charge')} at ${cur.name}.`);
          check();
        } catch (err) { ctx.flash(`Not repriced: ${(err && err.message) || 'the server did not answer'}.`, { kind: 'error' }); }
      } }) : null;
      replace(out,
        h('div', { class: 'figs pl-reprice-figs' },
          figure('Charges in the span', format(r.charges, 'count'), only.input.checked ? 'ended here, without a cost' : 'ended here, without a typed cost'),
          figure('Would change', format(r.repriced, 'count'), `${format(r.unchanged, 'count')} keep the same cost`),
          figure('Cost now', money(r.cost_before), 'of those charges, per currency'),
          figure('After the reprice', money(r.cost_after), 'with the price saved now')),
        go ? h('div', { class: 'form-actions' }, go) : h('p', { class: 'note' }, r.repriced ? 'Only an admin can reprice.' : 'Every charge in the span already has the cost this price gives.'));
    }
    replace(repriceBox,
      h('p', { class: 'note' }, `Applies the price saved now to charges that already ended here, in the span you pick (days in ${zoneLabel(tz)}). Check first: nothing changes until you press Reprice.`),
      h('div', { class: 'pl-reprice-form' }, fFrom, fTo, only, h('div', { class: 'pl-reprice-go' }, button('Check what would change', { onclick: check, iconName: 'search' }))),
      out);
  }
  drawReprice();

  /* ---------------------------------------------------------------- used by */
  const usedBy = cur ? section({ title: 'Used by', note: 'drill down', cls: 'pl-used' },
    h('p', { class: 'note' }, `${plural(cur.counts.drives, 'drive')}, ${plural(cur.counts.charges, 'charge')} and ${plural(cur.counts.visits, 'stop')} of the cars your account sees${cur.last_visit_ms ? `; last visit ${formatDateTime(cur.last_visit_ms, cur.time_zone, { now: api.now() })}` : ''}.`),
    h('ul', { class: 'pl-used-list' },
      cur.counts.charges ? h('li', null, h('a', { class: 'link-arrow', href: state.buildUrl('charges', { query: { cars: 'all', places: cur.id } }) }, 'Charges here, every car', icon('arrowRight'))) : null,
      cars.slice(0, 12).map((c) => h('li', null, h('span', { class: 'pl-car' }, c.name),
        h('a', { href: state.buildUrl('drives', { car: c.id, query: { places: cur.id, place_side: 'any' } }) }, 'drives'), ' · ',
        h('a', { href: state.buildUrl('charges', { car: c.id, query: { places: cur.id } }) }, 'charges'), ' · ',
        h('a', { href: state.buildUrl('timeline', { car: c.id, query: { place: cur.id } }) }, 'timeline'))))) : null;

  /* ---------------------------------------------------------------- save, leave, delete */
  const topError = h('div', { class: 'form-error', role: 'alert', hidden: true });
  const saveBtn = h('button', { type: 'submit', class: 'button', cover: 'web.geofences.form.save', hidden: !editable }, cur ? 'Save the place' : 'Add the place');
  const leaveBtn = h('a', { class: 'button button-quiet', href: listUrl(), cover: 'web.geofences.form.back' }, editable ? 'Leave without saving' : 'Back to all places');
  const delBtn = cur && editable ? button('Delete…', { kind: 'danger', cls: 'pl-del-btn', onclick: async () => { if (await removePlace(ctx, cur, null)) { dirty = false; ctx.go(listUrl()); } } }) : null;
  const actions = h('div', { class: 'form-actions pl-actions-bar' }, saveBtn, leaveBtn, h('span', { class: 'pl-grow' }), delBtn);
  for (const a of [backTop, leaveBtn]) a.addEventListener('click', async (e) => {
    if (!dirty) return;
    e.preventDefault();
    if (await confirmDialog({ title: 'Leave without saving?', text: 'Your changes to this place are not saved. Leaving drops them; history stays as it is.', yes: 'Leave', no: 'Stay', danger: true })) { dirty = false; ctx.go(listUrl()); }
  });
  const onUnload = (e) => { if (dirty) { e.preventDefault(); e.returnValue = ''; } };
  window.addEventListener('beforeunload', onUnload);
  ctx.onCleanup(() => window.removeEventListener('beforeunload', onUnload));

  function touched(preview = true) {
    dirty = editable && JSON.stringify(currentBody()) !== JSON.stringify(saved ? bodyOf(saved) : initialBody);
    status.textContent = !editable ? 'View only: an admin changes places.' : dirty ? 'Unsaved changes' : cur ? 'Saved' : '';
    status.classList.toggle('is-dirty', dirty);
    if (!topError.hidden && !formEl.querySelector('[aria-invalid="true"]')) topError.hidden = true;
    if (preview) schedulePreview();
  }
  const bodyOf = (x) => ({ name: x.name.trim(), kind: x.kind, lat: x.lat, lon: x.lon, radius_m: x.radius_m, price: x.price ? cleanPrice(x.price) : null });
  const currentBody = () => bodyOf(d);
  const initialBody = bodyOf(d);

  function cleanPrice(p) {
    return { currency: p.currency, per_kwh: p.per_kwh ?? null, per_minute: p.per_minute ?? null, session_fee: p.session_fee ?? null,
      rates: (p.rates || []).map((r) => ({ name: (r.name || '').trim(), days: r.days || 'all', from: r.from || '00:00', to: r.to || '24:00', per_kwh: r.per_kwh ?? null, per_minute: r.per_minute ?? null })) };
  }
  function check() {
    const bad = {};
    const nm = d.name.trim();
    if (!nm) bad.name = 'Give the place a name.';
    else if (nm.length > 64) bad.name = 'At most 64 characters.';
    const la = num(fLat.control.value), lo = num(fLon.control.value);
    if (la == null || Number.isNaN(la)) bad.lat = 'Click the map or type a latitude.'; else if (Math.abs(la) > 90) bad.lat = 'Between −90 and 90.';
    if (lo == null || Number.isNaN(lo)) bad.lon = 'Click the map or type a longitude.'; else if (Math.abs(lo) > 180) bad.lon = 'Between −180 and 180.';
    const rv = num(fRadius.control.value);
    const rm = rv == null || Number.isNaN(rv) ? null : toMetric(rv, 'length');
    if (rm == null) bad.radius_m = 'Give a radius.';
    else if (rm < RADIUS_MIN - 0.5 || rm > RADIUS_MAX + 0.5) bad.radius_m = `Between ${format(RADIUS_MIN, 'length')} and ${format(RADIUS_MAX, 'length')}.`;
    if (d.price) {
      const p = d.price;
      for (const k of ['per_kwh', 'per_minute', 'session_fee']) if (Number.isNaN(p[k])) bad[`price.${k}`] = 'A number, or leave it empty.';
      if (p.session_fee != null && p.session_fee < 0) bad['price.session_fee'] = 'Zero or more; a fee cannot be negative.';
      for (const k of ['per_kwh', 'per_minute']) if (typeof p[k] === 'number' && (p[k] < -10 || p[k] > 100)) bad[`price.${k}`] = 'Between −10 and 100.';
      p.rates.forEach((r, i) => {
        if (!(r.name || '').trim()) bad[`price.rates.${i}.name`] = 'Name the rate.';
        if (hm(r.from) == null) bad[`price.rates.${i}.from`] = 'A time such as 07:00.';
        if (hm(r.to) == null) bad[`price.rates.${i}.to`] = 'A time such as 21:00, or 24:00.';
        if (r.from && r.from === r.to) bad[`price.rates.${i}.to`] = 'Must differ from the start.';
        if (r.per_kwh == null && r.per_minute == null) bad[`price.rates.${i}.${billing === 'minute' ? 'per_minute' : 'per_kwh'}`] = 'Give the rate a price.';
      });
      if (p.per_kwh == null && p.per_minute == null && !p.rates.length && p.session_fee == null) bad[billing === 'minute' ? 'price.per_minute' : 'price.per_kwh'] = 'Give a price or a session fee, or switch the price off.';
    }
    return bad;
  }
  function showErrors(fields) {
    const unplaced = [];
    for (const f of [...fieldsByPath.values(), ...priceFields.values()]) setFieldError(f, null);
    for (const [path, msg] of Object.entries(fields)) {
      const f = fieldsByPath.get(path) || priceFields.get(path);
      if (f) setFieldError(f, String(msg)); else unplaced.push(String(msg));
    }
    topError.hidden = !Object.keys(fields).length;
    replace(topError, Object.keys(fields).length ? `Not saved yet: ${unplaced.length ? unplaced.join(' ') : 'fix the fields marked below.'}` : '');
    const first = formEl.querySelector('[aria-invalid="true"]');
    if (first) first.focus();
  }
  async function save() {
    if (!editable) return;
    const bad = check();
    if (Object.keys(bad).length) { showErrors(bad); return; }
    showErrors({});
    const all = currentBody();
    let body = all;
    if (cur) {
      body = { version: cur.version };
      const was = bodyOf(saved);
      for (const k of Object.keys(all)) if (JSON.stringify(all[k]) !== JSON.stringify(was[k])) body[k] = all[k];
      if (Object.keys(body).length === 1) { ctx.flash('Nothing to save: the place is as it was.', { kind: 'info' }); return; }
    }
    saveBtn.disabled = true;
    const label = saveBtn.textContent;
    saveBtn.textContent = 'Saving…';
    let res;
    try {
      res = cur ? await ctx.api.patch(api.apiPath`places/${cur.id}`, body) : await ctx.api.post('places', body);
    } catch (err) {
      saveBtn.disabled = false; saveBtn.textContent = label;
      if (!ctx.alive()) return;
      if (err && err.code === 'stale_version') {
        topError.hidden = false;
        replace(topError, 'Not saved: someone changed this place after you opened it. ', h('a', { href: editUrl(cur.id) }, 'Open the latest version'), ' (your changes here are dropped).');
        return;
      }
      if (err && err.status === 404) { topError.hidden = false; replace(topError, 'Not saved: this place was deleted meanwhile. ', h('a', { href: listUrl() }, 'All places')); return; }
      if (err && err.fields) { showErrors(err.fields); return; }
      topError.hidden = false; replace(topError, `Not saved: ${(err && err.message) || 'the server did not answer'}.`);
      return;
    }
    dirty = false;
    const e = res.effect || {};
    const gained = [e.drives_named ? plural(e.drives_named, 'drive') : null, e.charges_named ? plural(e.charges_named, 'charge') : null].filter(Boolean);
    const priceChanged = cur && 'price' in body && res.price;
    const msg = `${cur ? 'Saved' : 'Added'} ${res.name}.${gained.length ? ` ${gained.join(' and ')} now show its name.` : ''}${priceChanged && e.charges_repriced ? ` ${plural(e.charges_repriced, 'past charge')} would get a new cost: reprice them below if you want.` : ''}`;
    ctx.flash(msg, { next: true });
    const target = priceChanged ? editUrl(res.id) : listUrl();
    // the same address does not mount the page again by itself: the editor must show the saved place
    if (target === location.pathname + location.search) remount(); else ctx.go(target);
  }
  const formEl = h('form', { class: 'pl-form', novalidate: true, cover: 'web.geofences.form.validation', 'aria-label': 'The place' });
  formEl.addEventListener('submit', (e) => { e.preventDefault(); save(); });

  /* ---------------------------------------------------------------- layout */
  formEl.append(...[
    topError,
    !editable ? h('p', { class: 'note pl-warn' }, icon('info'), 'Your account can look at this place but not change it.') : null,
    section({ title: 'The place', cls: 'pl-block-place' },
      h('div', { class: 'pl-fields' }, fName, fKind),
      startNote),
    section({ title: 'Where it is', note: 'centre and radius', cls: 'pl-block-where' },
      h('div', { class: 'pl-fields pl-fields-3' }, fLat, fLon, fRadius), zoneNote),
    section({ title: 'Charging price', cls: 'pl-block-price' }, priceSwitch, priceBody, autoCost),
    actions].filter(Boolean));
  const aside = h('div', { class: 'pl-aside' },
    h('div', { class: 'pl-map-wrap', cover: 'web.geofences.form.position-map' }, mapBox, mapNote),
    section({ title: 'Which place wins', cls: 'pl-match', cover: 'web.geofences.form.match-rule' }, matchBox),
    section({ title: 'Effect on history', note: 'before you save', cls: 'pl-effect', cover: 'web.geofences.form.relabel-history' }, effectBox));
  ctx.main.append(head, h('div', { class: 'pl-editor' }, formEl, aside),
    section({ title: 'Reprice past charges', cls: 'pl-reprice', id: 'pl-reprice', cover: cur ? 'web.geofences.form.retro-costs' : null }, repriceBox),
    usedBy);
  touched(false);
  const mapDone = drawMap().catch(() => null);
  await runPreview();
  await Promise.race([mapDone.then(() => m && m.ready), new Promise((r) => setTimeout(r, 4000))]);
  ctx.ready();
  return {};
}
