// Sentry Mate, "Logbook" UI pack: Car overview (/cars/{car}).
//
// What the car is doing now, why it is awake, and the chosen period (default 24 hours) at a glance.
// Data: GET /api/v1/cars/{car} (the car, with its configuration and sources), /cars/{car}/now (every value
// with its own time; updated live by `now` events), /settings (whether outside map links are on), and for
// the period: /cars/{car}/timeline, /activity, /series (battery % and range) and /trip (distance and
// consumption). Latest drive and charge: the first item of /drives and /charges. Arrival time: the open
// stop of /visits; whether the place is a service centre: its kind from /places/{place}.

import { h, svg, replace } from '../shared/dom.js';
import { format, parts, rangeLabel, rangeField, getUnits } from '../shared/units.js';
import { formatTime, formatDate, formatDateTime, formatDuration, formatRelative, dayKey, periodLabel } from '../shared/time.js';
import { formatList } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { pageHead, q, plus, figure, chip, icon, stateMark, empty, errorView, loadingBlock, sep, ago, routeSketch } from '../core/ui.js';
import { column, timeColumn, csvFilename, downloadCsv } from '../shared/csv.js';
import { makeMap, coordsText, boundsOf } from '../core/map.js';
import { stackedCharts, spansFromStates } from '../core/chart.js';
import { activityStrip, timelineRow, AWAKE_WORDS, endLabel } from '../core/journal.js';

export const id = 'car';
export const css = true;
export const title = () => 'Car overview';

const v = (x) => (x && typeof x === 'object' && 'value' in x ? x.value : x ?? null);
const at = (x) => (x && typeof x === 'object' ? x.at_ms ?? null : null);
const isPair = (x) => x && typeof x === 'object' && !Array.isArray(x) && 'value' in x;
/** Merge a `now` event (only what changed) into the object: { value, at_ms } pairs are replaced whole. */
export function mergeNow(base, delta) {
  const out = { ...base };
  for (const [k, d] of Object.entries(delta || {})) {
    const b = out[k];
    if (d && typeof d === 'object' && !Array.isArray(d) && !isPair(d) && b && typeof b === 'object' && !Array.isArray(b) && !isPair(b)) out[k] = mergeNow(b, d);
    else out[k] = d;
  }
  return out;
}
const COMPASS = ['north', 'northeast', 'east', 'southeast', 'south', 'southwest', 'west', 'northwest'];
const compass = (deg) => COMPASS[Math.round((((deg % 360) + 360) % 360) / 45) % 8];
const DOORS = [['fl', 'Front left door'], ['fr', 'Front right door'], ['rl', 'Rear left door'], ['rr', 'Rear right door']];
const CHARGE_WORDS = { disconnected: 'Not plugged in', no_power: 'Plugged in, no power from the station', about_to_charge: 'Plugged in, about to charge', charging: 'Charging', complete: 'Charge complete', stopped: 'Plugged in, charging stopped' };
const LIMIT_WORDS = { pilot: 'the station', cable: 'the cable', car: 'the car' };

export async function mount(ctx) {
  const car = ctx.car;
  const tz = ctx.tz;
  const [nowRes, settings] = await Promise.all([
    ctx.api.get(api.apiPath`cars/${car.id}/now`),
    ctx.api.get('settings', null, { maxAgeMs: 300000 }).catch(() => null),
  ]);
  let now = nowRes;
  const outsideMaps = !!(settings && settings.outside_links && settings.outside_links.maps);

  // The live part is drawn again whenever a `now` event arrives; the map stays and its pin moves.
  const live = h('div', { class: 'now-main' });
  const head = h('div');
  const mapBox = h('div', { class: 'map map-where', role: 'region', 'aria-label': 'Map of where the car is', cover: ['summary.map.position', 'summary.map.attribution', 'summary.map.fullscreen', 'summary.map.zoom'] });
  const whereText = h('div', { class: 'where-text' });
  const where = h('section', { class: 'where', 'aria-labelledby': 'where-title' }, whereText, mapBox);
  const latest = h('section', { class: 'latest', 'aria-label': 'Latest drive and charge' }, loadingBlock(3));
  const aside = h('aside', { class: 'now-aside', 'aria-label': 'The period at a glance' }, loadingBlock(6));
  const periodBox = h('div', { class: 'car-period', cover: 'shots.overview' });

  // The usual start time of charges at the place where the car is plugged in and waiting (the car does
  // not send its charging schedule on the CAN bus, so it is learned from past charges there).
  const usual = new Map();
  function usualStart() {
    const ch = now.charge || {};
    const place = now.location && now.location.place;
    if (!v(ch.plugged) || !place || ['charging', 'complete'].includes(v(ch.status))) return null;
    if (!usual.has(place.id)) {
      usual.set(place.id, { loading: true });
      ctx.api.page(api.apiPath`cars/${car.id}/charges`, { places: place.id, kind: 'ac' }, { limit: 30 })
        .then((r) => { usual.set(place.id, usualFrom(r.items, tz, place)); if (ctx.alive()) draw(); })
        .catch(() => usual.set(place.id, { none: true }));
    }
    return usual.get(place.id);
  }

  // `now` names the place by id and name only; its kind (is it a service centre?) comes from /places.
  const kinds = new Map();
  function placeKind() {
    const place = now.location && now.location.place;
    if (!place) return null;
    if (!kinds.has(place.id)) {
      kinds.set(place.id, null);
      ctx.api.get(api.apiPath`places/${place.id}`, null, { maxAgeMs: 300000 })
        .then((p) => { kinds.set(place.id, p.kind || null); if (ctx.alive()) draw(); })
        .catch(() => {});
    }
    return kinds.get(place.id);
  }
  // When the car arrived where it is: the open stop of /visits (left_ms null), not the start of the
  // current state (a car that fell asleep at home arrived there earlier).
  let visit = null;
  const visitKey = () => `${now.location?.place?.id ?? ''}|${now.state}|${!!now.stale}`;
  let visitFor = null;
  function currentVisit() {
    const key = visitKey();
    if (visitFor !== key) {
      visitFor = key;
      visit = undefined;
      ctx.api.page('visits', { cars: car.id }, { limit: 1 })
        .then((r) => { if (visitFor !== key) return; const x = r.items[0]; visit = x && x.left_ms == null ? x : null; if (ctx.alive()) draw(); })
        .catch(() => { if (visitFor === key) { visit = null; if (ctx.alive()) draw(); } });
    }
    return visit;
  }

  const stateBox = h('div', { class: 'state-box' });
  const drawState = () => replace(stateBox, stateLine(now, tz, placeKind()));
  function draw() {
    replace(head, header(car, now, tz));
    replace(whereText, whereLines(now, tz, outsideMaps, currentVisit()));
    drawState();
    replace(live, staleBanner(now, tz), h('p', { class: 'lede' }, lede(now, tz)), stateBox, hero(car, now, tz), vitals(car, now, tz), awake(now), statusFlags(now, tz), charging(now, tz, usualStart()), where, latest);
  }
  draw();
  // The time in the current state counts on by itself (a clock, not a request).
  const tick = setInterval(drawState, 30000);
  ctx.onCleanup(() => clearInterval(tick));
  ctx.main.append(head, h('div', { class: ['now-grid', `now-${now.state}`, { 'is-stale': !!now.stale }] }, live, aside), periodBox);

  // The map: where the car is, with a pin (an arrow along the heading when it is known).
  const lat = v(now.location?.lat), lon = v(now.location?.lon);
  let marker = null;
  const mapReady = lat != null && lon != null
    ? makeMap(ctx, mapBox, { center: [lon, lat], zoom: 14.2, fullscreen: true }).then(async (m) => {
      if (!m) return;
      await m.loaded;
      marker = m.addPin([lon, lat], now.location.place ? now.location.place.name : null, 'pin-here');
    }).catch((err) => replace(mapBox, h('p', { class: 'map-note' }, `The map did not load: ${err.message}`)))
    : (mapBox.append(h('p', { class: 'map-note' }, 'No position recorded yet.')), Promise.resolve());

  // Latest drive and charge.
  const latestP = Promise.all([
    ctx.api.page(api.apiPath`cars/${car.id}/drives`, null, { limit: 1 }),
    ctx.api.page(api.apiPath`cars/${car.id}/charges`, null, { limit: 1 }),
  ]).then(async ([d, c]) => {
    const drive = d.items[0] || null;
    let sketch = null;
    if (drive) {
      sketch = await ctx.api.get('tracks', { cars: car.id, from_ms: drive.start_ms, to_ms: (drive.end_ms ?? api.now()) + 1, max_points: 200 })
        .then((t) => (t.drives || []).find((x) => x.drive === drive.id)).catch(() => null);
    }
    replace(latest, driveCard(car, drive, sketch, tz), chargeCard(car, c.items[0] || null, now, tz));
  }).catch((err) => replace(latest, errorView(err, { eyebrow: 'Latest drive and charge' })));

  const periodP = drawPeriod(ctx, aside, periodBox);
  await Promise.all([latestP, periodP]);

  // Live: values pushed by the server as the car sends them; nothing is re-queried on a timer.
  ctx.events(['now', 'rows'], (ev) => {
    const d = ev.data || {};
    if (d.car !== car.id) return;
    if (ev.type === 'rows') { drawPeriod(ctx, aside, periodBox).catch(() => {}); return; }
    if (!d.now) return;
    now = mergeNow(now, d.now);
    draw();
    const la = v(now.location?.lat), lo = v(now.location?.lon);
    if (marker && la != null && lo != null) marker.setLngLat([lo, la]);
  });
  await Promise.race([mapReady, new Promise((r) => setTimeout(r, 4000))]);
  ctx.ready();
  return {
    async update(c, why = {}) {
      if (why.prefs) draw();
      await drawPeriod(c, aside, periodBox);
      c.ready();
    },
  };
}

