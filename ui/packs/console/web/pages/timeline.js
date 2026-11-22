// Timeline (/cars/{car}/timeline): one line per event in time order, newest first: the time (opens the
// span as a trip), a type tag [drive] [charge] [park] [gap] [update] (opens the drive, charge or software
// page), a summary (places open the place editor) and the duration. Days are separated by a rule with the
// date. Above it: the states now and their shares, and the state strip. A filter line picks the kinds,
// a place and a text (GET /cars/{car}/timeline?kinds=&place=&q=, paged).

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, dayKey } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { head, section, kv, note } from '../core/ui.js';
import { num, stateWord, when, ago, tag } from '../core/fmt.js';
import { stateStrip } from '../core/strip.js';
import { empty, loadingLine, errorWords } from '../core/states.js';
import { src, isEstimated } from '../core/source.js';
import { modWords } from './firmware.js';

export const id = 'timeline';

const KIND = {
  drive: { tag: 'drive', tone: 'drive', plural: 'drives' },
  charge: { tag: 'charge', tone: 'charge', plural: 'charges' },
  parked: { tag: 'park', tone: 'park', plural: 'parked' },
  gap: { tag: 'gap', tone: 'gap', plural: 'gaps' },
  update: { tag: 'update', tone: 'update', plural: 'updates' },
};
const SHOWN = { live: 'live history', import: 'imported history', 'import:teslamate': 'the TeslaMate import', 'import:teslafi': 'the TeslaFi import' };
const GAP = {
  no_log: 'no log arrived for this time', not_recorded: 'recording was paused on the CAN Bridge',
  failed_check: 'a log failed its check, so its data is not used', dropped_frames: 'the log lost frames here',
};
const AWAKE = {
  sentry: 'sentry', keep_climate_on: 'keep climate', keep_climate_dog: 'dog mode', keep_climate_camp: 'camp mode', user_present: 'someone inside',
  preconditioning_cabin: 'cabin preconditioning', preconditioning_battery: 'battery preconditioning', charging: 'charging', keep_warm: 'battery kept warm',
  door_open: 'a door open', accessory_power: 'accessory power',
};

export const placeLabel = (p) => p?.place?.name || p?.label || (typeof p?.address === 'string' ? p.address : p?.address?.short) || (p?.lat != null ? `${p.lat.toFixed(4)}, ${p.lon.toFixed(4)}` : null);
export const placeHref = (p) => (p?.place?.id ? state.buildUrl('places', { query: { place: p.place.id } })
  : p?.lat != null ? state.buildUrl('places', { query: { lat: p.lat.toFixed(5), lon: p.lon.toFixed(5), new: 1 } }) : null);
/** A place as a link to its editor (or to make the spot a place). */
export function placeLink(p) {
  const text = placeLabel(p);
  if (!text) return null;
  const href = placeHref(p);
  return href ? h('a', { class: 'lnk-plain', href, title: p.place ? `edit the place ${p.place.name}` : 'make this spot a place' }, text) : h('span', null, text);
}
const sign = (v, kind) => (v == null ? null : format(v, kind, { sign: true }));

