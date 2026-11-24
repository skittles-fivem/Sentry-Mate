// Fleet (/): one table row per car: name, state, battery as a 20-cell text bar and a number, range,
// odometer, place, last seen. The list is sorted and paged by the server (GET /cars); the odometer comes
// from each listed car's /now. Order keys: [your order] (pinned cars first, then by activity), [activity],
// [name], [battery]; the pins are this account's (preferences pinned_cars). Updates itself on 'fleet' and
// 'now' events. Above it: the counts per state, and the cars that need a look. Sold cars are left out of the
// list and the counts; "sold cars (n)" at the end shows them, muted, without live state.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { formatNumber } from '../shared/i18n.js';
import { head, section, kv, textBar, link, note } from '../core/ui.js';
import { num, numText, stateWord, ago, when, tag } from '../core/fmt.js';
import { table } from '../core/table.js';
import { empty } from '../core/states.js';
import { getPrefs, savePrefs, isAdmin } from '../core/prefs.js';
import { toast } from '../core/toast.js';
import { src, srcText } from '../core/source.js';

export const id = 'fleet';
export const title = () => 'Fleet';

const ORDERS = [['custom', 'your order'], ['last_activity', 'activity'], ['name', 'name'], ['soc', 'battery']];
const FIRST_DIR = { custom: 'asc', name: 'asc', soc: 'desc', last_activity: 'desc' };
const ATTN = {
  stopped_mid_drive: ['data stopped in the middle of a drive', 'drives'],
  stopped_mid_charge: ['data stopped in the middle of a charge', 'charges'],
  upload_silence: ['no upload for a long time: the CAN Bridge may be off or out of range', 'health'],
  trust_failed: ['a log failed its check; its time shows as unknown', 'health'],
  low_battery: ['battery low', 'car'],
  tyre_low: ['a tyre is under the recommended pressure', 'car'],
  door_open_asleep: ['a door is open while the car sleeps', 'car'],
};
const FIX = {
  no_token: 'no upload token yet: make one in settings / cars and enter it in the CAN Bridge app',
  token_unused: 'a token was made but never used: check the server address and token in the CAN Bridge app',
  key_waiting: 'a CAN Bridge key waits to be tied to a car: assign it in settings / cars',
  logs_waiting_trust: 'logs wait until their signing key is trusted: trust it in settings / cars',
  import_running: 'an import is still running: its cars appear when it finishes',
};