/* ------------------------------------------------------------------ the head and the story */

function header(car, now, tz) {
  const cfg = car.config || {};
  const trim = cfg.trim && cfg.trim.value ? cfg.trim : null;
  const src = (s) => ({ can: 'from the car', vin: 'from the VIN', owner: 'typed by the owner', teslamate: 'imported' }[s] || s);
  const stale = !!now.stale;
  const sameDay = now.at_ms && dayKey(now.at_ms, tz) === dayKey(api.now(), tz);
  return pageHead({
    eyebrow: 'Car overview',
    title: h('span', { cover: 'summary.header.name' }, car.name || 'Unnamed car'),
    deck: h('span', { class: 'car-line' },
      h('span', { cover: 'summary.header.model', title: car.vin_prefix ? `Model and year from the VIN prefix ${car.vin_prefix}` : null }, [car.year, car.model].filter(Boolean).join(' ')),
      trim ? [sep(), h('span', { cover: 'summary.header.trim', title: `Trim ${src(trim.source)}` }, trim.value, h('span', { class: 'source-note' }, ` (${src(trim.source)})`))] : null,
      cfg.paint && cfg.paint.value ? [sep(), cfg.paint.value] : null,
      sep(), h('span', { class: 'mono' }, car.vin_prefix || car.id)),
    cls: 'car-head',
    cover: ['shots.web-interface'],
    aside: h('div', { class: ['freshness', { 'is-stale': stale }], cover: ['summary.data.live_push', 'summary.data.restored_values', 'nr.dash.last-update', 'summary.data.prefs_snapshot'] },
      h('p', { class: 'freshness-line' }, h('span', { class: 'freshness-dot', 'aria-hidden': 'true' }), 'Last seen ', h('strong', null, ago(now.at_ms))),
      h('p', { class: 'freshness-note' }, now.at_ms == null ? 'Nothing received yet.'
        : sameDay ? `Measured ${formatTime(now.at_ms, tz, { seconds: true })}, received ${formatTime(now.received_ms, tz, { seconds: true })}${now.delay_s != null ? ` (${now.delay_s} s later)` : ''}`
          : `Last report ${formatDateTime(now.at_ms, tz, { now: api.now() })}`),
      h('p', { class: 'freshness-note' }, 'Values usually arrive about 75 s after the car sends them; each shows its own time on hover.')),
  });
}

function staleBanner(now, tz) {
  if (!now.stale && now.state !== 'unknown') return null;
  const wasDriving = now.drive && now.drive.drive_id;
  return h('p', { class: 'stale-banner', role: 'note', cover: 'summary.state.drive_offline' },
    h('strong', null, `Last known values, ${formatDuration(api.now() - (now.at_ms || api.now()))} old.`),
    ` The last report came ${formatDateTime(now.at_ms, tz, { now: api.now() })}; nothing has arrived since${wasDriving ? ', in the middle of a drive' : ''}.`,
    ' ', h('a', { href: state.linkTo('health') }, 'See the car’s data health'));
}

function placeText(now) {
  const L = now.location || {};
  if (L.place) return L.place.name;
  if (L.address) return L.address.short || L.address.locality || 'an unnamed spot';
  return 'an unknown place';
}

function lede(now, tz) {
  const em = (t) => h('em', null, t);
  const p = placeText(now);
  const soc = format(v(now.soc_pct), 'percent');
  const ch = now.charge || {};
  if (now.state === 'unknown' || now.stale) return ['Last heard from ', formatRelative(now.at_ms, api.now()), ', at ', em(p), ` (${soc}).`];
  if (now.state === 'driving') {
    const d = now.drive || {};
    const hd = v(now.location?.heading_deg);
    return ['Driving', d.distance_km != null ? `, ${format(d.distance_km, 'distance')} since ${formatTime(d.start_ms, tz)}` : '', hd != null ? `, heading ${compass(hd)}` : '', ' near ', em(p), '.'];
  }
  if (now.state === 'charging') {
    const min = v(ch.to_full_min);
    return ['Charging at ', em(p), `: ${soc} now`, v(ch.limit_pct) != null ? `, to ${format(v(ch.limit_pct), 'percent')}` : '', min != null ? `, about ${formatDuration(min * 60000)} to go.` : '.'];
  }
  const verb = now.state === 'asleep' ? 'Asleep at ' : 'Parked at ';
  return [verb, em(p), ` since ${formatTime(now.since_ms, tz)}`, v(ch.plugged) ? ', plugged in.' : '.', v(ch.status) === 'complete' ? ` Charge complete at ${soc}.` : ''];
}

/* ------------------------------------------------------------------ battery and range */

