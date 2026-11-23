// Sentry Mate, "Pebble" UI pack: Fleet (/), one big tile per car.
//
// Each car is a tile with its battery ring (the number centred in it), its name and model, a state line ('Parked
// at Work garage'), the range and how old the data is; what needs a look shows as chips. The person's own order
// comes first (pinned cars, then car_order); "Order" opens a sheet to sort by name, battery or activity, or to
// move cars in their own order. Live: `now` events redraw a car's tile, `fleet` events load the list again.
// Data: GET /api/v1/cars (paged), /fleet/attention, /setup/status (why a car may be missing), /me/preferences.

import { h } from '../shared/dom.js';
import { format, rangeValue, rangeLabel } from '../shared/units.js';
import { formatTime, formatDate, dayKey, formatDayKey } from '../shared/time.js';
import { tn } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { tile, ring, stateTag, chip, btn, linkBtn, ago, icon, note } from '../core/ui.js';
import { emptyTile, errorWords } from '../core/states.js';
import { srcPill } from '../core/source.js';
import { createSheet, sheetGroup } from '../core/sheet.js';
import { segmented } from '../core/form.js';
import { currentPrefs, savePref } from '../core/prefs.js';
import { moreButton } from '../core/bits.js';

export const id = 'fleet';
export const css = true;
export const shape = [[2, 'h2'], [2, 'h2'], [2, 'h2'], [2, 'h2']];
export const title = () => 'Fleet';

const SORTS = [['custom', 'Your order'], ['last_activity', 'Last active'], ['name', 'Name'], ['soc', 'Battery']];
const SORT_DIR = { last_activity: 'desc', name: 'asc', soc: 'desc' };
const ATTN = {
  stopped_mid_drive: ['Data stopped mid-drive', 'bad', 'drives'], stopped_mid_charge: ['Data stopped mid-charge', 'bad', 'charges'],
  upload_silence: ['No upload lately', 'sun', 'health'], trust_failed: ['A log failed its check', 'bad', 'health'],
  low_battery: ['Battery low', 'sun', 'car'], tyre_low: ['A tyre is low', 'sun', 'car'], door_open_asleep: ['A door is open', 'sun', 'car'],
};
const FIXES = {
  no_token: ['No upload token yet', 'Make an upload token for the car in Settings: Cars and put it in the CAN Bridge app.'],
  token_unused: ['A token was never used', 'Check that the CAN Bridge app has the token and can reach this server.'],
  key_waiting: ['A CAN Bridge key is waiting', 'A CAN Bridge sent logs with a key no car has yet. Give it to a car in Settings: Cars.'],
  logs_waiting_trust: ['Logs wait for trust', 'Uploaded logs wait until their key is trusted. Trust the key in Settings: Cars.'],
  import_running: ['An import is running', 'Cars from the TeslaMate import show when it is done. See Import.'],
};

/** 'Parked at Work garage', 'Driving', 'Charging at Home', 'Asleep at Home'. */
function stateLine(c) {
  const n = c.now || {};
  const s = n.state || 'unknown';
  if (s === 'unknown') return 'No recent log';
  const where = n.place ? ` at ${n.place.name}` : '';
  const word = { driving: 'Driving', charging: 'Charging', parked: 'Parked', asleep: 'Asleep', online: 'Awake', updating: 'Updating', offline: 'Offline' }[s] || 'Seen';
  return s === 'driving' ? 'On the road' : `${word}${where}`;
}
function sinceText(c) {
  const n = c.now || {};
  if (!n.since_ms) return null;
  const tz = c.time_zone;
  return `since ${dayKey(n.since_ms, tz) === dayKey(api.now(), tz) ? formatTime(n.since_ms, tz) : formatDate(n.since_ms, tz, { now: api.now() })}`;
}
/** Why a value is blank. */
function blankWhy(c) {
  const s = c.now?.state;
  if (!c.now || s === 'unknown') return 'no recent log';
  if (s === 'asleep') return 'asleep, not sent';
  return 'not sent by this car';
}

