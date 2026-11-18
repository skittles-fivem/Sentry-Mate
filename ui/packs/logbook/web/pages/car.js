// Sentry Mate, "Logbook" UI pack: Car overview (/cars/{car}).
//
// What the car is doing now, why it is awake, and the chosen period (default 24 hours) at a glance.
// Data: GET /api/v1/cars/{car} (the car, with its configuration and sources), /cars/{car}/now (every value
// with its own time; updated live by `now` events), /settings (whether outside map links are on), and for
// the period: /cars/{car}/timeline, /activity, /series (battery % and range) and /trip (distance and
// consumption). Latest drive and charge: the first item of /drives and /charges.

import { h, svg, replace } from '../shared/dom.js';
import { format, parts, rangeLabel, rangeField, getUnits } from '../shared/units.js';
import { formatTime, formatDate, formatDateTime, formatDuration, formatRelative, dayKey, periodLabel } from '../shared/time.js';
import { formatList } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { pageHead, q, plus, figure, chip, icon, stateLabel, empty, errorView, loadingBlock, sep, ago, routeSketch } from '../core/ui.js';
import { makeMap, coordsText, boundsOf } from '../core/map.js';
import { stackedCharts, spansFromStates } from '../core/chart.js';
import { activityStrip, timelineRow, AWAKE_WORDS } from '../core/journal.js';

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
  const periodBox = h('div', { class: 'car-period' });

  function draw() {
    replace(head, header(car, now, tz));
    replace(whereText, whereLines(now, tz, outsideMaps));
    replace(live, staleBanner(now, tz), h('p', { class: 'lede' }, lede(now, tz)), hero(car, now, tz), vitals(car, now, tz), awake(now), charging(now, tz), where, latest);
  }
  draw();
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

/** Status flags as chips: plugged in, charge port, Sentry, climate, someone in the car, service mode. */
function flags(now) {
  const f = now.flags || {};
  const ch = now.charge || {};
  const out = [];
  if (v(ch.plugged)) out.push(chip(now.state === 'charging' ? 'Charging' : 'Plugged in', { kind: 'plug', iconName: 'plug', cover: 'summary.flag.plugged_in' }));
  else if (now.state !== 'driving') out.push(chip('Not plugged in', { cover: 'summary.flag.plugged_in' }));
  if (v(f.charge_port_open)) out.push(chip('Charge port open'));
  if (v(f.sentry)) out.push(chip('Sentry mode on', { kind: 'on', cover: 'summary.flag.sentry_mode' }));
  if (v(f.sentry_event_guess)) out.push(chip('Sentry may be recording (a guess from the camera and screen)', { kind: 'warn', cover: 'summary.flag.sentry_recording' }));
  const climate = v(f.climate);
  if (climate === 'precondition' || v(f.battery_preconditioning)) out.push(chip(v(f.battery_preconditioning) ? 'Battery preconditioning' : 'Cabin preconditioning', { kind: 'on', cover: 'summary.flag.preconditioning' }));
  else if (climate === 'on') out.push(chip('Climate on'));
  else if (climate === 'overheat_protect') out.push(chip('Cabin overheat protection'));
  const keep = v(f.keep_climate);
  if (keep && keep !== 'off') out.push(chip({ on: 'Keep climate on', dog: 'Dog mode', camp: 'Camp mode' }[keep] || keep, { kind: 'on', cover: 'summary.flag.dog_mode' }));
  if (v(f.user_present)) out.push(chip('Someone in the car', { cover: 'summary.flag.driver_present' }));
  if (v(f.service_mode)) out.push(chip('Service mode', { kind: 'warn', cover: 'summary.flag.service_mode' }));
  if (v(f.reduced_range)) out.push(chip('Reduced range: cold battery', { kind: 'warn', cover: 'summary.flag.reduced_range' }));
  if (v(f.parking_brake)) out.push(chip('Parking brake on'));
  return h('div', { class: 'chips' }, now.stale ? h('span', { class: 'chips-label' }, 'Last known') : null, out);
}

