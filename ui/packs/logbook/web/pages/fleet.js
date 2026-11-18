// Sentry Mate, "Logbook" UI pack: Fleet (/), the register of every car.
//
// Data: GET /api/v1/fleet/summary (counts per state), /fleet/attention (cars that need a look and why),
// /cars (the list: search, state, sort, cursor paging, CSV), /setup/status (why no car shows, for the empty
// state), /me/preferences (pinned cars and the person's own order). Live: `now` events update a car's row
// in place, `fleet` events (a car added, removed or renamed) load the list again.

import { h } from '../shared/dom.js';
import { formatNumber, tn } from '../shared/i18n.js';
import { format } from '../shared/units.js';
import { formatTime, formatDate, dayKey } from '../shared/time.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { pageHead, stateMark, stateLabel, ago, batteryBar, icon, empty, flag, sep, q } from '../core/ui.js';
import { dataTable } from '../core/table.js';
import { currentPrefs } from '../core/prefs.js';
import { savePref } from '../core/frame.js';

export const id = 'fleet';
export const css = true;
export const title = () => 'Fleet';

const zone = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC'; } catch { return 'UTC'; } })();
const STATES = [['all', 'All cars'], ['driving', 'Driving'], ['charging', 'Charging'], ['parked', 'Parked'], ['asleep', 'Asleep'], ['unknown', 'Unknown']];
const SORTS = [['custom', 'Your order: pinned first'], ['last_activity', 'Last activity, newest first'], ['name', 'Name, A to Z'], ['soc', 'Battery, highest first']];

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
  const pinned = new Set(prefs.pinned_cars || []);
  const [summary, attention] = await Promise.all([ctx.api.get('fleet/summary'), ctx.api.get('fleet/attention', { limit: 20 })]);

  if (!summary.cars) return emptyFleet(ctx);

  // Tallies: counts per state; a click shows only those cars.
  const by = summary.by_state || {};
  const tallies = h('div', { class: 'tallies', role: 'group', 'aria-label': 'Show cars by state' },
    STATES.map(([k, name]) => h('button', {
      type: 'button', class: ['tally', `tally-${k}`], 'aria-pressed': String(st.state === k), dataset: { state: k },
      onclick: () => { st.state = k; syncTallies(); write(); reload(); },
    },
    h('span', { class: 'tally-count' }, formatNumber(k === 'all' ? summary.cars : by[k] || 0)),
    h('span', { class: 'tally-name' }, k === 'all' ? null : h('span', { class: ['state-dot', `state-${k}`], 'aria-hidden': 'true' }), name,
      k === 'all' && summary.stale ? h('span', { class: 'tally-note' }, `${formatNumber(summary.stale)} stale`) : null,
      k === 'unknown' ? h('span', { class: 'tally-note' }, 'no data') : null))));
  const syncTallies = () => tallies.querySelectorAll('.tally').forEach((b) => b.setAttribute('aria-pressed', String(b.dataset.state === st.state)));

  // Attention: which car needs a look and why, each linked to the page that shows the cause.
  const carNames = new Map();
  const attentionBox = h('section', { class: 'attention', 'aria-labelledby': 'attention-title' },
    h('h2', { class: 'label', id: 'attention-title' }, 'Needs a look', h('span', { class: 'label-note' }, attention.items.length ? tn(attention.items.length, '{n} item', '{n} items') : 'nothing')),
    attention.items.length
      ? h('ul', { class: 'attention-list' }, attention.items.map((a) => {
        const li = h('li', { class: 'attention-item', dataset: { car: a.car } },
          h('span', { class: 'attention-car' }, a.car),
          h('span', { class: 'attention-text' }, attentionText(a, zone)),
          h('a', { class: 'link-arrow', href: state.buildUrl(ATTENTION_PAGE[a.code] || 'car', { car: a.car, period: ATTENTION_PAGE[a.code] === 'car' ? null : state.current().period }) }, 'See why', icon('arrowRight')));
        return li;
      }))
      : h('p', { class: 'note' }, 'Every car is uploading, its logs pass their checks, and no tyre, door or battery needs a look.'));

  // Search and order.
  const search = h('input', { class: 'field-input', type: 'search', id: 'fleet-search', value: st.q, placeholder: 'Name, model or VIN prefix', autocomplete: 'off', spellcheck: 'false' });
  let timer = 0;
  search.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(() => { st.q = search.value.trim(); write(); reload(); }, 200); });
  const sortSel = h('select', { class: 'field-select', id: 'fleet-sort', cover: ['web.settings.general.car-order', 'faq.reorder-cars', 'cars.list.order'] },
    SORTS.map(([v, t]) => h('option', { value: v, selected: v === st.sort }, t)));
  sortSel.addEventListener('change', () => { st.sort = sortSel.value; write(); reload(); });
  const toolbar = h('div', { class: 'toolbar' },
    h('div', { class: 'field field-search' },
      h('label', { class: 'field-label', for: 'fleet-search' }, 'Find a car'),
      h('div', { class: 'field-control' }, icon('search', 'icon field-icon'), search)),
    h('div', { class: 'toolbar-end' },
      h('div', { class: 'field' }, h('label', { class: 'field-label', for: 'fleet-sort' }, 'Order'), sortSel),
      h('p', { class: 'note toolbar-note' }, 'Pin a car with its star. In your order, the arrows move a car up or down; the order is kept with your account.')));

  const write = () => ctx.setQuery({ q: st.q, state: st.state === 'all' ? null : st.state, sort: st.sort === 'custom' ? null : st.sort });
  const params = () => ({ search: st.q || undefined, state: st.state === 'all' ? undefined : st.state, sort: st.sort, dir: st.sort === 'name' ? 'asc' : undefined });

  // The person's order: pinned first, then car_order. Moving a car writes the whole visible order.
  async function togglePin(car) {
    const list = [...(currentPrefs()?.pinned_cars || [])];
    const i = list.indexOf(car.id);
    if (i >= 0) list.splice(i, 1); else list.unshift(car.id);
    try { await savePref({ pinned_cars: list }); ctx.flash(i >= 0 ? `${car.name} is no longer pinned.` : `${car.name} is pinned to the top.`); } catch { return; }
    pinned.clear(); list.forEach((x) => pinned.add(x));
    if (st.sort === 'custom') reload(); else redrawRows();
  }
  async function move(car, step) {
    const ids = table.items.map((c) => c.id);
    const i = ids.indexOf(car.id), j = i + step;
    if (i < 0 || j < 0 || j >= ids.length) return;
    [ids[i], ids[j]] = [ids[j], ids[i]];
    const rest = (currentPrefs()?.car_order || []).filter((x) => !ids.includes(x));
    try { await savePref({ car_order: [...ids, ...rest] }); } catch { return; }
    ctx.flash(`${car.name} moved ${step < 0 ? 'up' : 'down'}.`);
    reload();
  }

  const rows = new Map(); // car id -> row element
  const nameOf = (car) => car.name || 'Unnamed car';
  const columns = [
    { key: 'name', label: 'Car', sort: 'name', firstDir: 'asc', th: true, cls: 'c-car', value: (c) => c.name,
      cell: (c) => [
        h('a', { class: 'cell-main car-name', href: state.buildUrl('car', { car: c.id, period: null }) }, nameOf(c)),
        h('span', { class: 'cell-sub' }, [c.year, c.model, c.trim].filter(Boolean).join(' '), sep(), h('span', { class: 'mono' }, c.vin_prefix || c.id)),
        c.name ? null : h('span', { class: 'cell-sub' }, h('a', { href: state.buildUrl('settings-cars', { car: c.id }) }, 'Give it a name'))] },
    { key: 'state', label: 'State', cls: 'c-state', value: (c) => c.now?.state,
      cell: (c) => [stateMark(c.now?.state, { stale: !!c.now?.stale }), h('span', { class: 'cell-sub' }, sinceText(c))] },
    { key: 'soc', label: 'Battery', sort: 'soc', firstDir: 'desc', cls: 'c-battery', value: (c) => c.now?.soc_pct,
      cell: (c) => [h('span', { class: 'batt-row' }, q(c.now?.soc_pct, 'percent'), batteryBar(c.now?.soc_pct, { cls: 'bar-cell' })),
        h('span', { class: 'cell-sub' }, format(c.now?.range_km, 'range'), c.now?.range_km != null ? ' range' : '')] },
    { key: 'place', label: 'Where', cls: 'c-where', value: (c) => c.now?.place?.name,
      cell: (c) => [h('span', { class: 'place' }, c.now?.place ? c.now.place.name : c.now?.state === 'driving' ? 'On the road' : 'No saved place'),
        h('span', { class: 'cell-sub' }, c.time_zone ? c.time_zone.split('/').pop().replace(/_/g, ' ') : '')] },
    { key: 'plug', label: 'Charging', cls: 'c-plug', value: (c) => c.now?.charge_power_kw,
      cell: (c) => (c.now?.charge_power_kw != null ? [q(c.now.charge_power_kw, 'power'), h('span', { class: 'cell-sub' }, 'charging now')]
        : c.now?.plugged ? [h('span', { class: 'plug-mark' }, icon('plug', 'icon icon-plug'), ' Plugged in')] : h('span', { class: 'nil' }, 'Not plugged in')) },
    { key: 'last_activity', label: 'Data age', sort: 'last_activity', firstDir: 'desc', cls: 'c-seen', value: (c) => c.last_activity_ms,
      cell: (c) => [h('span', { class: 'seen-line' }, ago(c.now?.at_ms ?? c.last_activity_ms, { stale: !!c.now?.stale }), c.now?.stale ? flag('Stale', { kind: 'warn', title: 'No upload for more than an hour' }) : null),
        h('span', { class: 'cell-sub' }, c.now?.received_ms ? `received ${formatTime(c.now.received_ms, c.time_zone)}` : '')] },
    { key: 'order', label: 'Order', cls: 'c-order',
      cell: (c) => h('span', { class: 'order-tools' },
        h('button', { type: 'button', class: ['icon-button', 'pin-button', { 'is-on': pinned.has(c.id) }], 'aria-pressed': String(pinned.has(c.id)), title: pinned.has(c.id) ? 'Unpin' : 'Pin to the top', 'aria-label': `${pinned.has(c.id) ? 'Unpin' : 'Pin'} ${nameOf(c)}`, onclick: () => togglePin(c) }, icon('star')),
        st.sort === 'custom' ? [
          h('button', { type: 'button', class: 'icon-button', title: 'Move up', 'aria-label': `Move ${nameOf(c)} up`, onclick: () => move(c, -1) }, icon('chevronLeft', 'icon icon-up')),
          h('button', { type: 'button', class: 'icon-button', title: 'Move down', 'aria-label': `Move ${nameOf(c)} down`, onclick: () => move(c, 1) }, icon('chevronRight', 'icon icon-down'))] : null) },
  ];
  function sinceText(c) {
    const n = c.now || {};
    if (n.stale) return 'last known';
    if (!n.since_ms) return '';
    const t = c.time_zone;
    return `since ${dayKey(n.since_ms, t) === dayKey(api.now(), t) ? formatTime(n.since_ms, t) : formatDate(n.since_ms, t, { weekday: false, now: api.now() })}`;
  }
  const table = dataTable(ctx, {
    path: 'cars', params: params(), limit: 50, columns, itemName: ['car', 'cars'], cls: 'fleet-ledger', label: 'Cars',
    cover: ['cars.list.cards', 'faq.null-panels', 'cars.list.live_refresh'],
    rowAttrs: (c) => ({ class: ['ledger-row', { 'is-stale': !!c.now?.stale, 'is-unknown': c.now?.state === 'unknown' }], dataset: { car: c.id } }),
    onRows: (items) => { for (const c of items) carNames.set(c.id, c.name); nameAttention(); },
    empty: () => empty({
      title: st.q ? `No car matches “${st.q}”${st.state !== 'all' ? ` among ${stateLabel(st.state).toLowerCase()} cars` : ''}.` : `No car is ${stateLabel(st.state).toLowerCase()} right now.`,
      text: st.q ? 'Try part of a name, a model or a VIN prefix.' : 'Choose another state above to see more cars.',
      action: h('button', { type: 'button', class: 'button', onclick: () => { st.q = ''; search.value = ''; st.state = 'all'; syncTallies(); write(); reload(); } }, 'Show all cars'),
    }),
  });
  function reload() { return table.reload(params(), { sort: null }); }
  function redrawRows() { table.redraw(); }
  function nameAttention() {
    for (const li of attentionBox.querySelectorAll('.attention-item')) {
      const n = carNames.get(li.dataset.car);
      if (n) li.querySelector('.attention-car').textContent = n;
    }
  }

  const asof = h('p', { class: 'asof' }, h('span', { class: 'asof-dot', 'aria-hidden': 'true' }), 'Live', h('span', { class: 'asof-note' }, 'values reach Sentry Mate about a minute after the car sends them'));
  ctx.main.append(
    h('div', { class: 'fleet-top' },
      pageHead({ eyebrow: 'The register', title: 'Fleet', deck: `${tn(summary.cars, '{n} car', '{n} cars')}. ${attention.items.length ? tn(new Set(attention.items.map((a) => a.car)).size, '{n} needs a look.', '{n} need a look.') : 'None needs a look.'}`, aside: asof, cls: 'page-head-fleet' }),
      tallies),
    attentionBox,
    toolbar,
    table.el);
  await table.load();
  ctx.main.append(await missingCars(ctx));
  // Car names for the attention list, when those cars are not on the first page.
  for (const a of attention.items) if (!carNames.has(a.car)) ctx.api.get(api.apiPath`cars/${a.car}`).then((c) => { carNames.set(c.id, c.name); nameAttention(); }).catch(() => {});

  // Live: a car's row follows its `now` events; a car added, removed or renamed loads the list again.
  let again = 0;
  ctx.events(['now', 'fleet'], (ev) => {
    if (ev.type === 'fleet') { clearTimeout(again); again = setTimeout(reload, 500); return; }
    const d = ev.data || {};
    const car = table.items.find((c) => c.id === d.car);
    if (!car || !d.now) return;
    const n = d.now;
    const v = (x) => (x && typeof x === 'object' && 'value' in x ? x.value : x);
    const now = { ...car.now };
    if (n.state) now.state = n.state;
    if (n.since_ms) now.since_ms = n.since_ms;
    if (n.at_ms) now.at_ms = n.at_ms;
    if (n.received_ms) now.received_ms = n.received_ms;
    if ('stale' in n) now.stale = n.stale;
    if (n.soc_pct) now.soc_pct = v(n.soc_pct);
    if (n.range_km) now.range_km = v(n.range_km);
    if (n.location && 'place' in n.location) now.place = n.location.place;
    if (n.charge) {
      if (n.charge.plugged) now.plugged = v(n.charge.plugged);
      if (n.charge.power_kw) now.charge_power_kw = now.state === 'charging' ? v(n.charge.power_kw) : null;
    }
    car.now = now;
    table.redrawRow(car);
  });
  ctx.ready();
  // New units: the rows are drawn again in them (the page has no period).
  return { update: (c) => { table.redraw(); c.ready(); } };
}

