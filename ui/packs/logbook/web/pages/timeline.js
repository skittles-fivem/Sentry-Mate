// Sentry Mate, "Logbook" UI pack: Timeline (/cars/{car}/timeline).
//
// Everything the car did in the shared period, as one strip and one journal: drives, charges, parked
// spells (asleep, awake and unknown time apart), gaps with their reason, and software changes. Data:
// GET /api/v1/cars/{car}/states (current state, last change, shares), /activity (the strip),
// /timeline (the rows, newest first, cursor paged; kinds and text filters; CSV), /trip (the period's
// figures) and /tracks (the period's roads on the map).

import { h, replace } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { formatDateTime, formatDuration, dayKey, formatDayKey, addDays, periodLabel } from '../shared/time.js';
import { formatNumber, tn } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { pageHead, q, plus, figure, icon, empty, errorView, loadingBlock, button, stateLabel } from '../core/ui.js';
import { activityStrip, timelineRow } from '../core/journal.js';
import { makeMap, boundsOf } from '../core/map.js';

export const id = 'timeline';
export const css = true;
export const title = () => 'Timeline';

const KINDS = [['drive', 'Drives'], ['charge', 'Charges'], ['parked', 'Parked'], ['gap', 'Gaps'], ['update', 'Software']];

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz;
  const kinds = new Set((ctx.query.kinds || '').split(',').filter((k) => KINDS.some(([x]) => x === k)));
  if (!kinds.size) KINDS.forEach(([k]) => kinds.add(k));
  let text = ctx.query.q || '';

  const head = h('div');
  const summary = h('section', { class: 'state-summary', cover: ['states.stat.current', 'states.stat.last_change', 'states.stat.parked_pct'] });
  const stripBox = h('section', { class: 'strip-block', cover: ['states.chart.states', 'states.chart.legend', 'states.control.time_range', 'timeline.control.time_range', 'states.filter.car', 'timeline.filter.car'] });
  const checks = KINDS.map(([k, t]) => h('label', { class: 'check' }, h('input', { type: 'checkbox', value: k, checked: kinds.has(k) }), t));
  const qIn = h('input', { class: 'field-input', type: 'search', id: 'tl-q', value: text, placeholder: 'A place or an address', autocomplete: 'off' });
  const filters = h('div', { class: 'toolbar tl-filters', cover: ['timeline.filter.action', 'timeline.filter.address', 'timeline.table.column_filter'] },
    h('fieldset', { class: 'field kinds' }, h('legend', { class: 'field-label' }, 'Show'), h('div', { class: 'checks' }, checks)),
    h('div', { class: 'field field-search' }, h('label', { class: 'field-label', for: 'tl-q' }, 'Find'),
      h('div', { class: 'field-control' }, icon('search', 'icon field-icon'), qIn),
      h('p', { class: 'field-hint' }, 'Only hides rows; the figures of the rows shown never change.')));
  const list = h('div', { class: 'tl-list' });
  const more = h('div', { class: 'list-more' });
  const count = h('p', { class: 'ledger-count', 'aria-live': 'polite' });
  const csv = h('a', { class: 'button button-quiet button-small', download: '', href: '#' }, icon('download'), 'Download CSV');
  const mapBox = h('div', { class: 'map map-day', role: 'region', 'aria-label': 'Map of the period’s drives', cover: 'projects.dash' });
  const sideFigs = h('dl', { class: 'today-figs' });
  const side = h('aside', { class: 'day-side', 'aria-label': 'The period at a glance' }, h('h2', { class: 'label' }, 'The period’s roads'), mapBox, sideFigs, awakeHelp());

  ctx.main.append(head, summary, stripBox, filters,
    h('div', { class: 'timeline-grid' },
      h('section', { class: 'timeline-main', 'aria-label': 'Entries, newest first', cover: ['timeline.table', 'timeline.row.drive', 'timeline.row.charging', 'timeline.row.parking', 'timeline.row.missing', 'timeline.row.parking_gap_overlap', 'timeline.row.updating', 'timeline.col.start', 'timeline.col.end', 'timeline.col.action', 'timeline.col.start_address', 'timeline.col.end_address', 'timeline.col.odometer', 'timeline.col.duration', 'timeline.col.distance', 'timeline.col.range', 'timeline.col.range_diff', 'timeline.col.energy_diff', 'timeline.col.soc', 'timeline.col.soc_diff', 'timeline.col.temp', 'timeline.link.trip_span', 'timeline.link.slot_details', 'timeline.link.geofence_start', 'timeline.link.geofence_end', 'timeline.link.place_cells_other_rows', 'shots.timeline', 'shots.states'] },
        h('div', { class: 'ledger-top' }, count, csv), list, more),
      side));

  const params = () => ({ ...ctx.params, kinds: [...kinds].join(','), q: text || undefined });
  let pager = null;
  async function loadRows() {
    pager = ctx.api.pager(api.apiPath`cars/${car.id}/timeline`, params(), { limit: 60 });
    replace(list, loadingBlock(8));
    csv.setAttribute('href', ctx.api.csvUrl(api.apiPath`cars/${car.id}/timeline`, params()));
    await moreRows(true);
  }
  async function moreRows(first = false) {
    try { await pager.more(); } catch (err) {
      if (ctx.signal.aborted) return;
      replace(list, errorView(err, { retry: () => loadRows() }));
      if (first) throw err;
      return;
    }
    const items = pager.items;
    count.textContent = pager.done ? tn(items.length, '{n} entry', '{n} entries') : `${formatNumber(items.length)} entries shown, more to load`;
    csv.hidden = !items.length;
    if (!items.length) {
      replace(list, empty({ title: text ? `No entry matches “${text}”.` : 'Nothing recorded in this period.', text: text || kinds.size < KINDS.length ? 'Clear the text or tick more kinds above.' : 'Choose a longer period at the top.' }));
      replace(more);
      return;
    }
    const groups = new Map();
    for (const r of items) { const k = dayKey(r.start_ms, tz); if (!groups.has(k)) groups.set(k, []); groups.get(k).push(r); }
    const today = dayKey(api.now(), tz);
    replace(list, [...groups.entries()].map(([k, rows]) => h('section', { class: 'tl-day', 'aria-label': formatDayKey(k, { weekday: 'long', month: 'long' }) },
      h('h3', { class: 'tl-day-name' }, k === today ? 'Today' : k === addDays(today, -1) ? 'Yesterday' : formatDayKey(k, { weekday: 'long', month: 'long', year: k.slice(0, 4) !== today.slice(0, 4) }),
        k === today || k === addDays(today, -1) ? h('span', { class: 'day-date' }, formatDayKey(k, { weekday: 'long', month: 'long', year: false })) : null),
      h('ol', { class: 'tl tl-full' }, rows.map((r) => timelineRow(r, { car: car.id, tz, full: true, period: ctx.state.period }))))));
    if (!pager.done) {
      const b = button('Show earlier entries', { kind: 'primary', onclick: async () => { b.disabled = true; b.textContent = 'Loading…'; await moreRows(); } });
      replace(more, b);
    } else replace(more, h('p', { class: 'list-more-note' }, 'The start of the period.'));
  }

  checks.forEach((c) => c.querySelector('input').addEventListener('change', (e) => {
    if (e.target.checked) kinds.add(e.target.value); else kinds.delete(e.target.value);
    if (!kinds.size) { kinds.add(e.target.value); e.target.checked = true; return; }
    ctx.setQuery({ kinds: kinds.size === KINDS.length ? null : [...kinds].join(',') });
    loadRows();
  }));
  let timer = 0;
  qIn.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(() => { text = qIn.value.trim(); ctx.setQuery({ q: text || null }); loadRows(); }, 250); });

  let sideMap = null;
  async function drawAll(c) {
    const [st, act, trip, tracks] = await Promise.allSettled([
      c.api.get(api.apiPath`cars/${car.id}/states`, { ...c.params, limit: 1 }),
      c.api.get(api.apiPath`cars/${car.id}/activity`, c.params),
      c.api.get(api.apiPath`cars/${car.id}/trip`, c.params),
      c.api.get('tracks', { cars: car.id, ...c.params, max_points: 4000 }),
    ]);
    if (c.signal.aborted) return;
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
    replace(head, pageHead({ eyebrow: `${car.name} · Timeline`, title: periodLabel(c.period, tz, api.now()), deck: sentence.join(' ') || c.info.purpose }));
    if (st.status === 'fulfilled') {
      const s = st.value;
      const sh = s.share_pct || {};
      replace(summary,
        figure('Now', h('span', { class: ['state', `state-${s.current?.state}`] }, h('span', { class: 'state-dot', 'aria-hidden': 'true' }), stateLabel(s.current?.state)), s.current?.since_ms ? `since ${formatDateTime(s.current.since_ms, tz, { now: api.now() })}` : null),
        figure('Last change', s.last_change_ms ? formatDateTime(s.last_change_ms, tz, { now: api.now() }) : '—', 'between awake, asleep and unknown'),
        figure('Parked', q(sh.parked, 'percent'), 'of the known time, not driving'),
        figure('Asleep', q(sh.asleep, 'percent'), 'both buses quiet'),
        figure('Awake', q(sh.awake, 'percent'), 'driving included'),
        figure('Unknown', q(sh.unknown, 'percent'), 'no log, paused or failed check'));
    } else replace(summary, errorView(st.reason, { eyebrow: 'State summary' }));
    replace(stripBox, act.status === 'fulfilled'
      ? activityStrip(act.value, { tz, link: (sg) => (sg.ref && sg.ref.drive ? state.buildUrl('drive', { car: car.id, drive: sg.ref.drive }) : sg.ref && sg.ref.charge ? state.buildUrl('charge', { car: car.id, charge: sg.ref.charge }) : null) })
      : errorView(act.reason, { eyebrow: 'Activity' }));
    if (t) {
      const added = (t.energy_kwh.added_ac || 0) + (t.energy_kwh.added_dc || 0);
      replace(sideFigs,
        figure('Drives', formatNumber((t.drives || []).length)), figure('Distance', q(t.distance.drives_km, 'distance')),
        figure('Energy used', q(t.energy_kwh.drive_net, 'energy')), figure('Charged', added ? plus(q(added, 'energy')) : '—'));
    }
    // The roads of the period: one line per drive.
    const drives = tracks.status === 'fulfilled' ? tracks.value.drives || [] : [];
    const lines = drives.map((d) => d.lon.map((x, i) => [x, d.lat[i]])).filter((l) => l.length > 1);
    if (!lines.length) { if (!sideMap) replace(mapBox, h('p', { class: 'map-note' }, 'No drives in this period.')); return; }
    const bounds = boundsOf(lines.flat());
    if (!sideMap) {
      sideMap = await makeMap(c, mapBox, { bounds, padding: 28, maxZoom: 15 });
      if (!sideMap) return;
      await sideMap.loaded;
    } else sideMap.frame(bounds);
    const src = sideMap.map.getSource('roads');
    const fc = { type: 'FeatureCollection', features: lines.map((l) => ({ type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: l } })) };
    if (src) src.setData(fc);
    else {
      sideMap.map.addSource('roads', { type: 'geojson', data: fc });
      sideMap.addLine({ id: 'roads-line', source: 'roads' }, 3, '--accent');
    }
  }

  await Promise.all([drawAll(ctx), loadRows()]);
  ctx.ready();
  return {
    async update(c) { await Promise.all([drawAll(c), loadRows()]); c.ready(); },
  };
}