/* ------------------------------------------------------------------ temperatures, tyres, the car */

function row(name, value, note = null, { cover = null, measured = null } = {}) {
  return h('div', { class: 'spec-row', cover, title: measured ? `Measured ${measured}` : null },
    h('dt', null, name), h('dd', null, value, note ? h('span', { class: 'spec-note' }, note) : null));
}

function vitals(car, now, tz) {
  const t = (x) => (at(x) ? formatDateTime(at(x), tz, { now: api.now(), seconds: true }) : null);
  const sp = (x) => { const val = v(x); return typeof val === 'string' ? val : format(val, 'temperature'); };
  const temps = h('section', { class: 'vital' }, h('h2', { class: 'label' }, 'Temperature'),
    h('dl', { class: 'spec' },
      row('Outside', q(v(now.outside_c), 'temperature'), null, { cover: ['summary.stat.outside_temp', 'overview.stat.outside_temp'], measured: t(now.outside_c) }),
      row('Cabin', q(v(now.cabin_c), 'temperature', { why: car.profile === 'tesla-legacy' ? 'not sent by this car' : null }), null, { cover: ['summary.stat.inside_temp', 'overview.stat.inside_temp'], measured: t(now.cabin_c) }),
      row('Battery', q(v(now.battery_c), 'temperature'), null, { measured: t(now.battery_c) }),
      row('Set points', h('span', null, sp(now.setpoint_left_c), ' / ', sp(now.setpoint_right_c)), 'driver / passenger', { cover: 'overview.stat.driver_temp', measured: t(now.setpoint_left_c) })));

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
  const buses = now.buses || [];
  const vb = buses.find((b) => b.bus === 'vehicle'), cb = buses.find((b) => b.bus === 'chassis');
  let text;
  if (now.state === 'asleep') text = 'Asleep: both buses are quiet.';
  else if (now.state === 'unknown' || now.stale) text = 'Not known: no recent data.';
  else if (now.state === 'driving') text = 'Driving.';
  else text = reasons.length ? `Awake because of ${formatList(reasons)}.` : 'Awake, with no reason the car reports.';
  const busNote = vb && cb && vb.awake && !cb.awake ? 'Parked but kept awake: the vehicle bus is awake while the chassis bus sleeps.' : null;
  const doors = DOORS.map(([k, name]) => [name, v(f.doors && f.doors[k])]).concat([['Trunk', v(f.trunk)], ['Frunk', v(f.frunk)]]);
  const lift = v(f.liftgate_deg);
  const open = doors.filter(([, o]) => o === true).map(([n]) => n);
  return h('section', { class: 'awake-doors' },
    h('div', { class: 'awake', cover: 'web.settings.car.sleep-blockers' },
      h('h2', { class: 'label' }, 'Why the car is awake'),
      h('p', { class: 'awake-text' }, text),
      busNote ? h('p', { class: 'note' }, busNote) : null,
      h('p', { class: 'note' }, 'Sentry Mate only listens: it never wakes the car or keeps it awake.')),
    h('div', { class: 'doors', cover: 'summary.flag.doors_open' },
      h('h2', { class: 'label' }, 'Doors and openings'),
      h('ul', { class: 'door-list' }, doors.map(([n, o]) => h('li', { class: ['door', { 'is-open': o === true }] }, h('span', null, n), h('span', { class: 'door-state' }, o == null ? 'not sent' : o ? 'Open' : 'Closed'))),
        lift != null ? h('li', { class: ['door', { 'is-open': lift > 2 }] }, h('span', null, 'Liftgate'), h('span', { class: 'door-state' }, lift > 2 ? `Open, ${Math.round(lift)}°` : 'Closed')) : null),
      h('p', { class: ['note', { 'is-warn': open.length && now.state === 'asleep' }] }, open.length ? `Open: ${formatList(open)}.` : 'All closed.'),
      h('p', { class: 'note', cover: ['summary.flag.lock', 'summary.flag.windows_open'] }, 'Locks and windows: the car does not send these on its CAN bus, so they are left out.')));
}