export async function mount(ctx) {
  const prefs = getPrefs() || {};
  let pinned = [...(prefs.pinned_cars || [])];
  const q = ctx.query;
  let order = ORDERS.some(([v]) => v === q.sort) ? q.sort : (pinned.length ? 'custom' : 'last_activity');
  let dir = q.dir === 'asc' || q.dir === 'desc' ? q.dir : FIRST_DIR[order];
  let only = ['driving', 'charging', 'parked', 'asleep', 'unknown'].includes(q.state) ? q.state : '';
  const odo = new Map();
  let showSold = q.sold === '1';
  const soldSlot = h('div');

  const counts = h('div', { class: 'stats', role: 'group', 'aria-label': 'Cars by state' });
  const attn = h('div');
  const emptyBox = h('div', { hidden: true, cover: ['cars.empty.title', 'cars.empty.hint'] });

  const orderKeys = h('div', { class: 'keys', role: 'group', 'aria-label': 'Order', cover: ['cars.list.order', 'faq.reorder-cars', 'web.settings.general.car-order'] });
  const orderNote = h('span', { class: 'muted' });
  const paintOrder = () => {
    replace(orderKeys, ORDERS.map(([v, l]) => h('button', { class: 'key', type: 'button', 'aria-pressed': String(order === v), onclick: () => setOrder(v) }, l)));
    orderNote.textContent = order === 'custom' ? 'pinned cars first, in pin order, then by activity; pins are yours' : dir === FIRST_DIR[order] ? '' : 'reversed';
  };
  const params = () => ({ state: only || null, sort: order, dir });

  const pinCell = (c) => {
    const i = pinned.indexOf(c.id);
    const on = i >= 0;
    const move = (d) => () => { const n = [...pinned]; const j = i + d; if (j < 0 || j >= n.length) return; [n[i], n[j]] = [n[j], n[i]]; setPinned(n); };
    return h('span', { class: 'pin' },
      h('button', { class: 'btn btn--quiet', type: 'button', 'aria-pressed': String(on), title: on ? 'Unpin' : 'Pin: list it first in your order', 'aria-label': `${on ? 'Unpin' : 'Pin'} ${c.name || 'this car'}`, onclick: () => setPinned(on ? pinned.filter((x) => x !== c.id) : [...pinned, c.id]) }, on ? '[x]' : '[ ]'),
      on && order === 'custom' ? [
        h('button', { class: 'btn btn--quiet', type: 'button', disabled: i === 0, 'aria-label': `Move ${c.name} up`, onclick: move(-1) }, 'up'),
        h('button', { class: 'btn btn--quiet', type: 'button', disabled: i === pinned.length - 1, 'aria-label': `Move ${c.name} down`, onclick: move(1) }, 'dn')] : null);
  };
  async function setPinned(next) {
    const before = pinned;
    pinned = next;
    try { await savePrefs({ pinned_cars: next }); list.reload(params(), { quiet: true }); } catch (e) { pinned = before; toast(`Could not change the order: ${e.message}`, { kind: 'error' }); }
  }

  const placeText = (c) => {
    const n = c.now || {};
    if (n.place?.name) return n.place.name;
    return n.state === 'driving' ? 'on the road' : null;
  };
  const columns = [
    { key: 'name', label: 'car', ph: 'main', render: (c) => h('a', { class: 'lnk-plain', href: state.linkTo('car', { car: c.id }) }, c.name || `unnamed ${c.model || 'car'}`) },
    { key: 'state', label: 'state', ph: 'main', render: (c) => stateWord(c.now?.state, { stale: c.now?.stale }) },
    { key: 'soc', label: 'battery', render: (c) => h('span', { class: 'soc-cell' }, textBar(c.now?.soc_pct, { cells: 20, tone: c.now?.soc_pct != null && c.now.soc_pct < 20 ? 'bad' : c.now?.stale ? 'text' : 'accent' }), h('span', { class: 'soc-n' }, num(c.now?.soc_pct, 'percent'))) },
    { key: 'soc_n', label: 'soc', num: true, ph: 'num', desk: false, render: (c) => num(c.now?.soc_pct, 'percent') },
    { key: 'range', label: 'range', num: true, ph: 'num', render: (c) => num(c.now?.range_km, 'range') },
    { key: 'odo', label: 'odometer', num: true, render: (c) => (odo.has(c.id) ? num(odo.get(c.id), 'odometer') : null) },
    { key: 'place', label: 'place', ph: 'sub', render: (c) => placeText(c) },
    { key: 'seen', label: 'last seen', ph: 'sub', title: 'When the newest value was recorded', render: (c) => (c.now?.at_ms ? h('span', { class: c.now.stale ? 'tone--warn' : null, title: when(c.now.at_ms, c.time_zone) }, ago(c.now.at_ms)) : 'no data') },
    { key: 'from', label: 'data', title: 'what the car has data from: smo (the CAN Bridge), api (the Tesla API), imp (imported)', render: (c) => src(c), value: (c) => srcText(c) },
    { key: 'pin', label: 'pin', render: pinCell },
  ];
  const list = table(ctx, {
    title: 'cars', path: 'cars', params: params(), limit: 50, columns, csvName: 'cars',
    cover: ['cars.list.cards', 'cars.list.live_refresh', 'faq.null-panels'],
    rowHref: (c) => state.linkTo('car', { car: c.id }),
    keep: (c) => !state.isSold(c),
    onPage: (pg, items) => loadOdo(items.filter((c) => !state.isSold(c))),
    empty: () => (only ? empty(`no ${only} car`, h('button', { class: 'btn', type: 'button', onclick: () => setOnly('') }, 'show every state'))
      : lastSummary?.sold ? empty('every car is sold', 'sold cars are listed under sold cars below') : empty('no cars yet', 'the reason and the fix are shown above')),
  });

  // sold cars: a muted list at the end, behind "sold cars (n)"
  let soldCars = null;
  async function loadSold() {
    const pg = ctx.api.pager('cars', { sort: 'name' }, { limit: 100 });
    for (let i = 0; i < 20 && !pg.done; i++) await pg.more();
    return state.splitSold(pg.items).sold;
  }
  async function paintSold(fresh = false) {
    const n = lastSummary?.sold || 0;
    if (!n) { replace(soldSlot); return; }
    const toggle = h('button', { class: 'key', type: 'button', 'aria-pressed': String(showSold), 'aria-expanded': String(showSold), onclick: () => { showSold = !showSold; ctx.setQuery({ sold: showSold ? '1' : null }); paintSold(); } },
      `sold cars (${formatNumber(n)})`);
    const line = h('div', { class: 'optline' }, toggle, h('span', { class: 'muted' }, 'not in the list or the counts above; their history stays browsable'));
    if (!showSold) { replace(soldSlot, line); return; }
    if (!soldCars || fresh) {
      replace(soldSlot, line, h('p', { class: 'note' }, 'loading sold cars'));
      try { soldCars = await loadSold(); } catch (e) { soldCars = null; if (ctx.alive()) replace(soldSlot, line, h('p', { class: 'note' }, `could not load the sold cars: ${e.message}`)); return; }
      if (!ctx.alive() || !showSold) return;
    }
    const t = table(ctx, {
      title: 'sold cars', rows: soldCars, csv: false, label: 'Sold cars', wrapCls: 'tbl-sec--sold',
      rowHref: (c) => state.linkTo('car', { car: c.id }),
      columns: [
        { key: 'name', label: 'car', ph: 'main', render: (c) => [h('a', { class: 'lnk-plain', href: state.linkTo('car', { car: c.id }) }, c.name || `unnamed ${c.model || 'car'}`), ' ', tag('sold', 'muted')] },
        { key: 'model', label: 'model', ph: 'sub', render: (c) => [c.model, c.year].filter(Boolean).join(' ') || null },
        { key: 'sold_date', label: 'sold on', ph: 'num', render: (c) => c.sold_date || 'date not given' },
        { key: 'seen', label: 'last data', ph: 'sub', grow: true, render: (c) => (c.now?.at_ms ? h('span', { title: when(c.now.at_ms, c.time_zone) }, ago(c.now.at_ms)) : 'no data') },
        { key: 'from', label: 'data', render: (c) => src(c), value: (c) => srcText(c) },
      ],
      empty: () => empty('no sold car'),
    });
    replace(soldSlot, line, t.el);
  }

  async function loadOdo(items) {
    const need = items.filter((c) => !odo.has(c.id)).slice(0, 50);
    await Promise.all(need.map(async (c) => {
      try { const n = await ctx.api.get(`cars/${encodeURIComponent(c.id)}/now`, null, { maxAgeMs: 30000 }); odo.set(c.id, n.odometer_km?.value ?? null); } catch { odo.set(c.id, null); }
    }));
    if (!ctx.alive() || !need.length) return;
    list.repaint();
  }

  function setOrder(v) {
    dir = order === v && v !== 'custom' ? (dir === 'asc' ? 'desc' : 'asc') : FIRST_DIR[v];
    order = v;
    ctx.setQuery({ sort: order, dir: dir === FIRST_DIR[order] ? null : dir });
    paintOrder();
    list.reload(params());
  }
  function setOnly(v) {
    only = v;
    ctx.setQuery({ state: only || null });
    paintCounts(lastSummary);
    list.reload(params());
  }

  let lastSummary = null;
  function paintCounts(s) {
    if (!s) return;
    const b = s.by_state || {};
    const item = (k, n) => h('button', { class: 'key', type: 'button', 'aria-pressed': String(only === k), title: only === k ? 'Show every state' : `Show only ${k} cars`, onclick: () => setOnly(only === k ? '' : k) },
      h('span', { class: `st st--${k}` }, `[${k}]`), ` ${formatNumber(n || 0)}`);
    replace(counts,
      h('span', { class: 'stat' }, h('span', { class: 'stat-k' }, 'cars'), formatNumber(s.cars || 0)),
      h('span', { class: 'keys' }, ['driving', 'charging', 'parked', 'asleep', 'unknown'].map((k) => item(k, b[k]))),
      h('span', { class: 'stat', title: 'No upload for over an hour: the last known state is shown' }, h('span', { class: 'stat-k' }, 'stale'), h('span', { class: s.stale ? 'tone--warn' : null }, formatNumber(s.stale || 0))));
  }

  async function loadTop(fresh = false) {
    const [s, at, setup] = await Promise.all([
      ctx.api.get('fleet/summary'),
      ctx.api.page('fleet/attention', {}, { limit: 8 }).catch(() => ({ items: [], next: null })),
      isAdmin() ? ctx.api.get('setup/status').catch(() => null) : Promise.resolve(null),
    ]);
    if (!ctx.alive()) return;
    lastSummary = s;
    paintCounts(s);
    paintSold(fresh).catch(() => {});
    const fixes = (setup?.fixes || []).filter((f) => FIX[f.cause]);
    const names = new Map();
    await Promise.all([...new Set(at.items.map((a) => a.car))].map(async (cid) => {
      try { names.set(cid, (await ctx.api.get(`cars/${encodeURIComponent(cid)}`, null, fresh ? {} : { maxAgeMs: 60000 })).name); } catch { names.set(cid, cid); }
    }));
    if (!ctx.alive()) return;
    replace(attn, at.items.length || fixes.length ? section({ title: 'need a look', side: at.next ? h('span', { class: 'muted' }, 'more in the list') : null },
      h('div', { class: 'lines' },
        at.items.map((a) => {
          const [text, page] = ATTN[a.code] || [a.code.replace(/_/g, ' '), 'car'];
          const href = state.linkTo(page, { car: a.car });
          return h('div', { class: 'ln ln--attn', dataset: { row: '', href } },
            h('span', { class: 'ln-t' }, a.since_ms ? ago(a.since_ms).replace(/ ago$/, '') : ''),
            h('span', { class: 'ln-tag' }, tag('warn', 'warn')),
            h('span', { class: 'ln-what' }, h('a', { class: 'lnk-plain', href: state.linkTo('car', { car: a.car }) }, names.get(a.car) || a.car), h('span', { class: 'muted' }, ': '), text),
            h('a', { class: 'lnk ln-num', href }, page));
        }),
        fixes.map((f) => h('div', { class: 'ln ln--attn', dataset: { row: '', href: f.link || '/settings/cars' } },
          h('span', { class: 'ln-t' }), h('span', { class: 'ln-tag' }, tag('todo', 'warn')),
          h('span', { class: 'ln-what' }, h('span', { class: 'muted' }, 'set-up: '), FIX[f.cause]),
          h('a', { class: 'lnk ln-num', href: f.link || '/settings/cars' }, f.cause === 'import_running' ? 'import' : 'settings')))))
      : null);
    const none = !(s.cars > 0) && !(s.sold > 0);
    emptyBox.hidden = !none;
    replace(emptyBox, section({ title: 'no cars' },
      empty('no car to show yet', isAdmin() ? 'a car appears here when its CAN Bridge (SMO) uploads its first log, or when a car of a Tesla account is matched to a new car here' : 'your account may not see any car yet: ask an admin to add one'),
      isAdmin() ? h('p', { class: 'note' }, setup?.tesla_accounts
        ? [`${formatNumber(setup.tesla_accounts)} tesla ${setup.tesla_accounts === 1 ? 'account is' : 'accounts are'} added: match its cars to start reading them. `, link(`${state.buildUrl('settings-integrations')}#tesla`, 'tesla account')]
        : ['another way in: add a tesla account, and the server reads its cars from the tesla api as well (optional; off until you add one). ', link(`${state.buildUrl('settings-integrations')}#tesla`, 'add a tesla account')]) : null,
      fixes.length ? kv(fixes.map((f) => [f.cause.replace(/_/g, ' '), FIX[f.cause]]), { cols: 1 }) : null,
      isAdmin() && !fixes.length ? link(state.buildUrl('settings-cars'), 'settings / cars') : null));
  }

  paintOrder();
  replace(ctx.view, h('div', { class: 'page', dataset: { page: 'fleet' } },
    head(ctx, { title: 'Fleet', period: false, car: false, extra: h('span', { class: 'muted ph-extra' }, 'updates by itself') }),
    counts, attn, emptyBox,
    h('div', { class: 'optline' }, h('span', { class: 'optline-k' }, 'order:'), orderKeys, orderNote),
    list.el, soldSlot,
    note('j / k move through the rows, enter opens the car. Pins and the order are yours; other people keep their own.')));

  try { await Promise.all([loadTop(), list.ready]); } catch (e) { ctx.fail(e); return; }
  ctx.ready();

  let pending = 0, fleetSeen = false;
  ctx.events(['fleet', 'now'], (ev) => {
    const isFleet = ev.type === 'fleet';
    if (pending && !(isFleet && !fleetSeen)) return;
    if (isFleet) { fleetSeen = true; clearTimeout(pending); }
    pending = setTimeout(async () => {
      pending = 0;
      const f = fleetSeen; fleetSeen = false;
      if (f) odo.clear();
      try { await loadTop(f); } catch { /* the next event tries again */ }
      if (ctx.alive()) list.reload(params(), { quiet: true });
    }, isFleet ? 300 : 10000);
  });
  ctx.onCleanup(() => clearTimeout(pending));
}
