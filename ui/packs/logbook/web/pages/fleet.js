// Sentry Mate, "Logbook" UI pack: Fleet (/), the register of every car.
//
// Data: GET /api/v1/fleet/summary (counts per state), /fleet/attention (cars that need a look and why),
// /cars (the list: search, state, sort, cursor paging, CSV), /setup/status (why no car shows, for the empty
// state), /me/preferences (pinned cars and the person's own order). Live: `now` events update a car's row
// in place, `fleet` events (a car added, removed or renamed) load the list again.

import { h } from '../shared/dom.js';
import { formatNumber, tn } from '../shared/i18n.js';
import { format, rangeValue, rangeLabel } from '../shared/units.js';
import { formatTime, formatDate, dayKey } from '../shared/time.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { pageHead, stateMark, stateLabel, ago, batteryBar, icon, empty, flag, sep, q, errorWords, button } from '../core/ui.js';
import { sourceMark } from '../core/sources.js';
import { dataTable } from '../core/table.js';
import { currentPrefs } from '../core/prefs.js';
import { savePref } from '../core/frame.js';

export const id = 'fleet';
export const css = true;
export const title = () => 'Fleet';

const zone = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC'; } catch { return 'UTC'; } })();
const STATES = [['all', 'All cars'], ['driving', 'Driving'], ['charging', 'Charging'], ['parked', 'Parked'], ['asleep', 'Asleep'], ['unknown', 'Unknown']];
const SORTS = [['custom', 'Your order: pinned first'], ['last_activity', 'Last activity, newest first'], ['name', 'Name, A to Z'], ['soc', 'Battery, highest first']];
/** Each order's first direction, and its words both ways (the column heads and the Order menu are one control). */
const SORT_DIR = { last_activity: 'desc', name: 'asc', soc: 'desc' };
const SORT_WORDS = {
  last_activity: { desc: 'Last activity, newest first', asc: 'Last activity, oldest first' },
  name: { asc: 'Name, A to Z', desc: 'Name, Z to A' },
  soc: { desc: 'Battery, highest first', asc: 'Battery, lowest first' },
};

/** The sentence for an attention item, from its code and detail (metric values, shown in the person's units). */
function attentionText(a, tz) {
  const d = a.detail || {};
  switch (a.code) {
    case 'stopped_mid_drive': return `Data stopped in the middle of a drive${d.last_data_ms ? `, at ${formatTime(d.last_data_ms, tz)}` : ''}.`;
    case 'stopped_mid_charge': return `Data stopped in the middle of a charge${d.last_data_ms ? `, at ${formatTime(d.last_data_ms, tz)}` : ''}.`;
    case 'upload_silence': return `No upload since ${d.last_upload_ms ? `${formatDate(d.last_upload_ms, tz, { now: api.now() })}, ${formatTime(d.last_upload_ms, tz)}` : 'a long time'}.`;
    case 'trust_failed': return `The last log failed its check${d.reason ? ` (${String(d.reason).replace(/_/g, ' ')})` : ''}; its data is not used until it is trusted.`;
    case 'low_battery': return `Battery low: ${format(d.soc_pct, 'percent')}.`;
    case 'tyre_low': return `${({ fl: 'Front left', fr: 'Front right', rl: 'Rear left', rr: 'Rear right' })[d.tyre] || 'A'} tyre at ${format(d.pressure_kpa, 'pressure')}, below the ${format(d.recommended_kpa, 'pressure')} the car recommends.`;
    case 'door_open_asleep': return `${d.door ? `${String(d.door).replace(/_/g, ' ')} ` : 'A door '}open while the car sleeps.`;
    default: return String(a.code).replace(/_/g, ' ');
  }
}
/** Where an attention item is explained in full. */
const ATTENTION_PAGE = { stopped_mid_drive: 'drives', stopped_mid_charge: 'charges', upload_silence: 'health', trust_failed: 'health', low_battery: 'car', tyre_low: 'car', door_open_asleep: 'car' };
const FIX_WORDS = {
  no_token: ['No upload token yet', 'Make an upload token for the car in Settings: Cars and put it in the CAN Bridge app.'],
  token_unused: ['A token was made but never used', 'Check that the CAN Bridge app has the token and can reach this server.'],
  key_waiting: ['A CAN Bridge key is waiting', 'A CAN Bridge uploaded logs with a key no car has yet. Assign it to a car in Settings: Cars.'],
  logs_waiting_trust: ['Logs are waiting for trust', 'Uploaded logs wait until their key is trusted. Trust the key in Settings: Cars.'],
  import_running: ['An import is running', 'Cars from the TeslaMate import appear when it is done. See Import.'],
};

