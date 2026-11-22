// Sentry Mate, "Pebble" UI pack: Timeline (/cars/{car}/timeline), everything the car did as small tiles by day.
//
// On top: the car and the period, kind pills (drives, charges, stops, gaps, updates) and a place search; a state
// tile (now, last change, share of time parked and asleep) and the period's activity strip. Then one header per
// day ('Tuesday') and a tile per row: an icon for drive, charge, park, gap or update, a one-line summary, the
// figures (odometer, distance, duration, range, energy, battery, temperature) and links to the drive or charge,
// the Trip for exactly that span, and the places. Data: GET /api/v1/cars/{car}/timeline (paged), /states, /activity.

import { h } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { formatTime } from '../shared/time.js';
import { formatList } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { tile, num, stateTag, chip, icon, note } from '../core/ui.js';
import { emptyTile } from '../core/states.js';
import { spanHref } from '../core/period.js';
import { AWAKE_WORDS, GAP_WORDS, endLabel, placeLink, byDay, dayHead, strip, stripShares, filterBar, moreButton, dur } from '../core/bits.js';
import { srcPill } from '../core/source.js';

export const id = 'timeline';
export const css = true;
export const shape = [['full', 'h1'], [2, 'h1'], [2, 'h1'], [2, 'h1'], [2, 'h1']];
export const title = () => 'Timeline';

const KINDS = [['drive', 'Drives', 'route', 'coral'], ['charge', 'Charges', 'bolt', 'teal'], ['parked', 'Stops', 'park', 'sky'], ['gap', 'Gaps', 'warning', 'bad'], ['update', 'Updates', 'chip', 'sun']];
const KIND = Object.fromEntries(KINDS.map(([k, name, ic, tone]) => [k, { name, ic, tone }]));
const signed = (val, kind) => (val == null ? '—' : `${val > 0 ? '+' : val < 0 ? '−' : ''}${format(Math.abs(val), kind)}`);

function summary(r) {
  const a = endLabel(r.start), b = r.end ? endLabel(r.end) : null;
  switch (r.kind) {
    case 'drive': return `${a} to ${b || 'now'}`;
    case 'charge': return `Charged at ${a}`;
    case 'parked': return `Parked at ${a}`;
    case 'gap': return `No data: ${GAP_WORDS[r.gap_reason] || 'reason not known'}`;
    case 'update': return 'Software update';
    default: return r.kind;
  }
}

