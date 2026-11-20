// Timeline (/cars/{car}/timeline): everything the car did, as a tape. The state meters (now, last change,
// parked, asleep, awake, unknown), the activity strip, then the entries newest first, a day per side of the
// tape: drives, charges, parked spells (asleep, awake and unknown apart), gaps with their reason, and
// software updates. Filters: kinds, a place, and a text that only hides rows. Each entry's time opens Trip
// for exactly its span, its kind opens the drive, charge or software page, and place names open the editor.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, dayKey } from '../shared/time.js';
import { timeColumn, csvFilename } from '../shared/csv.js';
import { icon, hud, plate, btn, lamp, tag } from '../core/ui.js';
import { controls, slot, select, search, periodBtn, carBtn, csvBtn, placeLink, stat, rack, plural } from '../core/kit.js';
import { strip, KINDS as STRIP } from '../core/strip.js';
import { emptyState, errorWords } from '../core/states.js';
import { when, ago, stateWord } from '../core/fmt.js';

export const id = 'timeline';
export const css = true;

const KIND = {
  drive: { word: 'Drive', many: 'Drives', icon: 'route', tone: 'driving' },
  charge: { word: 'Charge', many: 'Charges', icon: 'bolt', tone: 'charging' },
  parked: { word: 'Parked', many: 'Parked', icon: 'pin', tone: 'parked' },
  gap: { word: 'Gap', many: 'Gaps', icon: 'question', tone: 'unknown' },
  update: { word: 'Update', many: 'Updates', icon: 'chip', tone: 'updating' },
};
const GAP = {
  no_log: 'No log arrived for this time.',
  not_recorded: 'Recording was paused on the CAN Bridge.',
  failed_check: 'A log failed its check, so its data is not used.',
  dropped_frames: 'The log lost frames here.',
};
const AWAKE = {
  sentry: 'Sentry', keep_climate_on: 'keep climate', keep_climate_dog: 'dog mode', keep_climate_camp: 'camp mode', user_present: 'someone inside',
  preconditioning_cabin: 'cabin preconditioning', preconditioning_battery: 'battery preconditioning', charging: 'charging', keep_warm: 'battery kept warm',
  door_open: 'a door open', accessory_power: 'accessory power',
};
const signed = (v, kind) => (v == null ? null : format(v, kind, { sign: true }));

