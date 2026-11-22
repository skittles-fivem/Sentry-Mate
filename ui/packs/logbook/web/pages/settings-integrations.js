// Sentry Mate, "Logbook" UI pack: Settings: Integrations (/settings/integrations).
//
// Sending the cars' live values to other systems, if the owner wants to: the MQTT broker (with a test,
// its live status and its log), the topic layout and its rules, every value Sentry Mate publishes
// (previewed for one car, with unit, source and status), Home Assistant discovery and the entities it
// gets, the events automations listen to, the read API, every network connection, and other tools.
//
// API: GET / PUT settings/mqtt (admins; PUT sends the whole setting with its version), GET
// settings/mqtt/status, GET settings/mqtt/preview?car=, POST settings/mqtt/test, GET network (admins),
// GET cars/{car}/events, GET audit (admins, for the broker log), GET cars (the preview car).
// Switches and single fields save at once; the broker connection saves with its button, because host,
// port, TLS, pin and password are checked together.

import { h, replace } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { format } from '../shared/units.js';
import { formatDateTime, DAY } from '../shared/time.js';
import { downloadCsv, csvFilename } from '../shared/csv.js';
import { pageHead, section, figure, figures, chip, flag, pageLink, button, icon, empty, errorView, errorWords, ago } from '../core/ui.js';
import { field, form, setFieldError, switchField, choice, carOptions } from '../core/form.js';
import { dataTable } from '../core/table.js';
import { confirmDialog } from '../core/dialog.js';
import { teslaPart, showTeslaHash } from './integrations-tesla.js';
import { teslafiPart } from './integrations-teslafi.js';

export const id = 'settings-integrations';
export const css = true;
export const title = () => 'Integrations';

/* ------------------------------------------------------------------ what each value is */

// A value's status: what a consumer can count on today.
const STATUS = {
  published: { word: 'Published', kind: 'on', text: 'Sent now as its own retained topic.' },
  device: { word: 'On the device', kind: 'on', text: 'Sent once with the Home Assistant device, not as a topic.' },
  logged: { word: 'Kept, not sent yet', kind: null, text: 'Sentry Mate records or works it out; it is not its own topic yet.' },
  signal: { word: 'Needs a CAN signal', kind: 'plug', text: 'The car sends it; Sentry Mate does not decode it yet. Nothing is guessed meanwhile.' },
  none: { word: 'Not on the CAN bus', kind: 'warn', text: 'No message on the car’s buses carries it, so it is never published.' },
  never: { word: 'Never published', kind: 'warn', text: 'Left out on purpose.' },
};
const STATUS_ORDER = ['published', 'device', 'logged', 'signal', 'none', 'never'];

// [item id, topic key(s), name, unit kind, status, where it comes from, note]
const VALUES = [
  ['mqtt.display-name', null, 'Car name', null, 'device', 'Settings: Cars', 'The Home Assistant device name. Topics use the car id, so renaming a car never moves them.'],
  ['mqtt.state', 'state', 'State', 'text', 'published', 'The timeline', 'driving, charging, online, asleep or unknown. Unknown (no log, recording paused) is never called asleep.'],
  ['mqtt.since', 'since', 'State since', 'time', 'logged', 'Start of the current timeline span', 'Exact to the CAN frame, not to a poll.'],
  ['mqtt.healthy', 'healthy', 'Logger healthy', 'bool', 'published', 'The Data health checks', 'False when no log came while the car was awake, the newest log failed its check, or frames were lost.'],
  ['mqtt.version', null, 'Software version', null, 'none', 'No version text on CAN', 'No version topic. Instead the software_changed event (Events below) is sent when the module build ids change, with the version you typed on the Software page.'],
  ['mqtt.update-available', null, 'Update waiting', null, 'none', 'Only Tesla’s servers know', 'Left out.'],
  ['mqtt.update-version', null, 'Version of the waiting update', null, 'none', 'Only Tesla’s servers know', 'Left out; software_changed tells you afterwards.'],
  ['mqtt.download-perc', null, 'Update download progress', 'pct', 'none', 'No progress on CAN', 'Left out.'],
  ['mqtt.install-perc', 'installing', 'Update installing', 'bool', 'published', 'Update status (0x343 VCRIGHT_OTAState)', 'There is no percentage on CAN: an installing true / false instead.'],
  ['mqtt.vin', null, 'VIN', null, 'never', 'Only its first 11 characters are kept', 'Never published, so no broker or Home Assistant ever holds it.'],
  ['mqtt.model', null, 'Model and year', null, 'device', 'The VIN prefix', 'The Home Assistant device model.'],
  ['mqtt.trim-badging', 'trim', 'Trim', 'text', 'logged', 'The VIN prefix (drive units, battery)', 'Shown in Settings: Cars; the car configuration message can confirm it.'],
  ['mqtt.exterior-color', 'paint', 'Paint', 'text', 'published', 'Car configuration (0x7FF GTW_exteriorColor)', 'Newer paints come as a bare code; you can type the name in Settings: Cars.'],
  ['mqtt.wheel-type', 'wheels', 'Wheels', 'text', 'published', 'Car configuration (0x7FF GTW_wheelType)', ''],
  ['mqtt.spoiler-type', 'spoiler', 'Spoiler', 'text', 'published', 'Car configuration (0x7FF GTW_spoilerType)', 'None or passive.'],
  ['mqtt.geofence', 'place', 'Place', 'text', 'published', 'Your saved places', 'The saved place the car is in, or the no-place text set above.'],
  ['mqtt.latitude-longitude-deprecated', ['latitude', 'longitude'], 'Latitude and longitude', 'deg', 'published', 'The car’s GPS', 'Only with location publishing on; the topics are empty while it is off.'],
  ['mqtt.location', 'location', 'Position (JSON)', 'json', 'published', 'The car’s GPS with its accuracy', 'Only with location publishing on: latitude, longitude and gps_accuracy in metres.'],
  ['mqtt.shift-state', 'gear', 'Gear', 'text', 'published', 'Drive inverter (0x118 DI_gear)', 'P, R, N or D; held while parked, when the bus is quiet.'],
  ['mqtt.power', 'power', 'Battery power', 'kW', 'published', 'Pack volts × amps (0x132)', 'Positive out of the pack, negative while charging or braking. Always kW.'],
  ['mqtt.speed', 'speed', 'Speed', 'speed', 'published', 'Wheel speed (0x257)', ''],
  ['mqtt.heading', 'heading', 'Heading', 'deg', 'published', 'GPS heading (0x3D9 UI_gpsVehicleHeading, chassis bus)', 'Only while driving: the chassis bus is quiet when parked.'],
  ['mqtt.elevation', 'elevation', 'Elevation', 'elevation', 'published', 'The car’s GPS height (0x3D8)', 'Kept with every drive, from the car, with no map lookup.'],
  ['mqtt.locked', null, 'Locked', null, 'none', 'No lock state on CAN', 'Only a lock request exists, which is not the state. Never guessed.'],
  ['mqtt.sentry-mode', 'sentry_mode', 'Sentry mode', 'bool', 'published', 'Vehicle modes (0x284 UIsentryMode284)', ''],
  ['mqtt.windows-open', null, 'Any window open', null, 'none', 'Only window switch presses on CAN', 'Left out: a switch press cannot tell open from closed.'],
  ['mqtt.window-each', null, 'Each window', null, 'none', 'Only window switch presses on CAN', 'Left out.'],
  ['mqtt.doors-open', 'doors_open', 'Any door open', 'bool', 'published', 'Door latches (0x102, 0x103)', ''],
  ['mqtt.door-each', ['door_front_left', 'door_front_right', 'door_rear_left', 'door_rear_right'], 'Each door', 'bool', 'published', 'Door latches (0x102 left, 0x103 right)', 'Named by side, not by driver or passenger.'],
  ['mqtt.sun-roof-state', null, 'Sunroof position', null, 'none', 'No roof position on CAN', 'The roof type below instead.'],
  ['mqtt.sun-roof-installed', 'roof', 'Roof type', 'text', 'published', 'Car configuration (0x7FF GTW_roofType)', 'Metal, fixed glass or panoramic.'],
  ['mqtt.sun-roof-percent-open', null, 'Sunroof opening', 'pct', 'none', 'No roof position on CAN', 'Left out.'],
  ['mqtt.trunk-open', 'trunk_open', 'Trunk open', 'bool', 'published', 'Trunk latch (0x103), liftgate (0x142)', 'A power liftgate also gives its angle.'],
  ['mqtt.frunk-open', 'frunk_open', 'Frunk open', 'bool', 'published', 'Frunk latch (0x2E1 VCFRONT_frunkLatchStatus)', ''],
  ['mqtt.is-user-present', 'user_present', 'Someone in the car', 'bool', 'published', 'Occupancy (0x3B3 UI_userPresent)', 'The user_present event is sent too.'],
  ['mqtt.is-climate-on', 'climate_on', 'Climate on', 'bool', 'published', 'HVAC status (0x243 VCRIGHT_hvacPowerState)', ''],
  ['mqtt.inside-temp', 'inside_temp', 'Inside temperature', 'temperature', 'published', 'Cabin estimate (0x243)', ''],
  ['mqtt.outside-temp', 'outside_temp', 'Outside temperature', 'temperature', 'published', 'Ambient sensor (0x2E1, 0x343)', ''],
  ['mqtt.is-preconditioning', ['preconditioning_cabin', 'preconditioning_battery'], 'Preconditioning', 'bool', 'published', 'HVAC status (0x243), battery heating request (0x082, 0x3B3)', 'Cabin and battery apart, not one flag.'],
  ['mqtt.odometer', 'odometer', 'Odometer', 'distance', 'published', 'The car’s odometer (0x3F3, 0x3B6)', 'Only ever grows.'],
  ['mqtt.est-battery-range-km', 'range_estimated', 'Estimated range', 'distance', 'published', 'Energy left ÷ your recent consumption', 'Sentry Mate’s own estimate, named as such.'],
  ['mqtt.rated-battery-range-km', 'range', 'Rated range', 'distance', 'published', 'The car’s range (0x33A UI_Range)', 'Comes in 1 mile steps.'],
  ['mqtt.ideal-battery-range-km', 'range_ideal', 'Ideal range', 'distance', 'published', 'The car’s ideal range (0x33A UI_idealRange)', ''],
  ['mqtt.battery-level', 'soc', 'Battery level', 'pct', 'published', 'State of charge as the screen shows it (0x33A UI_SOC)', ''],
  ['mqtt.usable-battery-level', 'soc_usable', 'Usable battery level', 'pct', 'published', 'Usable state of energy (0x33A UI_uSOE)', 'Lower than the battery level while the pack is cold.'],
  ['mqtt.plugged-in', 'plugged_in', 'Plugged in', 'bool', 'published', 'Charge cable state (0x25D CP_chargeCableState)', 'A cable is in, charging or not; charge_kind still says ac or dc.'],
  ['mqtt.charging-state', 'charge_status', 'Charging status', 'text', 'published', 'Charge status word (0x212 BMS_uiChargeStatus)', 'One of disconnected, no_power, about_to_charge, charging, complete or stopped.'],
  ['mqtt.charge-energy-added', 'charge_energy_added', 'Energy added', 'kWh', 'published', 'The pack’s lifetime charge counter (0x3D2)', 'The lifetime counter, so it only grows; charge_ended carries each charge.'],
  ['mqtt.charge-limit-soc', 'charge_limit', 'Charge limit', 'pct', 'published', 'Charge request (0x333 UI_chargeTerminationPct)', ''],
  ['mqtt.charge-port-door-open', 'charge_port_open', 'Charge port open', 'bool', 'published', 'Charge port (0x25D CP_chargeDoorOpen)', ''],
  ['mqtt.charger-actual-current', 'charger_current', 'Charger current', 'A', 'published', 'AC line current (0x264), DC charger current', 'Sent while charging; also kept on every charge curve.'],
  ['mqtt.charger-phases', 'charger_phases', 'Phases in use', 'phases', 'published', 'Charger phase enables (0x204)', ''],
  ['mqtt.charger-power', 'charge_power', 'Charge power', 'kW', 'published', 'Pack side, volts × amps', 'Always kW.'],
  ['mqtt.charger-voltage', 'charger_voltage', 'Charger voltage', 'V', 'published', 'AC line voltage (0x264), DC charger voltage', 'Sent while charging; also kept on every charge curve.'],
  ['mqtt.charge-current-request', 'charge_current_request', 'Current asked for', 'A', 'published', 'The current set on the screen (0x333)', ''],
  ['mqtt.charge-current-request-max', 'charge_current_limit', 'Current limit', 'A', 'published', 'Pilot, cable and car limits (0x21D, 0x13D)', 'Says which of the three limits applies.'],
  ['mqtt.scheduled-charging-start-time', null, 'Scheduled charge start', null, 'none', 'Only schedule on / off flags on CAN', 'Instead: “plugged in, waiting”, and the usual start time at that place from past charges.'],
  ['mqtt.time-to-full-charge', 'time_to_full', 'Time to full', 'min', 'published', 'Time remaining (0x132)', ''],
  ['mqtt.tpms-pressure', ['tire_pressure_fl', 'tire_pressure_fr', 'tire_pressure_rl', 'tire_pressure_rr'], 'Tyre pressures', 'pressure', 'published', 'Tyre sensors (0x219)', 'Front left, front right, rear left, rear right.'],
  ['mqtt.tpms-soft-warning', 'tire_low', 'Tyre pressure low', 'bool', 'published', 'Pressure against the recommended one (0x219 page 4)', ''],
  ['mqtt.active-route', null, 'Route (destination, distance, minutes, traffic)', null, 'none', 'Not on CAN', 'Only navigating and the energy at the destination are.'],
  ['mqtt.active-route-payload-routing', 'energy_at_destination', 'Energy at destination', 'kWh', 'published', 'Trip planning (0x082 UI_energyAtDestination)', 'While navigating.'],
  ['mqtt.active-route-payload-idle', 'navigating', 'Navigating', 'bool', 'published', 'Trip planning (0x082), map data (0x238)', 'A plain false when no route is set, not an error text.'],
  ['mqtt.center-display-state', 'display_mode', 'Screen mode', 'text', 'published', 'Display on (0x00C), modes (0x284), keep climate (0x2F3)', 'Named modes (off, on, sentry, dog, camp), not bare numbers.'],
  ['mqtt.service-mode', 'service_mode', 'Service mode', 'bool', 'published', 'Vehicle modes (0x284 UIserviceMode284)', ''],
  // published by Sentry Mate with no counterpart above
  [null, 'driving', 'Driving', 'bool', 'published', 'The timeline', ''],
  [null, 'charging', 'Charging', 'bool', 'published', 'Charge status (0x212)', ''],
  [null, 'charge_kind', 'Charger kind', 'text', 'published', 'Charger type (0x13D)', 'ac or dc; nothing while no charger is connected.'],
  [null, 'energy_remaining', 'Energy remaining', 'kWh', 'published', 'The pack (0x352)', ''],
  [null, 'battery_temp', 'Battery temperature', 'temperature', 'published', 'Cell temperatures', ''],
  [null, 'lv_voltage', '12 V battery', 'V', 'published', 'Low-voltage battery', ''],
  [null, 'updated', 'Last data', 'time', 'published', 'The newest decoded frame', 'Every value set carries its time: read it before trusting a value.'],
].map(([cid, keys, name, unit, status, source, why]) => ({ cid, keys: keys == null ? [] : [].concat(keys), name, unit, status, source, why }));

