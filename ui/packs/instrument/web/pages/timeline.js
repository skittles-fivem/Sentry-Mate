// Timeline (/cars/{car}/timeline): everything the car did in the period, as one strip and one list:
// drives, charges, parked spells (asleep, awake and unknown time apart), gaps (with their reason) and
// software updates. Newest first, paged by cursor. Filters: kinds, a place, and a text that only hides
// rows (it never changes their figures). Every row links to its detail, to Trip for its exact span, and
// to the place editor for the places it shows.

import * as state from '../shared/state.js';
import { h, replace, cover } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, dayKey } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { downloadCsv, timeColumn, csvFilename } from '../shared/csv.js';
import { icon } from '../core/icons.js';
import { pageHead, panel, csvButton } from '../core/page.js';
import { activityStrip, KINDS as STRIP_KINDS } from '../core/strip.js';
import { openCarSwitcher } from '../core/carswitch.js';
import { openPeriodPicker } from '../core/period.js';
import { emptyState, errorWords } from '../core/states.js';
import { when, ago, statePill, spotText } from '../core/fmt.js';

export const id = 'timeline';
export const css = true;

const KIND = {
  drive: { word: 'Drive', plural: 'Drives', icon: 'route', state: 'driving' },
  charge: { word: 'Charge', plural: 'Charges', icon: 'bolt', state: 'charging' },
  parked: { word: 'Parked', plural: 'Parked', icon: 'park', state: 'parked' },
  gap: { word: 'Gap', plural: 'Gaps', icon: 'question', state: 'unknown' },
  update: { word: 'Update', plural: 'Updates', icon: 'chip', state: 'updating' },
};
const GAP = {
  no_log: 'No log arrived for this time.',
  not_recorded: 'Recording was paused on the CAN Bridge.',
  failed_check: 'A log failed its check, so its data is not used.',
  dropped_frames: 'The log lost frames here.',
};
const AWAKE_WORD = {
  sentry: 'Sentry', keep_climate_on: 'keep climate', keep_climate_dog: 'dog mode', keep_climate_camp: 'camp mode', user_present: 'someone inside',
  preconditioning_cabin: 'cabin preconditioning', preconditioning_battery: 'battery preconditioning', charging: 'charging', keep_warm: 'battery kept warm',
  door_open: 'a door open', accessory_power: 'accessory power',
};

const placeLabel = (p) => p?.place?.name || p?.label || (typeof p?.address === 'string' ? p.address : p?.address?.short) || spotText(p) || null;
// the place editor for a point: an existing place opens for editing, else a new one starts there
const placeHref = (p) => (p?.place?.id ? state.buildUrl('places', { query: { place: p.place.id } })
  : p?.lat != null ? state.buildUrl('places', { query: { lat: p.lat.toFixed(5), lon: p.lon.toFixed(5), new: 1 } }) : null);
function placeLink(p, extra = null) {
  const text = placeLabel(p);
  if (!text) return null;
  const href = placeHref(p);
  return href ? h('a', { class: 'tl-place-link', href, title: p.place ? `Edit the place ${p.place.name}` : 'Make this spot a place' }, text, extra) : h('span', null, text);
}
const sign = (v, kind) => (v == null ? null : format(v, kind, { sign: true }));

/**
 * One timeline row (also used by Car overview for today's list). r: a /timeline row. opts: { car, tz,
 * compact }. The row's time opens Trip for exactly its span; its title opens the drive, charge or
 * software page; place names open the place editor.
 */