/** Short help next to the awake reasons: what keeps a car awake, and what Sentry Mate cannot see. */
function awakeHelp() {
  return h('section', { class: 'awake-help', cover: ['faq.no-sleep.accessory', 'faq.no-sleep.mcu1', 'faq.no-sleep.log-timeline', 'faq.no-sleep.state-image', 'faq.no-sleep.other-loggers'] },
    h('h2', { class: 'label' }, 'When a car will not sleep'),
    h('ul', { class: 'help-list' },
      h('li', null, h('strong', null, 'Look at the parked rows. '), 'Each says what kept the car awake, from the car’s own messages: Sentry mode, keep-climate, preconditioning, someone in the car, charging, the battery kept warm or a door open.'),
      h('li', null, h('strong', null, 'Accessory power. '), 'With the car’s accessory power setting on, the car stays awake while parked. Turn it off on the car’s screen if you do not need it.'),
      h('li', null, h('strong', null, 'Older Model S and X. '), 'Cars with the older media computer (built before about March 2018) sleep less readily; their parked rows show more awake time.'),
      h('li', null, h('strong', null, 'Other apps. '), 'Sentry Mate only listens and never wakes the car, but an app that asks Tesla’s servers for data can keep it awake.'),
      h('li', null, h('strong', null, 'A healthy night '), 'looks like one parked row, mostly asleep, with the strip above in the asleep colour from soon after parking until the next drive.')));
}
