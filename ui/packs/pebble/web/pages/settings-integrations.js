// Sentry Mate, "Pebble" UI pack: Settings: Integrations (/settings/integrations).
//
// The Tesla account first (a second live input beside SMO, pages/tesla.js, admins only), TeslaFi (a third,
// pages/teslafi.js, #teslafi), then sending the cars'
// live values to other systems, if the owner wants to. One card per integration, each with its
// status: the MQTT broker (publishing switch, connection form, a test, live counts), the topic layout and its rules,
// every value Sentry Mate publishes previewed for a chosen car (unit, status, where it comes from), Home Assistant
// discovery and the entities it gets with dashboard cards to paste, the events automations listen to with ideas
// that use them, the read API, every network connection the server makes, and tools that work with the values.
// Data: GET / PUT settings/mqtt (admin), GET settings/mqtt/status, GET settings/mqtt/preview?car=, POST
// settings/mqtt/test (admin), GET cars/{car}/events, GET network (admin), and the Tesla and TeslaFi routes in pages/tesla.js and pages/teslafi.js.

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { formatDateTime } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { tile, btn, chip, note, icon, ago } from '../core/ui.js';
import { toggle, field, form, setFieldError } from '../core/form.js';
import { confirmSheet } from '../core/sheet.js';
import { sectionHead, pills, nums } from '../core/bits.js';
import { group, infoRow, said, notSaved } from '../core/settings.js';
import { inOrder } from '../core/settings.js';
import { copyButton } from './move-parts.js';
import { teslaTiles } from './tesla.js';
import { teslafiTiles } from './teslafi.js';
import { teslamateTiles } from './teslamate.js';

export const id = 'settings-integrations';
export const shape = [['full', 'h1'], [2, 'h3'], [2, 'h3'], [2, 'h2'], [2, 'h2']];
export const title = () => 'Integrations';

const DAY = 864e5;
const NOT_SENT = 'Not sent by this car yet';
/* ---------------------------------------------------------------------- what is published, and why */

// what a value's status means for whoever reads it: [word, chip tone, explanation]
const STATUS = {
  published: ['Published', 'teal', 'Sent now, retained, in the server units.'],
  logged: ['Logged, not yet sent', 'sky', 'Sentry Mate has the value; it is not its own topic yet.'],
  signal: ['Needs a new CAN signal', 'sun', 'The car sends it; Sentry Mate does not decode it yet. Nothing is guessed meanwhile.'],
  none: ['Not on the CAN bus', 'muted', 'No message on the car’s buses carries it, so it is never published.'],
  never: ['Never published', 'muted', 'Left out on purpose.'],
  device: ['On the device', 'teal', 'Sent once as part of the Home Assistant device, not as a topic.'],
};
const ORDER = ['published', 'device', 'logged', 'signal', 'none', 'never'];
function unitText(kind, u) {
  const mi = u && u.distance === 'mi';
  switch (kind) {
    case 'distance': return mi ? 'mi' : 'km';
    case 'speed': return mi ? 'mph' : 'km/h';
    case 'temperature': return u && u.temperature === 'f' ? '°F' : '°C';
    case 'pressure': return u && u.pressure === 'psi' ? 'psi' : 'bar';
    case 'elevation': return mi ? 'ft' : 'm';
    default: return kind || '';
  }
}