function hero(car, now, tz) {
  const soc = v(now.soc_pct), usable = v(now.usable_soc_pct);
  const ch = now.charge || {};
  const rf = rangeField('range_km');
  const range = v(now[rf]) ?? v(now.range_km);
  const full = range != null && soc ? (range / soc) * 100 : null;
  const timeOf = (x) => (at(x) ? `measured ${formatTime(at(x), tz, { seconds: true })}` : null);
  const figs = [
    figure('Battery', q(soc, 'percent'), usable != null && soc != null && Math.abs(usable - soc) >= 1 ? `${format(usable, 'percent')} usable; the cold battery locks part of it` : v(ch.plugged) ? CHARGE_WORDS[v(ch.status)] || 'plugged in' : 'charge level', { cls: 'fig-hero', cover: ['summary.stat.soc', 'overview.stat.battery_level'] }),
    figure(rangeLabel(), q(range, 'range'), full != null ? `${format(full, 'range')} at a full charge` : null, { cls: 'fig-hero', cover: ['summary.stat.range_preferred', 'overview.stat.range'] }),
  ];
  if (now.state === 'driving' && !now.stale) {
    const pw = v(now.power_kw);
    figs.push(figure('Speed', q(v(now.speed_kph), 'speed'), pw != null ? `${pw < 0 ? 'regenerating' : 'drawing'} ${format(Math.abs(pw), 'power')}` : null, { cls: 'fig-hero', cover: 'summary.stat.speed' }));
  } else if (now.state === 'charging' && !now.stale) {
    figs.push(figure('Charging at', q(v(ch.power_kw), 'power'), [v(ch.kind) ? v(ch.kind).toUpperCase() : null, v(ch.volts_v) != null ? format(v(ch.volts_v), 'voltage') : null, v(ch.amps_a) != null ? format(v(ch.amps_a), 'current') : null].filter(Boolean).join(' · '), { cls: 'fig-hero', cover: ['summary.stat.charger_power', 'overview.stat.charging_power', 'nr.dash.charging-power'] }));
  } else {
    figs.push(figure('Energy stored', q(v(now.energy_remaining_kwh), 'energy'), 'from the battery’s own reading', { cls: 'fig-hero' }));
  }
  const limit = v(ch.limit_pct);
  const charging = now.state === 'charging' && !now.stale;
  const gauge = h('div', { class: ['gauge', { 'is-charging': charging }], cover: ['nr.dash.battery-level', 'nr.dash.battery-level.bands', 'nr.dash.charge-limit'] },
    svg('svg', { class: 'bar-gauge', width: '100%', height: '100%', 'aria-hidden': 'true', focusable: 'false', preserveAspectRatio: 'none' },
      svg('rect', { class: 'band band-low', x: '0%', y: 0, width: '20%', height: '100%' }),
      svg('rect', { class: 'band band-day', x: '20%', y: 0, width: '60%', height: '100%' }),
      svg('rect', { class: 'band band-high', x: '80%', y: 0, width: '20%', height: '100%' }),
      soc != null ? svg('rect', { class: ['bar-fill', { 'is-low': soc < 20 }], x: 0, y: '25%', width: `${Math.max(0, Math.min(100, soc))}%`, height: '50%' }) : null,
      usable != null && soc != null && usable < soc - 0.5 ? svg('rect', { class: 'bar-locked', x: `${usable}%`, y: '25%', width: `${soc - usable}%`, height: '50%' }) : null,
      limit != null ? svg('rect', { class: 'bar-limit', x: `${Math.min(99.6, limit)}%`, y: 0, width: '0.4%', height: '100%' }) : null),
    svg('svg', { class: 'gauge-scale', width: '100%', 'aria-hidden': 'true', focusable: 'false' },
      [0, 20, 50, 80, 100].map((t) => [
        svg('line', { class: 'gauge-tick', x1: `${t}%`, x2: `${t}%`, y1: 0, y2: '23%' }),
        limit == null || Math.abs(t - limit) > 7 ? svg('text', { class: 'gauge-num', x: `${t}%`, y: '77%', 'text-anchor': t === 0 ? 'start' : t === 100 ? 'end' : 'middle' }, `${t}`) : null]),
      limit != null ? svg('text', { class: 'gauge-limit', x: `${limit}%`, y: '77%', 'text-anchor': limit > 92 ? 'end' : 'middle' }, `limit ${limit}`) : null),
    h('p', { class: 'gauge-note' }, 'Everyday band 20 to 80 %', limit != null ? `; the car stops charging at ${format(limit, 'percent')}` : '', '.'));
  return h('section', { class: ['hero', { 'is-stale': !!now.stale }], 'aria-label': 'Battery and range' },
    h('div', { class: 'hero-figs' }, figs), gauge, flags(now));
}

/** The flags that are on, as chips under the battery: a quick glance (every flag is listed under Status flags). */
function flags(now) {
  const f = now.flags || {};
  const ch = now.charge || {};
  const out = [];
  if (v(ch.plugged)) out.push(chip(now.state === 'charging' ? 'Charging' : 'Plugged in', { kind: 'plug', iconName: 'plug' }));
  if (v(f.sentry)) out.push(chip('Sentry mode on', { kind: 'on' }));
  if (v(f.sentry_event_guess)) out.push(chip('Sentry may be recording', { kind: 'warn' }));
  const climate = v(f.climate);
  if (v(f.battery_preconditioning)) out.push(chip('Battery preconditioning', { kind: 'on' }));
  if (climate === 'precondition') out.push(chip('Cabin preconditioning', { kind: 'on' }));
  else if (climate === 'on') out.push(chip('Climate on'));
  else if (climate === 'overheat_protect') out.push(chip('Cabin overheat protection'));
  const keep = v(f.keep_climate);
  if (keep && keep !== 'off') out.push(chip(KEEP_WORDS[keep] || keep, { kind: 'on' }));
  if (v(f.user_present)) out.push(chip('Someone in the car'));
  if (v(f.service_mode)) out.push(chip('Service mode', { kind: 'warn' }));
  if (v(f.reduced_range)) out.push(chip('Reduced range: cold battery', { kind: 'warn' }));
  if (!out.length) return null;
  return h('div', { class: 'chips' }, now.stale ? h('span', { class: 'chips-label' }, 'Last known') : null, out);
}

/* ------------------------------------------------------------------ state, flags */

const KEEP_WORDS = { off: 'Off', on: 'Keep climate on', dog: 'Dog mode', camp: 'Camp mode' };
const CLIMATE_WORDS = { off: 'Off', on: 'On', precondition: 'Preconditioning', overheat_protect: 'Overheat protection' };
const STATE_WHAT = {
  driving: 'In gear and moving or ready to move.',
  charging: 'Energy is going into the battery.',
  parked: 'Awake but not driving or charging.',
  asleep: 'Both buses are quiet: the car went to sleep.',
  unknown: 'No recent data: not the same as asleep.',
};

function stateLine(now, tz, placeKind) {
  const st = now.state || 'unknown';
  const since = now.since_ms;
  const buses = now.buses || [];
  const busText = (b) => (b ? (b.awake ? 'awake' : 'asleep') : 'not heard');
  const vb = buses.find((b) => b.bus === 'vehicle'), cb = buses.find((b) => b.bus === 'chassis');
  const f = now.flags || {};
  const svc = v(f.service_mode);
  const place = now.location && now.location.place;
  const atService = !!place && placeKind === 'service';
  const wasDriving = now.drive && now.drive.drive_id;
  let busNote = null;
  if ((now.stale || st === 'unknown') && wasDriving) busNote = 'Nothing has arrived since the middle of a drive: the drive stays open, timed from its start, until data comes again.';
  else if (now.stale || st === 'unknown') busNote = 'No recent data, so the buses are not known now.';
  else if (vb && cb && vb.awake && !cb.awake) busNote = 'Parked but kept awake: the vehicle bus is awake while the chassis bus sleeps.';
  return h('section', { class: 'state-line', 'aria-label': 'State' },
    h('div', { class: 'state-cell', cover: 'summary.stat.status' },
      h('p', { class: 'fig-name' }, 'State'),
      h('p', { class: 'state-now' }, stateMark(st, { stale: !!now.stale })),
      h('p', { class: 'state-note' }, since ? [`since ${formatDateTime(since, tz, { now: api.now() })}`, sep(), formatDuration(api.now() - since)] : 'start not known'),
      h('p', { class: 'state-note' }, STATE_WHAT[st] || '')),
    h('div', { class: 'state-cell', cover: 'summary.state.drive_offline' },
      h('p', { class: 'fig-name' }, 'Buses'),
      h('p', { class: 'state-now' }, now.stale || st === 'unknown' ? 'Last known: ' : null, vb && cb && vb.awake === cb.awake ? `${now.stale || st === 'unknown' ? 'both' : 'Both'} ${busText(vb)}` : [`Vehicle ${busText(vb)}`, sep(), `chassis ${busText(cb)}`]),
      h('p', { class: ['state-note', { 'is-warn': !!busNote && (now.stale || st === 'unknown') }] }, busNote || 'Heard ' + (vb && vb.at_ms ? formatTime(vb.at_ms, tz, { seconds: true }) : 'never'))),
    h('div', { class: 'state-cell', cover: ['summary.state.in_service', 'summary.flag.service_mode'] },
      h('p', { class: 'fig-name' }, 'Service'),
      h('p', { class: ['state-now', { 'is-warn': svc || atService }] }, svc == null ? 'Service mode not sent' : svc ? 'Service mode on' : 'Service mode off'),
      h('p', { class: 'state-note' }, atService ? `Parked at ${place.name}, a service centre.` : 'Not at a place marked as a service centre.')));
}