function carTile(c, attention) {
  const n = c.now || {};
  const st = n.state || 'unknown';
  const range = rangeValue(n, 'range_km');
  const codes = [...new Set([...(c.attention || []), ...attention.filter((a) => a.car === c.id).map((a) => a.code)])];
  return tile({ w: 2, href: state.buildUrl('car', { car: c.id }), cls: ['car-tile', `st-${st}`, { 'is-stale': !!n.stale }], label: `${c.name || 'Unnamed car'}: ${stateLine(c)}`, cover: 'cars.list.cards' },
    h('div', { class: 'car-tile-main' },
      ring(n.soc_pct, { label: 'Battery', size: 'm', tone: st === 'charging' ? 'teal' : 'coral', cover: 'faq.null-panels' }),
      h('div', { class: 'car-tile-text' },
        h('p', { class: 'car-tile-name' }, c.name || 'Unnamed car'),
        h('p', { class: 'car-tile-model' }, [c.year, c.model, c.trim].filter(Boolean).join(' ') || c.vin_prefix || c.id),
        h('p', { class: 'car-tile-state' }, stateTag(st, { stale: !!n.stale }), h('span', { class: 'car-tile-where' }, stateLine(c))),
        h('p', { class: 'car-tile-line' },
          range != null ? `${format(range, 'range')} ${rangeLabel().toLowerCase()}` : `Range: ${blankWhy(c)}`,
          n.charge_power_kw != null ? ` · ${format(n.charge_power_kw, 'power')}` : n.plugged ? ' · plugged in' : ''),
        c.sources?.length ? h('p', { class: 'src-line' }, srcPill(c, { short: true })) : null)),
    h('div', { class: 'car-tile-foot' },
      h('span', { class: 'car-tile-age' }, icon('pulse'), ago(n.at_ms ?? c.last_activity_ms), sinceText(c) ? h('span', { class: 'muted' }, ` · ${sinceText(c)}`) : null),
      codes.length ? h('span', { class: 'item-flags' }, codes.map((k) => chip((ATTN[k] || [k.replace(/_/g, ' ')])[0], { tone: (ATTN[k] || [0, 'sun'])[1] }))) : null));
}

/** A sold car: muted, a Sold badge and its date, no live state, battery or attention. Its pages stay open. */
function soldTile(c) {
  return tile({ w: 2, href: state.buildUrl('car', { car: c.id }), cls: ['car-tile', 'is-sold'], label: `${c.name || 'Unnamed car'}: sold` },
    h('div', { class: 'car-tile-main' },
      h('span', { class: 'avatar sold-avatar', 'aria-hidden': 'true' }, (c.name || '?').slice(0, 1).toUpperCase()),
      h('div', { class: 'car-tile-text' },
        h('p', { class: 'car-tile-name' }, c.name || 'Unnamed car'),
        h('p', { class: 'car-tile-model' }, [c.year, c.model, c.trim].filter(Boolean).join(' ') || c.vin_prefix || c.id),
        h('p', { class: 'car-tile-state' }, chip('Sold', { tone: 'muted' }), h('span', { class: 'car-tile-where' }, c.sold_date ? `Sold on ${formatDayKey(c.sold_date, { weekday: false })}` : 'Sold'))),
    ),
    h('div', { class: 'car-tile-foot' },
      h('span', { class: 'car-tile-age' }, icon('pulse'), 'Last heard ', ago(c.last_activity_ms)),
      h('span', { class: 'muted' }, 'History kept')));
}

