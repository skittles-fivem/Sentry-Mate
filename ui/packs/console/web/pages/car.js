// Car overview (/cars/{car}): what the car is doing now and the chosen period (default 24 h).
// Live values from GET /cars/{car}/now, then merged from 'now' events (each value carries its own time).
// The period part (figures, battery chart, charging chart, the state strip, last drive and charge)
// loads again when a drive, charge or parked spell starts or ends ('rows' events).
//
//   ## now        key: value lines in two columns (state, battery, range, odometer, temperatures,
//                 doors and modes, lock, windows, sunroof, route, tyres FL FR RL RR, software and updates, data age;
//                 lock, windows, sunroof, route, version, updates and the charge schedule are not on CAN: they come from
//                 now.off_can, the Tesla API's or TeslaFi's newest reading, each marked [api] or [tfi] and [stale] when old)
//   ## where      a small flat grey map, and charging now / why the car is awake beside it
//   ## period     figures, the battery sparkline, the charging chart, the state strip, last drive and charge
//   ## vehicle    the car's build facts, each with its source and how sure (pages/vehicle.js); an admin can type over them

import { h, replace, cover } from '../shared/dom.js';
import { isSold, buildUrl } from '../shared/state.js';
import { tyreView, recommendedLine } from '../shared/tyres.js';
import { format, getUnits, onUnitsChange, unitLabel, convert } from '../shared/units.js';
import { formatTime, formatDateTime } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { head, section, kv, textBar, link, note } from '../core/ui.js';
import { num, stateWord, ago, when, dur, tag, date } from '../core/fmt.js';
import { safeMap } from '../core/map.js';
import { offstage } from '../core/stage.js';
import { chartOrNote, line, timeAxis, valueAxis, cursor, readout } from '../core/chart.js';
import { rpx } from '../core/display.js';
import { stateStrip } from '../core/strip.js';
import { src, via, inputsLine } from '../core/source.js';
import { vehicleSection } from './vehicle.js';

export const id = 'car';
export const title = () => 'Overview';

// a live value is { value, at_ms, sources }; an event may bring only a newer time for it ({ at_ms })
const val = (p) => (p && typeof p === 'object' ? ('value' in p ? p.value : null) : p ?? null);
const at = (p) => (p && typeof p === 'object' ? p.at_ms ?? null : null);

const AWAKE = {
  sentry: 'sentry mode', keep_climate_on: 'keep climate on', keep_climate_dog: 'dog mode', keep_climate_camp: 'camp mode',
  user_present: 'someone in the car', preconditioning_cabin: 'cabin preconditioning', preconditioning_battery: 'battery preconditioning',
  charging: 'charging', keep_warm: 'battery kept warm', door_open: 'a door is open', accessory_power: 'accessory power',
};
const CHARGE_STATUS = { disconnected: 'unplugged', no_power: 'plugged in, no power', about_to_charge: 'plugged in, waiting', charging: 'charging', complete: 'charge complete', stopped: 'stopped' };
const CLIMATE = { off: 'off', on: 'on', precondition: 'preconditioning', overheat_protect: 'overheat protection' };
const KEEP = { off: 'off', on: 'keep climate', dog: 'dog mode', camp: 'camp mode' };

function merge(into, delta) {
  for (const [k, v] of Object.entries(delta || {})) {
    if (v && typeof v === 'object' && !Array.isArray(v) && !('value' in v) && into[k] && typeof into[k] === 'object' && !Array.isArray(into[k])) merge(into[k], v);
    else into[k] = v;
  }
  return into;
}