/** One event line. */
export function eventLine(r, { car, tz }) {
  const k = KIND[r.kind] || KIND.gap;
  const tripHref = state.buildUrl('trip', { car, period: { from: r.start_ms, to: r.end_ms ?? null } });
  const detailHref = r.ref?.drive ? state.buildUrl('drive', { car, drive: r.ref.drive }) : r.ref?.charge ? state.buildUrl('charge', { car, charge: r.ref.charge })
    : r.kind === 'update' ? `${state.buildUrl('software', { car })}${r.detected_by === 'fingerprint' && r.ref?.update ? `#upd-${r.ref.update}` : ''}` : null;
  const est = isEstimated(r) ? ' est.' : '';
  const arrow = h('span', { class: 'arrow' }, ' -> ');
  let what;
  const bits = [];
  if (r.kind === 'drive') {
    what = [placeLink(r.start), arrow, r.end && r.end_ms != null ? placeLink(r.end) : h('span', { class: 'tone--accent' }, 'driving now')];
    bits.push(r.energy_delta_kwh != null ? `${format(-r.energy_delta_kwh, 'energy')} used${est}` : null);
  } else if (r.kind === 'charge') {
    what = [h('span', { class: 'muted' }, 'at '), placeLink(r.start) || 'an unknown spot'];
    bits.push(r.energy_delta_kwh != null ? `+${format(r.energy_delta_kwh, 'energy')}${est}` : null, r.range_delta_km != null ? `${sign(r.range_delta_km, 'range')} range` : null, r.cost ? format(r.cost.amount, 'money', { currency: r.cost.currency }) : null);
  } else if (r.kind === 'parked') {
    what = [placeLink(r.start) || 'parked'];
    const pm = r.parked_ms || {};
    bits.push(pm.asleep ? `asleep ${formatDuration(pm.asleep)}` : null, pm.awake ? `awake ${formatDuration(pm.awake)}` : null,
      pm.awake_vehicle_bus_only ? `vehicle bus only ${formatDuration(pm.awake_vehicle_bus_only)}` : null, pm.unknown ? h('span', { class: 'tone--bad' }, `no data ${formatDuration(pm.unknown)}`) : null,
      r.energy_delta_kwh != null ? `${format(Math.abs(r.energy_delta_kwh), 'energy')} used${est}` : null,
      r.awake_reasons?.length ? `kept awake by ${r.awake_reasons.map((x) => AWAKE[x] || x.replace(/_/g, ' ')).join(', ')}` : null);
  } else if (r.kind === 'gap') {
    const a = placeLink(r.start), b = r.end ? placeLink(r.end) : null;
    what = [a, a && b ? arrow : null, b, h('span', { class: 'tone--bad' }, `${a || b ? ' ' : ''}no data`)];
    bits.push(GAP[r.gap_reason] || 'no data for this time', r.distance_km ? `moved ${format(r.distance_km, 'distance')} without a drive` : null);
  } else {
    if (r.detected_by === 'fingerprint') {
      // found from the modules' firmware fingerprints: the version is what an admin typed (or none yet)
      what = ['Tesla update installed', r.version ? h('span', { class: 'muted' }, ` ${r.version}`) : h('span', { class: 'tone--warn' }, ' not named yet')];
      bits.push(r.changed_modules?.length ? `changed: ${modWords(r.changed_modules, { short: true })}` : null);
    } else what = ['software update', r.version ? h('span', { class: 'muted' }, ` ${r.version}`) : null];
  }
  if (r.soc_end_pct != null && r.kind !== 'gap' && r.kind !== 'update') bits.push(`${format(r.soc_end_pct - (r.soc_delta_pct || 0), 'percent')} -> ${format(r.soc_end_pct, 'percent')} (${sign(r.soc_delta_pct, 'percent')})`);
  if (r.range_end_km != null && r.kind !== 'gap') bits.push(`range ${format(r.range_end_km, 'range')}${r.range_delta_km != null && r.kind !== 'charge' ? ` (${sign(r.range_delta_km, 'range')})` : ''}`);
  if (r.outside_avg_c != null) bits.push(`${format(r.outside_avg_c, 'temperature')} out`);
  if (r.odometer_km != null) bits.push(`odo ${format(r.odometer_km, 'odometer')}`);
  // where it came from: [smo] [api] [imp], the full line in its title
  bits.push(src(r));
  const end = r.end_ms;
  const numCell = r.kind === 'drive' ? num(r.distance_km, 'distance') : r.kind === 'charge' && r.energy_delta_kwh != null ? num(r.energy_delta_kwh, 'energy') : null;
  return h('div', { class: ['ln', 'ln--ev', `ln--${k.tone}`], dataset: { row: '', href: detailHref || tripHref } },
    h('a', { class: 'ln-t lnk-plain', href: tripHref, title: `open ${formatTime(r.start_ms, tz)} to ${end ? formatTime(end, tz) : 'now'} as a trip` }, formatTime(r.start_ms, tz)),
    h('span', { class: 'ln-tag' }, detailHref ? h('a', { class: `tag tag--${k.tone} lnk-plain`, href: detailHref }, `[${k.tag}]`) : tag(k.tag, k.tone)),
    h('span', { class: 'ln-what' },
      h('span', { class: 'ln-main' }, h('span', { class: `ln-ph-tag tag tag--${k.tone}` }, `[${k.tag}] `), what),
      h('span', { class: 'ln-detail' }, h('span', { class: 'muted' }, `to ${end ? formatTime(end, tz) : 'now'}`), bits.filter(Boolean).flatMap((b) => [h('span', { class: 'sep' }, '\u00a0| '), h('span', { class: 'muted' }, b)]))),
    h('span', { class: 'ln-num' }, h('span', { class: 'ln-dur' }, r.duration_ms != null ? formatDuration(r.duration_ms) : end ? formatDuration(end - r.start_ms) : ''), numCell ? h('span', { class: 'ln-n2' }, numCell) : null));
}