export async function mount(ctx) {
  const q = ctx.query;
  let sort = SORTS.some(([k]) => k === q.sort) ? q.sort : 'custom';
  const isAdmin = ctx.admin;
  const summaryP = ctx.api.get('fleet/summary', null, { maxAgeMs: 5000 }).catch(() => null); // the frame's answer of this load
  const attnP = ctx.api.get('fleet/attention', { limit: 50 }).then((r) => r.items || [], () => []);
  let pager = null;
  let attention = [];
  const tiles = new Map(); // car id -> { car, el }
  const anchor = h('div', { class: 'fleet-end w-full', hidden: true });

  const orderBtn = btn('Order', { kind: 'soft', icon: 'bars', cover: ['cars.list.order', 'faq.reorder-cars', 'web.settings.general.car-order'], onclick: (e) => openOrder(e.currentTarget) });
  const live = h('span', { class: 'chip tone-teal live-chip', cover: 'cars.list.live_refresh', title: 'Tiles follow the cars as values arrive' }, h('span', { class: 'dot', 'aria-hidden': 'true' }), h('span', null, 'Live'));
  ctx.head({ actions: [live, orderBtn] });

  const params = () => ({ sort: sort === 'custom' ? 'custom' : sort, dir: SORT_DIR[sort] });
  async function load() {
    pager = ctx.api.pager('cars', params(), { limit: 24 });
    await pager.more();
    return pager;
  }
  const first = await load();
  attention = await attnP;
  let soldCount = (await summaryP)?.sold;
  if (!ctx.alive()) return {};
  if (!first.items.length) return emptyFleet(ctx, isAdmin);

  // Sold cars wait at the end behind 'Sold cars (n)'; summary.sold counts them, else the ones loaded so far.
  let showSold = false;
  const soldBtn = btn('Sold cars', { kind: 'soft', icon: 'chevronDown', onclick: () => { showSold = !showSold; drawSold(); } });
  const soldBar = h('div', { class: 'fleet-sold-bar w-full' }, soldBtn);
  const soldBox = h('div', { class: 'fleet-sold w-full' }, soldBar); // its own grid, so sold tiles never fill gaps above
  const soldTiles = [];
  function drawSold() {
    const loaded = state.splitSold(pager.items).sold.length;
    const n = typeof soldCount === 'number' ? Math.max(soldCount, loaded) : loaded;
    soldBox.hidden = !n;
    soldBtn.querySelector('.btn-text').textContent = `${showSold ? 'Hide sold cars' : 'Sold cars'} (${n})`;
    soldBtn.setAttribute('aria-expanded', String(showSold));
    soldBtn.querySelector('.ic')?.replaceWith(icon(showSold ? 'chevronUp' : 'chevronDown'));
    for (const el of soldTiles) el.hidden = !showSold;
  }
  const more = moreButton(async () => { const before = pager.items.length; await pager.more(); drawFrom(before); return !pager.done; }, { text: 'Show more cars' });
  function drawFrom(start = 0) {
    if (start === 0) { tiles.clear(); soldTiles.length = 0; soldBox.replaceChildren(soldBar); ctx.view.replaceChildren(); }
    if (!more.isConnected) ctx.view.append(more);
    if (!soldBox.isConnected) ctx.view.append(soldBox);
    if (!missing.isConnected) ctx.view.append(missing);
    for (const c of pager.items.slice(start)) {
      if (state.isSold(c)) {
        const el = soldTile(c);
        soldTiles.push(el);
        soldBox.append(el);
        continue;
      }
      const el = carTile(c, attention);
      tiles.set(c.id, { car: c, el });
      ctx.view.insertBefore(el, more);
    }
    more.querySelector('button').hidden = pager.done;
    drawSold();
  }
  const missing = missingTile(ctx, isAdmin);
  drawFrom(0);
  await missing.done;

  /* ---------------------------------------------------------- the order sheet */
  let sheet = null;
  function openOrder(opener) {
    if (!sheet) {
      sheet = createSheet({ title: 'Order of the cars', cls: 'order-sheet' });
    }
    drawOrder();
    sheet.open(opener);
  }
  function drawOrder() {
    const seg = segmented({ label: 'Sort by', name: 'sort', value: sort, options: SORTS, onchange: async (val) => {
      sort = val;
      ctx.setQuery({ sort: val === 'custom' ? null : val });
      await load(); drawFrom(0); drawOrder();
    } });
    const rows = sort === 'custom' ? pager.items.filter((c) => !state.isSold(c)).map((c, i, all) => h('div', { class: 'row order-row' },
      h('span', { class: ['avatar', `st-${c.now?.state || 'unknown'}`], 'aria-hidden': 'true' }, (c.name || '?').slice(0, 1).toUpperCase()),
      h('span', { class: 'row-text' }, h('span', { class: 'row-name' }, c.name || 'Unnamed car'), h('span', { class: 'row-sub' }, [c.year, c.model].filter(Boolean).join(' '))),
      h('span', { class: 'row-side' },
        btn(`Move ${c.name} up`, { kind: 'ghost', icon: 'chevronUp', iconOnly: true, disabled: i === 0, onclick: () => move(i, -1) }),
        btn(`Move ${c.name} down`, { kind: 'ghost', icon: 'chevronDown', iconOnly: true, disabled: i === all.length - 1, onclick: () => move(i, 1) })))) : [];
    sheet.body.replaceChildren(seg,
      sort === 'custom' ? sheetGroup('Your order, kept with your account', rows) : h('p', { class: 'sheet-note' }, 'Choose “Your order” to move cars up and down.'),
      pager.done ? null : h('p', { class: 'sheet-note' }, 'Only the cars shown so far are listed; show more on the page to move the rest.'));
  }
  async function move(i, step) {
    // the order sheet lists the unsold cars; sold ones keep their place after them
    const { active: unsold, sold } = state.splitSold(pager.items);
    const list = unsold.map((c) => c.id);
    const j = i + step;
    if (j < 0 || j >= list.length) return;
    [list[i], list[j]] = [list[j], list[i]];
    list.push(...sold.map((c) => c.id));
    const pinned = new Set(currentPrefs()?.pinned_cars || []);
    try {
      await savePref({ car_order: list.filter((x) => !pinned.has(x)), pinned_cars: list.filter((x) => pinned.has(x)) });
    } catch { return; }
    await load(); drawFrom(0); drawOrder();
    ctx.toast('The new order is saved.');
  }

  /* ---------------------------------------------------------- live */
  let again = 0;
  ctx.onCleanup(() => clearTimeout(again));
  ctx.events(['now', 'fleet'], (ev) => {
    if (ev.type === 'fleet') {
      clearTimeout(again);
      again = setTimeout(async () => {
        if (ev.data?.change === 'sold') soldCount = (await ctx.api.get('fleet/summary', null, { maxAgeMs: 0 }).catch(() => null))?.sold;
        await load(); drawFrom(0);
      }, 500);
      return;
    }
    const d = ev.data || {};
    const t = tiles.get(d.car);
    if (!t || !d.now) return;
    // A delta carries only what changed: a { value, at_ms } pair sent as just { at_ms } kept its value.
    const now = { ...t.car.now };
    const put = (key, x) => {
      if (x && typeof x === 'object' && !Array.isArray(x)) { if ('value' in x) now[key] = x.value; }
      else now[key] = x;
    };
    for (const k of ['state', 'since_ms', 'at_ms', 'received_ms', 'stale', 'soc_pct', 'range_km', 'ideal_range_km']) if (k in d.now) put(k, d.now[k]);
    if (d.now.location && 'place' in d.now.location) {
      const p = d.now.location.place;
      now.place = p && typeof p === 'object' ? ('value' in p ? p.value : { ...(now.place || {}), ...p }) : p;
    }
    if (d.now.charge) {
      if ('plugged' in d.now.charge) put('plugged', d.now.charge.plugged);
      if ('power_kw' in d.now.charge) put('charge_power_kw', d.now.charge.power_kw);
    }
    if (now.state !== 'charging') now.charge_power_kw = null;
    t.car.now = now;
    const el = carTile(t.car, attention);
    el.querySelector('.ring-arc')?.classList.add('no-anim');
    t.el.replaceWith(el);
    t.el = el;
  });
  ctx.ready();
  return { update: () => { drawFrom(0); ctx.ready(); } };
}

