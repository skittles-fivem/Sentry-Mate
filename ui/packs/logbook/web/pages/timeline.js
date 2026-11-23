// Sentry Mate, "Logbook" UI pack: Timeline (/cars/{car}/timeline).
//
// Everything the car did in the shared period, as one strip and one journal: drives, charges, parked
// spells (asleep, awake and unknown time apart), gaps with their reason, and software changes. Data:
// GET /api/v1/cars/{car}/states (current state, last change, shares), /activity (the strip, CSV built
// here), /timeline (the rows, newest first, cursor paged; kinds, place and text filters; CSV from the
// server), /trip (the period's figures), /tracks (the period's roads on the map) and /places (the place
// filter).

import { h, replace } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { trackBounds } from '../shared/tracks.js';
import { formatTime, formatDate, formatDateTime, formatDuration, dayKey, formatDayKey, addDays, periodLabel } from '../shared/time.js';
import { formatNumber, formatList, tn } from '../shared/i18n.js';
import { downloadCsv, column, timeColumn } from '../shared/csv.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { pageHead, q, plus, figure, icon, empty, errorView, loadingBlock, button, stateLabel, flag, sep } from '../core/ui.js';
import { sourceMark, sourceLabel, isImported, teslaOnly, ESTIMATED_WHY } from '../core/sources.js';
import { activityStrip, placeLink, AWAKE_WORDS, GAP_WORDS, STRIP_NAMES } from '../core/journal.js';
import { makeMap, boundsOf } from '../core/map.js';
import { zoneLabel } from '../core/format.js';
import { spanHref } from '../core/period.js';

export const id = 'timeline';
export const css = true;
export const title = () => 'Timeline';

const KINDS = [['drive', 'Drives'], ['charge', 'Charges'], ['parked', 'Parked'], ['gap', 'Gaps'], ['update', 'Software']];
const KIND_WORD = { drive: 'Drive', charge: 'Charge', parked: 'Parked', gap: 'Gap in the data', update: 'Software update' };

/** What each reason for staying awake means, in a sentence. */
const AWAKE_HELP = {
  sentry: 'Sentry mode keeps the cameras watching, so the car cannot sleep while it is on.',
  keep_climate_on: 'Keep-climate holds the cabin temperature and keeps the car awake.',
  keep_climate_dog: 'Dog mode holds the cabin temperature and keeps the car awake.',
  keep_climate_camp: 'Camp mode keeps climate, screen and power on all night.',
  user_present: 'The car stays awake while it senses someone inside.',
  preconditioning_cabin: 'Warming or cooling the cabin before a drive, set on the car or in the phone app.',
  preconditioning_battery: 'Warming the battery before a drive or a fast charge.',
  charging: 'A charging car is awake.',
  keep_warm: 'In the cold the car may keep the battery warm by itself.',
  door_open: 'An open door, boot or bonnet keeps the car awake.',
  accessory_power: 'Accessory power keeps the 12 V sockets on, and the car awake, while parked.',
};

/** What each reason for a gap means, and what happens next. */
const GAP_HELP = {
  no_log: 'No log reached Sentry Mate for this time. If the CAN Bridge was out of reach, its late upload fills this in by itself.',
  not_recorded: 'Recording was paused on the CAN Bridge for this time.',
  failed_check: 'A log arrived but failed its integrity check, so its data is not used.',
  dropped_frames: 'The CAN Bridge dropped frames here, so this stretch is not trusted.',
};

// The rows do all of these; they are marked on the list so that a period without a gap or an update still
// shows they are handled (each row also marks the figure or link it draws).
const ROW_COVERS = ['timeline.table', 'projects.dash', 'shots.timeline', 'shots.states',
  'timeline.row.drive', 'timeline.row.charging', 'timeline.row.parking', 'timeline.row.missing', 'timeline.row.parking_gap_overlap', 'timeline.row.updating',
  'timeline.col.start', 'timeline.col.end', 'timeline.col.action', 'timeline.col.start_address', 'timeline.col.end_address', 'timeline.col.odometer',
  'timeline.col.duration', 'timeline.col.distance', 'timeline.col.range', 'timeline.col.range_diff', 'timeline.col.energy_diff', 'timeline.col.soc',
  'timeline.col.soc_diff', 'timeline.col.temp',
  'timeline.link.trip_span', 'timeline.link.slot_details', 'timeline.link.geofence_start', 'timeline.link.geofence_end', 'timeline.link.place_cells_other_rows'];

