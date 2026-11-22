// All cars (/): the garage. Every car as a cartridge with its own small cluster (state light, charge gauge,
// range, where it is, how old the data is), the state counts as warning lights that filter the garage, and
// the cars that need a look. The list is searched, sorted and paged by the server (GET /cars); pinned cars
// come first in "Your order" (preferences pinned_cars). Live: a car added, renamed or changing state redraws.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { formatNumber } from '../shared/i18n.js';
import { icon, hud, plate, btn, linkBtn, lamp, tag } from '../core/ui.js';
import { fuel } from '../core/gauges.js';
import { controls, slot, select, search, plural } from '../core/kit.js';
import { emptyState, errorWords } from '../core/states.js';
import { srcChip } from '../core/sources.js';
import { STATES, stateWord, carLine, agoEl, ago, when } from '../core/fmt.js';
import { getPrefs, savePrefs, isAdmin } from '../core/prefs.js';
import { toast } from '../core/layer.js';

export const id = 'fleet';
export const css = true;
export const title = () => 'All cars';

const ORDERS = [['custom', 'Your order'], ['last_activity', 'Last activity'], ['name', 'Name'], ['soc', 'Battery']];
const FIRST_DIR = { custom: 'asc', last_activity: 'desc', name: 'asc', soc: 'desc' };
const TYRE = { fl: 'Front left', fr: 'Front right', rl: 'Rear left', rr: 'Rear right' };
const DOOR = { fl: 'front left door', fr: 'front right door', rl: 'rear left door', rr: 'rear right door', trunk: 'trunk', frunk: 'frunk' };
const FIX = {
  no_token: ['No upload token yet', 'Make one in Car settings and enter it in the CAN Bridge app. A car appears when its first log arrives.'],
  token_unused: ['A token was never used', 'No CAN Bridge has uploaded with it yet. Check the server address and the token in the CAN Bridge app.'],
  key_waiting: ['A CAN Bridge key is waiting', 'A CAN Bridge uploads with a key that is not tied to a car yet. Assign it in Car settings.'],
  logs_waiting_trust: ['Logs are waiting for trust', 'Some logs wait until their signing key is trusted. Trust the key in Car settings.'],
  import_running: ['An import is still running', 'Its cars appear when it finishes. Follow it on the Import page.'],
};

function attention(a) {
  const d = a.detail || {};
  const c = { car: a.car };
  switch (a.code) {
    case 'stopped_mid_drive': return { text: 'Data stopped in the middle of a drive.', href: state.linkTo('drives', c), go: 'Drives', tone: 'org' };
    case 'stopped_mid_charge': return { text: 'Data stopped in the middle of a charge.', href: state.linkTo('charges', c), go: 'Charges', tone: 'org' };
    case 'upload_silence': return { text: `No upload since ${d.last_upload_ms ? ago(d.last_upload_ms) : 'a long time'}. The CAN Bridge may be off or out of range.`, href: state.linkTo('health', c), go: 'Data health', tone: 'org' };
    case 'trust_failed': return { text: `A log failed its check${d.reason ? ` (${String(d.reason).replace(/_/g, ' ')})` : ''}; that time shows as unknown.`, href: state.linkTo('health', c), go: 'Data health', tone: 'red' };
    case 'low_battery': return { text: `Battery low: ${format(d.soc_pct, 'percent')}.`, href: state.linkTo('car', c), go: 'Cluster', tone: 'red' };
    case 'tyre_low': return { text: `${TYRE[d.tyre] || 'A'} tyre at ${format(d.pressure_kpa, 'pressure')}; ${format(d.recommended_kpa, 'pressure')} recommended.`, href: state.linkTo('car', c), go: 'Cluster', tone: 'org' };
    case 'door_open_asleep': return { text: `The ${DOOR[d.door] || 'a door'} is open while the car sleeps.`, href: state.linkTo('car', c), go: 'Cluster', tone: 'org' };
    default: return { text: String(a.code || 'Something').replace(/_/g, ' '), href: state.linkTo('car', c), go: 'Cluster', tone: 'org' };
  }
}