// Home Assistant entity groups: [item id, name, what it gives, topics, status, note]
const ENTITY_GROUPS = [
  ['ha.entity.location-tracker', 'Device tracker', 'The car on Home Assistant’s map, home or away by zone', 'location', 'published', 'Only with location publishing on; about 75 s behind the car.'],
  ['ha.entity.active-route-tracker', 'Destination tracker', 'Where the car is navigating to', null, 'none', 'The destination is not on CAN.'],
  ['ha.entity.status-sensors', 'Status sensors', 'State, charging, screen mode, last data', 'state, charging, display_mode, updated', 'published', 'State since comes next; software version and waiting updates are not on CAN.'],
  ['ha.entity.config-sensors', 'Build sensors', 'Paint, wheels, spoiler, roof', 'paint, wheels, spoiler, roof', 'published', 'Model and year are on the device already; trim is not sent yet.'],
  ['ha.entity.motion-sensors', 'Motion sensors', 'Place, gear, power, speed, heading, elevation', 'place, gear, power, speed, heading, elevation', 'published', 'Heading only while driving: the chassis bus is quiet when parked.'],
  ['ha.entity.parking-brake', 'In park', 'On while the gear is P', 'in_park', 'published', 'From the gear; the real parking brake state (0x228, 0x288) is on CAN too.'],
  ['ha.entity.temperature-sensors', 'Temperature sensors', 'Inside, outside and battery temperature', 'inside_temp, outside_temp, battery_temp', 'published', 'Battery temperature is extra.'],
  ['ha.entity.range-sensors', 'Range sensors', 'Odometer, rated, ideal and estimated range', 'odometer, range, range_ideal, range_estimated', 'published', 'Estimated range is Sentry Mate’s own, named as such.'],
  ['ha.entity.charging-sensors', 'Charging sensors', 'Battery, usable level, charge status, limit, charge power, charger, current, voltage, phases, current asked for, current limit, time to full, energy remaining', 'soc, soc_usable, charge_status, charge_limit, charge_power, charge_kind, charger_*, charge_current_*, time_to_full, energy_remaining', 'published', 'Current, voltage, phases and time to full are sent while charging.'],
  ['ha.entity.charge-energy-added.counter', 'Energy added counter', 'A running total for Home Assistant’s energy view', 'charge_energy_added', 'published', 'Sent as the pack’s lifetime counter (total_increasing), which never resets, rather than a per-charge value that does.'],
  ['ha.entity.tpms-sensors', 'Tyre sensors', 'The four tyre pressures, pressure low', 'tire_pressure_fl … rr, tire_low', 'published', ''],
  ['ha.entity.active-route-destination', 'Route destination', 'Destination name', null, 'none', 'Not on CAN.'],
  ['ha.entity.active-route-energy', 'Route energy', 'Navigating, battery expected on arrival', 'navigating, energy_at_destination', 'published', 'A real 0 when it is 0; unavailable while not navigating.'],
  ['ha.entity.active-route-distance', 'Route distance', 'Distance left', null, 'none', 'Not on CAN.'],
  ['ha.entity.active-route-minutes', 'Route minutes', 'Minutes left', null, 'none', 'Not on CAN.'],
  ['ha.entity.active-route-traffic', 'Route traffic', 'Minutes lost to traffic', null, 'none', 'Not on CAN.'],
  ['ha.entity.binary-status', 'Health on / off', 'Logger healthy, update installing, service mode', 'healthy, installing, service_mode', 'published', 'A waiting update is not on CAN; software_changed says when one was installed.'],
  ['ha.entity.binary-security', 'Security on / off', 'Doors (any and each), trunk, frunk, Sentry mode, someone in the car', 'doors_open, door_*, trunk_open, frunk_open, sentry_mode, user_present', 'published', 'Lock and windows are not on CAN and are never guessed.'],
  ['ha.entity.binary-climate-charge', 'Climate and charging on / off', 'Climate, preconditioning, cable, charge port', 'climate_on, preconditioning_*, plugged_in, charge_port_open', 'published', ''],
  ['ha.proximity', 'Distance to home', 'Home Assistant’s own proximity helper, fed by the device tracker', 'location', 'published', 'Needs location publishing on.'],
].map(([cid, name, gives, topics, status, why]) => ({ cid, name, gives, topics, status, why }));

// The events: one topic per car, never retained.
const EVENTS = [
  ['drive_started', 'A drive starts', 'drive id, start time and place, battery, range and odometer'],
  ['drive_ended', 'A drive ends', 'the whole drive: distance, duration, energy, efficiency, top speed, power, end place, battery, range, temperatures, climb'],
  ['charge_started', 'A charge starts', 'charge id, ac or dc, place, battery at the start'],
  ['charge_ended', 'A charge ends', 'energy added, battery at the end, battery temperature, cost where the place has a price'],
  ['software_changed', 'Module builds changed', 'the update id and, when known, the version you typed or one from a matching build'],
  ['user_present', 'Someone is seen in the car', 'the time and the place'],
];
const EVENT_TYPES = EVENTS.map((e) => e[0]);

const ERROR_WORDS = {
  connection_refused: 'the broker refused the connection',
  pin_mismatch: 'the broker’s certificate did not match the pin',
  no_host: 'no broker host is set',
  tls_failed: 'the TLS handshake failed',
  auth_failed: 'the broker refused the user name or password',
};

const NET_PURPOSE = {
  geocoding: ['Address lookup', 'Only when an admin turns online addresses on; positions are rounded first.', 'settings-display'],
  tiles: ['Map pictures', 'Asked for by each viewer’s browser while online tiles are on.', 'settings-display'],
  geonames_download: ['Place-name download', 'Only when an admin asks for new offline place names.', 'settings-display'],
  mqtt: ['MQTT broker', 'Only while publishing is on (this page).', null],
  release_check: ['Release check', 'Asks whether a newer Sentry Mate exists, when turned on.', 'settings-display'],
  tesla: ['Tesla API', 'Only while a Tesla account is added (Tesla account, on this page): signs in and reads its cars, never sends a command.', null],
  teslafi: ['TeslaFi API', 'Only while a TeslaFi token is added and not paused: reads your cars’ data from TeslaFi', null],
  upload: ['CAN Bridge uploads', 'The car’s CAN Bridge sends its logs here.', null],
  web: ['This app and the API', 'Browsers, phone apps and scripts.', null],
};