const MODULE_NAME = { vcfront: 'front body controller', bms: 'battery management', dir: 'rear drive unit', dif: 'front drive unit' };
const VERSION_SOURCE = {
  teslamate: 'from the TeslaMate import', teslafi: 'from the TeslaFi import', teslafi_api: 'from TeslaFi', teslamate_local: 'from local TeslaMate',
  tesla_api: 'by the Tesla API', owner: 'typed by the owner', build_match: 'from an earlier update with the same fingerprints',
};
/** Live inputs that name a fingerprint update when they report a new version within a day after it. */
const LIVE_VERSION = new Set(['teslafi_api', 'teslamate_local', 'tesla_api']);

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz;
  const kinds = new Set((ctx.query.kinds || '').split(',').filter((k) => KINDS.some(([x]) => x === k)));
  if (!kinds.size) KINDS.forEach(([k]) => kinds.add(k));
  let text = ctx.query.q || '';
  let place = ctx.query.place || '';

  /* ---------------------------------------------------------------- the parts of the page */

  const head = h('div');
  const summary = h('section', { class: 'state-summary', 'aria-label': 'The car’s state' });
  const stripBox = h('div', { class: 'strip-box' });
  const stripCsv = button('Download CSV', { kind: 'quiet', iconName: 'download' });
  stripCsv.classList.add('button-small');
  const periodNote = h('p', { class: 'strip-period', cover: ['states.control.time_range', 'timeline.control.time_range'] });
  const strip = h('section', { class: 'strip-block', 'aria-label': 'State strip' },
    h('div', { class: 'strip-head' }, h('h2', { class: 'label' }, 'What the car was doing'), periodNote, stripCsv), stripBox);

  const checks = KINDS.map(([k, t]) => h('label', { class: 'check' }, h('input', { type: 'checkbox', value: k, checked: kinds.has(k) }), t));
  const qIn = h('input', { class: 'field-input', type: 'search', id: 'tl-q', value: text, placeholder: 'A place, an address or a word', autocomplete: 'off' });
  const placeSel = h('select', { class: 'field-select', id: 'tl-place' }, h('option', { value: '' }, 'Any place'));
  const carField = h('div', { class: 'field tl-car', cover: ['states.filter.car', 'timeline.filter.car'] },
    h('p', { class: 'field-label' }, 'Car'),
    h('p', { class: 'tl-car-name' }, car.name),
    h('p', { class: 'field-hint' }, 'Switch car at the top; ', h('a', { href: state.buildUrl('fleet', { period: ctx.state.period }) }, 'all cars in Fleet'), '.'));
  const filters = h('div', { class: 'tl-filters', role: 'search', 'aria-label': 'Which entries to show' },
    carField,
    h('fieldset', { class: 'field kinds', cover: ['timeline.filter.action', 'timeline.table.column_filter'] },
      h('legend', { class: 'field-label' }, 'Show'), h('div', { class: 'checks' }, checks)),
    h('div', { class: 'field tl-place-field', cover: 'timeline.filter.address' },
      h('label', { class: 'field-label', for: 'tl-place' }, 'Place'), h('div', { class: 'field-control' }, placeSel),
      h('p', { class: 'field-hint' }, 'Places this car has been to; entries that start or end there.')),
    h('div', { class: 'field field-search', cover: 'timeline.filter.address' },
      h('label', { class: 'field-label', for: 'tl-q' }, 'Find'),
      h('div', { class: 'field-control' }, icon('search', 'icon field-icon'), qIn),
      h('p', { class: 'field-hint' }, 'Only hides rows; the figures of the rows shown never change.')));

  const list = h('div', { class: 'tl-list' });
  const more = h('div', { class: 'list-more' });
  const count = h('p', { class: 'ledger-count', 'aria-live': 'polite' });
  const csv = h('a', { class: 'button button-quiet button-small', download: '', href: '#' }, icon('download'), 'Download CSV');
  const mapBox = h('div', { class: 'map map-day', role: 'region', 'aria-label': 'Map of the period’s drives' });
  const mapNote = h('p', { class: 'note map-note-line' });
  const sideFigs = h('div', { class: 'today-figs side-figs' });
  const side = h('aside', { class: 'day-side', 'aria-label': 'The period at a glance' },
    h('section', { class: 'roads', cover: 'projects.dash' }, h('h2', { class: 'label' }, 'The period’s roads'), mapBox, mapNote),
    sideFigs, awakeHelp());

  ctx.main.append(head, summary, strip, filters,
    h('div', { class: 'timeline-grid' },
      h('section', { class: 'timeline-main', 'aria-label': 'Entries, newest first', cover: ROW_COVERS },
        h('div', { class: 'ledger-top' }, count, csv), list, more),
      side));

  /* ---------------------------------------------------------------- the rows */

  const params = () => ({ ...ctx.params, kinds: [...kinds].join(','), place: place || undefined, q: text || undefined });
  let pager = null;
  let loadSeq = 0;
  async function loadRows() {
    const seq = ++loadSeq;
    pager = ctx.api.pager(api.apiPath`cars/${car.id}/timeline`, params(), { limit: 60 });
    // Loading shows only the first time; after that the entries on screen stay until the new ones are in.
    if (!list.firstChild) replace(list, loadingBlock(8));
    csv.setAttribute('href', ctx.api.csvUrl(api.apiPath`cars/${car.id}/timeline`, params()));
    await moreRows(true, seq);
  }
  async function moreRows(first = false, seq = loadSeq) {
    const p = pager;
    try { await p.more(); } catch (err) {
      if (ctx.signal.aborted || seq !== loadSeq) return;
      replace(list, errorView(err, { retry: () => loadRows() }));
      if (first) throw err;
      return;
    }
    if (seq !== loadSeq) return;
    const items = p.items;
    count.textContent = p.done ? tn(items.length, '{n} entry', '{n} entries') : `${formatNumber(items.length)} entries shown, more to load`;
    csv.hidden = !items.length;
    if (!items.length) {
      const narrowed = text || place || kinds.size < KINDS.length;
      replace(list, empty({
        title: text ? `No entry matches “${text}”.` : place ? 'No entry at this place in this period.' : 'Nothing recorded in this period.',
        text: narrowed ? 'Clear the text or the place, or tick more kinds above.' : 'Choose a longer period at the top.',
      }));
      replace(more);
      return;
    }
    const groups = new Map();
    for (const r of items) { const k = dayKey(r.start_ms, tz); if (!groups.has(k)) groups.set(k, []); groups.get(k).push(r); }
    const now = api.now();
    const today = dayKey(now, tz);
    const yesterday = addDays(today, -1);
    replace(list, [...groups.entries()].map(([k, rows]) => h('section', { class: 'tl-day', 'aria-label': formatDayKey(k, { weekday: 'long', month: 'long' }) },
      h('h3', { class: 'tl-day-name' }, k === today ? 'Today' : k === yesterday ? 'Yesterday' : formatDayKey(k, { weekday: 'long', month: 'long', year: k.slice(0, 4) !== today.slice(0, 4) }),
        k === today || k === yesterday ? h('span', { class: 'day-date' }, formatDayKey(k, { weekday: 'long', month: 'long', year: false })) : null),
      h('ol', { class: 'tl tl-full tl-page' }, rows.map((r) => entry(r, { car: car.id, tz, now, period: ctx.state.period }))))));
    if (!p.done) {
      const b = button('Show earlier entries', { kind: 'primary', onclick: async () => { b.disabled = true; b.textContent = 'Loading…'; await moreRows(false, seq); } });
      replace(more, b, h('p', { class: 'list-more-note' }, 'Newest first; the CSV holds every matching entry.'));
    } else replace(more, h('p', { class: 'list-more-note' }, 'The start of the period.'));
  }

  checks.forEach((c) => c.querySelector('input').addEventListener('change', (e) => {
    if (e.target.checked) kinds.add(e.target.value); else kinds.delete(e.target.value);
    if (!kinds.size) { kinds.add(e.target.value); e.target.checked = true; ctx.flash('At least one kind stays ticked.', { kind: 'info' }); return; }
    ctx.setQuery({ kinds: kinds.size === KINDS.length ? null : [...kinds].join(',') });
    loadRows().catch(() => {});
  }));
  let timer = 0;
  qIn.addEventListener('input', () => {
    clearTimeout(timer);
    timer = setTimeout(() => { text = qIn.value.trim(); ctx.setQuery({ q: text || null }); loadRows().catch(() => {}); }, 250);
  });
  placeSel.addEventListener('change', () => { place = placeSel.value; ctx.setQuery({ place: place || null }); loadRows().catch(() => {}); });
  ctx.onCleanup(() => clearTimeout(timer));

  // The place filter lists the places this car has been to (counts are the car's with ?car=), by name, and
  // where two share a name, with the part of town (else the street) that tells them apart.
  async function loadPlaces() {
    try {
      const all = await ctx.api.list('places', { car: car.id, sort: 'name' }, { limit: 200 }).all({ max: 2000 });
      const used = (p) => p.counts && (p.counts.drives || p.counts.charges || p.counts.visits);
      const found = all.filter((p) => used(p) || p.id === place);
      const names = placeNames(found);
      replace(placeSel, h('option', { value: '' }, found.length ? 'Any place' : 'Any place (this car has no saved place yet)'),
        found.map((p) => h('option', { value: p.id, selected: p.id === place }, names.get(p.id))));
      if (place && !found.some((p) => p.id === place)) { place = ''; ctx.setQuery({ place: null }); }
    } catch (err) {
      if (ctx.signal.aborted) return;
      placeSel.disabled = true;
      replace(placeSel, h('option', { value: '' }, 'Places could not be loaded'));
      if (place) { place = ''; ctx.setQuery({ place: null }); }
    }
  }

  /* ---------------------------------------------------------------- head, summary, strip, side */

  let sideMap = null;
  let act = null;
  stripCsv.addEventListener('click', () => {
    if (!act) return;
    const now = api.now();
    downloadCsv(`${car.name} states ${dayKey(act.from_ms, tz)}.csv`, act.items || [], [
      { label: 'State', value: (s) => STRIP_NAMES[s.kind] || s.kind },
      { label: 'Code', value: (s) => s.kind },
      timeColumn('Start', 'start_ms', tz),
      timeColumn('End', (s) => s.end_ms, tz),
      column('Minutes', (s) => ((s.end_ms ?? now) - s.start_ms) / 60000, null),
      { label: 'Data from', value: (s) => sourceLabel(s) },
    ]);
  });

  async function drawAll(c) {
    // "All time" has no start; /trip and /activity would fall back to a short window, so ask /trip from the
    // epoch and start the strip where the car's recorded time begins (its time spent, counted back from the end).
    const tripP = c.api.get(api.apiPath`cars/${car.id}/trip`, { ...c.params, from_ms: c.params.from_ms ?? 0 });
    const actP = c.params.from_ms != null ? c.api.get(api.apiPath`cars/${car.id}/activity`, c.params)
      : tripP.then((tr) => {
        const spent = Object.values(tr.time_ms || {}).reduce((a, b) => a + b, 0);
        return c.api.get(api.apiPath`cars/${car.id}/activity`, { ...c.params, from_ms: Math.max(0, tr.to_ms - Math.max(spent, 86400000)) });
      });
    // What the car is doing now, whatever the period: the strip's segments of the last two days.
    const nowP = c.api.get(api.apiPath`cars/${car.id}/activity`, { from_ms: api.now() - 2 * 86400000 });
    const [st, ac, trip, tracks, nowAct] = await Promise.allSettled([
      c.api.get(api.apiPath`cars/${car.id}/states`, { ...c.params, limit: 1 }),
      actP, tripP, roads(c), nowP,
    ]);
    if (c.signal.aborted) return;
    const now = api.now();
    const t = trip.status === 'fulfilled' ? trip.value : null;
    const sentence = [];
    if (t) {
      const n = (t.drives || []).length;
      sentence.push(n ? `${tn(n, '{n} drive', '{n} drives')}, ${format(t.distance.drives_km, 'distance')} in all.` : 'No drives.');
      const total = Object.values(t.time_ms || {}).reduce((a, b) => a + b, 0);
      if (t.time_ms && t.time_ms.asleep > 3600000) sentence.push(`Asleep for ${formatDuration(t.time_ms.asleep)}${total ? ` (${format((t.time_ms.asleep / total) * 100, 'percent')})` : ''}.`);
      const added = (t.energy_kwh.added_ac || 0) + (t.energy_kwh.added_dc || 0);
      if (added) sentence.push(`Charged ${format(added, 'energy')}.`);
      if (t.time_ms && t.time_ms.not_recorded > 60000) sentence.push(`${formatDuration(t.time_ms.not_recorded)} not recorded.`);
    }
    replace(head, pageHead({ eyebrow: `${car.name} · Timeline`, title: periodLabel(c.period, tz, now), deck: sentence.join(' ') || c.info.purpose }));
    periodNote.textContent = `${c.period?.label || periodLabel(c.period, tz, now)}, in ${zoneLabel(tz, now)}. Change the period at the top.`;

    if (st.status === 'fulfilled') {
      const s = st.value;
      const sh = s.share_pct || {};
      const cur = currentState(s, nowAct.status === 'fulfilled' ? nowAct.value : null);
      replace(summary,
        figure('Now', h('span', { class: ['state', `state-${cur.state}`] }, h('span', { class: 'state-dot', 'aria-hidden': 'true' }), stateLabel(cur.state)),
          cur.since_ms ? `${cur.how} ${formatDateTime(cur.since_ms, tz, { now })}` : 'no data from the car yet', { cover: 'states.stat.current' }),
        figure('Last change', s.last_change_ms ? formatDateTime(s.last_change_ms, tz, { now }) : '—',
          s.last_change_ms ? 'between awake, asleep and unknown' : 'no change in this period', { cover: 'states.stat.last_change' }),
        figure('Parked', q(sh.parked, 'percent', { why: 'no known time' }), 'of the known time, not driving', { cover: 'states.stat.parked_pct' }),
        figure('Asleep', q(sh.asleep, 'percent'), 'both buses quiet'),
        figure('Awake', q(sh.awake, 'percent'), 'driving included'),
        figure('Unknown', q(sh.unknown, 'percent'), 'no log, paused or failed check'));
    } else replace(summary, errorView(st.reason, { eyebrow: 'State summary', retry: () => drawAll(ctx) }));

    if (ac.status === 'fulfilled') {
      act = ac.value;
      stripCsv.hidden = !(act.items || []).length;
      replace(stripBox, activityStrip(act, {
        tz, now, cover: ['states.chart.states', 'states.chart.legend'],
        link: (sg) => (sg.ref && sg.ref.drive ? state.buildUrl('drive', { car: car.id, drive: sg.ref.drive, period: ctx.state.period })
          : sg.ref && sg.ref.charge ? state.buildUrl('charge', { car: car.id, charge: sg.ref.charge, period: ctx.state.period }) : null),
      }), h('p', { class: 'note strip-note' }, 'Asleep means the car went quiet; unknown means no data reached Sentry Mate. A drive or charge on the strip opens it.'));
    } else { act = null; stripCsv.hidden = true; replace(stripBox, errorView(ac.reason, { eyebrow: 'State strip' })); }

    if (t) {
      const added = (t.energy_kwh.added_ac || 0) + (t.energy_kwh.added_dc || 0);
      replace(sideFigs,
        figure('Drives', formatNumber((t.drives || []).length), 'started in the period'),
        figure('Distance', q(t.distance.drives_km, 'distance'), 'driven, from the drives'),
        figure('Energy used', q(t.energy_kwh.drive_net, 'energy'), 'by the drives, net of regen'),
        figure('Charged', added ? plus(q(added, 'energy')) : q(null, 'energy', { why: 'no charge' }), 'AC and DC, into the battery'));
    } else replace(sideFigs, errorView(trip.reason, { eyebrow: 'The period’s figures' }));

    await drawRoads(c, tracks.status === 'fulfilled' ? tracks.value : null, tracks.reason);
  }

  // The period's roads: many drives need more points; ask again with more before giving up.
  async function roads(c) {
    const p = { cars: car.id, ...c.params };
    try { return await c.api.get('tracks', { ...p, max_points: 8000 }); } catch (err) {
      if (!(err && err.status === 400)) throw err;
      return c.api.get('tracks', { ...p, max_points: 200000 });
    }
  }

  async function drawRoads(c, tr, reason) {
    if (!tr) {
      if (sideMap) { sideMap.map.getSource('roads')?.setData({ type: 'FeatureCollection', features: [] }); }
      else replace(mapBox, h('p', { class: 'map-note' }, 'The roads could not be drawn.'));
      mapNote.textContent = reason && reason.status === 400 ? 'Too many drives to draw for this period; choose a shorter one.' : 'The roads could not be loaded.';
      return;
    }
    const drives = tr.drives || [];
    const lines = drives.map((d) => ({ drive: d.drive, coords: d.lon.map((x, i) => [x, d.lat[i]]) })).filter((l) => l.coords.length > 1);
    mapNote.textContent = lines.length ? `${tn(lines.length, '{n} drive', '{n} drives')} drawn. Click a road to open its drive.` : '';
    if (!lines.length) {
      if (!sideMap) replace(mapBox, h('p', { class: 'map-note' }, 'No drives in this period.'));
      else sideMap.map.getSource('roads')?.setData({ type: 'FeatureCollection', features: [] });
      return;
    }
    const bounds = trackBounds(drives) || boundsOf(lines.flatMap((l) => l.coords)); // without stray fixes
    if (!sideMap) {
      sideMap = await makeMap(c, mapBox, { bounds, padding: 28, maxZoom: 15 });
      if (!sideMap) return;
      await sideMap.loaded;
      if (!c.alive()) return;
    } else sideMap.frame(bounds);
    const fc = { type: 'FeatureCollection', features: lines.map((l) => ({ type: 'Feature', properties: { drive: l.drive }, geometry: { type: 'LineString', coordinates: l.coords } })) };
    const src = sideMap.map.getSource('roads');
    if (src) { src.setData(fc); return; }
    sideMap.map.addSource('roads', { type: 'geojson', data: fc });
    sideMap.addLine({ id: 'roads-line', source: 'roads' }, 3, '--accent');
    sideMap.map.on('click', 'roads-line', (e) => {
      const d = e.features && e.features[0] && e.features[0].properties.drive;
      if (d) ctx.go(state.buildUrl('drive', { car: car.id, drive: String(d), period: ctx.state.period }));
    });
    sideMap.map.on('mouseenter', 'roads-line', () => { sideMap.map.getCanvas().style.cursor = 'pointer'; });
    sideMap.map.on('mouseleave', 'roads-line', () => { sideMap.map.getCanvas().style.cursor = ''; });
  }

  await Promise.all([drawAll(ctx), loadRows(), loadPlaces()]);
  if (sideMap) await Promise.race([sideMap.ready, new Promise((r) => setTimeout(r, 4000))]);

  // Live: a drive, charge, parked spell or gap that starts or ends loads the first page again.
  let liveTimer = 0;
  ctx.events(['rows'], (ev) => {
    const d = ev.data || {};
    if (d.car !== car.id) return;
    clearTimeout(liveTimer);
    liveTimer = setTimeout(() => {
      drawAll(ctx).catch(() => {});
      if (pager && pager.items.length <= 60) loadRows().catch(() => {});
    }, 1500);
  });
  ctx.onCleanup(() => clearTimeout(liveTimer));

  ctx.ready();
  return {
    async update(c) { await Promise.all([drawAll(c), loadRows()]); c.ready(); },
  };
}

