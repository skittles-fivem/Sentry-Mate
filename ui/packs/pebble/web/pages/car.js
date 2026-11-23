// Sentry Mate, "Pebble" UI pack: Car (/cars/{car}), what the car is doing now and its last day.
//
// A hero tile with the big battery ring, the range and where the car is; a map tile; small tiles for speed,
// temperatures, odometer, tyres (four dots on a car outline), locks and Sentry, software, the last drive and the
// last charge; a charging tile; what keeps the car awake; the period's activity strip and summary; a battery
// chart and a charging chart that share one cursor; then the vehicle info (pages/vehicle-parts.js: each fact of the
// car's build with where it came from and how sure, an admin's corrections). The software tile links to the car's
// firmware fingerprints on Software (#firmware) and to its module firmware history (#firmware-history). Every blank value says why (asleep, not sent by this car,
// not on the CAN bus). What no CAN message carries (lock, windows, sunroof, the route, the software version and a
// waiting update, the charge schedule) comes from now.off_can, the Tesla API's or TeslaFi's newest reading: each
// with its input's mark, and Stale once it is past its age limit; a value no input has is a dash.
// Live: `now` events update the tiles in place; `rows` events of the car (once a burst has passed) read the last
// drive and charge, the strip and the period's summary again, written in place.
//
// Data: GET /api/v1/cars/{car}/now, /activity, /series (battery), /trip (the period's summary), /drives and
// /charges (the latest of each, and the period's charges with their /curve), /settings (outside map links).

import { h, holdScroll } from '../shared/dom.js';
import { format, rangeValue, rangeLabel, getUnits } from '../shared/units.js';
import { formatDate, formatWhen, formatDuration, formatRelative } from '../shared/time.js';
import { formatList } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { tyreView, recommendedLine, wheelGuessLine, tyresTitle } from '../shared/tyres.js';
import { tile, num, ring, stateTag, chip, btn, icon, note, ago } from '../core/ui.js';
import { ghostTile, plotGhost } from '../core/states.js';
import { openPeriod, rangeText } from '../core/period.js';
import { mapTile, coordsText } from '../core/map.js';
import { SPOT_ZOOM } from '../shared/maptiles.js';
import { nearNow } from '../shared/lazy.js';
import { timeChart, spansOf } from '../core/chart.js';
import { v, AWAKE_WORDS, endLabel, placeLink, strip, stripShares, facts, dur } from '../core/bits.js';
import { srcPill, viaMark, inputsLine } from '../core/source.js';

// a tile's new children; a null or false part is left out (replaceChildren would print it as text)
// (what is on screen stays where it is when a tile above it is drawn again taller or shorter: shared/dom.js)
const put = (el, ...kids) => { holdScroll(el); el.replaceChildren(...kids.flat(Infinity).filter((k) => k != null && k !== false && k !== '')); };
import { vehicleTiles } from './vehicle-parts.js';
import { historyHref, VERSION_SOURCE } from '../core/firmware.js';
import { onRows } from '../shared/live.js';
import { morph } from '../core/morph.js';

// the door placard's pressure for a wheel size that is only a guess: say so, and (for an admin) link to where the
// size is typed
const wheelGuess = (now, anchor, admin) => {
  const g = wheelGuessLine(now);
  return g ? [' ', g.said, admin ? [' ', g.ask, ' ', h('a', { href: anchor }, g.action), '.'] : null] : null;
};

const withVia = (text, x) => { const m = viaMark(x); return m ? h('span', { class: 'src-line' }, text, m) : text; };
/** The input mark of an off_can value ({ value, at_ms, sources, stale } or null), and Stale past its age limit. */
const offMark = (x, tz) => (x ? [viaMark(x), x.stale ? h('span', { class: 'src src-via is-stale', title: x.at_ms ? `Newest ${formatWhen(x.at_ms, tz, api.now())}; past its age limit` : null }, 'Stale') : null] : null);
/** Text with its off_can marks, or null when no input has the value. */
const withOff = (text, x, tz) => (x == null || v(x) == null || text == null ? null : h('span', { class: ['src-line', { 'is-stale': !!x.stale }] }, text, offMark(x, tz)));
const WINDOWS = [['driver_front', 'driver front'], ['passenger_front', 'passenger front'], ['driver_rear', 'driver rear'], ['passenger_rear', 'passenger rear']];

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