export async function mount(ctx) {
  const prefs = currentPrefs() || {};
  const st = {
    q: ctx.query.q || '',
    state: STATES.some(([k]) => k === ctx.query.state) ? ctx.query.state : 'all',
    sort: SORTS.some(([k]) => k === ctx.query.sort) ? ctx.query.sort : 'custom',
  };
  st.dir = st.sort !== 'custom' && (ctx.query.dir === 'asc' || ctx.query.dir === 'desc') ? ctx.query.dir : SORT_DIR[st.sort] || null;
  const isAdmin = ctx.me?.role === 'admin';
  const pinned = new Set(prefs.pinned_cars || []);
  // The totals are the page; the "Needs a look" panel is extra, so its failure stays in the panel.
  const attnFirst = ctx.api.get('fleet/attention', { limit: 20 }).then((r) => ({ ok: r }), (err) => ({ err }));
  const summary = await ctx.api.get('fleet/summary');

  if (!summary.cars) return emptyFleet(ctx, isAdmin);

  // Tallies: counts per state; a click shows only those cars.
  const by = summary.by_state || {};
  const tallies = h('div', { class: 'tallies', role: 'group', 'aria-label': 'Show cars by state' },
    STATES.map(([k, name]) => h('button', {
      type: 'button', class: ['tally', `tally-${k}`], 'aria-pressed': String(st.state === k), dataset: { state: k },
      onclick: () => { st.state = k; syncTallies(); write(); reload(); },
    },
    h('span', { class: 'tally-count', dataset: { count: k } }, formatNumber(k === 'all' ? summary.cars : by[k] || 0)),
    h('span', { class: 'tally-name' }, k === 'all' ? null : h('span', { class: ['state-dot', `state-${k}`], 'aria-hidden': 'true' }), name,
      k === 'all' ? h('span', { class: 'tally-note', dataset: { stale: '' } }, summary.stale ? `${formatNumber(summary.stale)} stale` : 'none stale') : null,
      k === 'unknown' ? h('span', { class: 'tally-note' }, 'no recent log') : null))));
  function drawSummary(sm) {
    const b = sm.by_state || {};
    for (const el of tallies.querySelectorAll('[data-count]')) el.textContent = formatNumber(el.dataset.count === 'all' ? sm.cars : b[el.dataset.count] || 0);
    const stale = tallies.querySelector('[data-stale]');
    if (stale) stale.textContent = sm.stale ? `${formatNumber(sm.stale)} stale` : 'none stale';
    summary.cars = sm.cars;
    deckLine.textContent = deckText();
  }
  const syncTallies = () => tallies.querySelectorAll('.tally').forEach((b) => b.setAttribute('aria-pressed', String(b.dataset.state === st.state)));

  // Attention: which car needs a look and why, each linked to the page that shows the cause. Times are in the
  // car's own zone once the car is known (from the list, or one GET per car not on the first page).
  const carInfo = new Map(); // car id -> { name, tz }
  const attnItems = [];
  let attnNext = null;
  let attnError = null;
  const attnList = h('ul', { class: 'attention-list' });
  const attnCount = h('span', { class: 'label-note' });
  const attnMore = h('button', { type: 'button', class: ['button', 'button-quiet', 'button-small', 'attention-more'], onclick: moreAttention }, 'Show more');
  const attnEmpty = h('p', { class: 'note' }, 'Every car is uploading, its logs pass their checks, and no tyre, door or battery needs a look.');
  const attnStatus = h('div', { class: 'attention-status' });
  const attentionBox = h('section', { class: 'attention', 'aria-labelledby': 'attention-title' },
    h('h2', { class: 'label', id: 'attention-title' }, 'Needs a look', attnCount),
    attnList, attnEmpty, attnStatus, attnMore);
  function attentionRow(a) {
    const info = carInfo.get(a.car);
    const tz = info?.tz || zone;
    const page = ATTENTION_PAGE[a.code] || 'car';
    return h('li', { class: 'attention-item', dataset: { car: a.car } },
      h('span', { class: 'attention-car' }, info?.name || a.car),
      h('span', { class: 'attention-text' }, attentionText(a, tz),
        a.since_ms ? h('span', { class: 'attention-since' }, ' Since ', ago(a.since_ms), '.') : null),
      h('a', { class: 'link-arrow', href: state.buildUrl(page, { car: a.car, period: page === 'car' ? null : state.current().period }) }, 'See why', icon('arrowRight')));
  }
  function drawAttention() {
    attnList.replaceChildren(...attnItems.map(attentionRow));
    attnCount.textContent = attnError ? 'not known' : attnItems.length ? `${tn(attnItems.length, '{n} item', '{n} items')}${attnNext ? ' shown, more below' : ''}` : 'nothing';
    attnList.hidden = !attnItems.length;
    attnEmpty.hidden = !!attnItems.length || !!attnError;
    attnMore.hidden = !attnNext || !!attnError;
    attnStatus.replaceChildren(...(attnError ? [partError(attnError, 'Could not check which cars need a look', retryAttention)] : []));
    deckLine.textContent = deckText();
  }
  /** The head's line: the cars, and how many need a look (at least that many while more items are to load). */
  function deckText() {
    const cars = tn(summary.cars, '{n} car', '{n} cars');
    if (attnError) return `${cars}. Which need a look is not known right now.`;
    const n = new Set(attnItems.map((a) => a.car)).size;
    if (!n) return `${cars}. None needs a look.`;
    if (attnNext) return `${cars}. At least ${tn(n, '{n} needs a look', '{n} need a look')}; more are listed below.`;
    return `${cars}. ${tn(n, '{n} needs a look.', '{n} need a look.')}`;
  }
  function takeAttention(res) {
    if (res.err) { attnError = res.err; attnItems.length = 0; attnNext = null; } else { attnError = null; attnItems.splice(0, attnItems.length, ...res.ok.items); attnNext = res.ok.next || null; }
    drawAttention();
  }
  async function retryAttention() {
    takeAttention(await ctx.api.get('fleet/attention', { limit: 20 }).then((r) => ({ ok: r }), (err) => ({ err })));
    learnCars();
  }
  async function moreAttention() {
    if (!attnNext) return;
    attnMore.disabled = true;
    try {
      const r = await ctx.api.get('fleet/attention', { limit: 20, after: attnNext });
      attnItems.push(...r.items); attnNext = r.next || null;
      drawAttention(); learnCars();
    } catch (err) { ctx.flash(`More cars could not be loaded: ${err.message || err}`, { kind: 'error' }); } finally { attnMore.disabled = false; }
  }
  function learnCars() {
    for (const a of attnItems) {
      if (carInfo.has(a.car)) continue;
      carInfo.set(a.car, null);
      ctx.api.get(api.apiPath`cars/${a.car}`).then((c) => { carInfo.set(c.id, { name: c.name, tz: c.time_zone }); drawAttention(); }).catch(() => {});
    }
  }
  const deckLine = h('span', { class: 'fleet-deck' });
  takeAttention(await attnFirst);

  // Search and order.
  const search = h('input', { class: 'field-input', type: 'search', id: 'fleet-search', value: st.q, placeholder: 'Name, model or VIN prefix', autocomplete: 'off', spellcheck: 'false' });
  let timer = 0;
  search.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(() => { st.q = search.value.trim(); write(); reload(); }, 200); });
  ctx.onCleanup(() => clearTimeout(timer));
  const sortSel = h('select', { class: 'field-select', id: 'fleet-sort', cover: ['web.settings.general.car-order', 'faq.reorder-cars', 'cars.list.order'] },
    SORTS.map(([v, t]) => h('option', { value: v, selected: v === st.sort }, t)));
  sortSel.addEventListener('change', () => { st.sort = sortSel.value; st.dir = SORT_DIR[st.sort] || null; syncSort(); write(); reload(); });
  /** The menu says the order the rows are in, whichever way it was chosen (the menu or a column head). */
  function syncSort() {
    sortSel.value = st.sort;
    for (const o of sortSel.options) if (SORT_WORDS[o.value]) o.textContent = SORT_WORDS[o.value][o.value === st.sort ? st.dir : SORT_DIR[o.value]];
  }
  syncSort();
  const toolbar = h('div', { class: 'toolbar' },
    h('div', { class: 'field field-search' },
      h('label', { class: 'field-label', for: 'fleet-search' }, 'Find a car'),
      h('div', { class: 'field-control' }, icon('search', 'icon field-icon'), search)),
    h('div', { class: 'toolbar-end' },
      h('div', { class: 'field' }, h('label', { class: 'field-label', for: 'fleet-sort' }, 'Order'), sortSel),
      h('p', { class: 'note toolbar-note' }, 'Pin a car with its star. In your order, the arrows move a car among the pinned or the other cars; the order is kept with your account. A column head orders by that column.')));

  const write = () => ctx.setQuery({ q: st.q, state: st.state === 'all' ? null : st.state, sort: st.sort === 'custom' ? null : st.sort,
    dir: st.sort !== 'custom' && st.dir !== SORT_DIR[st.sort] ? st.dir : null });
  // Your order is the server's `custom` sort; any other order is a column's sort (the table sends sort and dir).
  const params = () => ({ search: st.q || undefined, state: st.state === 'all' ? undefined : st.state, sort: st.sort === 'custom' ? 'custom' : undefined });
  const tableSort = () => (st.sort === 'custom' ? null : { key: st.sort, dir: st.dir });
  /** A column head was clicked: that is the order now, in the menu and the URL; the move arrows go away. */
  function headSort(s) { st.sort = s.key; st.dir = s.dir; syncSort(); write(); }

  // The person's order: pinned cars first (in pinned_cars order), then car_order, then the rest by last activity.
  // A car moves only within its own group, and only in your order (the rows are then the server's custom order).
  async function togglePin(car) {
    const list = [...(currentPrefs()?.pinned_cars || [])];
    const i = list.indexOf(car.id);
    if (i >= 0) list.splice(i, 1); else list.unshift(car.id);
    try { await savePref({ pinned_cars: list }); ctx.flash(i >= 0 ? `${car.name} is no longer pinned.` : `${car.name} is pinned to the top.`); } catch { return; }
    pinned.clear(); list.forEach((x) => pinned.add(x));
    if (st.sort === 'custom') reload(); else redrawRows();
  }
  /** The loaded car next to this one in its own group (pinned or not), or null. */
  function neighbour(car, step) {
    if (st.sort !== 'custom') return null;
    const list = table.items || [];
    const i = list.findIndex((c) => c.id === car.id);
    const o = i < 0 ? null : list[i + step];
    return o && pinned.has(o.id) === pinned.has(car.id) ? o : null;
  }
  async function move(car, step) {
    const other = neighbour(car, step);
    if (!other) return;
    const p = currentPrefs() || {};
    const group = pinned.has(car.id);
    let key, order;
    if (group) {
      key = 'pinned_cars'; order = [...(p.pinned_cars || [])];
    } else {
      // Cars not yet in car_order follow it in the order shown; adding them at its end keeps that order.
      key = 'car_order'; order = [...(p.car_order || [])];
      for (const c of table.items) if (!pinned.has(c.id) && !order.includes(c.id)) order.push(c.id);
    }
    const a = order.indexOf(car.id), b = order.indexOf(other.id);
    if (a < 0 || b < 0) return;
    [order[a], order[b]] = [order[b], order[a]];
    try { await savePref({ [key]: order }); } catch { return; }
    if (group) { pinned.clear(); order.forEach((x) => pinned.add(x)); }
    ctx.flash(`${nameOf(car)} moved ${step < 0 ? 'up' : 'down'}, ${step < 0 ? 'above' : 'below'} ${nameOf(other)}.`);
    reload();
  }

  const rows = new Map(); // car id -> row element
  const nameOf = (car) => car.name || 'Unnamed car';
  const columns = [
    { key: 'name', label: 'Car', sort: 'name', firstDir: 'asc', th: true, cls: 'c-car', value: (c) => c.name,
      cell: (c) => [
        h('a', { class: 'cell-main car-name', href: state.buildUrl('car', { car: c.id, period: null }) }, nameOf(c)),
        h('span', { class: 'cell-sub' }, [c.year, c.model, c.trim].filter(Boolean).join(' '), sep(), h('span', { class: 'mono' }, c.vin_prefix || c.id)),
        sourceMark(c, { block: true, short: true, cls: 'fleet-src' }),
        c.name ? null : h('span', { class: 'cell-sub' }, h('a', { href: state.buildUrl('settings-cars', { car: c.id }) }, 'Give it a name'))] },
    { key: 'state', label: 'State', cls: 'c-state', value: (c) => c.now?.state,
      cell: (c) => [stateMark(c.now?.state, { stale: !!c.now?.stale }), h('span', { class: 'cell-sub' }, sinceText(c))] },
    { key: 'soc', label: 'Battery', sort: 'soc', firstDir: 'desc', cls: 'c-battery', value: (c) => c.now?.soc_pct,
      cell: (c) => [h('span', { class: 'batt-row' }, q(c.now?.soc_pct, 'percent', { why: blankWhy(c) }), c.now?.soc_pct != null ? batteryBar(c.now.soc_pct, { cls: 'bar-cell' }) : null),
        h('span', { class: 'cell-sub' }, rangeText(c))] },
    { key: 'place', label: 'Where', cls: 'c-where', value: (c) => c.now?.place?.name,
      cell: (c) => [h('span', { class: 'place' }, c.now?.place ? c.now.place.name : c.now?.state === 'driving' ? 'On the road' : 'No saved place'),
        h('span', { class: 'cell-sub' }, c.time_zone ? `${c.time_zone.split('/').pop().replace(/_/g, ' ')} time` : '')] },
    { key: 'plug', label: 'Charging', cls: 'c-plug', value: (c) => c.now?.charge_power_kw,
      cell: (c) => (c.now?.charge_power_kw != null ? [q(c.now.charge_power_kw, 'power'), h('span', { class: 'cell-sub' }, 'charging now')]
        : c.now?.plugged ? [h('span', { class: 'plug-mark' }, icon('plug', 'icon icon-plug'), ' Plugged in'), c.now?.stale ? h('span', { class: 'cell-sub' }, 'last known') : null]
          : c.now?.plugged == null ? [h('span', { class: 'nil' }, 'Not known'), h('span', { class: 'cell-sub' }, blankWhy(c))]
            : [h('span', { class: 'nil' }, 'Not plugged in'), c.now?.stale ? h('span', { class: 'cell-sub' }, 'last known') : null]) },
    { key: 'last_activity', label: 'Data age', sort: 'last_activity', firstDir: 'desc', cls: 'c-seen', value: (c) => c.last_activity_ms,
      cell: (c) => [h('span', { class: 'seen-line' }, ago(c.now?.at_ms ?? c.last_activity_ms, { stale: !!c.now?.stale }), c.now?.stale ? flag('Stale', { kind: 'warn', title: 'No upload for more than an hour' }) : null),
        h('span', { class: 'cell-sub' }, c.now?.received_ms ? `received ${formatTime(c.now.received_ms, c.time_zone)}` : '')] },
    { key: 'order', label: 'Order', cls: 'c-order',
      cell: (c) => h('span', { class: 'order-tools' },
        h('button', { type: 'button', class: ['icon-button', 'pin-button', { 'is-on': pinned.has(c.id) }], 'aria-pressed': String(pinned.has(c.id)), title: pinned.has(c.id) ? 'Unpin' : 'Pin to the top', 'aria-label': `${pinned.has(c.id) ? 'Unpin' : 'Pin'} ${nameOf(c)}`, onclick: () => togglePin(c) }, icon('star')),
        st.sort === 'custom' ? [
          moveButton(c, -1), moveButton(c, 1)] : null) },
  ];
  function moveButton(c, step) {
    const up = step < 0;
    const other = neighbour(c, step);
    const why = other ? null : `No ${pinned.has(c.id) ? 'pinned' : 'unpinned'} car ${up ? 'above' : 'below'} it in the list`;
    return h('button', { type: 'button', class: 'icon-button', disabled: !other, title: why || (up ? `Move up, above ${nameOf(other)}` : `Move down, below ${nameOf(other)}`),
      'aria-label': `Move ${nameOf(c)} ${up ? 'up' : 'down'}`, onclick: () => move(c, step) }, icon(up ? 'chevronLeft' : 'chevronRight', `icon icon-${up ? 'up' : 'down'}`));
  }
  /** The range under the battery, named rated or ideal as the person chose. */
  function rangeText(c) {
    const v = rangeValue(c.now, 'range_km');
    return v != null ? `${format(v, 'range')} ${rangeLabel().toLowerCase()}` : `${rangeLabel().toLowerCase()} not known`;
  }
  /** Why a value is blank: a sleeping car sends nothing new; unknown means no log reached Sentry Mate. */
  function blankWhy(c) {
    const s = c.now?.state;
    if (!c.now || s === 'unknown') return 'no recent log';
    if (s === 'asleep') return 'asleep, not sent';
    return 'not sent by this car';
  }
  function sinceText(c) {
    const n = c.now || {};
    if (n.stale) return 'last known';
    if (!n.since_ms) return '';
    const t = c.time_zone;
    return `since ${dayKey(n.since_ms, t) === dayKey(api.now(), t) ? formatTime(n.since_ms, t) : formatDate(n.since_ms, t, { weekday: false, now: api.now() })}`;
  }
  const table = dataTable(ctx, {
    path: 'cars', params: params(), sort: tableSort(), onSort: headSort, limit: 50, columns, itemName: ['car', 'cars'], cls: 'fleet-ledger', label: 'Cars',
    cover: ['cars.list.cards', 'faq.null-panels', 'cars.list.live_refresh'],
    rowAttrs: (c) => ({ class: ['ledger-row', { 'is-stale': !!c.now?.stale, 'is-unknown': c.now?.state === 'unknown' }], dataset: { car: c.id } }),
    onRows: (items) => { for (const c of items) carInfo.set(c.id, { name: c.name, tz: c.time_zone }); drawAttention(); },
    empty: () => empty({
      title: st.q ? `No car matches “${st.q}”${st.state !== 'all' ? ` among ${stateLabel(st.state).toLowerCase()} cars` : ''}.` : `No car is ${stateLabel(st.state).toLowerCase()} right now.`,
      text: st.q ? 'Try part of a name, a model or a VIN prefix.' : 'Choose another state above to see more cars.',
      action: h('button', { type: 'button', class: 'button', onclick: () => { st.q = ''; search.value = ''; st.state = 'all'; syncTallies(); write(); reload(); } }, 'Show all cars'),
    }),
  });
  function reload() { return table.reload(params(), { sort: tableSort() }); }
  function redrawRows() { table.redraw(); }

  const asof = h('p', { class: 'asof' }, h('span', { class: 'asof-dot', 'aria-hidden': 'true' }), 'Live', h('span', { class: 'asof-note' }, 'values reach Sentry Mate about a minute after the car sends them'));
  ctx.main.append(
    h('div', { class: 'fleet-top' },
      pageHead({ eyebrow: 'The register', title: 'Fleet', deck: deckLine, aside: asof, cls: 'page-head-fleet' }),
      tallies),
    attentionBox,
    toolbar,
    table.el);
  await table.load();
  const missing = missingCars(ctx, isAdmin);
  ctx.main.append(missing);
  await missing.done;
  // Names and zones for the attention list, when those cars are not on the first page.
  learnCars();

  // Live: a car's row follows its `now` events; a car added, removed or renamed loads the list again.
  let again = 0, sumTimer = 0;
  ctx.onCleanup(() => { clearTimeout(again); clearTimeout(sumTimer); });
  // The totals follow the cars: a state change asks for the counts again (once per second at most).
  function refreshSummary() {
    clearTimeout(sumTimer);
    sumTimer = setTimeout(() => ctx.api.get('fleet/summary').then(drawSummary).catch(() => {}), 1000);
  }
  ctx.events(['now', 'fleet'], (ev) => {
    if (ev.type === 'fleet') { clearTimeout(again); again = setTimeout(reload, 500); refreshSummary(); return; }
    const d = ev.data || {};
    const car = table.items.find((c) => c.id === d.car);
    if (!car || !d.now) return;
    const n = d.now;
    const v = (x) => (x && typeof x === 'object' && 'value' in x ? x.value : x);
    const now = { ...car.now };
    const before = now.state, staleBefore = !!now.stale;
    // a pair that comes with only a newer at_ms kept its value: leave it as it is
    const has = (o, k) => k in o && !(o[k] && typeof o[k] === 'object' && !Array.isArray(o[k]) && !('value' in o[k]));
    for (const k of ['state', 'since_ms', 'at_ms', 'received_ms', 'stale', 'soc_pct', 'range_km', 'ideal_range_km']) if (has(n, k)) now[k] = v(n[k]);
    if (n.location && 'place' in n.location) now.place = v(n.location.place);
    if (n.charge) {
      if (has(n.charge, 'plugged')) now.plugged = v(n.charge.plugged);
      if (has(n.charge, 'power_kw')) now.charge_power_kw = v(n.charge.power_kw);
    }
    if (now.state !== 'charging') now.charge_power_kw = null;
    if (now.state !== before || !!now.stale !== staleBefore) refreshSummary();
    car.now = now;
    table.redrawRow(car);
  });
  ctx.ready();
  // New units: the rows are drawn again in them (the page has no period).
  return { update: (c) => { table.redraw(); c.ready(); } };
}