/* ------------------------------------------------------------------ place names */

/** Each place's name for the filter, with what tells apart two of the same name ('Home · Noe Valley'). */
function placeNames(list) {
  const by = new Map();
  for (const p of list) { const k = p.name.trim().toLowerCase(); if (!by.has(k)) by.set(k, []); by.get(k).push(p); }
  const out = new Map();
  for (const group of by.values()) {
    if (group.length === 1) { out.set(group[0].id, group[0].name); continue; }
    // the finest part of the address that differs in the group: the part of town, the town, the street
    const parts = ['neighbourhood', 'locality', 'short'];
    const a = (p, k) => (p.address && p.address[k]) || '';
    const key = parts.find((k) => new Set(group.map((p) => a(p, k))).size === group.length && group.every((p) => a(p, k)))
      || parts.find((k) => group.some((p) => a(p, k))) || null;
    for (const p of group) {
      const extra = key ? a(p, key) || a(p, 'locality') || a(p, 'short') : '';
      out.set(p.id, extra ? `${p.name} · ${extra}` : p.name);
    }
    // still the same words: add the place's number so each choice is its own
    const seen = new Map();
    for (const p of group) { const t = out.get(p.id); seen.set(t, (seen.get(t) || 0) + 1); }
    for (const p of group) if (seen.get(out.get(p.id)) > 1) out.set(p.id, `${out.get(p.id)} (${p.id})`);
  }
  return out;
}