/* ------------------------------------------------------------------ small pieces */

const code = (t) => h('code', { class: 'si-code' }, t);
// A long topic: may break after each / or _, never inside a word.
const topic = (t) => h('code', { class: 'si-code si-topic' }, String(t).split(/(?<=[/_])/).flatMap((p, i) => (i ? [h('wbr'), p] : [p])));
const statusFlag = (s) => chip(STATUS[s].word, { kind: STATUS[s].kind, title: STATUS[s].text });

function unitText(kind, u) {
  const mi = u && u.distance === 'mi';
  switch (kind) {
    case 'distance': return mi ? 'mi' : 'km';
    case 'speed': return mi ? 'mph' : 'km/h';
    case 'elevation': return mi ? 'ft' : 'm';
    case 'temperature': return u && u.temperature === 'f' ? '°F' : '°C';
    case 'pressure': return u && u.pressure === 'psi' ? 'psi' : 'bar';
    case 'pct': return '%';
    case 'deg': return '°';
    case 'bool': return 'true / false';
    case 'time': return 'ISO time, UTC';
    case 'json': return 'JSON';
    case 'phases': return '1 to 3';
    case 'min': return 'min';
    case 'text': return 'text';
    case null: case undefined: return '';
    default: return kind;
  }
}

// A rule: its name and one paragraph (text, or text mixed with inline nodes), then any controls.
function rule(name, text, { cover = null, cls = null, extra = null } = {}) {
  return h('div', { class: ['si-rule', cls], cover }, h('h3', { class: 'si-rule-name' }, name), text ? h('p', null, text) : null, extra);
}

function setting(name, desc, control, { cover = null, extra = null } = {}) {
  return h('div', { class: 'si-setting', cover },
    h('div', { class: 'si-setting-text' }, h('h3', { class: 'si-setting-name' }, name), desc ? h('p', { class: 'si-setting-desc' }, desc) : null, extra),
    h('div', { class: 'si-setting-control' }, control));
}

function copyButton(ctx, textFn, label = 'Copy') {
  return button(label, {
    cls: 'button-small',
    onclick: async () => {
      try { await navigator.clipboard.writeText(textFn()); ctx.flash('Copied.'); } catch { ctx.flash('This browser did not allow copying; select the text instead.', { kind: 'info' }); }
    },
  });
}

const csvButton = (onclick) => h('button', { type: 'button', class: 'button button-quiet button-small', dataset: { csv: '' }, onclick }, icon('download'), 'Download CSV');

// A unit for a key of an event payload, from the payload's own units.
function eventUnit(k, units, currency) {
  const u = units || {};
  if (/(^|_)(distance|range|odometer)($|_)/.test(k)) return u.distance || 'km';
  if (/speed/.test(k)) return u.speed || 'km/h';
  if (/temp/.test(k)) return u.temperature || '°C';
  if (/efficiency/.test(k)) return u.efficiency || 'Wh/km';
  if (/^(ascent|descent)$/.test(k)) return u.elevation || 'm';
  if (/_kwh$/.test(k)) return 'kWh';
  if (/_kw$/.test(k)) return 'kW';
  if (/soc/.test(k)) return '%';
  if (/_min$/.test(k)) return 'min';
  if (k === 'cost') return currency || u.currency || 'in the place’s currency';
  return '';
}
function payloadParts(p) {
  return Object.entries(p || {}).filter(([k]) => !['event_type', 'car', 'units'].includes(k)).map(([k, v]) => {
    const val = v == null ? '—' : typeof v === 'object' ? JSON.stringify(v) : String(v);
    const unit = v == null || typeof v !== 'number' ? '' : eventUnit(k, p.units, p.currency);
    return `${k} ${val}${unit ? ` ${unit}` : ''}`;
  });
}

/* ================================================================== the page */