/** A failed part of the page (the rest still shows): what failed, why, and Try again. */
function partError(err, what, retry) {
  const w = errorWords(err);
  const generic = !err || !err.status || err.status >= 500;
  return h('div', { class: 'part-error', role: 'alert' },
    h('p', { class: 'part-error-title' }, what, '.'),
    h('p', { class: 'note' }, generic && err?.status !== 0 ? `The server did not answer this part${err?.message ? ` (${err.message})` : ''}; the rest of the page is up to date.` : `${w.title} ${w.text}`),
    h('p', { class: 'part-error-action' }, button('Try again', { onclick: retry, iconName: 'reset', kind: 'quiet' })));
}

/** The fixes set-up lists (GET /api/v1/setup/status), each linked to where it is fixed, or for a watch account who to ask. */
function fixList(fixes, isAdmin) {
  return h('ul', { class: 'fix-list' }, fixes.map((f) => h('li', null,
    h('p', { class: 'fix-title' }, (FIX_WORDS[f.cause] || [String(f.cause).replace(/_/g, ' ')])[0]),
    h('p', { class: 'fix-text' }, (FIX_WORDS[f.cause] || ['', ''])[1]),
    isAdmin
      ? (f.link ? h('a', { class: 'link-arrow', href: f.link }, 'Fix it', icon('arrowRight')) : null)
      : h('p', { class: 'note fix-ask' }, 'Ask an admin of this Sentry Mate to fix this; a watch account cannot change set-up.'))));
}

