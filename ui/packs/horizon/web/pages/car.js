// Cluster (/cars/{car}): the car right now as an instrument cluster, then the chosen period.
// Now: GET /cars/{car}/now (every value with its own time), then live `now` events merged field by field.
// The cluster: a speedometer, the battery dial with its range and a fuel gauge, a power meter, trip counters
// (odometer, temperatures, software, data age), warning lights for the car's modes and doors, the map,
// why the car is awake, the tyres and charging now. Its live inputs (SMO, the Tesla API) are named with the
// age of each one's newest value, and every value says which input sent it. The period (default 24 h): figures from /statistics,
// the battery and charging charts on one cursor, and the activity strip. Rows events reload the period.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { format, getUnits, onUnitsChange, unitLabel } from '../shared/units.js';
import { formatDateTime, formatDate, zoneName } from '../shared/time.js';
import { downloadCsv, csvFilename, column, timeColumn } from '../shared/csv.js';
import { icon, hud, plate, btn, linkBtn, lamp, tag } from '../core/ui.js';
import { dial, fuel, odo, lights } from '../core/gauges.js';
import { carBtn, periodBtn, stat, rack, band, csvBtn, placeLink } from '../core/kit.js';
import { strip } from '../core/strip.js';
import { scope, loadUPlot, well } from '../core/chart.js';
import { createMap, pin, mapFallback } from '../core/map.js';
import { val, stateWord, when, agoEl, dur, ago } from '../core/fmt.js';
import { srcChip, valSrc, inputsLine, kindsOf } from '../core/sources.js';
import { vehiclePanel } from '../core/vehicle.js';

export const id = 'car';
export const css = true;
export const title = (ctx) => `${ctx.car?.name || 'Car'} cluster`;

const CLIMATE = { off: 'Off', on: 'On', precondition: 'Preconditioning', overheat_protect: 'Overheat protection' };
const KEEP = { off: 'Off', on: 'Keep on', dog: 'Dog mode', camp: 'Camp mode' };
const AWAKE = {
  sentry: ['Sentry is on', 'Sentry keeps the cameras watching, so the car cannot sleep.'],
  keep_climate_on: ['Keep climate is on', 'The car keeps the cabin at its set point.'],
  keep_climate_dog: ['Dog mode is on', 'The car keeps the cabin safe for a pet.'],
  keep_climate_camp: ['Camp mode is on', 'The car keeps the cabin comfortable and the screen on.'],
  user_present: ['Someone is in the car', 'The seat sensor or a key nearby keeps it awake.'],
  preconditioning_cabin: ['The cabin is preconditioning', 'Heating or cooling before a drive.'],
  preconditioning_battery: ['The battery is preconditioning', 'Warming the pack before a drive or a fast charge.'],
  charging: ['Charging', 'The car stays awake while it charges.'],
  keep_warm: ['The battery is kept warm', 'In the cold the car warms the pack while plugged in.'],
  door_open: ['A door is open', 'The car waits until every door, the trunk and the frunk are shut.'],
  accessory_power: ['Accessory power is on', 'Power to the sockets keeps the car awake even with nothing plugged in.'],
};
const TYRES = [['fl', 'Front left'], ['fr', 'Front right'], ['rl', 'Rear left'], ['rr', 'Rear right']];
const DOORS = [['fl', 'Front left door'], ['fr', 'Front right door'], ['rl', 'Rear left door'], ['rr', 'Rear right door']];
const at = (x) => (x && typeof x === 'object' && 'at_ms' in x ? x.at_ms : null);

// merge a live `now` delta into the values we hold (objects field by field)
function merge(into, from) {
  for (const [k, v] of Object.entries(from || {})) {
    if (v && typeof v === 'object' && !Array.isArray(v) && !('value' in v) && into[k] && typeof into[k] === 'object') merge(into[k], v);
    else into[k] = v;
  }
}