/* ------------------------------------------------------------------ the state now */

// The strip's segment kinds as the states a person reads (the car switcher's words).
const NOW_STATE = { driving: 'driving', charging: 'charging', updating: 'updating', online: 'online', online_vehicle_bus_only: 'online', asleep: 'asleep', unknown: 'unknown' };
const NOW_HOW = { updating: 'installing software', online: 'parked and awake', unknown: 'no data' };

/**
 * What the car is doing at its newest data: the last activity segment (driving, charging and updating
 * included), with the time it started; /states only knows awake, asleep and unknown, so it is the fallback.
 */
function currentState(st, act) {
  const segs = (act && act.items) || [];
  const last = segs[segs.length - 1];
  if (last) {
    const state = NOW_STATE[last.kind] || 'unknown';
    // the segments of one state run on (awake on both buses, then on the vehicle bus only): start at the first
    let i = segs.length - 1;
    while (i > 0 && (NOW_STATE[segs[i - 1].kind] || 'unknown') === state && segs[i - 1].end_ms === segs[i].start_ms) i -= 1;
    let since = segs[i].start_ms;
    // clipped at the start of the window: the three-way state knows when an awake, asleep or unknown spell began
    if (i === 0 && since <= act.from_ms && ['online', 'asleep', 'unknown'].includes(state) && st.current?.state === state && st.current.since_ms) since = st.current.since_ms;
    return { state, since_ms: since, how: NOW_HOW[state] ? `${NOW_HOW[state]} since` : 'since' };
  }
  const state = st.current?.state || 'unknown';
  return { state, since_ms: st.current?.since_ms || null, how: NOW_HOW[state] ? `${NOW_HOW[state]} since` : 'since' };
}