export async function mount(ctx) {
  const { tz } = ctx;
  let car = ctx.car;
  const cid = car.id;
  const enc = encodeURIComponent(cid);
  let now, settings;
  try {
    [now, settings] = await Promise.all([ctx.api.get(`cars/${enc}/now`), ctx.api.get('settings', null, { maxAgeMs: 300000 }).catch(() => null)]);
  } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;

  const hd = head(ctx, { title: h('span', { cover: 'summary.header.name' }, car.name || `unnamed ${car.model || 'car'}`),
    extra: isSold(car) ? h('span', { class: 'muted ph-extra', title: 'Sold: its history stays; it is left out of the fleet list' }, tag('sold', 'muted'), car.sold_date ? ` on ${car.sold_date}` : '') : null });
  const btns = hd.querySelectorAll('.ph-btn');
  if (btns[0]) cover(btns[0], 'overview.control.time_range');
  if (btns[1]) cover(btns[1], 'overview.filter.car');
  const sub = h('p', { class: 'subline' });
  const inputsEl = h('div', { class: 'src-car' });
  const staleLine = h('p', { class: 'staleline', role: 'status', cover: ['summary.state.drive_offline', 'summary.data.restored_values'] });
  const nowBlock = h('div');
  const mapEl = h('div', { class: 'map map--sm', role: 'img', 'aria-label': `Map: where ${car.name} is` });
  const mapTools = h('div', { class: 'map-tools' });
  const chargeBlock = h('div');
  const whyBlock = h('div');
  const periodBlock = h('div');
  const vehicle = vehicleSection(ctx, { getCar: () => car, setCar: (c) => { car = { ...car, ...c }; paintSub(); } });

  replace(ctx.view, h('div', { class: 'page', dataset: { page: 'car' }, cover: ['shots.web-interface', 'shots.overview'] },
    hd, sub, inputsEl, staleLine, nowBlock,
    h('div', { class: 'cols2' },
      section({ title: 'where', cover: ['summary.map.position', 'summary.map.attribution', 'summary.map.zoom'] }, mapEl, mapTools),
      h('div', null, chargeBlock, whyBlock)),
    periodBlock, vehicle.el));

  /* ---------- the lines at the top */
  function paintSub() {
    const n = now, trim = car.config?.trim;
    const trimSrc = { can: 'from SMO', smo: 'from SMO', vin: 'from the VIN', owner: 'typed by the owner', teslamate: 'imported', tesla_api: 'from the Tesla API' }[trim?.source];
    replace(sub,
      h('span', { cover: 'summary.stat.status' }, stateWord(n.state, { stale: n.stale }), ' since ', n.since_ms ? when(n.since_ms, tz) : '—',
        n.since_ms ? h('span', { class: 'muted' }, ` (${dur(Math.max(0, ctx.api.now() - n.since_ms))})`) : null),
      h('span', { class: 'sep', 'aria-hidden': 'true' }, '\u00a0| '),
      h('span', { cover: 'summary.header.model', title: 'Model and year come from the VIN prefix' }, [car.model, car.year].filter(Boolean).join(' ') || 'model not known yet'),
      h('span', { class: 'sep', 'aria-hidden': 'true' }, '\u00a0| '),
      h('span', { cover: 'summary.header.trim' }, trim?.value ? [trim.value, trimSrc ? h('span', { class: 'muted' }, ` (${trimSrc})`) : null] : h('span', { class: 'muted' }, 'trim not known yet')),
      car.vin_prefix ? [h('span', { class: 'sep', 'aria-hidden': 'true' }, '\u00a0| '), h('span', { class: 'muted', title: 'Only the VIN prefix is stored' }, 'vin '), car.vin_prefix] : null);
    // the live inputs (SMO, the Tesla API) with each one's newest value, and every input its history has
    const hist = src(car, { full: true });
    const inputs = inputsLine(n.inputs);
    // admins change the inputs' priority in the car's settings (data sources)
    if (ctx.admin) inputs.append(h('span', { class: 'src-in' }, link(`${buildUrl('settings-cars', { car: car.id })}#data-sources`, 'change order', { class: 'lnk lnk--tap' })));
    replace(inputsEl, inputs, hist ? h('p', { class: 'src-line src-line--car' }, h('span', { class: 'src-inputs-k' }, 'history '), hist) : null);
    const midDrive = n.state === 'driving' && n.stale;
    // no data: no sample at all (an unknown state with fresh values is not "no data")
    const none = !n.at_ms;
    const show = n.stale || none;
    staleLine.hidden = !show;
    replace(staleLine, tag(none ? 'offline' : 'stale', 'warn'), ' ',
      midDrive ? 'data stopped during a drive; it is finished when later logs arrive. ' : none ? 'no data for now. ' : 'old data. ',
      `newest values from ${n.at_ms ? formatDateTime(n.at_ms, tz) : 'a while ago'} (${n.at_ms ? ago(n.at_ms) : '—'}); below are the last known values.`);
  }

  /* ---------- ## now */
  // a value older than the snapshot shows its age; one the Tesla API sent says [api] after it
  const aged = (pair, content) => {
    const t = at(pair);
    const old = t && now.at_ms && now.at_ms - t > 10 * 60000;
    return [content, old ? h('span', { class: 'muted' }, ` (${ago(t)})`) : null, via(pair)];
  };
  const by = (pair, content) => (content == null ? null : [content, via(pair)]);
  // a value from now.off_can ({ value, at_ms, sources, stale } or null): its input's mark, [stale] past its age limit
  const oc = () => now.off_can || {};
  const offMark = (p) => (p ? [via(p), p.stale ? h('span', { class: 'src-stale', title: p.at_ms ? `newest ${formatDateTime(p.at_ms, tz)}; past its age limit` : null }, tag('stale', 'warn'), h('span', { class: 'muted' }, p.at_ms ? ` ${ago(p.at_ms)}` : '')) : null] : null);
  const off = (p, content) => (val(p) == null || content == null ? null : [p.stale ? h('span', { class: 'muted' }, content) : content, offMark(p)]);
  const WINDOWS = [['driver_front', 'driver front'], ['passenger_front', 'passenger front'], ['driver_rear', 'driver rear'], ['passenger_rear', 'passenger rear']];
  const yes = (pair, on = 'open', off = 'closed', warn = true) => { const v = val(pair); return v == null ? null : v ? (warn ? h('span', { class: 'tone--warn' }, on) : on) : off; };
  function paintNow() {
    const n = now, f = n.flags || {}, c = n.charge || {}, loc = n.location || {};
    const ideal = getUnits().range === 'ideal';
    const soc = val(n.soc_pct), usable = val(n.usable_soc_pct);
    const rangePair = ideal ? n.ideal_range_km : n.range_km;
    const setp = (p) => { const v = val(p); return v == null ? null : format(v, 'temperature'); };
    const tv = tyreView(n);
    const tyres = tv.kpa;
    const tyre = (i) => h('span', { class: ['tyre', tv.low[i] === true && 'tone--warn'] }, num(tyres[i], 'pressure'));
    const doors = f.doors || {};
    const openOnes = [['fl', 'fl'], ['fr', 'fr'], ['rl', 'rl'], ['rr', 'rr']].filter(([k]) => val(doors[k])).map(([, l]) => `door ${l}`)
      .concat(val(f.trunk) ? ['trunk'] : [], val(f.frunk) ? ['frunk'] : [], val(f.liftgate_deg) ? ['liftgate'] : []);
    const doorsKnown = ['fl', 'fr', 'rl', 'rr'].some((k) => val(doors[k]) != null);
    const sw = n.software || {};
    const swSrc = { teslamate: 'imported', tesla_api: 'from the Tesla API', owner: 'typed by the owner', build_match: 'matched by module builds' }[sw.version_source];
    const o = oc();
    const winOpen = WINDOWS.filter(([k]) => val(o.windows?.[k])).map(([, w]) => w);
    const ov = val(o.version);
    const upd = val(o.update_available);
    const updText = upd == null ? null : val(o.update_installing) ? h('span', { class: 'tone--warn' }, `installing ${val(o.update_version) || 'an update'}`)
      : upd ? [h('span', { class: 'tone--warn' }, `${val(o.update_version) || 'an update'} waiting`), val(o.update_download_pct) != null ? h('span', { class: 'muted' }, `, ${format(val(o.update_download_pct), 'percent')} downloaded`) : null]
      : 'none waiting';
    const roofPct = val(o.sunroof_pct);
    const roofText = val(o.sunroof_state) != null ? [String(val(o.sunroof_state)).replace(/_/g, ' '), roofPct != null ? h('span', { class: 'muted' }, ` ${format(roofPct, 'percent')} open`) : null] : roofPct != null ? `${format(roofPct, 'percent')} open` : null;
    const r = o.route || {};
    const routeText = val(r.destination) == null && val(r.km) == null ? null
      : [val(r.destination) || 'a destination', val(r.km) != null ? h('span', { class: 'muted' }, `, ${format(val(r.km), 'distance')}`) : null,
        val(r.minutes) != null ? h('span', { class: 'muted' }, `, ${dur(val(r.minutes) * 60000)}`) : null,
        val(r.traffic_min) ? h('span', { class: 'tone--warn' }, ` +${dur(val(r.traffic_min) * 60000)} traffic`) : null];
    const routeP = r.destination || r.km || r.minutes;
    const band = soc == null ? null : soc < 20 ? 'low' : soc > 80 ? 'high' : 'normal';
    replace(nowBlock, section({ title: 'now', cover: 'nr.dash.panel.car-status', side: h('span', { class: 'muted', cover: ['nr.dash.last-update', 'summary.data.live_push'], title: n.at_ms ? `recorded ${formatDateTime(n.at_ms, tz)}` : null }, 'newest ', n.at_ms ? ago(n.at_ms) : '—', ', updates by itself') },
      kv([
        ['state', [stateWord(n.state, { stale: n.stale }), val(n.gear) && n.state === 'driving' ? h('span', { class: 'muted' }, ` gear ${val(n.gear)}`) : null], { cover: 'summary.stat.status' }],
        ['speed', aged(n.speed_kph, num(val(n.speed_kph), 'speed')), { cover: 'summary.stat.speed' }],
        ['battery', aged(n.soc_pct, h('span', { class: 'soc-cell' }, textBar(soc, { cells: 20, tone: band === 'low' ? 'bad' : n.stale ? 'text' : 'accent' }), num(soc, 'percent'),
          band ? h('span', { class: 'muted', title: 'bands: under 20 %, 20 to 80 %, over 80 %' }, ` ${band}`) : null)),
        { cover: ['overview.stat.battery_level', 'nr.dash.battery-level', 'nr.dash.battery-level.bands', 'summary.stat.soc'] }],
        ['usable', by(n.usable_soc_pct, usable != null && soc != null && Math.abs(usable - soc) >= 0.5 ? h('span', { class: 'tone--warn' }, num(usable, 'percent')) : num(usable, 'percent'))],
        [`${ideal ? 'ideal' : 'rated'} range`, aged(rangePair, num(val(rangePair), 'range')), { cover: ['summary.stat.range_preferred', 'overview.stat.range', 'nr.dash.ideal-range'] }],
        ['est. range', [num(val(n.est_range_km), 'range'), via(n.est_range_km), h('span', { class: 'muted' }, n.est_basis === 'wh_per_km_30d' ? ' at 30 d use' : ' at recent use')], { cover: ['summary.stat.range_estimated', 'nr.dash.est-range'] }],
        ['in the pack', aged(n.energy_remaining_kwh, num(val(n.energy_remaining_kwh), 'energy'))],
        ['odometer', by(n.odometer_km, h('a', { class: 'lnk-plain', href: ctx.link('statistics'), title: 'distance over time on statistics' }, num(val(n.odometer_km), 'odometer'))), { cover: ['summary.stat.odometer', 'overview.stat.odometer', 'overview.link.mileage'] }],
        ['outside', aged(n.outside_c, num(val(n.outside_c), 'temperature')), { cover: ['summary.stat.outside_temp', 'overview.stat.outside_temp'] }],
        ['inside', aged(n.cabin_c, val(n.cabin_c) == null ? h('span', { class: 'muted' }, 'not sent by this car') : num(val(n.cabin_c), 'temperature')), { cover: ['summary.stat.inside_temp', 'overview.stat.inside_temp'] }],
        ['climate set', setp(n.setpoint_left_c) == null && setp(n.setpoint_right_c) == null ? h('span', { class: 'muted' }, val(f.climate) === 'off' ? 'none while climate is off' : 'not sent')
          : [setp(n.setpoint_left_c) ?? '—', h('span', { class: 'muted' }, ' left  '), setp(n.setpoint_right_c) ?? '—', h('span', { class: 'muted' }, ' right')], { cover: 'overview.stat.driver_temp' }],
        ['battery temp', aged(n.battery_c, num(val(n.battery_c), 'temperature'))],
        ['climate', by(f.climate, CLIMATE[val(f.climate)] || null), { cover: 'summary.flag.preconditioning' }],
        ['keep climate', by(f.keep_climate, KEEP[val(f.keep_climate)] || null), { cover: 'summary.flag.dog_mode' }],
        ['locked', off(o.locked, val(o.locked) ? 'locked' : h('span', { class: 'tone--warn' }, 'unlocked')), { cover: 'summary.flag.lock' }],
        ['windows', off(o.windows_open, val(o.windows_open) ? h('span', { class: 'tone--warn' }, winOpen.length ? `open: ${winOpen.join(', ')}` : 'open') : 'all closed'), { cover: 'summary.flag.windows_open' }],
        roofText ? ['sunroof', off(o.sunroof_state || o.sunroof_pct, roofText)] : null,
        routeText ? ['route', off(routeP, ['to ', routeText])] : null,
        ['doors', !doorsKnown ? null : openOnes.length ? h('span', { class: 'tone--warn' }, `open: ${openOnes.join(', ')}`) : 'all closed', { cover: 'summary.flag.doors_open' }],
        ['sentry', by(f.sentry, yes(f.sentry, 'on', 'off')), { cover: 'summary.flag.sentry_mode' }],
        ['sentry event', val(f.sentry_event_guess) ? h('span', { class: 'tone--warn' }, 'likely (a guess)') : 'none seen', { cover: 'summary.flag.sentry_recording' }],
        ['someone inside', val(f.user_present) == null ? null : by(f.user_present, val(f.user_present) ? 'yes' : 'no'), { cover: 'summary.flag.driver_present' }],
        ['cable', by(c.plugged, yes(c.plugged, 'plugged in', 'unplugged')), { cover: 'summary.flag.plugged_in' }],
        ['cold battery', val(f.reduced_range) == null ? null : val(f.reduced_range) ? h('span', { class: 'tone--warn' }, 'range reduced') : 'no', { cover: 'summary.flag.reduced_range' }],
        ['service mode', val(f.service_mode) == null ? null : val(f.service_mode) ? h('span', { class: 'tone--warn' }, loc.place ? `on, at ${loc.place.name}` : 'on') : 'off', { cover: ['summary.flag.service_mode', 'summary.state.in_service'] }],
        [tv.mapped ? 'tyres fl fr' : 'tyre sensors 1 2', [tyre(0), '  ', tyre(1)], { cover: 'summary.flag.tire_pressure_warning' }],
        [tv.mapped ? 'tyres rl rr' : 'tyre sensors 3 4', [tyre(2), '  ', tyre(3)]],
        ['tyre pressure', h('span', { class: 'muted' }, recommendedLine(n))],
        ['12 v battery', aged(n.lv_v, num(val(n.lv_v), 'voltage'))],
        ['software', [h('a', { class: 'lnk-plain', href: ctx.link('software') }, sw.version || ov || 'not known'), sw.version && swSrc ? h('span', { class: 'muted' }, ` (${swSrc})`) : null,
          ov != null && sw.version && ov !== sw.version ? h('span', { class: 'muted' }, `; the car reports ${ov}`) : null, ov != null ? offMark(o.version) : null], { cover: ['summary.stat.version', 'overview.stat.firmware', 'overview.link.updates'] }],
        ['update', off(o.update_available, updText)],
        ['firmware', [h('a', { class: 'lnk-plain', href: `${ctx.link('software')}#firmware`, title: 'the modules\' firmware fingerprints, and the Tesla updates found from them' }, 'module fingerprints'), h('span', { class: 'muted' }, ', '), h('a', { class: 'lnk-plain', href: `${ctx.link('software')}#firmware-history`, title: 'every fingerprint each module has had, and the versions over time' }, 'history')]],
        ['update seen', sw.changed_since_ms ? h('span', { class: 'tone--warn' }, `builds changed ${when(sw.changed_since_ms, tz)}`) : h('span', { class: 'muted' }, 'no change seen'), { cover: 'summary.flag.software_update' }],
        ['log received', n.received_ms ? ago(n.received_ms) : null],
      ]),
      note(h('span', { class: 'src' }, h('span', { class: 'src-b' }, '[', h('span', { class: 'src-k src-k--tesla_api' }, 'api'), ']')), ' or ', h('span', { class: 'src' }, h('span', { class: 'src-b' }, '[', h('span', { class: 'src-k src-k--teslafi_api' }, 'tfi'), ']')),
        ' after a value: the Tesla API or TeslaFi sent it; the rest came from SMO. ', tag('stale', 'warn'), ' that input has not sent it again within its age limit. ', `Logs arrive about ${n.delay_s ? (n.delay_s < 120 ? `${Math.round(n.delay_s)} s` : dur(n.delay_s * 1000)) : '75 s'} after the car records them; a value older than that shows its own age. Values in your units (${unitLabel('distance')}, ${unitLabel('temperature')}, ${unitLabel('pressure')}). `,
        h('a', { class: 'lnk', href: ctx.link('health'), cover: 'summary.data.prefs_snapshot' }, 'every signal'))));
  }

  /* ---------- charging now, why awake */
  function paintCharge() {
    const n = now, chg = n.charge || {};
    const charging = n.state === 'charging';
    const plugged = val(chg.plugged);
    const not = (why) => h('span', { class: 'muted' }, why);
    replace(chargeBlock, section({ title: 'charging now', cover: ['nr.dash.panel.charge-status', 'summary.stat.scheduled_charging'],
      side: chg.charge_id ? link(ctx.link('charge', { charge: chg.charge_id }), 'open') : link(ctx.link('charges'), 'charges') },
    kv([
      ['status', [CHARGE_STATUS[val(chg.status)] || (plugged ? 'plugged in' : 'not plugged in'), val(chg.kind) ? h('span', { class: 'muted' }, ` ${String(val(chg.kind)).toUpperCase()}`) : null]],
      ['power', charging ? by(chg.power_kw, num(val(chg.power_kw), 'power')) : not('not charging'), { cover: ['summary.stat.charger_power', 'nr.dash.charging-power', 'overview.stat.charging_power'] }],
      ['volts', charging ? num(val(chg.volts_v), 'voltage') : not('not charging'), { cover: 'overview.stat.ac_voltage' }],
      ['amps', charging ? [num(val(chg.amps_a), 'current'), val(chg.phases) ? h('span', { class: 'muted' }, ` ${val(chg.phases)} ph`) : null] : not('not charging'), { cover: 'nr.dash.charging-current' }],
      ['added', val(chg.added_kwh) != null ? by(chg.added_kwh, num(val(chg.added_kwh), 'energy')) : not('no charge under way'), { cover: 'summary.stat.energy_added' }],
      ['limit', val(chg.limit_pct) != null ? by(chg.limit_pct, num(val(chg.limit_pct), 'percent')) : not(plugged ? 'not sent yet' : 'not plugged in'), { cover: ['summary.stat.charge_limit', 'nr.dash.charge-limit'] }],
      ['to full', val(chg.to_full_min) != null ? by(chg.to_full_min, dur(val(chg.to_full_min) * 60000)) : not(charging ? 'not known yet' : 'no charge under way'), { cover: ['summary.stat.remaining_time', 'nr.dash.time-to-full'] }],
      ['done at', chg.finish_ms ? when(chg.finish_ms, tz) : not(charging ? 'not known yet' : 'no charge under way'), { cover: 'summary.stat.expected_finish' }],
      ['scheduled', off(n.off_can?.scheduled_charge_start_ms, when(val(n.off_can?.scheduled_charge_start_ms), tz))],
      ['departure', off(n.off_can?.scheduled_departure_ms, when(val(n.off_can?.scheduled_departure_ms), tz))],
    ], { cols: 1 })));
    const buses = Object.fromEntries((n.buses || []).map((b) => [b.bus, b.awake]));
    const reasons = (n.awake_reasons || []).filter((r) => AWAKE[r]);
    const text = n.state === 'asleep' ? 'asleep: both buses are quiet' : n.state === 'driving' ? 'driving' : n.state === 'unknown' ? 'not known: no log for now'
      : buses.vehicle && !buses.chassis ? 'kept awake: only the vehicle bus is awake' : 'awake';
    replace(whyBlock, section({ title: 'why awake', cover: 'web.settings.car.sleep-blockers' },
      kv([['now', text], ...reasons.map((r) => ['reason', AWAKE[r]]), reasons.length || n.state !== 'parked' ? null : ['reason', h('span', { class: 'muted' }, 'none reported; it should sleep soon')]], { cols: 1 }),
      note('Read from the car’s own messages. Sentry Mate only listens, so it never keeps the car awake.')));
  }

  /* ---------- the map */
  let map = null, pin = null;
  function paintTools() {
    const loc = now.location || {};
    const lat = val(loc.lat), lon = val(loc.lon);
    const outside = !!settings?.outside_links?.maps;
    const where = loc.place?.name || loc.address?.short || (now.state === 'driving' ? 'on the road' : lat != null ? 'not a saved place' : 'no position yet');
    replace(mapTools,
      h('span', null, h('span', { class: 'muted' }, 'at '), where, val(loc.accuracy_m) != null ? h('span', { class: 'muted' }, ` ±${format(val(loc.accuracy_m), 'length')}`) : null),
      h('button', { class: 'btn btn--quiet btn--line', type: 'button', cover: 'summary.map.fullscreen', onclick: () => (document.fullscreenElement ? document.exitFullscreen?.() : mapEl.requestFullscreen?.().catch(() => {})) }, document.fullscreenElement ? '[exit full screen]' : '[full screen]'),
      outside && lat != null
        ? h('a', { class: 'lnk', href: `https://www.openstreetmap.org/?mlat=${lat.toFixed(5)}&mlon=${lon.toFixed(5)}#map=17/${lat.toFixed(5)}/${lon.toFixed(5)}`, target: '_blank', rel: 'noopener noreferrer external', cover: 'summary.map.external_link' }, 'openstreetmap')
        : h('span', { class: 'muted', cover: 'summary.map.external_link', title: 'An admin can turn outside map links on in settings / display' }, 'outside map links off'));
  }
  ctx.on(document, 'fullscreenchange', () => { paintTools(); map?.map.resize(); });
  async function mountMap() {
    const loc = now.location || {};
    const lat = val(loc.lat), lon = val(loc.lon);
    if (lat == null || lon == null) { mapEl.append(h('p', { class: 'map-note' }, '-- no position recorded yet --')); return; }
    map = await safeMap(mapEl, ctx, { center: [lon, lat], zoom: now.state === 'driving' ? 13 : 15 });
    if (!map || !ctx.alive()) return;
    placePin();
  }
  function placePin() {
    if (!map) return;
    const loc = now.location || {};
    const lat = val(loc.lat), lon = val(loc.lon);
    if (lat == null) return;
    pin?.remove();
    const tone = now.stale || now.state === 'unknown' || now.state === 'asleep' ? ' map-pin--muted' : now.state === 'charging' ? ' map-pin--warn' : '';
    pin = map.marker([lon, lat], `map-pin map-pin--car${tone}`);
    map.frame(() => map.map.jumpTo({ center: [lon, lat] }));
  }

  /* ---------- ## period */
  const sync = `car-${cid}`;
  async function paintPeriod() {
    const p = ctx.period;
    const from = p.from_ms ?? (ctx.api.now() - 86400000), to = p.to_ms ?? ctx.api.now();
    const rk = getUnits().range === 'ideal' ? 'ideal_range_km' : 'range_km';
    const qs = { from_ms: from, to_ms: to };
    const [stats, series, act, charges, lastDrive, lastCharge] = await Promise.all([
      ctx.api.get(`cars/${enc}/statistics`, { period: 'all', ...qs }).catch(() => null),
      ctx.api.get(`cars/${enc}/series`, { fields: `soc_pct,${rk}`, ...qs, max_points: 480 }),
      ctx.api.get(`cars/${enc}/activity`, qs),
      ctx.api.page(`cars/${enc}/charges`, { from_ms: from - 12 * 3600000, to_ms: to }, { limit: 10 }).catch(() => ({ items: [] })),
      ctx.api.page(`cars/${enc}/drives`, {}, { limit: 1 }).catch(() => ({ items: [] })),
      ctx.api.page(`cars/${enc}/charges`, {}, { limit: 1 }).catch(() => ({ items: [] })),
    ]);
    if (!ctx.alive()) return;
    const row = stats?.items?.[0] || {};
    const rec = row.completeness?.recorded_pct;
    const blank = !stats || !stats.items?.length ? 'could not be loaded' : rec === 0 ? 'no data for this period' : null;
    const fig = (v, kind) => (blank ? h('span', { class: 'muted' }, blank) : num(v, kind));

    const battEl = h('div', { class: 'chart chart--pane' });
    const battRead = readout('move over the chart for values');
    const chgEl = h('div', { class: 'chart chart--pane' });
    const supEl = h('div', { class: 'chart chart--pane' });
    const chgRead = readout('');
    const keyBtn = (label, n, plotRef, idx) => {
      const b = h('button', { class: `chart-key-i chart-key-i--${n} btn--line`, type: 'button', 'aria-pressed': 'true', title: 'show or hide this line' }, `${['──', '┄┄', '┈┈'][n - 1]} ${label}`);
      b.addEventListener('click', () => { const on = b.getAttribute('aria-pressed') !== 'true'; b.setAttribute('aria-pressed', String(on)); plotRef()?.u?.setSeries(idx, { show: on }); });
      return b;
    };
    let battPlot = null, chgPlot = null, supPlot = null;
    const lastD = lastDrive.items?.[0], lastC = lastCharge.items?.[0];
    const placeOf = (x) => x?.place?.name || x?.label || x?.address?.short || '—';

    // drawn off screen and swapped in whole, charts included: a live update never empties the period first
    await offstage(periodBlock, async (slot) => {
    replace(slot,
      section({ title: 'period', cover: 'overview.control.refresh', side: h('span', { class: 'muted' }, 'reloads when a drive, charge or parked spell starts or ends') },
        h('div', { class: 'stats' },
          h('span', { class: 'stat', cover: 'overview.stat.distance_logged' }, h('span', { class: 'stat-k' }, 'distance'), fig(row.distance_logged_km, 'distance'), blank ? null : h('span', { class: 'muted' }, ` ${formatNumber(row.drives ?? 0)} drives`)),
          h('span', { class: 'stat', cover: 'overview.stat.consumption_net' }, h('span', { class: 'stat-k' }, 'net'), fig(row.net_wh_per_km, 'efficiency')),
          h('span', { class: 'stat', cover: 'overview.stat.consumption_gross' }, h('span', { class: 'stat-k' }, 'gross'), fig(row.gross_wh_per_km, 'efficiency')),
          h('span', { class: 'stat' }, h('span', { class: 'stat-k' }, 'added'), fig(row.energy_added_kwh, 'energy'), blank ? null : h('span', { class: 'muted' }, ` ${formatNumber(row.charges ?? 0)} charges`)),
          rec != null && rec < 100 ? h('span', { class: 'stat' }, h('span', { class: 'stat-k' }, 'recorded'), num(rec, 'percent')) : null),
        h('div', { class: 'lines' },
          h('div', { class: 'ln ln--last', dataset: { row: '', href: lastD ? ctx.link('drive', { drive: lastD.id }) : null } },
            h('span', { class: 'ln-t' }, lastD ? formatTime(lastD.start_ms, tz) : ''), h('span', { class: 'ln-tag' }, tag('drive', 'drive')),
            h('span', { class: 'ln-what' }, lastD ? [h('span', { class: 'muted' }, 'last drive '), h('a', { class: 'lnk-plain', href: ctx.link('drive', { drive: lastD.id }) }, `${date(lastD.start_ms, tz)}  ${placeOf(lastD.start)} -> ${placeOf(lastD.end)}`)] : h('span', { class: 'muted' }, '-- no drive yet --')),
            h('span', { class: 'ln-num' }, lastD ? [num(lastD.distance_km, 'distance'), ' ', src(lastD)] : null)),
          h('div', { class: 'ln ln--last', dataset: { row: '', href: lastC ? ctx.link('charge', { charge: lastC.id }) : null } },
            h('span', { class: 'ln-t' }, lastC ? formatTime(lastC.start_ms, tz) : ''), h('span', { class: 'ln-tag' }, tag('charge', 'charge')),
            h('span', { class: 'ln-what' }, lastC ? [h('span', { class: 'muted' }, 'last charge '), h('a', { class: 'lnk-plain', href: ctx.link('charge', { charge: lastC.id }) }, `${date(lastC.start_ms, tz)}  ${lastC.place?.name || lastC.address?.short || '—'}`)] : h('span', { class: 'muted' }, '-- no charge yet --')),
            h('span', { class: 'ln-num' }, lastC ? [num(lastC.energy_added_kwh, 'energy'), ' ', src(lastC)] : null)))),
      section({ title: 'battery', cover: ['overview.chart.charge_level', 'overview.control.shared_crosshair', 'overview.chart.legend_toggle'],
        side: [src(series), ' ', link(ctx.link('battery'), 'battery', { cover: 'overview.link.charge_level' })] },
      h('div', { class: 'chart-head' }, h('span', { class: 'chart-key' }, keyBtn('soc %', 1, () => battPlot, 1), keyBtn(`${rk === 'ideal_range_km' ? 'ideal' : 'rated'} range ${unitLabel('range')}`, 2, () => battPlot, 2))),
      battEl, battRead.el),
      section({ title: 'charging', cover: ['overview.chart.charging_details.power', 'overview.chart.charging_details.energy', 'overview.chart.charging_details.ac_current', 'overview.chart.charging_details.ac_voltage', 'overview.chart.charging_details.battery_heater'],
        side: link(ctx.link('charges'), 'charges', { cover: 'overview.link.charging_stats' }) },
      h('div', { class: 'chart-head' }, h('span', { class: 'chart-key' }, keyBtn('power kW', 1, () => chgPlot, 1), keyBtn('added kWh', 2, () => chgPlot, 2), keyBtn('heater kW', 3, () => chgPlot, 3))),
      chgEl,
      h('div', { class: 'chart-head' }, h('span', { class: 'chart-key' }, keyBtn('volts V', 1, () => supPlot, 1), keyBtn('amps A', 2, () => supPlot, 2))),
      supEl, chgRead.el),
      section({ title: 'states', cover: 'overview.chart.states', side: link(ctx.link('timeline'), 'timeline', { cover: 'overview.link.states' }) },
        stateStrip(ctx, act.items || [], from, to, tz)));

    // battery: soc and range over the period; asleep keeps the line going, unknown breaks it
    const xs = series.t_ms;
    const carry = (a) => { let last = null; return (a || []).map((v, i) => { if (v != null) { last = v; return v; } return series.state?.[i] === 'asleep' ? last : (last = null); }); };
    const soc = carry(series.values.soc_pct);
    const range = carry(series.values[rk]).map((v) => (v == null ? null : convert(v, 'range')));
    battPlot = await chartOrNote(battEl, ctx, () => ({
      padding: [rpx(8), 0, 0, 0],
      cursor: cursor(sync),
      scales: { x: { time: true, min: from, max: to }, pct: { range: [0, 100] }, dist: { range: (u, a, b) => [0, Math.max(10, (b || 0) * 1.1)] } },
      axes: [timeAxis(tz), valueAxis((v) => `${formatNumber(v)}%`, { scale: 'pct', size: 48 }), valueAxis((v) => formatNumber(v), { scale: 'dist', side: 1, grid: false, size: 48 })],
      series: [{}, line('soc', 0, { scale: 'pct' }), line('range', 1, { scale: 'dist' })],
    }), [xs, soc, range], { onCursor: (i) => battRead.show(i == null ? null : [['at', formatTime(xs[i], tz)], ['soc', format(soc[i], 'percent')], ['range', format(range[i] == null ? null : series.values[rk][i], 'range')]]) });
    if (!ctx.alive()) return;

    // charging: the curves of the charges in the period, on one time axis, broken between charges
    const sorted = [...(charges.items || [])].filter((c) => (c.end_ms ?? to) > from).sort((a, b) => a.start_ms - b.start_ms);
    const curves = await Promise.all(sorted.map((c) => ctx.api.get(`cars/${enc}/charges/${encodeURIComponent(c.id)}/curve`, { fields: 'power_kw,volts_v,amps_a,heater_kw,dc_volts_v,dc_amps_a', max_points: 300 }).catch(() => null)));
    if (!ctx.alive()) return;
    const T = [], P = [], E = [], HT = [], V = [], A = [];
    curves.forEach((cv, k) => {
      if (!cv?.t_ms?.length) return;
      const dc = cv.kind === 'dc';
      let raw = 0;
      cv.t_ms.forEach((t, i) => { if (i) raw += ((cv.values.power_kw[i - 1] || 0) * (t - cv.t_ms[i - 1])) / 3600000; });
      const own = sorted[k].energy_added_kwh;
      const scale = own != null && raw > 0 ? own / raw : 1;
      let kwh = 0;
      cv.t_ms.forEach((t, i) => {
        if (i) kwh += (((cv.values.power_kw[i - 1] || 0) * (t - cv.t_ms[i - 1])) / 3600000) * scale;
        if (t < from || t > to) return;
        T.push(t); P.push(cv.values.power_kw[i]); E.push(kwh); HT.push(cv.values.heater_kw?.[i] ?? null);
        V.push(dc ? cv.values.dc_volts_v?.[i] ?? null : cv.values.volts_v?.[i] ?? null); A.push(dc ? cv.values.dc_amps_a?.[i] ?? null : cv.values.amps_a?.[i] ?? null);
      });
      const last = T[T.length - 1];
      if (last != null) { T.push(last + 1); P.push(null); E.push(null); HT.push(null); V.push(null); A.push(null); }
    });
    const cx = T.length ? T : [from, to];
    const none = (a) => (T.length ? a : [null, null]);
    chgRead.show(null);
    if (!T.length) chgRead.el.replaceChildren(h('span', { class: 'muted' }, '-- no charging in this period --'));
    const chgCursor = (i) => { if (!T.length) return; chgRead.show(i == null ? null : [['at', formatTime(cx[i], tz)], ['power', format(P[i], 'power')], ['added', format(E[i], 'energy')], ['heater', format(HT[i], 'power')], ['volts', format(V[i], 'voltage')], ['amps', format(A[i], 'current')]]); };
    chgPlot = await chartOrNote(chgEl, ctx, () => ({
      padding: [rpx(8), 0, rpx(8), 0], cursor: cursor(sync),
      scales: { x: { time: true, min: from, max: to }, kw: { range: (u, a, b) => [0, Math.max(5, (b || 0) * 1.1)] }, kwh: { range: (u, a, b) => [0, Math.max(1, (b || 0) * 1.1)] } },
      axes: [timeAxis(tz, { show: false }), valueAxis((v) => formatNumber(v), { scale: 'kw', size: 48 }), valueAxis((v) => formatNumber(v), { scale: 'kwh', side: 1, grid: false, size: 48 })],
      series: [{}, line('power', 0, { scale: 'kw' }), line('added', 1, { scale: 'kwh' }), line('heater', 2, { scale: 'kw' })],
    }), [cx, none(P), none(E), none(HT)], { onCursor: chgCursor });
    if (!ctx.alive()) return;
    supPlot = await chartOrNote(supEl, ctx, () => ({
      padding: [rpx(8), 0, 0, 0], cursor: cursor(sync),
      scales: { x: { time: true, min: from, max: to }, v: { auto: true }, a: { range: (u, a, b) => [0, Math.max(5, (b || 0) * 1.1)] } },
      axes: [timeAxis(tz), valueAxis((v) => formatNumber(v), { scale: 'v', size: 48 }), valueAxis((v) => formatNumber(v), { scale: 'a', side: 1, grid: false, size: 48 })],
      series: [{}, line('volts', 0, { scale: 'v' }), line('amps', 1, { scale: 'a' })],
    }), [cx, none(V), none(A)], { onCursor: chgCursor });
    });
  }

  paintSub(); paintNow(); paintCharge(); paintTools();
  try { await paintPeriod(); } catch (e) {
    if (!ctx.alive()) return;
    replace(periodBlock, section({ title: 'period' }, h('p', { class: 'note' }, `-- the period could not be loaded: ${e.message} --`)));
  }
  if (!ctx.alive()) return;
  await mountMap();
  if (!ctx.alive()) return;
  ctx.ready();

  let pend = 0;
  ctx.events('now', (ev) => {
    if (ev.data?.car !== cid || !ev.data.now) return;
    merge(now, ev.data.now);
    if (pend) return;
    pend = setTimeout(() => { pend = 0; if (!ctx.alive()) return; paintSub(); paintNow(); paintCharge(); paintTools(); placePin(); }, 2000);
  });
  let rowsT = 0;
  ctx.events('rows', (ev) => {
    if (ev.data?.car !== cid) return;
    clearTimeout(rowsT);
    rowsT = setTimeout(() => { if (ctx.alive()) paintPeriod().catch(() => {}); }, 1500);
  });
  ctx.onCleanup(() => { clearTimeout(pend); clearTimeout(rowsT); });
  ctx.onCleanup(onUnitsChange(() => { paintNow(); paintCharge(); paintPeriod().catch(() => {}); }));
}