function onOff(x, on = 'On', off = 'Off') {
  const val = v(x);
  return val == null ? h('span', { class: 'nil' }, 'not sent') : val ? on : off;
}

/** Every status flag the car sends, on or off, each with its own time. */
function statusFlags(now, tz) {
  const f = now.flags || {};
  const ch = now.charge || {};
  const t = (x) => (at(x) ? `Measured ${formatDateTime(at(x), tz, { now: api.now(), seconds: true })}` : null);
  const soc = v(now.soc_pct), usable = v(now.usable_soc_pct);
  const item = (name, value, x, { cover = null, warn = false, on = false, note = null } = {}) => h('li', { class: ['flag-item', { 'is-warn': warn, 'is-on': on }], cover, title: t(x) },
    h('span', { class: 'flag-name' }, name), h('span', { class: 'flag-value' }, value, note ? h('span', { class: 'spec-note' }, note) : null));
  const climate = v(f.climate);
  const keep = v(f.keep_climate);
  return h('section', { class: ['status-flags', { 'is-stale': !!now.stale }], 'aria-labelledby': 'flags-title' },
    h('h2', { class: 'label', id: 'flags-title' }, 'Status flags', h('span', { class: 'label-note' }, now.stale ? 'last known values' : 'hover a value for its time')),
    h('ul', { class: 'flag-list' },
      item('Sentry mode', onOff(f.sentry), f.sentry, { cover: 'summary.flag.sentry_mode', on: !!v(f.sentry) }),
      item('Sentry recording', v(f.sentry_event_guess) == null ? h('span', { class: 'nil' }, 'not sent') : v(f.sentry_event_guess) ? 'Probably' : 'No sign', f.sentry_event_guess,
        { cover: 'summary.flag.sentry_recording', warn: !!v(f.sentry_event_guess), note: 'a guess: camera on with the screen off' }),
      item('Keep climate', keep == null ? h('span', { class: 'nil' }, 'not sent') : KEEP_WORDS[keep] || keep, f.keep_climate, { cover: 'summary.flag.dog_mode', on: keep && keep !== 'off', note: 'keep, dog or camp mode' }),
      h('li', { class: 'flag-pair', cover: 'summary.flag.preconditioning' },
        h('ul', null,
          item('Cabin climate', climate == null ? h('span', { class: 'nil' }, 'not sent') : CLIMATE_WORDS[climate] || climate, f.climate, { on: climate === 'precondition' }),
          item('Battery preconditioning', onOff(f.battery_preconditioning), f.battery_preconditioning, { on: !!v(f.battery_preconditioning) }))),
      item('Someone in the car', onOff(f.user_present, 'Yes', 'No'), f.user_present, { cover: 'summary.flag.driver_present' }),
      item('Plugged in', onOff(ch.plugged, 'Yes', 'No'), ch.plugged, { cover: 'summary.flag.plugged_in', on: !!v(ch.plugged), note: v(f.charge_port_open) == null ? null : v(f.charge_port_open) ? 'charge port open' : 'charge port closed' }),
      item('Reduced range', onOff(f.reduced_range, 'Yes: cold battery', 'No'), f.reduced_range, {
        cover: 'summary.flag.reduced_range', warn: !!v(f.reduced_range),
        note: usable != null && soc != null ? `${format(usable, 'percent')} of ${format(soc, 'percent')} usable now` : null }),
      item('Battery kept warm', onOff(f.keep_warm), f.keep_warm),
      item('Parking brake', onOff(f.parking_brake), f.parking_brake),
      item('Accessory power', onOff(f.accessory_power), f.accessory_power),
      item('Centre display', onOff(f.display_on), f.display_on)));
}

/** The usual local start time of AC charges at a place, from past charges there (circular mean). */
function usualFrom(items, tz, place) {
  const fmt = new Intl.DateTimeFormat('en-GB', { timeZone: tz, hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });
  const mins = (items || []).filter((c) => c.start_ms).map((c) => { const [hh, mm] = fmt.format(c.start_ms).split(':').map(Number); return hh * 60 + mm; });
  if (mins.length < 3) return { none: true, count: mins.length, place };
  const a = mins.map((m) => (m / 1440) * 2 * Math.PI);
  const mean = Math.atan2(a.reduce((x, y) => x + Math.sin(y), 0) / a.length, a.reduce((x, y) => x + Math.cos(y), 0) / a.length);
  const minutes = Math.round((((mean / (2 * Math.PI)) * 1440 + 1440) % 1440) / 5) * 5;
  const [nh, nm] = fmt.format(api.now()).split(':').map(Number);
  let delta = minutes - (nh * 60 + nm);
  if (delta < 0) delta += 1440;
  return { at_ms: Math.floor((api.now() + delta * 60000) / 60000) * 60000, count: mins.length, place };
}

/* ------------------------------------------------------------------ temperatures, tyres, the car */

function row(name, value, note = null, { cover = null, measured = null } = {}) {
  return h('div', { class: 'spec-row', cover, title: measured ? `Measured ${measured}` : null },
    h('dt', null, name), h('dd', null, value, note ? h('span', { class: 'spec-note' }, note) : null));
}