// The calls that start at once: [now, activity, series, trip, last drive, last charge, settings]. prefetch()
// warms the same ones when a link here is under the pointer.
function firstReads(ctx) {
  const car = ctx.car, P = ctx.params;
  return [
    ctx.api.get(api.apiPath`cars/${car.id}/now`),
    ctx.api.get(api.apiPath`cars/${car.id}/activity`, P),
    ctx.api.get(api.apiPath`cars/${car.id}/series`, { ...P, fields: 'soc_pct,usable_soc_pct,range_km,ideal_range_km', max_points: 600 }),
    ctx.api.get(api.apiPath`cars/${car.id}/trip`, P),
    ctx.api.page(api.apiPath`cars/${car.id}/drives`, {}, { limit: 1 }),
    ctx.api.page(api.apiPath`cars/${car.id}/charges`, {}, { limit: 1 }),
    ctx.api.get('settings', null, { maxAgeMs: 60000 }),
  ];
}
// the charging chart: the period's last charges and their curves
const chargesRead = (ctx) => ctx.api.page(api.apiPath`cars/${ctx.car.id}/charges`, ctx.params, { limit: 8 });
const curveRead = (ctx, c) => ctx.api.get(api.apiPath`cars/${ctx.car.id}/charges/${c.id}/curve`, { fields: 'power_kw,heater_kw,ac_volts_v,ac_amps_a,dc_volts_v,dc_amps_a', max_points: 240 });
export async function prefetch(ctx) {
  const [res] = await Promise.all([chargesRead(ctx), ...firstReads(ctx)]);
  await Promise.all((res?.items || []).map((c) => curveRead(ctx, c)));
}

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz;
  const P = ctx.params;
  const keep = (p) => p.catch((err) => ({ error: err }));
  // Every call starts at once and each tile paints when its own answer is in; a grey tile of its size keeps its
  // room until then, so the live tiles show while the period's strip and charts are still on their way.
  const [nowP, ...rest] = firstReads(ctx);
  const [actP0, seriesP, tripP0, lastDriveP0, lastChargeP0, settingsP] = rest.map(keep);
  // (the activity, trip and last drive and charge are read again in place when the car's rows change)
  let actP = actP0, tripP = tripP0, lastDriveP = lastDriveP0, lastChargeP = lastChargeP0;
  let now = {};
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
    if (!now.at_ms) return 'no recent log';
    if (sleepy && now.state === 'asleep') return 'asleep, not sent';
    return 'not sent by this car';
  }
  /** Where the car is navigating to, from the Tesla API or TeslaFi; nothing while no route is known. */
  function routeLine() {
    const r = now.off_can?.route || {};
    const dest = v(r.destination), km = v(r.km), min = v(r.minutes), traffic = v(r.traffic_min);
    if (dest == null && km == null && min == null) return null;
    const bits = [km != null ? format(km, 'distance') : null, min != null ? `${formatDuration(min * 60000)} to go` : null, traffic ? `${formatDuration(traffic * 60000)} lost to traffic` : null].filter(Boolean);
    // each mark after a space, not a margin: a mark that wraps to a line of its own stays centred on a phone
    const marks = (offMark(r.destination || r.km || r.minutes, tz) || []).filter(Boolean).flatMap((m) => [h('span', { class: 'src-gap' }, ' '), m]);
    return h('p', { class: 'hero-route' }, icon('route'), h('span', null, `Navigating to ${dest || 'a destination'}`, bits.length ? h('span', { class: 'muted' }, `, ${bits.join(', ')}`) : null, marks));
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
    put(hero, 
      h('div', { class: 'hero-top' },
        ring(soc, { label: 'Battery', size: 'l', tone: st === 'charging' ? 'teal' : 'coral', cover: ['summary.stat.soc', 'overview.stat.battery_level'] }),
        h('div', { class: 'hero-text' },
          h('p', { class: 'hero-model', cover: ['summary.header.model', 'summary.header.trim'] }, [car.year, car.model, car.trim || v(car.config?.trim)].filter(Boolean).join(' ')),
          h('p', { class: 'hero-name', cover: 'summary.header.name' }, car.name),
          h('p', { class: 'hero-state', cover: ['summary.stat.status', 'summary.state.drive_offline'] }, stateTag(st, { stale: !!now.stale }),
            now.since_ms ? h('span', { class: 'muted' }, `since ${formatWhen(now.since_ms, tz, api.now())}`) : null),
          h('p', { class: 'hero-where' }, icon('pin'), whereWords),
          routeLine(),
          inputsLine(now.inputs, { cls: 'car-inputs', extra: ctx.admin ? h('a', { class: 'tile-link car-inputs-link', href: `${state.buildUrl('settings-cars', { car: car.id })}#data-sources` }, 'Change order', icon('chevronRight')) : null }),
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
    put(tSpeed, tSpeed.firstChild, num(v(now.speed_kph), 'speed', { why: why(now.speed_kph) }),
      h('p', { class: 'tile-line' }, now.gear ? `Gear ${v(now.gear)}` : '', v(now.power_kw) != null ? ` · ${format(v(now.power_kw), 'power')}` : ''),
      viaMark(now.speed_kph) ? h('p', { class: 'src-line' }, viaMark(now.speed_kph)) : null);
    put(tOut, tOut.firstChild, num(v(now.outside_c), 'temperature', { why: why(now.outside_c) }), h('p', { class: 'tile-line' }, `Battery ${format(v(now.battery_c), 'temperature')}`), viaMark(now.outside_c) ? h('p', { class: 'src-line' }, viaMark(now.outside_c)) : null);
    const sl = v(now.setpoint_left_c);
    put(tIn, tIn.firstChild, num(v(now.cabin_c), 'temperature', { why: why(now.cabin_c) }),
      h('p', { class: 'tile-line', cover: 'overview.stat.driver_temp' }, 'Driver set to ', sl == null ? 'not sent' : typeof sl === 'string' ? sl : format(sl, 'temperature'),
        `, climate ${(CLIMATE[v(now.flags?.climate)] || 'not known').toLowerCase()}`, viaMark(now.flags?.climate)),
      viaMark(now.cabin_c) ? h('p', { class: 'src-line' }, viaMark(now.cabin_c)) : null);
    put(tOdo, tOdo.firstChild, num(v(now.odometer_km), 'odometer', { why: why(now.odometer_km) }),
      viaMark(now.odometer_km) ? h('p', { class: 'src-line' }, viaMark(now.odometer_km)) : null,
      h('a', { class: 'tile-link', href: state.buildUrl('statistics', { car: car.id }), cover: 'overview.link.mileage' }, 'Mileage over time'));
  }

  /* ---------------------------------------------------------------- tyres */
  const tTyres = tile({ w: 2, title: 'Tyres', icon: 'tyre', cls: 'tyres-tile' });
  function drawTyres() {
    const list = now.tyres_kpa || [];
    const ty = tyreView(now);
    const low = ty.anyLow === true;
    const dots = TYRES.map(([k, name], i) => {
      const val = v(list[i]);
      const bad = ty.low[i] === true;
      return h('div', { class: ['tyre', `tyre-${k}`, { 'is-low': bad, 'is-missing': val == null }] },
        h('span', { class: 'tyre-dot', 'aria-hidden': 'true' }),
        h('span', { class: 'tyre-text' }, h('span', { class: 'tyre-name' }, name), h('span', { class: 'tyre-val' }, val == null ? '—' : format(val, 'pressure'))));
    });
    put(tTyres, tTyres.firstChild,
      h('div', { class: 'tyres', role: 'img', 'aria-label': TYRES.map(([, n], i) => `${n} ${format(v(list[i]), 'pressure')}`).join(', ') },
        h('span', { class: 'tyres-car', 'aria-hidden': 'true' }), dots),
      h('p', { class: ['tile-line', { 'is-warn': low }], cover: 'summary.flag.tire_pressure_warning', title: list.length ? tyresTitle(now) || null : null },
        !list.length ? `Tyre pressures: ${why(null)}.` : low ? 'A tyre is more than 10 % under its recommended pressure. ' : ty.front != null ? 'All near the recommended pressure. ' : '',
        list.length ? recommendedLine(now) : '', list.length ? wheelGuess(now, '#vi-wheels', ctx.admin) : null));
  }

  /* ---------------------------------------------------------------- status flags */
  const tFlags = tile({ w: 2, title: 'Car status', icon: 'shield', cls: 'flags-tile' });
  function flag(text, on, { tone = 'coral', cover = null, why: w = null, via = null, off = null, words = ['On', 'Off'] } = {}) {
    return h('li', { class: ['flag', { 'is-on': on === true, 'is-unknown': on == null, 'is-stale': !!off?.stale }], cover },
      h('span', { class: ['flag-dot', `tone-${tone}`], 'aria-hidden': 'true' }),
      h('span', { class: 'flag-name' }, text),
      h('span', { class: 'flag-val' }, on == null ? (w || 'not sent') : on ? words[0] : words[1], via ? viaMark(via) : null, on != null && off ? offMark(off, tz) : null));
  }
  function drawFlags() {
    const f = now.flags || {};
    const doors = f.doors ? Object.values(f.doors).map(v) : [];
    const doorsOpen = doors.length ? doors.some(Boolean) || !!v(f.trunk) || !!v(f.frunk) : null;
    const keepC = v(f.keep_climate);
    const oc = now.off_can || {};
    const openWins = v(oc.windows_open) ? WINDOWS.filter(([k]) => v(oc.windows?.[k])).map(([, w]) => w) : [];
    const roofState = v(oc.sunroof_state), roofPct = v(oc.sunroof_pct);
    const roofWord = roofState != null ? String(roofState).replace(/_/g, ' ') : null;
    const roof = roofWord == null && roofPct == null ? null : [roofWord && roofWord.charAt(0).toUpperCase() + roofWord.slice(1), roofPct != null ? format(roofPct, 'percent') : null].filter(Boolean).join(', ');
    const roofOpen = roofPct != null ? roofPct > 0 : roofState != null ? roofState !== 'closed' : null;
    put(tFlags, tFlags.firstChild, h('ul', { class: 'flags' },
      flag('Sentry mode', v(f.sentry), { tone: 'coral', cover: 'summary.flag.sentry_mode', via: f.sentry }),
      flag('Sentry recording', v(f.sentry_event_guess), { tone: 'coral', cover: 'summary.flag.sentry_recording' }),
      flag('Someone in the car', v(f.user_present), { tone: 'sky', cover: 'summary.flag.driver_present', via: f.user_present }),
      flag('Plugged in', v(now.charge?.plugged), { tone: 'teal', cover: 'summary.flag.plugged_in' }),
      flag('Doors, trunk or frunk open', doorsOpen, { tone: 'sun', cover: 'summary.flag.doors_open' }),
      flag(keepC === 'camp' ? 'Camp mode' : 'Dog mode', keepC == null ? null : keepC === 'dog' || keepC === 'camp', { tone: 'sky', cover: 'summary.flag.dog_mode' }),
      flag('Preconditioning', v(f.climate) == null && v(f.battery_preconditioning) == null ? null : v(f.climate) === 'precondition' || !!v(f.battery_preconditioning), { tone: 'teal', cover: 'summary.flag.preconditioning' }),
      flag('Reduced range', v(f.reduced_range), { tone: 'sun', cover: 'summary.flag.reduced_range' }),
      flag('Service mode', v(f.service_mode), { tone: 'sun', cover: ['summary.flag.service_mode', 'summary.state.in_service'] }),
      flag('Locked', v(oc.locked), { tone: 'teal', why: '—', off: oc.locked, words: ['Yes', 'No'], cover: 'summary.flag.lock' }),
      flag('Windows', v(oc.windows_open), { tone: 'sun', why: '—', off: oc.windows_open, words: ['Open', 'Closed'], cover: 'summary.flag.windows_open' }),
      roof != null ? flag('Sunroof', roofOpen, { tone: 'sun', off: oc.sunroof_state || oc.sunroof_pct, words: [roof, roof] }) : null),
      openWins.length ? h('p', { class: 'tile-line is-warn' }, `Open: ${formatList(openWins)}.`) : null);
  }

  /* ---------------------------------------------------------------- awake */
  const tAwake = tile({ w: 2, title: 'Why it is awake', icon: 'sun', cls: 'awake-tile', cover: 'web.settings.car.sleep-blockers' });
  function drawAwake() {
    const r = now.awake_reasons || [];
    const buses = (now.buses || []).map((b) => `${b.bus} bus ${b.awake ? 'awake' : 'asleep'}`);
    put(tAwake, tAwake.firstChild,
      h('p', { class: 'lede' }, now.state === 'asleep' ? 'Asleep: nothing keeps it awake.' : r.length ? `Kept awake by ${formatList(r.map((x) => AWAKE_WORDS[x] || x.replace(/_/g, ' ')))}.` : now.state === 'driving' ? 'Driving.' : 'Nothing known keeps it awake; it should fall asleep soon.'),
      buses.length ? note(`${buses.join(', ')}.`) : null,
      h('p', { class: 'note' }, 'Keep climate: ', (KEEP[v(now.flags?.keep_climate)] || 'not sent').toLowerCase(), '.'));
  }

  /* ---------------------------------------------------------------- charging */
  const tCharge = tile({ w: 2, title: 'Charging', icon: 'bolt', cls: 'charge-tile', cover: ['nr.dash.panel.charge-status'] });
  const sched = (x) => (v(x) == null ? null : formatWhen(v(x), tz, api.now()));
  function drawCharge() {
    const c = now.charge || {};
    const status = v(c.status);
    const on = status === 'charging' || now.state === 'charging';
    const kind = v(c.kind);
    put(tCharge, tCharge.firstChild,
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
        ['Scheduled charging', withOff(sched(now.off_can?.scheduled_charge_start_ms), now.off_can?.scheduled_charge_start_ms, tz) || '—', 'summary.stat.scheduled_charging'],
        ['Scheduled departure', withOff(sched(now.off_can?.scheduled_departure_ms), now.off_can?.scheduled_departure_ms, tz) || '—'],
      ]));
  }

  /* ---------------------------------------------------------------- software, last drive, last charge, data */
  const tSoft = tile({ w: 1, title: 'Software', icon: 'chip', cls: 'tile-stat', cover: ['summary.stat.version', 'overview.stat.firmware', 'summary.flag.software_update'] });
  function drawSoft() {
    const sw = now.software || {};
    const oc = now.off_can || {};
    const ov = v(oc.version);
    const ver = sw.version || ov;
    const upd = v(oc.update_available), pct = v(oc.update_download_pct), uv = v(oc.update_version);
    const updText = upd == null ? null : v(oc.update_installing) ? `Installing ${uv || 'an update'}` : upd ? `${uv || 'An update'} waiting${pct != null ? `, ${format(pct, 'percent')} downloaded` : ''}` : 'No update waiting';
    put(tSoft, tSoft.firstChild,
      h('p', { class: 'tile-big' }, ver || 'Not known'),
      h('p', { class: 'tile-line' }, sw.version ? (VERSION_SOURCE[sw.version_source] || 'from its source') : ov ? 'as the car reports it' : 'the version is not on the CAN bus',
        sw.changed_since_ms ? `; changed ${formatDate(sw.changed_since_ms, tz, { now: api.now() })}` : '', ov && sw.version && ov !== sw.version ? `; the car reports ${ov}` : ''),
      ov ? h('p', { class: 'src-line' }, offMark(oc.version, tz)) : null,
      updText ? h('p', { class: ['tile-line', 'src-line', { 'is-warn': !!upd }] }, updText, offMark(oc.update_available, tz)) : null,
      h('a', { class: 'tile-link', href: state.buildUrl('software', { car: car.id }), cover: 'overview.link.updates' }, 'Software history'),
      h('a', { class: 'tile-link', href: `${state.buildUrl('software', { car: car.id })}#firmware` }, 'Car firmware'),
      h('a', { class: 'tile-link', href: historyHref(car.id) }, 'Module firmware history'));
  }
  // the tiles on the page (grey ones until the first answer): a later answer is written into them in place
  let driveS = ghostTile(1, 'h1'), chargeS = ghostTile(1, 'h1');
  const put1 = (el, next) => (el.classList.contains('tile-ghost') || el.nodeName !== next.nodeName ? (el.replaceWith(next), next) : morph(el, next));
  const paintLast = async () => {
    const [lastDrive, lastCharge] = await Promise.all([lastDriveP, lastChargeP]);
    if (!ctx.alive()) return;
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
    driveS = put1(driveS, tDrive); chargeS = put1(chargeS, tLastC);
  };
  const tData = tile({ w: 1, title: 'Data', icon: 'pulse', cls: 'tile-stat', cover: ['summary.data.live_push', 'summary.data.restored_values', 'nr.dash.last-update', 'summary.data.prefs_snapshot'] });
  function drawData() {
    const u = getUnits();
    put(tData, tData.firstChild,
      h('p', { class: 'tile-big' }, ago(now.at_ms)),
      h('p', { class: 'tile-line' }, now.delay_s != null ? `Reached Sentry Mate ${Math.round(now.delay_s)} s after the car sent it; updates live.` : 'Updates live as logs arrive.'),
      car.sources?.length ? h('p', { class: 'src-line' }, h('span', { class: 'hero-k' }, 'History'), srcPill(car, { short: true })) : null,
      h('p', { class: 'tile-line' }, now.stale ? 'Last known values, kept from the last log.' : 'Values the car did not send again are the last known.'),
      h('p', { class: 'tile-line' }, `Shown in ${u.distance}, °${u.temperature.toUpperCase()}, ${u.pressure}, ${rangeLabel().toLowerCase()}.`));
  }

  /* ---------------------------------------------------------------- the period: strip, summary, charts */
  const periodBtn = h('button', { type: 'button', class: 'fpill', cover: 'overview.control.time_range', onclick: (e) => openPeriod(ctx.period, tz, e.currentTarget) }, icon('calendar'), rangeText(ctx.period, tz));
  const tStrip = tile({ w: 'full', title: 'What it did', icon: 'timeline', side: periodBtn, cls: 'strip-tile', cover: ['overview.chart.states', 'overview.filter.car'] });
  const stripGhost = h('div', { class: 'strip-ghost', 'aria-hidden': 'true' });
  tStrip.append(stripGhost);
  let stripParts = null;   // what the strip tile shows under its head
  const paintStrip = async () => {
    const act = await actP;
    if (!ctx.alive()) return;
    stripGhost.remove();
    const parts = act.error ? [note('The activity did not load.')] : [strip(act, { tz, link: (s) => (s.ref?.drive ? state.buildUrl('drive', { car: car.id, drive: s.ref.drive }) : s.ref?.charge ? state.buildUrl('charge', { car: car.id, charge: s.ref.charge }) : null) }),
        h('p', { class: 'note' }, stripShares(act).slice(0, 4).map(([k, ms]) => `${({ driving: 'Driving', charging: 'Charging', online: 'Awake', online_vehicle_bus_only: 'Bus awake', asleep: 'Asleep', unknown: 'No data', updating: 'Updating' })[k] || k} ${dur(ms)}`).join(' · ')),
        h('a', { class: 'tile-link', href: state.buildUrl('timeline', { car: car.id, period: ctx.state.period }), cover: 'overview.link.states' }, 'Open the timeline')];
    if (stripParts && stripParts.length === parts.length) stripParts = stripParts.map((el, i) => morph(el, parts[i]));
    else { for (const el of stripParts || []) el.remove(); tStrip.append(...parts); stripParts = parts; }
  };
  let sumS = ghostTile(2, 'h1');
  const paintSum = async () => {
    const trip = await tripP;
    if (!ctx.alive()) return;
    const tSum = trip.error ? tile({ w: 2, title: 'This period', icon: 'trip' }, note('The summary did not load.'))
      : tile({ w: 2, title: 'This period', icon: 'trip', cls: 'sum-tile', cover: ['overview.stat.distance_logged', 'overview.stat.consumption_net', 'overview.stat.consumption_gross'] },
        h('div', { class: 'sum-nums' },
          h('div', null, h('span', { class: 'hero-k' }, 'Distance'), num(trip.distance?.drives_km, 'distance')),
          h('div', null, h('span', { class: 'hero-k' }, 'Driving use'), num(trip.wh_per_km?.net, 'efficiency', { why: 'no drive' })),
          h('div', null, h('span', { class: 'hero-k' }, 'With parked use'), num(trip.wh_per_km?.gross, 'efficiency', { why: 'no drive' }))),
        note('Driving use counts only the drives; with parked use adds what the car used while parked.'));
    sumS = put1(sumS, tSum);
  };

  const tBatt = tile({ w: 2, title: 'Battery', icon: 'battery', cls: 'chart-tile', side: h('a', { class: 'tile-link', href: state.buildUrl('battery', { car: car.id, period: ctx.state.period }), cover: 'overview.link.charge_level' }, 'Battery page'),
    cover: ['overview.chart.charge_level', 'overview.control.shared_crosshair'] });
  const tChart = tile({ w: 2, title: 'Charging in this period', icon: 'bolt', cls: 'chart-tile', side: h('a', { class: 'tile-link', href: state.buildUrl('charges', { car: car.id, period: ctx.state.period }), cover: 'overview.link.charging_stats' }, 'Charges page'),
    cover: ['overview.chart.charging_details.power', 'overview.chart.charging_details.energy', 'overview.chart.charging_details.ac_current', 'overview.chart.charging_details.ac_voltage', 'overview.chart.charging_details.battery_heater'] });

  // the tiles drawn from /now wait in grey tiles of their size; they go in together when /now is in
  const live = [[hero, 2, 'h3'], [where, 2, 'h3'], [tSpeed, 1, 'h1'], [tOut, 1, 'h1'], [tIn, 1, 'h1'], [tOdo, 1, 'h1'], [tCharge, 2, 'h3'], [tTyres, 2, 'h2'],
    [tFlags, 2, 'h3'], [tAwake, 2, 'h1'], [tSoft, 1, 'h1'], [tData, 1, 'h1']].map(([el, w, hh]) => [el, ghostTile(w, hh)]);
  const slotOf = new Map(live);
  tBatt.append(plotGhost());
  tChart.append(plotGhost());
  ctx.view.replaceChildren(slotOf.get(hero), slotOf.get(where), slotOf.get(tSpeed), slotOf.get(tOut), slotOf.get(tIn), slotOf.get(tOdo), slotOf.get(tCharge), slotOf.get(tTyres),
    slotOf.get(tFlags), slotOf.get(tAwake), slotOf.get(tSoft), driveS, chargeS, slotOf.get(tData), tStrip, sumS, tBatt, tChart,
    ...vehicleTiles(ctx, car, { onChanged: () => ctx.reload() }));

  /* the live tiles and the map: GET now (and settings for the outside link) */
  const paintNow = async () => {
    now = await nowP;
    if (!ctx.alive()) return;
    drawHero(); drawSmall(); drawTyres(); drawFlags(); drawAwake(); drawCharge(); drawSoft(); drawData();
    for (const [el, slot] of live) slot.replaceWith(el);
    ctx.events(['now'], (ev) => {
      const d = ev.data || {};
      if (d.car !== car.id || !d.now) return;
      now = mergeNow(now, d.now);
      drawHero(); drawSmall(); drawTyres(); drawFlags(); drawAwake(); drawCharge(); drawSoft(); drawData();
      hero.querySelector('.ring-arc')?.classList.add('no-anim');
    });
    /* the map: the car's spot, an outside link when the owner allows it */
    const lat = v(now.location?.lat), lon = v(now.location?.lon);
    const settings = await settingsP;
    if (!ctx.alive()) return;
    const outside = !settings.error && settings.outside_links?.maps;
    whereFoot.append(
      h('span', { title: posTitle(now.location?.lat) }, now.location?.place ? placeLink({ place: now.location.place, lat, lon }) : now.location?.address?.short || (lat != null ? coordsText(lon, lat) : 'Position not known')),
      lat != null && outside ? h('a', { class: 'tile-link', href: `https://www.openstreetmap.org/?mlat=${lat.toFixed(5)}&mlon=${lon.toFixed(5)}#map=16/${lat.toFixed(5)}/${lon.toFixed(5)}`, target: '_blank', rel: 'noopener noreferrer', cover: 'summary.map.external_link' }, 'Open in a map', icon('external'))
        : h('span', { class: 'muted', cover: 'summary.map.external_link' }, 'Outside map links are off'));
    if (lat != null && lon != null) {
      const drawn = (async () => {
        const m = await mapTile(ctx, mapBox, { center: [lon, lat], zoom: SPOT_ZOOM, fullscreen: true, lazy: true });
        if (!m) return;
        if (m.map) m.pin([lon, lat], { tone: now.state === 'charging' ? 'teal' : 'coral', title: car.name });
        await m.ready;
      })();
      // a map below the fold is made when it comes near (core/map.js), and ready does not wait for it
      if (nearNow(mapBox)) await drawn;
      else drawn.catch((err) => console.warn('map', err));
    } else {
      mapBox.append(h('p', { class: 'map-note' }, now.state === 'unknown' ? 'No recent log, so no position.' : 'The car has not sent its position yet.'));
    }
  };

  /* the charts: battery over the period, the period's charges joined; one cursor */
  const sync = `car-${car.id}-${Date.now()}`;
  const paintBattery = async () => {
    const series = await seriesP;
    if (!ctx.alive()) return;
    tBatt.querySelector('.plot-ghost')?.remove();
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
    await Promise.all(charts);
  };
  // GET now failing fails the page (as before); every other tile shows its own "did not load"
  await Promise.all([paintNow(), paintLast(), paintStrip(), paintSum(), paintBattery(), drawCharging(ctx, tChart, sync)]);
  if (!ctx.alive()) return {};
  ctx.ready();
  // a drive, charge or parked spell of the car started or ended: the period's parts read again, in place
  onRows(ctx.api, { car: car.id, waitMs: 1500, run: () => {
    if (!ctx.alive()) return;
    actP = keep(ctx.api.get(api.apiPath`cars/${car.id}/activity`, P));
    tripP = keep(ctx.api.get(api.apiPath`cars/${car.id}/trip`, P));
    lastDriveP = keep(ctx.api.page(api.apiPath`cars/${car.id}/drives`, {}, { limit: 1 }));
    lastChargeP = keep(ctx.api.page(api.apiPath`cars/${car.id}/charges`, {}, { limit: 1 }));
    Promise.all([paintLast(), paintStrip(), paintSum()]).catch(() => {});
  } });
  return {};
}

/** The period's charges (up to 8), their curves joined on one time axis with gaps between them. */
async function drawCharging(ctx, el, sync) {
  const car = ctx.car;
  let res;
  const done = () => el.querySelector('.plot-ghost')?.remove(); // the chart's grey room, until there is something to show
  try { res = await chargesRead(ctx); } catch { done(); el.append(note('The charges did not load.')); return; }
  const list = (res.items || []).slice().sort((a, b) => a.start_ms - b.start_ms);
  if (!list.length) { done(); el.append(h('p', { class: 'lede chart-empty' }, 'No charge in this period.'), note('Pick a longer period to see power, energy, current, voltage and battery heating.')); return; }
  const curves = await Promise.all(list.map((c) => curveRead(ctx, c).catch(() => null)));
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
  done();
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

/** A position's hover title: when it was recorded and the input that sent it (an API's when SMO's GPS is out of step). */
function posTitle(pair) {
  const t = pair && typeof pair === 'object' ? pair.at_ms : null;
  if (t == null) return null;
  const src = pair.sources?.[0]?.label;
  return `Position recorded ${formatRelative(t, api.now())}${src ? `; ${src}` : ''}`;
}