/** Why a car may be missing: what waits in set-up, each linked to its fix (or who to ask). */
function fixRows(fixes, isAdmin) {
  return h('ul', { class: 'fix-list' }, fixes.map((f) => {
    const [t, text] = FIXES[f.cause] || [String(f.cause).replace(/_/g, ' '), ''];
    return h('li', { class: 'fix' },
      h('span', { class: 'item-ic tone-sun', 'aria-hidden': 'true' }, icon('warning')),
      h('span', { class: 'item-text' }, h('span', { class: 'item-title' }, t), h('span', { class: 'item-sub' }, text),
        isAdmin && f.link ? h('a', { class: 'fix-link', href: f.link }, 'Fix it') : !isAdmin ? h('span', { class: 'item-sub' }, 'Ask an admin to fix this.') : null));
  }));
}
function missingTile(ctx, isAdmin) {
  const body = h('div', { class: 'fix-box' }, note('Checking set-up…'));
  const el = tile({ w: 2, title: 'Missing a car?', icon: 'help', cls: 'missing-tile', cover: ['cars.empty.title', 'cars.empty.hint'] },
    h('p', { class: 'note' }, 'A car shows here once its CAN Bridge uploads its first log, or once a car of a Tesla account is matched to a new car (Settings: Integrations). You name it after that.'), body);
  el.done = ctx.api.get('setup/status').then((s) => {
    const fixes = (s && s.fixes) || [];
    body.replaceChildren(fixes.length ? fixRows(fixes, isAdmin) : note('Nothing is waiting: every upload token was used and every CAN Bridge key has its car.'));
  }, (err) => body.replaceChildren(note(`Set-up could not be checked: ${errorWords(err).title}.`)));
  return el;
}

async function emptyFleet(ctx, isAdmin) {
  const box = h('div', { class: 'fix-box' });
  ctx.view.replaceChildren(emptyTile({
    icon: 'car', title: 'No car is logged yet', cover: ['cars.empty.title', 'cars.empty.hint'],
    text: 'A car shows here when its CAN Bridge (SMO) uploads its first log. A Tesla account is another way: add one under Settings: Integrations and its cars are read from the Tesla API. You name a car after that.',
    action: isAdmin ? h('span', { class: 'form-actions' }, linkBtn('Make an upload token', '/settings/cars', { kind: 'primary', icon: 'key' }), linkBtn('Add a Tesla account', `${state.buildUrl('settings-integrations')}#tesla`, { kind: 'soft', icon: 'plug' })) : null,
  }));
  try {
    const s = await ctx.api.get('setup/status');
    if (s.tesla_accounts) ctx.view.append(tile({ w: 2, title: 'Tesla account', icon: 'plug' }, note(`${s.tesla_accounts === 1 ? 'A Tesla account is' : `${s.tesla_accounts} Tesla accounts are`} added. Its cars show here once each is matched to a new car.`), isAdmin ? linkBtn('Match its cars', `${state.buildUrl('settings-integrations')}#tesla`, { kind: 'soft', icon: 'car' }) : null));
    if ((s.fixes || []).length) { box.append(fixRows(s.fixes, isAdmin)); ctx.view.append(tile({ w: 2, title: 'What waits in set-up', icon: 'warning' }, box)); }
  } catch { /* the empty tile says enough */ }
  ctx.ready();
  return {};
}