export function timelineRow(r, { car, tz, compact = false }) {
  const k = KIND[r.kind] || KIND.gap;
  const end = r.end_ms;
  const tripHref = state.buildUrl('trip', { car, period: { from: r.start_ms, to: end ?? null } });
  const detailHref = r.ref?.drive ? state.buildUrl('drive', { car, drive: r.ref.drive }) : r.ref?.charge ? state.buildUrl('charge', { car, charge: r.ref.charge })
    : r.kind === 'update' ? state.buildUrl('software', { car }) : null;
  const est = r.energy_basis === 'estimated';
  let title, detail;
  const bits = [];
  if (r.kind === 'drive') {
    title = [placeLink(r.start), h('span', { class: 'tl-arrow', 'aria-hidden': 'true' }, ' → '), r.end && r.end_ms != null ? placeLink(r.end) : h('span', null, r.end_ms == null ? 'driving now' : '…')];
    bits.push(format(r.distance_km, 'distance'), r.energy_delta_kwh != null ? `${format(-r.energy_delta_kwh, 'energy')} used${est ? ' (estimated)' : ''}` : null);
  } else if (r.kind === 'charge') {
    title = [h('span', null, 'Charging at '), placeLink(r.start) || h('span', null, 'an unknown spot')];
    bits.push(r.energy_delta_kwh != null ? `${format(r.energy_delta_kwh, 'energy')} added${est ? ' (estimated)' : ''}` : null, r.range_delta_km != null ? `${sign(r.range_delta_km, 'range')} range` : null,
      r.cost ? format(r.cost, 'money') : null);
  } else if (r.kind === 'parked') {
    title = [placeLink(r.start) || h('span', null, 'Parked')];
    const pm = r.parked_ms || {};
    bits.push(pm.asleep ? `asleep ${formatDuration(pm.asleep)}` : null, pm.awake ? `awake ${formatDuration(pm.awake)}` : null,
      pm.awake_vehicle_bus_only ? `vehicle bus only ${formatDuration(pm.awake_vehicle_bus_only)}` : null, pm.unknown ? `unknown ${formatDuration(pm.unknown)}` : null,
      r.energy_delta_kwh != null ? `${format(Math.abs(r.energy_delta_kwh), 'energy')} used${est ? ' (estimated)' : ''}` : null);
  } else if (r.kind === 'gap') {
    // where the gap began and, when the car moved, where it ended: both open the place editor
    const a = placeLink(r.start), b = r.end ? placeLink(r.end) : null;
    title = [a, a && b ? h('span', { class: 'tl-arrow', 'aria-hidden': 'true' }, ' → ') : null, b, a || b ? h('span', { class: 'tl-place' }, ' · ') : null,
      h('span', null, 'No data'), r.distance_km ? h('span', { class: 'tl-place' }, ` · moved ${format(r.distance_km, 'distance')}`) : null];
    bits.push(GAP[r.gap_reason] || 'No data for this time.', r.distance_km ? 'The car moved, but no drive was recorded.' : null);
  } else {
    title = [h('span', null, 'Software update'), r.version ? h('span', { class: 'tl-place' }, ` · ${r.version}`) : null];
  }
  if (r.soc_end_pct != null && r.kind !== 'gap' && r.kind !== 'update') bits.push(`${format(r.soc_end_pct - (r.soc_delta_pct || 0), 'percent')} → ${format(r.soc_end_pct, 'percent')}`);
  if (!compact) {
    if (r.range_end_km != null && r.kind !== 'gap') bits.push(`range ${format(r.range_end_km, 'range')}${r.range_delta_km != null && r.kind !== 'charge' ? ` (${sign(r.range_delta_km, 'range')})` : ''}`);
    if (r.outside_avg_c != null) bits.push(`${format(r.outside_avg_c, 'temperature')} outside`);
    if (r.odometer_km != null) bits.push(`odometer ${format(r.odometer_km, 'odometer')}`);
  }
  detail = bits.filter(Boolean).join(' · ');
  const awake = r.kind === 'parked' && r.awake_reasons?.length ? r.awake_reasons.map((x) => AWAKE_WORD[x] || x.replace(/_/g, ' ')) : null;
  const t0 = formatTime(r.start_ms, tz), t1 = end ? formatTime(end, tz) : 'now';
  return h('div', { class: ['tl-item', r.source === 'teslamate' && 'is-imported'], dataset: { state: k.state, kind: r.kind } },
    h('a', { class: 'tl-time', href: tripHref, title: 'Open this span as a trip' },
      compact ? [h('span', null, t0), h('span', { class: 'faint' }, ` – ${t1}`)] : [h('span', { class: 'tl-day' }, formatDate(r.start_ms, tz, { weekday: true, year: false })), h('span', null, `${t0} – ${t1}`)]),
    h('span', { class: 'tl-rail', 'aria-hidden': 'true' }, h('i')),
    h('div', { class: 'tl-body' },
      h('span', { class: 'title' },
        detailHref ? h('a', { class: 'tl-kind', href: detailHref, title: `Open the ${k.word.toLowerCase()}` }, icon(k.icon, 'ico--sm'), k.word) : h('span', { class: 'tl-kind' }, icon(k.icon, 'ico--sm'), k.word),
        h('span', { class: 'tl-what' }, title)),
      detail ? h('span', { class: 'detail' }, detail) : null,
      awake ? h('span', { class: 'detail tl-awake' }, `Kept awake by ${awake.join(', ')}`) : null,
      r.source === 'teslamate' ? h('span', { class: 'detail' }, h('span', { class: 'tag tag--plain' }, 'Imported from TeslaMate')) : null),
    h('span', { class: 'tl-dur' }, r.duration_ms != null ? formatDuration(r.duration_ms) : end ? formatDuration(end - r.start_ms) : ''),
    detailHref ? h('a', { class: 'row-chev', href: detailHref, 'aria-label': `Open the ${k.word.toLowerCase()}` }, icon('chev-r')) : h('span', { class: 'row-chev' }));
}

