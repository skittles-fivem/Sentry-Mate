// Fleet (/): every car at once, the ones that need a look, and the counts per state. Works for any
// number of cars: the list is searched, sorted and paged by the server (GET /api/v1/cars). Updates by
// itself when a car is added, renamed or changes state (live events). Each person can pin cars; pinned
// cars come first in "Your order" (preferences pinned_cars).

import * as state from '../shared/state.js';
import { h, replace, setVars } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { formatNumber } from '../shared/i18n.js';
import { icon } from '../core/icons.js';
import { pageHead, panel, panelLink } from '../core/page.js';
import { dataTable } from '../core/table.js';
import { emptyState } from '../core/states.js';
import { statePill, STATES, ago, agoEl, carMeta, when, dur } from '../core/fmt.js';
import { getPrefs, savePrefs, isAdmin } from '../core/prefs.js';
import { flash } from '../core/flash.js';

export const id = 'fleet';
export const css = true;
export const title = () => 'All cars';

const SORTS = [['custom', 'Your order'], ['last_activity', 'Last activity'], ['name', 'Name'], ['soc', 'Battery']];
const TYRE = { fl: 'Front left', fr: 'Front right', rl: 'Rear left', rr: 'Rear right' };
const DOOR = { fl: 'Front left door', fr: 'Front right door', rl: 'Rear left door', rr: 'Rear right door', trunk: 'Trunk', frunk: 'Frunk', liftgate: 'Liftgate' };

// what an attention item says, and the page that shows its cause
function attention(a, car) {
  const d = a.detail || {};
  const c = { car: a.car };
  switch (a.code) {
    case 'stopped_mid_drive': return { text: 'Data stopped in the middle of a drive.', href: state.linkTo('drives', c), go: 'Drives' };
    case 'stopped_mid_charge': return { text: 'Data stopped in the middle of a charge.', href: state.linkTo('charges', c), go: 'Charges' };
    case 'upload_silence': return { text: `No upload for ${d.last_upload_ms ? ago(d.last_upload_ms).replace(/ ago$/, '') : 'a long time'}. The CAN Bridge may be off or out of range.`, href: state.linkTo('health', c), go: 'Data health' };
    case 'trust_failed': return { text: `A log failed its check${d.reason ? ` (${String(d.reason).replace(/_/g, ' ')})` : ''}; its time shows as unknown.`, href: state.linkTo('health', c), go: 'Data health' };
    case 'low_battery': return { text: `Battery low: ${format(d.soc_pct, 'percent')}.`, href: state.linkTo('car', c), go: 'Overview' };
    case 'tyre_low': return { text: `${TYRE[d.tyre] || 'A'} tyre at ${format(d.pressure_kpa, 'pressure')}, ${format(d.recommended_kpa, 'pressure')} recommended.`, href: state.linkTo('car', c), go: 'Overview' };
    case 'door_open_asleep': return { text: `${DOOR[d.door] || 'A door'} open while the car sleeps.`, href: state.linkTo('car', c), go: 'Overview' };
    default: return { text: a.code.replace(/_/g, ' '), href: state.linkTo('car', c), go: 'Overview' };
  }
}

// set-up causes, said plainly, with the fix
const FIX = {
  no_token: ['No upload token yet', 'Make one in Settings: Cars and enter it in the CAN Bridge app. A car appears when its first log arrives.'],
  token_unused: ['A token was made but never used', 'No CAN Bridge has uploaded with it yet. Check the server address and the token in the CAN Bridge app.'],
  key_waiting: ['A CAN Bridge key is waiting', 'A CAN Bridge is uploading with a key that is not tied to a car yet. Assign it to a car in Settings: Cars.'],
  logs_waiting_trust: ['Logs are waiting for trust', 'Some logs wait until their signing key is trusted. Trust the key in Settings: Cars.'],
  import_running: ['An import is still running', 'Its cars appear when the import finishes. Follow it on the Import page.'],
};