/* ------------------------------------------------------------------ charging now */

function charging(now, tz) {
  const ch = now.charge || {};
  if (!v(ch.plugged) && now.state !== 'charging') return null;
  const finish = ch.finish_ms;
  const toMin = v(ch.to_full_min);
  const rows = [
    ['Status', CHARGE_WORDS[v(ch.status)] || '—', null],
    ['Kind', v(ch.kind) ? (v(ch.kind) === 'dc' ? 'DC (fast charger)' : 'AC') : '—', null],
    ['Power', q(v(ch.power_kw), 'power'), ['summary.stat.charger_power', 'nr.dash.charging-power']],
    ['Volts', q(v(ch.volts_v), 'voltage'), 'overview.stat.ac_voltage'],
    ['Amps', q(v(ch.amps_a), 'current'), 'nr.dash.charging-current'],
    ['Phases', v(ch.phases) != null ? String(v(ch.phases)) : '—', null],
    ['Pilot current', v(ch.pilot_a) != null ? h('span', null, format(v(ch.pilot_a), 'current'), v(ch.limit_source) ? h('span', { class: 'spec-note' }, `limit set by ${LIMIT_WORDS[v(ch.limit_source)] || v(ch.limit_source)}`) : null) : '—', null],
    ['Added', plus(q(v(ch.added_kwh), 'energy')), 'summary.stat.energy_added'],
    ['Charge limit', q(v(ch.limit_pct), 'percent'), 'summary.stat.charge_limit'],
    ['Still to add', q(v(ch.to_full_kwh), 'energy'), null],
    ['Time to full', toMin != null ? formatDuration(toMin * 60000) : 'not charging', ['summary.stat.remaining_time', 'nr.dash.time-to-full']],
    ['Done at', finish ? formatDateTime(finish, tz, { now: api.now() }) : '—', 'summary.stat.expected_finish'],
  ];
  return h('section', { class: 'charging-now', 'aria-labelledby': 'charging-title', cover: 'nr.dash.panel.charge-status' },
    h('h2', { class: 'label', id: 'charging-title' }, 'Charging now', ch.charge_id ? h('a', { class: 'label-note', href: state.buildUrl('charge', { car: state.current().car, charge: ch.charge_id }) }, 'Open this charge') : null),
    h('dl', { class: 'charge-grid' }, rows.map(([n, val, cover]) => h('div', { class: 'charge-cell', cover }, h('dt', { class: 'fig-name' }, n), h('dd', { class: 'charge-value' }, val)))));
}

/* ------------------------------------------------------------------ where */