function rowTile(r, ctx) {
  const tz = ctx.tz, car = ctx.car;
  const k = KIND[r.kind] || KIND.gap;
  const going = r.end_ms == null;
  const end = r.end_ms ?? api.now();
  const detail = r.ref?.drive ? state.buildUrl('drive', { car: car.id, drive: r.ref.drive, period: ctx.state.period })
    : r.ref?.charge ? state.buildUrl('charge', { car: car.id, charge: r.ref.charge, period: ctx.state.period })
      : r.ref?.update ? state.buildUrl('software', { car: car.id }) : null;
  const pm = r.parked_ms;
  const nums = [
    r.distance_km != null ? h('span', { class: 'tl-n', cover: 'timeline.col.distance' }, num(r.distance_km, 'distance', { size: 's' })) : null,
    h('span', { class: 'tl-n', cover: 'timeline.col.duration' }, icon('clock'), dur(r.duration_ms)),
    r.soc_end_pct != null ? h('span', { class: 'tl-n', cover: ['timeline.col.soc', 'timeline.col.soc_diff'] }, icon('battery'), `${format(r.soc_end_pct, 'percent')} (${signed(r.soc_delta_pct, 'percent')})`) : null,
    r.range_end_km != null ? h('span', { class: 'tl-n', cover: ['timeline.col.range', 'timeline.col.range_diff'] }, `${format(r.range_end_km, 'range')} range (${signed(r.range_delta_km, 'range')})`) : null,
    r.energy_delta_kwh != null ? h('span', { class: 'tl-n', cover: 'timeline.col.energy_diff' }, icon('bolt'), signed(r.energy_delta_kwh, 'energy'), r.energy_basis === 'estimated' ? ' estimated' : '') : null,
    r.outside_avg_c != null ? h('span', { class: 'tl-n', cover: 'timeline.col.temp' }, icon('thermo'), format(r.outside_avg_c, 'temperature')) : null,
    r.cost ? h('span', { class: 'tl-n' }, icon('coin'), format(r.cost.amount, 'money', { currency: r.cost.currency })) : null,
  ];
  const links = [
    detail ? h('a', { href: detail, cover: 'timeline.link.slot_details' }, r.kind === 'update' ? 'Software' : 'Details') : null,
    r.kind !== 'update' ? h('a', { href: spanHref('trip', { car: car.id, from: r.start_ms, to: Math.max(end, r.start_ms + 60000) }), cover: 'timeline.link.trip_span' }, 'Trip for this span') : null,
  ];
  const kinds = { drive: 'timeline.row.drive', charge: 'timeline.row.charging', parked: 'timeline.row.parking', gap: ['timeline.row.missing', 'timeline.row.parking_gap_overlap'], update: 'timeline.row.updating' };
  return tile({ w: 2, cls: ['item', 'tl-row', `tl-${r.kind}`], cover: kinds[r.kind] },
    h('div', { class: 'item-top' },
      h('span', { class: ['item-ic', `tone-${k.tone}`], 'aria-hidden': 'true' }, icon(k.ic)),
      h('div', { class: 'item-text' },
        h('p', { class: 'item-title', cover: 'timeline.col.action' }, summary(r)),
        h('p', { class: 'item-sub' },
          h('span', { cover: 'timeline.col.start' }, formatTime(r.start_ms, tz)), ' – ', h('span', { cover: 'timeline.col.end' }, going ? 'now' : formatTime(r.end_ms, tz)),
          r.odometer_km != null ? h('span', { cover: 'timeline.col.odometer' }, ` · ${format(r.odometer_km, 'odometer')}`) : null)),
      going ? chip('Now', { tone: k.tone }) : null),
    r.kind === 'drive' || r.kind === 'charge' || r.kind === 'parked' ? h('p', { class: 'route-words item-sub' },
      h('span', { cover: ['timeline.col.start_address', 'timeline.link.geofence_start', 'timeline.link.place_cells_other_rows'] }, placeLink(r.start)),
      r.end ? [icon('arrowRight', 'route-arrow'), h('span', { cover: ['timeline.col.end_address', 'timeline.link.geofence_end'] }, placeLink(r.end, { still: going }))] : null) : null,
    h('div', { class: 'item-nums tl-nums' }, nums),
    pm ? h('p', { class: 'item-sub' }, `Asleep ${dur(pm.asleep)}, awake ${dur((pm.awake || 0) + (pm.awake_vehicle_bus_only || 0))}${pm.unknown ? `, no data ${dur(pm.unknown)}` : ''}`,
      r.awake_reasons?.length ? `; awake for ${formatList(r.awake_reasons.map((x) => AWAKE_WORDS[x] || x.replace(/_/g, ' ')))}` : '', '.') : null,
    h('p', { class: 'item-links tile-foot' }, srcPill(r, { short: true, cls: 'tl-src' }), links));
}

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz;
  const qk = String(ctx.query.kinds || '').split(',').filter((k) => KIND[k]);
  const kinds = new Set(qk.length ? qk : KINDS.map(([k]) => k));
  let qtext = ctx.query.q || '';
  const [states, act] = await Promise.all([
    ctx.api.get(api.apiPath`cars/${car.id}/states`, { ...ctx.params, limit: 1 }).catch(() => null),
    ctx.api.get(api.apiPath`cars/${car.id}/activity`, ctx.params).catch(() => null),
  ]);
  if (!ctx.alive()) return {};

  const kindPills = h('div', { class: 'pills', role: 'group', 'aria-label': 'Show these kinds', cover: ['timeline.filter.action', 'timeline.table.column_filter'] },
    KINDS.map(([k, name, ic]) => h('button', { type: 'button', class: 'fpill fpill-ic', 'aria-pressed': String(kinds.has(k)), cover: k === 'gap' ? ['timeline.row.missing', 'timeline.row.parking_gap_overlap'] : k === 'update' ? 'timeline.row.updating' : null, onclick: (e) => {
      if (kinds.has(k) && kinds.size > 1) kinds.delete(k); else kinds.add(k);
      e.currentTarget.setAttribute('aria-pressed', String(kinds.has(k)));
      write(); reload();
    } }, icon(ic), name)));
  const search = h('input', { type: 'search', value: qtext, placeholder: 'Place or address', 'aria-label': 'Find a place or address', autocomplete: 'off' });
  let timer = 0;
  search.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(() => { qtext = search.value.trim(); write(); reload(); }, 250); });
  ctx.onCleanup(() => clearTimeout(timer));
  const bar = filterBar(ctx, { carCover: ['timeline.filter.car', 'states.filter.car'], periodCover: ['timeline.control.time_range', 'states.control.time_range'] },
    kindPills, h('label', { class: 'search-pill', cover: 'timeline.filter.address' }, icon('search'), search));
  const write = () => ctx.setQuery({ kinds: kinds.size === KINDS.length ? null : [...kinds].join(','), q: qtext || null });

  // the state tile and the strip
  const cur = states?.current;
  const share = states?.share_pct || {};
  const stTile = tile({ w: 2, title: 'Now', icon: 'pulse', cls: 'tl-state', cover: ['states.stat.current', 'shots.states'] },
    h('p', { class: 'tl-state-now' }, stateTag(car.now?.state || 'unknown', { stale: !!car.now?.stale }), cur?.since_ms ? h('span', { class: 'muted' }, ` since ${formatTime(cur.since_ms, tz)}`) : null),
    h('div', { class: 'tl-shares' },
      h('div', { cover: 'states.stat.parked_pct' }, h('span', { class: 'hero-k' }, 'Parked'), num(share.parked, 'percent')),
      h('div', null, h('span', { class: 'hero-k' }, 'Asleep'), num(share.asleep, 'percent')),
      h('div', null, h('span', { class: 'hero-k' }, 'Awake'), num(share.awake, 'percent')),
      h('div', { cover: 'states.stat.last_change' }, h('span', { class: 'hero-k' }, 'Last change'), h('span', { class: 'tl-change' }, states?.last_change_ms ? formatTime(states.last_change_ms, tz) : '—'))));
  const awake = tile({ w: 2, title: 'Why it stayed awake', icon: 'sun', cls: 'tl-awake', cover: ['faq.no-sleep.log-timeline', 'faq.no-sleep.state-image', 'faq.no-sleep.accessory', 'faq.no-sleep.mcu1', 'faq.no-sleep.other-loggers'] },
    note('Each stop below says how long the car slept and what kept it awake: Sentry, climate, someone inside, accessory power or a door. '),
    note('Other apps that poll the car keep it awake too; Sentry Mate only listens to the CAN bus, so it never wakes the car. Older cars with the first media computer sleep less.'));
  const stripTile = tile({ w: 'full', title: 'States', icon: 'timeline', cls: 'strip-tile', cover: ['states.chart.states', 'states.chart.legend', 'projects.dash'] },
    act ? strip(act, { tz, link: (s) => (s.ref?.drive ? state.buildUrl('drive', { car: car.id, drive: s.ref.drive }) : s.ref?.charge ? state.buildUrl('charge', { car: car.id, charge: s.ref.charge }) : null) }) : note('The states did not load.'),
    act ? note(stripShares(act).slice(0, 4).map(([k, ms]) => `${({ driving: 'Driving', charging: 'Charging', online: 'Awake', online_vehicle_bus_only: 'Bus awake', asleep: 'Asleep', unknown: 'No data', updating: 'Updating' })[k] || k} ${dur(ms)}`).join(' · ')) : null);

  const listStart = h('div', { class: 'tl-list-start w-full', cover: ['timeline.table', 'shots.timeline'] }, h('h2', { class: 'sr-only' }, 'Everything, newest first'));
  let pager = null;
  let lastDay = null;
  const more = moreButton(async () => { const before = pager.items.length; await pager.more(); drawFrom(before); return !pager.done; }, { text: 'Show earlier' });
  function drawFrom(start) {
    const items = pager.items.slice(start);
    const groups = byDay(items, tz);
    for (const g of groups) {
      if (g.key !== lastDay) {
        const all = pager.items.filter((r) => r.start_ms && byDay([r], tz)[0].key === g.key);
        const dist = all.reduce((s, r) => s + (r.kind === 'drive' ? r.distance_km || 0 : 0), 0);
        ctx.view.insertBefore(dayHead(g.key, tz, dist ? `${format(dist, 'distance')} driven` : null), more);
        lastDay = g.key;
      }
      for (const r of g.items) ctx.view.insertBefore(rowTile(r, ctx), more);
    }
    more.querySelector('button').hidden = pager.done;
  }
  async function reload() {
    pager = ctx.api.pager(api.apiPath`cars/${car.id}/timeline`, { ...ctx.params, kinds: [...kinds].join(','), q: qtext || undefined }, { limit: 30 });
    await pager.more();
    if (!ctx.alive()) return;
    while (listStart.nextSibling && listStart.nextSibling !== more) listStart.nextSibling.remove();
    lastDay = null;
    const empty = ctx.view.querySelector('.tl-empty');
    if (empty) empty.remove();
    if (!pager.items.length) {
      ctx.view.insertBefore(emptyTile({ icon: 'timeline', title: qtext ? `Nothing at “${qtext}” in this period` : 'Nothing in this period', text: 'Pick a longer period, or turn more kinds on above.' }), more).classList.add('tl-empty');
      more.querySelector('button').hidden = true;
      return;
    }
    drawFrom(0);
  }
  ctx.view.replaceChildren(bar, stTile, awake, stripTile, listStart, more);
  await reload();
  ctx.ready();
  return { onQuery: async () => { await reload(); } };
}