function entry(r, { car, tz }) {
  const k = KIND[r.kind] || KIND.gap;
  const trip = state.buildUrl('trip', { car, period: { from: r.start_ms, to: r.end_ms ?? null } });
  const detail = r.ref?.drive ? state.buildUrl('drive', { car, drive: r.ref.drive }) : r.ref?.charge ? state.buildUrl('charge', { car, charge: r.ref.charge })
    : r.kind === 'update' ? state.buildUrl('software', { car }) : null;
  const est = r.energy_basis === 'estimated' ? ' (estimated)' : '';
  let what;
  const facts = [];
  if (r.kind === 'drive') {
    what = h('span', { class: 'route' }, placeLink(r.start), icon('arrow-r', 'i--sm route-to'), r.end ? placeLink(r.end) : h('span', { class: 'plc plc--bare' }, r.end_ms == null ? 'driving now' : 'not known'));
    facts.push(format(r.distance_km, 'distance'), r.energy_delta_kwh != null ? `${format(-r.energy_delta_kwh, 'energy')} used${est}` : null);
  } else if (r.kind === 'charge') {
    what = h('span', null, 'Charging at ', placeLink(r.start, { text: r.start ? undefined : 'an unknown spot' }));
    facts.push(r.energy_delta_kwh != null ? `${format(r.energy_delta_kwh, 'energy')} added${est}` : null, r.range_delta_km != null ? `${signed(r.range_delta_km, 'range')} range` : null, r.cost ? format(r.cost, 'money') : null);
  } else if (r.kind === 'parked') {
    what = r.start ? placeLink(r.start) : h('span', null, 'Parked');
    const pm = r.parked_ms || {};
    facts.push(pm.asleep ? `asleep ${formatDuration(pm.asleep)}` : null, pm.awake ? `awake ${formatDuration(pm.awake)}` : null,
      pm.awake_vehicle_bus_only ? `vehicle bus only ${formatDuration(pm.awake_vehicle_bus_only)}` : null, pm.unknown ? `unknown ${formatDuration(pm.unknown)}` : null,
      r.energy_delta_kwh != null ? `${format(Math.abs(r.energy_delta_kwh), 'energy')} used${est}` : null);
  } else if (r.kind === 'gap') {
    what = h('span', { class: 'route' }, r.start ? placeLink(r.start) : null, r.start && r.end ? icon('arrow-r', 'i--sm route-to') : null, r.end ? placeLink(r.end) : null,
      h('span', { class: 'tl-nodata' }, r.distance_km ? `No data · moved ${format(r.distance_km, 'distance')}` : 'No data'));
    facts.push(GAP[r.gap_reason] || 'No data for this time.', r.distance_km ? 'The car moved, but no drive was recorded. A gap never counts as parked time.' : null);
  } else {
    what = h('span', null, 'Software update', r.version ? ` to ${r.version}` : '');
  }
  if (r.soc_end_pct != null && r.kind !== 'gap' && r.kind !== 'update') facts.push(`${format(r.soc_end_pct - (r.soc_delta_pct || 0), 'percent')} → ${format(r.soc_end_pct, 'percent')} (${signed(r.soc_delta_pct, 'percent')})`);
  if (r.range_end_km != null && r.kind !== 'gap') facts.push(`range ${format(r.range_end_km, 'range')}${r.range_delta_km != null && r.kind !== 'charge' ? ` (${signed(r.range_delta_km, 'range')})` : ''}`);
  if (r.outside_avg_c != null) facts.push(`${format(r.outside_avg_c, 'temperature')} outside`);
  if (r.odometer_km != null) facts.push(`odometer ${format(r.odometer_km, 'odometer')}`);
  const awake = r.kind === 'parked' && r.awake_reasons?.length ? r.awake_reasons.map((x) => AWAKE[x] || x.replace(/_/g, ' ')) : null;
  const dur = r.duration_ms ?? (r.end_ms ? r.end_ms - r.start_ms : null);
  return h('li', { class: ['tl-row', r.source === 'teslamate' && 'is-imported'], dataset: { tone: k.tone, kind: r.kind } },
    h('a', { class: 'tl-when', href: trip, title: 'Open exactly this span as a trip' },
      h('span', { class: 'tl-t0' }, formatTime(r.start_ms, tz)), h('span', { class: 'tl-t1' }, r.end_ms ? formatTime(r.end_ms, tz) : 'now')),
    h('span', { class: 'tl-led', 'aria-hidden': 'true' }, icon(k.icon, 'i--sm')),
    h('div', { class: 'tl-body' },
      h('p', { class: 'tl-what' },
        detail ? h('a', { class: 'tl-kind', href: detail, title: `Open the ${k.word.toLowerCase()}` }, k.word) : h('span', { class: 'tl-kind' }, k.word), what),
      h('p', { class: 'tl-facts' }, facts.filter(Boolean).join(' · ')),
      awake ? h('p', { class: 'tl-awake' }, `Kept awake by ${awake.join(', ')}`) : null,
      r.source === 'teslamate' ? h('p', null, tag('Imported from TeslaMate', 'org')) : null),
    h('span', { class: 'tl-dur' }, dur != null ? formatDuration(dur) : ''));
}

