// Sentry Mate, "Pebble" UI pack: Car (/cars/{car}), what the car is doing now and its last day.
//
// A hero tile with the big battery ring, the range and where the car is; a map tile; small tiles for speed,
// temperatures, odometer, tyres (four dots on a car outline), locks and Sentry, software, the last drive and the
// last charge; a charging tile; what keeps the car awake; the period's activity strip and summary; a battery
// chart and a charging chart that share one cursor. Every blank value says why (asleep, not sent by this car,
// not on the CAN bus). Live: `now` events update the tiles in place.
//
// Data: GET /api/v1/cars/{car}/now, /activity, /series (battery), /trip (the period's summary), /drives and
// /charges (the latest of each, and the period's charges with their /curve), /settings (outside map links).

import { h } from '../shared/dom.js';
import { format, rangeValue, rangeLabel, getUnits } from '../shared/units.js';
import { formatDate, formatWhen, formatDuration } from '../shared/time.js';
import { formatList } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { tile, num, ring, stateTag, chip, btn, icon, note, ago } from '../core/ui.js';
import { openPeriod, rangeText } from '../core/period.js';
import { mapTile, coordsText } from '../core/map.js';
import { timeChart, spansOf } from '../core/chart.js';
import { v, AWAKE_WORDS, endLabel, placeLink, strip, stripShares, facts, dur } from '../core/bits.js';
import { srcPill, viaMark, inputsLine } from '../core/source.js';

const withVia = (text, x) => { const m = viaMark(x); return m ? h('span', { class: 'src-line' }, text, m) : text; };