/**
 * Loads set-up's status into `box` with `draw(fixes)`; a failure says the status is not known, with Try again,
 * never an all-clear or advice that may not apply.
 */
function loadStatus(ctx, box, draw) {
  const run = () => {
    box.replaceChildren(h('p', { class: 'note' }, 'Checking set-up…'));
    return ctx.api.get('setup/status').then((s) => box.replaceChildren(...[].concat(draw((s && s.fixes) || [], s || {}))),
      (err) => { if (!ctx.signal.aborted) box.replaceChildren(partError(err, 'Could not check why a car might be missing', run)); });
  };
  box.done = run();
  return box;
}

/** Why a car may be missing although others show: what waits in set-up. */
function missingCars(ctx, isAdmin) {
  const status = loadStatus(ctx, h('div', { class: 'missing-status' }), (fixes) => (fixes.length ? fixList(fixes, isAdmin)
    : h('p', { class: 'note-serif' }, 'Nothing is waiting: every upload token was used and every CAN Bridge key belongs to a car.')));
  const el = h('section', { class: 'block missing-cars', 'aria-labelledby': 'missing-title', cover: ['cars.empty.title', 'cars.empty.hint'] },
    h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'missing-title' }, 'Missing a car?')),
    h('p', { class: 'note' }, 'A car appears here when its SMO (CAN Bridge) uploads its first log, or once a Tesla account is added and its car matched (Settings: Integrations); you name it afterwards.'),
    status);
  el.done = status.done;
  return el;
}