function vitals(car, now, tz) {
  const t = (x) => (at(x) ? formatDateTime(at(x), tz, { now: api.now(), seconds: true }) : null);
  const notSent = /legacy/.test(car.profile || '') ? 'not sent by this car' : 'not sent yet';
  const sp = (x) => { const val = v(x); return typeof val === 'string' ? val : q(val, 'temperature'); };
  const bothNil = v(now.setpoint_left_c) == null && v(now.setpoint_right_c) == null;
  const temps = h('section', { class: 'vital' }, h('h2', { class: 'label' }, 'Temperature'),
    h('dl', { class: 'spec' },
      row('Outside', q(v(now.outside_c), 'temperature'), null, { cover: ['summary.stat.outside_temp', 'overview.stat.outside_temp'], measured: t(now.outside_c) }),
      row('Cabin', q(v(now.cabin_c), 'temperature', { why: notSent }), null, { cover: ['summary.stat.inside_temp', 'overview.stat.inside_temp'], measured: t(now.cabin_c) }),
      row('Battery', q(v(now.battery_c), 'temperature'), null, { measured: t(now.battery_c) }),
      row('Set points', bothNil ? q(null, 'temperature', { why: notSent }) : h('span', null, sp(now.setpoint_left_c), ' / ', sp(now.setpoint_right_c)), 'driver / passenger', { cover: 'overview.stat.driver_temp', measured: t(now.setpoint_left_c) })));

  const kpa = (now.tyres_kpa || []).map(v);
  const rec = v(now.tyre_recommended_kpa);
  const low = (p) => p != null && rec != null && p < rec * 0.9;
  const pu = getUnits().pressure === 'psi' ? 'psi' : 'bar';
  const val = (p) => (p == null ? '—' : parts(p, 'pressure').number);
  const wheel = (x, y) => svg('rect', { class: 'tire-wheel', x, y, width: 9, height: 24, rx: 3 });
  const reading = (p, x, y, anchor) => svg('text', { class: ['tire-value', { 'is-low': low(p) }], x, y, 'text-anchor': anchor }, val(p));
  const names = ['Front left', 'Front right', 'Rear left', 'Rear right'];
  const lows = kpa.map((p, i) => (low(p) ? `${names[i]} ${format(p, 'pressure')}` : null)).filter(Boolean);
  const tyres = h('section', { class: 'vital vital-tires', cover: 'summary.flag.tire_pressure_warning' }, h('h2', { class: 'label' }, 'Tyres', h('span', { class: 'label-note' }, pu)),
    kpa.length ? h('div', { class: 'tires-figure' },
      h('span', { class: 'tire-axle', 'aria-hidden': 'true' }, 'front'),
      svg('svg', { class: 'tires', viewBox: '0 15 220 124', role: 'img', 'aria-label': `Tyre pressures in ${pu}: ${names.map((n, i) => `${n.toLowerCase()} ${val(kpa[i])}`).join(', ')}.` },
        svg('rect', { class: 'tire-body', x: 84, y: 19, width: 52, height: 116, rx: 22 }),
        svg('path', { class: 'tire-glass', d: 'M92 53c8-6 28-6 36 0M92 105c8 5 28 5 36 0' }),
        wheel(76, 33), wheel(135, 33), wheel(76, 99), wheel(135, 99),
        reading(kpa[0], 64, 52, 'end'), reading(kpa[1], 156, 52, 'start'), reading(kpa[2], 64, 118, 'end'), reading(kpa[3], 156, 118, 'start')),
      h('span', { class: 'tire-axle', 'aria-hidden': 'true' }, 'rear')) : h('p', { class: 'note' }, 'This car sends no tyre pressures.'),
    h('p', { class: ['spec-foot', { 'is-low': lows.length }] }, lows.length ? `Low: ${formatList(lows)}` : kpa.length ? 'All four at or near the recommended pressure' : '',
      rec != null ? ` · ${format(rec, 'pressure')} recommended by the car` : ''));

  const sw = now.software || {};
  const swSrc = { teslamate: 'imported', owner: 'typed by the owner', build_match: 'matched by build' }[sw.version_source] || null;
  const ideal = v(now.ideal_range_km);
  const carSpec = h('section', { class: 'vital' }, h('h2', { class: 'label' }, 'The car'),
    h('dl', { class: 'spec' },
      row('Speed', q(v(now.speed_kph), 'speed'), v(now.gear) ? `gear ${v(now.gear)}` : null, { cover: 'summary.stat.speed', measured: t(now.speed_kph) }),
      row('12 V battery', q(v(now.lv_v), 'voltage', { digits: 1 }), null, { measured: t(now.lv_v) }),
      row(h('a', { href: state.linkTo('statistics'), cover: 'overview.link.mileage' }, 'Odometer'), q(v(now.odometer_km), 'odometer'), null, { cover: ['summary.stat.odometer', 'overview.stat.odometer'], measured: t(now.odometer_km) }),
      row('Ideal range', q(ideal, 'range'), null, { cover: 'nr.dash.ideal-range', measured: t(now.ideal_range_km) }),
      row('Estimated range', q(v(now.est_range_km), 'range'), 'Sentry Mate’s: energy left over the last 30 days’ use', { cover: ['summary.stat.range_estimated', 'nr.dash.est-range'] }),
      row(h('a', { href: state.linkTo('software'), cover: 'overview.link.updates' }, 'Software'),
        sw.version ? h('span', { class: 'mono' }, sw.version) : h('span', { class: 'nil' }, 'unknown'),
        [swSrc, sw.version_since_ms ? `since ${formatDate(sw.version_since_ms, tz, { now: api.now() })}` : null, sw.changed_since_ms ? `build changed ${formatDate(sw.changed_since_ms, tz, { now: api.now() })}` : null].filter(Boolean).join(' · '),
        { cover: ['summary.stat.version', 'overview.stat.firmware', 'summary.flag.software_update'] })));
  return h('div', { class: ['vitals', { 'is-stale': !!now.stale }], cover: 'nr.dash.panel.car-status' }, temps, tyres, carSpec);
}

/* ------------------------------------------------------------------ why awake, doors */

function awake(now) {
  const f = now.flags || {};
  const reasons = (now.awake_reasons || []).map((r) => AWAKE_WORDS[r] || r.replace(/_/g, ' '));
  let text;
  if (now.state === 'asleep') text = 'Asleep: both buses are quiet.';
  else if (now.state === 'unknown' || now.stale) text = 'Not known: no recent data.';
  else if (now.state === 'driving') text = 'Driving.';
  else text = reasons.length ? `Awake because of ${formatList(reasons)}.` : 'Awake, with no reason the car reports.';
  const doors = DOORS.map(([k, name]) => [name, v(f.doors && f.doors[k])]).concat([['Trunk', v(f.trunk)], ['Frunk', v(f.frunk)]]);
  const lift = v(f.liftgate_deg);
  const open = doors.filter(([, o]) => o === true).map(([n]) => n);
  return h('section', { class: 'awake-doors' },
    h('div', { class: 'awake', cover: 'web.settings.car.sleep-blockers' },
      h('h2', { class: 'label' }, 'Why the car is awake'),
      h('p', { class: 'awake-text' }, text),
      h('p', { class: 'note' }, 'Sentry Mate only listens: it never wakes the car or keeps it awake.')),
    h('div', { class: 'doors', cover: 'summary.flag.doors_open' },
      h('h2', { class: 'label' }, 'Doors and openings'),
      h('ul', { class: 'door-list' }, doors.map(([n, o]) => h('li', { class: ['door', { 'is-open': o === true }] }, h('span', null, n), h('span', { class: 'door-state' }, o == null ? 'not sent' : o ? 'Open' : 'Closed'))),
        lift != null ? h('li', { class: ['door', { 'is-open': lift > 2 }] }, h('span', null, 'Liftgate'), h('span', { class: 'door-state' }, lift > 2 ? `Open, ${Math.round(lift)}°` : 'Closed')) : null),
      h('p', { class: ['note', { 'is-warn': open.length && now.state === 'asleep' }] }, open.length ? `Open: ${formatList(open)}.` : 'All closed.'),
      h('p', { class: 'note', cover: ['summary.flag.lock', 'summary.flag.windows_open'] }, 'Locks and windows: the car does not send these on its CAN bus, so they are left out.')));
}

/* ------------------------------------------------------------------ charging now */