export const id = 'car';
export const css = true;
export const shape = [[2, 'h3'], [2, 'h3'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [2, 'h2'], [2, 'h2']];
export const title = () => 'Car';

const isPair = (x) => x && typeof x === 'object' && !Array.isArray(x) && 'value' in x;
/** Merge a `now` delta (only what changed) into the object. A { value, at_ms, sources } pair carries only its changed
 *  parts: a newer at_ms alone keeps the value, a new value without sources keeps the input. */
function mergeNow(base, delta) {
  const out = { ...base };
  for (const [k, d] of Object.entries(delta || {})) {
    const b = out[k];
    if (d && typeof d === 'object' && !Array.isArray(d) && !isPair(d) && b && typeof b === 'object' && !Array.isArray(b) && !isPair(b)) out[k] = mergeNow(b, d);
    else if (d && typeof d === 'object' && !Array.isArray(d) && isPair(b)) out[k] = { ...b, ...d };
    else out[k] = d;
  }
  return out;
}
const TYRES = [['fl', 'Front left'], ['fr', 'Front right'], ['rl', 'Rear left'], ['rr', 'Rear right']];
const CLIMATE = { off: 'Off', on: 'On', precondition: 'Preconditioning', overheat_protect: 'Overheat protection' };
const KEEP = { off: 'Off', on: 'Keep climate on', dog: 'Dog mode', camp: 'Camp mode' };

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz;
  const P = ctx.params;
  const keep = (p) => p.catch((err) => ({ error: err }));
  const [now0, act, series, trip, lastDrive, lastCharge, settings] = await Promise.all([
    ctx.api.get(api.apiPath`cars/${car.id}/now`),
    keep(ctx.api.get(api.apiPath`cars/${car.id}/activity`, P)),
    keep(ctx.api.get(api.apiPath`cars/${car.id}/series`, { ...P, fields: 'soc_pct,usable_soc_pct,range_km,ideal_range_km', max_points: 600 })),
    keep(ctx.api.get(api.apiPath`cars/${car.id}/trip`, P)),
    keep(ctx.api.page(api.apiPath`cars/${car.id}/drives`, {}, { limit: 1 })),
    keep(ctx.api.page(api.apiPath`cars/${car.id}/charges`, {}, { limit: 1 })),
    keep(ctx.api.get('settings', null, { maxAgeMs: 60000 })),
  ]);
  if (!ctx.alive()) return {};
  let now = now0;
  const refresh = btn('Refresh', { kind: 'soft', icon: 'refresh', iconOnly: true, cover: 'overview.control.refresh', onclick: () => ctx.reload() });
  ctx.head({ actions: [refresh] });

  /* ---------------------------------------------------------------- the hero */
  const hero = tile({ w: 2, cls: 'hero', cover: ['shots.web-interface', 'shots.overview', 'nr.dash.panel.car-status'] });
  const where = tile({ w: 2, title: 'Where it is', icon: 'pin', cls: ['map-tile', 'where-tile'], cover: ['summary.map.position', 'summary.map.attribution', 'summary.map.fullscreen', 'summary.map.zoom'] });
  const mapBox = h('div', { class: 'map' });
  const whereFoot = h('div', { class: 'map-foot' });
  where.append(mapBox, whereFoot);

  function why(x, { sleepy = true } = {}) {
    if (x != null && v(x) != null) return null;
    if (now.state === 'unknown') return 'no recent log';
    if (sleepy && now.state === 'asleep') return 'asleep, not sent';
    return 'not sent by this car';
  }
  function drawHero() {
    const st = now.state || 'unknown';
    const soc = v(now.soc_pct);
    const limit = v(now.charge?.limit_pct);
    const place = now.location?.place;
    const addr = now.location?.address;
    const whereWords = st === 'driving' ? `Driving${addr?.short ? ` near ${addr.short}` : ''}` : place ? `at ${place.name}` : addr?.short ? `near ${addr.short}` : 'place not known';
    const band = h('div', { class: 'band', cover: ['nr.dash.battery-level', 'nr.dash.battery-level.bands', 'nr.dash.charge-limit', 'summary.stat.charge_limit'] },
      h('span', { class: 'band-track', 'aria-hidden': 'true' },
        h('span', { class: 'band-low' }), h('span', { class: 'band-fill' }), limit != null ? h('span', { class: 'band-limit' }) : null),
      h('span', { class: 'band-words' }, `Under 20 % is low.`, limit != null ? ` Charge limit ${format(limit, 'percent')}.` : ` Charge limit: ${why(now.charge?.limit_pct, { sleepy: false })}.`));
    band.querySelector('.band-fill').style.setProperty('--v', ((soc ?? 0) / 100).toFixed(4));
    if (limit != null) band.querySelector('.band-limit').style.setProperty('--v', (limit / 100).toFixed(4));
    band.classList.toggle('is-low', soc != null && soc < 20);
    const ideal = v(now.ideal_range_km), rated = v(now.range_km), est = v(now.est_range_km);
    hero.replaceChildren(
      h('div', { class: 'hero-top' },
        ring(soc, { label: 'Battery', size: 'l', tone: st === 'charging' ? 'teal' : 'coral', cover: ['summary.stat.soc', 'overview.stat.battery_level'] }),
        h('div', { class: 'hero-text' },
          h('p', { class: 'hero-model', cover: ['summary.header.model', 'summary.header.trim'] }, [car.year, car.model, car.trim || v(car.config?.trim)].filter(Boolean).join(' ')),
          h('p', { class: 'hero-name', cover: 'summary.header.name' }, car.name),
          h('p', { class: 'hero-state', cover: ['summary.stat.status', 'summary.state.drive_offline'] }, stateTag(st, { stale: !!now.stale }),
            now.since_ms ? h('span', { class: 'muted' }, `since ${formatWhen(now.since_ms, tz, api.now())}`) : null),
          h('p', { class: 'hero-where' }, icon('pin'), whereWords),
          inputsLine(now.inputs, { cls: 'car-inputs' }),
          viaMark(now.soc_pct) ? h('p', { class: 'src-line' }, h('span', { class: 'hero-k' }, 'Battery level'), viaMark(now.soc_pct)) : null,
          now.stale ? h('p', { class: 'note' }, 'No upload for a while: these are the last known values.') : null)),
      h('div', { class: 'hero-nums' },
        h('div', { class: 'hero-num', cover: ['summary.stat.range_preferred', 'overview.stat.range'] }, h('span', { class: 'hero-k' }, rangeLabel()), num(rangeValue({ range_km: rated, ideal_range_km: ideal }, 'range_km'), 'range', { size: 'm', why: why(now.range_km) }), viaMark(now.range_km)),
        h('div', { class: 'hero-num', cover: 'nr.dash.ideal-range' }, h('span', { class: 'hero-k' }, getUnits().range === 'ideal' ? 'Rated range' : 'Ideal range'), num(getUnits().range === 'ideal' ? rated : ideal, 'range', { size: 'm', why: why(now.ideal_range_km) })),
        h('div', { class: 'hero-num', cover: ['summary.stat.range_estimated', 'nr.dash.est-range'] }, h('span', { class: 'hero-k' }, 'Estimated'), num(est, 'range', { size: 'm', why: why(now.est_range_km) }),
          est != null ? h('span', { class: 'hero-k' }, now.est_basis === 'wh_per_km_30d' ? 'at your last 30 days' : 'at recent use') : null)),
      band);
  }

  /* ---------------------------------------------------------------- small live tiles */
  const tSpeed = tile({ w: 1, title: 'Speed', icon: 'gauge', cls: 'tile-stat', cover: 'summary.stat.speed' });
  const tOut = tile({ w: 1, title: 'Outside', icon: 'thermo', cls: 'tile-stat', cover: ['summary.stat.outside_temp', 'overview.stat.outside_temp'] });
  const tIn = tile({ w: 1, title: 'Inside', icon: 'thermo', cls: 'tile-stat', cover: ['summary.stat.inside_temp', 'overview.stat.inside_temp'] });
  const tOdo = tile({ w: 1, title: 'Odometer', icon: 'route', cls: 'tile-stat', cover: ['summary.stat.odometer', 'overview.stat.odometer'] });
  function drawSmall() {
    tSpeed.replaceChildren(tSpeed.firstChild, num(v(now.speed_kph), 'speed', { why: why(now.speed_kph) }),
      h('p', { class: 'tile-line' }, now.gear ? `Gear ${v(now.gear)}` : '', v(now.power_kw) != null ? ` · ${format(v(now.power_kw), 'power')}` : ''),
      viaMark(now.speed_kph) ? h('p', { class: 'src-line' }, viaMark(now.speed_kph)) : null);
    tOut.replaceChildren(tOut.firstChild, num(v(now.outside_c), 'temperature', { why: why(now.outside_c) }), h('p', { class: 'tile-line' }, `Battery ${format(v(now.battery_c), 'temperature')}`), viaMark(now.outside_c) ? h('p', { class: 'src-line' }, viaMark(now.outside_c)) : null);
    const sl = v(now.setpoint_left_c);
    tIn.replaceChildren(tIn.firstChild, num(v(now.cabin_c), 'temperature', { why: why(now.cabin_c) }),
      h('p', { class: 'tile-line', cover: 'overview.stat.driver_temp' }, 'Driver set to ', sl == null ? 'not sent' : typeof sl === 'string' ? sl : format(sl, 'temperature'),
        `, climate ${(CLIMATE[v(now.flags?.climate)] || 'not known').toLowerCase()}`, viaMark(now.flags?.climate)),
      viaMark(now.cabin_c) ? h('p', { class: 'src-line' }, viaMark(now.cabin_c)) : null);
    tOdo.replaceChildren(tOdo.firstChild, num(v(now.odometer_km), 'odometer', { why: why(now.odometer_km) }),
      viaMark(now.odometer_km) ? h('p', { class: 'src-line' }, viaMark(now.odometer_km)) : null,
      h('a', { class: 'tile-link', href: state.buildUrl('statistics', { car: car.id }), cover: 'overview.link.mileage' }, 'Mileage over time'));
  }

  /* ---------------------------------------------------------------- tyres */
  const tTyres = tile({ w: 2, title: 'Tyres', icon: 'tyre', cls: 'tyres-tile' });
  function drawTyres() {
    const list = now.tyres_kpa || [];
    const rec = v(now.tyre_recommended_kpa);
    const low = list.some((t) => v(t) != null && rec != null && v(t) < rec * 0.9);
    const dots = TYRES.map(([k, name], i) => {
      const val = v(list[i]);
      const bad = val != null && rec != null && val < rec * 0.9;
      return h('div', { class: ['tyre', `tyre-${k}`, { 'is-low': bad, 'is-missing': val == null }] },
        h('span', { class: 'tyre-dot', 'aria-hidden': 'true' }),
        h('span', { class: 'tyre-text' }, h('span', { class: 'tyre-name' }, name), h('span', { class: 'tyre-val' }, val == null ? '—' : format(val, 'pressure'))));
    });
    tTyres.replaceChildren(tTyres.firstChild,
      h('div', { class: 'tyres', role: 'img', 'aria-label': TYRES.map(([, n], i) => `${n} ${format(v(list[i]), 'pressure')}`).join(', ') },
        h('span', { class: 'tyres-car', 'aria-hidden': 'true' }), dots),
      h('p', { class: ['tile-line', { 'is-warn': low }], cover: 'summary.flag.tire_pressure_warning' },
        !list.length ? `Tyre pressures: ${why(null)}.` : low ? 'A tyre is more than 10 % under what the car recommends.' : rec != null ? `All near the ${format(rec, 'pressure')} the car recommends.` : 'No recommended pressure sent.',
        now.tyres_slots_mapped === false ? ' Which wheel is which is not known yet.' : ''));
  }

  /* ---------------------------------------------------------------- status flags */
  const tFlags = tile({ w: 2, title: 'Car status', icon: 'shield', cls: 'flags-tile' });
  function flag(text, on, { tone = 'coral', cover = null, why: w = null, via = null } = {}) {
    return h('li', { class: ['flag', { 'is-on': on === true, 'is-unknown': on == null }], cover },
      h('span', { class: ['flag-dot', `tone-${tone}`], 'aria-hidden': 'true' }),
      h('span', { class: 'flag-name' }, text),
      h('span', { class: 'flag-val' }, on == null ? (w || 'not sent') : on ? 'On' : 'Off', via ? viaMark(via) : null));
  }
  function drawFlags() {
    const f = now.flags || {};
    const doors = f.doors ? Object.values(f.doors).map(v) : [];
    const doorsOpen = doors.length ? doors.some(Boolean) || !!v(f.trunk) || !!v(f.frunk) : null;
    const keepC = v(f.keep_climate);
    tFlags.replaceChildren(tFlags.firstChild, h('ul', { class: 'flags' },
      flag('Sentry mode', v(f.sentry), { tone: 'coral', cover: 'summary.flag.sentry_mode', via: f.sentry }),
      flag('Sentry recording', v(f.sentry_event_guess), { tone: 'coral', cover: 'summary.flag.sentry_recording' }),
      flag('Someone in the car', v(f.user_present), { tone: 'sky', cover: 'summary.flag.driver_present', via: f.user_present }),
      flag('Plugged in', v(now.charge?.plugged), { tone: 'teal', cover: 'summary.flag.plugged_in' }),
      flag('Doors, trunk or frunk open', doorsOpen, { tone: 'sun', cover: 'summary.flag.doors_open' }),
      flag(keepC === 'camp' ? 'Camp mode' : 'Dog mode', keepC == null ? null : keepC === 'dog' || keepC === 'camp', { tone: 'sky', cover: 'summary.flag.dog_mode' }),
      flag('Preconditioning', v(f.climate) == null && v(f.battery_preconditioning) == null ? null : v(f.climate) === 'precondition' || !!v(f.battery_preconditioning), { tone: 'teal', cover: 'summary.flag.preconditioning' }),
      flag('Reduced range', v(f.reduced_range), { tone: 'sun', cover: 'summary.flag.reduced_range' }),
      flag('Service mode', v(f.service_mode), { tone: 'sun', cover: ['summary.flag.service_mode', 'summary.state.in_service'] }),
      flag('Locked', null, { why: 'not on the CAN bus', cover: 'summary.flag.lock' }),
      flag('Windows open', null, { why: 'not on the CAN bus', cover: 'summary.flag.windows_open' })));
  }

  /* ---------------------------------------------------------------- awake */
  const tAwake = tile({ w: 2, title: 'Why it is awake', icon: 'sun', cls: 'awake-tile', cover: 'web.settings.car.sleep-blockers' });
  function drawAwake() {
    const r = now.awake_reasons || [];
    const buses = (now.buses || []).map((b) => `${b.bus} bus ${b.awake ? 'awake' : 'asleep'}`);
    tAwake.replaceChildren(tAwake.firstChild,
      h('p', { class: 'lede' }, now.state === 'asleep' ? 'Asleep: nothing keeps it awake.' : r.length ? `Kept awake by ${formatList(r.map((x) => AWAKE_WORDS[x] || x.replace(/_/g, ' ')))}.` : now.state === 'driving' ? 'Driving.' : 'Nothing known keeps it awake; it should fall asleep soon.'),
      buses.length ? note(`${buses.join(', ')}.`) : null,
      h('p', { class: 'note' }, 'Keep climate: ', (KEEP[v(now.flags?.keep_climate)] || 'not sent').toLowerCase(), '.'));
  }

  /* ---------------------------------------------------------------- charging */
  const tCharge = tile({ w: 2, title: 'Charging', icon: 'bolt', cls: 'charge-tile', cover: ['nr.dash.panel.charge-status'] });
  function drawCharge() {
    const c = now.charge || {};
    const status = v(c.status);
    const on = status === 'charging' || now.state === 'charging';
    const kind = v(c.kind);
    tCharge.replaceChildren(tCharge.firstChild,
      h('div', { class: 'charge-top' },
        on ? h('span', { class: 'src-line' }, num(v(c.power_kw), 'power', { size: 'l', why: why(c.power_kw), cover: ['summary.stat.charger_power', 'overview.stat.charging_power', 'nr.dash.charging-power'] }), viaMark(c.power_kw))
          : h('p', { class: 'lede', cover: ['summary.stat.charger_power', 'overview.stat.charging_power', 'nr.dash.charging-power'] }, v(c.plugged) ? 'Plugged in, not charging' : 'Not plugged in'),
        chip(status ? String(status).replace(/_/g, ' ') : 'status not sent', { tone: on ? 'teal' : 'muted' }),
        kind ? chip(kind.toUpperCase(), { tone: 'sky' }) : null),
      facts([
        ['Energy added', withVia(format(v(c.added_kwh), 'energy'), c.added_kwh), 'summary.stat.energy_added'],
        ['Time to full', withVia(v(c.to_full_min) != null ? formatDuration(v(c.to_full_min) * 60000) : on ? 'not sent' : 'not charging', c.to_full_min), ['summary.stat.remaining_time', 'nr.dash.time-to-full']],
        ['Done at', c.finish_ms ? formatWhen(v(c.finish_ms), tz, api.now()) : on ? 'not known' : 'not charging', 'summary.stat.expected_finish'],
        ['Charge limit', withVia(format(v(c.limit_pct), 'percent'), c.limit_pct), 'nr.dash.charge-limit'],
        ['Current', v(c.amps_a) != null ? `${format(v(c.amps_a), 'current')}${v(c.phases) ? `, ${v(c.phases)} phase${v(c.phases) > 1 ? 's' : ''}` : ''}${v(c.pilot_a) ? ` (pilot ${format(v(c.pilot_a), 'current')})` : ''}` : on ? 'not sent' : '—', 'nr.dash.charging-current'],
        ['Voltage', format(v(c.volts_v), 'voltage'), 'overview.stat.ac_voltage'],
        ['Scheduled charging', 'not on the CAN bus', 'summary.stat.scheduled_charging'],
      ]));
  }

  /* ---------------------------------------------------------------- software, last drive, last charge, data */
  const sw = now.software || {};
  const tSoft = tile({ w: 1, title: 'Software', icon: 'chip', cls: 'tile-stat', cover: ['summary.stat.version', 'overview.stat.firmware', 'summary.flag.software_update'] },
    h('p', { class: 'tile-big' }, sw.version || 'Not known'),
    h('p', { class: 'tile-line' }, sw.version ? (sw.version_source === 'owner' ? 'typed by the owner' : sw.version_source === 'build_match' ? 'matched from the build' : sw.version_source === 'tesla_api' ? 'from the Tesla API' : 'from the import') : 'the version is not on the CAN bus',
      sw.changed_since_ms ? `; changed ${formatDate(sw.changed_since_ms, tz, { now: api.now() })}` : ''),
    h('a', { class: 'tile-link', href: state.buildUrl('software', { car: car.id }), cover: 'overview.link.updates' }, 'Software history'));
  const d0 = lastDrive.error ? null : lastDrive.items?.[0];
  const tDrive = d0 ? tile({ w: 1, title: 'Last drive', icon: 'route', href: state.buildUrl('drive', { car: car.id, drive: d0.id }), cls: 'tile-stat' },
    num(d0.distance_km, 'distance'),
    h('p', { class: 'tile-line' }, `${endLabel(d0.start)} to ${d0.end_ms ? endLabel(d0.end) : 'now'}`),
    h('p', { class: 'tile-line' }, formatWhen(d0.start_ms, tz, api.now())))
    : tile({ w: 1, title: 'Last drive', icon: 'route', cls: 'tile-stat' }, note(lastDrive.error ? 'Drives did not load.' : 'No drive recorded yet.'));
  const c0 = lastCharge.error ? null : lastCharge.items?.[0];
  const tLastC = c0 ? tile({ w: 1, title: 'Last charge', icon: 'bolt', href: state.buildUrl('charge', { car: car.id, charge: c0.id }), cls: 'tile-stat' },
    num(c0.energy_added_kwh, 'energy'),
    h('p', { class: 'tile-line' }, `${format(c0.start_soc_pct, 'percent')} to ${format(c0.end_soc_pct, 'percent')}, ${c0.place ? c0.place.name : c0.address?.short || 'unnamed spot'}`),
    h('p', { class: 'tile-line' }, formatWhen(c0.start_ms, tz, api.now())))
    : tile({ w: 1, title: 'Last charge', icon: 'bolt', cls: 'tile-stat' }, note(lastCharge.error ? 'Charges did not load.' : 'No charge recorded yet.'));
  const tData = tile({ w: 1, title: 'Data', icon: 'pulse', cls: 'tile-stat', cover: ['summary.data.live_push', 'summary.data.restored_values', 'nr.dash.last-update', 'summary.data.prefs_snapshot'] });
  function drawData() {
    const u = getUnits();
    tData.replaceChildren(tData.firstChild,
      h('p', { class: 'tile-big' }, ago(now.at_ms)),
      h('p', { class: 'tile-line' }, now.delay_s != null ? `Reached Sentry Mate ${Math.round(now.delay_s)} s after the car sent it; updates live.` : 'Updates live as logs arrive.'),
      car.sources?.length ? h('p', { class: 'src-line' }, h('span', { class: 'hero-k' }, 'History'), srcPill(car, { short: true })) : null,
      h('p', { class: 'tile-line' }, now.stale ? 'Last known values, kept from the last log.' : 'Values the car did not send again are the last known.'),
      h('p', { class: 'tile-line' }, `Shown in ${u.distance}, °${u.temperature.toUpperCase()}, ${u.pressure}, ${rangeLabel().toLowerCase()}.`));
  }

  /* ---------------------------------------------------------------- the period: strip, summary, charts */
  const periodBtn = h('button', { type: 'button', class: 'fpill', cover: 'overview.control.time_range', onclick: (e) => openPeriod(ctx.period, tz, e.currentTarget) }, icon('calendar'), rangeText(ctx.period, tz));
  const tStrip = tile({ w: 'full', title: 'What it did', icon: 'timeline', side: periodBtn, cls: 'strip-tile', cover: ['overview.chart.states', 'overview.filter.car'] });
  if (act.error) tStrip.append(note('The activity did not load.'));
  else {
    tStrip.append(strip(act, { tz, link: (s) => (s.ref?.drive ? state.buildUrl('drive', { car: car.id, drive: s.ref.drive }) : s.ref?.charge ? state.buildUrl('charge', { car: car.id, charge: s.ref.charge }) : null) }),
      h('p', { class: 'note' }, stripShares(act).slice(0, 4).map(([k, ms]) => `${({ driving: 'Driving', charging: 'Charging', online: 'Awake', online_vehicle_bus_only: 'Bus awake', asleep: 'Asleep', unknown: 'No data', updating: 'Updating' })[k] || k} ${dur(ms)}`).join(' · ')),
      h('a', { class: 'tile-link', href: state.buildUrl('timeline', { car: car.id, period: ctx.state.period }), cover: 'overview.link.states' }, 'Open the timeline'));
  }
  const tSum = trip.error ? tile({ w: 2, title: 'This period', icon: 'trip' }, note('The summary did not load.'))
    : tile({ w: 2, title: 'This period', icon: 'trip', cls: 'sum-tile', cover: ['overview.stat.distance_logged', 'overview.stat.consumption_net', 'overview.stat.consumption_gross'] },
      h('div', { class: 'sum-nums' },
        h('div', null, h('span', { class: 'hero-k' }, 'Distance'), num(trip.distance?.drives_km, 'distance')),
        h('div', null, h('span', { class: 'hero-k' }, 'Driving use'), num(trip.wh_per_km?.net, 'efficiency', { why: 'no drive' })),
        h('div', null, h('span', { class: 'hero-k' }, 'With parked use'), num(trip.wh_per_km?.gross, 'efficiency', { why: 'no drive' }))),
      note('Driving use counts only the drives; with parked use adds what the car used while parked.'));

  const tBatt = tile({ w: 2, title: 'Battery', icon: 'battery', cls: 'chart-tile', side: h('a', { class: 'tile-link', href: state.buildUrl('battery', { car: car.id, period: ctx.state.period }), cover: 'overview.link.charge_level' }, 'Battery page'),
    cover: ['overview.chart.charge_level', 'overview.control.shared_crosshair'] });
  const tChart = tile({ w: 2, title: 'Charging in this period', icon: 'bolt', cls: 'chart-tile', side: h('a', { class: 'tile-link', href: state.buildUrl('charges', { car: car.id, period: ctx.state.period }), cover: 'overview.link.charging_stats' }, 'Charges page'),
    cover: ['overview.chart.charging_details.power', 'overview.chart.charging_details.energy', 'overview.chart.charging_details.ac_current', 'overview.chart.charging_details.ac_voltage', 'overview.chart.charging_details.battery_heater'] });

  drawHero(); drawSmall(); drawTyres(); drawFlags(); drawAwake(); drawCharge(); drawData();
  ctx.view.replaceChildren(hero, where, tSpeed, tOut, tIn, tOdo, tCharge, tTyres, tFlags, tAwake, tSoft, tDrive, tLastC, tData, tStrip, tSum, tBatt, tChart);

  /* the map: the car's spot, an outside link when the owner allows it */
  const lat = v(now.location?.lat), lon = v(now.location?.lon);
  let mapReady = Promise.resolve();
  const outside = !settings.error && settings.outside_links?.maps;
  whereFoot.append(
    h('span', null, now.location?.place ? placeLink({ place: now.location.place, lat, lon }) : now.location?.address?.short || (lat != null ? coordsText(lon, lat) : 'Position not known')),
    lat != null && outside ? h('a', { class: 'tile-link', href: `https://www.openstreetmap.org/?mlat=${lat.toFixed(5)}&mlon=${lon.toFixed(5)}#map=16/${lat.toFixed(5)}/${lon.toFixed(5)}`, target: '_blank', rel: 'noopener noreferrer', cover: 'summary.map.external_link' }, 'Open in a map', icon('external'))
      : h('span', { class: 'muted', cover: 'summary.map.external_link' }, 'Outside map links are off'));
  if (lat != null && lon != null) {
    const m = await mapTile(ctx, mapBox, { center: [lon, lat], zoom: 14, fullscreen: true });
    if (!m) return {};
    if (m.map) m.pin([lon, lat], { tone: now.state === 'charging' ? 'teal' : 'coral', title: car.name });
    mapReady = m.ready;
  } else {
    mapBox.append(h('p', { class: 'map-note' }, now.state === 'unknown' ? 'No recent log, so no position.' : 'The car has not sent its position yet.'));
  }

  /* the charts: battery over the period, the period's charges joined; one cursor */
  const sync = `car-${car.id}-${Date.now()}`;
  const charts = [];
  if (series.error || !series.t_ms?.length) tBatt.append(note(series.error ? 'The battery chart did not load.' : 'No battery values in this period.'));
  else {
    const rv = series.values;
    charts.push(timeChart(ctx, tBatt, {
      tz, t_ms: series.t_ms, states: series.state, spans: spansOf(series.t_ms, series.state, series.bucket_ms), sync, label: 'Battery', cover: 'overview.chart.legend_toggle',
      series: [
        { key: 'soc', name: 'Battery', kind: 'percent', values: rv.soc_pct, tone: 'c1', fill: true, floor: 0 },
        { key: 'range', name: rangeLabel(), kind: 'range', values: getUnits().range === 'ideal' ? rv.ideal_range_km : rv.range_km, tone: 'c2', dashed: true, floor: 0 },
      ],
    }));
  }
  if (!series.error && series.sources?.length) tBatt.append(h('p', { class: 'src-line' }, srcPill(series)));
  charts.push(drawCharging(ctx, tChart, sync));
  await Promise.all([...charts, mapReady]);
  if (!ctx.alive()) return {};

  ctx.events(['now'], (ev) => {
    const d = ev.data || {};
    if (d.car !== car.id || !d.now) return;
    now = mergeNow(now, d.now);
    drawHero(); drawSmall(); drawTyres(); drawFlags(); drawAwake(); drawCharge(); drawData();
    hero.querySelector('.ring-arc')?.classList.add('no-anim');
  });
  ctx.ready();
  return {};
}