/** A day rule: '-- Monday, Nov 16, 2026 ----'. */
export const dayRule = (ms, tz, side = null) => h('div', { class: 'ln-day', role: 'presentation' }, h('span', null, formatDate(ms, tz, { weekday: 'long', year: true })), side ? h('span', { class: 'ln-day-sum' }, side) : null);

export async function mount(ctx) {
  const { car, tz, period } = ctx;
  const cid = car.id;
  const enc = encodeURIComponent(cid);
  const from = period.from_ms, to = period.to_ms ?? ctx.api.now();
  const q = ctx.query;
  const kinds = new Set((q.kinds || 'drive,charge,parked,gap,update').split(',').filter((x) => KIND[x]));
  let placeId = q.place || '';
  let text = q.q || '';

  let states, act, overlaps;
  try {
    [states, act, overlaps] = await Promise.all([
      ctx.api.get(`cars/${enc}/states`, { from_ms: from, to_ms: to, limit: 1 }),
      ctx.api.get(`cars/${enc}/activity`, { from_ms: from ?? 0, to_ms: to }),
      // spans where imported and live history both exist: only one side is listed for each
      ctx.api.get(`cars/${enc}/overlaps`).then((o) => (o.items || []).filter((x) => x.start_ms < to && (from == null || x.end_ms > from))).catch(() => []),
    ]);
  } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;

  const hd = head(ctx, { title: 'Timeline' });
  const btns = hd.querySelectorAll('.ph-btn');
  if (btns[0]) btns[0].dataset.cover = 'timeline.control.time_range states.control.time_range';
  if (btns[1]) btns[1].dataset.cover = 'timeline.filter.car states.filter.car';

  /* ---------- filter line */
  const kindKeys = h('span', { class: 'keys', role: 'group', 'aria-label': 'Kinds of entry', cover: ['timeline.filter.action', 'timeline.table.column_filter'] },
    Object.entries(KIND).map(([k, v]) => h('button', { class: 'key', type: 'button', 'aria-pressed': String(kinds.has(k)),
      onclick: (e) => { if (kinds.has(k) && kinds.size === 1) return; if (kinds.has(k)) kinds.delete(k); else kinds.add(k); e.currentTarget.setAttribute('aria-pressed', String(kinds.has(k))); reload(); } }, v.plural)));
  const placeSel = h('select', { class: 'in', 'aria-label': 'Place' }, h('option', { value: '' }, 'place: any'));
  ctx.api.page('places', { sort: 'visits' }, { limit: 100 }).then((pg) => {
    if (!ctx.alive()) return;
    placeSel.append(...pg.items.map((p) => h('option', { value: p.id, selected: p.id === placeId }, `place: ${p.name}`)));
    if (placeId && !pg.items.some((p) => p.id === placeId)) placeSel.append(h('option', { value: placeId, selected: true }, 'place: the one in the link'));
  }).catch(() => {});
  placeSel.addEventListener('change', () => { placeId = placeSel.value; reload(); });
  const textIn = h('input', { class: 'in', type: 'search', value: text, placeholder: 'text in a row', 'aria-label': 'Hide rows without this text' });
  let tt = 0;
  textIn.addEventListener('input', () => { clearTimeout(tt); tt = setTimeout(() => { text = textIn.value.trim(); reload(); }, 300); });
  ctx.onCleanup(() => clearTimeout(tt));

  /* ---------- states now */
  const share = states.share_pct || {};
  const cur = states.current || {};
  const live = { ...(car.now || {}) };
  const nowVal = h('span');
  const paintNow = () => {
    const st = live.state || (cur.state === 'online' ? 'parked' : cur.state) || 'unknown';
    const since = live.since_ms ?? cur.since_ms;
    replace(nowVal, stateWord(st, { stale: !!live.stale }), since ? h('span', { class: 'muted' }, ` since ${when(since, tz)}`) : null);
  };
  paintNow();

  /* ---------- the list */
  const list = h('div', { class: 'lines', role: 'list' });
  const count = h('span', { class: 'muted' });
  const csvLink = h('a', { class: 'lnk', download: '' }, 'csv');
  const moreBtn = h('button', { class: 'btn', type: 'button' }, 'older');
  const foot = h('div', { class: 'tbl-more', hidden: true }, moreBtn);
  const params = () => ({ from_ms: from, to_ms: period.to_ms, kinds: [...kinds].join(','), place: placeId || null, q: text || null });
  let pager = null, gen = 0, shown = 0, lastDay = null;

  async function loadMore(my) {
    moreBtn.disabled = true;
    try {
      const items = await pager.more();
      if (my !== gen || !ctx.alive()) return;
      if (!shown) list.replaceChildren();
      for (const r of items) {
        const dk = dayKey(r.start_ms, tz);
        if (dk !== lastDay) { lastDay = dk; list.append(dayRule(r.start_ms, tz)); }
        list.append(eventLine(r, { car: cid, tz }));
      }
      shown += items.length;
      foot.hidden = pager.done;
      count.textContent = `${formatNumber(shown)}${pager.done ? '' : '+'} ${shown === 1 ? 'entry' : 'entries'}`;
      if (!shown) replace(list, empty(text ? `no entry contains "${text}"` : placeId ? 'nothing at this place in the range' : 'nothing in this range', kinds.size < 5 ? 'turn more kinds on, or pick a longer range' : 'pick a longer range'));
    } catch (e) {
      if (my !== gen || e.name === 'AbortError') return;
      if (e.status === 400 && e.fields?.place && placeId) {
        replace(list, empty('this place no longer exists', h('button', { class: 'btn', type: 'button', onclick: () => { placeId = ''; placeSel.value = ''; reload(); } }, 'every place')));
        return;
      }
      const w = errorWords(e);
      replace(list, h('p', { class: 'errbox-line', role: 'alert' }, tag(w.tag, 'bad'), ' ', w.text));
    } finally { moreBtn.disabled = false; }
  }
  moreBtn.addEventListener('click', () => loadMore(gen));
  async function reload() {
    const my = ++gen;
    shown = 0; lastDay = null;
    ctx.setQuery({ kinds: kinds.size === 5 ? null : [...kinds].join(','), place: placeId || null, q: text || null });
    pager = ctx.api.pager(`cars/${enc}/timeline`, params(), { limit: 60 });
    csvLink.href = ctx.api.csvUrl(`cars/${enc}/timeline`, params());
    // the loading line only the first time: a live reload keeps the entries on screen until the new ones are in
    if (!list.firstChild) replace(list, loadingLine('loading the timeline'));
    await loadMore(my);
  }

  // what the marks mean, and the overlaps in the range (where only one side is listed)
  function srcNote() {
    const key = (kind, label, site) => src({ sources: [{ kind, label, ...(site ? { site } : {}) }] });
    return [
      h('p', { class: 'note' }, 'data from: ', key('smo', 'Data from SMO'), ' smo (the CAN Bridge)  ', key('tesla_api', 'Data from the Tesla API'), ' the tesla api  ',
        key('teslafi_api', 'Data from TeslaFi'), ' teslafi  ', key('import', 'Imported from TeslaMate', 'teslamate'), ' imported (teslamate or teslafi)', car.sources?.length ? h('span', { class: 'muted' }, `. ${car.name} has: `) : null, car.sources?.length ? src(car) : null),
      overlaps.length ? h('p', { class: 'note tone--warn' }, tag('overlap', 'warn'), ' ',
        overlaps.map((o, i) => [i ? '; ' : '', `${formatDate(o.start_ms, tz, { year: false })} ${formatTime(o.start_ms, tz)} to ${formatDate(o.end_ms, tz, { year: false })} ${formatTime(o.end_ms, tz)}: `,
          o.kind === 'imports' ? `${SHOWN[o.shown_source] || 'one import'} is listed (TeslaMate and TeslaFi imports both cover it)` : `${SHOWN[o.shown_source || (o.shown === 'teslamate' ? 'import:teslamate' : 'live')] || 'imported history'} is listed (live and imported both exist)`]),
        '. ', ctx.admin ? h('a', { class: 'lnk', href: `${state.buildUrl('import')}#overlaps` }, 'choose in import') : 'an admin chooses which in import') : null,
    ];
  }

  const stripFrom = from ?? (act.items?.[0]?.start_ms ?? to - 86400000);
  replace(ctx.view, h('div', { class: 'page', dataset: { page: 'timeline' }, cover: ['shots.states', 'shots.timeline'] },
    hd,
    h('div', { class: 'filt' }, h('span', { class: 'filt-p' }, 'filter>'), kindKeys, h('span', { class: 'filt-sel', cover: 'timeline.filter.address' }, placeSel), textIn),
    section({ title: 'states' },
      kv([
        ['now', nowVal, { cover: 'states.stat.current' }],
        ['last change', states.last_change_ms ? [when(states.last_change_ms, tz), h('span', { class: 'muted' }, ` (${ago(states.last_change_ms)})`)] : null, { cover: 'states.stat.last_change' }],
        ['parked', [num(share.parked, 'percent'), h('span', { class: 'muted' }, ' of known time')], { cover: 'states.stat.parked_pct' }],
        ['asleep', [num(share.asleep, 'percent'), h('span', { class: 'muted' }, ' both buses quiet')]],
        ['awake', [num(share.awake, 'percent'), h('span', { class: 'muted' }, ' driving included')]],
        ['no data', [num(share.unknown, 'percent'), h('span', { class: 'muted' }, ' no log, paused or failed check')]],
      ]),
      note('The shares overlap on purpose: parked time includes asleep time.'),
      stateStrip(ctx, act.items || [], stripFrom, to, tz, { cover: ['states.chart.states', 'states.chart.legend'] })),
    section({ title: 'entries', side: [count, csvLink], cover: ['timeline.table', 'projects.dash', 'timeline.row.drive', 'timeline.row.charging', 'timeline.row.parking', 'timeline.row.missing', 'timeline.row.parking_gap_overlap', 'timeline.row.updating',
      'timeline.col.start', 'timeline.col.end', 'timeline.col.action', 'timeline.col.start_address', 'timeline.col.end_address', 'timeline.col.odometer', 'timeline.col.duration', 'timeline.col.distance', 'timeline.col.range', 'timeline.col.range_diff', 'timeline.col.energy_diff', 'timeline.col.soc', 'timeline.col.soc_diff', 'timeline.col.temp',
      'timeline.link.trip_span', 'timeline.link.slot_details', 'timeline.link.geofence_start', 'timeline.link.geofence_end', 'timeline.link.place_cells_other_rows'] },
    note('newest first. A time opens that span as a trip; a [tag] opens the drive, charge or software page; a place opens the place editor. A gap is never counted as parked time; the text filter only hides rows.'),
    srcNote(), list, foot),
    section({ title: 'why a car stays awake' },
      kv([
        ['normal', 'After a drive the car stays awake for a while, then both buses go quiet and the line turns to [park] asleep. Short awake spells while parked are normal (the car checks its 12 V battery).', { cover: 'faq.no-sleep.log-timeline' }],
        ['the strip', 'A long run of - while parked means something kept the car awake; the [park] line says what.', { cover: 'faq.no-sleep.state-image' }],
        ['accessory', 'With accessory power kept on, the car stays awake even with nothing plugged in.', { cover: 'faq.no-sleep.accessory' }],
        ['older S / X', 'Cars with the older media computer sleep less deeply and wake more often.', { cover: 'faq.no-sleep.mcu1' }],
        ['other apps', 'Sentry Mate only listens to the car, and its own Tesla API reading never wakes it (an asleep car only shows in the car list). Another app that asks Tesla’s servers for data can wake it.', { cover: 'faq.no-sleep.other-loggers' }],
      ], { cols: 1, cls: 'kv--prose' }))));

  await reload();
  if (!ctx.alive()) return;
  ctx.ready();

  ctx.events('now', (ev) => { if (ev.data?.car !== cid || !ev.data.now) return; Object.assign(live, ev.data.now); paintNow(); });
  let rt = 0;
  ctx.events('rows', (ev) => { if (ev.data?.car !== cid) return; clearTimeout(rt); rt = setTimeout(() => { if (ctx.alive()) reload(); }, 2000); });
  ctx.onCleanup(() => clearTimeout(rt));
}