// [item id, topic key(s), what, unit kind, status, source, note]
const VALUES = [
  ['mqtt.display-name', null, 'Car name', null, 'device', 'Typed in Settings: Cars', 'The Home Assistant device name. Topics use the car id, so a rename never moves them.'],
  ['mqtt.state', 'state', 'State', null, 'published', 'The timeline', 'driving, charging, online, asleep or unknown. Unknown (no log, recording paused) is never called asleep.'],
  ['mqtt.since', 'since', 'State since', 'ISO time, UTC', 'logged', 'The start of the timeline span', 'Exact to the CAN frame, not to a poll.'],
  ['mqtt.healthy', 'healthy', 'Logger healthy', 'true / false', 'published', 'The Data health rule', 'False when no log came while the car was awake, the newest log failed its check, or frames were lost in the last hour. See Data health.'],
  ['mqtt.version', 'version', 'Software version', null, 'none', 'No version text on CAN', 'Instead: a software_changed event when module build ids change, with the version you typed (Software page).'],
  ['mqtt.update-available', null, 'Update waiting', null, 'none', 'Only Tesla’s servers know', 'Left out.'],
  ['mqtt.update-version', null, 'Waiting update version', null, 'none', 'Only Tesla’s servers know', 'Left out; the software_changed event tells you afterwards.'],
  ['mqtt.download-perc', null, 'Update download %', '%', 'none', 'No progress on CAN', 'Left out.'],
  ['mqtt.install-perc', 'installing', 'Update install %', 'true / false', 'published', 'Update status (0x343 VCRIGHT_OTAState)', 'No percentage on CAN: an installing true/false flag instead.'],
  ['mqtt.vin', null, 'VIN', null, 'never', 'Only the first 11 characters are ever kept', 'The VIN is never published, so a broker or Home Assistant never holds it.'],
  ['mqtt.model', null, 'Model', null, 'device', 'The VIN prefix', 'Model and year are the Home Assistant device model.'],
  ['mqtt.trim-badging', 'trim', 'Trim', null, 'logged', 'The VIN prefix (drive units, battery)', 'Shown in Settings: Cars; the car configuration message can confirm it.'],
  ['mqtt.exterior-color', 'paint', 'Paint', null, 'published', 'Car configuration (0x7FF GTW_exteriorColor)', 'Newer paints come as a bare code; you can type the name.'],
  ['mqtt.wheel-type', 'wheels', 'Wheels', null, 'published', 'Car configuration (0x7FF GTW_wheelType)', ''],
  ['mqtt.spoiler-type', 'spoiler', 'Spoiler', null, 'published', 'Car configuration (0x7FF GTW_spoilerType)', 'None or passive.'],
  ['mqtt.geofence', 'place', 'Place', null, 'published', 'Your saved places', 'The place the car is in; the no-place text (below) when it is in none.'],
  ['mqtt.latitude-longitude-deprecated', ['latitude', 'longitude'], 'Latitude, longitude', '°', 'published', 'The car’s GPS', 'Only with location publishing on; empty topics while it is off.'],
  ['mqtt.location', 'location', 'Position (JSON)', 'JSON', 'published', 'The car’s GPS, with its accuracy', 'Only with location publishing on. Holds latitude, longitude and gps_accuracy in m.'],
  ['mqtt.shift-state', 'gear', 'Gear', null, 'published', 'Drive inverter (0x118 DI_gear)', 'P, R, N or D. Held while parked (the bus is quiet).'],
  ['mqtt.power', 'power', 'Battery power', 'kW', 'published', 'Pack volts x amps (0x132)', 'Positive out of the pack, negative while charging or braking. Always kW.'],
  ['mqtt.speed', 'speed', 'Speed', 'speed', 'published', 'Wheel speed (0x257)', ''],
  ['mqtt.heading', 'heading', 'Heading', '°', 'published', 'GPS heading (0x3D9 UI_gpsVehicleHeading, chassis bus)', 'Only while driving: the chassis bus is quiet when parked.'],
  ['mqtt.elevation', 'elevation', 'Elevation', 'elevation', 'published', 'The car’s GPS height (0x3D8)', 'Logged with every drive; from the car, no map lookup.'],
  ['mqtt.locked', null, 'Locked', null, 'none', 'No lock state on CAN', 'Only a lock request exists, which is not the lock state. Never guessed.'],
  ['mqtt.sentry-mode', 'sentry_mode', 'Sentry mode', 'true / false', 'published', 'Vehicle modes (0x284 UIsentryMode284)', ''],
  ['mqtt.windows-open', null, 'Any window open', null, 'none', 'Only window switch presses on CAN', 'Left out: switch presses cannot tell open from closed.'],
  ['mqtt.window-each', null, 'Each window', null, 'none', 'Only window switch presses on CAN', 'Left out.'],
  ['mqtt.doors-open', 'doors_open', 'Any door open', 'true / false', 'published', 'Door latches (0x102, 0x103)', ''],
  ['mqtt.door-each', ['door_front_left', 'door_front_right', 'door_rear_left', 'door_rear_right'], 'Each door', 'true / false', 'published', 'Door latches (0x102 left, 0x103 right)', 'Named by side; right-hand drive comes from the car configuration.'],
  ['mqtt.sun-roof-state', null, 'Sunroof position', null, 'none', 'No roof position on CAN', 'The roof type below instead.'],
  ['mqtt.sun-roof-installed', 'roof', 'Roof type', null, 'published', 'Car configuration (0x7FF GTW_roofType)', 'Metal, fixed glass or panoramic.'],
  ['mqtt.sun-roof-percent-open', null, 'Sunroof % open', '%', 'none', 'No roof position on CAN', 'Left out.'],
  ['mqtt.trunk-open', 'trunk_open', 'Trunk open', 'true / false', 'published', 'Trunk latch (0x103), liftgate (0x142)', 'A power liftgate also gives its angle.'],
  ['mqtt.frunk-open', 'frunk_open', 'Frunk open', 'true / false', 'published', 'Frunk latch (0x2E1 VCFRONT_frunkLatchStatus)', ''],
  ['mqtt.is-user-present', 'user_present', 'Someone in the car', 'true / false', 'published', 'Occupancy (0x3B3 UI_userPresent)', 'The user_present event is sent too, when someone is seen.'],
  ['mqtt.is-climate-on', 'climate_on', 'Climate on', 'true / false', 'published', 'HVAC status (0x243 VCRIGHT_hvacPowerState)', ''],
  ['mqtt.inside-temp', 'inside_temp', 'Inside temperature', 'temperature', 'published', 'Cabin estimate (0x243)', ''],
  ['mqtt.outside-temp', 'outside_temp', 'Outside temperature', 'temperature', 'published', 'Ambient sensor (0x2E1, 0x343)', ''],
  ['mqtt.is-preconditioning', ['preconditioning_cabin', 'preconditioning_battery'], 'Preconditioning', 'true / false', 'published', 'HVAC status (0x243), battery heating request (0x082, 0x3B3)', 'Cabin and battery apart, not one flag.'],
  ['mqtt.odometer', 'odometer', 'Odometer', 'distance', 'published', 'The car’s odometer (0x3F3, 0x3B6)', 'Only ever grows.'],
  ['mqtt.est-battery-range-km', 'range_estimated', 'Estimated range', 'distance', 'published', 'Energy left / your recent consumption', 'Sentry Mate’s own estimate, labelled as such.'],
  ['mqtt.rated-battery-range-km', 'range', 'Rated range', 'distance', 'published', 'The car’s range (0x33A UI_Range)', '1 mile steps on the wire.'],
  ['mqtt.ideal-battery-range-km', 'range_ideal', 'Ideal range', 'distance', 'published', 'The car’s ideal range (0x33A UI_idealRange)', ''],
  ['mqtt.battery-level', 'soc', 'Battery level', '%', 'published', 'State of charge as the screen shows it (0x33A UI_SOC)', ''],
  ['mqtt.usable-battery-level', 'soc_usable', 'Usable battery level', '%', 'published', 'Usable state of energy (0x33A UI_uSOE)', 'Lower than the plain level when the pack is cold.'],
  ['mqtt.plugged-in', 'plugged_in', 'Plugged in', 'true / false', 'published', 'Charge cable state (0x25D CP_chargeCableState)', 'A cable is in, charging or not. Charger still says ac or dc.'],
  ['mqtt.charging-state', 'charge_status', 'Charging status', null, 'published', 'Charge status word (0x212 BMS_uiChargeStatus)', 'One of disconnected, no_power, about_to_charge, charging, complete or stopped.'],
  ['mqtt.charge-energy-added', 'charge_energy_added', 'Energy added', 'kWh', 'published', 'The pack’s lifetime charge counter (0x3D2)', 'The lifetime counter: it only grows. charge_ended carries what each charge added.'],
  ['mqtt.charge-limit-soc', 'charge_limit', 'Charge limit', '%', 'published', 'Charge request (0x333 UI_chargeTerminationPct)', ''],
  ['mqtt.charge-port-door-open', 'charge_port_open', 'Charge port open', 'true / false', 'published', 'Charge port (0x25D CP_chargeDoorOpen)', ''],
  ['mqtt.charger-actual-current', 'charger_current', 'Charger current', 'A', 'published', 'AC line current (0x264), DC charger current (0x29D)', 'Sent while charging; also logged on every charge curve.'],
  ['mqtt.charger-phases', 'charger_phases', 'Phases in use', '1 to 3', 'published', 'Charger phase enables (0x204)', ''],
  ['mqtt.charger-power', 'charge_power', 'Charge power', 'kW', 'published', 'Pack side, volts x amps', 'Always kW.'],
  ['mqtt.charger-voltage', 'charger_voltage', 'Charger voltage', 'V', 'published', 'AC line voltage (0x264), DC charger voltage', 'Sent while charging; also logged on every charge curve.'],
  ['mqtt.charge-current-request', 'charge_current_request', 'Current asked for', 'A', 'published', 'The current set on the screen (0x333)', ''],
  ['mqtt.charge-current-request-max', 'charge_current_limit', 'Current limit', 'A', 'published', 'Charger pilot, cable and car limits (0x21D, 0x13D)', 'Says which of the three limits applies.'],
  ['mqtt.scheduled-charging-start-time', null, 'Scheduled charge start', null, 'none', 'Only schedule on/off flags on CAN', 'Instead: “plugged in, waiting” from the charge status, and the usual start time at that place from past charges.'],
  ['mqtt.time-to-full-charge', 'time_to_full', 'Time to full', 'min', 'published', 'Time remaining (0x132)', ''],
  ['mqtt.tpms-pressure', ['tire_pressure_fl', 'tire_pressure_fr', 'tire_pressure_rl', 'tire_pressure_rr'], 'Tyre pressures', 'pressure', 'published', 'Tyre sensors (0x219)', 'Front left, front right, rear left, rear right.'],
  ['mqtt.tpms-soft-warning', 'tire_low', 'Tyre pressure low', 'true / false', 'published', 'Pressure against the recommended pressure (0x219 page 4)', ''],
  ['mqtt.active-route', null, 'Route (destination, distance, minutes, traffic)', null, 'none', 'Not on CAN', 'Only navigating and the energy at the destination are on CAN.'],
  ['mqtt.active-route-payload-routing', 'energy_at_destination', 'Energy at destination', 'kWh', 'published', 'Trip planning (0x082 UI_energyAtDestination)', 'While navigating; destination and minutes are not on CAN.'],
  ['mqtt.active-route-payload-idle', 'navigating', 'Navigating', 'true / false', 'published', 'Trip planning (0x082), map data (0x238)', 'A plain false with no destination, not an error text.'],
  ['mqtt.center-display-state', 'display_mode', 'Screen mode', null, 'published', 'Display on (0x00C), modes (0x284), keep climate (0x2F3)', 'Named modes: off, on, sentry, dog, camp. Not bare numbers.'],
  ['mqtt.service-mode', 'service_mode', 'Service mode', 'true / false', 'published', 'Vehicle modes (0x284 UIserviceMode284)', ''],
  // what Sentry Mate publishes that the list above has no row for
  [null, 'driving', 'Driving', 'true / false', 'published', 'The timeline', 'Sentry Mate extra.'],
  [null, 'charging', 'Charging', 'true / false', 'published', 'Charge status (0x212)', 'Sentry Mate extra.'],
  [null, 'charge_kind', 'Charger', 'ac / dc', 'published', 'Charger type (0x13D)', 'Sentry Mate extra.'],
  [null, 'energy_remaining', 'Energy remaining', 'kWh', 'published', 'The pack (0x352)', 'Sentry Mate extra.'],
  [null, 'battery_temp', 'Battery temperature', 'temperature', 'published', 'Cell temperatures', 'Sentry Mate extra.'],
  [null, 'lv_voltage', '12 V battery', 'V', 'published', 'Low-voltage battery', 'Sentry Mate extra.'],
  [null, 'updated', 'Last data', 'ISO time, UTC', 'published', 'The newest decoded frame', 'Every value set carries its time: read it before trusting a value.'],
].map(([cid, keys, what, unitKind, status, source, why]) => ({ cid, keys: keys == null ? [] : Array.isArray(keys) ? keys : [keys], what, unitKind, status, source, why }));