export async function mount(ctx) {
  const prefs = getPrefs() || {};
  let pinned = [...(prefs.pinned_cars || [])];
  const q = ctx.query;
  let order = ORDERS.some(([v]) => v === q.sort) ? q.sort : (pinned.length || (prefs.car_order || []).length ? 'custom' : 'last_activity');
  let dir = (q.dir === 'asc' || q.dir === 'desc') && order !== 'custom' ? q.dir : FIRST_DIR[order];
  let only = Object.keys(STATES).includes(q.state) ? q.state : '';
  let text = q.search || '';

  const lightsEl = h('div', { class: 'gar-lights', role: 'group', 'aria-label': 'Cars by state' });
  const attnEl = h('div');
  const emptyEl = h('div', { hidden: true, cover: ['cars.empty.title', 'cars.empty.hint'] });
  const bay = h('div', { class: 'gar-bay', role: 'list', 'aria-label': 'Cars' });
  let showSold = q.sold === '1';
  const soldEl = h('div', { class: 'gar-sold', hidden: true });
  const count = h('span', { class: 'trk-count', 'aria-live': 'polite' });
  const moreBtn = btn('Load more cars', () => more(gen), { icon: 'chev-d', sm: true });
  const foot = h('div', { class: 'gar-foot', hidden: true }, moreBtn);
  const orderNote = h('span', { class: 'gar-order-note' });

  const orderSel = select(ORDERS.map(([v, l]) => [v, l]), order, (v) => { order = v; dir = FIRST_DIR[v]; apply(); }, { label: 'Order' });
  const dirBtn = btn(null, () => { dir = dir === 'asc' ? 'desc' : 'asc'; apply(); }, { icon: 'arrow-r', sm: true, label: 'Reverse the order', cls: 'gar-dir' });
  const stateSel = select([['', 'Every state'], ...Object.entries(STATES).map(([k, v]) => [k, v.word])], only, (v) => { only = v; apply(); }, { label: 'State' });
  const NOTE = {
    custom: 'Pinned cars first, in pin order, then by last activity. Pins are yours; everyone keeps their own.',
    last_activity: ['Newest activity first.', 'Oldest activity first.'], name: ['A to Z.', 'Z to A.'], soc: ['Highest battery first.', 'Lowest battery first.'],
  };
  const paintNote = () => {
    const n = NOTE[order];
    orderNote.textContent = Array.isArray(n) ? n[dir === FIRST_DIR[order] ? 0 : 1] : n;
    dirBtn.hidden = order === 'custom';
    dirBtn.classList.toggle('is-up', dir === 'asc');
  };
  paintNote();

  replace(ctx.view, h('div', { class: 'page gar' },
    plate({ kicker: 'Cars · the garage', title: 'All cars', aim: ctx.info.aim, tools: count }),
    lightsEl, attnEl, emptyEl,
    controls({ label: 'Find and order cars' },
      slot('Find', search(text, (v) => { text = v; apply(); }, { placeholder: 'Name, model or VIN start', label: 'Find a car', ctx }), { wide: true }),
      slot('State', stateSel),
      slot('Order', h('span', { class: 'gar-order' }, orderSel, dirBtn), { cover: ['cars.list.order', 'faq.reorder-cars', 'web.settings.general.car-order'] }),
      slot('', orderNote, { cls: 'gar-note-slot' })),
    h('section', { class: 'gar-cars', 'aria-label': 'The cars', cover: ['cars.list.cards', 'cars.list.live_refresh', 'faq.null-panels'] }, bay, foot, soldEl)));

  /* ---------- the cars */
  let pager = null, gen = 0, shown = [];
  const params = () => ({ search: text || null, state: only || null, sort: order, dir });

  async function setPinned(next, note) {
    const before = pinned;
    pinned = next;
    try { await savePrefs({ pinned_cars: next }); if (note) toast(note); load(true); } catch (e) { pinned = before; toast(`Could not change the order: ${e.message}`, { kind: 'error' }); }
  }

  function cartridge(c) {
    const n = c.now || {};
    const st = n.state || 'unknown';
    const i = pinned.indexOf(c.id);
    const on = i >= 0;
    const soc = n.soc_pct;
    const href = state.linkTo('car', { car: c.id });
    const doing = st === 'charging' && n.charge_power_kw != null ? `Charging at ${format(n.charge_power_kw, 'power')}` : n.plugged ? 'Plugged in' : st === 'driving' ? 'On the road' : null;
    const move = (d) => () => { const next = [...pinned]; const j = i + d; if (j < 0 || j >= next.length) return; [next[i], next[j]] = [next[j], next[i]]; setPinned(next); };
    return h('article', { class: ['bay-car', n.stale && 'is-stale'], role: 'listitem', dataset: { state: st } },
      h('div', { class: 'bay-car-top' },
        h('span', { class: 'bay-car-state' }, lamp(st, { stale: !!n.stale }), h('span', null, n.stale && st !== 'unknown' ? `Last known: ${stateWord(st).toLowerCase()}` : stateWord(st))),
        h('span', { class: 'bay-car-pins' },
          on && order === 'custom' ? btn(null, move(-1), { icon: 'chev-l', sm: true, kind: 'ghost', label: `Move ${c.name || 'this car'} up`, disabled: i === 0, cls: 'bay-car-move' }) : null,
          on && order === 'custom' ? btn(null, move(1), { icon: 'chev-r', sm: true, kind: 'ghost', label: `Move ${c.name || 'this car'} down`, disabled: i === pinned.length - 1, cls: 'bay-car-move' }) : null,
          h('button', { class: ['btn', 'btn--sm', 'btn--icon', 'btn--ghost', 'bay-car-pin', on && 'is-on'], type: 'button', 'aria-pressed': String(on),
            'aria-label': `${on ? 'Unpin' : 'Pin'} ${c.name || 'this car'}`, title: on ? 'Unpin: list it by last activity' : 'Pin: list it first in Your order',
            onclick: () => setPinned(on ? pinned.filter((x) => x !== c.id) : [...pinned, c.id], on ? `${c.name} unpinned.` : `${c.name} pinned: it comes first in Your order.`) }, icon('flag', 'i--sm')))),
      h('a', { class: 'bay-car-name', href }, h('span', { class: 'chrome ell' }, c.name || `Unnamed ${c.model || 'car'}`)),
      h('p', { class: 'bay-car-line' }, [carLine(c), c.trim].filter(Boolean).join(' · ') || 'Model not known yet',
        c.vin_prefix ? h('span', { class: 'bay-car-vin', title: 'VIN start (the serial number is never stored)' }, c.vin_prefix) : null,
        !c.name && isAdmin() ? [' · ', h('a', { href: state.buildUrl('settings-cars', { car: c.id }) }, 'Name it')] : null),
      h('div', { class: 'bay-car-gauge' },
        h('div', { class: 'bay-car-soc' }, h('span', { class: 'bay-car-soc-val' }, soc == null ? '—' : format(soc, 'percent')), h('span', { class: 'bay-car-soc-range' }, n.range_km == null ? 'range not known' : `${format(n.range_km, 'range')} range`)),
        fuel({ fraction: soc == null ? null : soc / 100, cells: 12, label: `${c.name || 'Car'} battery` })),
      h('dl', { class: 'bay-car-facts' },
        h('div', null, h('dt', null, 'Where'), h('dd', { class: 'ell' }, n.place?.name || (st === 'driving' ? 'Driving' : 'No saved place'))),
        h('div', null, h('dt', null, st === 'unknown' ? 'State' : 'Since'), h('dd', null, n.since_ms ? when(n.since_ms, c.time_zone) : 'no data yet')),
        h('div', null, h('dt', null, 'Data'), h('dd', { class: ['ell', n.stale && 'warn'] }, n.at_ms ? agoEl(n.at_ms) : 'none yet'))),
      srcChip(c) ? h('p', { class: 'bay-car-src' }, srcChip(c)) : null,
      doing ? h('p', { class: 'bay-car-doing' }, icon(st === 'charging' ? 'bolt' : st === 'driving' ? 'route' : 'plug', 'i--sm'), h('span', null, doing)) : null,
      h('a', { class: 'bay-car-open', href, 'aria-label': `Open the cluster of ${c.name || 'this car'}` }, h('span', null, 'Open cluster'), icon('arrow-r', 'i--sm')));
  }

  // a sold car: muted, marked Sold, no live state, no gauge
  function soldCartridge(c) {
    const href = state.linkTo('car', { car: c.id });
    return h('article', { class: 'bay-car is-sold', role: 'listitem' },
      h('div', { class: 'bay-car-top' }, h('span', { class: 'bay-car-state' }, tag('Sold'), h('span', null, c.sold_date ? `Sold on ${c.sold_date}` : 'Sold'))),
      h('a', { class: 'bay-car-name', href }, h('span', { class: 'chrome ell' }, c.name || `Unnamed ${c.model || 'car'}`)),
      h('p', { class: 'bay-car-line' }, [carLine(c), c.trim].filter(Boolean).join(' · ') || 'Model not known yet',
        c.vin_prefix ? h('span', { class: 'bay-car-vin', title: 'VIN start (the serial number is never stored)' }, c.vin_prefix) : null),
      h('p', { class: 'bay-car-src faint small' }, 'Its history stays browsable. Not counted in the totals or the lights.'),
      h('a', { class: 'bay-car-open', href, 'aria-label': `Open the history of ${c.name || 'this car'}` }, h('span', null, 'Open history'), icon('arrow-r', 'i--sm')));
  }

  function paintSold(sold) {
    const n = Math.max(sold.length, text ? 0 : (lastSummary?.sold || 0));
    soldEl.hidden = !!only || n === 0;
    if (soldEl.hidden) { replace(soldEl); return; }
    const toggle = h('button', { class: 'btn btn--sm btn--ghost gar-sold-toggle', type: 'button', 'aria-expanded': String(showSold),
      onclick: () => { showSold = !showSold; ctx.setQuery({ sold: showSold ? '1' : null }); paintSold(sold); } },
    icon(showSold ? 'chev-d' : 'chev-r', 'i--sm'), h('span', null, `Sold cars (${formatNumber(n)})`));
    replace(soldEl, h('div', { class: 'gar-sold-head' }, toggle, h('span', { class: 'faint small' }, 'Kept with their history; left out of the garage, the totals and the lights.')),
      showSold ? h('div', { class: 'gar-bay gar-bay--sold', role: 'list', 'aria-label': 'Sold cars' }, sold.length ? sold.map(soldCartridge) : h('p', { class: 'faint small' }, 'Load more cars to see the rest.')) : null);
  }

  function paintCars() {
    const { active, sold } = state.splitSold(shown);
    paintSold(sold);
    replace(bay, active.length ? active.map(cartridge) : (lastSummary?.cars ? h('div', { class: 'gar-none hud' }, emptyState({ icon: 'search', title: 'No car matches',
      text: `Nothing matches${text ? ` "${text}"` : ''}${only ? ` in the state ${stateWord(only).toLowerCase()}` : ''}. Clear the search or pick Every state.`,
      actions: btn('Show every car', () => { text = ''; only = ''; ctx.view.querySelector('.ctl-search input').value = ''; stateSel.value = ''; apply(); }, { icon: 'grid' }) })) : null));
    const total = pager?.last?.total;
    count.textContent = total != null && !text && !only && lastSummary?.sold != null ? plural(Math.max(0, total - lastSummary.sold), 'car') : plural(active.length, 'car');
    foot.hidden = !!pager?.done;
  }

  async function load(quiet = false) {
    const my = ++gen;
    pager = ctx.api.pager('cars', params(), { limit: 24 });
    if (!quiet) { shown = []; replace(bay, Array.from({ length: 3 }, () => h('div', { class: 'bay-car cart--skel hud', 'aria-hidden': 'true' }, h('span', { class: 'skel skel--title' }), h('span', { class: 'skel skel--line' }), h('span', { class: 'skel skel--line' })))); }
    await more(my, true);
  }
  async function more(my, fresh = false) {
    moreBtn.disabled = true;
    try {
      const items = await pager.more();
      if (my !== gen || !ctx.alive()) return;
      shown = fresh ? [...items] : [...shown, ...items];
      paintCars();
    } catch (e) {
      if (my !== gen || !ctx.alive() || e.name === 'AbortError') return;
      if (fresh && shown.length) return;
      const w = errorWords(e);
      replace(bay, h('div', { class: 'gar-none hud' }, emptyState({ icon: w.icon, title: w.title, text: w.text, actions: btn('Try again', () => load(), { icon: 'arrow-r' }) })));
    } finally { if (my === gen) moreBtn.disabled = false; }
  }

  function apply() {
    paintNote();
    ctx.setQuery({ search: text || null, state: only || null, sort: order, dir: dir === FIRST_DIR[order] ? null : dir });
    paintLights(lastSummary);
    return load();
  }

  /* ---------- the warning lights and the cars that need a look */
  let lastSummary = null;
  function paintLights(s) {
    if (!s) return;
    const b = s.by_state || {};
    const light = (key, ic, value, sub) => {
      const pressed = only === key;
      return h('button', { class: ['gar-light', value > 0 && 'is-lit'], type: 'button', dataset: { state: key }, 'aria-pressed': String(pressed),
        title: pressed ? 'Show every state' : `Show only the cars ${stateWord(key).toLowerCase()}`, onclick: () => { only = pressed ? '' : key; stateSel.value = only; apply(); } },
      h('span', { class: 'gar-light-ic' }, icon(ic)), h('span', { class: 'gar-light-txt' }, h('span', { class: 'gar-light-n' }, formatNumber(value)), h('span', { class: 'gar-light-word' }, STATES[key].word)), h('span', { class: 'gar-light-sub' }, sub));
    };
    const n = s.cars || 0;
    const share = (v) => (n ? `${Math.round((v / n) * 100)} % of the cars` : 'no cars');
    replace(lightsEl,
      light('driving', 'route', b.driving || 0, share(b.driving || 0)),
      light('charging', 'bolt', b.charging || 0, share(b.charging || 0)),
      light('parked', 'pin', b.parked || 0, 'awake, not moving'),
      light('asleep', 'moon', b.asleep || 0, 'sleeping'),
      light('unknown', 'signal', b.unknown || 0, 'no recent log'),
      h('div', { class: ['gar-light', 'gar-light--stale', s.stale && 'is-lit'], title: 'Cars with no upload for over an hour. Their last known state is shown and counted above too.' },
        h('span', { class: 'gar-light-ic' }, icon('clock')), h('span', { class: 'gar-light-txt' }, h('span', { class: 'gar-light-n' }, formatNumber(s.stale || 0)), h('span', { class: 'gar-light-word' }, 'Stale')), h('span', { class: 'gar-light-sub' }, 'no upload for 1 h')));
  }

  async function loadTop(fresh = false) {
    const [s, at, setup] = await Promise.all([
      ctx.api.get('fleet/summary'),
      ctx.api.page('fleet/attention', {}, { limit: 8 }).catch(() => ({ items: [], next: null })),
      isAdmin() ? ctx.api.get('setup/status').catch(() => null) : Promise.resolve(null),
    ]);
    if (!ctx.alive()) return;
    lastSummary = s;
    paintLights(s);
    const fixes = (setup?.fixes || []).filter((f) => FIX[f.cause]);
    if (at.items.length || fixes.length) {
      const names = new Map();
      await Promise.all([...new Set(at.items.map((a) => a.car))].map(async (cid) => {
        try { names.set(cid, (await ctx.api.get(`cars/${encodeURIComponent(cid)}`, null, fresh ? {} : { maxAgeMs: 60000 })).name); } catch { names.set(cid, cid); }
      }));
      if (!ctx.alive()) return;
      replace(attnEl, hud({ title: 'Needs a look', icon: 'warn', tone: 'orange', cls: 'gar-attn', side: at.next ? h('span', { class: 'trk-count' }, 'more in the cars below') : null },
        h('ul', { class: 'attn' },
          at.items.map((a) => {
            const t = attention(a);
            return h('li', { class: 'attn-row', dataset: { tone: t.tone } },
              h('span', { class: 'attn-light', 'aria-hidden': 'true' }, icon('warn', 'i--sm')),
              h('a', { class: 'attn-car', href: state.linkTo('car', { car: a.car }) }, names.get(a.car) || a.car),
              h('span', { class: 'attn-text' }, t.text),
              h('span', { class: 'attn-when' }, a.since_ms ? ago(a.since_ms) : ''),
              linkBtn(t.go, t.href, { sm: true, kind: 'ghost', icon: 'arrow-r' }));
          }),
          fixes.map((f) => h('li', { class: 'attn-row', dataset: { tone: 'cyan' } },
            h('span', { class: 'attn-light', 'aria-hidden': 'true' }, icon('sliders', 'i--sm')),
            h('span', { class: 'attn-car' }, 'Set-up'),
            h('span', { class: 'attn-text' }, `${FIX[f.cause][0]}. ${FIX[f.cause][1]}`),
            h('span', { class: 'attn-when' }),
            linkBtn(f.cause === 'import_running' ? 'Import' : 'Car settings', f.link || '/settings/cars', { sm: true, kind: 'ghost', icon: 'arrow-r' }))))));
    } else replace(attnEl);

    const cars = s.cars || 0;
    emptyEl.hidden = cars > 0;
    const teslaN = setup?.tesla_accounts || 0;
    replace(emptyEl, hud({ cls: 'gar-empty', tone: 'cyan', pad: false }, emptyState({
      icon: 'car', title: 'No cars in the garage yet',
      text: isAdmin() ? (teslaN
        ? `A car shows up here by itself when its CAN Bridge (SMO) uploads its first log, or once you match a car of the Tesla account${teslaN === 1 ? '' : 's'} already added to a car here or as a new car.`
        : 'A car shows up here by itself when its CAN Bridge (SMO) uploads its first log. You can also add a Tesla account, so the server reads its cars from the Tesla API as a second live input.')
        : 'Your account cannot see any car yet. Ask an admin to give it one.',
      actions: isAdmin() ? [linkBtn('Car settings', state.buildUrl('settings-cars'), { icon: 'sliders', kind: 'go' }),
        linkBtn(teslaN ? 'Match Tesla cars' : 'Add a Tesla account', `${state.buildUrl('settings-integrations')}#tesla`, { icon: 'key', kind: 'ghost' })] : null }),
    fixes.length ? h('ul', { class: 'gar-causes' }, fixes.map((f) => h('li', null, icon('chev-r', 'i--sm'), h('span', null, h('b', null, FIX[f.cause][0]), `: ${FIX[f.cause][1]} `, h('a', { href: f.link || '/settings/cars' }, 'Fix it'))))) : null));
  }

  try { await Promise.all([loadTop(), load()]); } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  paintCars();
  ctx.ready();

  // live: a 'fleet' event (added, removed, renamed) within 300 ms; 'now' changes at most every 10 s
  let pend = 0, fleetSeen = false;
  ctx.events(['fleet', 'now'], (ev) => {
    const isFleet = ev.type === 'fleet';
    if (pend && !(isFleet && !fleetSeen)) return;
    if (isFleet) { fleetSeen = true; clearTimeout(pend); }
    pend = setTimeout(async () => {
      pend = 0;
      const f = fleetSeen; fleetSeen = false;
      try { await loadTop(f); } catch { /* the next event tries again */ }
      if (ctx.alive() && (f || shown.length <= 24)) load(true);
    }, isFleet ? 300 : 10000);
  });
  ctx.onCleanup(() => clearTimeout(pend));
}