export async function mount(ctx) {
  const { car, tz, period } = ctx;
  const cid = car.id;
  const from = period.from_ms, to = period.to_ms;
  const q = ctx.query;
  const kinds = new Set((q.kinds || 'drive,charge,parked,gap,update').split(',').filter((x) => KIND[x]));
  let placeId = q.place || '';
  let text = q.q || '';

  const [states, act] = await Promise.all([
    ctx.api.get(`cars/${encodeURIComponent(cid)}/states`, { from_ms: from, to_ms: to, limit: 1 }),
    ctx.api.get(`cars/${encodeURIComponent(cid)}/activity`, { from_ms: from ?? 0, to_ms: to }),
  ]);
  if (!ctx.alive()) return;

  /* ---------- filters */
  const chips = h('div', { class: 'chips', role: 'group', 'aria-label': 'Kinds of entry', cover: ['timeline.filter.action', 'timeline.table.column_filter'] },
    Object.entries(KIND).map(([k, v]) => h('button', { class: 'chip', type: 'button', 'aria-pressed': String(kinds.has(k)), dataset: { tone: v.tone },
      onclick: (e) => { if (kinds.has(k) && kinds.size === 1) return; if (kinds.has(k)) kinds.delete(k); else kinds.add(k); e.currentTarget.setAttribute('aria-pressed', String(kinds.has(k))); reload(); } },
    h('i', { class: 'chip-dot', vars: { '--c': 'var(--tc)' } }), v.many)));
  const placeSel = select([['', 'Every place']], placeId, (v) => { placeId = v; reload(); }, { label: 'Place' });
  ctx.api.page('places', { sort: 'visits' }, { limit: 100 }).then((pg) => {
    if (!ctx.alive()) return;
    placeSel.append(...pg.items.map((p) => h('option', { value: p.id, selected: p.id === placeId }, p.name)));
    if (placeId && !pg.items.some((p) => p.id === placeId)) placeSel.append(h('option', { value: placeId, selected: true, dataset: { link: '1' } }, 'The place in the link'));
  }).catch(() => {});

  /* ---------- state meters */
  const share = states.share_pct || {};
  const cur = states.current || {};
  const live = { ...(car.now || {}) };
  const nowSlot = h('div');
  function paintNow() {
    const st = live.state || (cur.state === 'online' ? 'parked' : cur.state) || 'unknown';
    const since = live.since_ms ?? cur.since_ms;
    replace(nowSlot, stat('Now', h('span', { class: 'tl-now' }, lamp(st, { stale: !!live.stale }), h('span', null, stateWord(st))), null, { cover: 'states.stat.current',
      how: [since ? `since ${when(since, tz)}` : null, live.stale ? 'last known; no upload for over an hour' : null, st === 'unknown' ? 'no recent log (not the same as asleep)' : null].filter(Boolean).join(' · ') }));
  }
  paintNow();
  const meter = (label, v, how, cover) => h('div', { class: 'tl-meter', cover },
    stat(label, v, 'percent', { how }), h('span', { class: 'tl-meter-bar', 'aria-hidden': 'true' }, h('i', { vars: { '--w': `${Math.max(0, Math.min(100, v ?? 0)).toFixed(1)}%` } })));

  const stripFrom = from ?? (act.items?.[0]?.start_ms ?? to - 86400000);
  const list = h('ol', { class: 'tl-tape', 'aria-label': 'Entries, newest first' });
  const count = h('span', { class: 'trk-count', 'aria-live': 'polite' });
  const csvSlot = h('span');
  const moreBtn = btn('Older entries', () => more(gen), { icon: 'chev-d', sm: true });
  const foot = h('div', { class: 'trk-foot', hidden: true }, h('span', { class: 'trk-foot-text' }, 'There are older entries in this period.'), moreBtn);

  replace(ctx.view, h('div', { class: 'page tl', cover: ['shots.states', 'shots.timeline'] },
    plate({ kicker: `Cars · ${car.name}`, title: 'Timeline', aim: ctx.info.aim, tools: carBtn(ctx, ['timeline.filter.car', 'states.filter.car']) }),
    controls({ label: 'Filters', note: 'The text box only hides entries; it never changes the figures of the ones it keeps.' },
      slot('Period', periodBtn(ctx, ['timeline.control.time_range', 'states.control.time_range'])),
      slot('Show', chips),
      slot('Place', placeSel, { cover: 'timeline.filter.address' }),
      slot('Text', search(text, (v) => { text = v; reload(); }, { placeholder: 'Hide entries without this text', ctx }), { wide: true })),
    hud({ title: 'States', icon: 'moon', cls: 'tl-states' },
      h('div', { class: 'tl-meters' },
        nowSlot,
        stat('Last change', states.last_change_ms ? when(states.last_change_ms, tz) : null, null, { cover: 'states.stat.last_change', how: states.last_change_ms ? ago(states.last_change_ms) : null, why: 'none in the period' }),
        meter('Parked', share.parked, 'of the known time, not driving', 'states.stat.parked_pct'),
        meter('Asleep', share.asleep, 'both buses quiet'),
        meter('Awake', share.awake, 'driving included'),
        meter('Unknown', share.unknown, 'no log, paused or a failed check')),
      h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, 'The shares overlap on purpose (parked time includes asleep time), so they need not add up to 100 %.'))),
    hud({ title: 'What the car did', icon: 'timeline', cover: ['states.chart.states', 'states.chart.legend'],
      side: csvBtn({ name: [car.name, 'activity', period.label], rows: () => act.items || [], columns: [
        { label: 'State', value: (s) => (STRIP[s.kind] || STRIP.unknown).word }, timeColumn('Start', 'start_ms', tz), timeColumn('End', 'end_ms', tz),
        { label: 'Minutes', value: (s) => (s.end_ms != null ? Math.round((s.end_ms - s.start_ms) / 60000) : null) }, { label: 'Source', value: (s) => s.source || '' }] }) },
    strip(act.items || [], stripFrom, to, { tz, now: ctx.api.now(), tall: true }),
    from == null && act.items?.length ? h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, `The whole history, from the first data on ${formatDate(stripFrom, tz, { year: true })}.`)) : null),
    hud({ title: 'Entries, newest first', icon: 'timeline', pad: false, side: [count, csvSlot], cls: 'tl-entries',
      cover: ['timeline.table', 'projects.dash', 'timeline.row.drive', 'timeline.row.charging', 'timeline.row.parking', 'timeline.row.missing', 'timeline.row.parking_gap_overlap', 'timeline.row.updating',
        'timeline.col.start', 'timeline.col.end', 'timeline.col.action', 'timeline.col.start_address', 'timeline.col.end_address', 'timeline.col.odometer', 'timeline.col.duration', 'timeline.col.distance',
        'timeline.col.range', 'timeline.col.range_diff', 'timeline.col.energy_diff', 'timeline.col.soc', 'timeline.col.soc_diff', 'timeline.col.temp',
        'timeline.link.trip_span', 'timeline.link.slot_details', 'timeline.link.geofence_start', 'timeline.link.geofence_end', 'timeline.link.place_cells_other_rows'] },
    h('p', { class: 'trk-note' }, icon('info', 'i--sm'), h('span', null, 'A time opens that span as a trip, a kind opens its drive, charge or software page, and a place opens the place editor.')),
    list, foot),
    h('details', { class: 'hud hud--plain fold tl-help', open: true },
      h('summary', null, icon('question'), h('span', null, 'Why does a car stay awake?'), icon('chev-d', 'i--sm')),
      h('div', { class: 'fold-body faq' },
        h('div', { class: 'faq-item', cover: 'faq.no-sleep.log-timeline' }, h('h3', null, 'What normal looks like'), h('p', null, 'After a drive the car stays awake for a while, then both buses go quiet and the entry turns to asleep. Short awake spells while parked are normal: the car checks its 12 V battery.')),
        h('div', { class: 'faq-item', cover: 'faq.no-sleep.state-image' }, h('h3', null, 'Reading the strip'), h('p', null, 'Dim lilac is asleep, violet is parked and awake, striped violet is awake on the vehicle bus only, hatched grey is unknown. A long violet stretch while parked means something kept the car awake; the parked entry says what.')),
        h('div', { class: 'faq-item', cover: 'faq.no-sleep.accessory' }, h('h3', null, 'Accessory power'), h('p', null, 'With accessory power kept on, the car stays awake even with nothing plugged in. Turn it off on the car if you do not need it.')),
        h('div', { class: 'faq-item', cover: 'faq.no-sleep.mcu1' }, h('h3', null, 'Older Model S and X'), h('p', null, 'Cars with the older media computer sleep less deeply and wake more often; expect more awake time than on newer cars.')),
        h('div', { class: 'faq-item', cover: 'faq.no-sleep.other-loggers' }, h('h3', null, 'Other apps'), h('p', null, 'Sentry Mate only listens to the car, so it never keeps it awake. An app that asks Tesla’s servers for the car’s data can wake it; look for awake spells in the parked entries while such an app runs.'))))));

  /* ---------- the tape */
  let pager = null, gen = 0, shown = 0, lastDay = null, side = null;
  const params = () => ({ from_ms: from, to_ms: to, kinds: [...kinds].join(','), place: placeId || null, q: text || null });
  async function more(my) {
    moreBtn.disabled = true;
    try {
      const items = await pager.more();
      if (my !== gen || !ctx.alive()) return;
      if (!shown) list.replaceChildren();
      for (const r of items) {
        const dk = dayKey(r.start_ms, tz);
        if (dk !== lastDay) {
          lastDay = dk;
          side = h('ol', { class: 'tl-side' });
          list.append(h('li', { class: 'tl-day' }, h('h3', { class: 'tl-day-head' }, h('span', { class: 'tl-day-name' }, formatDate(r.start_ms, tz, { weekday: 'long', year: true }))), side));
        }
        side.append(entry(r, { car: cid, tz }));
      }
      shown += items.length;
      foot.hidden = pager.done;
      count.textContent = `${plural(shown, 'entry', 'entries')}${pager.done ? '' : ' so far'}`;
      if (!shown) {
        const names = [...kinds].map((k) => KIND[k].many.toLowerCase());
        replace(list, h('li', null, emptyState({ icon: 'timeline', title: 'Nothing on the tape',
          text: text ? `No entry in this period contains "${text}".` : placeId ? 'Nothing happened at this place in the period.'
            : kinds.size < 5 ? `No ${names.join(', ')} in this period. Turn more kinds on, or pick a longer period.` : 'Nothing in this period. Pick a longer one.' })));
      }
    } catch (e) {
      if (my !== gen || e.name === 'AbortError') return;
      if (e.status === 400 && e.fields?.place && placeId) {
        replace(list, h('li', null, emptyState({ icon: 'pin', title: 'This place no longer exists', text: 'The link filters by a place that was removed, so nothing can be shown for it.',
          actions: btn('Show every place', () => { placeId = ''; placeSel.querySelector('[data-link]')?.remove(); placeSel.value = ''; reload(); }, { icon: 'grid' }) })));
        return;
      }
      const w = errorWords(e);
      replace(list, h('li', null, emptyState({ icon: w.icon, title: w.title, text: w.text, actions: btn('Try again', () => reload(), { icon: 'arrow-r' }) })));
    } finally { moreBtn.disabled = false; }
  }
  async function reload() {
    const my = ++gen;
    shown = 0; lastDay = null;
    ctx.setQuery({ kinds: kinds.size === 5 ? null : [...kinds].join(','), place: placeId || null, q: text || null });
    pager = ctx.api.pager(`cars/${encodeURIComponent(cid)}/timeline`, params(), { limit: 50 });
    replace(csvSlot, csvBtn({ href: ctx.api.csvUrl(`cars/${encodeURIComponent(cid)}/timeline`, params()), filename: csvFilename(car.name, 'timeline', period.label) }));
    list.replaceChildren(...Array.from({ length: 4 }, () => h('li', { class: 'tl-skel', 'aria-hidden': 'true' }, h('span', { class: 'skel skel--line' }))));
    await more(my);
  }

  await reload();
  if (!ctx.alive()) return;
  ctx.ready();

  ctx.events('now', (ev) => { if (ev.data?.car !== cid || !ev.data.now) return; Object.assign(live, ev.data.now); paintNow(); });
  let rt = 0;
  ctx.events('rows', (ev) => { if (ev.data?.car !== cid) return; clearTimeout(rt); rt = setTimeout(() => { if (ctx.alive()) ctx.reload(); }, 2000); });
  ctx.onCleanup(() => clearTimeout(rt));
}