/* ------------------------------------------------------------------ one entry */

const signed = (v, kind) => (v == null ? null : Math.abs(v) < 0.5 ? h('span', { class: 'tl-delta' }, '(no change)')
  : h('span', { class: 'tl-delta' }, '(', v > 0 ? '+' : '−', q(Math.abs(v), kind), ')'));

/** A figure of an entry: its name over its value, and a quiet note after it. */
function fig(name, value, note = null, cover = null) {
  if (value == null) return null;
  return h('div', { class: 'tl-fig', cover }, h('dt', null, name), h('dd', null, value, note ? h('span', { class: 'tl-fig-note' }, ' ', note) : null));
}

/** A place as a link to the Places editor (edit its place, or a new one there), or plain text. */
function placeA(p, cover) {
  if (!p) return h('span', { class: 'tl-place' }, 'an unknown place');
  const name = p.label || (p.place && p.place.name) || p.address || 'an unnamed spot';
  const href = placeLink(p);
  if (!href) return h('span', { class: 'tl-place' }, name);
  return h('a', { class: 'tl-place tl-place-link', href, cover, 'aria-label': p.place ? `${name}: edit this place` : `${name}: name this place` }, name, icon('pin'));
}

function entry(r, { car, tz, now, period }) {
  const going = r.end_ms == null;
  const end = r.end_ms ?? now;
  const len = r.duration_ms ?? end - r.start_ms;
  const estimated = r.energy_basis === 'estimated' ? flag('Estimated', { kind: 'estimated', title: teslaOnly(r) ? ESTIMATED_WHY : 'Worked out, not measured at the pack' }) : null;
  let cls = r.kind, detail = null, title = [], figs = [], extra = [];

  if (r.kind === 'drive') {
    if (r.ref && r.ref.drive) detail = h('a', { href: state.buildUrl('drive', { car, drive: r.ref.drive, period }) }, going ? 'Follow the drive' : 'Open the drive');
    title = going ? ['From ', placeA(r.start, 'timeline.link.geofence_start'), ', still driving'] : [placeA(r.start, 'timeline.link.geofence_start'), h('span', { class: 'tl-to' }, ' to '), placeA(r.end, 'timeline.link.geofence_end')];
    figs = [
      fig('Distance', q(r.distance_km, 'distance'), going ? 'so far' : null, 'timeline.col.distance'),
      fig('Took', formatDuration(len)),
      fig('Energy used', r.energy_delta_kwh != null ? q(Math.abs(r.energy_delta_kwh), 'energy') : q(null, 'energy', { why: 'not measured' }), estimated, 'timeline.col.energy_diff'),
      fig('Battery at the end', q(r.soc_end_pct, 'percent'), signed(r.soc_delta_pct, 'percent'), ['timeline.col.soc', 'timeline.col.soc_diff']),
      fig('Range at the end', q(r.range_end_km, 'range'), signed(r.range_delta_km, 'range'), ['timeline.col.range', 'timeline.col.range_diff']),
      fig('Outside', q(r.outside_avg_c, 'temperature'), 'average', 'timeline.col.temp'),
      fig('Odometer', q(r.odometer_km, 'odometer'), 'at the end', 'timeline.col.odometer'),
    ];
  } else if (r.kind === 'charge') {
    if (r.ref && r.ref.charge) detail = h('a', { href: state.buildUrl('charge', { car, charge: r.ref.charge, period }) }, 'Open the charge');
    title = [going ? 'Charging at ' : 'At ', placeA(r.start, 'timeline.link.geofence_start')];
    figs = [
      fig('Added', r.energy_delta_kwh != null ? plus(q(r.energy_delta_kwh, 'energy')) : null, estimated),
      fig('Charging for', formatDuration(len)),
      fig('Battery at the end', q(r.soc_end_pct, 'percent'), signed(r.soc_delta_pct, 'percent')),
      fig('Range at the end', q(r.range_end_km, 'range'), signed(r.range_delta_km, 'range')),
      r.cost ? fig('Cost', format(r.cost.amount, 'money', { currency: r.cost.currency })) : null,
      fig('Outside', q(r.outside_avg_c, 'temperature'), 'average'),
      fig('Odometer', q(r.odometer_km, 'odometer')),
    ];
  } else if (r.kind === 'parked') {
    const p = r.parked_ms || {};
    cls = (p.asleep || 0) > len * 0.6 ? 'asleep' : 'parked';
    title = [going ? 'At ' : 'At ', placeA(r.start, 'timeline.link.geofence_start'), r.plugged ? ', plugged in' : '', going ? ', still parked' : ''];
    figs = [
      fig('Parked for', formatDuration(len)),
      fig('Used', r.energy_delta_kwh != null ? q(Math.abs(r.energy_delta_kwh), 'energy') : null, estimated ? [estimated, ' while standing'] : 'while standing'),
      fig('Battery at the end', q(r.soc_end_pct, 'percent'), signed(r.soc_delta_pct, 'percent')),
      fig('Range at the end', q(r.range_end_km, 'range'), signed(r.range_delta_km, 'range')),
      fig('Outside', q(r.outside_avg_c, 'temperature'), 'average'),
    ];
    extra.push(splitBar(p, len));
    if (r.awake_reasons && r.awake_reasons.length) {
      const awakeMs = (p.awake || 0) + (p.awake_vehicle_bus_only || 0);
      const long = awakeMs > 3600000 || awakeMs > len * 0.25;
      extra.push(h('p', { class: 'tl-why' }, 'Stayed awake for ', formatList(r.awake_reasons.map((x) => AWAKE_WORDS[x] || x.replace(/_/g, ' '))), '.',
        long ? r.awake_reasons.map((x) => (AWAKE_HELP[x] ? [' ', h('span', { class: 'tl-why-help' }, AWAKE_HELP[x])] : null)) : null));
    }
  } else if (r.kind === 'gap') {
    title = r.end && r.start ? ['No data between ', placeA(r.start, 'timeline.link.place_cells_other_rows'), ' and ', placeA(r.end, 'timeline.link.place_cells_other_rows')]
      : r.start ? ['No data, the car at ', placeA(r.start, 'timeline.link.place_cells_other_rows')] : ['No data'];
    figs = [
      fig('Missing for', formatDuration(len), going ? 'so far' : null),
      fig('The car moved', r.distance_km ? q(r.distance_km, 'distance') : null, 'by the odometer'),
      fig('Why', r.gap_reason ? GAP_WORDS[r.gap_reason] || r.gap_reason.replace(/_/g, ' ') : 'not known'),
    ];
    extra.push(h('p', { class: 'tl-why' }, GAP_HELP[r.gap_reason] || 'No data reached Sentry Mate for this time.'));
    extra.push(h('p', { class: 'tl-why tl-once', cover: 'timeline.row.parking_gap_overlap' },
      r.distance_km ? 'The car moved but no drive was recorded. ' : '',
      'This time is listed once, here: it is never also counted as parked time.'));
  } else if (r.kind === 'update') {
    const fp = r.detected_by === 'fingerprint';
    const swHref = `${state.buildUrl('software', { car, period })}${r.ref && r.ref.update ? `#update-${r.ref.update}` : ''}`;
    detail = h('a', { href: swHref }, fp ? (r.version ? 'Its fingerprints on Software' : 'Name it on Software') : 'Open Software');
    title = fp
      ? ['Tesla update installed', r.version ? [': ', h('span', { class: 'tl-version' }, r.version)] : '', r.start ? [' at ', placeA(r.start, 'timeline.link.place_cells_other_rows')] : '']
      : [r.version ? ['Software ', h('span', { class: 'tl-version' }, r.version)] : 'Software changed', r.start ? [' at ', placeA(r.start, 'timeline.link.place_cells_other_rows')] : ''];
    figs = [
      fig(fp ? 'Between' : 'Took', fp ? `${formatTime(r.start_ms, tz)} and ${dayKey(end, tz) === dayKey(r.start_ms, tz) ? formatTime(end, tz) : formatDateTime(end, tz, { now })}` : formatDuration(len), fp ? 'last old, first new fingerprint' : going ? 'so far' : null),
      fig('Named', r.version ? VERSION_SOURCE[r.version_source] || 'by the car' : fp ? 'not yet' : null),
      fp && r.changed_modules && r.changed_modules.length ? fig('Changed', formatList(r.changed_modules.map((m) => MODULE_NAME[m] || m))) : null,
      fig('Battery', q(r.soc_end_pct, 'percent')),
    ];
    if (fp) extra.push(h('p', { class: 'tl-why' }, 'Found from the firmware fingerprints the modules broadcast on the vehicle bus: the car does not send its version number. ',
      r.version_source === 'build_match' ? 'Named from an earlier update with the same fingerprints.'
        : r.version && LIVE_VERSION.has(r.version_source) ? `Named ${VERSION_SOURCE[r.version_source]}, which reported this version within a day after the update.`
          : r.version ? '' : 'An admin can type the version from the car’s screen.'));
    else if (!r.version) extra.push(h('p', { class: 'tl-why' }, 'Seen as changed module builds; the version number can be typed on the Software page.'));
  }

  const endTime = going ? 'now' : dayKey(r.end_ms, tz) === dayKey(r.start_ms, tz) ? formatTime(r.end_ms, tz) : null;
  // the row's span, without making it the tab's period: Back and later pages keep the one chosen
  const tripHref = spanHref('trip', { car, from: r.start_ms, to: Math.max(end, r.start_ms + 60000) });
  return h('li', { class: ['tl-item', `tl-${cls}`, { 'is-now': going }], dataset: { id: r.id } },
    h('div', { class: 'tl-time' },
      h('a', { class: 'tl-start', href: tripHref, 'aria-label': `${formatTime(r.start_ms, tz)}: the Trip for this span` },
        h('time', { datetime: new Date(r.start_ms).toISOString() }, formatTime(r.start_ms, tz))),
      h('span', { class: 'tl-end' }, endTime ? `to ${endTime}` : [h('span', { class: 'tl-end-day' }, `to ${formatDate(r.end_ms, tz, { weekday: false, now })},`), formatTime(r.end_ms, tz)])),
    h('div', { class: 'tl-mark', 'aria-hidden': 'true' }),
    h('div', { class: 'tl-body' },
      h('p', { class: 'tl-kind' }, KIND_WORD[r.kind] || r.kind, going ? ', still going' : '', isImported(r) ? [' ', flag('Imported')] : null),
      h('p', { class: 'tl-title' }, title),
      h('dl', { class: 'tl-figs' }, figs.filter(Boolean)),
      extra,
      sourceMark(r, { block: true, cls: 'tl-src' }),
      h('p', { class: 'tl-links' },
        detail ? h('span', { class: 'tl-detail', cover: 'timeline.link.slot_details' }, detail) : null,
        h('a', { href: tripHref, cover: 'timeline.link.trip_span' }, 'Trip for this span'))));
}