// Home Assistant: what it gets, per group, with the same statuses
const ENTITY_GROUPS = [
  ['ha.entity.location-tracker', 'Device tracker', 'The car on the Home Assistant map, home or away by zone', 'location', 'published', 'Only with location publishing on. About 75 s behind live (one-minute logs and the upload).'],
  ['ha.entity.active-route-tracker', 'Destination tracker', 'Where the car is navigating to', null, 'none', 'The destination is not on CAN.'],
  ['ha.entity.status-sensors', 'Status sensors', 'State, Charging, Screen mode, Last data', 'state, charging, display_mode, updated', 'published', 'State since comes next; software version and waiting updates are not on CAN.'],
  ['ha.entity.config-sensors', 'Build sensors', 'Paint, wheels, spoiler, roof', 'paint, wheels, spoiler, roof', 'published', 'Model and year are on the device already. Trim is not sent yet.'],
  ['ha.entity.motion-sensors', 'Motion sensors', 'Place, Gear, Power, Speed, Heading, Elevation', 'place, gear, power, speed, heading, elevation', 'published', 'Heading only while driving: the chassis bus is quiet when parked.'],
  ['ha.entity.parking-brake', 'In park', 'On while the gear is P', 'in_park', 'published', 'From the gear. The parking brake itself (0x228, 0x288) is on CAN as well.'],
  ['ha.entity.temperature-sensors', 'Temperature sensors', 'Inside, outside and battery temperature', 'inside_temp, outside_temp, battery_temp', 'published', 'Battery temperature is extra.'],
  ['ha.entity.range-sensors', 'Range sensors', 'Odometer, rated, ideal and estimated range', 'odometer, range, range_ideal, range_estimated', 'published', 'Estimated range is Sentry Mate’s own, labelled as such.'],
  ['ha.entity.charging-sensors', 'Charging sensors', 'Battery, Usable level, Charge status, Limit, Charge power, Charger, Current, Voltage, Phases, Current asked for, Current limit, Time to full, Energy remaining', 'soc, soc_usable, charge_status, charge_limit, charge_power, charge_kind, charger_*, charge_current_*, time_to_full, energy_remaining', 'published', 'Current, voltage, phases and time to full are sent while charging.'],
  ['ha.entity.charge-energy-added.counter', 'Energy added counter', 'A running total for Home Assistant’s Energy view', 'charge_energy_added', 'published', 'Sent as the pack’s lifetime counter (total_increasing), which never resets, rather than a per-charge value that does.'],
  ['ha.entity.tpms-sensors', 'Tyre sensors', 'Four tyre pressures, Pressure low', 'tire_pressure_fl … rr, tire_low', 'published', 'Which sensor is which wheel is still to be confirmed on new firmware.'],
  ['ha.entity.active-route-destination', 'Route destination', 'Destination name', null, 'none', 'Not on CAN.'],
  ['ha.entity.active-route-energy', 'Route energy', 'Navigating, Battery expected on arrival', 'navigating, energy_at_destination', 'published', 'Shown as a real 0; unavailable while not navigating.'],
  ['ha.entity.active-route-distance', 'Route distance', 'Distance left', null, 'none', 'Not on CAN.'],
  ['ha.entity.active-route-minutes', 'Route minutes', 'Minutes left', null, 'none', 'Not on CAN.'],
  ['ha.entity.active-route-traffic', 'Route traffic', 'Minutes lost to traffic', null, 'none', 'Not on CAN.'],
  ['ha.entity.binary-status', 'Health on/off', 'Logger healthy, Update installing, Service mode', 'healthy, installing, service_mode', 'published', 'A waiting update is not on CAN; the software_changed event says when one was installed.'],
  ['ha.entity.binary-security', 'Security on/off', 'Doors (any and each), trunk, frunk, Sentry mode, someone in the car', 'doors_open, door_*, trunk_open, frunk_open, sentry_mode, user_present', 'published', 'Lock and windows are not on CAN and are never guessed.'],
  ['ha.entity.binary-climate-charge', 'Climate and charging on/off', 'Climate, preconditioning, cable, charge port', 'climate_on, preconditioning_*, plugged_in, charge_port_open', 'published', ''],
  ['ha.proximity', 'Distance to home', 'Home Assistant’s own proximity helper, fed by the device tracker', 'location', 'published', 'Needs location publishing on. Arrival is known about 75 s late.'],
].map(([cid, what, gives, topics, status, why]) => ({ cid, what, gives, topics, status, why }));