/** Why a car may be missing although others show: what waits in set-up (GET /api/v1/setup/status). */
async function missingCars(ctx) {
  const s = await ctx.api.get('setup/status').catch(() => null);
  const fixes = (s && s.fixes) || [];
  return h('section', { class: 'block missing-cars', 'aria-labelledby': 'missing-title', cover: ['cars.empty.title', 'cars.empty.hint'] },
    h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'missing-title' }, 'Missing a car?')),
    h('p', { class: 'note' }, 'A car appears here when its CAN Bridge uploads its first log; you name it afterwards.'),
    fixes.length
      ? h('ul', { class: 'fix-list' }, fixes.map((f) => h('li', null,
        h('p', { class: 'fix-title' }, (FIX_WORDS[f.cause] || [f.cause])[0]),
        h('p', { class: 'fix-text' }, (FIX_WORDS[f.cause] || ['', ''])[1]),
        f.link ? h('a', { class: 'link-arrow', href: f.link }, 'Fix it', icon('arrowRight')) : null)))
      : h('p', { class: 'note-serif' }, 'Nothing is waiting: every upload token was used and every CAN Bridge key belongs to a car.'));
}

/** No car shows: say exactly why, and link the fix. */
async function emptyFleet(ctx) {
  const s = await ctx.api.get('setup/status').catch(() => null);
  const fixes = (s && s.fixes) || [];
  ctx.main.append(
    pageHead({ eyebrow: 'The register', title: 'Fleet', deck: 'No car shows yet.' }),
    h('section', { class: 'empty fleet-empty', cover: ['cars.empty.title', 'cars.empty.hint'] },
      h('p', { class: 'empty-title' }, 'No car is being logged yet.'),
      h('p', { class: 'empty-text' }, 'A car appears here when its CAN Bridge uploads its first log. You name it afterwards.'),
      fixes.length
        ? h('ul', { class: 'fix-list' }, fixes.map((f) => h('li', null,
          h('p', { class: 'fix-title' }, (FIX_WORDS[f.cause] || [f.cause])[0]),
          h('p', { class: 'fix-text' }, (FIX_WORDS[f.cause] || ['', ''])[1]),
          f.link ? h('a', { class: 'link-arrow', href: f.link }, 'Fix it', icon('arrowRight')) : null)))
        : h('p', { class: 'empty-action' }, h('a', { class: 'button', href: '/settings/cars' }, 'Make an upload token in Settings: Cars'))));
  ctx.ready();
  return {};
}