function whereLines(now, tz, outsideMaps) {
  const L = now.location || {};
  const lat = v(L.lat), lon = v(L.lon);
  const acc = v(L.accuracy_m), hd = v(L.heading_deg);
  const title = now.stale ? 'Last known place' : now.state === 'driving' ? 'On the road' : 'Where';
  const a = L.address || {};
  return [
    h('h2', { class: 'label', id: 'where-title' }, title),
    h('p', { class: 'where-place' }, L.place ? L.place.name : a.short || '—'),
    h('p', { class: 'where-area' }, [L.place ? a.short : null, a.locality, a.region].filter((x, i, arr) => x && arr.indexOf(x) === i).join(', ')),
    now.state !== 'driving' && now.since_ms ? h('p', { class: 'where-since' }, `Here since ${formatDateTime(now.since_ms, tz, { now: api.now() })}`, sep(), formatDuration(api.now() - now.since_ms)) : null,
    hd != null ? h('p', { class: 'where-since' }, `Heading ${compass(hd)} (${Math.round(hd)}°)`) : null,
    lat != null ? h('p', { class: 'where-coords mono' }, coordsText([lon, lat]), acc != null ? ` ± ${format(acc, 'length')}` : '') : null,
    h('p', { class: 'where-ext', cover: 'summary.map.external_link' }, outsideMaps && lat != null
      ? h('a', { href: `https://www.openstreetmap.org/?mlat=${lat.toFixed(5)}&mlon=${lon.toFixed(5)}#map=16/${lat.toFixed(5)}/${lon.toFixed(5)}`, target: '_blank', rel: 'external noopener noreferrer' }, 'Open on OpenStreetMap')
      : h('span', { class: 'note' }, 'Outside map links are off. An admin can turn them on in Settings: Display and units.')),
  ];
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
          h('h3', { class: 'entry-title' }, h('em', null, d.start.label), going ? null : [h('span', { class: 'to' }, ' to '), h('em', null, d.end.label)]),
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
  replace(aside, 
    h('div', { class: 'today-head' }, h('p', { class: 'eyebrow' }, 'The period'), h('h2', { class: 'today-date' }, label)),
    t ? h('dl', { class: 'today-figs', cover: ['overview.stat.distance_logged', 'overview.stat.consumption_net', 'overview.stat.consumption_gross'] },
      figure('Distance', q(t.distance.drives_km, 'distance'), 'logged drives'),
      figure('Drives', String((t.drives || []).length)),
      figure('Net consumption', q(t.wh_per_km.net, 'efficiency'), 'driving only'),
      figure('Gross consumption', q(t.wh_per_km.gross, 'efficiency'), 'parked use included')) : errorView(trip.reason, { eyebrow: 'Figures' }),
    rows.length ? h('ol', { class: 'tl' }, rows.map((r) => timelineRow(r, { car: car.id, tz, period: ctx.state.period }))) : empty({ title: 'Nothing recorded in this period.' }),
    h('a', { class: 'link-arrow', href: state.linkTo('timeline') }, rows.length >= 40 ? 'More on the timeline' : 'The period on the timeline', icon('arrowRight')));

  // Below: the activity strip and the battery chart, on one time axis.
  const strip = act.status === 'fulfilled'
    ? activityStrip(act.value, { tz, link: (s) => (s.ref && s.ref.drive ? state.buildUrl('drive', { car: car.id, drive: s.ref.drive }) : s.ref && s.ref.charge ? state.buildUrl('charge', { car: car.id, charge: s.ref.charge }) : null) })
    : errorView(act.reason, { eyebrow: 'Activity' });
  const charts = h('div', { class: 'charts' });
  replace(box, 
    h('section', { class: 'block', cover: ['overview.chart.states', 'overview.control.time_range', 'overview.control.refresh', 'overview.filter.car'] },
      h('div', { class: 'block-head' }, h('h2', { class: 'label' }, 'Activity', h('span', { class: 'label-note' }, label)),
        h('a', { class: 'link-arrow', href: state.linkTo('timeline'), cover: 'overview.link.states' }, 'Timeline', icon('arrowRight'))),
      strip),
    h('section', { class: 'block', cover: ['overview.chart.charge_level', 'overview.control.shared_crosshair'] },
      h('div', { class: 'block-head' }, h('h2', { class: 'label' }, 'Battery', h('span', { class: 'label-note' }, 'asleep and unknown time shaded')),
        h('a', { class: 'link-arrow', href: state.linkTo('battery'), cover: 'overview.link.charge_level' }, 'Battery history', icon('arrowRight'))),
      charts));
  if (series.status === 'fulfilled' && series.value.t_ms.length) {
    const s = series.value;
    const rf = rangeField('range_km');
    await stackedCharts(ctx, charts, {
      tz, t_ms: s.t_ms, states: s.state, spans: spansFromStates(s.t_ms, s.state, s.bucket_ms),
      rows: [
        { key: 'soc', name: 'Battery', kind: 'percent', values: s.values.soc_pct, colour: '--accent', fill: '--chart-wash', floor: 0, ticks: 4 },
        { key: 'range', name: rangeLabel(), kind: 'range', values: s.values[rf] || s.values.range_km, colour: '--ink-2', floor: 0, ticks: 4 },
      ],
    });
  } else {
    replace(charts, series.status === 'rejected' ? errorView(series.reason, { eyebrow: 'Battery' }) : empty({ title: 'No battery readings in this period.' }));
  }
}