/** The Tesla account as another way in: how many are added, and where to add or match one. */
function teslaLine(s, isAdmin) {
  const n = s && s.tesla_accounts;
  if (n == null) return isAdmin ? null : h('p', { class: 'empty-text fleet-tesla' }, 'An admin can also add a Tesla account, so the cars are read from the Tesla API.');
  if (n > 0) return h('p', { class: 'empty-text fleet-tesla' }, `${n === 1 ? 'One Tesla account is' : `${n} Tesla accounts are`} added: a car shows once it is matched there. `,
    isAdmin ? h('a', { class: 'link-arrow', href: '/settings/integrations#tesla' }, 'Match its cars', icon('arrowRight')) : null);
  return isAdmin ? h('p', { class: 'empty-action fleet-tesla' }, h('a', { class: 'button button-quiet', href: '/settings/integrations#tesla' }, 'Or add a Tesla account in Settings: Integrations')) : null;
}

/** No car shows: say exactly why, and link the fix (for an admin). */
async function emptyFleet(ctx, isAdmin) {
  const status = h('div', { class: 'missing-status' });
  ctx.main.append(
    pageHead({ eyebrow: 'The register', title: 'Fleet', deck: 'No car shows yet.' }),
    h('section', { class: 'empty fleet-empty', cover: ['cars.empty.title', 'cars.empty.hint'] },
      h('p', { class: 'empty-title' }, 'No car is being logged yet.'),
      h('p', { class: 'empty-text' }, 'A car appears here when its SMO (CAN Bridge) uploads its first log, or, as a second way, when a Tesla account is added and its car matched: then the Tesla API is read for it. You name it afterwards.'),
      loadStatus(ctx, status, (fixes, s) => [fixes.length ? fixList(fixes, isAdmin)
        : isAdmin ? h('p', { class: 'empty-action' }, h('a', { class: 'button', href: '/settings/cars' }, 'Make an upload token in Settings: Cars'))
          : h('p', { class: 'empty-text' }, 'Ask an admin of this Sentry Mate to make an upload token for the car.'),
        teslaLine(s, isAdmin)])));
  await status.done;
  ctx.ready();
  return {};
}