export async function mount(ctx) {
  const prefs = getPrefs() || {};
  let pinned = [...(prefs.pinned_cars || [])];
  const q = ctx.query;
  let sort = SORTS.some(([v]) => v === q.sort) ? q.sort : (pinned.length || (prefs.car_order || []).length ? 'custom' : 'last_activity');
  let stateFilter = ['driving', 'charging', 'parked', 'asleep', 'unknown'].includes(q.state) ? q.state : '';
  const search = q.search || '';
  // the order: the Order menu picks what to sort by, a column header too (and flips the direction)
  const FIRST_DIR = { custom: 'asc', name: 'asc', soc: 'desc', last_activity: 'desc' };
  let dir = (q.dir === 'asc' || q.dir === 'desc') && sort !== 'custom' ? q.dir : FIRST_DIR[sort];
  const COL_OF = { name: 'name', soc: 'soc', last_activity: 'age' };
  const SORT_OF = { name: 'name', soc: 'soc', age: 'last_activity' };
  const headSort = () => (COL_OF[sort] ? { key: COL_OF[sort], dir } : null);

  const sub = h('p', { class: 'page-sub' });
  const headStats = h('dl', { class: 'head-stats' });
  const board = h('div', { class: 'fleet-board', role: 'group', 'aria-label': 'Cars by state' });
  const attn = h('div');
  const empty = h('section', { class: 'panel', hidden: true, cover: ['cars.empty.title', 'cars.empty.hint'] });

  // the list
  const params = () => ({ search: searchIn.value.trim() || null, state: stateFilter || null, sort, dir });
  const searchIn = h('input', { class: 'input', type: 'search', placeholder: 'Search name, model or VIN prefix', value: search, 'aria-label': 'Search cars' });
  const stateSel = h('select', { class: 'select', 'aria-label': 'State' },
    h('option', { value: '' }, 'All states'), Object.entries(STATES).map(([k, v]) => h('option', { value: k, selected: k === stateFilter }, v.label)));
  const sortSel = h('select', { class: 'select', 'aria-label': 'Order', cover: ['cars.list.order', 'faq.reorder-cars', 'web.settings.general.car-order'] },
    SORTS.map(([v, l]) => h('option', { value: v, selected: v === sort }, `Order: ${l}`)));
  const orderNote = h('span', { class: 'toolbar-note' });
  const ORDER_NOTE = {
    custom: 'Pinned cars first, in pin order; then by last activity. Pins are yours, other people keep their own.',
    last_activity: 'Newest activity first.', name: 'A to Z by name.', soc: 'Highest battery first.',
  };
  const setOrderNote = () => {
    const flipped = sort !== 'custom' && dir !== FIRST_DIR[sort];
    orderNote.textContent = flipped ? { last_activity: 'Oldest activity first.', name: 'Z to A by name.', soc: 'Lowest battery first.' }[sort] : ORDER_NOTE[sort];
  };
  setOrderNote();

  const pinBtn = (c) => {
    const i = pinned.indexOf(c.id);
    const on = i >= 0;
    const move = (d) => async () => {
      const next = [...pinned];
      const j = i + d;
      if (j < 0 || j >= next.length) return;
      [next[i], next[j]] = [next[j], next[i]];
      await setPinned(next, null);
    };
    return h('span', { class: 'pin-cell' },
      h('button', { class: ['btn btn--ghost btn--icon btn--sm pin-btn', on && 'is-on'], type: 'button', 'aria-pressed': String(on), title: on ? 'Unpin: list it by last activity' : 'Pin: list it first in Your order',
        'aria-label': `${on ? 'Unpin' : 'Pin'} ${c.name || 'this car'}`, onclick: () => setPinned(on ? pinned.filter((x) => x !== c.id) : [...pinned, c.id], on ? `${c.name} unpinned.` : `${c.name} pinned: it comes first in Your order.`) }, icon('flag', 'ico--sm')),
      on && sort === 'custom' ? h('button', { class: 'btn btn--ghost btn--icon btn--sm', type: 'button', disabled: i === 0, 'aria-label': `Move ${c.name} up`, title: 'Move up', onclick: move(-1) }, icon('chev-l', 'ico--sm rot-90')) : null,
      on && sort === 'custom' ? h('button', { class: 'btn btn--ghost btn--icon btn--sm', type: 'button', disabled: i === pinned.length - 1, 'aria-label': `Move ${c.name} down`, title: 'Move down', onclick: move(1) }, icon('chev-r', 'ico--sm rot-90')) : null);
  };
  async function setPinned(next, note) {
    const before = pinned;
    pinned = next;
    try {
      await savePrefs({ pinned_cars: next });
      if (note) flash(note);
      table.reload(params());
    } catch (e) {
      pinned = before;
      flash(`Could not change the order: ${e.message}`, { kind: 'error' });
    }
  }

  const columns = [
    { key: 'name', label: 'Car', sort: 'name', firstDir: 'asc', render: (c) => h('span', { class: 'car-cell' },
      h('a', { class: 'car-name main', href: state.linkTo('car', { car: c.id }) }, c.name || `Unnamed ${c.model || 'car'}`),
      h('span', { class: 'car-meta' }, carMeta(c), ' ', c.vin_prefix ? h('span', { class: 'key-chip', title: 'VIN prefix (the serial number is never stored)' }, c.vin_prefix) : null,
        !c.name && isAdmin() ? [' · ', h('a', { class: 'link', href: state.buildUrl('settings-cars', { car: c.id }) }, 'Name it')] : null)) },
    { key: 'state', label: 'State', render: (c) => h('span', { class: 'cell-since' },
      statePill(c.now?.state, { stale: c.now?.stale }),
      h('span', null, c.now?.since_ms ? `since ${when(c.now.since_ms, c.time_zone)}` : 'no data yet')) },
    { key: 'soc', label: 'Battery', sort: 'soc', firstDir: 'desc', title: 'Battery charge and estimated range, last known value', render: (c) => {
      const soc = c.now?.soc_pct;
      const bar = h('span', { class: ['bar', soc != null && soc < 20 && 'bar--low', c.now?.stale && 'bar--muted'] }, h('i'));
      setVars(bar, { '--w': `${Math.max(0, Math.min(100, soc ?? 0))}%` });
      return h('span', { class: 'cell-batt' }, bar, h('span', { class: 'cell-batt-text' }, h('b', null, format(soc, 'percent')), h('span', null, format(c.now?.range_km, 'range'))));
    } },
    { key: 'place', label: 'Place', render: (c) => {
      const n = c.now || {};
      const what = n.state === 'charging' && n.charge_power_kw != null ? `Charging at ${format(n.charge_power_kw, 'power')}` : n.plugged ? 'Plugged in' : n.state === 'driving' ? 'On the road' : null;
      return h('span', { class: 'cell-place' }, h('b', null, n.place?.name || (n.state === 'driving' ? 'Driving' : 'No saved place')), what ? h('span', null, what) : null);
    } },
    { key: 'age', label: 'Data', sort: 'last_activity', firstDir: 'desc', title: 'When the newest value was recorded. Logs arrive about a minute after the car records them; stale means no upload for over an hour.', render: (c) => {
      const n = c.now || {};
      return h('span', { class: 'cell-since' },
        h('span', { class: ['age', n.stale ? 'age--stale' : n.state === 'driving' || n.state === 'charging' ? 'age--fresh' : 'age--old'] }, n.stale ? icon('clock') : null, n.at_ms ? agoEl(n.at_ms, c.time_zone) : 'no data'),
        n.stale ? h('span', null, 'last known state') : null);
    } },
    { key: 'pin', label: 'Order', cls: 'col-pin', csv: false, render: pinBtn },
  ];
  const table = dataTable(ctx, {
    title: 'Cars', icon: 'car', path: 'cars', params: params(), columns, limit: 50, csvName: 'cars', sort: headSort(),
    onSort: (sNow) => {
      sort = SORT_OF[sNow.key]; dir = sNow.dir;
      sortSel.value = sort;
      setOrderNote();
      ctx.setQuery({ sort, dir: dir === FIRST_DIR[sort] ? null : dir });
    },
    cover: ['cars.list.cards', 'cars.list.live_refresh', 'faq.null-panels'],
    rowHref: (c) => state.linkTo('car', { car: c.id }),
    empty: () => (searchIn.value.trim() || stateFilter
      ? emptyState({ icon: 'search', title: 'No car matches', text: `Nothing matches${searchIn.value.trim() ? ` "${searchIn.value.trim()}"` : ''}${stateFilter ? ` in the state ${STATES[stateFilter].label.toLowerCase()}` : ''}. Clear the search or pick All states.` })
      : emptyState({ icon: 'car', title: 'No cars yet', text: 'See why below.' })),
  });
  table.el.classList.add('fleet-table');

  let timer = 0;
  const apply = () => {
    if (sortSel.value !== sort) { sort = sortSel.value; dir = FIRST_DIR[sort]; table.setSortKey(headSort()); }
    stateFilter = stateSel.value;
    setOrderNote();
    ctx.setQuery({ search: searchIn.value.trim() || null, state: stateFilter || null, sort, dir: dir === FIRST_DIR[sort] ? null : dir });
    table.reload(params());
    paintBoard(lastSummary);
  };
  searchIn.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(apply, 250); });
  stateSel.addEventListener('change', apply);
  sortSel.addEventListener('change', apply);
  ctx.onCleanup(() => clearTimeout(timer));

  replace(ctx.view,
    pageHead({ title: 'All cars', cls: 'page-head--fleet', actions: headStats }),
    board, attn, empty,
    h('div', { class: 'toolbar' },
      h('label', { class: 'field field--search' }, icon('search'), searchIn),
      stateSel, sortSel, orderNote, h('span', { class: 'grow' })),
    table.el);
  ctx.view.querySelector('.page-head-text').append(sub);

  let lastSummary = null;
  function paintBoard(s) {
    if (!s) return;
    const tile = (key, label, ic, value, subText, { stale = false } = {}) => {
      const pressed = !stale && stateFilter === key;
      const attrs = { class: ['state-tile', stale && 'state-tile--stale'], dataset: { state: stale ? null : key } };
      const kids = [h('span', { class: 'state-tile-label' }, icon(ic), label), h('span', { class: 'state-tile-value' }, formatNumber(value)), h('span', { class: 'state-tile-sub' }, subText)];
      if (stale) return h('div', { ...attrs, title: 'Cars with no upload for over an hour. Their last known state is shown and counted under it as well.' }, kids);
      return h('button', { ...attrs, type: 'button', 'aria-pressed': String(pressed), title: pressed ? 'Show every state' : `Show only ${label.toLowerCase()} cars`,
        onclick: () => { stateSel.value = pressed ? '' : key; apply(); } }, kids);
    };
    const n = s.cars || 0, b = s.by_state || {};
    const share = (v) => (n ? `${Math.round((v / n) * 100)} % of cars` : '—');
    replace(board,
      tile('driving', 'Driving', 'steer', b.driving || 0, share(b.driving || 0)),
      tile('charging', 'Charging', 'bolt', b.charging || 0, share(b.charging || 0)),
      tile('parked', 'Parked', 'park', b.parked || 0, 'awake, not moving'),
      tile('asleep', 'Asleep', 'moon', b.asleep || 0, 'sleeping'),
      tile('unknown', 'Unknown', 'question', b.unknown || 0, 'no log for now'),
      tile('stale', 'Stale', 'clock', s.stale || 0, 'no upload for over 1 h', { stale: true }));
  }

  async function loadTop() {
    const [s, at, setup] = await Promise.all([
      ctx.api.get('fleet/summary'),
      ctx.api.page('fleet/attention', {}, { limit: 8 }).catch(() => ({ items: [], next: null })),
      isAdmin() ? ctx.api.get('setup/status').catch(() => null) : Promise.resolve(null),
    ]);
    if (!ctx.alive()) return;
    lastSummary = s;
    paintBoard(s);
    const cars = s.cars || 0;
    replace(sub,
      h('span', null, `${formatNumber(cars)} ${cars === 1 ? 'car' : 'cars'}`),
      h('span', { class: 'sep', 'aria-hidden': 'true' }), h('span', null, `${formatNumber(s.by_state?.driving || 0)} driving now`),
      s.stale ? [h('span', { class: 'sep', 'aria-hidden': 'true' }), h('span', { class: 'sub-warn' }, icon('clock', 'ico--sm'), `${formatNumber(s.stale)} stale, last known state shown`)] : null,
      h('span', { class: 'sep', 'aria-hidden': 'true' }), h('span', null, 'Updates by itself'));
    // attention: the cars that need a look, with the cause and the page that shows it, then set-up fixes
    const fixes = (setup?.fixes || []).filter((f) => FIX[f.cause]);
    // the count is the rows of the panel below ("8+" when more car items wait in the list)
    replace(headStats,
      h('div', { class: 'head-stat' }, h('dt', null, 'Need a look'), h('dd', null, formatNumber(at.items.length + fixes.length), at.next ? '+' : '')),
      h('div', { class: 'head-stat' }, h('dt', null, 'Charging now'), h('dd', null, formatNumber(s.by_state?.charging || 0))));

    if (at.items.length || fixes.length) {
      const names = new Map();
      await Promise.all([...new Set(at.items.map((a) => a.car))].map(async (cid) => {
        try { names.set(cid, (await ctx.api.get(`cars/${encodeURIComponent(cid)}`, null, { maxAgeMs: 60000 })).name); } catch { names.set(cid, cid); }
      }));
      if (!ctx.alive()) return;
      replace(attn, panel({ title: 'Need a look', icon: 'warn', cls: 'attn-panel', side: at.next ? h('span', { class: 'faint' }, 'More below in the list') : null },
        h('ul', { class: 'attn-list' },
          at.items.map((a) => {
            const t = attention(a);
            return h('li', { class: 'attn-row' },
              h('a', { class: 'attn-car', href: state.linkTo('car', { car: a.car }) }, names.get(a.car) || a.car),
              h('span', { class: 'attn-text' }, t.text),
              h('span', { class: 'attn-when faint' }, a.since_ms ? ago(a.since_ms) : ''),
              h('a', { class: 'panel-link', href: t.href }, t.go, icon('chev-r', 'ico--sm')));
          }),
          fixes.map((f) => h('li', { class: 'attn-row' },
            h('span', { class: 'attn-car' }, 'Set-up'),
            h('span', { class: 'attn-text' }, `${FIX[f.cause][0]}. ${FIX[f.cause][1]}`),
            h('span', { class: 'attn-when' }),
            h('a', { class: 'panel-link', href: f.link || '/settings/cars' }, f.cause === 'import_running' ? 'Import' : 'Settings: Cars', icon('chev-r', 'ico--sm')))))));
    } else replace(attn);

    // no car: say exactly why, and link the fix
    empty.hidden = cars > 0;
    const causes = (setup?.fixes || []).filter((f) => FIX[f.cause]);
    replace(empty, h('div', { class: 'empty' },
      h('div', { class: 'empty-icon' }, icon('car')),
      h('h3', null, 'No car to show yet'),
      h('p', null, isAdmin() ? 'A car appears here when its CAN Bridge uploads its first log.' : 'Your account may not see any car yet. Ask an admin to add one to your account.'),
      causes.length ? h('ul', { class: 'empty-causes' }, causes.map((f) => h('li', null, h('b', null, FIX[f.cause][0]), ': ', FIX[f.cause][1], ' ', h('a', { class: 'link', href: f.link || '/settings/cars' }, 'Fix it')))) : null,
      isAdmin() && !causes.length ? h('a', { class: 'btn', href: state.buildUrl('settings-cars') }, icon('settings'), 'Settings: Cars') : null));
  }

  try {
    await Promise.all([loadTop(), table.ready]);
  } catch (e) { ctx.fail(e); return; }
  ctx.ready();

  // live: counts and attention follow the cars; the list reloads while only its first page is open
  let pending = 0;
  ctx.events(['fleet', 'now'], (ev) => {
    if (pending) return;
    pending = setTimeout(async () => {
      pending = 0;
      try { await loadTop(); } catch { /* the next event tries again */ }
      if (ev.type === 'fleet' || table.rows().length <= 50) table.reload(params());
    }, ev.type === 'fleet' ? 300 : 10000);
  });
  ctx.onCleanup(() => clearTimeout(pending));
}