export async function mount(ctx) {
  const { car, tz, period } = ctx;
  const cid = car.id;
  const from = period.from_ms, to = period.to_ms;
  const q = ctx.query;
  const kinds = new Set((q.kinds || 'drive,charge,parked,gap,update').split(',').filter((x) => KIND[x]));
  let placeId = q.place || '';
  let text = q.q || '';

  const root = h('div', { class: 'timeline-page', cover: ['shots.states', 'shots.timeline'] });
  replace(ctx.view, root);

  const [states, act] = await Promise.all([
    ctx.api.get(`cars/${encodeURIComponent(cid)}/states`, { from_ms: from, to_ms: to, limit: 1 }),
    // /activity defaults to the last 24 hours without a start: ask for the whole history when the period has none
    ctx.api.get(`cars/${encodeURIComponent(cid)}/activity`, { from_ms: from ?? 0, to_ms: to }),
  ]);
  if (!ctx.alive()) return;

  // filters
  const kindBtns = h('div', { class: 'seg', role: 'group', 'aria-label': 'Kinds of entry', cover: ['timeline.filter.action', 'timeline.table.column_filter'] },
    Object.entries(KIND).map(([k, v]) => h('button', {
      class: 'seg-btn', type: 'button', 'aria-pressed': String(kinds.has(k)), dataset: { state: v.state },
      onclick: (e) => { if (kinds.has(k) && kinds.size === 1) return; if (kinds.has(k)) kinds.delete(k); else kinds.add(k); e.currentTarget.setAttribute('aria-pressed', String(kinds.has(k))); reload(); },
    }, h('span', { class: 'dot' }), v.plural)));
  const placeSel = h('select', { class: 'select', 'aria-label': 'Place' }, h('option', { value: '' }, 'Every place'));
  ctx.api.page('places', { sort: 'visits' }, { limit: 100 }).then((pg) => {
    if (!ctx.alive()) return;
    placeSel.append(...pg.items.map((p) => h('option', { value: p.id, selected: p.id === placeId }, p.name)));
    // a place from the link that is not in the list (removed, or past the first 100): show it, so the select and the list agree
    if (placeId && !pg.items.some((p) => p.id === placeId)) placeSel.append(h('option', { value: placeId, selected: true, dataset: { fromLink: '1' } }, 'The place in the link'));
  }).catch(() => {});
  placeSel.addEventListener('change', () => { placeId = placeSel.value; reload(); });
  const textIn = h('input', { class: 'input', type: 'search', value: text, placeholder: 'Hide rows without this text', 'aria-label': 'Text filter', title: 'Only hides rows; it never changes the figures of the rows it keeps.' });
  let tt = 0;
  textIn.addEventListener('input', () => { clearTimeout(tt); tt = setTimeout(() => { text = textIn.value.trim(); reload(); }, 300); });
  ctx.onCleanup(() => clearTimeout(tt));

  // state summary
  const share = states.share_pct || {};
  const cur = states.current || {};
  // Now: the car's own state (driving, charging, parked, asleep, unknown) from the live data, else /states
  const nowSlot = h('span', { class: 'metric-value' });
  const nowExplain = h('span', { class: 'explain' });
  const live = { ...(car.now || {}) };
  function paintNow() {
    const st = live.state || (cur.state === 'online' ? 'parked' : cur.state) || 'unknown';
    const since = live.since_ms ?? cur.since_ms;
    replace(nowSlot, statePill(st, { lg: true, stale: !!live.stale }));
    nowExplain.textContent = [since ? `since ${when(since, tz)}` : null, live.stale ? 'last known; no upload for over an hour' : null,
      st === 'unknown' ? 'no recent log, so the state is not known (not the same as asleep)' : null].filter(Boolean).join(' · ');
  }
  paintNow();
  const summary = panel({ title: 'States', icon: 'park', cls: 'tl-summary' },
    h('div', { class: 'figures' },
      h('div', { class: 'metric', cover: 'states.stat.current' }, h('span', { class: 'metric-label' }, 'Now'), nowSlot, nowExplain),
      h('div', { class: 'metric', cover: 'states.stat.last_change' }, h('span', { class: 'metric-label' }, 'Last change'), h('span', { class: 'metric-value metric-value--sm' }, states.last_change_ms ? when(states.last_change_ms, tz) : '—'), h('span', { class: 'explain' }, states.last_change_ms ? ago(states.last_change_ms) : '')),
      h('div', { class: 'metric', cover: 'states.stat.parked_pct' }, h('span', { class: 'metric-label' }, 'Parked'), h('span', { class: 'metric-value' }, format(share.parked, 'percent')), h('span', { class: 'explain' }, 'of the known time, not driving')),
      h('div', { class: 'metric' }, h('span', { class: 'metric-label' }, 'Asleep'), h('span', { class: 'metric-value' }, format(share.asleep, 'percent')), h('span', { class: 'explain' }, 'both buses quiet')),
      h('div', { class: 'metric' }, h('span', { class: 'metric-label' }, 'Awake'), h('span', { class: 'metric-value' }, format(share.awake, 'percent')), h('span', { class: 'explain' }, 'driving included')),
      h('div', { class: 'metric' }, h('span', { class: 'metric-label' }, 'Unknown'), h('span', { class: 'metric-value' }, format(share.unknown, 'percent')), h('span', { class: 'explain' }, 'no log, paused or failed check'))),
    h('p', { class: 'note' }, 'The shares overlap on purpose (parked time includes asleep time), so they need not add up to 100 %.'));

  const stripFrom = from ?? (act.items?.[0]?.start_ms ?? to - 86400000);
  const stripCsv = csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'activity', period.label), act.items || [], [
    { label: 'State', value: (s) => (STRIP_KINDS[s.kind] || STRIP_KINDS.unknown).label },
    timeColumn('Start', 'start_ms', tz), timeColumn('End', 'end_ms', tz),
    { label: 'Duration (min)', value: (s) => (s.end_ms != null ? Math.round((s.end_ms - s.start_ms) / 60000) : null) },
    { label: 'Source', value: (s) => s.source || '' },
  ]) });
  const strip = panel({ title: 'What the car did', icon: 'timeline', cls: 'tl-panel', side: [stripCsv], cover: ['states.chart.states', 'states.chart.legend'] },
    activityStrip(act.items || [], stripFrom, to, { tz, now: ctx.api.now(), size: 'lg', kinds: ['driving', 'charging', 'online', 'online_vehicle_bus_only', 'asleep', 'updating', 'unknown'] }),
    from == null && act.items?.length ? h('p', { class: 'note' }, `The whole history, from the first data on ${formatDate(stripFrom, tz, { year: true })}.`) : null);

  // the list
  const list = h('div', { class: 'tl-list', role: 'list' });
  const count = h('span', { class: 'tbl-count' });
  const csvSlot = h('span');
  const more = h('button', { class: 'btn btn--sm', type: 'button' }, icon('chev-d', 'ico--sm'), 'Show older entries');
  const foot = h('div', { class: 'list-foot', hidden: true }, more);
  const params = () => ({ from_ms: from, to_ms: to, kinds: [...kinds].join(','), place: placeId || null, q: text || null });
  let pager = null, gen = 0, shown = 0, lastDay = null;

  const dayHead = (ms) => h('div', { class: 'day-head', role: 'presentation' }, h('h3', null, formatDate(ms, tz, { weekday: 'long', year: true })));
  async function loadMore(my) {
    more.disabled = true;
    try {
      const items = await pager.more();
      if (my !== gen || !ctx.alive()) return;
      if (!shown) list.replaceChildren();
      for (const r of items) {
        const dk = dayKey(r.start_ms, tz);
        if (dk !== lastDay) { lastDay = dk; list.append(dayHead(r.start_ms)); }
        list.append(timelineRow(r, { car: cid, tz }));
      }
      shown += items.length;
      foot.hidden = pager.done;
      count.textContent = `${formatNumber(shown)} ${shown === 1 ? 'entry' : 'entries'}${pager.done ? '' : ' so far'}`;
      if (!shown) {
        const names = [...kinds].map((k) => KIND[k].plural.toLowerCase());
        const kindWords = names.length > 1 ? `${names.slice(0, -1).join(', ')} or ${names[names.length - 1]}` : names[0];
        replace(list, emptyState({ icon: 'timeline', title: 'Nothing to show', text: text ? `No entry in this period contains "${text}".` : placeId ? 'Nothing happened at this place in the period.'
          : kinds.size < 5 ? `No ${kindWords} in this period. Turn more kinds on above, or pick a longer period.` : 'Nothing in this period. Pick a longer one.' }));
      }
    } catch (e) {
      if (my !== gen || e.name === 'AbortError') return;
      if (e.status === 400 && e.fields?.place && placeId) {
        // a link to a place that no longer exists: say so and offer to drop that filter
        replace(list, emptyState({ icon: 'pin', warn: true, title: 'This place no longer exists', text: 'The link filters by a place that was removed or never existed, so nothing can be shown for it.',
          action: h('button', { class: 'btn btn--sm', type: 'button', onclick: () => { placeId = ''; placeSel.querySelector('[data-from-link]')?.remove(); placeSel.value = ''; reload(); } }, 'Show every place') }));
        return;
      }
      const w = errorWords(e);
      replace(list, h('div', { class: 'empty', role: 'alert' }, h('h3', null, w.title), h('p', null, w.text)));
    } finally { more.disabled = false; }
  }
  more.addEventListener('click', () => loadMore(gen));
  async function reload() {
    const my = ++gen;
    shown = 0; lastDay = null;
    ctx.setQuery({ kinds: kinds.size === 5 ? null : [...kinds].join(','), place: placeId || null, q: text || null });
    pager = ctx.api.pager(`cars/${encodeURIComponent(cid)}/timeline`, params(), { limit: 50 });
    replace(csvSlot, csvButton({ href: ctx.api.csvUrl(`cars/${encodeURIComponent(cid)}/timeline`, params()), filename: csvFilename(car.name, 'timeline', period.label) }));
    list.replaceChildren(...Array.from({ length: 4 }, () => h('div', { class: 'tl-item tl-skel', 'aria-hidden': 'true' }, h('span', { class: 'skel skel--text' }), h('span'), h('span', { class: 'skel skel--text' }))));
    await loadMore(my);
  }

  const help = h('details', { class: 'panel panel-pad tl-help' },
    h('summary', null, icon('question', 'ico--sm'), 'Why does a car stay awake?'),
    h('ul', { class: 'help-list' },
      h('li', { cover: 'faq.no-sleep.log-timeline' }, h('b', null, 'What normal looks like. '), 'After a drive the car stays awake for a while, then the vehicle bus and the chassis bus go quiet and the row turns to asleep. Short awake spells while parked are normal (the car checks its 12 V battery).'),
      h('li', { cover: 'faq.no-sleep.state-image' }, h('b', null, 'Reading the strip. '), 'Hatched blue is asleep, solid blue is parked and awake, striped blue is awake with only the vehicle bus, grey is unknown. A long solid stretch while parked means something kept the car awake; the parked row says what.'),
      h('li', { cover: 'faq.no-sleep.accessory' }, h('b', null, 'Accessory power. '), 'With accessory power kept on, the car stays awake even with nothing plugged in. Turn it off on the car if you do not need it.'),
      h('li', { cover: 'faq.no-sleep.mcu1' }, h('b', null, 'Older Model S and X. '), 'Cars with the older media computer sleep less deeply and wake more often; expect more awake time than on newer cars.'),
      h('li', { cover: 'faq.no-sleep.other-loggers' }, h('b', null, 'Other apps. '), 'Sentry Mate only listens to the car, so it never keeps it awake. An app that asks Tesla’s servers for the car’s data can wake it; check the parked rows for awake spells when such an app runs.')));

  replace(root,
    pageHead({
      eyebrow: car.name, title: 'Timeline', sub: [period.label, `times in ${tz.replace(/_/g, ' ')}`],
      actions: [h('button', { class: 'btn', type: 'button', onclick: openCarSwitcher, cover: ['timeline.filter.car', 'states.filter.car'] }, icon('car'), 'Switch car')],
    }),
    h('div', { class: 'toolbar tl-filters' },
      h('button', { class: 'btn', type: 'button', onclick: (e) => openPeriodPicker(e.currentTarget), cover: ['timeline.control.time_range', 'states.control.time_range'] }, icon('calendar', 'ico--sm'), period.preset === 'custom' ? 'Custom period' : period.label, icon('chev-d', 'ico--sm')),
      kindBtns,
      h('span', { cover: ['timeline.filter.address'] }, placeSel),
      h('label', { class: 'field field--search' }, icon('search'), textIn)),
    summary, strip,
    h('section', { class: 'panel tl-rows', 'aria-label': 'Entries', cover: ['timeline.table', 'projects.dash', 'timeline.row.drive', 'timeline.row.charging', 'timeline.row.parking', 'timeline.row.missing', 'timeline.row.parking_gap_overlap', 'timeline.row.updating',
      'timeline.col.start', 'timeline.col.end', 'timeline.col.action', 'timeline.col.start_address', 'timeline.col.end_address', 'timeline.col.odometer', 'timeline.col.duration', 'timeline.col.distance', 'timeline.col.range', 'timeline.col.range_diff', 'timeline.col.energy_diff', 'timeline.col.soc', 'timeline.col.soc_diff', 'timeline.col.temp',
      'timeline.link.trip_span', 'timeline.link.slot_details', 'timeline.link.geofence_start', 'timeline.link.geofence_end', 'timeline.link.place_cells_other_rows'] },
    h('div', { class: 'tbl-bar' }, h('h2', { class: 'panel-title' }, icon('timeline'), 'Entries, newest first'), count, csvSlot),
    h('p', { class: 'note tl-legend-note' }, 'A time opens that span as a trip. A kind opens the drive, charge or software page. A place name opens the place editor. A gap is never counted as parked time. The text filter only hides rows; it never changes their figures.'),
    list, foot),
    help);

  await reload();
  if (!ctx.alive()) return;
  ctx.ready();

  ctx.events('now', (ev) => { if (ev.data?.car !== cid || !ev.data.now) return; Object.assign(live, ev.data.now); paintNow(); });
  let rt = 0;
  ctx.events('rows', (ev) => { if (ev.data?.car !== cid) return; clearTimeout(rt); rt = setTimeout(() => { if (ctx.alive()) ctx.reload(); }, 2000); });
  ctx.onCleanup(() => clearTimeout(rt));
}