/** A parked spell's time, asleep, awake, awake on the vehicle bus only and unknown, as a ruled bar and words. */
function splitBar(p, len) {
  const parts = [['asleep', p.asleep, 'asleep'], ['online', p.awake, 'awake'], ['online_vehicle_bus_only', p.awake_vehicle_bus_only, 'vehicle bus only'], ['unknown', p.unknown, 'unknown']];
  const total = parts.reduce((a, [, v]) => a + (v || 0), 0) || len || 1;
  const words = parts.filter(([, v]) => v > 30000).map(([k, v, w]) => h('span', { class: 'tl-split-word' }, h('span', { class: ['key-swatch', `strip-${k}`], 'aria-hidden': 'true' }), `${w} ${formatDuration(v)}`));
  return h('div', { class: 'tl-split' },
    h('div', { class: 'tl-split-bar', 'aria-hidden': 'true' },
      parts.filter(([, v]) => v > 0).map(([k, v]) => h('span', { class: ['tl-split-seg', `strip-${k}`], vars: { '--w': `${((v / total) * 100).toFixed(2)}%` } }))),
    h('p', { class: 'tl-split-words' }, words.length ? words.flatMap((w, i) => (i ? [sep(), w] : [w])) : 'no split recorded',
      !(p.unknown > 30000) && words.length ? [sep(), 'no unknown time'] : null));
}