export async function mount(ctx) {
  const { car, tz } = ctx;
  const cid = car.id;
  const [now, settings] = await Promise.all([
    ctx.api.get(`cars/${encodeURIComponent(cid)}/now`),
    ctx.api.get('settings', null, { maxAgeMs: 300000 }).catch(() => null),
  ]);
  if (!ctx.alive()) return;

  const headLine = h('p', { class: 'cl-head' });
  const dataLine = h('div', { class: 'cl-data', cover: ['summary.state.drive_offline', 'summary.data.restored_values'] });
  const clusterEl = h('div', { class: 'cl-cluster' });
  const countersEl = h('div', { class: 'cl-counters' });
  const lightsEl = h('div', { class: 'cl-lights' });
  const mapEl = h('div', { class: 'map cl-map', role: 'img', 'aria-label': `Where ${car.name} is` });
  const mapSide = h('div', { class: 'cl-map-tools' });
  const placeEl = h('p', { class: 'cl-place' });
  const awakeEl = h('div');
  const tyresEl = h('div');
  const chargeEl = h('div');
  const periodEl = h('div', { class: 'stack' });
  const fwLink = h('p', { class: 'vi-fw' }, icon('chip', 'i--sm'), h('span', null, 'The modules\' firmware fingerprints, and each Tesla update found from them: ',
    h('a', { class: 'plc', href: `${ctx.link('software')}#firmware` }, 'Car firmware'), ' on Software.'));
  const vehicle = vehiclePanel(ctx, car, { foot: fwLink, onSaved: (c) => { ctx.carChanged?.(c); paintHead(); } });

  replace(ctx.view, h('div', { class: 'page cl', cover: ['shots.web-interface', 'shots.overview'] },
    plate({
      kicker: 'Cars · Cluster',
      title: h('span', { cover: 'summary.header.name' }, car.name || `Unnamed ${car.model || 'car'}`),
      tools: [carBtn(ctx, 'overview.filter.car'), linkBtn('Timeline', ctx.link('timeline'), { icon: 'timeline', sm: true, kind: 'ghost' })],
      extra: headLine,
    }),
    dataLine,
    hud({ title: 'Cluster', icon: 'gauge', tone: 'cyan', cls: 'cl-dash', cover: 'nr.dash.panel.car-status', side: h('span', { class: 'trk-count' }, `values in ${unitLabel('distance')}, ${unitLabel('temperature')}, ${unitLabel('pressure')}`) },
      clusterEl, countersEl, lightsEl),
    h('div', { class: 'cols cols--wide' },
      hud({ title: 'Where it is', icon: 'pin', pad: false, cls: 'cl-map-panel', side: mapSide, cover: ['summary.map.position', 'summary.map.attribution', 'summary.map.zoom'] }, mapEl, placeEl),
      h('div', { class: 'stack' }, awakeEl, tyresEl)),
    chargeEl,
    vehicle.el,
    periodEl));

  /* ---------- head */
  function paintHead() {
    const trim = car.config?.trim;
    const st = now.state || 'unknown';
    replace(headLine,
      h('span', { cover: 'summary.header.model', title: 'Model and year come from the VIN start.' }, [car.model, car.year].filter(Boolean).join(' · ') || 'Model not known yet'),
      trim?.value ? h('span', { class: 'cl-sep' }, h('span', { cover: 'summary.header.trim', title: `Trim, from ${trim.source === 'can' ? "the car's own configuration message (SMO)" : trim.source === 'tesla_api' ? 'the Tesla API' : trim.source === 'owner' ? 'the owner' : trim.source === 'teslamate' ? 'the TeslaMate import' : 'the VIN start'}` }, trim.value)) : h('span', { class: 'cl-sep' }, h('span', { cover: 'summary.header.trim', class: 'faint' }, 'Trim not sent yet')),
      srcChip(car) ? h('span', { class: 'cl-sep' }, srcChip(car, { full: true })) : null,
      h('span', { class: 'cl-sep cl-status', cover: 'summary.stat.status' }, lamp(st, { stale: !!now.stale }),
        h('b', null, now.stale && st !== 'unknown' ? `Last known: ${stateWord(st).toLowerCase()}` : stateWord(st)),
        now.since_ms ? h('span', null, ` since ${when(now.since_ms, tz)} (${dur(Math.max(0, ctx.api.now() - now.since_ms))})`) : null));
    const quiet = now.stale || !now.at_ms;
    const viaApi = (now.inputs || []).some((i) => i.kind === 'tesla_api');
    replace(dataLine, inputsLine(now.inputs, { cover: 'summary.data.live_push' }), quiet
      ? h('div', { class: 'callout callout--warn', role: 'status' }, icon('signal'), h('div', null,
        h('p', { class: 'callout-head' }, !now.at_ms ? 'No recent log from this car' : 'Showing the last known values'),
        h('p', { class: 'callout-text' }, `Nothing has arrived since ${now.at_ms ? ago(now.at_ms) : 'a long time'}. Every value below is the last one the car sent, with its age. ${viaApi ? 'Neither SMO nor the Tesla API has sent anything newer.' : 'SMO (the CAN Bridge) may be off or out of range; a drive under way then shows as stopped until its logs arrive.'}`)))
      : h('p', { class: 'cl-live' }, icon('live', 'i--sm'), h('span', null, `Live: SMO values arrive about a minute after the car records them${viaApi ? ', Tesla API values as often as the server asks' : ''}, and update here by themselves. Each value names the input it came from. Values from before a restart are kept until a new one comes.`)));
  }

  /* ---------- the cluster: speed, battery, power */
  function paintCluster() {
    const soc = val(now.soc_pct);
    const ideal = getUnits().range === 'ideal';
    const range = val(ideal ? now.ideal_range_km : now.range_km);
    const est = val(now.est_range_km);
    const speed = now.state === 'driving' ? val(now.speed_kph) : (val(now.speed_kph) ?? 0);
    const power = val(now.power_kw);
    const lowTone = soc != null && soc < 20;
    const zones = soc == null ? 'not known' : soc < 10 ? 'very low' : soc < 20 ? 'low' : soc > 90 ? 'high' : 'normal';
    replace(clusterEl,
      h('div', { class: 'cl-dial', cover: 'summary.stat.speed' },
        dial({ value: speed, max: getUnits().distance === 'mi' ? 160.9344 : 160, kind: 'speed', label: 'Speed', ticks: 8, why: 'not sent' }),
        h('p', { class: 'cl-dial-how' }, now.state === 'driving' ? `Gear ${val(now.gear) || '—'}` : 'Standing still', valSrc(now.speed_kph))),
      h('div', { class: 'cl-dial cl-dial--main', cover: ['overview.stat.battery_level', 'nr.dash.battery-level', 'nr.dash.battery-level.bands', 'summary.stat.soc'] },
        dial({ value: soc, max: 100, kind: 'percent', label: 'Battery', ticks: 10, warm: lowTone, why: 'not sent' }),
        h('div', { class: 'cl-fuel' }, fuel({ fraction: soc == null ? null : soc / 100, cells: 20, label: 'Battery' }),
          h('p', { class: 'cl-dial-how' }, `Level ${zones}: under 20 % shows orange, under 10 % red.${val(now.usable_soc_pct) != null && Math.abs(val(now.usable_soc_pct) - soc) >= 1 ? ` Usable ${format(val(now.usable_soc_pct), 'percent')}.` : ''}`, valSrc(now.soc_pct))),
        h('div', { class: 'cl-ranges' },
          stat(ideal ? 'Ideal range' : 'Rated range', range, 'range', { cover: ['summary.stat.range_preferred', 'overview.stat.range', 'nr.dash.ideal-range'], how: 'as the car shows it', src: valSrc(ideal ? now.ideal_range_km : now.range_km) }),
          stat('Sentry Mate estimate', est, 'range', { cover: ['summary.stat.range_estimated', 'nr.dash.est-range'], why: 'not enough driving yet',
            how: now.est_basis === 'wh_per_km_30d' ? 'energy left at your last 30 days of driving' : 'energy left at your consumption' }))),
      h('div', { class: 'cl-dial' },
        dial({ value: power == null ? null : Math.abs(power), max: 250, kind: 'power', label: power != null && power < 0 ? 'Regen' : 'Power', ticks: 10, warm: power != null && power < 0, why: 'not sent' }),
        h('p', { class: 'cl-dial-how' }, power != null && power < 0 ? 'Energy going back into the battery' : 'Power the battery gives', valSrc(now.power_kw))));

    const climate = val(now.flags?.climate);
    const setp = (x) => { const v = val(x); return v == null ? '—' : typeof v === 'string' ? v : format(v, 'temperature'); };
    const sw = now.software || {};
    replace(countersEl,
      h('div', { class: 'cl-odo', cover: ['summary.stat.odometer', 'overview.stat.odometer', 'overview.link.mileage'] },
        h('span', { class: 'ctr-label' }, 'Odometer', valSrc(now.odometer_km)),
        h('a', { class: 'cl-odo-face', href: ctx.link('statistics'), title: 'Distance over time on Statistics' }, odo(val(now.odometer_km) == null ? '—' : format(val(now.odometer_km), 'odometer', { unit: false, digits: 1, grouping: false })), h('span', { class: 'ctr-unit' }, unitLabel('odometer')))),
      rack([
        stat('Outside', val(now.outside_c), 'temperature', { icon: 'thermo', cover: ['summary.stat.outside_temp', 'overview.stat.outside_temp'], why: 'not sent', src: valSrc(now.outside_c) }),
        stat('Cabin', val(now.cabin_c), 'temperature', { icon: 'thermo', cover: ['summary.stat.inside_temp', 'overview.stat.inside_temp', 'nr.dash.panel.car-status'], why: 'not sent', src: valSrc(now.cabin_c) }),
        stat('Climate', climate === 'off' ? 'Off' : `${setp(now.setpoint_left_c)} · ${setp(now.setpoint_right_c)}`, null, { icon: 'sun', cover: 'overview.stat.driver_temp',
          how: climate === 'off' ? 'no set point is sent while climate is off' : 'set point left · right', src: valSrc(now.flags?.climate) }),
        stat('Software', h('a', { href: `${ctx.link('software')}#firmware`, title: 'Software, with the car firmware' }, sw.version || 'not known'), null, { icon: 'chip', cover: ['summary.stat.version', 'overview.stat.firmware', 'overview.link.updates', 'summary.flag.software_update'],
          how: sw.changed_since_ms ? `changed since ${when(sw.changed_since_ms, tz)}; the new version text is not known yet` : sw.version_since_ms ? `since ${formatDate(sw.version_since_ms, tz, { weekday: false })}` : 'no change seen' }),
        stat('12 V battery', val(now.lv_v), 'voltage', { icon: 'battery', why: 'not sent', src: valSrc(now.lv_v) }),
        stat('Newest value', now.at_ms ? agoEl(now.at_ms) : null, null, { icon: 'live', cover: ['nr.dash.last-update', 'summary.data.live_push'], why: 'none yet',
          how: now.at_ms ? `recorded ${formatDateTime(now.at_ms, tz)}, ${zoneName(now.at_ms, tz)}${now.received_ms ? `; last data in ${ago(now.received_ms)}` : ''}` : null }),
        stat('Your units', h('a', { href: state.buildUrl('settings-display') }, `${unitLabel('distance')} · ${unitLabel('temperature')} · ${unitLabel('pressure')}`), null, { icon: 'sliders', cover: 'summary.data.prefs_snapshot',
          how: 'every value converts at once when you change them' }),
      ], { cls: 'cl-rack' }));

    const f = now.flags || {};
    const c = now.charge || {};
    const doorsOpen = DOORS.filter(([k]) => val(f.doors?.[k])).map(([, w]) => w);
    for (const k of ['trunk', 'frunk', 'liftgate']) if (val(f[k])) doorsOpen.push(k[0].toUpperCase() + k.slice(1));
    const keep = val(f.keep_climate);
    replace(lightsEl,
      lights([
        { icon: 'eye', text: 'Sentry', on: !!val(f.sentry), tone: 'red', cover: 'summary.flag.sentry_mode', title: 'Sentry mode' },
        { icon: 'warn', text: 'Sentry event?', on: !!val(f.sentry_event_guess), tone: 'red', cover: 'summary.flag.sentry_recording', title: 'A guess: the screen and cameras woke while Sentry was on. Not a confirmed event.' },
        { icon: 'sun', text: climate === 'precondition' ? 'Preconditioning' : `Climate ${(CLIMATE[climate] || 'unknown').toLowerCase()}`, on: climate === 'on' || climate === 'precondition' || !!val(f.battery_preconditioning), tone: 'cyan', cover: 'summary.flag.preconditioning' },
        { icon: 'home', text: KEEP[keep] || 'Keep climate', on: keep && keep !== 'off', tone: 'cyan', cover: 'summary.flag.dog_mode', title: 'Keep climate, dog mode or camp mode' },
        { icon: 'thermo', text: 'Cold battery', on: !!val(f.reduced_range), cover: 'summary.flag.reduced_range', title: 'Part of the charge is locked by a cold battery, so range reads low' },
        { icon: 'user', text: 'Someone inside', on: !!val(f.user_present), tone: 'grn', cover: 'summary.flag.driver_present' },
        { icon: 'plug', text: 'Plugged in', on: !!val(c.plugged), tone: 'grn', cover: 'summary.flag.plugged_in' },
        { icon: 'sliders', text: 'Service mode', on: !!val(f.service_mode), cover: ['summary.flag.service_mode', 'summary.state.in_service'] },
        { icon: 'door', text: doorsOpen.length ? `${doorsOpen.length} open` : 'Doors shut', on: doorsOpen.length > 0, tone: 'red', cover: 'summary.flag.doors_open', title: doorsOpen.length ? `Open: ${doorsOpen.join(', ')}` : 'Every door, the trunk and the frunk are shut' },
      ]),
      h('p', { class: 'note cl-lock-note', cover: ['summary.flag.lock', 'summary.flag.windows_open'] }, icon('info', 'i--sm'),
        h('span', null, doorsOpen.length ? `Open now: ${doorsOpen.join(', ')}. ` : '', 'Locks and windows are not sent on the CAN bus, so they are not shown here; a lit light means the mode is on.',
          flagSrc() ? [' Sentry, someone inside and climate ', flagSrc()] : null)));
  }

  // which input the flags that the Tesla API can send came from now
  function flagSrc() {
    const f = now.flags || {};
    const api = ['sentry', 'user_present', 'keep_climate', 'climate', 'battery_preconditioning'].some((k) => kindsOf(f[k]).includes('tesla_api'));
    return api ? h('span', null, 'come from ', valSrc({ sources: [{ kind: 'tesla_api', label: 'Data from the Tesla API' }] }), '.') : null;
  }

  /* ---------- the map */
  const lat = () => val(now.location?.lat), lon = () => val(now.location?.lon);
  let m = null, carPin = null, full = false;
  const mapPanel = () => mapEl.closest('.cl-map-panel');
  function paintMapTools() {
    const outside = !!settings?.outside_links?.maps;
    replace(mapSide,
      btn(full ? 'Back' : 'Full screen', () => toggleFull(), { icon: 'monitor', sm: true, kind: 'ghost', cover: 'summary.map.fullscreen' }),
      outside && lat() != null
        ? h('a', { class: 'btn btn--sm btn--ghost', href: `https://www.openstreetmap.org/?mlat=${lat().toFixed(5)}&mlon=${lon().toFixed(5)}#map=17/${lat().toFixed(5)}/${lon().toFixed(5)}`, target: '_blank', rel: 'noopener noreferrer external', cover: 'summary.map.external_link' }, icon('share', 'i--sm'), h('span', null, 'Open outside'))
        : h('span', { class: 'tag', cover: 'summary.map.external_link', title: 'Links to outside map sites are off. An admin can turn them on in Display and units.' }, 'Outside links off'));
  }
  function toggleFull() {
    const p = mapPanel();
    if (!document.fullscreenElement) p.requestFullscreen?.().catch(() => { p.classList.toggle('is-full'); });
    else document.exitFullscreen?.();
  }
  ctx.on(document, 'fullscreenchange', () => { full = !!document.fullscreenElement; paintMapTools(); setTimeout(() => m?.map.resize(), 50); });
  function paintPlace() {
    const loc = now.location || {};
    const a = loc.address;
    replace(placeEl, icon('pin', 'i--sm'), h('span', null,
      loc.place ? placeLink({ place: loc.place, lat: lat(), lon: lon() }) : lat() != null ? placeLink({ lat: lat(), lon: lon(), label: a?.short || 'This spot' }) : 'Position not known yet',
      a ? h('span', { class: 'faint' }, ` · ${[a.locality, a.region].filter(Boolean).join(', ')}`) : null,
      val(loc.accuracy_m) != null ? h('span', { class: 'faint' }, ` · within ${format(val(loc.accuracy_m), 'length')}`) : null,
      at(loc.lat) ? h('span', { class: 'faint' }, ' · ', agoEl(at(loc.lat))) : null, valSrc(loc.lat)));
  }
  async function mountMap() {
    if (lat() == null) { mapEl.append(mapFallback('No position has been recorded yet.')); return; }
    try {
      m = await createMap(mapEl, ctx, { center: [lon(), lat()], zoom: 14 });
      if (!m) return;
      await m.ready;
      if (!ctx.alive()) return;
      carPin = pin(m, [lon(), lat()], { tone: 'car', title: car.name });
    } catch (e) { console.warn(e); replace(mapEl, mapFallback()); }
  }
  const moveMarker = () => { if (carPin && lat() != null) { carPin.setLngLat([lon(), lat()]); if (now.state === 'driving') m.map.easeTo({ center: [lon(), lat()], duration: 600 }); } };

  /* ---------- why awake, tyres, charging */
  function paintAwake() {
    const r = now.awake_reasons || [];
    const st = now.state;
    const asleep = st === 'asleep';
    replace(awakeEl, hud({ title: 'Why the car is awake', icon: 'moon', cover: 'web.settings.car.sleep-blockers' },
      asleep ? h('p', { class: 'cl-awake-none' }, icon('moon', 'i--sm'), h('span', null, 'Asleep: both buses are quiet.'))
        : r.length ? h('ul', { class: 'cl-awake' }, r.map((k) => h('li', null, h('span', { class: 'cl-awake-dot', 'aria-hidden': 'true' }), h('div', null, h('b', null, (AWAKE[k] || [k.replace(/_/g, ' ')])[0]), h('span', null, (AWAKE[k] || [, ''])[1])))))
          : h('p', { class: 'cl-awake-none' }, icon('info', 'i--sm'), h('span', null, st === 'driving' ? 'Driving: the car is awake to drive.' : st === 'unknown' ? 'Not known: no recent log.' : 'Nothing that keeps it awake is on. A car falls asleep about 15 minutes after the last use.')),
      h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, 'Sentry Mate only listens, so it never keeps the car awake. ', h('a', { href: ctx.link('timeline') }, 'The timeline'), ' shows when it slept.'))));
  }
  function paintTyres() {
    const rec = val(now.tyre_recommended_kpa);
    const ty = now.tyres_kpa || [];
    const low = ty.some((x) => val(x) != null && rec != null && val(x) < rec * 0.9);
    replace(tyresEl, hud({ title: 'Tyres', icon: 'tyre', cover: 'summary.flag.tire_pressure_warning', side: [ty.length ? valSrc(ty.find((x) => x)) : null, low ? tag('Low', 'org') : ty.length ? tag('Fine', 'grn') : null] },
      ty.length ? h('div', { class: 'cl-tyres' }, h('div', { class: 'cl-tyre-car', 'aria-hidden': 'true' }, icon('car', 'i--xl')),
        TYRES.map(([k, w], i) => {
          const v = val(ty[i]);
          const isLow = v != null && rec != null && v < rec * 0.9;
          return h('div', { class: ['cl-tyre', `cl-tyre--${k}`, isLow && 'is-low'] }, h('span', { class: 'cl-tyre-k' }, w), h('span', { class: 'cl-tyre-v' }, format(v, 'pressure')));
        }))
        : h('p', { class: 'empty-line' }, icon('info', 'i--sm'), h('span', null, 'This car does not send tyre pressures.')),
      h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, rec != null ? `Recommended ${format(rec, 'pressure')}; under 90 % of it lights the warning.` : 'No recommended pressure is known.',
        now.tyres_slots_mapped === false ? ' Which sensor sits on which wheel is not known yet.' : ''))));
  }
  function paintCharge() {
    const c = now.charge || {};
    const plugged = !!val(c.plugged);
    const charging = now.state === 'charging' || val(c.status) === 'charging';
    const why = charging ? 'No reading' : plugged ? 'Not charging' : 'Not plugged in';
    const kind = val(c.kind);
    replace(chargeEl, hud({ title: 'Charging now', icon: 'bolt', tone: charging ? 'cyan' : null, cls: ['cl-charge', charging && 'is-on'], cover: ['nr.dash.panel.charge-status', 'summary.stat.scheduled_charging'],
      side: [h('span', { class: 'cl-chg-state' }, lamp(charging ? 'charging' : plugged ? 'parked' : 'unknown'), h('span', null, charging ? `Charging${kind ? ` (${kind.toUpperCase()})` : ''}` : plugged ? 'Plugged in, not charging' : 'Not plugged in')),
        c.charge_id ? linkBtn('This charge', state.buildUrl('charge', { car: cid, charge: c.charge_id }), { sm: true, kind: 'ghost', icon: 'arrow-r' }) : null] },
    rack([
      stat('Power', val(c.power_kw), 'power', { hot: charging, why, src: valSrc(c.power_kw), cover: ['summary.stat.charger_power', 'nr.dash.charging-power', 'overview.stat.charging_power'] }),
      stat('Volts', val(c.volts_v), 'voltage', { why, cover: 'overview.stat.ac_voltage', how: kind === 'dc' ? "the charger's" : null }),
      stat('Amps', val(c.amps_a), 'current', { why, cover: 'nr.dash.charging-current', how: val(c.phases) ? `${val(c.phases)} ${val(c.phases) === 1 ? 'phase' : 'phases'}${val(c.pilot_a) ? `, cable allows ${val(c.pilot_a)} A` : ''}` : null }),
      stat('Added', val(c.added_kwh), 'energy', { why: 'No charge under way', cover: 'summary.stat.energy_added', src: valSrc(c.added_kwh) }),
      stat('Charge limit', val(c.limit_pct), 'percent', { why: plugged ? 'Not sent yet' : 'Not plugged in', cover: ['summary.stat.charge_limit', 'nr.dash.charge-limit'], how: val(c.limit_pct) != null ? 'set on the car' : null, src: valSrc(c.limit_pct) }),
      stat('Time to full', val(c.to_full_min) != null ? val(c.to_full_min) * 60000 : null, 'duration', { why: charging ? 'Not known yet' : 'No charge under way', cover: ['summary.stat.remaining_time', 'nr.dash.time-to-full'],
        how: val(c.to_full_kwh) != null ? `${format(val(c.to_full_kwh), 'energy')} to go` : null, src: valSrc(c.to_full_min) }),
      stat('Done at', c.finish_ms ? when(c.finish_ms, tz) : null, null, { why: charging ? 'Not known yet' : 'No charge under way', cover: 'summary.stat.expected_finish', how: c.finish_ms ? `${zoneName(c.finish_ms, tz)}, the car's time` : null }),
    ]),
    h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, 'A charge timer set on the car is not sent on the CAN bus; a planned start shows here once the charge begins. The charge limit, energy added and time to full can come from the Tesla API while it reads the car.'))));
  }

  /* ---------- the period */
  const sync = `cl-${cid}`;
  async function paintPeriod() {
    const p = ctx.period;
    const from = p.from_ms ?? (ctx.api.now() - 86400000), to = p.to_ms;
    const ideal = getUnits().range === 'ideal';
    const rk = ideal ? 'ideal_range_km' : 'range_km';
    const qs = { from_ms: from, to_ms: to };
    const [stats, series, act, charges] = await Promise.all([
      ctx.api.get(`cars/${encodeURIComponent(cid)}/statistics`, { period: 'all', ...qs }).catch(() => null),
      ctx.api.get(`cars/${encodeURIComponent(cid)}/series`, { fields: `soc_pct,usable_soc_pct,${rk}`, ...qs, max_points: 720 }),
      ctx.api.get(`cars/${encodeURIComponent(cid)}/activity`, qs),
      ctx.api.page(`cars/${encodeURIComponent(cid)}/charges`, { from_ms: from - 12 * 3600000, to_ms: to }, { limit: 6 }).catch(() => ({ items: [] })),
    ]);
    if (!ctx.alive()) return;
    const row = stats?.items?.[0] || {};
    const rec = row.completeness?.recorded_pct;
    const blank = !stats?.items?.length ? 'Could not be loaded' : rec === 0 ? 'No data for this period' : null;
    const part = !blank && rec != null && rec < 100 ? `; ${format(rec, 'percent')} of the period recorded` : '';
    const fig = (v) => (blank ? null : v);

    // charging curves of the charges in the period, joined on one time axis (a break between them)
    const inPeriod = charges.items.filter((c) => (c.end_ms ?? to) > from).slice(0, 4);
    const curves = await Promise.all(inPeriod.map((c) => ctx.api.get(`cars/${encodeURIComponent(cid)}/charges/${encodeURIComponent(c.id)}/curve`,
      { fields: 'power_kw,heater_kw,volts_v,amps_a', max_points: 400 }).catch(() => null)));
    if (!ctx.alive()) return;
    const cg = { t: [], power: [], heater: [], energy: [], volts: [], amps: [] };
    curves.filter(Boolean).sort((a, b) => a.t_ms[0] - b.t_ms[0]).forEach((cv, k) => {
      if (k) { cg.t.push(cv.t_ms[0] - 1000); for (const key of ['power', 'heater', 'energy', 'volts', 'amps']) cg[key].push(null); }
      let e = 0;
      cv.t_ms.forEach((t, i) => {
        const pw = cv.values.power_kw?.[i] ?? null;
        if (i && pw != null) e += (pw * (t - cv.t_ms[i - 1])) / 3.6e6;
        cg.t.push(t); cg.power.push(pw); cg.heater.push(cv.values.heater_kw?.[i] ?? null); cg.energy.push(e);
        cg.volts.push(cv.values.volts_v?.[i] ?? null); cg.amps.push(cv.values.amps_a?.[i] ?? null);
      });
    });

    const battWell = well('well--lg', 'Battery level and range through the period');
    const chgWell = well('', 'Charging power and energy');
    const supWell = well('well--sm', 'Charging volts and amps');
    const readBatt = h('span', { class: 'scope-read' });
    const readChg = h('span', { class: 'scope-read' });
    const keyBtn = (label, color, onClick) => h('button', { class: 'key', type: 'button', 'aria-pressed': 'true', onclick: (e) => { const b = e.currentTarget; const on = b.getAttribute('aria-pressed') !== 'true'; b.setAttribute('aria-pressed', String(on)); onClick(on); } },
      h('i', { class: 'key-sw', vars: { '--c': `var(${color})` } }), h('span', null, label));
    let battPlot = null, chgPlot = null;
    const battKeys = h('div', { class: 'keys', role: 'group', 'aria-label': 'Show or hide a line' },
      keyBtn('Battery', '--c-soc', (on) => battPlot?.u?.setSeries(1, { show: on })),
      keyBtn('Usable', '--violet', (on) => battPlot?.u?.setSeries(2, { show: on })),
      keyBtn(ideal ? 'Ideal range' : 'Rated range', '--cyan', (on) => battPlot?.u?.setSeries(3, { show: on })));
    const chgKeys = h('div', { class: 'keys' },
      h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': 'var(--c-charge)' } }), 'Power'),
      h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': 'var(--org)' } }), 'Battery heating'),
      h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': 'var(--mag)' } }), 'Energy added'),
      h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': 'var(--yel)' } }), 'Volts'),
      h('span', { class: 'key' }, h('i', { class: 'key-sw', vars: { '--c': 'var(--cyan)' } }), 'Amps'));

    replace(periodEl,
      band(2, `The period · ${p.label}`, [periodBtn(ctx, 'overview.control.time_range')]),
      h('p', { class: 'note', cover: 'overview.control.refresh' }, icon('live', 'i--sm'), h('span', null, 'Updates by itself when a drive, charge or parked spell starts or ends.')),
      hud({ title: 'Period figures', icon: 'stats' }, rack([
        stat('Distance driven', fig(row.distance_logged_km), 'distance', { cover: 'overview.stat.distance_logged', why: blank || 'No drive', how: blank ? null : `${row.drives ?? 0} ${row.drives === 1 ? 'drive' : 'drives'}${part}` }),
        stat('Net consumption', fig(row.net_wh_per_km), 'efficiency', { cover: 'overview.stat.consumption_net', why: blank || 'No drive in the period', how: 'driving only, regen taken off' }),
        stat('Gross consumption', fig(row.gross_wh_per_km), 'efficiency', { cover: 'overview.stat.consumption_gross', why: blank || 'No drive in the period', how: 'parked use included' }),
        stat('Energy added', fig(row.energy_added_kwh), 'energy', { why: blank, how: blank ? null : `${row.charges ?? 0} ${row.charges === 1 ? 'charge' : 'charges'}` }),
        stat('Cost', blank ? null : (row.cost_by_currency?.length ? format(row.cost_by_currency[0], 'money') : null), null, { why: blank || 'No priced charge' }),
      ])),
      h('div', { class: 'cols' },
        hud({ title: 'Battery', icon: 'battery', cover: ['overview.chart.charge_level', 'overview.control.shared_crosshair', 'overview.chart.legend_toggle'],
          side: [srcChip(series), csvBtn({ name: [car.name, 'battery'], rows: () => series.t_ms.map((t, i) => ({ t, soc: series.values.soc_pct?.[i], usable: series.values.usable_soc_pct?.[i], range: series.values[rk]?.[i] })),
            columns: [timeColumn('Time', 't', tz), column('Battery', 'soc', 'percent'), column('Usable', 'usable', 'percent'), column(ideal ? 'Ideal range' : 'Rated range', 'range', 'range')] }),
          linkBtn('Battery', ctx.link('battery'), { sm: true, kind: 'ghost', icon: 'arrow-r', cover: 'overview.link.charge_level' })] },
        h('div', { class: 'scope-head' }, battKeys, readBatt), battWell),
        hud({ title: 'Charging', icon: 'bolt', cover: ['overview.chart.charging_details.power', 'overview.chart.charging_details.energy', 'overview.chart.charging_details.ac_current', 'overview.chart.charging_details.ac_voltage', 'overview.chart.charging_details.battery_heater'],
          side: [csvBtn({ name: [car.name, 'charging'], rows: () => cg.t.map((t, i) => ({ t, power: cg.power[i], heater: cg.heater[i], energy: cg.energy[i], volts: cg.volts[i], amps: cg.amps[i] })).filter((r) => r.power != null),
            columns: [timeColumn('Time', 't', tz), column('Power', 'power', 'power'), column('Battery heating', 'heater', 'power'), column('Energy added', 'energy', 'energy'), column('Volts', 'volts', 'voltage'), column('Amps', 'amps', 'current')] }),
          linkBtn('Charges', ctx.link('charges'), { sm: true, kind: 'ghost', icon: 'arrow-r', cover: 'overview.link.charging_stats' })] },
        h('div', { class: 'scope-head' }, chgKeys, readChg),
        cg.t.length ? [chgWell, supWell] : h('p', { class: 'empty-line' }, icon('info', 'i--sm'), h('span', null, 'No charge in this period, so there is no charging curve.')))),
      hud({ title: 'What the car did', icon: 'timeline', cover: 'overview.chart.states',
        side: linkBtn('Timeline', ctx.link('timeline'), { sm: true, kind: 'ghost', icon: 'arrow-r', cover: 'overview.link.states' }) },
      strip(act.items || [], from, to, { tz, now: ctx.api.now(), tall: true })));

    await loadUPlot();
    if (!ctx.alive()) return;
    const v = series.values;
    battPlot = await scope(battWell, ctx, {
      tz, t_ms: series.t_ms, sync,
      series: [
        { label: 'Battery', values: v.soc_pct || [], kind: 'percent', color: '--c-soc', fill: true },
        { label: 'Usable', values: v.usable_soc_pct || [], kind: 'percent', color: '--violet', width: 1.5, dash: [4, 3] },
        { label: 'Range', values: v[rk] || [], kind: 'range', color: '--cyan', scale: 'y2', width: 1.5 },
      ],
      ranges: { y: [0, 100] },
      onCursor: (i) => { readBatt.textContent = i == null ? '' : `${formatDateTime(series.t_ms[i], tz)} · ${format(v.soc_pct?.[i], 'percent')} · ${format(v[rk]?.[i], 'range')}`; },
    });
    if (cg.t.length) {
      chgPlot = await scope(chgWell, ctx, {
        tz, t_ms: cg.t, sync: `chg-${cid}`,
        series: [
          { label: 'Power', values: cg.power, kind: 'power', color: '--c-charge', fill: true },
          { label: 'Heating', values: cg.heater, kind: 'power', color: '--org', width: 1.5 },
          { label: 'Energy', values: cg.energy, kind: 'energy', color: '--mag', scale: 'y2', width: 1.5 },
        ],
        axes: { y2: { digits: 1 } },
        onCursor: (i) => { readChg.textContent = i == null ? '' : `${formatDateTime(cg.t[i], tz)} · ${format(cg.power[i], 'power')} · ${format(cg.energy[i], 'energy')} · ${format(cg.volts[i], 'voltage')} · ${format(cg.amps[i], 'current')}`; },
      });
      await scope(supWell, ctx, {
        tz, t_ms: cg.t, sync: `chg-${cid}`,
        series: [{ label: 'Volts', values: cg.volts, kind: 'voltage', color: '--yel' }, { label: 'Amps', values: cg.amps, kind: 'current', color: '--cyan', scale: 'y2' }],
      });
    }
    void chgPlot;
  }

  paintHead(); paintCluster(); paintMapTools(); paintPlace(); paintAwake(); paintTyres(); paintCharge();
  try { await paintPeriod(); } catch (e) {
    if (!ctx.alive()) return;
    replace(periodEl, hud({ title: 'The period', icon: 'stats' }, h('p', { class: 'empty-line' }, icon('warn', 'i--sm'), h('span', null, `The period could not be loaded: ${e.message}`))));
  }
  if (!ctx.alive()) return;
  ctx.ready();
  mountMap();

  let pend = 0;
  ctx.events('now', (ev) => {
    if (ev.data?.car !== cid || !ev.data.now) return;
    merge(now, ev.data.now);
    if (pend) return;
    pend = setTimeout(() => { pend = 0; if (!ctx.alive()) return; paintHead(); paintCluster(); paintPlace(); paintAwake(); paintTyres(); paintCharge(); moveMarker(); }, 2000);
  });
  let rowsT = 0;
  ctx.events('rows', (ev) => {
    if (ev.data?.car !== cid) return;
    clearTimeout(rowsT);
    rowsT = setTimeout(() => { if (ctx.alive()) paintPeriod().catch(() => {}); }, 1500);
  });
  ctx.onCleanup(() => { clearTimeout(pend); clearTimeout(rowsT); });
  ctx.onCleanup(onUnitsChange(() => { paintCluster(); paintTyres(); paintCharge(); paintPeriod().catch(() => {}); }));
}