export async function mount(ctx) {
  const admin = ctx.me && ctx.me.role === 'admin';
  const [cfgFirst, statusFirst, netFirst, carsFirst] = await Promise.all([
    admin ? ctx.api.get('settings/mqtt') : Promise.resolve(null),
    ctx.api.get('settings/mqtt/status').catch((e) => { if (api.isAbort(e)) throw e; return null; }),
    admin ? ctx.api.get('network').catch((e) => { if (api.isAbort(e)) throw e; return null; }) : Promise.resolve(null),
    ctx.api.get('cars', { sort: 'name', limit: 100 }),
  ]);
  let cfg = cfgFirst;
  let status = statusFirst;
  let net = netFirst;
  let carList = carsFirst.items || [];
  const carTotal = carsFirst.total ?? carList.length;
  let carId = ctx.query.car || (carList[0] && carList[0].id) || null;
  let pv = null;

  const carOf = (cid) => carList.find((c) => c.id === cid) || null;
  const carTz = () => (carOf(carId) && carOf(carId).time_zone) || ctx.tz;

  async function loadPreview() {
    if (!carId) { pv = null; return; }
    try { pv = await ctx.api.get('settings/mqtt/preview', { car: carId }); } catch (e) {
      if (api.isAbort(e)) throw e;
      pv = { error: e, messages: [], ha_entities: [] };
    }
  }
  await loadPreview();
  if (!ctx.alive()) return undefined;

  // What the settings say, also for an account that may not read them (from the preview's topics).
  const ns = () => (cfg && cfg.namespace) || ((pv && pv.messages && pv.messages[0] && pv.messages[0].topic) || 'sentrymate/status').replace(/\/status$/, '');
  const units = () => (pv && pv.units) || { distance: 'km', temperature: 'c', pressure: 'bar' };
  const locationOn = () => (cfg ? !!cfg.publish_location : !!(pv && pv.messages || []).find((m) => m.topic.endsWith('/location') && m.payload));
  const haOn = () => (cfg ? !!cfg.ha_discovery.enabled : !!(pv && pv.ha_entities && pv.ha_entities.length));

  /* ---------------- saving: PUT the whole setting with its version */
  const whole = (patch) => {
    const b = {
      enabled: cfg.enabled, host: cfg.host, port: cfg.port, username: cfg.username, tls: cfg.tls, pin_sha256: cfg.pin_sha256,
      namespace: cfg.namespace, publish_location: cfg.publish_location, ha_discovery: { ...cfg.ha_discovery }, no_place_text: cfg.no_place_text,
      version: cfg.version,
    };
    Object.assign(b, patch, { ha_discovery: { ...b.ha_discovery, ...(patch.ha_discovery || {}) } });
    return b;
  };
  async function put(patch) {
    try {
      cfg = await ctx.api.put('settings/mqtt', whole(patch));
      return true;
    } catch (e) {
      if (e && e.code === 'stale_version') {
        cfg = await ctx.api.get('settings/mqtt');
        ctx.flash('Someone changed these settings meanwhile; they are shown fresh now. Make your change again.', { kind: 'info' });
        await refreshAll();
        return false;
      }
      throw e;
    }
  }
  // A switch or single field: saved at once, the page drawn again; an error says what and puts it back.
  async function quick(patch, what) {
    try {
      if (await put(patch)) { ctx.flash(`${what} saved.`); await refreshAll(); }
      return null;
    } catch (e) {
      if (api.isAbort(e)) return e;
      const words = e && e.fields ? Object.values(e.fields).join(' ') : errorWords(e).text;
      ctx.flash(`Not saved: ${words}`, { kind: 'error' });
      await refreshAll();
      return e;
    }
  }
  async function refreshStatus() {
    [status, net] = await Promise.all([
      ctx.api.get('settings/mqtt/status').catch((e) => { if (api.isAbort(e)) throw e; return status; }),
      admin ? ctx.api.get('network').catch((e) => { if (api.isAbort(e)) throw e; return net; }) : Promise.resolve(net),
    ]);
  }
  async function refreshAll() {
    await Promise.all([refreshStatus(), loadPreview()]);
    if (!ctx.alive()) return;
    paintAll();
  }

  /* ---------------- the Tesla account (its own module) */
  const tesla = teslaPart(ctx);
  const teslafi = teslafiPart(ctx);
  const teslaLoad = Promise.all([tesla.load(), teslafi.load()]);
  const onHash = () => showTeslaHash();
  window.addEventListener('hashchange', onHash);
  ctx.onCleanup(() => window.removeEventListener('hashchange', onHash));

  /* ---------------- the head */
  const headStatus = h('div', { class: 'si-head-status' });
  function paintHead() {
    if (!status) { replace(headStatus, chip('Status not known', { kind: 'warn' })); return; }
    replace(headStatus,
      !status.enabled ? chip('Publishing off')
        : status.connected ? chip(`Connected to ${status.broker}`, { kind: 'on', iconName: 'check' })
          : chip('Not connected', { kind: 'warn', iconName: 'warning' }),
      h('p', { class: 'si-head-note' }, status.enabled ? `${format(status.delivered, 'count')} messages delivered since the server started` : 'Nothing leaves the server until publishing is on'));
  }
  const JUMPS = [['tesla', 'Tesla account'], ['teslafi', 'TeslaFi'], ['si-broker', 'Broker'], ['si-topics', 'Topics'], ['si-values', 'Values'], ['si-ha', 'Home Assistant'], ['si-events', 'Events'], ['si-api', 'API'], ['si-network', 'Network'], ['si-works', 'Works with']];
  const jumpNav = h('nav', { class: 'si-jump', 'aria-label': 'Parts of this page' },
    h('span', { class: 'si-jump-label' }, 'On this page'),
    JUMPS.map(([target, label]) => h('button', { type: 'button', class: 'si-jump-link', onclick: () => { const t = document.getElementById(target); if (t) { t.scrollIntoView({ block: 'start' }); t.querySelector('h2')?.focus?.(); } } }, label)));

  /* ================================================================ MQTT broker */
  const brokerSlot = h('div', { class: 'si-broker-form' });
  const statusSlot = h('div', { class: 'si-status', 'aria-live': 'polite' });
  const logSlot = h('div', { class: 'si-log' });

  function paintBroker() {
    const brokerCovers = ['docs.env.mqtt.disable-mqtt', 'docs.env.mqtt.mqtt-host', 'docs.env.mqtt.mqtt-port', 'docs.env.mqtt.mqtt-username', 'docs.env.mqtt.mqtt-password', 'docs.env.mqtt.mqtt-tls', 'docs.env.mqtt.mqtt-tls-accept-invalid-certs', 'docs.env.mqtt.mqtt-ipv6', 'docs.env.mqtt.mqtt-namespace'];
    if (!cfg) {
      replace(brokerSlot,
        h('p', { class: 'si-lead' }, 'Only an admin sees and changes the broker settings. The live status, the preview and the events on this page are yours to read.'),
        h('dl', { class: 'si-facts', cover: brokerCovers },
          h('dt', null, 'Publishing'), h('dd', null, status && status.enabled ? 'on' : 'off'),
          h('dt', null, 'Broker'), h('dd', null, status && status.broker ? [code(status.broker), status.tls ? ' over TLS' : ' without TLS'] : 'not set'),
          h('dt', null, 'Topics start with'), h('dd', null, code(ns()))));
      return;
    }
    const onSw = switchField({
      label: 'Publish to the broker', checked: cfg.enabled, cover: 'docs.env.mqtt.disable-mqtt',
      hint: 'Off: Sentry Mate connects to no broker and sends nothing. On: every car you see below is published.',
      onchange: async (on) => {
        if (on && !cfg.host) { onSw.set(false); ctx.flash('Set the broker host and save the connection first.', { kind: 'info' }); hostF.control.focus(); return; }
        await quick({ enabled: on }, on ? 'Publishing on' : 'Publishing off');
      },
    });
    const hostF = field({ label: 'Host', name: 'host', value: cfg.host, cover: ['docs.env.mqtt.mqtt-host', 'docs.env.mqtt.mqtt-ipv6'],
      hint: 'A name such as broker.local, or an IPv4 address. For an IPv6 broker give a name that resolves to its IPv6 address: it is used as such, so there is no separate IPv6 switch.',
      attrs: { autocomplete: 'off', spellcheck: 'false', placeholder: 'broker.local' } });
    const portF = field({ label: 'Port', name: 'port', type: 'number', value: cfg.port, cover: 'docs.env.mqtt.mqtt-port',
      hint: '8883 is the usual port for MQTT over TLS, 1883 without.', attrs: { min: '1', max: '65535', step: '1', inputmode: 'numeric' } });
    const userF = field({ label: 'User name', name: 'username', value: cfg.username || '', cover: 'docs.env.mqtt.mqtt-username',
      hint: 'Empty for a broker without accounts.', attrs: { autocomplete: 'off', spellcheck: 'false' } });
    const passF = field({ label: 'Password', name: 'password', type: 'password', cover: 'docs.env.mqtt.mqtt-password',
      hint: cfg.password_set ? 'A password is saved; it is never shown again. Leave this empty to keep it.' : 'No password is saved. A password is only ever sent over TLS.',
      attrs: { autocomplete: 'new-password', placeholder: cfg.password_set ? 'saved, kept unless you type a new one' : '' } });
    const tlsSw = switchField({ label: 'Encrypt the connection (TLS)', checked: cfg.tls });
    const tlsF = h('div', { class: 'field si-tls', dataset: { field: 'tls' }, cover: 'docs.env.mqtt.mqtt-tls' }, tlsSw,
      h('p', { class: 'field-hint' }, 'On by default. Without TLS no password can be saved or sent.'), h('p', { class: 'field-error', hidden: true }));
    tlsF.control = tlsSw.input;
    const pinF = field({ label: 'Pinned certificate (SHA-256)', name: 'pin_sha256', value: cfg.pin_sha256 || '', cover: 'docs.env.mqtt.mqtt-tls-accept-invalid-certs',
      hint: 'For a broker with its own (self-signed) certificate: its SHA-256, 64 hex digits, colons allowed. Then only that certificate is accepted. There is no “accept any certificate” switch. Empty: the certificate must be valid for the host.',
      attrs: { autocomplete: 'off', spellcheck: 'false', placeholder: 'AB:CD:…' } });
    const nsF = field({ label: 'Topic prefix', name: 'namespace', value: cfg.namespace, cover: 'docs.env.mqtt.mqtt-namespace',
      hint: 'Every topic starts with this. Two servers on one broker use two prefixes. Levels with /, no + or #, not starting with $.',
      attrs: { autocomplete: 'off', spellcheck: 'false' } });

    const testOut = h('div', { class: 'si-test', 'aria-live': 'polite' });
    const testBtn = button('Test the saved settings', {
      iconName: 'check',
      onclick: async () => {
        testBtn.disabled = true;
        replace(testOut, h('p', { class: 'si-test-line' }, 'Connecting…'));
        try {
          const r = await ctx.api.post('settings/mqtt/test');
          if (!ctx.alive()) return;
          replace(testOut, h('p', { class: ['si-test-line', r.ok ? 'is-ok' : 'is-bad'] }, icon(r.ok ? 'check' : 'warning'), ' ',
            r.message || (r.ok ? 'Connected.' : ERROR_WORDS[r.error] || r.error),
            r.ok && r.latency_ms != null ? ` Answered in ${format(r.latency_ms, 'count')} ms.` : '',
            r.ok && r.topic ? [' Topic ', code(r.topic), '.'] : '',
            h('span', { class: 'si-quiet' }, ` Tested ${formatDateTime(r.at_ms, ctx.tz)}.`)));
          await refreshStatus();
          if (ctx.alive()) { paintStatus(); paintHead(); paintLog(); }
        } catch (e) {
          if (!api.isAbort(e)) replace(testOut, h('p', { class: 'si-test-line is-bad' }, icon('warning'), ' ', errorWords(e).text));
        } finally { testBtn.disabled = false; }
      },
    });
    const removePw = cfg.password_set ? button('Remove the saved password', {
      kind: 'danger', cls: 'button-small',
      onclick: async () => {
        if (!(await confirmDialog({ title: 'Remove the broker password?', text: 'The password is deleted from Sentry Mate. Publishing then connects with the user name alone, or none.', yes: 'Remove', danger: true }))) return;
        await quick({ password: null }, 'Password removal');
      },
    }) : null;

    const f = form({
      fields: [hostF, portF, userF, passF, tlsF, pinF, nsF], submitLabel: 'Save the connection', cls: 'si-form',
      extra: h('div', { class: 'si-form-extra' }, removePw),
      onsubmit: async (v) => {
        const patch = {
          host: String(v.host || '').trim(), port: v.port == null ? null : Math.round(v.port),
          username: String(v.username || '').trim() || null, tls: tlsSw.input.checked,
          pin_sha256: String(v.pin_sha256 || '').trim() || null, namespace: String(v.namespace || '').trim(),
        };
        if (v.password) patch.password = v.password;
        // The server's rules, said here first.
        const bad = {};
        if (cfg.enabled && !patch.host) bad.host = 'Publishing is on, so the broker needs a host.';
        if (patch.port == null || patch.port < 1 || patch.port > 65535) bad.port = 'A port from 1 to 65535.';
        if (patch.pin_sha256 && !patch.tls) bad.pin_sha256 = 'A certificate pin needs TLS.';
        if (patch.pin_sha256 && !/^([0-9A-Fa-f]{64}|([0-9A-Fa-f]{2}:){31}[0-9A-Fa-f]{2})$/.test(patch.pin_sha256)) bad.pin_sha256 = '64 hex digits, with or without colons.';
        if ((v.password || cfg.password_set) && !patch.tls) bad.tls = 'A password is only ever sent over TLS. Keep TLS on, or remove the password.';
        if (!patch.namespace || /[+#]/.test(patch.namespace) || patch.namespace.startsWith('$') || patch.namespace.split('/').some((p) => !p)) bad.namespace = 'No + or #, not starting with $, and no empty level.';
        if (Object.keys(bad).length) throw Object.assign(new Error('Not saved: check the fields marked.'), { fields: bad });
        if (await put(patch)) { ctx.flash('Broker connection saved.'); await refreshAll(); }
      },
    });
    f.querySelector('.form-actions').append(testBtn);
    replace(brokerSlot, onSw, f, testOut);
  }

  function paintStatus() {
    if (!status) { replace(statusSlot, errorView(new Error('The status did not answer.'), { retry: async () => { await refreshStatus(); if (ctx.alive()) { paintStatus(); paintHead(); } }, eyebrow: 'Status' })); return; }
    const s = status;
    const conn = !s.enabled ? 'Off' : s.connected ? 'Connected' : 'Not connected';
    replace(statusSlot,
      h('div', { class: 'si-sub-head' }, h('h3', { class: 'si-sub' }, 'Live status'),
        button('Refresh', { iconName: 'reset', cls: 'button-small', onclick: async () => { await refreshStatus(); if (ctx.alive()) { paintStatus(); paintHead(); } } })),
      figures([
        figure('Connection', conn, !s.enabled ? 'Publishing is off.' : s.broker ? `${s.broker}${s.tls ? ', TLS' : ', no TLS'}` : 'No broker set.', { cls: s.enabled && !s.connected ? 'si-bad' : null }),
        figure('Delivered', h('span', null, format(s.delivered, 'count'), h('span', { class: 'si-unit' }, ' messages')), 'Taken by the broker since the server started.'),
        figure('Waiting', h('span', null, format(s.waiting, 'count'), h('span', { class: 'si-unit' }, ' messages')), s.waiting ? 'Held while the broker is away; sent when it is back.' : 'Nothing held back.'),
        figure('Dropped', h('span', null, format(s.dropped, 'count'), h('span', { class: 'si-unit' }, ' messages')), 'Replaced by a newer value of the same topic while the broker was away.'),
      ], { cls: 'si-figs' }),
      h('p', { class: ['si-last-error', { 'is-bad': !!s.last_error }] },
        s.last_error ? ['Last error: ', ERROR_WORDS[s.last_error] || s.last_error, s.last_error_ms ? [', ', ago(s.last_error_ms), ` (${formatDateTime(s.last_error_ms, ctx.tz)})`] : '', '.']
          : 'No error since the server started.'),
      h('p', { class: 'si-quiet' }, 'Asked when this page opened; press Refresh for the newest figures.'));
  }

  async function paintLog() {
    if (!admin) { replace(logSlot, h('p', { class: 'si-quiet' }, 'The broker log (changes and tests) is shown to admins.')); return; }
    let rows = [];
    try {
      const now = api.now();
      // The audit log has no target filter: read page after page until 8 MQTT rows are found or the
      // whole 90 days have been read, so "nothing in 90 days" is only said once it is true.
      const p = ctx.api.pager('audit', { from_ms: now - 90 * DAY, to_ms: now + 60_000 }, { limit: 200 });
      while (!p.done && ctx.alive()) {
        const got = await p.more();
        rows = rows.concat((got || []).filter((e) => e.target === 'settings:mqtt'));
        if (rows.length >= 8) break;
      }
      rows = rows.slice(0, 8);
    } catch (e) {
      if (api.isAbort(e)) return;
      replace(logSlot, h('p', { class: 'si-quiet' }, 'The broker log could not be read.'));
      return;
    }
    if (!ctx.alive()) return;
    replace(logSlot,
      h('div', { class: 'si-sub-head' }, h('h3', { class: 'si-sub' }, 'Broker log', h('span', { class: 'label-note' }, 'changes and tests, last 90 days')),
        rows.length ? csvButton(() => downloadCsv(csvFilename('sentry-mate', 'mqtt-log'), rows, [
          { label: 'Time (UTC)', value: (r) => new Date(r.at_ms).toISOString() }, { label: 'Account', value: (r) => r.account },
          { label: 'Action', value: (r) => r.action }, { label: 'What happened', value: (r) => r.summary }])) : null),
      rows.length
        ? h('ol', { class: 'si-log-list' }, rows.map((r) => h('li', null, h('span', { class: 'si-log-time' }, formatDateTime(r.at_ms, ctx.tz)), h('span', { class: 'si-log-text' }, r.summary), h('span', { class: 'si-quiet' }, r.account || ''))))
        : h('p', { class: 'si-quiet' }, 'Nothing was changed or tested in the last 90 days.'),
      pageLink('The whole audit log in Server and access', 'settings-server'));
  }

  const clientSlot = h('div', { class: 'si-client', cover: 'nr.setup.mqtt' });
  function paintClient() {
    const n = ns();
    replace(clientSlot, h('h3', { class: 'si-sub' }, 'Connect a client'),
      h('p', null, 'Home Assistant, Node-RED or any MQTT client uses this same broker. Subscribe to ', topic(`${n}/cars/+/#`), ' for every car, or ', topic(`${n}/cars/<car id>/#`), ' for one; use TLS as set here.'),
      h('p', { class: 'si-quiet' }, 'Sentry Mate only publishes. It takes no commands, and nothing here can control a car.'));
  }

  const brokerSec = section({ title: 'MQTT broker', note: admin ? 'admins only to change' : 'read only for your account', id: 'si-broker', cls: 'si-sec' },
    h('div', { class: 'si-two' }, brokerSlot, h('div', { class: 'si-side' }, statusSlot, logSlot, clientSlot)));

  /* ================================================================ topics and rules */
  const topicsSlot = h('div');
  function paintTopics() {
    const n = ns();
    const c = carId || 'c1';
    const treeRow = (t, text) => h('li', { class: 'si-tree-row' }, topic(t), h('span', { class: 'si-tree-text' }, text));
    let noPlace = null;
    let locSw = null;
    if (cfg) {
      noPlace = field({ label: 'Text when the car is at no saved place', name: 'no_place_text', value: cfg.no_place_text ?? '',
        hint: 'Published as the place, up to 40 characters; saved when you leave the field. Empty publishes an empty place.', attrs: { maxlength: '40' } });
      noPlace.control.addEventListener('change', async () => {
        setFieldError(noPlace, null);
        const e = await quick({ no_place_text: noPlace.control.value.trim() }, 'No-place text');
        if (e && e.fields && ctx.alive()) setFieldError(noPlace, e.fields.no_place_text || null);
      });
      locSw = switchField({ label: 'Publish the car’s position', checked: cfg.publish_location,
        hint: 'Off by default. Off: latitude, longitude and location are published empty, so a broker never holds where the car is. The place name is always sent.',
        onchange: (on) => quick({ publish_location: on }, on ? 'Location publishing on' : 'Location publishing off') });
    }
    replace(topicsSlot,
      h('ul', { class: 'si-tree', cover: 'mqtt.topic-scheme', 'aria-label': 'Topic layout' },
        treeRow(`${n}/status`, 'online or offline. Retained; the broker sets offline by itself when the server goes away (its last will).'),
        treeRow(`${n}/cars/${c}/<value>`, 'One topic per value (the table below). Retained, QoS 1, text in the server’s units.'),
        treeRow(`${n}/cars/${c}/updated`, 'When the newest data behind these values was recorded (ISO time, UTC).'),
        treeRow(`${n}/cars/${c}/event`, 'One JSON message per event (Events below). Not retained, so a restart or a new subscriber never sees an old event as new.')),
      h('div', { class: 'si-rules' },
        rule('Car ids', ['Topics use the car’s id (', code(c), ' here), never its name or VIN, so renaming a car keeps every topic and automation working. The ids are listed in Settings: Cars.'], { cover: 'mqtt.note-car-id' }),
        rule('Missing values', 'A value the car did not send is not published at all: the broker keeps the last real value, and the updated topic says how old it is. Nothing is sent as 0 or as a made-up number.', { cover: 'mqtt.note-empty-values' }),
        rule('Units', `Payloads are text in the server’s units (now ${unitText('distance', units())}, ${unitText('temperature', units())}, ${unitText('pressure', units())}), set by an admin in Display and units. Power is always kW, energy kWh.`),
        rule('At no saved place', cfg ? null : 'While the car is at no saved place, the place topic carries a text an admin sets here (it can be empty). The other topics are sent as usual.', { cover: 'docs.env.core.default-geofence', extra: cfg ? [noPlace, locSw] : null })));
  }
  const topicsSec = section({ title: 'MQTT topics and rules', note: 'how the values are laid out and sent', id: 'si-topics', cls: 'si-sec' }, topicsSlot);

  /* ================================================================ published values */
  const carPickSlot = h('div', { class: 'si-carpick' });
  const valuesSlot = h('div');
  let valuesTable = null;
  let valueFilter = ctx.query.show || 'all';

  async function pickCar(cid) {
    carId = cid;
    ctx.setQuery({ car: cid });
    await loadPreview();
    if (!ctx.alive()) return;
    paintTopics(); paintClient(); paintCarPick(); paintValues(); paintHa(); paintEntities(); paintEvents(); paintWorks();
  }
  function paintCarPick() {
    if (!carList.length) { replace(carPickSlot); return; }
    const sel = h('select', { class: 'field-select', id: 'si-car', 'aria-label': 'Car for the preview' },
      carOptions(carList, { option: (c) => h('option', { value: c.id, selected: c.id === carId }, `${c.name} (${c.id})`) }));
    if (carId && !carOf(carId)) sel.prepend(h('option', { value: carId, selected: true }, carId));
    sel.addEventListener('change', () => pickCar(sel.value));
    let search = null;
    if (carTotal > carList.length) {
      search = h('input', { type: 'search', class: 'field-input field-search', placeholder: 'Find a car by name', 'aria-label': 'Find a car for the preview' });
      search.addEventListener('change', async () => {
        const r = await ctx.api.get('cars', { search: search.value.trim(), sort: 'name', limit: 100 });
        if (!ctx.alive()) return;
        carList = r.items || [];
        paintCarPick();
        if (carList[0] && !carOf(carId)) pickCar(carList[0].id);
      });
    }
    const car = carOf(carId);
    const at = car && car.now && car.now.at_ms;
    replace(carPickSlot,
      h('label', { class: 'field-label', for: 'si-car' }, 'Preview for'), sel, search,
      h('p', { class: 'si-quiet' }, carTotal > carList.length ? `${format(carList.length, 'count')} of ${format(carTotal, 'count')} cars listed; find others by name.` : `Every car you see is published the same way.`,
        at ? [' Newest data ', ago(at), '.'] : ''),
      carId ? pageLink('Car overview', 'car', { car: carId }) : null);
  }

  function valueRows() {
    const msgs = (pv && pv.messages) || [];
    const byKey = new Map();
    const prefix = `${ns()}/cars/${carId}/`;
    for (const m of msgs) if (m.topic.startsWith(prefix)) byKey.set(m.topic.slice(prefix.length), m);
    return VALUES.map((v) => ({ ...v, now: v.keys.map((k) => [k, byKey.get(k)]) }));
  }
  function nowCell(r, u) {
    if (r.status === 'device') {
      const d = pv && pv.device;
      if (!d) return h('span', { class: 'si-quiet' }, 'with discovery on');
      return h('span', null, r.cid === 'mqtt.model' ? d.model : d.name);
    }
    if (!r.keys.length) return h('span', { class: 'si-quiet' }, 'never sent');
    if (r.status !== 'published') return h('span', { class: 'si-quiet' }, 'not sent yet');
    if (r.now.every(([, m]) => !m)) return h('span', { class: 'si-quiet' }, 'not sent by this car (nothing known yet)');
    if (r.now.every(([, m]) => m && m.payload === '')) return h('span', { class: 'si-quiet' }, 'empty: location publishing is off');
    const parts = r.now.map(([k, m]) => {
      if (!m) return h('span', { class: 'si-quiet' }, r.keys.length > 1 ? `${k}: not sent by this car` : 'not sent by this car');
      if (m.payload === '') return h('span', { class: 'si-quiet' }, r.keys.length > 1 ? `${k}: empty` : 'empty (location publishing off)');
      const unit = ['text', 'bool', 'time', 'json'].includes(r.unit) ? '' : unitText(r.unit, u);
      const t = r.unit === 'time' ? Date.parse(m.payload) : NaN;
      return h('span', { class: 'si-payload' }, r.keys.length > 1 ? h('span', { class: 'si-quiet' }, `${k.split('_').pop()} `) : null, h('code', { class: 'si-code' }, m.payload), unit ? h('span', { class: 'si-unit' }, ` ${unit}`) : null,
        Number.isFinite(t) ? h('span', { class: 'si-quiet si-payload-note' }, `${formatDateTime(t, carTz())}, `, ago(t)) : null);
    });
    return h('span', { class: 'si-now' }, parts);
  }
  function paintValues() {
    if (!carId) { replace(valuesSlot, empty({ title: 'No car to preview.', text: 'Once a car has uploaded its logs, every value it publishes shows here.' })); valuesTable = null; return; }
    if (pv && pv.error) { replace(valuesSlot, errorView(pv.error, { retry: () => pickCar(carId), eyebrow: 'Preview' })); valuesTable = null; return; }
    const u = units();
    const all = valueRows();
    const rows = valueFilter === 'all' ? all : valueFilter === 'published' ? all.filter((r) => r.status === 'published' || r.status === 'device') : all.filter((r) => r.status !== 'published' && r.status !== 'device');
    const counts = STATUS_ORDER.map((s) => [s, all.filter((r) => r.status === s).length]).filter(([, n]) => n);
    const filter = choice({ label: 'Show', name: 'si-show', value: valueFilter, options: [['all', `All ${all.length}`], ['published', 'Sent now'], ['later', 'Not sent, and why']],
      onchange: (v) => { valueFilter = v; ctx.setQuery({ show: v === 'all' ? null : v }); paintValues(); } });
    const carName = (pv && pv.name) || (carOf(carId) && carOf(carId).name) || carId;
    valuesTable = dataTable(ctx, {
      rows, itemName: ['value', 'values'], label: 'Published values',
      caption: `previewed for ${carName}, exactly as a subscriber gets them now`,
      csv: { filename: csvFilename('sentry-mate', 'mqtt-values', carId), columns: [
        { label: 'Value', value: (r) => r.name }, { label: 'Topics', value: (r) => r.keys.map((k) => `${ns()}/cars/${carId}/${k}`).join(' ') },
        { label: `Now (${carName})`, value: (r) => r.now.map(([, m]) => (m ? m.payload : '')).join(' ') }, { label: 'Unit', value: (r) => unitText(r.unit, u) },
        { label: 'Status', value: (r) => STATUS[r.status].word }, { label: 'From', value: (r) => r.source }, { label: 'Note', value: (r) => r.why }] },
      columns: [
        { key: 'name', label: 'Value', th: true, sort: true, cell: (r) => h('span', { class: 'cell-main', cover: r.cid || null }, r.name, r.cid ? null : h('span', { class: 'si-extra' }, ' extra')) },
        { key: 'topic', label: 'Topic', cell: (r) => (r.keys.length ? h('span', { class: 'si-topics' }, r.keys.map((k) => topic(k))) : h('span', { class: 'si-quiet' }, '—')), value: (r) => r.keys.join(' ') },
        { key: 'now', label: `Now for ${carName}`, cell: (r) => nowCell(r, u), value: (r) => r.now.map(([, m]) => (m ? m.payload : '')).join(' ') },
        { key: 'unit', label: 'Unit', cls: 'si-nowrap', cell: (r) => unitText(r.unit, u) || h('span', { class: 'si-quiet' }, '—'), value: (r) => unitText(r.unit, u) },
        { key: 'status', label: 'Status', sort: true, cell: (r) => statusFlag(r.status), value: (r) => STATUS_ORDER.indexOf(r.status) },
        { key: 'source', label: 'From', cls: 'si-wrap', cell: (r) => h('span', null, h('span', { class: 'cell-main' }, r.source), r.why ? h('span', { class: 'cell-sub' }, r.why) : null), value: (r) => r.source },
      ],
      empty: () => empty({ title: 'No value with this status.', text: 'Choose “All” above.' }),
    });
    replace(valuesSlot,
      h('div', { class: 'si-values-top' }, filter,
        h('p', { class: 'si-counts' }, counts.map(([s, n], i) => [i ? ' · ' : '', h('b', null, format(n, 'count')), ` ${STATUS[s].word.charAt(0).toLowerCase()}${STATUS[s].word.slice(1)}`]))),
      valuesTable.el,
      h('dl', { class: 'si-key' }, STATUS_ORDER.map((s) => [h('dt', null, statusFlag(s)), h('dd', null, STATUS[s].text)])),
      messagesBlock());
    valuesTable.load();
  }
  // Every message exactly as the broker gets it, in one list.
  function messagesBlock() {
    const msgs = (pv && pv.messages) || [];
    if (!msgs.length) return null;
    const t = dataTable(ctx, {
      rows: msgs, itemName: ['message', 'messages'], label: 'Messages a broker gets now',
      csv: { filename: csvFilename('sentry-mate', 'mqtt-messages', carId), columns: [{ label: 'Topic', value: (m) => m.topic }, { label: 'Payload', value: (m) => m.payload }, { label: 'Retained', value: (m) => (m.retained ? 'yes' : 'no') }] },
      columns: [
        { key: 'topic', label: 'Topic', th: true, cell: (m) => topic(m.topic) },
        { key: 'payload', label: 'Payload (server units)', cls: 'si-wrap', cell: (m) => (m.payload === '' ? h('span', { class: 'si-quiet' }, 'empty') : h('code', { class: 'si-code' }, m.payload)) },
        { key: 'retained', label: 'Retained', cell: (m) => (m.retained ? 'yes' : 'no') },
      ],
    });
    t.load();
    return h('details', { class: 'si-more' }, h('summary', null, `Every message a broker gets now (${msgs.length}), raw`), t.el);
  }
  const valuesSec = section({ title: 'MQTT published values', note: 'each with its unit, where it comes from, and whether it is sent', id: 'si-values', cls: 'si-sec' },
    carPickSlot, valuesSlot);

  /* ================================================================ Home Assistant discovery */
  const haSlot = h('div');
  function paintHa() {
    const ents = (pv && pv.ha_entities) || [];
    const dev = pv && pv.device;
    const prefix = cfg ? cfg.ha_discovery.prefix : (ents[0] && ents[0].config_topic.split('/')[0]) || 'homeassistant';
    let controls;
    if (cfg) {
      const haSw = switchField({ label: 'Home Assistant discovery', checked: cfg.ha_discovery.enabled,
        hint: 'On: Home Assistant finds one device per car with its entities by itself, with nothing to write in its configuration.',
        onchange: (on) => quick({ ha_discovery: { enabled: on } }, on ? 'Discovery on' : 'Discovery off') });
      const prefixF = field({ label: 'Discovery prefix', name: 'ha_prefix', value: cfg.ha_discovery.prefix, hint: 'Home Assistant listens on homeassistant unless you changed it there. Saved when you leave the field.', attrs: { spellcheck: 'false', autocomplete: 'off' } });
      prefixF.control.addEventListener('change', async () => {
        setFieldError(prefixF, null);
        const e = await quick({ ha_discovery: { prefix: prefixF.control.value.trim() } }, 'Discovery prefix');
        if (e && e.fields && ctx.alive()) setFieldError(prefixF, e.fields['ha_discovery.prefix'] || null);
      });
      const urlF = field({ label: 'Address of this app, for Home Assistant', name: 'ha_url', value: cfg.ha_discovery.url || '', hint: 'Optional, https:// only. Each car’s device in Home Assistant links here (“Visit”). Empty: no link. Saved when you leave the field.', attrs: { spellcheck: 'false', autocomplete: 'off', placeholder: `https://${location.host}` } });
      urlF.control.addEventListener('change', async () => {
        setFieldError(urlF, null);
        const v = urlF.control.value.trim();
        if (v && !/^https:\/\/[^\s/]+(\/\S*)?$/.test(v)) { setFieldError(urlF, 'An https:// address, or empty.'); return; }
        const e = await quick({ ha_discovery: { url: v || null } }, 'App address');
        if (e && e.fields && ctx.alive()) setFieldError(urlF, e.fields['ha_discovery.url'] || null);
      });
      controls = h('div', { class: 'si-ha-controls' },
        h('div', { cover: 'ha.discovery.enable' }, haSw),
        h('div', { cover: 'ha.discovery.prefix' }, prefixF),
        h('div', { cover: 'ha.discovery.url' }, urlF));
    } else {
      controls = h('dl', { class: 'si-facts', cover: ['ha.discovery.enable', 'ha.discovery.prefix', 'ha.discovery.url'] },
        h('dt', null, 'Discovery'), h('dd', null, haOn() ? 'on' : 'off'),
        h('dt', null, 'Prefix'), h('dd', null, code(prefix)),
        h('dt', null, 'App address'), h('dd', null, 'set by an admin'));
    }
    const haValues = new Set(ents.map((e) => e.state_topic.split('/').pop()));
    const published = VALUES.filter((v) => v.status === 'published').flatMap((v) => v.keys);
    const missing = published.filter((k) => !haValues.has(k) && !['latitude', 'longitude'].includes(k) && !(k === 'location' && !locationOn()));
    const node = (dev && dev.identifiers && dev.identifiers[0]) || `${ns().replace(/[^A-Za-z0-9_-]/g, '_')}_${carId || 'c1'}`;
    replace(haSlot, h('div', { class: 'si-two' },
      h('div', null, controls,
        h('div', { class: 'si-rules si-rules-one' },
          rule('A car deleted or renamed', 'Deleting a car, or taking it off your list, removes its device and every entity from Home Assistant at once. A rename sends the device again with the new name at once; entity ids stay as they were.', { cover: 'ha.discovery.removed-car-cleanup' }),
          rule('Discovery turned off', 'Every car’s device and entities are removed from Home Assistant the moment you save, not at the next restart. The plain topics stay.', { cover: 'ha.discovery.disabled-cleanup' }),
          rule('How clean-up works', 'Nothing has to be remembered: on each such change, and again at every start, removals are sent for whatever should not be there. A clean-up cut short is simply sent again; a changed prefix sends removals under the old prefix first.', { cover: ['ha.discovery.cleanup-policy', 'ha.discovery.limits-note'] }))),
      h('div', null,
        h('div', { class: 'si-device', cover: ['ha.discovery.device-payload', 'ha.device.identity'] },
          h('h3', { class: 'si-sub' }, 'The device Home Assistant gets'),
          h('dl', { class: 'si-facts' },
            h('dt', null, 'Name'), h('dd', null, dev ? dev.name : '—'),
            h('dt', null, 'Model'), h('dd', null, dev ? dev.model : '—'),
            h('dt', null, 'Made by'), h('dd', null, 'Tesla, recorded by Sentry Mate'),
            h('dt', null, 'Identifier'), h('dd', null, code(node)),
            h('dt', null, 'Software'), h('dd', null, 'the version you typed on the Software page, when there is one')),
          h('p', { class: 'si-quiet' }, 'One device per car. The identifier is made from the topic prefix and the car id, never the VIN, so a second Sentry Mate on the same Home Assistant never mixes its cars with these.')),
        rule('Entity ids', ['Each entity is ', code(`${node}_<value>`), ', for example ', code(`sensor.${node}_soc`), '. Home Assistant shows the names; you can rename an entity there and the link stays.'], { cover: 'ha.entity.id-scheme' }),
        rule('An entity for every value', haOn()
          ? (missing.length ? ['Every published value has its entity, except ', missing.map((k, i) => [i ? ', ' : '', code(k)]), '.'] : 'Every published value has its entity. Events have one event entity per car.')
          : 'Discovery is off, so Home Assistant gets no entities. Turn it on to see them below.', { cover: 'ha.discovery.coverage-gap', cls: missing.length ? 'is-warn' : null }))));
  }
  const haSec = section({ title: 'Home Assistant discovery', note: 'one device per car, found by Home Assistant by itself', id: 'si-ha', cls: 'si-sec' }, haSlot);

  /* ================================================================ Home Assistant entities */
  const entSlot = h('div');
  const domainOf = (e) => e.config_topic.split('/').slice(-4)[0];
  function paintEntities() {
    const ents = (pv && pv.ha_entities) || [];
    const groupTable = dataTable(ctx, {
      rows: ENTITY_GROUPS, itemName: ['entity group', 'entity groups'], label: 'Home Assistant entity groups',
      csv: { filename: csvFilename('sentry-mate', 'ha-entity-groups'), columns: [{ label: 'Group', value: (r) => r.name }, { label: 'Gives', value: (r) => r.gives }, { label: 'Topics', value: (r) => r.topics || '' }, { label: 'Status', value: (r) => STATUS[r.status].word }, { label: 'Note', value: (r) => r.why }] },
      columns: [
        { key: 'name', label: 'Group', th: true, sort: true, cell: (r) => h('span', { class: 'cell-main', cover: r.cid }, r.name) },
        { key: 'gives', label: 'What it gives', cls: 'si-wrap', cell: (r) => h('span', null, h('span', { class: 'cell-main' }, r.gives), r.why ? h('span', { class: 'cell-sub' }, r.why) : null) },
        { key: 'topics', label: 'From topics', cls: 'si-wrap', cell: (r) => (r.topics ? h('code', { class: 'si-code' }, r.topics) : h('span', { class: 'si-quiet' }, '—')) },
        { key: 'status', label: 'Status', sort: true, cell: (r) => statusFlag(r.status), value: (r) => STATUS_ORDER.indexOf(r.status) },
      ],
    });
    groupTable.load();
    let entTable = null;
    if (ents.length) {
      entTable = dataTable(ctx, {
        rows: ents, itemName: ['entity', 'entities'], label: 'Home Assistant entities for this car',
        caption: `for ${(pv && pv.name) || carId}, read-only: Sentry Mate never takes a command`,
        csv: { filename: csvFilename('sentry-mate', 'ha-entities', carId), columns: [{ label: 'Entity id', value: (e) => `${domainOf(e)}.${e.id}` }, { label: 'Name', value: (e) => e.name }, { label: 'Unit', value: (e) => e.unit || '' }, { label: 'Device class', value: (e) => e.device_class || '' }, { label: 'State topic', value: (e) => e.state_topic }, { label: 'Config topic', value: (e) => e.config_topic }] },
        columns: [
          { key: 'name', label: 'Name', th: true, sort: true, cell: (e) => h('span', { class: 'cell-main' }, e.name) },
          { key: 'id', label: 'Entity id', cell: (e) => topic(`${domainOf(e)}.${e.id}`), value: (e) => `${domainOf(e)}.${e.id}` },
          { key: 'unit', label: 'Unit', cell: (e) => e.unit || h('span', { class: 'si-quiet' }, 'none') },
          { key: 'device_class', label: 'Kind', sort: true, cell: (e) => e.device_class || h('span', { class: 'si-quiet' }, '—') },
          { key: 'state_topic', label: 'Reads', cell: (e) => topic(e.state_topic.split('/').slice(-1)[0]), value: (e) => e.state_topic },
        ],
      });
      entTable.load();
    }
    replace(entSlot,
      h('h3', { class: 'si-sub' }, 'What Home Assistant gets, by group'), groupTable.el,
      h('h3', { class: 'si-sub' }, 'Every entity, now'),
      entTable ? entTable.el : empty({ title: 'No entities.', text: haOn() ? 'This car publishes nothing yet.' : 'Discovery is off: turn it on above, and the entities appear here before Home Assistant gets them.' }),
      cardsBlock(ents));
  }
  function cardsBlock(ents) {
    const name = (pv && pv.name) || 'Car';
    const eid = (key) => { const e = ents.find((x) => x.state_topic.endsWith(`/${key}`)); return e ? `${domainOf(e)}.${e.id}` : null; };
    const glanceKeys = ['state', 'soc', 'range', 'place', 'charging', 'plugged_in', 'sentry_mode', 'doors_open', 'inside_temp', 'outside_temp', 'updated'].map(eid).filter(Boolean);
    const yGlance = glanceKeys.length ? ['type: glance', `title: ${name}`, 'entities:', ...glanceKeys.map((x) => `  - entity: ${x}`)].join('\n') : '';
    const tracker = ents.find((e) => domainOf(e) === 'device_tracker');
    const yMap = tracker ? ['type: map', `title: ${name}`, 'hours_to_show: 24', 'entities:', `  - entity: ${domainOf(tracker)}.${tracker.id}`].join('\n') : '';
    const yList = ents.length ? ['type: entities', `title: ${name}`, 'entities:', ...ents.filter((e) => domainOf(e) !== 'event').map((e) => `  - ${domainOf(e)}.${e.id}`), ...(tracker ? ['  - type: attribute', `    entity: zone.home`, '    attribute: radius', '    name: Home zone radius'] : [])].join('\n') : '';
    const card = (t, text, yaml, cid, missingText) => h('div', { class: 'si-card', cover: cid },
      h('div', { class: 'si-card-head' }, h('h4', { class: 'si-card-name' }, t), yaml ? copyButton(ctx, () => yaml, 'Copy') : null),
      h('p', { class: 'si-quiet' }, text),
      yaml ? h('pre', { class: 'si-pre' }, yaml) : h('p', { class: 'si-pre si-pre-empty' }, missingText));
    return h('div', { class: 'si-cards-wrap' },
      h('h3', { class: 'si-sub' }, 'Dashboard cards to paste'),
      h('p', { class: 'si-lead' }, 'Made from the entities above, for a Home Assistant dashboard (Edit dashboard, Add card, Manual). If you renamed an entity in Home Assistant, change it here too.'),
      h('div', { class: 'si-cards' },
        card('At a glance', 'The facts looked at most, including plugged in, Sentry mode and doors; windows and lock are not on CAN.', yGlance, 'ha.lovelace.glance', 'Turn discovery on to get this card.'),
        card('Map', 'Where the car is, on Home Assistant’s map, with the last 24 hours.', yMap, 'ha.lovelace.map', 'Needs discovery on and location publishing on (Topics and rules).'),
        card('Everything', 'One list of every entity.', yList, 'ha.lovelace.entity-list', 'Turn discovery on to get this card.')),
      h('p', { class: 'si-note', cover: 'ha.proximity' }, 'Distance to home and the direction of travel come from Home Assistant’s own Proximity helper (Settings, Devices and services, Helpers), with the car’s device tracker as the tracked device. It needs location publishing on, and learns of an arrival about 75 s late.'));
  }
  const entSec = section({ title: 'Home Assistant entities', note: 'one per published value, read-only', id: 'si-entities', cls: 'si-sec' }, entSlot);

  /* ================================================================ events */
  const eventsSlot = h('div');
  function paintEvents() {
    const n = ns();
    const c = carId || 'c1';
    const tz = carTz();
    const typesTable = dataTable(ctx, {
      rows: EVENTS.map(([t, w, carries]) => ({ t, w, carries })), itemName: ['event type', 'event types'], label: 'Event types',
      csv: { filename: csvFilename('sentry-mate', 'event-types'), columns: [{ label: 'Event', value: (r) => r.t }, { label: 'When', value: (r) => r.w }, { label: 'Carries', value: (r) => r.carries }] },
      columns: [
        { key: 't', label: 'Event', th: true, cell: (r) => code(r.t) },
        { key: 'w', label: 'Sent when', cell: (r) => r.w },
        { key: 'carries', label: 'Carries', cls: 'si-wrap', cell: (r) => r.carries },
      ],
    });
    typesTable.load();
    let recent;
    if (!carId) recent = empty({ title: 'No car yet.', text: 'Events show here once a car has uploaded logs.' });
    else {
      const now = api.now();
      const t = dataTable(ctx, {
        path: api.apiPath`cars/${carId}/events`, params: { from_ms: now - 30 * DAY, to_ms: now + 60_000, types: EVENT_TYPES.join(',') }, limit: 10,
        itemName: ['event', 'events'], label: 'Recent events',
        caption: `last 30 days for ${(pv && pv.name) || carId}, newest first`,
        empty: () => empty({ title: 'No event in the last 30 days.', text: 'This car did not drive, charge or change software in that time.' }),
        columns: [
          { key: 'at', label: 'When', th: true, cell: (r) => h('span', { class: 'si-nowrap' }, formatDateTime(r.at_ms, tz)) },
          { key: 'type', label: 'Event', cell: (r) => code(r.type) },
          { key: 'payload', label: 'Payload', cls: 'si-wrap', cell: (r) => {
            const parts = payloadParts(r.payload);
            if (parts.length <= 4) return h('span', { class: 'si-json' }, parts.join(' · '));
            return h('details', { class: 'si-fold' }, h('summary', { class: 'si-json' }, parts.slice(0, 4).join(' · '), h('span', { class: 'si-quiet' }, ` and ${parts.length - 4} more`)),
              h('span', { class: 'si-json' }, parts.slice(4).join(' · ')));
          } },
        ],
      });
      t.load();
      recent = t.el;
    }
    const recipe = (t, cid, uses, text, s) => h('div', { class: 'si-card', cover: cid },
      h('div', { class: 'si-card-head' }, h('h4', { class: 'si-card-name' }, t), statusFlag(s)),
      h('p', null, text), h('p', { class: 'si-quiet' }, 'Uses ', uses.map((x, i) => [i ? ', ' : '', code(x)]), '.'));
    replace(eventsSlot,
      h('div', { class: 'si-two', cover: 'nr.overview' },
        h('p', { class: 'si-lead' }, 'Automations should react to what happened, not to a value that changed. Sentry Mate sends one message per event on ', topic(`${n}/cars/${c}/event`), '. It is never retained, so a restart of the server, the broker or Home Assistant never fires an automation again. Home Assistant gets them as one event entity per car; Node-RED or a script reads the same topic.'),
        typesTable.el),
      h('h3', { class: 'si-sub' }, 'Recent events'),
      carId ? h('p', { class: 'si-quiet' }, `Exactly as a subscriber got them; times in ${tz}, the car’s zone. Units are named in each payload.`) : null,
      recent,
      h('h3', { class: 'si-sub' }, 'Automations that work with these'),
      h('p', { class: 'si-lead' }, 'Values reach Sentry Mate about 75 s after the car sent them (one-minute logs and their upload): fine for reminders and logging, too late to open a door as the car pulls up.'),
      h('div', { class: 'si-cards' },
        recipe('Garage door', 'ha.automation.garage', ['drive_ended', 'end_place'], 'Close the garage door, or note the arrival, when a drive ends at your home place. Opening it on arrival is better left to the car’s own HomeLink.', 'published'),
        recipe('Left open', 'ha.automation.left-open', ['doors_open', 'trunk_open', 'frunk_open'], 'A reminder after five minutes with a door, the trunk or the frunk open, and every five minutes after. Windows are not on CAN.', 'published'),
        recipe('Stop the reminder', 'ha.automation.left-open-cancel', ['doors_open', 'trunk_open', 'frunk_open'], 'End the repeat as soon as everything reads closed again.', 'published'),
        recipe('Plug in tonight', 'ha.automation.plug-in-reminder', ['plugged_in', 'soc', 'range'], 'An evening reminder when the car is home and not plugged in, with its battery and range.', 'published'),
        recipe('State changed', 'nr.notify.state-change', ['drive_started', 'drive_ended', 'state'], 'A message when a drive starts or ends; drive_ended carries the whole drive, so it can say how far and how efficient.', 'published'),
        recipe('Someone got in', 'nr.notify.driver-present', ['user_present'], 'A message when someone is seen in the car. The event and the user_present topic both say so.', 'published'),
        recipe('Time to full', 'nr.notify.time-to-full', ['time_to_full', 'charge_power', 'soc'], 'A progress message while charging.', 'published'),
        recipe('Arrived, left', 'nr.notify.geofence', ['place', 'start_place', 'end_place'], 'A message when the car leaves one saved place or arrives at another: the place topic changes, and drive events name both ends.', 'published')));
  }
  const eventsSec = section({ title: 'Events for automations', note: 'explicit, never retained, so nothing fires on a restart', id: 'si-events', cls: 'si-sec' }, eventsSlot);

  /* ================================================================ API access */
  const origin = location.origin;
  const apiSec = section({ title: 'API access', note: 'the read API for other apps', id: 'si-api', cls: 'si-sec' },
    h('div', { class: 'si-rules' },
      rule('What you can build on', ['Two things are kept stable: the MQTT topics on this page, and the read API under ', topic(`${origin}/api/v1/`), '. Both looks of this app are built on that same API. The storage behind it is sealed and private: never read it directly.'], { cover: 'faq.public-api' }),
      rule('A JSON API, built in', ['Cars, live values, drives, charges, positions, places and every report, as JSON or CSV. For example ', code('GET /api/v1/cars'), ' lists the cars you may see with their live state, and ', code('GET /api/v1/cars/<id>/drives'), ' their drives, paged by cursor.'], { cover: 'projects.teslamateapi' }),
      rule('Who sees what', ['The API signs in like this app and answers for that account alone: a watch account sees only its own cars, and a hidden car looks the same as no car. Make one account per app or person in ', pageLink('Server and access', 'settings-server', {}, { arrow: false, cls: 'si-link' }), '.']),
      rule('More analysis, already here', ['Costs, battery health, range over time, charging by place and kind, and every gap in the record are pages of this app: ', pageLink('Statistics', 'statistics', {}, { arrow: false, cls: 'si-link' }), ', ', carId ? pageLink('Battery', 'battery', { car: carId }, { arrow: false, cls: 'si-link' }) : 'Battery', ', ', carId ? pageLink('Charges', 'charges', { car: carId }, { arrow: false, cls: 'si-link' }) : 'Charges', '. Every table and chart downloads as CSV for your own work.'], { cover: 'projects.custom-dashboards' }),
      rule('On a phone', 'This app works in a phone’s browser and can be added to the home screen. A phone app of your own can use the same API.', { cover: 'projects.matedroid' })));

  /* ================================================================ network */
  const netSlot = h('div');
  function paintNetwork() {
    if (!net) {
      replace(netSlot, h('p', { class: 'si-lead' }, admin ? 'The list of connections could not be read.' : 'The list of connections is shown to admins. Out of the box Sentry Mate makes no outbound connection at all.'));
      return;
    }
    const rows = [
      ...net.outbound.map((o) => ({ dir: 'out', ...o, port: null, tls: null })),
      ...net.inbound.map((i) => ({ dir: 'in', purpose: i.purpose, host: null, enabled: true, last_used_ms: null, port: i.port, tls: i.tls })),
    ];
    const on = net.outbound.filter((o) => o.enabled).length;
    // map pictures may come from several hosts (the chosen preset, its names and roads, the maps people may switch to)
    const hostsOf = (r) => (Array.isArray(r.hosts) && r.hosts.length ? r.hosts : r.host ? [r.host] : []);
    const t = dataTable(ctx, {
      rows, itemName: ['connection', 'connections'], label: 'Network connections',
      caption: `${on} of ${net.outbound.length} outbound connections on now; one that is off makes no request`,
      csv: { filename: csvFilename('sentry-mate', 'network'), columns: [
        { label: 'Direction', value: (r) => (r.dir === 'in' ? 'inbound' : 'outbound') }, { label: 'For', value: (r) => (NET_PURPOSE[r.purpose] || [r.purpose])[0] },
        { label: 'Host', value: (r) => hostsOf(r).join(' ') }, { label: 'Port', value: (r) => r.port ?? '' }, { label: 'TLS', value: (r) => (r.tls == null ? '' : r.tls ? 'yes' : 'no') },
        { label: 'State', value: (r) => (r.enabled ? 'on' : 'off') }, { label: 'Last used (UTC)', value: (r) => (r.last_used_ms ? new Date(r.last_used_ms).toISOString() : '') }] },
      columns: [
        { key: 'purpose', label: 'For', th: true, sort: true, cls: 'si-wrap', cell: (r) => h('span', null, h('span', { class: 'cell-main' }, (NET_PURPOSE[r.purpose] || [r.purpose])[0]), h('span', { class: 'cell-sub' }, (NET_PURPOSE[r.purpose] || [null, ''])[1])), value: (r) => (NET_PURPOSE[r.purpose] || [r.purpose])[0] },
        { key: 'dir', label: 'Direction', sort: true, cell: (r) => (r.dir === 'in' ? 'Inbound' : 'Outbound') },
        { key: 'host', label: 'Host or port', cell: (r) => (r.port != null ? h('span', null, code(`port ${r.port}`), r.tls ? ', TLS' : ', no TLS') : hostsOf(r).length ? h('span', { class: 'si-hosts' }, hostsOf(r).map((x, i) => [i ? ', ' : null, code(x)])) : h('span', { class: 'si-quiet' }, 'not set')) },
        { key: 'enabled', label: 'State', sort: true, cell: (r) => (r.enabled ? chip(r.dir === 'in' ? 'Open' : 'On', { kind: 'on' }) : chip('Off')), value: (r) => (r.enabled ? 1 : 0) },
        { key: 'last', label: 'Last used', sort: true, cell: (r) => (r.last_used_ms ? ago(r.last_used_ms) : h('span', { class: 'si-quiet' }, r.dir === 'in' ? 'always listening' : 'never')), value: (r) => r.last_used_ms },
        { key: 'change', label: 'Change it', cell: (r) => (r.purpose === 'teslafi' ? h('a', { class: 'link-arrow', href: '#teslafi' }, 'TeslaFi, on this page', icon('arrowRight')) : (NET_PURPOSE[r.purpose] || [])[2] ? pageLink('Settings', NET_PURPOSE[r.purpose][2], {}, { arrow: true }) : h('span', { class: 'si-quiet' }, r.purpose === 'mqtt' || r.purpose === 'tesla' ? 'on this page' : '—')), value: () => '' },
      ],
    });
    t.load();
    replace(netSlot, t.el);
  }
  const netSec = section({ title: 'Network use', note: 'for a firewall: every connection the server makes or takes', id: 'si-network', cls: 'si-sec', cover: 'faq.network-flows' },
    h('p', { class: 'si-lead' }, 'Sentry Mate needs no Tesla account and no cloud: the car’s SMO (its CAN Bridge) uploads to it, and everything else, the Tesla API input included, stays off until an admin turns it on.'), netSlot);

  /* ================================================================ works with */
  const worksSlot = h('div');
  function paintWorks() {
    const n = ns();
    const works = (t, cid, text, uses) => h('div', { class: 'si-card', cover: cid }, h('h4', { class: 'si-card-name' }, t), h('p', null, text), h('p', { class: 'si-quiet' }, 'Uses ', uses));
    replace(worksSlot,
      h('p', { class: 'si-lead', cover: 'projects.disclaimer' }, 'Other people’s tools, named so you know what fits. Sentry Mate does not make, check or vouch for them. Anything built on the MQTT topics or the API keeps working across updates; anything that reads the storage directly will break.'),
      h('div', { class: 'si-cards' },
        works('Route planners', 'projects.abrp', 'A planner that takes the live battery level plans with the real charge. Sentry Mate never sends to a planner by itself: a small MQTT bridge of yours forwards the values, if you want that.', [code('soc'), ', ', code('location'), ', ', code('speed'), ', ', code('power'), '; about 75 s behind the car.']),
        works('Home Assistant Tesla add-ons', 'projects.ha-tesla', 'Integrations that can read a car’s values from MQTT, instead of asking Tesla’s servers, can use these topics directly.', [topic(`${n}/cars/<id>/#`), '.']),
        works('Desk gauges and lights', 'projects.gaussmeter', 'A light or a model on the desk whose colour follows the state and whose level follows the battery.', [code('state'), ' and ', code('soc'), '.']),
        works('Smart mirrors and wall screens', 'projects.magic-mirror', 'A status tile on a mirror or a wall display, from MQTT or the API.', [code('soc'), ', ', code('range'), ', ', code('state'), ', ', code('updated'), '.']),
        works('Garage door openers', 'projects.geogdo', 'Openers that act on arrival and departure, for several cars, by zone or saved place. Values are about 75 s late: good for closing behind you, late for opening.', [code('location'), ', ', code('place'), ' and the drive events.'])));
  }
  const worksSec = section({ title: 'Works with', note: 'tools that use the published values', id: 'si-works', cls: 'si-sec' }, worksSlot);

  /* ---------------- put it together */
  function paintAll() {
    paintHead(); paintBroker(); paintStatus(); paintClient(); paintTopics(); paintCarPick(); paintValues(); paintHa(); paintEntities(); paintEvents(); paintNetwork(); paintWorks();
    return paintLog();
  }
  ctx.main.append(
    pageHead({ eyebrow: 'Settings', title: 'Integrations', deck: `${ctx.info.purpose} Nothing leaves this server until an admin turns it on, and the VIN is never sent.`, aside: headStatus }),
    jumpNav,
    h('p', { class: 'si-saving' }, icon('info'), ' Switches and single fields save at once and a short note confirms it; the broker connection saves with its button.'),
    tesla.el, teslafi.el, brokerSec, topicsSec, valuesSec, haSec, entSec, eventsSec, apiSec, netSec, worksSec);
  // The broker log is in before the page says it is ready, so it does not push the client notes down later.
  const logLoad = paintAll();
  await Promise.all([teslaLoad, logLoad.catch(() => {})]);
  if (!ctx.alive()) return {};
  // after the router has put focus on the page (it does so once mount returns), so the account keeps it
  const hashT = setTimeout(() => { if (ctx.alive()) showTeslaHash(); }, 0);
  ctx.onCleanup(() => clearTimeout(hashT));
  ctx.ready();
  return {};
}