/** Short help next to the awake reasons: what keeps a car awake, and what Sentry Mate cannot see. */
function awakeHelp() {
  return h('section', { class: 'awake-help', cover: ['faq.no-sleep.accessory', 'faq.no-sleep.mcu1', 'faq.no-sleep.log-timeline', 'faq.no-sleep.state-image', 'faq.no-sleep.other-loggers'] },
    h('h2', { class: 'label' }, 'When a car will not sleep'),
    h('ul', { class: 'help-list' },
      h('li', null, h('strong', null, 'Look at the parked rows. '), 'Each splits its time into asleep, awake and unknown, and says what kept the car awake, from the car’s own messages.'),
      h('li', null, h('strong', null, 'Accessory power. '), 'With the car’s accessory power setting on, the car stays awake while parked. Turn it off on the car’s screen if you do not need it.'),
      h('li', null, h('strong', null, 'Older Model S and X. '), 'Cars with the older media computer (built before about March 2018) sleep less readily; their parked rows show more awake time.'),
      h('li', null, h('strong', null, 'Other apps. '), 'Sentry Mate only listens and never wakes the car, but an app that asks Tesla’s servers for data can keep it awake.'),
      h('li', null, h('strong', null, 'A healthy night '), 'looks like one parked row, mostly asleep, with the strip above in the asleep colour from a few minutes after the car is locked until the next drive.')),
    h('dl', { class: 'awake-words' }, Object.entries(AWAKE_HELP).filter(([k]) => k !== 'keep_climate_dog' && k !== 'keep_climate_camp').map(([k, t]) =>
      h('div', null, h('dt', null, cap(AWAKE_WORDS[k] || k)), h('dd', null, t)))));
}

const cap = (s) => s.charAt(0).toUpperCase() + s.slice(1);