function charging(now, tz, usual) {
  const ch = now.charge || {};
  const plugged = !!v(ch.plugged);
  const on = now.state === 'charging' && !now.stale;
  const finish = ch.finish_ms;
  const toMin = v(ch.to_full_min);
  const off = (x, kind, why) => (x == null ? h('span', { class: 'nil' }, why) : q(x, kind));
  const idle = on ? 'not sent' : '—';
  const kind = v(ch.kind);
  let sched;
  if (on) sched = h('span', { class: 'nil' }, 'charging now');
  else if (!plugged) sched = h('span', { class: 'nil' }, 'not plugged in');
  else if (usual && usual.at_ms) sched = h('span', null, `about ${formatTime(usual.at_ms, tz)}`, h('span', { class: 'spec-note' }, `usual start of ${usual.count} AC charges at ${usual.place.name}`));
  else if (usual && usual.loading) sched = h('span', { class: 'nil' }, 'working it out');
  else sched = h('span', { class: 'nil' }, 'no pattern here yet');
  const rows = [
    ['Status', CHARGE_WORDS[v(ch.status)] || (plugged ? 'Plugged in' : 'Not plugged in'), null, ch.status],
    ['Kind', kind ? (kind === 'dc' ? 'DC (fast charger)' : 'AC') : h('span', { class: 'nil' }, idle), null, ch.kind],
    ['Power', off(v(ch.power_kw), 'power', idle), ['summary.stat.charger_power', 'nr.dash.charging-power', 'overview.stat.charging_power'], ch.power_kw],
    [kind === 'dc' ? 'Charger volts' : 'Line volts', off(v(ch.volts_v), 'voltage', idle), 'overview.stat.ac_voltage', ch.volts_v],
    [kind === 'dc' ? 'Charger amps' : 'Line amps', off(v(ch.amps_a), 'current', idle), 'nr.dash.charging-current', ch.amps_a],
    ['Phases', v(ch.phases) != null ? String(v(ch.phases)) : h('span', { class: 'nil' }, kind === 'dc' ? 'DC: none' : idle), null, ch.phases],
    ['Pilot current', v(ch.pilot_a) != null ? h('span', null, format(v(ch.pilot_a), 'current'), v(ch.limit_source) ? h('span', { class: 'spec-note' }, `limit set by ${LIMIT_WORDS[v(ch.limit_source)] || v(ch.limit_source)}`) : null) : h('span', { class: 'nil' }, idle), null, ch.pilot_a],
    ['Added', v(ch.added_kwh) != null ? plus(q(v(ch.added_kwh), 'energy')) : h('span', { class: 'nil' }, idle), 'summary.stat.energy_added', ch.added_kwh],
    ['Charge limit', q(v(ch.limit_pct), 'percent', { why: 'not sent yet' }), 'summary.stat.charge_limit', ch.limit_pct],
    ['Still to add', off(v(ch.to_full_kwh), 'energy', idle), null, ch.to_full_kwh],
    ['Time to full', toMin != null ? formatDuration(toMin * 60000) : h('span', { class: 'nil' }, idle), ['summary.stat.remaining_time', 'nr.dash.time-to-full'], ch.to_full_min],
    ['Done at', finish ? formatDateTime(finish, tz, { now: api.now() }) : h('span', { class: 'nil' }, idle), 'summary.stat.expected_finish', null],
    ['Scheduled start', sched, 'summary.stat.scheduled_charging', null],
  ];
  const measured = (x) => (at(x) ? `Measured ${formatDateTime(at(x), tz, { now: api.now(), seconds: true })}` : null);
  return h('section', { class: ['charging-now', { 'is-idle': !plugged && !on, 'is-stale': !!now.stale }], 'aria-labelledby': 'charging-title', cover: 'nr.dash.panel.charge-status' },
    h('h2', { class: 'label', id: 'charging-title' }, on ? 'Charging now' : 'Charging',
      ch.charge_id ? h('a', { class: 'label-note', href: state.buildUrl('charge', { car: state.current().car, charge: ch.charge_id }) }, 'Open this charge') : null),
    h('dl', { class: 'charge-grid' }, rows.map(([n, val, cover, x]) => h('div', { class: 'charge-cell', cover, title: measured(x) }, h('dt', { class: 'fig-name' }, n), h('dd', { class: 'charge-value' }, val)))),
    h('p', { class: 'note' }, on
      ? 'Time to full and the finish time come from the car’s own estimate; the finish time is in the car’s time zone.'
      : plugged ? 'Plugged in and not charging. The car does not send its charging schedule, so the start shown is the usual one at this place.'
        : 'Power, volts, amps and the time to full fill in while the car charges.'));
}

/* ------------------------------------------------------------------ where */

function whereLines(now, tz, outsideMaps, visit) {
  const L = now.location || {};
  const lat = v(L.lat), lon = v(L.lon);
  const acc = v(L.accuracy_m), hd = v(L.heading_deg);
  const title = now.stale ? 'Last known place' : now.state === 'driving' ? 'On the road' : 'Where';
  const a = L.address || {};
  return [
    h('h2', { class: 'label', id: 'where-title' }, title),
    h('p', { class: 'where-place' }, L.place ? L.place.name : a.short || '—'),
    h('p', { class: 'where-area' }, [L.place ? a.short : null, a.locality, a.region].filter((x, i, arr) => x && arr.indexOf(x) === i).join(', ')),
    sinceLine(now, tz, visit),
    hd != null ? h('p', { class: 'where-since' }, `Heading ${compass(hd)} (${Math.round(hd)}°)`) : null,
    lat != null ? h('p', { class: 'where-coords mono' }, coordsText([lon, lat]), acc != null ? ` ± ${format(acc, 'length')}` : '') : null,
    h('p', { class: 'where-ext', cover: 'summary.map.external_link' }, outsideMaps && lat != null
      ? h('a', { href: `https://www.openstreetmap.org/?mlat=${lat.toFixed(5)}&mlon=${lon.toFixed(5)}#map=16/${lat.toFixed(5)}/${lon.toFixed(5)}`, target: '_blank', rel: 'external noopener noreferrer' }, 'Open on OpenStreetMap')
      : h('span', { class: 'note' }, 'Outside map links are off. An admin can turn them on in Settings: Display and units.')),
  ];
}

/** 'Here since' the arrival of the open stop; with no recent data, only when the car was last seen here. */
function sinceLine(now, tz, visit) {
  if (now.state === 'driving' && !now.stale) return null;
  const opts = { now: api.now() };
  if (now.stale || now.state === 'unknown') {
    const last = now.at_ms;
    return last ? h('p', { class: 'where-since' }, `Last seen here ${formatDateTime(last, tz, opts)}`, sep(), `${formatDuration(api.now() - last)} ago`) : null;
  }
  if (!visit) return null;
  return h('p', { class: 'where-since' }, `Here since ${formatDateTime(visit.arrived_ms, tz, opts)}`, sep(), formatDuration(api.now() - visit.arrived_ms));
}

/* ------------------------------------------------------------------ latest drive and charge */

function driveCard(car, d, track, tz) {
  if (!d) return h('article', { class: 'entry entry-empty' }, h('p', { class: 'eyebrow' }, 'Latest drive'), h('p', { class: 'note-serif' }, 'No drives yet.'));
  const going = d.end_ms == null;
  const coords = track ? track.lon.map((x, i) => [x, track.lat[i]]) : [[d.start.lon, d.start.lat], [d.end.lon, d.end.lat]];
  return h('article', { class: 'entry' },
    h('a', { class: 'entry-link', href: state.buildUrl('drive', { car: car.id, drive: d.id, period: state.current().period }) },
      h('p', { class: 'eyebrow' }, going ? 'This drive' : 'Latest drive', going ? h('span', { class: 'eyebrow-note' }, ' · in progress') : null),
      h('div', { class: 'entry-head' },
        h('div', { class: 'entry-head-text' },
          h('h3', { class: 'entry-title' }, h('em', null, endLabel(d.start)), going ? null : [h('span', { class: 'to' }, ' to '), h('em', null, endLabel(d.end))]),
          h('p', { class: 'entry-when' }, `${formatDate(d.start_ms, tz, { now: api.now() })}, ${formatTime(d.start_ms, tz)} – ${going ? 'now' : formatTime(d.end_ms, tz)}`)),
        routeSketch(coords, { cls: 'sketch entry-sketch' })),
      h('dl', { class: 'entry-figs' },
        figure('Distance', q(d.distance_km, 'distance')), figure('Time', formatDuration(d.duration_ms)),
        figure('Consumption', q(d.wh_per_km, 'efficiency')), figure('Energy', q(d.energy_kwh, 'energy'), d.energy_basis === 'estimated' ? 'estimated' : 'measured')),
      h('span', { class: 'entry-more' }, 'Open the drive', icon('arrowRight'))));
}

function chargeCard(car, c, now, tz) {
  if (!c) return h('article', { class: 'entry entry-empty' }, h('p', { class: 'eyebrow' }, 'Latest charge'), h('p', { class: 'note-serif' }, 'No charges yet.'));
  const going = c.end_ms == null;
  return h('article', { class: 'entry' },
    h('a', { class: 'entry-link', href: state.buildUrl('charge', { car: car.id, charge: c.id, period: state.current().period }) },
      h('p', { class: 'eyebrow' }, going ? 'This charge' : 'Latest charge', going ? h('span', { class: 'eyebrow-note' }, ' · in progress') : null),
      h('div', { class: 'entry-head' },
        h('div', { class: 'entry-head-text' },
          h('h3', { class: 'entry-title' }, 'At ', h('em', null, c.place ? c.place.name : (c.address && c.address.short) || 'an unnamed spot')),
          h('p', { class: 'entry-when' }, `${formatDate(c.start_ms, tz, { now: api.now() })}, ${formatTime(c.start_ms, tz)} – ${going ? 'now' : formatTime(c.end_ms, tz)}`))),
      h('dl', { class: 'entry-figs' },
        figure('Added', plus(q(c.energy_added_kwh, 'energy'))), figure('Battery', `${format(c.start_soc_pct, 'number', { digits: 0 })}–${format(c.end_soc_pct, 'percent')}`),
        figure('Power', q(c.power_avg_kw, 'power'), c.kind ? `${c.kind.toUpperCase()}, average` : null),
        figure('Cost', c.cost ? format(c.cost.amount, 'money', { currency: c.cost.currency }) : '—', c.cost ? null : 'no price set')),
      h('span', { class: 'entry-more' }, 'Open the charge', icon('arrowRight'))));
}