// the events: one topic, never retained, so a restart never repeats one
const EVENTS = [
  ['drive_started', 'A drive starts', 'drive id, start time and place, battery, range and odometer at the start'],
  ['drive_ended', 'A drive ends', 'the whole drive: distance, duration, energy, efficiency, top speed, power, end place, battery and range at the end, temperatures, climb'],
  ['charge_started', 'A charge starts', 'charge id, kind (ac or dc), place, battery at the start'],
  ['charge_ended', 'A charge ends', 'energy added, battery at the end, battery temperature, cost when the place has a price'],
  ['software_changed', 'Module builds changed', 'the update id and, when known, the version you typed or a matching build'],
  ['user_present', 'Someone is seen in the car', 'the time and place'],
];
const ERRORS = { connection_refused: 'the broker refused the connection', pin_mismatch: 'the certificate did not match the pin', no_host: 'no broker host set', tls_failed: 'TLS failed', auth_failed: 'the user name or password was refused', timeout: 'no answer in time', invalid_settings: 'the settings do not work together' };
const PURPOSE = {
  geocoding: ['Address lookup', 'Only when an admin turns online addresses on; positions are rounded first.', 'settings-display'],
  tiles: ['Map pictures', 'Asked by each viewer’s browser while online tiles are on.', 'settings-display'],
  geonames_download: ['Place-name download', 'Only when an admin asks for new offline place names.', 'settings-display'],
  mqtt: ['MQTT broker', 'Only while publishing is on (this page).', null],
  tesla: ['Tesla API', 'Only while a Tesla account is added and not paused: Tesla’s sign-in, its Owner API and its live stream (Tesla account, above).', null],
  teslafi: ['TeslaFi API', 'Only while a TeslaFi token is added and not paused: reads your cars’ data from TeslaFi', '#teslafi'],
  teslamate_mqtt: ['Local TeslaMate (MQTT)', 'Only while a local TeslaMate is added and not paused: subscribes to its topics, never publishes', '#teslamate'],
  teslamate_db: ['Local TeslaMate (database)', 'Only while its database catch-up is on: reads what MQTT missed', '#teslamate'],
  release_check: ['Release check', 'Asks whether a newer Sentry Mate exists, when turned on.', 'settings-display'],
  upload: ['CAN Bridge uploads', 'The car’s CAN Bridge sends its logs here.', null],
  web: ['This app and the API', 'Browsers, apps and the API.', null],
};
const code = (t) => h('code', { class: 'set-mono' }, t);
// the Change it link of a connection: an anchor on this page (#teslafi) or another settings page
const changeLink = (p) => (p && p[2] ? [' ', h('a', { class: 'place-link', href: p[2].startsWith('#') ? `${state.buildUrl('settings-integrations')}${p[2]}` : state.buildUrl(p[2]) }, 'Change it')] : null);
const slug = (s) => String(s || '').toLowerCase().normalize('NFKD').replace(/[̀-ͯ]/g, '').replace(/[^a-z0-9]+/g, '_').replace(/^_+|_+$/g, '') || 'car';
const domainOf = (t) => { const p = String(t || '').split('/'); return p.length >= 5 ? p[p.length - 4] : null; };
const prefixOf = (t) => { const p = String(t || '').split('/'); return p.length >= 5 ? p.slice(0, -4).join('/') : null; };
const statusChip = (s) => chip(STATUS[s][0], { tone: STATUS[s][1] });
const nsBad = (v) => !v || /[+#]/.test(v) || v.startsWith('$') || v.split('/').some((x) => !x);

export async function mount(ctx) {
  inOrder(ctx);
  const admin = ctx.admin;
  let [cfg, status, net, cars] = await Promise.all([
    admin ? ctx.api.get('settings/mqtt') : Promise.resolve(null),
    ctx.api.get('settings/mqtt/status').catch(() => null),
    admin ? ctx.api.get('network').catch(() => null) : Promise.resolve(null),
    ctx.api.page('cars', { sort: 'custom' }, { limit: 12 }).then((r) => r.items || []).catch(() => []),
  ]);
  const allCars = admin ? await ctx.api.page('cars', { sort: 'name' }, { limit: 200 }).then((r) => r.items || []).catch(() => cars) : cars;
  const tesla = teslaTiles(ctx, { cars: allCars });
  const teslafi = teslafiTiles(ctx, { cars: allCars });
  const teslamate = teslamateTiles(ctx, { cars: allCars });
  await Promise.all([tesla.load(), teslafi.load(), teslamate.load()]);
  if (!ctx.alive()) return {};
  let carId = (ctx.query.car && cars.some((c) => c.id === ctx.query.car) ? ctx.query.car : null) || (cars.find((c) => c.id === state.lastCar()) || cars.find((c) => !state.isSold(c)) || cars[0] || {}).id || null;
  let pv = null;
  let events = [];
  async function loadCar() {
    if (!carId) { pv = null; events = []; return; }
    const now = ctx.api.now();
    [pv, events] = await Promise.all([
      ctx.api.get('settings/mqtt/preview', { car: carId }).catch((e) => ({ error: e, messages: [], ha_entities: [] })),
      ctx.api.page(api.apiPath`cars/${carId}/events`, { from_ms: now - 30 * DAY, to_ms: now + 60000, types: EVENTS.map((e) => e[0]).join(',') }, { limit: 8 }).then((r) => r.items || []).catch(() => []),
    ]);
  }
  await loadCar();
  if (!ctx.alive()) return {};
  const ns = () => (cfg && cfg.namespace) || ((pv && pv.messages && pv.messages[0] && pv.messages[0].topic) || 'sentrymate/status').replace(/\/status$/, '');
  const units = () => (pv && pv.units) || { distance: 'km', temperature: 'c', pressure: 'bar' };

  /* ---------- saving: PUT the whole setting with its version */
  const body = (patch) => {
    const b = { ...cfg, ...patch, ha_discovery: { ...cfg.ha_discovery, ...(patch.ha_discovery || {}) } };
    delete b.password_set; delete b.password;
    if ('password' in patch) b.password = patch.password;
    return b;
  };
  async function save(patch, what) {
    try {
      cfg = await ctx.api.put('settings/mqtt', body(patch));
      ctx.toast(`${what} saved.`);
    } catch (e) {
      if (e && e.status === 409) { cfg = await ctx.api.get('settings/mqtt'); ctx.toast('Someone else changed these settings; this page shows theirs now.', { kind: 'info' }); } else { ctx.toast(notSaved(e), { kind: 'error' }); await refresh(); return e; }
    }
    await refresh();
    return null;
  }
  async function refresh() {
    [status, net] = await Promise.all([ctx.api.get('settings/mqtt/status').catch(() => status), admin ? ctx.api.get('network').catch(() => net) : net]);
    await loadCar();
    if (ctx.alive()) draw();
  }

  /* ---------- the cards */
  function headChip() {
    if (!status) return chip('Status not known', { tone: 'muted' });
    return !status.enabled ? chip('Publishing off', { tone: 'muted', icon: 'minus' }) : status.connected ? chip(`Connected to ${status.broker}`, { tone: 'teal', icon: 'check' }) : chip('Not connected', { tone: 'sun', icon: 'warning' });
  }

  function brokerTile() {
    const covers = ['docs.env.mqtt.disable-mqtt', 'docs.env.mqtt.mqtt-host', 'docs.env.mqtt.mqtt-port', 'docs.env.mqtt.mqtt-username', 'docs.env.mqtt.mqtt-password', 'docs.env.mqtt.mqtt-tls', 'docs.env.mqtt.mqtt-tls-accept-invalid-certs', 'docs.env.mqtt.mqtt-ipv6', 'docs.env.mqtt.mqtt-namespace'];
    if (!cfg) {
      return tile({ w: 2, title: 'MQTT broker', icon: 'plug', side: headChip(), cover: covers },
        said('info', 'Only an admin sees and changes the broker settings. The status, the preview and the events are yours to read.'),
        group(null, [infoRow({ name: 'Publishing', side: h('span', { class: 'set-value' }, status && status.enabled ? 'On' : 'Off') }),
          infoRow({ name: 'Broker', side: h('span', { class: 'set-value' }, status && status.broker ? `${status.broker}${status.tls ? ', TLS' : ''}` : 'Not set') }),
          infoRow({ name: 'Topics start with', side: code(ns()) })]));
    }
    const onSw = toggle({ label: 'Publish to the broker', sub: 'Off until you turn it on: nothing connects anywhere before that.', checked: cfg.enabled, cover: 'docs.env.mqtt.disable-mqtt',
      onchange: async (on) => {
        if (on && !cfg.host) { onSw.set(false); ctx.toast('Set the broker host first, and save the connection.', { kind: 'info' }); hostF.control.focus(); return; }
        if (await save({ enabled: on }, on ? 'Publishing on' : 'Publishing off')) onSw.set(cfg.enabled);
      } });
    const hostF = field({ label: 'Host', name: 'host', value: cfg.host, cover: ['docs.env.mqtt.mqtt-host', 'docs.env.mqtt.mqtt-ipv6'], hint: 'A name such as broker.local. IPv4 or IPv6: the connection follows the address.', attrs: { autocomplete: 'off', spellcheck: 'false', placeholder: 'broker.local' } });
    const portF = field({ label: 'Port', name: 'port', type: 'number', value: cfg.port, cover: 'docs.env.mqtt.mqtt-port', hint: '8883 is usual with TLS, 1883 without.', attrs: { min: '1', max: '65535', step: '1' } });
    const userF = field({ label: 'User name', name: 'username', value: cfg.username || '', cover: 'docs.env.mqtt.mqtt-username', hint: 'Empty for a broker without accounts.', attrs: { autocomplete: 'off' } });
    const passF = field({ label: 'Password', name: 'password', type: 'password', cover: 'docs.env.mqtt.mqtt-password', hint: cfg.password_set ? 'A password is saved. Leave this empty to keep it; it is never shown again.' : 'No password saved. It is only ever sent over TLS.', attrs: { autocomplete: 'new-password', placeholder: cfg.password_set ? '•••••••• (kept)' : '' } });
    const tlsT = toggle({ label: 'Encrypt the connection (TLS)', sub: 'Without TLS no password is sent, and the server refuses to save one.', checked: cfg.tls, cover: 'docs.env.mqtt.mqtt-tls' });
    tlsT.input.name = 'tls';
    const pinF = field({ label: 'Pinned certificate (SHA-256)', name: 'pin', value: cfg.pin_sha256 || '', cover: 'docs.env.mqtt.mqtt-tls-accept-invalid-certs', hint: 'For a broker with its own certificate: 64 hex digits, colons allowed. Only that one is accepted; there is no “accept any” switch.', attrs: { autocomplete: 'off', spellcheck: 'false', class: 'input-mono' } });
    const nsF = field({ label: 'Topic prefix', name: 'namespace', value: cfg.namespace, cover: 'docs.env.mqtt.mqtt-namespace', hint: 'Every topic starts with this. Two servers on one broker use different prefixes.', attrs: { autocomplete: 'off', spellcheck: 'false' } });
    const testOut = h('div', { 'aria-live': 'polite' });
    const testBtn = btn('Test the saved settings', { kind: 'soft', icon: 'pulse', onclick: async () => {
      testBtn.disabled = true;
      testOut.replaceChildren(note('Connecting…'));
      try {
        const r = await ctx.api.post('settings/mqtt/test');
        if (!ctx.alive()) return;
        testOut.replaceChildren(said(r.ok ? 'ok' : 'warn', r.message || (r.ok ? 'It works.' : ERRORS[r.error] || r.error), r.ok && r.latency_ms != null ? ` ${formatNumber(r.latency_ms)} ms.` : ''));
        status = await ctx.api.get('settings/mqtt/status').catch(() => status);
        if (ctx.alive()) statusEl.replaceWith(statusEl = statusTile());
      } catch (e) { testOut.replaceChildren(said('bad', notSaved(e).replace(/^Not saved: /, ''))); } finally { testBtn.disabled = false; }
    } });
    const f = form({
      fields: [hostF, portF, userF, passF, tlsT, pinF, nsF], submitLabel: 'Save connection', secondary: testBtn,
      onsubmit: async (v) => {
        const patch = { host: (v.host || '').trim(), port: v.port == null ? null : Math.round(v.port), username: (v.username || '').trim() || null, tls: !!v.tls, pin_sha256: (v.pin || '').trim() || null, namespace: (v.namespace || '').trim() };
        if (v.password) patch.password = v.password;
        const bad = {};
        if (cfg.enabled && !patch.host) bad.host = 'Publishing is on, so the broker needs a host.';
        if (patch.pin_sha256 && !patch.tls) bad.pin = 'A certificate pin needs TLS.';
        if ((v.password || cfg.password_set) && !patch.tls) bad.password = 'A password is only ever sent over TLS. Keep TLS on, or remove the password.';
        if (nsBad(patch.namespace)) bad.namespace = 'No + or #, not starting with $, and no empty level.';
        if (Object.keys(bad).length) throw Object.assign(new Error('Not saved: check the fields marked.'), { fields: bad });
        try { cfg = await ctx.api.put('settings/mqtt', body(patch)); } catch (e) {
          if (e && e.fields) e.fields = Object.fromEntries(Object.entries(e.fields).map(([k, m]) => [k === 'pin_sha256' ? 'pin' : k, m]));
          throw e;
        }
        ctx.toast('Connection saved. Test it to see the broker answer.');
        await refresh();
      },
    });
    const removePw = cfg.password_set ? btn('Remove the saved password', { kind: 'ghost', icon: 'trash', cls: 'btn-sm', onclick: async () => {
      if (!await confirmSheet({ title: 'Remove the password?', text: 'The broker password is deleted from Sentry Mate. Publishing then connects without one.', yes: 'Remove', danger: true })) return;
      await save({ password: null }, 'Password removal');
    } }) : null;
    return tile({ w: 2, title: 'MQTT broker', icon: 'plug', side: headChip() },
      group(null, [onSw]), f, removePw, testOut);
  }

  let statusEl = null;
  function statusTile() {
    const s = status;
    return tile({ w: 2, title: 'Broker status', icon: 'pulse', side: btn('Refresh', { kind: 'ghost', icon: 'refresh', iconOnly: true, cls: 'btn-sm', onclick: async () => { status = await ctx.api.get('settings/mqtt/status').catch(() => status); if (ctx.alive()) statusEl.replaceWith(statusEl = statusTile()); } }) },
      !s ? said('info', 'The broker status could not be read.') : [
        h('ul', { class: 'checks' },
          h('li', { class: ['check', !s.enabled ? 'is-off' : s.connected ? 'is-ok' : 'is-warn'] }, h('span', { class: 'check-dot', 'aria-hidden': 'true' }),
            h('span', { class: 'check-text' }, h('span', { class: 'check-name' }, s.broker || 'No broker set'), h('span', { class: 'check-sub' }, s.tls ? 'Over TLS' : s.broker ? 'Without TLS' : 'Set one under MQTT broker')),
            h('span', { class: 'check-word' }, !s.enabled ? 'Off' : s.connected ? 'Connected' : 'Not connected')),
          h('li', { class: ['check', s.last_error ? 'is-warn' : 'is-ok'] }, h('span', { class: 'check-dot', 'aria-hidden': 'true' }),
            h('span', { class: 'check-text' }, h('span', { class: 'check-name' }, 'Last error'), h('span', { class: 'check-sub' }, s.last_error ? [ERRORS[s.last_error] || s.last_error, s.last_error_ms ? `, ${formatDateTime(s.last_error_ms, ctx.tz, { now: api.now() })}` : ''] : 'None since the server started.')),
            h('span', { class: 'check-word' }, s.last_error ? 'Yes' : 'None'))),
        nums([['Delivered', h('span', { class: 'num num-m' }, h('span', { class: 'num-n' }, formatNumber(s.delivered || 0)))], ['Waiting', h('span', { class: 'num num-m' }, h('span', { class: 'num-n' }, formatNumber(s.waiting || 0)))], ['Dropped', h('span', { class: 'num num-m' }, h('span', { class: 'num-n' }, formatNumber(s.dropped || 0)))]]),
        note('Delivered since the server started. Waiting values are kept until the broker is back; the newest of each topic wins, and older ones count as dropped.'),
      ],
      group('Connect a client', [infoRow({ icon: 'plug', name: 'Node-RED, Home Assistant or any MQTT client', sub: [`Use this same broker. Subscribe to `, code(`${ns()}/cars/+/#`), ' for every car, or ', code(`${ns()}/cars/<car id>/#`), ' for one.'] })], { cover: 'nr.setup.mqtt' }));
  }

  function topicsTile() {
    const n = ns();
    const cid = carId || 'c1';
    const noPlace = field({ label: 'Text when the car is at no saved place', name: 'no_place', value: (cfg && cfg.no_place_text) ?? '', hint: 'Published as the place, up to 40 characters. Empty publishes an empty place.', attrs: { maxlength: '40', disabled: !cfg } });
    if (cfg) noPlace.control.addEventListener('change', async () => { const e = await save({ no_place_text: noPlace.control.value.trim() }, 'No-place text'); if (e && e.fields) setFieldError(noPlace, e.fields.no_place_text || null); });
    const loc = cfg ? toggle({ label: 'Publish the position', sub: 'Off by default. On: latitude, longitude and a JSON position with its accuracy, for maps and arrival automations.', checked: cfg.publish_location,
      onchange: async (on) => { if (await save({ publish_location: on }, on ? 'Location publishing on' : 'Location publishing off')) loc.set(cfg.publish_location); } })
      : infoRow({ name: 'Publish the position', side: h('span', { class: 'set-value' }, pv && pv.messages && pv.messages.some((m) => m.topic.endsWith('/location') && m.payload) ? 'On' : 'Off') });
    return tile({ w: 2, title: 'Topics and rules', icon: 'route' },
      group('Layout', [
        infoRow({ name: code(`${n}/status`), sub: 'online or offline. Retained; the broker sets offline by itself if the server goes away.' }),
        infoRow({ name: code(`${n}/cars/${cid}/<value>`), sub: 'One topic per value (below). Retained, QoS 1, text in the server units.' }),
        infoRow({ name: code(`${n}/cars/${cid}/updated`), sub: 'The time of the newest data behind the values (ISO, UTC).' }),
        infoRow({ name: code(`${n}/cars/${cid}/event`), sub: 'One JSON message per event. Not retained, so nothing old fires again.' }),
      ], { cover: 'mqtt.topic-scheme' }),
      group('Rules', [
        infoRow({ icon: 'car', cover: 'mqtt.note-car-id', name: 'Car ids', sub: [`Each car has Sentry Mate’s own id (${cid} here), never reused. A new CAN Bridge key for the same car joins that car, so topics never move. Ids are in `, h('a', { href: state.buildUrl('settings-cars'), class: 'place-link' }, 'Settings: Cars'), '.'] }),
        infoRow({ icon: 'info', cover: 'mqtt.note-empty-values', name: 'Missing values', sub: 'A value the car did not send is not published: the broker keeps the last one, and updated says how old it is. Position topics are emptied when location publishing is turned off.' }),
      ]),
      h('div', { class: 'form', cover: 'docs.env.core.default-geofence' }, noPlace),
      group('Location', [loc]));
  }

  function valuesTile() {
    const byTopic = new Map(((pv && pv.messages) || []).map((m) => [m.topic.split('/').pop(), m.payload]));
    const u = units();
    const counts = Object.fromEntries(ORDER.map((s) => [s, VALUES.filter((r) => r.status === s).length]));
    const payload = (r) => {
      if (r.status === 'device') { const d = pv && pv.device; return r.cid === 'mqtt.model' ? (d && d.model) || '—' : (d && d.name) || (pv && pv.name) || '—'; }
      if (r.status !== 'published' || !r.keys.length) return null;
      const got = r.keys.map((k) => byTopic.get(k)).filter((x) => x !== undefined);
      if (!got.length) return NOT_SENT;
      return got.map((p) => (p === '' ? '(empty)' : p.length > 40 ? `${p.slice(0, 37)}…` : p)).join(' · ');
    };
    const sorted = [...VALUES].sort((a, b) => ORDER.indexOf(a.status) - ORDER.indexOf(b.status));
    return tile({ w: 'full', title: `Every value${pv && pv.name ? ` for ${pv.name}` : ''}`, icon: 'bars', side: h('span', { class: 'note' }, `${counts.published} published · ${counts.logged} logged · ${counts.signal} need a signal · ${counts.none + counts.never} left out`) },
      note(`Payloads are what a broker gets now, as text in the server units (${unitText('distance', u)}, ${unitText('temperature', u)}, ${unitText('pressure', u)}): MQTT has one set of units for everyone. The VIN is never published; the position only when you opt in.`),
      h('div', { class: 'set-chips' }, ORDER.map((k) => h('span', { class: 'set-line' }, statusChip(k), h('span', { class: 'note' }, STATUS[k][2])))),
      pv && pv.error ? said('warn', 'The preview could not be made: ', pv.error.message || 'no answer') : null,
      h('div', { class: 'iv-grid' }, sorted.map((r) => {
        const p = payload(r);
        return h('div', { class: 'iv', cover: r.cid || null },
          h('div', { class: 'iv-top' }, h('span', { class: 'row-name' }, r.what), statusChip(r.status)),
          r.keys.length ? h('span', { class: 'iv-topics' }, r.keys.map((k) => code(k))) : h('span', { class: 'row-sub' }, r.cid ? 'No topic' : ''),
          p != null ? h('span', { class: 'iv-pay' }, h('b', null, p), r.unitKind && p !== NOT_SENT && !/[/ ]/.test(unitText(r.unitKind, u)) ? ` ${unitText(r.unitKind, u)}` : '') : null,
          h('span', { class: 'row-sub' }, r.source, r.why ? `. ${r.why}` : ''));
      })));
  }

  function haTile() {
    const ha = (cfg && cfg.ha_discovery) || { enabled: ((pv && pv.ha_entities) || []).length > 0, prefix: prefixOf(pv && pv.ha_entities && pv.ha_entities[0] && pv.ha_entities[0].config_topic) || 'homeassistant', url: null };
    const ro = !cfg;
    const sw = toggle({ label: 'Home Assistant discovery', sub: 'Each car appears in Home Assistant by itself, with every sensor and no YAML. Needs publishing on.', checked: ha.enabled, disabled: ro, cover: 'ha.discovery.enable',
      onchange: async (on) => { if (await save({ ha_discovery: { enabled: on } }, on ? 'Discovery on' : 'Discovery off')) sw.set(cfg.ha_discovery.enabled); } });
    const prefixF = field({ label: 'Discovery prefix', name: 'ha_prefix', value: ha.prefix, cover: 'ha.discovery.prefix', hint: 'Must match Home Assistant’s own MQTT discovery prefix: homeassistant unless you changed it there.', attrs: { disabled: ro, spellcheck: 'false', autocomplete: 'off' } });
    const urlF = field({ label: 'Link on the device page (https)', name: 'ha_url', value: ha.url || '', cover: 'ha.discovery.url', hint: 'This server’s address as Home Assistant users reach it. Each car’s device then links to its Car page. Empty: no link.', attrs: { disabled: ro, placeholder: `https://${location.host}`, spellcheck: 'false', autocomplete: 'off' } });
    if (!ro) {
      prefixF.control.addEventListener('change', async () => {
        const v = prefixF.control.value.trim();
        setFieldError(prefixF, null);
        if (nsBad(v)) { setFieldError(prefixF, 'No + or #, not starting with $, and no empty level.'); return; }
        const e = await save({ ha_discovery: { prefix: v } }, 'Discovery prefix');
        if (e && e.fields) setFieldError(prefixF, e.fields['ha_discovery.prefix'] || null);
      });
      urlF.control.addEventListener('change', async () => {
        const v = urlF.control.value.trim();
        setFieldError(urlF, null);
        if (v && !/^https:\/\/[^\s/]+(\/\S*)?$/.test(v)) { setFieldError(urlF, 'An https:// address, or empty.'); return; }
        const e = await save({ ha_discovery: { url: v || null } }, 'Device link');
        if (e && e.fields) setFieldError(urlF, e.fields['ha_discovery.url'] || null);
      });
    }
    const d = pv && pv.device;
    const ents = (pv && pv.ha_entities) || [];
    const node = (d && d.identifiers && d.identifiers[0]) || `${ns().replace(/[^A-Za-z0-9_-]/g, '_')}_${carId || 'c1'}`;
    const published = new Set(((pv && pv.messages) || []).filter((m) => m.payload !== '').map((m) => m.topic.split('/').pop()).filter((k) => k !== 'status' && k !== 'latitude' && k !== 'longitude'));
    const withEnt = new Set(ents.map((e) => e.state_topic.split('/').pop()));
    const missing = [...published].filter((k) => !withEnt.has(k));
    return tile({ w: 2, title: 'Home Assistant', icon: 'park', side: chip(ha.enabled ? 'Discovery on' : 'Discovery off', { tone: ha.enabled ? 'teal' : 'muted' }) },
      group(null, [sw]), h('div', { class: 'form' }, prefixF, urlF),
      group('The device it gets', [
        infoRow({ name: 'Name', side: h('span', { class: 'set-value' }, (d && d.name) || (pv && pv.name) || '—') }),
        infoRow({ name: 'Maker and model', side: h('span', { class: 'set-value' }, `Tesla ${(d && d.model) || ''}`.trim()) }),
        infoRow({ name: 'Identifier', side: code(node) }),
        infoRow({ name: 'Entities', side: h('span', { class: 'set-value' }, formatNumber(ents.length)) }),
      ], { cover: ['ha.discovery.device-payload', 'ha.device.identity'], note: 'One device per car, holding its entities. Two servers with different topic prefixes on one broker never share a device.' }),
      group('Rules', [
        infoRow({ icon: 'chip', cover: 'ha.entity.id-scheme', name: 'Entity ids', sub: ['Each hidden unique id is the identifier and the value, such as ', code(`${node}_soc`), `. The visible id follows the device name: sensor.${slug((d && d.name) || (pv && pv.name))}_battery. Ids stay the same across versions.`] }),
        infoRow({ icon: missing.length ? 'warning' : 'check', tone: missing.length ? 'sun' : 'teal', cover: 'ha.discovery.coverage-gap', name: 'An entity for every value',
          sub: ents.length ? (missing.length ? `${withEnt.size} of ${published.size} values have an entity; missing: ${missing.join(', ')}.` : `All ${published.size} values published for this car have an entity.`) : 'Discovery is off, so Home Assistant gets no entities; the topics are still published.' }),
        infoRow({ icon: 'trash', cover: 'ha.discovery.removed-car-cleanup', name: 'A car deleted or renamed', sub: 'Deleting a car removes its device and entities at once. A rename sends the device again with the new name; entity ids stay put.' }),
        infoRow({ icon: 'minus', cover: 'ha.discovery.disabled-cleanup', name: 'Discovery turned off', sub: 'Every device and entity is removed from Home Assistant the moment you save. The plain topics stay.' }),
        infoRow({ icon: 'refresh', cover: ['ha.discovery.cleanup-policy', 'ha.discovery.limits-note'], name: 'How clean-up works', sub: 'Nothing is remembered: on each such change, and at each start, removals are sent for whatever should not be there. A changed prefix clears the old prefix first.' }),
      ]));
  }

  function entityTiles() {
    const ents = (pv && pv.ha_entities) || [];
    const name = slug(pv && pv.name);
    const eid = (key) => { const e = ents.find((x) => x.state_topic.endsWith(`/${key}`)); return e ? `${domainOf(e.config_topic) || 'sensor'}.${name}_${slug(e.name)}` : null; };
    const glance = ['soc', 'state', 'charging', 'plugged_in', 'gear', 'speed', 'range', 'sentry_mode', 'doors_open', 'inside_temp', 'updated'].map(eid).filter(Boolean);
    const all = ents.filter((e) => !['event', 'device_tracker'].includes(domainOf(e.config_topic))).map((e) => `${domainOf(e.config_topic) || 'sensor'}.${name}_${slug(e.name)}`);
    const tracker = ents.find((e) => domainOf(e.config_topic) === 'device_tracker');
    const yGlance = ['type: glance', `title: ${(pv && pv.name) || 'Car'}`, 'entities:', ...glance.map((x) => `  - ${x}`)].join('\n');
    const yMap = tracker ? ['type: map', 'hours_to_show: 24', 'entities:', `  - device_tracker.${name}_location`].join('\n') : '';
    const yList = ['type: entities', `title: ${(pv && pv.name) || 'Car'}`, 'entities:', ...all.map((x) => `  - ${x}`), ...(tracker ? ['  - entity: proximity.home', '    name: Distance to home'] : [])].join('\n');
    const card = (t, text, yaml, cid, missingText) => tile({ w: 1, title: t, icon: 'copy', cover: cid, side: yaml ? copyButton(() => yaml) : null },
      note(text), yaml ? h('pre', { class: 'set-pre' }, yaml) : said('info', missingText));
    return [
      tile({ w: 2, title: 'What Home Assistant gets', icon: 'park' },
        note('Read-only entities, one per published value. A group that needs a new CAN signal shows up as soon as Sentry Mate decodes it.'),
        group(null, ENTITY_GROUPS.map((g) => infoRow({ cover: g.cid, name: g.what, sub: `${g.gives}.${g.why ? ` ${g.why}` : ''}`, side: statusChip(g.status) })))),
      tile({ w: 2, title: `Entities for ${(pv && pv.name) || 'this car'}`, icon: 'bars' },
        ents.length ? group(null, ents.slice(0, 40).map((e) => infoRow({ name: e.name, sub: [code(e.state_topic)], side: h('span', { class: 'set-value' }, [e.unit, e.device_class].filter(Boolean).join(' · ') || '—') })))
          : said('info', cfg && !cfg.ha_discovery.enabled ? 'Discovery is off. Turn it on and every entity goes to Home Assistant.' : 'There is no car to preview yet.'),
        ents.length > 40 ? note(`And ${ents.length - 40} more.`) : null),
      card('At a glance card', 'The facts checked most, for a Home Assistant dashboard (Edit, Add card, Manual).', ents.length ? yGlance : '', 'ha.lovelace.glance', 'Turn discovery on to get this card.'),
      card('Map card', 'Where the car is, on Home Assistant’s map.', yMap, 'ha.lovelace.map', 'Needs location publishing on.'),
      card('Everything card', 'One list of every entity, and the distance to home when the position is published.', ents.length ? yList : '', 'ha.lovelace.entity-list', 'Turn discovery on to get this card.'),
      tile({ w: 1, title: 'Distance to home', icon: 'pin', cover: 'ha.proximity' }, note('Comes from Home Assistant’s own Proximity helper, with the car’s device tracker as the tracked device. It needs location publishing on, and knows of an arrival about 75 s late.')),
    ];
  }

  function eventsTiles() {
    const recipe = (t, cid, uses, text, s) => tile({ w: 1, title: t, icon: 'spark', cover: cid, side: statusChip(s) },
      h('p', { class: 'set-a' }, text), h('p', { class: 'note' }, 'Uses ', uses.map((u, i) => [i ? ', ' : '', code(u)]), '.'));
    return [
      sectionHead('Events for automations', 'Values reach Sentry Mate about 75 s after the car sent them. Fine for reminders and logging, too late to open a door as the car pulls up.'),
      tile({ w: 2, title: 'Events', icon: 'clock', cover: 'nr.overview' },
        note(['Automations should react to what happened, not to a value changing. One message per event goes to ', code(`${ns()}/cars/${carId || 'c1'}/event`), '. It is never retained, so a restart never fires an automation again. Sentry Mate only publishes; nothing here can control the car.']),
        group(null, EVENTS.map(([t, w, c]) => infoRow({ name: code(t), sub: `${w}: ${c}.` })))),
      tile({ w: 2, title: 'Recent events', icon: 'timeline' },
        events.length ? h('div', { class: 'log', role: 'list' }, events.map((e) => h('div', { class: 'log-row', role: 'listitem' },
          h('span', { class: 'log-k' }, `${formatDateTime(e.at_ms, ctx.tz, { now: api.now() })}  ${e.type}`), h('span', { class: 'log-v' }, Object.keys(e.payload || {}).length ? `${Object.keys(e.payload).length} fields` : ''))))
          : said('info', carId ? 'No events in the last 30 days.' : 'Events appear here once a car has uploaded logs.'),
        note('The last 30 days, newest first, as a subscriber got them.')),
      recipe('Garage door', 'ha.automation.garage', ['drive_ended', 'end_place'], 'Close the garage door, or log the arrival, when a drive ends at home. Opening it on arrival is better left to the car’s own HomeLink.', 'published'),
      recipe('Left open', 'ha.automation.left-open', ['doors_open', 'trunk_open', 'frunk_open'], 'Remind after five minutes with a door, the trunk or the frunk open, and every five minutes after.', 'published'),
      recipe('Stop the reminder', 'ha.automation.left-open-cancel', ['doors_open', 'trunk_open', 'frunk_open'], 'End the repeat as soon as everything reads closed again.', 'published'),
      recipe('Plug in tonight', 'ha.automation.plug-in-reminder', ['plugged_in', 'soc', 'range'], 'An evening reminder when the car is home and not plugged in, with its battery and range.', 'published'),
      recipe('State changed', 'nr.notify.state-change', ['drive_started', 'drive_ended', 'state'], 'A message when a drive starts or ends; drive_ended carries the whole drive.', 'published'),
      recipe('Someone got in', 'nr.notify.driver-present', ['user_present'], 'A message when someone is seen in the car.', 'published'),
      recipe('Time to full', 'nr.notify.time-to-full', ['time_to_full', 'charge_power', 'soc'], 'A progress message while charging.', 'published'),
      recipe('Arrived, left', 'nr.notify.geofence', ['place', 'start_place', 'end_place'], 'A message when the car leaves one saved place or arrives at another.', 'published'),
    ];
  }

  function apiTile() {
    return tile({ w: 2, title: 'API access', icon: 'server', side: chip('Built in', { tone: 'teal', icon: 'check' }) },
      group(null, [
        infoRow({ icon: 'shield', cover: 'faq.public-api', name: 'What you can build on', sub: ['Two things are kept stable: the MQTT topics on this page, and the read API under ', code(`${location.origin}/api/v1/`), '. Every UI pack is built on that API. Never read the storage directly: it is sealed.'] }),
        infoRow({ icon: 'chip', cover: 'projects.teslamateapi', name: 'A JSON API, built in', sub: ['Cars, live values, drives, charges, places and reports, as JSON or CSV. For example ', code('GET /api/v1/cars'), '.'] }),
        infoRow({ icon: 'person', name: 'Who sees what', sub: 'The API signs in like this app and answers for that account only: a watch account sees just its own cars.' }),
        infoRow({ icon: 'bars', cover: 'projects.custom-dashboards', name: 'More analysis, already here', sub: 'Costs, battery health, charging by place and every gap are pages of this app, each with a CSV download.' }),
        infoRow({ icon: 'person', cover: 'projects.matedroid', name: 'On a phone', sub: 'This app works in a phone’s browser and installs to the home screen. A phone app of your own can use the same API.' }),
      ]));
  }

  function networkTile() {
    if (!net) {
      return tile({ w: 2, title: 'Network use', icon: 'globe', cover: 'faq.network-flows' },
        said('info', admin ? 'The connection list could not be read.' : 'The list of connections is shown to admins. By default Sentry Mate makes no outbound connection at all.'));
    }
    const on = net.outbound.filter((o) => o.enabled).length;
    return tile({ w: 2, title: 'Network use', icon: 'globe', cover: 'faq.network-flows', side: chip(`${on} of ${net.outbound.length} out on`, { tone: on ? 'sun' : 'teal' }) },
      note('Sentry Mate needs no cloud: the CAN Bridge uploads to it, and everything else is off until you turn it on. A Tesla account is optional; without one no connection to Tesla is made.'),
      h('ul', { class: 'checks' },
        net.outbound.map((o) => h('li', { class: ['check', o.enabled ? 'is-warn' : 'is-off'] }, h('span', { class: 'check-dot', 'aria-hidden': 'true' }),
          h('span', { class: 'check-text' }, h('span', { class: 'check-name' }, `Out: ${(PURPOSE[o.purpose] || [o.purpose])[0]}`), h('span', { class: 'check-sub' }, `${o.host || 'no host set'} · ${(PURPOSE[o.purpose] || [null, ''])[1]}${o.last_used_ms ? ' Last used ' : ''}`, o.last_used_ms ? ago(o.last_used_ms) : null,
            changeLink(PURPOSE[o.purpose]))),
          h('span', { class: 'check-word' }, o.enabled ? 'On' : 'Off'))),
        net.inbound.map((i) => h('li', { class: 'check is-ok' }, h('span', { class: 'check-dot', 'aria-hidden': 'true' }),
          h('span', { class: 'check-text' }, h('span', { class: 'check-name' }, `In: ${(PURPOSE[i.purpose] || [i.purpose])[0]}`), h('span', { class: 'check-sub' }, `Port ${i.port}${i.tls ? ', HTTPS with TLS 1.3' : ''}`)),
          h('span', { class: 'check-word' }, 'Open')))));
  }

  function worksTiles() {
    const w = (t, cid, text, uses) => tile({ w: 1, title: t, icon: 'external', cover: cid }, h('p', { class: 'set-a' }, text), h('p', { class: 'note' }, 'Uses ', uses));
    return [
      sectionHead('Works with', 'Other people’s tools, named so you know what fits. Sentry Mate does not make, check or vouch for them; anything built on the topics or the API keeps working.', { cover: 'projects.disclaimer' }),
      w('Route planners', 'projects.abrp', 'A planner that takes the live battery level plans with the real charge. A small MQTT bridge of yours forwards the values, if you want that.', [code('soc'), ', ', code('location'), ', ', code('speed'), '.']),
      w('Home Assistant Tesla add-ons', 'projects.ha-tesla', 'Integrations that can read a car’s values from MQTT instead of asking Tesla’s servers.', [code(`${ns()}/cars/<id>/#`), '.']),
      w('Desk gauges and lights', 'projects.gaussmeter', 'A light whose colour follows the state and whose level follows the battery.', [code('state'), ' and ', code('soc'), '.']),
      w('Mirrors and wall screens', 'projects.magic-mirror', 'A status tile on a mirror or wall display, from MQTT or the API.', [code('soc'), ', ', code('range'), ', ', code('state'), '.']),
      w('Garage door openers', 'projects.geogdo', 'Openers that act on arrival and departure, by zone or saved place. Good for closing behind you.', [code('location'), ', ', code('place'), ' and drive events.']),
    ];
  }

  /* ---------- the car for the preview */
  const carPills = cars.length > 1 ? pills({ label: 'Preview for', value: carId, options: cars.map((c) => [c.id, c.name || c.id, state.isSold(c)]), onchange: async (v) => { carId = v; ctx.setQuery({ car: v }); await loadCar(); if (ctx.alive()) draw(); } }) : null;

  function draw() {
    statusEl = statusTile();
    ctx.head({ actions: [headChip()] });
    ctx.view.replaceChildren(
      tile({ w: 'full', cls: 'tile-intro' }, said('ok', admin ? 'Switches and single fields save at once and a toast confirms it; the broker connection saves with its button. Nothing leaves the server until publishing is on. Tesla account, TeslaFi and local TeslaMate changes ask for your password again.' : 'Your account can read these; an admin changes them.')),
      tesla.el,
      teslafi.el,
      teslamate.el,
      sectionHead('Sending values to other systems'),
      brokerTile(), statusEl, topicsTile(), haTile(),
      h('div', { class: 'page-bar w-full' }, h('h2', { class: 'sub-head' }, 'Published values'), carPills ? h('span', { class: 'page-bar-end' }, carPills) : null),
      valuesTile(),
      sectionHead('Home Assistant entities'), ...entityTiles(),
      ...eventsTiles(),
      sectionHead('API and network'), apiTile(), networkTile(),
      ...worksTiles());
  }
  draw();
  ctx.ready();
  return {};
}