/** The period's charges (up to 8), their curves joined on one time axis with gaps between them. */
async function drawCharging(ctx, el, sync) {
  const car = ctx.car;
  let res;
  try { res = await ctx.api.page(api.apiPath`cars/${car.id}/charges`, ctx.params, { limit: 8 }); } catch { el.append(note('The charges did not load.')); return; }
  const list = (res.items || []).slice().sort((a, b) => a.start_ms - b.start_ms);
  if (!list.length) { el.append(h('p', { class: 'lede chart-empty' }, 'No charge in this period.'), note('Pick a longer period to see power, energy, current, voltage and battery heating.')); return; }
  const curves = await Promise.all(list.map((c) => ctx.api.get(api.apiPath`cars/${car.id}/charges/${c.id}/curve`, { fields: 'power_kw,heater_kw,ac_volts_v,ac_amps_a,dc_volts_v,dc_amps_a', max_points: 240 }).catch(() => null)));
  if (!ctx.alive()) return;
  const t = [], power = [], energy = [], heat = [], amps = [], volts = [];
  const gap = (ms) => { t.push(ms); power.push(null); energy.push(null); heat.push(null); amps.push(null); volts.push(null); };
  list.forEach((c, n) => {
    const cv = curves[n];
    if (!cv || !cv.t_ms?.length) return;
    if (t.length) gap(cv.t_ms[0] - 1);
    const V = cv.values, dc = (cv.kind || c.kind) === 'dc';
    let e = 0;
    cv.t_ms.forEach((ms, i) => {
      const p = V.power_kw?.[i] ?? null;
      if (i > 0 && p != null && V.power_kw[i - 1] != null) e += ((p + V.power_kw[i - 1]) / 2) * ((ms - cv.t_ms[i - 1]) / 3600000);
      t.push(ms); power.push(p); energy.push(Math.round(e * 1000) / 1000); heat.push(V.heater_kw?.[i] ?? null);
      amps.push((dc ? V.dc_amps_a?.[i] : V.ac_amps_a?.[i]) ?? null); volts.push((dc ? V.dc_volts_v?.[i] : V.ac_volts_v?.[i]) ?? null);
    });
  });
  if (!t.length) { el.append(note('These charges have no recorded curve.')); return; }
  await timeChart(ctx, el, {
    tz: ctx.tz, t_ms: t, sync: `${sync}-c`, label: 'Charging',
    series: [
      { key: 'power', name: 'Power', kind: 'power', values: power, tone: 'c3', fill: true, floor: 0 },
      { key: 'energy', name: 'Added', kind: 'energy', values: energy, tone: 'c2', dashed: true, floor: 0 },
    ],
  });
  const facts2 = h('div', { class: 'mini-keys' },
    chip(`Current up to ${format(Math.max(0, ...amps.filter((x) => x != null)), 'current')}`, { tone: 'muted' }),
    chip(`Voltage up to ${format(Math.max(0, ...volts.filter((x) => x != null)), 'voltage')}`, { tone: 'muted' }),
    chip(`Battery heating up to ${format(Math.max(0, ...heat.filter((x) => x != null)), 'power')}`, { tone: 'muted' }));
  el.append(facts2, note(`${list.length} ${list.length === 1 ? 'charge' : 'charges'} drawn one after another${res.next ? '; the Charges page lists every one' : ''}.`));
}