/* ------------------------------------------------------------------ the period */

async function drawPeriod(ctx, aside, box) {
  const car = ctx.car, tz = ctx.tz, p = ctx.params;
  const label = periodLabel(ctx.period, tz, api.now());
  const [tl, act, series, trip] = await Promise.allSettled([
    ctx.api.page(api.apiPath`cars/${car.id}/timeline`, p, { limit: 40 }),
    ctx.api.get(api.apiPath`cars/${car.id}/activity`, p),
    ctx.api.get(api.apiPath`cars/${car.id}/series`, { ...p, fields: 'soc_pct,usable_soc_pct,range_km,ideal_range_km', max_points: 600 }),
    ctx.api.get(api.apiPath`cars/${car.id}/trip`, p),
  ]);
  if (ctx.signal.aborted) return;

  // The aside: the period's figures and its entries, newest first.
  const t = trip.status === 'fulfilled' ? trip.value : null;
  const rows = tl.status === 'fulfilled' ? tl.value.items : [];
  const noDrives = t && !(t.drives || []).length ? 'no drives in this period' : 'not enough data';
  replace(aside, 
    h('div', { class: 'today-head' }, h('p', { class: 'eyebrow' }, 'The period'), h('h2', { class: 'today-date' }, label)),
    t ? h('dl', { class: 'today-figs', cover: ['overview.stat.distance_logged', 'overview.stat.consumption_net', 'overview.stat.consumption_gross'] },
      figure('Distance', q(t.distance.drives_km, 'distance'), 'logged drives'),
      figure('Drives', String((t.drives || []).length)),
      figure('Net consumption', q(t.wh_per_km.net, 'efficiency', { why: noDrives }), 'driving only'),
      figure('Gross consumption', q(t.wh_per_km.gross, 'efficiency', { why: noDrives }), 'parked use included')) : errorView(trip.reason, { eyebrow: 'Figures' }),
    rows.length ? h('ol', { class: 'tl' }, rows.map((r) => timelineRow(r, { car: car.id, tz, period: ctx.state.period }))) : empty({ title: 'Nothing recorded in this period.' }),
    h('a', { class: 'link-arrow', href: state.linkTo('timeline') }, rows.length >= 40 ? 'More on the timeline' : 'The period on the timeline', icon('arrowRight')));

  // Below: the activity strip, the battery chart and the charging chart, each chart on one time axis.
  const strip = act.status === 'fulfilled'
    ? activityStrip(act.value, { tz, link: (s) => (s.ref && s.ref.drive ? state.buildUrl('drive', { car: car.id, drive: s.ref.drive }) : s.ref && s.ref.charge ? state.buildUrl('charge', { car: car.id, charge: s.ref.charge }) : null) })
    : errorView(act.reason, { eyebrow: 'Activity' });
  const rf = rangeField('range_km');
  const battery = chartBlock(ctx, {
    title: 'Battery', note: 'asleep time shaded violet, unknown time shaded red; a line never crosses unknown time',
    link: h('a', { class: 'link-arrow', href: state.linkTo('battery'), cover: 'overview.link.charge_level' }, 'Battery history', icon('arrowRight')),
    cover: ['overview.chart.charge_level', 'overview.control.shared_crosshair'], csvName: [car.name, 'battery', label],
  });
  const chargingBox = chartBlock(ctx, {
    title: 'Charging', note: 'each charge in the period; the lines break between charges',
    link: h('a', { class: 'link-arrow', href: state.linkTo('charges'), cover: 'overview.link.charging_stats' }, 'Charges and their statistics', icon('arrowRight')),
    cover: ['overview.chart.charging_details.power', 'overview.chart.charging_details.energy', 'overview.chart.charging_details.ac_current', 'overview.chart.charging_details.ac_voltage', 'overview.chart.charging_details.battery_heater'],
    csvName: [car.name, 'charging', label],
  });
  replace(box,
    h('section', { class: 'block', cover: ['overview.chart.states', 'overview.control.time_range', 'overview.control.refresh', 'overview.filter.car'] },
      h('div', { class: 'block-head' }, h('h2', { class: 'label' }, 'Activity', h('span', { class: 'label-note' }, label)),
        h('a', { class: 'link-arrow', href: state.linkTo('timeline'), cover: 'overview.link.states' }, 'Timeline', icon('arrowRight'))),
      strip),
    battery.el, chargingBox.el,
    h('p', { class: 'note period-note' }, 'This part follows the period picker at the top and loads again by itself when a drive, charge or parked spell starts or ends; nothing is re-asked on a timer.'));

  const draws = [];
  if (series.status === 'fulfilled' && series.value.t_ms.length) {
    const s = series.value;
    const states = s.state;
    draws.push(battery.draw({
      t_ms: s.t_ms, states, spans: spansFromStates(s.t_ms, states, s.bucket_ms),
      rows: [
        { key: 'soc', name: 'Battery', kind: 'percent', values: s.values.soc_pct, colour: '--accent', fill: '--chart-wash', floor: 0, ticks: 4 },
        { key: 'usable', name: 'Usable', kind: 'percent', values: s.values.usable_soc_pct, colour: '--dusk', floor: 0, ticks: 4, note: 'the part a cold battery can give', hidden: true },
        { key: 'range', name: rangeLabel(), kind: 'range', values: s.values[rf] || s.values.range_km, colour: '--ink-2', floor: 0, ticks: 4 },
      ],
      extraCsv: [{ label: 'State', value: (i) => states[i] }],
    }));
  } else {
    battery.fill(series.status === 'rejected' ? errorView(series.reason, { eyebrow: 'Battery' }) : empty({ title: 'No battery readings in this period.', text: 'Choose a longer period, or look at the Timeline for when data last arrived.' }));
  }
  draws.push(drawCharging(ctx, chargingBox).catch((err) => chargingBox.fill(errorView(err, { eyebrow: 'Charging' }))));
  await Promise.all(draws);
}

/* ------------------------------------------------------------------ charts with a legend, a readout and CSV */

/**
 * A chart block: a head with its title, a link and a Download CSV button, a legend whose entries show or
 * hide each chart (with the lowest and highest value in the period), a readout of the values under the
 * hover line, and the charts on one time axis.
 */
function chartBlock(ctx, { title, note, link, cover, csvName }) {
  const tz = ctx.tz;
  const legend = h('div', { class: 'legend', role: 'group', 'aria-label': `${title}: show or hide a chart`, cover: 'overview.chart.legend_toggle' });
  const readout = h('p', { class: 'readout chart-readout', 'aria-live': 'off' });
  const charts = h('div', { class: 'charts' });
  const zoomBtn = h('button', { type: 'button', class: 'button button-quiet button-small', hidden: true, onclick: () => {
    if (!handle || !data || !data.fit) return;
    const zoomed = zoomBtn.dataset.zoomed === '1';
    if (zoomed) handle.resetZoom();
    else handle.plots[0]?.setScale('x', { min: data.fit[0] / 1000, max: data.fit[1] / 1000 });
    zoomBtn.dataset.zoomed = zoomed ? '0' : '1';
    zoomBtn.textContent = zoomed ? data.fitLabel : 'Show the whole period';
  } });
  let data = null;
  let handle = null;
  let hidden = new Set();
  const csv = h('button', { type: 'button', class: 'button button-quiet button-small', disabled: true, onclick: () => data && downloadCsv(csvFilename(csvName), data.t_ms.map((t, i) => ({ t_ms: t, i })), [
    timeColumn('Time', 't_ms', tz),
    ...data.rows.map((r) => column(r.name, (row) => r.values[row.i], r.kind, { digits: 3 })),
    ...(data.extraCsv || []).map((c) => ({ label: c.label, value: (row) => c.value(row.i) })),
  ]) }, icon('download'), 'Download CSV');
  const el = h('section', { class: 'block chart-block', cover },
    h('div', { class: 'block-head' }, h('h2', { class: 'label' }, title, note ? h('span', { class: 'label-note' }, note) : null), h('div', { class: 'block-actions' }, link, csv)),
    h('div', { class: 'chart-bar' }, legend, zoomBtn), readout, charts);

  function idle() {
    if (!data) return;
    const t = data.t_ms;
    replace(readout, h('span', { class: 'readout-idle' }, data.idle || `${t.length} points, ${formatDateTime(t[0], tz, { now: api.now() })} to ${formatDateTime(t[t.length - 1], tz, { now: api.now() })}. Hover for the values; drag to zoom, double-click to zoom out.`));
  }
  function hover(i) {
    if (i == null || !data) { idle(); return; }
    const shown = data.rows.filter((r) => !hidden.has(r.key));
    replace(readout, h('time', { class: 'readout-time' }, formatDateTime(data.t_ms[i], tz, { now: api.now(), seconds: true })),
      shown.map((r) => h('span', null, `${r.name} `, h('b', null, r.values[i] == null ? '—' : format(r.values[i], r.kind)))));
  }
  async function render() {
    if (handle) handle.destroy();
    replace(charts);
    const rows = data.rows.filter((r) => !hidden.has(r.key));
    handle = await stackedCharts(ctx, charts, { tz, t_ms: data.t_ms, states: data.states, spans: data.spans || [], rows, onHover: hover });
    zoomBtn.dataset.zoomed = '0';
    if (data.fit) zoomBtn.textContent = data.fitLabel;
  }
  function drawLegend() {
    replace(legend, data.rows.map((r) => {
      const nums = r.values.filter((x) => x != null && Number.isFinite(x));
      const lo = nums.length ? Math.min(...nums) : null, hi = nums.length ? Math.max(...nums) : null;
      const on = !hidden.has(r.key);
      return h('button', {
        type: 'button', class: ['legend-item', { 'is-off': !on }], 'aria-pressed': String(on), vars: { '--swatch': `var(${r.colour || '--ink'})` },
        title: on ? `Hide the ${r.name.toLowerCase()} chart` : `Show the ${r.name.toLowerCase()} chart`,
        onclick: () => {
          if (on && data.rows.length - hidden.size <= 1) return;
          if (on) hidden.add(r.key); else hidden.delete(r.key);
          drawLegend();
          render().catch(() => {});
        },
      }, h('span', { class: 'legend-swatch', 'aria-hidden': 'true' }), h('span', { class: 'legend-name' }, r.name),
      h('span', { class: 'legend-range' }, lo == null ? 'no values' : lo === hi ? format(lo, r.kind) : `${format(lo, r.kind)} to ${format(hi, r.kind)}`));
    }));
  }
  return {
    el,
    fill(node) { replace(legend); replace(readout); replace(charts, node); },
    async draw(d) {
      data = d;
      hidden = new Set(d.rows.filter((r) => r.hidden).map((r) => r.key));
      csv.disabled = false;
      zoomBtn.hidden = !d.fit;
      zoomBtn.dataset.zoomed = '0';
      if (d.fit) zoomBtn.textContent = d.fitLabel;
      drawLegend();
      idle();
      await render();
    },
  };
}

/** The period's charges, their curves joined on one time axis, a gap between charges. */
async function drawCharging(ctx, block) {
  const car = ctx.car;
  const res = await ctx.api.page(api.apiPath`cars/${car.id}/charges`, ctx.params, { limit: 12 });
  const list = res.items.slice().sort((a, b) => a.start_ms - b.start_ms);
  if (!list.length) {
    block.fill(empty({ title: 'No charge in this period.', text: 'Choose a longer period at the top to see charging power, energy, volts, amps and battery heating.' }));
    return;
  }
  const per = Math.max(60, Math.floor(1800 / list.length));
  const curves = await Promise.all(list.map((c) => ctx.api.get(api.apiPath`cars/${car.id}/charges/${c.id}/curve`,
    { fields: 'power_kw,heater_kw,ac_volts_v,ac_amps_a,dc_volts_v,dc_amps_a', max_points: per }).catch(() => null)));
  if (ctx.signal.aborted) return;
  const t = [], power = [], energy = [], volts = [], amps = [], heat = [], which = [];
  const gap = (ms) => { t.push(ms); power.push(null); energy.push(null); volts.push(null); amps.push(null); heat.push(null); which.push(null); };
  const from = ctx.params.from_ms, to = Math.min(ctx.params.to_ms ?? api.now(), api.now());
  if (from != null && list[0].start_ms > from) gap(from);
  list.forEach((c, n) => {
    const cv = curves[n];
    if (!cv || !cv.t_ms.length) return;
    if (t.length && t[t.length - 1] != null && power[power.length - 1] != null) gap(cv.t_ms[0] - 1);
    const V = cv.values;
    const dc = (cv.kind || c.kind) === 'dc';
    let e = 0;
    cv.t_ms.forEach((ms, i) => {
      const p = V.power_kw[i];
      if (i > 0 && p != null && V.power_kw[i - 1] != null) e += ((p + V.power_kw[i - 1]) / 2) * ((ms - cv.t_ms[i - 1]) / 3600000);
      t.push(ms); power.push(p); energy.push(Math.round(e * 1000) / 1000);
      // the supply side, as the notes say: the AC line at the inlet (one phase), or the DC charger's output
      volts.push((dc ? V.dc_volts_v?.[i] : V.ac_volts_v?.[i]) ?? null);
      amps.push((dc ? V.dc_amps_a?.[i] : V.ac_amps_a?.[i]) ?? null);
      heat.push(V.heater_kw ? V.heater_kw[i] : null);
      which.push(c.id);
    });
  });
  if (!t.length) { block.fill(empty({ title: 'The charges in this period have no recorded curve.' })); return; }
  if (to > t[t.length - 1] + 60000) gap(to);
  const kinds = [...new Set(list.map((c) => c.kind).filter(Boolean))];
  const more = res.next ? ` Only the latest ${list.length} charges are drawn; the Charges page lists every one.` : '';
  await block.draw({
    t_ms: t, states: null, spans: [],
    fit: [Math.min(...list.map((c) => c.start_ms)) - 300000, Math.min(Math.max(...list.map((c) => c.end_ms ?? api.now())), api.now()) + 300000],
    fitLabel: list.length === 1 ? 'Zoom to the charge' : 'Zoom to the charges',
    idle: `${list.length} ${list.length === 1 ? 'charge' : 'charges'} (${kinds.map((k) => k.toUpperCase()).join(' and ') || 'kind not known'}). Hover for the values; drag to zoom, double-click to zoom out.${more}`,
    rows: [
      { key: 'power', name: 'Power', kind: 'power', values: power, colour: '--ochre', fill: '--chart-ochre-wash', floor: 0, ticks: 4, note: 'into the battery' },
      { key: 'energy', name: 'Energy added', kind: 'energy', values: energy, colour: '--ink-2', floor: 0, ticks: 3, note: 'so far in each charge, from the power' },
      { key: 'volts', name: 'Volts', kind: 'voltage', values: volts, colour: '--slate', floor: 0, ticks: 3, note: kinds.includes('dc') ? (kinds.includes('ac') ? 'AC line; DC charger for DC' : 'DC charger output') : 'AC line, one phase' },
      { key: 'amps', name: 'Amps', kind: 'current', values: amps, colour: '--accent', floor: 0, ticks: 3, note: kinds.includes('dc') ? (kinds.includes('ac') ? 'AC line; DC charger for DC' : 'DC charger output') : 'AC line, per phase' },
      { key: 'heat', name: 'Battery heating', kind: 'power', values: heat, colour: '--rust', floor: 0, ticks: 2, note: 'zero when the heater is off' },
    ],
    extraCsv: [{ label: 'Charge id', value: (i) => which[i] }],
  });
}
