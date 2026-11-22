// Settings: Integrations (/settings/integrations): send the cars' live values to other systems, if the
// owner wants to. The MQTT broker (with a test and live status), the topic layout and its rules, every
// value Sentry Mate publishes (previewed for a chosen car, each with its unit, source and status),
// Home Assistant discovery and the entities it gets, the events automations listen to, the read API,
// every network connection the server makes, and notes on tools that use the published values.
//
// Reading the broker settings, the network list and the broker log is for admins; anyone may read the
// status, the preview and the events of the cars they see. Changes: the switches and single fields save
// at once (PUT settings/mqtt with the whole setting and its version); the connection has a Save button,
// because host, port, TLS, pin and password are checked together.

import { h, replace } from '../shared/dom.js';
import { buildUrl, lastCar, preferredCar, rememberCar, isSold } from '../shared/state.js';
import { downloadCsv, csvFilename } from '../shared/csv.js';
import { icon } from '../core/icons.js';
import { soldParts, soldOptions } from '../core/carswitch.js';
import { pageHead, panel, panelLink, kv, tag, pill, note, csvButton, metric, figures } from '../core/page.js';
import { dataTable } from '../core/table.js';
import { field, form, showApiError } from '../core/form.js';
import { emptyState, errorWords } from '../core/states.js';
import { flash } from '../core/flash.js';
import { confirmDialog } from '../core/dialog.js';
import { dateTime, ago, num } from '../core/fmt.js';
import { teslaSection } from './tesla-account.js';
import { teslafiSection } from './teslafi-account.js';
import { teslamateSection } from './teslamate-local.js';

export const id = 'settings-integrations';
export const css = true;

const enc = encodeURIComponent;
const DAY = 86_400_000;

/* ---------------------------------------------------------------------- what is published, and why */

// status of a value: what a consumer can count on
const STATUS = {
  published: ['Published', 'live', 'Sent now, retained, in the server units.'],
  logged: ['Logged, not yet sent', 'plain', 'Sentry Mate has the value (from the car or worked out); it is not its own topic yet.'],
  signal: ['Needs a new CAN signal', 'warm', 'The car sends it; Sentry Mate does not decode it yet. Nothing is guessed meanwhile.'],
  api: ['From Tesla API / TeslaFi', 'live', 'No CAN message carries it; the Tesla API or TeslaFi’s polls give it, tagged with where it came from, and it goes stale (sent empty) when the input stops.'],
  none: ['Not on the CAN bus', 'plain', 'No message on the car’s buses carries it, and no other input gives it, so it is never published.'],
  never: ['Never published', 'plain', 'Left out on purpose.'],
  device: ['On the device', 'live', 'Sent once as part of the Home Assistant device, not as a topic.'],
};
const STATUS_ORDER = ['published', 'api', 'device', 'logged', 'signal', 'none', 'never'];
// the live inputs other than SMO, as a status word names them
const INPUT_WORD = { tesla_api: 'Tesla API', teslafi_api: 'TeslaFi', teslamate_local: 'TeslaMate' };
// the inputs an `api` row can come from (all of them so far)
const API_INPUTS = ['tesla_api', 'teslafi_api'];
// what the owner's own TeslaMate gives too (one flag for the windows, no window of its own, no sunroof)
const TM_KEYS = new Set(['version', 'update_available', 'update_version', 'locked', 'windows_open', 'scheduled_charge_start', 'route_destination', 'route_distance', 'route_minutes', 'route_traffic_delay']);
const apiInputs = (keys) => ([].concat(keys ?? []).some((k) => TM_KEYS.has(k)) ? [...API_INPUTS, 'teslamate_local'] : API_INPUTS);
const statusWord = (s, inputs = API_INPUTS) => (s === 'api' ? `From ${inputs.map((i) => INPUT_WORD[i] || i).join(' / ')}` : STATUS[s][0]);

// unit kinds in the server's units (payloads are text in the server's units, not each person's)
function unitText(kind, u) {
  const mi = u?.distance === 'mi';
  switch (kind) {
    case 'distance': return mi ? 'mi' : 'km';
    case 'speed': return mi ? 'mph' : 'km/h';
    case 'temperature': return u?.temperature === 'f' ? '°F' : '°C';
    case 'pressure': return u?.pressure === 'psi' ? 'psi' : 'bar';
    case 'elevation': return mi ? 'ft' : 'm';
    default: return kind || '';
  }
}

// [item id, topic key(s), what, unit kind, status, source, note]
const VALUES = [
  ['mqtt.display-name', null, 'Car name', null, 'device', 'Typed in Settings: Cars', 'The Home Assistant device name. Topics use the car id, so a rename never moves them.'],
  ['mqtt.state', 'state', 'State', null, 'published', 'The timeline', 'driving, charging, online, asleep or unknown. Unknown (no log, recording paused) is never called asleep.'],
  ['mqtt.since', 'since', 'State since', 'ISO time, UTC', 'published', 'The start of the timeline span', 'Exact to the CAN frame, not to a poll.'],
  ['mqtt.healthy', 'healthy', 'Logger healthy', 'true / false', 'published', 'The Data health rule', 'False when no log came while the car was awake, the newest log failed its check, or frames were lost in the last hour. See Data health.'],
  ['mqtt.version', 'version', 'Software version', null, 'api', 'The version the car reports to the Tesla API / TeslaFi', 'No version text on CAN. A software_changed event also comes when module build ids change.'],
  ['mqtt.update-available', 'update_available', 'Update waiting', 'true / false', 'api', 'The update status the Tesla API / TeslaFi reads', 'Only Tesla’s servers know; not on CAN.'],
  ['mqtt.update-version', 'update_version', 'Waiting update version', null, 'api', 'The waiting update, as the Tesla API / TeslaFi reads it', 'Not sent while none waits; the software_changed event tells you afterwards.'],
  ['mqtt.download-perc', 'update_download', 'Update download %', '%', 'api', 'Download progress from the Tesla API / TeslaFi', 'No progress on CAN.'],
  ['mqtt.install-perc', 'installing', 'Update install %', 'true / false', 'published', 'Update status (0x343), else the Tesla API / TeslaFi', 'No percentage on CAN: an installing true/false flag instead.'],
  ['mqtt.vin', null, 'VIN', null, 'never', 'Only the first 11 characters are ever kept', 'The VIN is never published, so a broker or Home Assistant never holds it.'],
  ['mqtt.model', null, 'Model', null, 'device', 'The VIN prefix', 'Model and year are the Home Assistant device model.'],
  ['mqtt.trim-badging', 'trim', 'Trim', null, 'published', 'The VIN prefix (drive units, battery)', 'Shown in Settings: Cars; the car configuration message can confirm it.'],
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
  ['mqtt.locked', 'locked', 'Locked', 'true / false', 'api', 'The lock state the Tesla API / TeslaFi reads', 'Never from a lock request: CAN only carries the request, which is not the lock state.'],
  ['mqtt.sentry-mode', 'sentry_mode', 'Sentry mode', 'true / false', 'published', 'Vehicle modes (0x284 UIsentryMode284)', ''],
  ['mqtt.windows-open', 'windows_open', 'Any window open', 'true / false', 'api', 'The four windows as the Tesla API / TeslaFi reads them', 'CAN only has switch presses, which cannot tell open from closed. False only when all four read closed.'],
  ['mqtt.window-each', ['window_driver_front', 'window_passenger_front', 'window_driver_rear', 'window_passenger_rear'], 'Each window', 'true / false', 'api', 'The Tesla API / TeslaFi (fd, fp, rd, rp)', 'Named by seat: driver front, passenger front, driver rear, passenger rear.'],
  ['mqtt.doors-open', 'doors_open', 'Any door open', 'true / false', 'published', 'Door latches (0x102, 0x103)', ''],
  ['mqtt.door-each', ['door_front_left', 'door_front_right', 'door_rear_left', 'door_rear_right'], 'Each door', 'true / false', 'published', 'Door latches (0x102 left, 0x103 right)', 'Named by side; right-hand drive comes from the car configuration.'],
  ['mqtt.sun-roof-state', 'sunroof_state', 'Sunroof position', null, 'api', 'The Tesla API / TeslaFi', 'closed, open, vent … as the input says it. Only a car with a sunroof sends it; the roof type is below.'],
  ['mqtt.sun-roof-installed', 'roof', 'Roof type', null, 'published', 'Car configuration (0x7FF GTW_roofType)', 'Metal, fixed glass or panoramic.'],
  ['mqtt.sun-roof-percent-open', 'sunroof_open', 'Sunroof % open', '%', 'api', 'The Tesla API / TeslaFi', 'No roof position on CAN.'],
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
  ['mqtt.scheduled-charging-start-time', 'scheduled_charge_start', 'Scheduled charge start', 'ISO time, UTC', 'api', 'The charge schedule the Tesla API / TeslaFi reads', 'CAN only has the schedule on/off flags. Not sent while no start is set.'],
  [null, 'scheduled_departure', 'Scheduled departure', 'ISO time, UTC', 'api', 'The departure the Tesla API / TeslaFi reads', 'The time set for off-peak charging and preconditioning to be done by.'],
  ['mqtt.time-to-full-charge', 'time_to_full', 'Time to full', 'min', 'published', 'Time remaining (0x132)', ''],
  ['mqtt.tpms-pressure', ['tire_pressure_fl', 'tire_pressure_fr', 'tire_pressure_rl', 'tire_pressure_rr'], 'Tyre pressures', 'pressure', 'published', 'Tyre sensors (0x219)', 'Front left, front right, rear left, rear right.'],
  ['mqtt.tpms-soft-warning', 'tire_low', 'Tyre pressure low', 'true / false', 'published', 'Pressure against the recommended pressure (0x219 page 4)', ''],
  ['mqtt.active-route', ['route_destination', 'route_distance', 'route_minutes', 'route_traffic_delay'], 'Route (destination, distance, minutes, traffic)', 'distance, min', 'api', 'The active route the Tesla API / TeslaFi reads', 'While navigating: destination, distance left (server units), minutes left and minutes lost to traffic. Navigating and the energy at the destination come from CAN.'],
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
].map(([cid, keys, what, unitKind, status, source, why]) => ({ cid, keys: keys == null ? [] : Array.isArray(keys) ? keys : [keys], what, unitKind, status, source, why, inputs: status === 'api' ? apiInputs(keys) : null }));

// Home Assistant: what it gets, per group, with the same statuses
const ENTITY_GROUPS = [
  ['ha.entity.location-tracker', 'Device tracker', 'The car on the Home Assistant map, home or away by zone', 'location', 'published', 'Only with location publishing on. About 75 s behind live (one-minute logs and the upload).'],
  ['ha.entity.active-route-tracker', 'Destination tracker', 'Where the car is navigating to', null, 'none', 'The destination’s position is on no input Sentry Mate reads: the route gives its name (Route destination), not where it is.'],
  ['ha.entity.status-sensors', 'Status sensors', 'State, State since, Charging, Screen mode, Last data', 'state, since, charging, display_mode, updated', 'published', 'Software version and waiting updates come from the Tesla API or TeslaFi (Software sensors).'],
  [null, 'Software sensors', 'Software version, Software update, Update version, Update downloaded', 'version, update_available, update_version, update_download', 'api', 'Not on CAN; unknown in Home Assistant while the input’s reading is stale.'],
  ['ha.entity.config-sensors', 'Build sensors', 'Paint, wheels, spoiler, roof, trim', 'paint, wheels, spoiler, roof, trim', 'published', 'Model and year are on the device already.'],
  ['ha.entity.motion-sensors', 'Motion sensors', 'Place, Gear, Power, Speed, Heading, Elevation', 'place, gear, power, speed, heading, elevation', 'published', 'Heading only while driving: the chassis bus is quiet when parked.'],
  ['ha.entity.parking-brake', 'In park', 'On while the gear is P', 'in_park', 'published', 'From the gear. The parking brake itself (0x228, 0x288) is on CAN as well.'],
  ['ha.entity.temperature-sensors', 'Temperature sensors', 'Inside, outside and battery temperature', 'inside_temp, outside_temp, battery_temp', 'published', 'Battery temperature is extra.'],
  ['ha.entity.range-sensors', 'Range sensors', 'Odometer, rated, ideal and estimated range', 'odometer, range, range_ideal, range_estimated', 'published', 'Estimated range is Sentry Mate’s own, labelled as such.'],
  ['ha.entity.charging-sensors', 'Charging sensors', 'Battery, Usable level, Charge status, Limit, Charge power, Charger, Current, Voltage, Phases, Current asked for, Current limit, Time to full, Energy remaining', 'soc, soc_usable, charge_status, charge_limit, charge_power, charge_kind, charger_*, charge_current_*, time_to_full, energy_remaining', 'published', 'Current, voltage, phases and time to full are sent while charging.'],
  ['ha.entity.charge-energy-added.counter', 'Energy added counter', 'A running total for Home Assistant’s Energy view', 'charge_energy_added', 'published', 'Sent as the pack’s lifetime counter (total_increasing), which never resets, rather than a per-charge value that does.'],
  ['ha.entity.tpms-sensors', 'Tyre sensors', 'Four tyre pressures, Pressure low', 'tire_pressure_fl … rr, tire_low', 'published', 'Which sensor is which wheel is still to be confirmed on new firmware.'],
  ['ha.entity.active-route-destination', 'Route destination', 'Destination name', 'route_destination', 'api', 'Not on CAN.'],
  ['ha.entity.active-route-energy', 'Route energy', 'Navigating, Battery expected on arrival', 'navigating, energy_at_destination', 'published', 'Shown as a real 0; unavailable while not navigating.'],
  ['ha.entity.active-route-distance', 'Route distance', 'Distance left', 'route_distance', 'api', 'Not on CAN.'],
  ['ha.entity.active-route-minutes', 'Route minutes', 'Minutes left', 'route_minutes', 'api', 'Not on CAN.'],
  ['ha.entity.active-route-traffic', 'Route traffic', 'Minutes lost to traffic', 'route_traffic_delay', 'api', 'Not on CAN.'],
  ['ha.entity.binary-status', 'Health on/off', 'Logger healthy, Update installing, Service mode', 'healthy, installing, service_mode', 'published', 'Update installing is CAN’s flag, else the Tesla API’s or TeslaFi’s word; a waiting update is in Software sensors.'],
  ['ha.entity.binary-security', 'Security on/off', 'Doors (any and each), trunk, frunk, Sentry mode, someone in the car', 'doors_open, door_*, trunk_open, frunk_open, sentry_mode, user_present', 'published', 'Lock and windows are their own group, from the Tesla API or TeslaFi.'],
  [null, 'Lock and windows', 'Lock (Home Assistant’s lock class), Windows (any and each), Sunroof, Sunroof open', 'locked, windows_open, window_*, sunroof_state, sunroof_open', 'api', 'Never guessed from a lock request; unknown while the reading is stale.'],
  [null, 'Charge schedule', 'Scheduled charge start, Scheduled departure', 'scheduled_charge_start, scheduled_departure', 'api', 'Timestamps; not on CAN.'],
  ['ha.entity.binary-climate-charge', 'Climate and charging on/off', 'Climate, preconditioning, cable, charge port', 'climate_on, preconditioning_*, plugged_in, charge_port_open', 'published', ''],
  ['ha.proximity', 'Distance to home', 'Home Assistant’s own proximity helper, fed by the device tracker', 'location', 'published', 'Needs location publishing on. Arrival is known about 75 s late.'],
].map(([cid, what, gives, topics, status, why]) => ({ cid, what, gives, topics, status, why, inputs: status === 'api' ? apiInputs(topics) : null }));

// the events: one topic, never retained, so a restart never repeats one
const EVENTS = [
  ['drive_started', 'A drive starts', 'drive id, start time and place, battery, range and odometer at the start'],
  ['drive_ended', 'A drive ends', 'the whole drive: distance, duration, energy, efficiency, top speed, power, end place, battery and range at the end, temperatures, climb'],
  ['charge_started', 'A charge starts', 'charge id, kind (ac or dc), place, battery at the start'],
  ['charge_ended', 'A charge ends', 'energy added, battery at the end, battery temperature, cost when the place has a price'],
  ['software_changed', 'Module builds changed', 'the update id and, when known, the version you typed or a matching build'],
  ['user_present', 'Someone is seen in the car', 'the time and place'],
];
const EVENT_TYPES = EVENTS.map((e) => e[0]);

/* ---------------------------------------------------------------------- small pieces */

const statusTag = (s, inputs) => tag(statusWord(s, inputs), STATUS[s][1]);
const code = (t) => h('code', { class: 'si-code' }, t);
// a long topic or id: may break after each / or _, never inside a word
const codeLong = (t) => h('code', { class: 'si-code si-code--long' }, String(t).split(/(?<=[/_])/).flatMap((part, i) => (i ? [h('wbr'), part] : [part])));
const setting = (titleText, text, control, cv = null) => h('div', { class: 'setting', cover: cv },
  h('div', null, h('h3', null, titleText), text ? h('p', null, text) : null), h('div', { class: 'setting-control' }, control));
const errText = (e) => (e?.fields ? Object.values(e.fields).join(' ') : errorWords(e).text);

function copyButton(textFn, label = 'Copy') {
  return h('button', {
    class: 'btn btn--sm', type: 'button',
    onclick: async () => {
      try { await navigator.clipboard.writeText(textFn()); flash('Copied.'); } catch { flash('This browser did not allow copying; select the text instead.', { kind: 'info' }); }
    },
  }, icon('book', 'ico--sm'), label);
}

function sw(idText, checked, labelText, onChange, disabled = false) {
  const input = h('input', { type: 'checkbox', id: idText, role: 'switch', checked: !!checked, disabled, onchange: () => onChange(input.checked, input) });
  return { el: h('div', { class: 'switch-row si-switch' }, h('span', { class: 'switch' }, input, h('i', { 'aria-hidden': 'true' })), h('label', { for: idText }, labelText)), input };
}

/* ====================================================================== the page */

export async function mount(ctx) {
  const admin = ctx.admin;
  let cfg = null, status = null, net = null, cars = null;
  try {
    [cfg, status, net, cars] = await Promise.all([
      admin ? ctx.api.get('settings/mqtt') : Promise.resolve(null),
      ctx.api.get('settings/mqtt/status').catch(() => null),
      admin ? ctx.api.get('network').catch(() => null) : Promise.resolve(null),
      ctx.api.get('cars', { sort: 'name', limit: 100 }),
    ]);
  } catch (e) { if (!ctx.alive()) return; ctx.fail(e); return; }
  if (!ctx.alive()) return;

  const carList = cars?.items || [];
  // the preview car: the address's, else the person's preferred car (Main car, last looked at, first in their order)
  const findCar = async (id) => {
    if (!id) return null;
    const known = carList.find((c) => c.id === id);
    if (known) return known;
    try { return await ctx.api.get(`cars/${enc(id)}`); } catch (e) { if (e.name === 'AbortError') throw e; return null; }
  };
  let extraCar = null, carMissing = null;
  try {
    const wanted = ctx.query.car || null;
    extraCar = await findCar(wanted);
    if (wanted && !extraCar) carMissing = wanted;
    if (!extraCar) extraCar = await findCar(preferredCar(carList) || lastCar());
    if (!extraCar && carList.length) extraCar = (await ctx.api.page('cars', { sort: 'custom', limit: 1 }).catch(() => null))?.items?.[0] || carList[0];
  } catch (e) { if (!ctx.alive()) return; ctx.fail(e); return; }
  if (!ctx.alive()) return;
  let carId = extraCar?.id || null;
  if (carMissing) ctx.setQuery({ car: carId });
  let pv = null;
  async function loadPreview() {
    if (!carId) { pv = null; return; }
    try { pv = await ctx.api.get('settings/mqtt/preview', { car: carId }); } catch (e) { if (e.name === 'AbortError') throw e; pv = { error: e, messages: [], ha_entities: [] }; }
  }
  await loadPreview();
  if (!ctx.alive()) return;

  // what the settings say, also for an account that may not read them (from the preview's topics)
  const ns = () => cfg?.namespace || (pv?.messages?.[0]?.topic || 'sentrymate/status').replace(/\/status$/, '');
  const units = () => pv?.units || { distance: 'km', temperature: 'c', pressure: 'bar' };

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
      flash(`${what} saved.`);
      await refreshAll();
      return null;
    } catch (e) {
      if (e?.name === 'AbortError') return e;
      if (e.status === 409) {
        cfg = await ctx.api.get('settings/mqtt');
        flash('Someone else changed these settings; this page shows theirs now.', { kind: 'info' });
        await refreshAll();
        return e;
      }
      flash(`Could not save ${what.toLowerCase()}: ${errText(e)}`, { kind: 'error' });
      return e;
    }
  }
  async function refreshAll() {
    [status, net] = await Promise.all([
      ctx.api.get('settings/mqtt/status').catch(() => status),
      admin ? ctx.api.get('network').catch(() => net) : Promise.resolve(net),
    ]);
    await loadPreview();
    if (!ctx.alive()) return;
    paintHead(); paintBroker(); paintStatus(); paintTopics(); paintValues(); paintHa(); paintEntities(); paintNetwork(); paintEventTopic(); paintEvents();
  }

  /* ---------- the head */
  const headSlot = h('div', { class: 'si-head-status' });
  function paintHead() {
    if (!status) { replace(headSlot, pill('Status not known', { lg: true })); return; }
    replace(headSlot,
      !status.enabled ? pill('Publishing off', { lg: true })
        : status.connected ? pill(`Connected to ${status.broker}`, { tone: 'live', lg: true }) : pill('Not connected', { tone: 'warm', lg: true }));
  }
  const tesla = teslaSection(ctx, { cars: carList });
  const teslafi = teslafiSection(ctx, { cars: carList });
  const teslamate = teslamateSection(ctx, { cars: carList });
  const jumps = [['tesla', 'Tesla account'], ['teslafi', 'TeslaFi'], ['teslamate', 'Local TeslaMate'], ['si-broker', 'Broker'], ['si-topics', 'Topics'], ['si-values', 'Values'], ['si-ha', 'Home Assistant'], ['si-events', 'Events'], ['si-api', 'API'], ['si-network', 'Network'], ['si-works', 'Works with']];
  const jumpNav = h('nav', { class: 'si-jump', 'aria-label': 'Sections of this page' },
    jumps.map(([target, label]) => h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: () => document.getElementById(target)?.scrollIntoView({ behavior: 'smooth', block: 'start' }) }, label)));

  /* ---------- MQTT broker */
  const brokerSlot = h('div', { class: 'si-broker-form' });
  function paintBroker() {
    if (!cfg) {
      replace(brokerSlot, note(icon('lock', 'ico--sm'), ' Only an admin sees and changes the broker settings. The status beside this, the preview and the events below are yours to read.'),
        // the items are still what this page offers, read-only for this account
        h('ul', { class: 'si-list', cover: ['docs.env.mqtt.disable-mqtt', 'docs.env.mqtt.mqtt-host', 'docs.env.mqtt.mqtt-port', 'docs.env.mqtt.mqtt-username', 'docs.env.mqtt.mqtt-password', 'docs.env.mqtt.mqtt-tls', 'docs.env.mqtt.mqtt-tls-accept-invalid-certs', 'docs.env.mqtt.mqtt-ipv6', 'docs.env.mqtt.mqtt-namespace'] },
          h('li', null, 'Publishing: ', h('b', null, status?.enabled ? 'on' : 'off')),
          h('li', null, 'Broker: ', h('b', null, status?.broker || 'not set'), status?.tls ? ' over TLS' : status?.broker ? ' without TLS' : ''),
          h('li', null, 'Topics start with ', code(ns()))));
      return;
    }
    const onSw = sw('si-enabled', cfg.enabled, 'Publish to the broker', async (on, input) => {
      if (on && !cfg.host) { input.checked = false; flash('Set the broker host first, and save the connection.', { kind: 'info' }); hostF.input.focus(); return; }
      if (await save({ enabled: on }, on ? 'Publishing on' : 'Publishing off')) input.checked = cfg.enabled;
    });
    const hostF = field({ label: 'Host', name: 'host', value: cfg.host, hint: 'A name such as broker.local. It may resolve to IPv4 or IPv6: the connection follows the address, so there is no IPv6 switch.', attrs: { autocomplete: 'off', spellcheck: 'false', placeholder: 'broker.local' } });
    const portF = field({ label: 'Port', name: 'port', type: 'number', value: cfg.port, hint: '8883 is the usual port for MQTT over TLS, 1883 without.', attrs: { min: '1', max: '65535', step: '1' } });
    const userF = field({ label: 'User name', name: 'username', path: 'username', value: cfg.username || '', hint: 'Empty for a broker without accounts.', attrs: { autocomplete: 'off' } });
    const passF = field({ label: 'Password', name: 'password', type: 'password', hint: cfg.password_set ? 'A password is saved. Leave this empty to keep it; it is never shown again.' : 'No password saved. It is only ever sent over TLS.', attrs: { autocomplete: 'new-password', placeholder: cfg.password_set ? '•••••••• (kept)' : '' } });
    const tlsF = field({ label: 'Encrypt the connection (TLS)', name: 'tls', type: 'checkbox', value: cfg.tls, hint: 'On by default. Without TLS no password is sent, and the server refuses to save one.' });
    const pinF = field({ label: 'Pinned certificate (SHA-256)', name: 'pin', path: 'pin_sha256', value: cfg.pin_sha256 || '', hint: 'For a broker with its own (self-signed) certificate: its SHA-256, 64 hex digits, colons allowed. Only that certificate is accepted; there is no “accept any certificate” switch. Empty: the certificate must be valid for the host.', attrs: { autocomplete: 'off', spellcheck: 'false' } });
    const nsF = field({ label: 'Topic prefix', name: 'namespace', value: cfg.namespace, hint: 'Every topic starts with this. Two servers on one broker use different prefixes. Levels with /, no + or #, not starting with $.', attrs: { autocomplete: 'off', spellcheck: 'false' } });
    const testOut = h('div', { class: 'si-test', 'aria-live': 'polite' });
    const testBtn = h('button', {
      class: 'btn', type: 'button', onclick: async () => {
        testBtn.disabled = true;
        replace(testOut, h('p', { class: 'note' }, 'Connecting…'));
        try {
          const r = await ctx.api.post('settings/mqtt/test');
          if (!ctx.alive()) return;
          replace(testOut, h('p', { class: ['si-test-line', r.ok ? 'ok' : 'warn'] }, icon(r.ok ? 'check' : 'warn', 'ico--sm'), ' ', r.message,
            r.ok && r.latency_ms != null ? ` ${num(r.latency_ms)} ms.` : '', r.ok && r.topic ? [' Topic ', code(r.topic), '.'] : '',
            h('span', { class: 'faint' }, ` ${dateTime(r.at_ms, ctx.tz)}`)));
          status = await ctx.api.get('settings/mqtt/status').catch(() => status);
          paintStatus(); paintHead(); paintLog();
        } catch (e) { if (e.name !== 'AbortError') replace(testOut, h('p', { class: 'si-test-line warn' }, icon('warn', 'ico--sm'), ' ', errText(e))); } finally { testBtn.disabled = false; }
      },
    }, icon('pulse', 'ico--sm'), 'Test the saved settings');
    const removePw = cfg.password_set ? h('button', {
      class: 'btn btn--sm btn--ghost', type: 'button', onclick: async () => {
        if (!(await confirmDialog('The broker password is deleted from Sentry Mate. Publishing then connects without one.', { title: 'Remove the password?', ok: 'Remove', danger: true }))) return;
        await save({ password: null }, 'Password removal');
      },
    }, icon('x', 'ico--sm'), 'Remove the saved password') : null;
    const f = form({
      fields: [hostF, portF, userF, passF, tlsF, pinF, nsF], submit: 'Save connection', cls: 'si-form',
      secondary: testBtn,
      async onSubmit(v) {
        const patch = { host: v.host.trim(), port: v.port == null ? null : Math.round(v.port), username: v.username.trim() || null, tls: !!v.tls, pin_sha256: v.pin.trim() || null, namespace: v.namespace.trim() };
        if (v.password) patch.password = v.password;
        // the same rules the server checks, said here first
        const bad = {};
        if (cfg.enabled && !patch.host) bad.host = 'Publishing is on, so the broker needs a host.';
        if (patch.pin_sha256 && !patch.tls) bad.pin_sha256 = 'A certificate pin needs TLS.';
        if ((v.password || cfg.password_set) && !patch.tls) bad.tls = 'A password is only ever sent over TLS. Keep TLS on, or remove the password.';
        if (/[+#]/.test(patch.namespace) || patch.namespace.startsWith('$') || patch.namespace.split('/').some((p) => !p)) bad.namespace = 'No + or #, not starting with $, and no empty level.';
        if (Object.keys(bad).length) { f.setErrors(bad); return; }
        try {
          cfg = await ctx.api.put('settings/mqtt', body(patch));
        } catch (e) {
          if (e.status === 409) { cfg = await ctx.api.get('settings/mqtt'); flash('Someone else changed these settings; this page shows theirs now.', { kind: 'info' }); await refreshAll(); return; }
          throw e;
        }
        flash('Connection saved. Test it to see the broker answer.');
        await refreshAll();
      },
    });
    replace(brokerSlot,
      setting('Publishing', 'Off until you turn it on: nothing connects anywhere before that. Off stops every message; the broker keeps what it last got.', onSw.el, 'docs.env.mqtt.disable-mqtt'),
      h('div', { class: 'si-form-wrap' }, f.el, removePw, testOut));
    for (const [fl, cid] of [[hostF, 'docs.env.mqtt.mqtt-host'], [portF, 'docs.env.mqtt.mqtt-port'], [userF, 'docs.env.mqtt.mqtt-username'], [passF, 'docs.env.mqtt.mqtt-password'], [tlsF, 'docs.env.mqtt.mqtt-tls'], [pinF, 'docs.env.mqtt.mqtt-tls-accept-invalid-certs'], [nsF, 'docs.env.mqtt.mqtt-namespace']]) fl.el.setAttribute('data-cover', cid);
    hostF.el.setAttribute('data-cover', 'docs.env.mqtt.mqtt-host docs.env.mqtt.mqtt-ipv6');
  }

  /* ---------- status and the broker log */
  const statusSlot = h('div', { class: 'si-status' });
  function paintStatus() {
    if (!status) { replace(statusSlot, note('The broker status could not be read.')); return; }
    const s = status;
    replace(statusSlot,
      h('div', { class: 'si-status-top' },
        !s.enabled ? pill('Publishing off', { lg: true }) : s.connected ? pill('Connected', { tone: 'live', lg: true }) : pill('Not connected', { tone: 'warm', lg: true }),
        h('span', { class: 'faint' }, s.broker ? [s.broker, s.tls ? ' · TLS' : ' · no TLS'] : 'No broker set'),
        h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: async () => { status = await ctx.api.get('settings/mqtt/status').catch(() => status); if (ctx.alive()) { paintStatus(); paintHead(); } } }, icon('reset', 'ico--sm'), 'Refresh')),
      figures([
        metric('Delivered', s.delivered, 'number', { digits: 0, explain: 'Messages the broker confirmed since the server started.' }),
        metric('Waiting', s.waiting, 'number', { digits: 0, explain: s.enabled ? 'Kept in memory until the broker is back; the newest value of each topic wins.' : 'Nothing waits while publishing is off.' }),
        metric('Dropped', s.dropped, 'number', { digits: 0, explain: 'Older values replaced by a newer one before the broker was back.' }),
      ], 'si-figs'),
      h('p', { class: ['si-last-error', s.last_error ? 'warn' : 'faint'] }, icon(s.last_error ? 'warn' : 'check', 'ico--sm'),
        s.last_error ? [' Last error: ', h('b', null, ERROR_WORDS[s.last_error] || s.last_error), ', ', dateTime(s.last_error_ms, ctx.tz), ` (${ago(s.last_error_ms)}).`] : ' No error since the server started.'));
  }
  const ERROR_WORDS = { connection_refused: 'the broker refused the connection', pin_mismatch: 'the certificate did not match the pin', no_host: 'no broker host set', tls_failed: 'TLS failed', auth_failed: 'user name or password refused' };

  const logSlot = h('div', { class: 'si-log' });
  async function paintLog() {
    if (!admin) { replace(logSlot, note('The broker log (changes and tests) is shown to admins.')); return; }
    let rows = [];
    try {
      const now = ctx.api.now();
      // the audit list has no target filter: read pages until 8 broker rows are found or the 90 days run out
      const p = ctx.api.pager('audit', { from_ms: now - 90 * DAY, to_ms: now + 60_000 }, { limit: 200 });
      for (let i = 0; i < 50 && rows.length < 8 && !p.done; i++) {
        const got = (await p.more()) || [];
        if (!ctx.alive()) return;
        rows = rows.concat(got.filter((e) => e.target === 'settings:mqtt'));
      }
      rows = rows.slice(0, 8);
    } catch (e) { if (e.name === 'AbortError') return; replace(logSlot, note('The broker log could not be read.')); return; }
    if (!ctx.alive()) return;
    replace(logSlot,
      h('div', { class: 'si-log-head' }, h('h3', null, 'Broker log'), h('span', { class: 'faint' }, 'Changes and tests, last 90 days'),
        rows.length ? csvButton({ onClick: () => downloadCsv(csvFilename('sentry-mate', 'mqtt-log'), rows, [{ label: 'Time (UTC)', value: (r) => new Date(r.at_ms).toISOString() }, { label: 'Account', value: (r) => r.account }, { label: 'Action', value: (r) => r.action }, { label: 'What happened', value: (r) => r.summary }]) }) : null),
      rows.length ? h('ol', { class: 'si-log-list' }, rows.map((r) => h('li', null, h('span', { class: 'faint nowrap' }, dateTime(r.at_ms, ctx.tz)), h('span', null, r.summary), h('span', { class: 'faint' }, r.account || '')))) : note('Nothing changed or tested in the last 90 days.'),
      h('a', { class: 'panel-link', href: buildUrl('settings-server') }, 'Whole audit log in Server and access', icon('chev-r', 'ico--sm')));
  }

  const brokerPanel = panel({ title: 'MQTT broker', icon: 'share', id: 'si-broker', cls: 'si-broker', side: h('span', { class: 'faint' }, admin ? 'Admin' : 'Read only') },
    h('div', { class: 'si-two' }, brokerSlot,
      h('div', { class: 'si-side' }, statusSlot, logSlot,
        h('div', { class: 'si-client', cover: 'nr.setup.mqtt' }, h('h3', null, 'Connect a client'),
          h('p', null, 'Node-RED, Home Assistant or any MQTT client uses this same broker. Subscribe to ', code(`${ns()}/cars/+/#`), ' for every car, or ', code(`${ns()}/cars/<car id>/#`), ' for one. Use TLS as here.')))));

  /* ---------- MQTT topics and rules */
  const topicsSlot = h('div');
  function paintTopics() {
    const n = ns();
    const cid = carId || 'c1';
    const noPlace = field({ label: 'Text when the car is at no saved place', name: 'no_place', path: 'no_place_text', value: cfg?.no_place_text ?? '', hint: 'Published as the place, up to 40 characters. Empty publishes an empty place.', attrs: { maxlength: '40', disabled: !cfg } });
    if (cfg) noPlace.input.addEventListener('change', async () => { const e = await save({ no_place_text: noPlace.input.value.trim() }, 'No-place text'); if (e?.fields) noPlace.setError(e.fields.no_place_text || null); });
    replace(topicsSlot,
      h('div', { class: 'si-tree', cover: 'mqtt.topic-scheme', role: 'list', 'aria-label': 'Topic layout' },
        treeRow(`${n}/status`, 'online or offline. Retained; the broker sets offline by itself if the server goes away (last will).'),
        treeRow(`${n}/cars/${cid}/<value>`, 'One topic per value (the table below). Retained, QoS 1, text in the server units.'),
        treeRow(`${n}/cars/${cid}/updated`, 'The time of the newest data behind the values (ISO, UTC).'),
        treeRow(`${n}/cars/${cid}/event`, 'One JSON message per event (Events below). Not retained, so a restart or a new subscriber never sees an old event as new.')),
      h('div', { class: 'si-rules' },
        h('div', { class: 'si-rule', cover: 'mqtt.note-car-id' }, h('h3', null, 'Car ids'),
          h('p', null, 'Each car has Sentry Mate’s own id (', code(cid), ' here), never reused. A new CAN Bridge key for the same car joins that car, so topics never move. Ids are listed in ', h('a', { class: 'link', href: buildUrl('settings-cars') }, 'Settings: Cars'), '.')),
        h('div', { class: 'si-rule', cover: 'mqtt.note-empty-values' }, h('h3', null, 'Missing values'),
          h('p', null, 'A value the car did not send is not published: the broker keeps the last one, and ', code('updated'), ' says how old it is. Speed and gear are held while parked; read ', code('state'), ' with them. Position topics are emptied when location publishing is turned off.')),
        h('div', { class: 'si-rule', cover: 'docs.env.core.default-geofence' }, h('h3', null, 'At no saved place'), noPlace.el),
        h('div', { class: 'si-rule' }, h('h3', null, 'Location'),
          cfg ? sw('si-loc', cfg.publish_location, 'Publish the position', async (on, input) => { if (await save({ publish_location: on }, on ? 'Location publishing on' : 'Location publishing off')) input.checked = cfg.publish_location; }).el
            : h('p', null, 'Location publishing is ', h('b', null, pv?.messages?.some((m) => m.topic.endsWith('/location') && m.payload) ? 'on' : 'off'), '.'),
          h('p', { class: 'setting-note' }, 'Off by default. On: latitude, longitude and a JSON position with its accuracy, for maps and arrival automations.'))));
  }
  const treeRow = (t, what) => h('div', { class: 'si-tree-row', role: 'listitem' }, code(t), h('span', null, what));
  const topicsPanel = panel({ title: 'MQTT topics and rules', icon: 'route', id: 'si-topics' }, topicsSlot);

  /* ---------- the car for the preview */
  const carSel = h('select', { class: 'select', 'aria-label': 'Car for the preview', onchange: () => pickCar(carSel.value) });
  const carSearch = h('input', { class: 'input si-car-search', type: 'search', placeholder: 'Find a car', 'aria-label': 'Find a car by name' });
  function fillCars(list) {
    const cur = extraCar && extraCar.id === carId ? extraCar : { id: carId, name: pv?.name || carId };
    const items = list.some((c) => c.id === carId) || !carId ? list : [cur, ...list];
    // sold cars last, in a "Sold cars" group, unless previewed now or searched for
    const { main, sold } = soldParts(items, [carId], { search: !!carSearch.value.trim() });
    const mk = (c) => h('option', { value: c.id, selected: c.id === carId }, c.name || c.id);
    replace(carSel, main.map(mk), soldOptions(sold, mk));
  }
  fillCars(carList);
  let searchGen = 0;
  carSearch.addEventListener('input', async () => {
    const my = ++searchGen;
    const q = carSearch.value.trim();
    try {
      const r = q ? await ctx.api.get('cars', { search: q, sort: 'name', limit: 100 }) : { items: carList };
      if (my === searchGen && ctx.alive()) fillCars(r.items || []);
    } catch { /* the list stays */ }
  });
  async function pickCar(next) {
    if (!next || next === carId) return;
    carId = next;
    extraCar = carList.find((c) => c.id === next) || { id: next, name: carSel.selectedOptions[0]?.textContent || next };
    rememberCar(next, { sold: isSold(extraCar) });
    replace(carNoteSlot);
    ctx.setQuery({ car: carId });
    await loadPreview();
    if (!ctx.alive()) return;
    paintTopics(); paintValues(); paintHa(); paintEntities(); paintEventTopic(); paintEvents();
    await eventsTable?.ready;
  }
  const carNoteSlot = h('div', { class: 'si-car-note' }, carMissing ? note(icon('info', 'ico--sm'), ` The car “${carMissing}” in the address does not exist or is not visible to this account, so the preview shows ${extraCar?.name || 'another car'}.`) : null);
  const carPicker = () => h('div', { class: 'si-car' }, h('label', { class: 'faint' }, 'Preview for'), carSel, carList.length >= 100 || (cars?.next) ? carSearch : null, carNoteSlot);

  /* ---------- MQTT published values */
  const valuesSlot = h('div', { class: 'si-values' });
  function valueRows() {
    const byTopic = new Map((pv?.messages || []).map((m) => [m.topic.split('/').pop(), m.payload]));
    const u = units();
    return VALUES.map((v) => {
      const shown = v.keys.map((k) => [k, byTopic.has(k) ? byTopic.get(k) : undefined]);
      return { ...v, shown, unit: v.unitKind ? unitText(v.unitKind, u) : '' };
    });
  }
  // whether the preview car has any input a row's value can come from (null: the preview does not say)
  const hasInput = (r) => (!r.inputs || !Array.isArray(pv?.inputs) ? null : pv.inputs.some((i) => r.inputs.includes(i.kind)));
  // where the value now published came from, for a value an API input gave: its label, and stale when it is
  function sourceLine(r) {
    const seen = new Map();
    for (const k of r.keys) {
      const src = pv?.sources?.[k];
      if (src) seen.set(`${src.label}|${src.stale}`, src);
    }
    if (!seen.size) return null;
    return h('span', { class: 'si-src' }, [...seen.values()].map((src) => h('span', { class: 'si-src-one', title: src.at_ms ? `Read ${ago(src.at_ms)}` : null },
      h('i', { class: `src-m src-m--${src.kind}`, 'aria-hidden': 'true' }), src.label, src.stale ? h('span', { class: 'si-stale' }, 'stale') : null)));
  }
  function payloadCell(r) {
    if (r.status === 'device') {
      const d = pv?.device;
      return h('span', { class: 'mono' }, r.cid === 'mqtt.model' ? d?.model || '—' : d?.name || pv?.name || '—');
    }
    if (!['published', 'api'].includes(r.status) || !r.keys.length) return h('span', { class: 'faint' }, '—');
    const any = r.shown.some(([, p]) => p !== undefined);
    if (!any) return h('span', { class: 'faint' }, r.status === 'api' && hasInput(r) === false ? 'No input gives this now' : 'Not sent by this car yet');
    return h('span', { class: 'si-payloads' }, r.shown.map(([k, p]) => h('span', { class: 'mono' }, r.shown.length > 1 ? h('span', { class: 'faint' }, `${k.replace(/^(window|route)_/, '')} `) : null,
      p === undefined ? '—' : p === '' ? h('span', { class: 'faint' }, '(empty)') : p.length > 60 ? `${p.slice(0, 57)}…` : p)), sourceLine(r));
  }
  const srcCsv = (r) => [...new Set(r.keys.map((k) => pv?.sources?.[k]).filter(Boolean).map((x) => `${x.label}${x.stale ? ' (stale)' : ''}`))].join('; ');
  let valuesTable = null;
  function paintValues() {
    const rows = valueRows();
    const counts = Object.fromEntries(STATUS_ORDER.map((s) => [s, rows.filter((r) => r.status === s).length]));
    if (!pv || pv.error || !carId) {
      replace(valuesSlot, emptyState({ icon: 'car', title: carId ? 'The preview could not be made' : 'No car to preview', text: carId ? errText(pv?.error) : 'A car appears once its CAN Bridge uploads its first log. The list below still says what each value is.' }));
    } else replace(valuesSlot);
    valuesTable = dataTable(ctx, {
      title: 'Every value', icon: 'signals', rows, csvName: `mqtt-values-${carId || 'none'}`, label: 'Published values',
      side: h('span', { class: 'faint' }, `${counts.published} published · ${counts.api} from Tesla API / TeslaFi · ${counts.logged} logged · ${counts.signal} need a signal · ${counts.none + counts.never} left out`),
      note: `Payloads are what a broker gets now for ${pv?.name || 'this car'}, as text in the server units (${unitText('distance', units())}, ${unitText('temperature', units())}, ${unitText('pressure', units())}): MQTT has one set of units for everyone. Sort by Status to group them.`,
      sort: { key: 'status', dir: 'asc' },
      columns: [
        { key: 'what', label: 'Value', sort: true, render: (r) => h('span', { class: 'si-what', cover: r.cid || null }, h('b', null, r.what), r.cid ? null : h('span', { class: 'faint' }, ' extra')), csv: (r) => r.what },
        { key: 'topic', label: 'Topic', value: (r) => r.keys[0] || '', sort: true, render: (r) => (r.keys.length ? h('span', { class: 'si-topics-cell' }, r.keys.map((k) => codeLong(k))) : h('span', { class: 'faint' }, 'no topic')), csv: (r) => r.keys.map((k) => `${ns()}/cars/${carId}/${k}`).join(' ') },
        { key: 'payload', label: 'Payload now', render: payloadCell, csv: (r) => (['published', 'api'].includes(r.status) ? [r.shown.map(([, p]) => p ?? '').join(' '), srcCsv(r) ? `(${srcCsv(r)})` : ''].filter(Boolean).join(' ') : r.status === 'device' ? (r.cid === 'mqtt.model' ? pv?.device?.model : pv?.name) || '' : '') },
        { key: 'unit', label: 'Unit', cls: 'nowrap', render: (r) => r.unit || h('span', { class: 'faint' }, 'text'), csv: (r) => r.unit },
        { key: 'status', label: 'Status', sort: true, value: (r) => STATUS_ORDER.indexOf(r.status), render: (r) => statusTag(r.status, r.inputs || undefined), csv: (r) => statusWord(r.status, r.inputs || undefined) },
        { key: 'source', label: 'Comes from', sort: true, cls: 'si-wrap', render: (r) => h('span', null, r.source, r.why ? h('span', { class: 'si-why' }, r.why) : null), csv: (r) => `${r.source}${r.why ? `. ${r.why}` : ''}` },
      ],
    });
    valuesSlot.append(valuesTable.el);
  }
  const valuesPanel = h('section', { class: 'si-section', id: 'si-values', 'aria-label': 'MQTT published values' },
    h('div', { class: 'si-section-head' }, h('h2', { class: 'si-h2' }, icon('signals'), 'MQTT published values'), carPicker(),
      h('span', { class: 'grow' }), h('a', { class: 'panel-link', href: carId ? buildUrl('car', { car: carId }) : '#', hidden: !carId, onclick: (e) => { e.currentTarget.href = buildUrl('car', { car: carId }); } }, 'Car overview', icon('chev-r', 'ico--sm'))),
    h('p', { class: 'note' }, 'Every value Sentry Mate publishes, before you turn anything on. The VIN is never one of them; the position only when you opt in. What each status means:'),
    h('dl', { class: 'si-legend' }, STATUS_ORDER.map((k) => h('div', null, h('dt', null, statusTag(k)), h('dd', null, STATUS[k][2])))),
    valuesSlot);

  /* ---------- Home Assistant discovery */
  const haSlot = h('div');
  function paintHa() {
    const ha = cfg?.ha_discovery || { enabled: (pv?.ha_entities || []).length > 0, prefix: haPrefixOf(pv?.ha_entities?.[0]?.config_topic) || 'homeassistant', url: null };
    const ro = !cfg;
    const onSw = sw('si-ha-on', ha.enabled, 'Home Assistant discovery', async (on, input) => {
      if (await save({ ha_discovery: { enabled: on } }, on ? 'Discovery on' : 'Discovery off')) input.checked = cfg.ha_discovery.enabled;
    }, ro);
    const prefixF = field({ label: 'Discovery prefix', name: 'ha_prefix', path: 'ha_discovery.prefix', value: ha.prefix, hint: 'Must match Home Assistant’s own MQTT discovery prefix; homeassistant unless you changed it there.', attrs: { disabled: ro, spellcheck: 'false', autocomplete: 'off' } });
    const urlF = field({ label: 'Link on the device page (https)', name: 'ha_url', path: 'ha_discovery.url', value: ha.url || '', hint: 'This server’s address as Home Assistant users reach it. Each car’s device then links to its Car overview. Empty: no link.', attrs: { disabled: ro, placeholder: `https://${location.host}`, spellcheck: 'false', autocomplete: 'off' } });
    if (!ro) {
      prefixF.input.addEventListener('change', async () => {
        const v = prefixF.input.value.trim();
        if (!v || /[+#]/.test(v) || v.startsWith('$') || v.split('/').some((p) => !p)) { prefixF.setError('No + or #, not starting with $, and no empty level.'); return; }
        const e = await save({ ha_discovery: { prefix: v } }, 'Discovery prefix');
        if (e?.fields) prefixF.setError(e.fields['ha_discovery.prefix'] || null);
      });
      urlF.input.addEventListener('change', async () => {
        const v = urlF.input.value.trim();
        if (v && !/^https:\/\/[^\s/]+(\/\S*)?$/.test(v)) { urlF.setError('An https:// address, or empty.'); return; }
        const e = await save({ ha_discovery: { url: v || null } }, 'Device link');
        if (e?.fields) urlF.setError(e.fields['ha_discovery.url'] || null);
      });
    }
    const d = pv?.device;
    const ents = pv?.ha_entities || [];
    const node = d?.identifiers?.[0] || `${ns().replace(/[^A-Za-z0-9_-]/g, '_')}_${carId || 'c1'}`;
    const publishedKeys = new Set((pv?.messages || []).filter((m) => m.payload !== '').map((m) => m.topic.split('/').pop()).filter((k) => k !== 'status' && !['latitude', 'longitude'].includes(k)));
    const withEntity = new Set(ents.map((e) => e.state_topic.split('/').pop()));
    const missing = [...publishedKeys].filter((k) => !withEntity.has(k));
    replace(haSlot,
      h('div', { class: 'si-two' },
        h('div', null,
          setting('Discovery', 'On: each car appears in Home Assistant by itself, with every sensor, and no YAML to write. Needs publishing on.', [onSw.el, ro ? h('p', { class: 'setting-note' }, 'An admin changes this.') : null], 'ha.discovery.enable'),
          setting('Prefix', 'Where the discovery messages go.', prefixF.el, 'ha.discovery.prefix'),
          setting('Device link', 'Optional.', urlF.el, 'ha.discovery.url'),
          h('div', { class: 'si-rules si-rules--one' },
            h('div', { class: 'si-rule', cover: 'ha.discovery.removed-car-cleanup' }, h('h3', null, 'A car deleted or renamed'), h('p', null, 'Deleting a car (or taking it off publishing) removes its device and every entity from Home Assistant at once. A rename re-sends the device at once with the new name; entity ids stay put.')),
            h('div', { class: 'si-rule', cover: 'ha.discovery.disabled-cleanup' }, h('h3', null, 'Discovery turned off'), h('p', null, 'Every car’s device and entities are removed from Home Assistant the moment you save, not at the next restart. The plain topics stay.')),
            h('div', { class: 'si-rule', cover: ['ha.discovery.cleanup-policy', 'ha.discovery.limits-note'] }, h('h3', null, 'How clean-up works'), h('p', null, 'Nothing is remembered: on every such change, and again at each start, removals are sent for whatever should not be there. A clean-up cut short is simply sent again. A changed prefix sends removals under the old prefix first, so nothing is left behind there.')))),
        h('div', null,
          h('div', { class: 'si-device', cover: ['ha.discovery.device-payload', 'ha.device.identity'] },
            h('h3', null, 'The device Home Assistant gets'),
            kv([['Name', d?.name || pv?.name || '—', { icon: 'car' }], ['Maker', 'Tesla', { icon: 'info' }], ['Model', d?.model || '—', { icon: 'chip' }], ['Identifier', code(node), { icon: 'pin' }],
              ['Link', ha.url ? code(`${ha.url.replace(/\/$/, '')}/cars/${carId}`) : h('span', { class: 'faint' }, 'none'), { icon: 'ext' }], ['Entities', `${ents.length}`, { icon: 'signals' }]]),
            h('p', { class: 'setting-note' }, 'One device per car, holding all of its entities. Two servers with different topic prefixes on one broker never share a device.')),
          h('div', { class: 'si-rule', cover: 'ha.entity.id-scheme' }, h('h3', null, 'Entity ids'),
            h('p', null, 'Each entity’s hidden unique id is the identifier and the value: ', code(`${node}_soc`), '. Home Assistant names the visible id after the device: ', code(`sensor.${slug(d?.name || pv?.name || 'car')}_battery`), '. Several cars never clash, and ids stay the same across versions.')),
          h('div', { class: ['si-rule', missing.length ? 'si-rule--warn' : null], cover: 'ha.discovery.coverage-gap' }, h('h3', null, 'An entity for every value'),
            h('p', null, ents.length ? (missing.length ? `${withEntity.size} of the ${publishedKeys.size} values published for this car have an entity; missing: ${missing.join(', ')}.` : `All ${publishedKeys.size} values published for this car have an entity (position values through the device tracker).`) : 'Discovery is off, so Home Assistant gets no entities; the topics are still published.'),
            h('p', { class: 'setting-note' }, 'The rule: every value topic gets an entity. A value is only missing in Home Assistant when it is missing from MQTT too.')))));
  }
  const haPanel = panel({ title: 'Home Assistant discovery', icon: 'home', id: 'si-ha' }, haSlot);

  /* ---------- Home Assistant entities */
  const entSlot = h('div', { class: 'si-ents' });
  function paintEntities() {
    const ents = pv?.ha_entities || [];
    const groups = dataTable(ctx, {
      title: 'What Home Assistant gets', icon: 'home', rows: ENTITY_GROUPS, csvName: 'home-assistant-entity-groups', label: 'Home Assistant entity groups',
      note: 'Read-only entities, one per published value. A group needing a new CAN signal shows up as soon as Sentry Mate decodes it.',
      columns: [
        { key: 'what', label: 'Group', sort: true, render: (r) => h('span', { cover: r.cid }, h('b', null, r.what)) },
        { key: 'gives', label: 'Entities', cls: 'si-wrap' },
        { key: 'topics', label: 'From topics', cls: 'si-wrap', render: (r) => (r.topics ? h('span', { class: 'mono' }, r.topics) : h('span', { class: 'faint' }, '—')) },
        { key: 'status', label: 'Status', sort: true, value: (r) => STATUS_ORDER.indexOf(r.status), render: (r) => statusTag(r.status, r.inputs || undefined), csv: (r) => statusWord(r.status, r.inputs || undefined) },
        { key: 'why', label: 'Notes', cls: 'si-wrap' },
      ],
    });
    const list = dataTable(ctx, {
      title: `Entities for ${pv?.name || 'this car'}`, icon: 'signals', rows: ents, csvName: `home-assistant-entities-${carId || 'none'}`, label: 'Home Assistant entities',
      empty: () => emptyState({ icon: 'home', title: 'No entities', text: cfg && !cfg.ha_discovery.enabled ? 'Discovery is off. Turn it on above and every entity listed here goes to Home Assistant.' : 'There is no car to preview yet.' }),
      note: 'Exactly what discovery sends now: units are the server units, named to Home Assistant so it never mixes them up.',
      sort: { key: 'name', dir: 'asc' },
      columns: [
        { key: 'name', label: 'Name', sort: true, render: (r) => h('b', null, r.name) },
        { key: 'id', label: 'Unique id', sort: true, render: (r) => codeLong(r.id) },
        { key: 'unit', label: 'Unit', render: (r) => r.unit || h('span', { class: 'faint' }, '—') },
        { key: 'device_class', label: 'Kind', sort: true, render: (r) => r.device_class || h('span', { class: 'faint' }, '—') },
        { key: 'state_topic', label: 'State topic', render: (r) => codeLong(r.state_topic) },
        { key: 'config_topic', label: 'Discovery topic', render: (r) => codeLong(r.config_topic) },
      ],
    });
    replace(entSlot, groups.el, list.el, cards(ents));
  }
  function cards(ents) {
    const name = slug(pv?.name || 'car');
    const eid = (key, domain = 'sensor') => {
      const e = ents.find((x) => x.state_topic.endsWith(`/${key}`));
      return e ? `${haDomainOf(e.config_topic) || domain}.${name}_${slug(e.name)}` : null;
    };
    const glance = ['soc', 'state', 'charging', 'plugged_in', 'gear', 'speed', 'range', 'sentry_mode', 'doors_open', 'inside_temp', 'updated'].map((k) => eid(k)).filter(Boolean);
    const all = ents.filter((e) => !['event', 'device_tracker'].includes(haDomainOf(e.config_topic))).map((e) => `${haDomainOf(e.config_topic) || 'sensor'}.${name}_${slug(e.name)}`);
    const tracker = ents.find((e) => haDomainOf(e.config_topic) === 'device_tracker');
    const yGlance = ['type: glance', `title: ${pv?.name || 'Car'}`, 'entities:', ...glance.map((x) => `  - ${x}`)].join('\n');
    const yMap = tracker ? ['type: map', 'dark_mode: true', 'hours_to_show: 24', 'entities:', `  - device_tracker.${name}_location`].join('\n') : '';
    const yList = ['type: entities', `title: ${pv?.name || 'Car'}`, 'entities:', ...all.map((x) => `  - ${x}`), ...(tracker ? ['  - entity: proximity.home', '    name: Distance to home'] : [])].join('\n');
    const card = (titleText, text, yaml, cid, missingText) => h('div', { class: 'si-card', cover: cid },
      h('div', { class: 'si-card-head' }, h('h3', null, titleText), yaml ? copyButton(() => yaml) : null),
      h('p', { class: 'setting-note' }, text),
      yaml ? h('pre', { class: 'si-pre' }, yaml) : h('p', { class: 'si-pre si-pre--empty' }, missingText));
    return h('div', { class: 'si-cards-wrap' },
      h('h3', { class: 'si-h3' }, 'Dashboard cards to paste'),
      h('p', { class: 'note' }, 'Made from the entities above, for a Home Assistant dashboard (Edit dashboard, Add card, Manual). The visible ids follow the car’s name; if you renamed an entity in Home Assistant, change it here too.'),
      h('div', { class: 'si-cards' },
        card('At a glance', 'The facts checked most, including plugged in, Sentry mode and doors; windows and lock are not on CAN.', ents.length ? yGlance : '', 'ha.lovelace.glance', 'Turn discovery on to get this card.'),
        card('Map', 'Where the car is, on Home Assistant’s map.', yMap, 'ha.lovelace.map', 'Needs location publishing on (Topics and rules above).'),
        card('Everything', 'One list of every entity, and the distance to home when the position is published.', ents.length ? yList : '', ['ha.lovelace.entity-list'], 'Turn discovery on to get this card.')),
      h('p', { class: 'note', cover: 'ha.proximity' }, icon('info', 'ico--sm'), ' Distance to home and the direction of travel come from Home Assistant’s own Proximity helper (Settings, Devices and services, Helpers), with the car’s device tracker as the tracked device. It needs location publishing on, and knows of an arrival about 75 s late.'));
  }
  const entitiesPanel = h('section', { class: 'si-section', 'aria-label': 'Home Assistant entities' },
    h('div', { class: 'si-section-head' }, h('h2', { class: 'si-h2' }, icon('home'), 'Home Assistant entities')), entSlot);

  /* ---------- Events for automations */
  const eventsSlot = h('div', { class: 'si-events-list' });
  let eventsTable = null;
  function paintEvents() {
    if (!carId) { eventsTable = null; replace(eventsSlot, emptyState({ icon: 'clock', title: 'No car yet', text: 'Events appear here once a car has uploaded logs.' })); return; }
    eventsTable = dataTable(ctx, {
      title: 'Recent events', icon: 'clock', path: `cars/${enc(carId)}/events`, params: eventParams(), csvName: `events-${carId}`, label: 'Recent events', limit: 12,
      note: 'The last 30 days, newest first, exactly as a subscriber to the event topic got them (positions only with location publishing on).',
      empty: () => emptyState({ icon: 'clock', title: 'No events in the last 30 days', text: 'This car did not drive, charge or change software in that time.' }),
      columns: [
        { key: 'at_ms', label: 'When', render: (r) => h('span', { class: 'nowrap' }, dateTime(r.at_ms, ctx.tz)) },
        { key: 'type', label: 'Event', render: (r) => code(r.type) },
        { key: 'payload', label: 'Payload', cls: 'si-wrap', render: (r) => payloadFold(r.payload), csv: (r) => JSON.stringify(r.payload ?? {}) },
      ],
    });
    replace(eventsSlot, eventsTable.el);
  }
  const eventParams = () => { const now = ctx.api.now(); return { from_ms: now - 30 * DAY, to_ms: now + 60_000, types: EVENT_TYPES.join(',') }; };
  const payloadFold = (p) => {
    const parts = Object.entries(p || {}).filter(([k]) => !['event_type', 'car', 'units'].includes(k))
      .map(([k, v]) => `${k}: ${v == null ? '—' : typeof v === 'object' ? JSON.stringify(v) : v}`);
    if (parts.length <= 4) return h('span', { class: 'mono si-json' }, parts.join(' · '));
    return h('details', { class: 'si-fold' }, h('summary', { class: 'mono si-json' }, parts.slice(0, 4).join(' · '), h('span', { class: 'faint' }, ` +${parts.length - 4} more`)),
      h('span', { class: 'mono si-json' }, parts.slice(4).join(' · ')));
  };

  const recipe = (titleText, cid, uses, text, s) => h('div', { class: 'si-card', cover: cid },
    h('div', { class: 'si-card-head' }, h('h3', null, titleText), statusTag(s)),
    h('p', null, text), h('p', { class: 'setting-note' }, 'Uses ', uses.map((u, i) => [i ? ', ' : '', code(u)]), '.'));
  const eventTopicSlot = h('span');
  const paintEventTopic = () => replace(eventTopicSlot, code(`${ns()}/cars/${carId || 'c1'}/event`));
  paintEventTopic();
  function eventsBody() {
    return [
      h('div', { class: 'si-two' },
        h('div', { cover: 'nr.overview' },
          h('p', null, 'Automations should react to what happened, not to a value changing. Sentry Mate sends one message per event on ', eventTopicSlot, '. It is never retained, so a restart of the server, the broker or Home Assistant never fires an automation again. Home Assistant gets them as one event entity per car.'),
          h('p', { class: 'setting-note' }, 'Any tool that reads MQTT can use them: Home Assistant, Node-RED, a script. Sentry Mate only publishes; it never takes commands, and nothing here can control the car.'),
          h('div', { class: 'tbl-scroll' }, h('table', { class: 'tbl si-evt-types', 'aria-label': 'Event types' },
            h('thead', null, h('tr', null, h('th', { scope: 'col' }, 'Event'), h('th', { scope: 'col' }, 'When'), h('th', { scope: 'col' }, 'Carries'))),
            h('tbody', null, EVENTS.map(([t, w, c2]) => h('tr', null, h('td', null, code(t)), h('td', null, w), h('td', { class: 'si-wrap' }, c2))))))),
        eventsSlot),
      h('h3', { class: 'si-h3' }, 'Ideas that work with these'),
      h('p', { class: 'note' }, icon('clock', 'ico--sm'), ' Values reach Sentry Mate about 75 s after the car sent them (one-minute logs and the upload). Fine for reminders and logging, too late to open a door as the car pulls up.'),
      h('div', { class: 'si-cards' },
        recipe('Garage door', 'ha.automation.garage', ['drive_ended', 'end_place'], 'Close the garage door, or log the arrival, when a drive ends at your home place. Opening it on arrival is better left to the car’s own HomeLink: values here are about 75 s late.', 'published'),
        recipe('Left open', 'ha.automation.left-open', ['doors_open', 'trunk_open', 'frunk_open'], 'Remind after five minutes with a door, the trunk or the frunk open, and every five minutes after. The 75 s delay does not matter at that pace. Windows are not on CAN.', 'published'),
        recipe('Stop the reminder', 'ha.automation.left-open-cancel', ['doors_open', 'trunk_open', 'frunk_open'], 'End the repeat as soon as everything reads closed again.', 'published'),
        recipe('Plug in tonight', 'ha.automation.plug-in-reminder', ['plugged_in', 'soc', 'range'], 'An evening reminder when the car is home and not plugged in, with its battery and range.', 'published'),
        recipe('State changed', 'nr.notify.state-change', ['drive_started', 'drive_ended', 'state'], 'A message when a drive starts or ends; drive_ended carries the whole drive, so the message can say how far and how efficient.', 'published'),
        recipe('Someone got in', 'nr.notify.driver-present', ['user_present'], 'A message when someone is seen in the car. The event and the user_present topic both say so.', 'published'),
        recipe('Time to full', 'nr.notify.time-to-full', ['time_to_full', 'charge_power', 'soc'], 'A progress message while charging.', 'published'),
        recipe('Arrived, left', 'nr.notify.geofence', ['place', 'start_place', 'end_place'], 'A message when the car leaves one saved place or arrives at another: the place topic changes, and drive events name both ends.', 'published')),
    ];
  }
  const eventsPanel = panel({ title: 'Events for automations', icon: 'pulse', id: 'si-events' });

  /* ---------- API access */
  const apiPanel = panel({ title: 'API access', icon: 'server', id: 'si-api' },
    h('div', { class: 'si-rules' },
      h('div', { class: 'si-rule', cover: 'faq.public-api' }, h('h3', null, 'What you can build on'),
        h('p', null, 'Two things are supported and kept stable: the MQTT topics on this page, and the read API under ', code(`${location.origin}/api/v1/`), '. Both interface packs of this app are built on that same API. The storage behind them is sealed and private, so never read it directly.')),
      h('div', { class: 'si-rule', cover: 'projects.teslamateapi' }, h('h3', null, 'A JSON API, built in'),
        h('p', null, 'Cars, live values, drives, charges, positions, places and every report, as JSON or CSV, versioned. For example ', code('GET /api/v1/cars'), ' lists the cars you may see with their live state, and ', code('GET /api/v1/cars/<id>/drives'), ' their drives.')),
      h('div', { class: 'si-rule' }, h('h3', null, 'Who sees what'),
        h('p', null, 'The API signs in like this app and answers for that account only: a watch account sees just its own cars, and a hidden car looks the same as no car. Make an account per app or person in ', h('a', { class: 'link', href: buildUrl('settings-server') }, 'Server and access'), '.')),
      h('div', { class: 'si-rule', cover: 'projects.custom-dashboards' }, h('h3', null, 'More analysis, already here'),
        h('p', null, 'Costs, battery health and range over time, charging by place and kind, and every gap in the record are pages of this app: ', h('a', { class: 'link', href: buildUrl('statistics') }, 'Statistics'), ', ', h('a', { class: 'link', href: carId ? buildUrl('battery', { car: carId }) : buildUrl('fleet') }, 'Battery'), ', ', h('a', { class: 'link', href: buildUrl('charges') }, 'Charges'), '. Each table and chart downloads as CSV for your own.')),
      h('div', { class: 'si-rule', cover: 'projects.matedroid' }, h('h3', null, 'On a phone'),
        h('p', null, 'This app works in a phone’s browser and can be installed to the home screen. A phone app of your own can use the same API.'))));

  /* ---------- Network use */
  const netSlot = h('div');
  const PURPOSE = {
    geocoding: ['Address lookup', 'Only when an admin turns online addresses on; positions are rounded first.', 'settings-display'],
    tiles: ['Map pictures', 'Asked by each viewer’s browser when online tiles are on: the chosen map (satellite by default), its place names and roads, and the maps people may switch to.', 'settings-display'],
    geonames_download: ['Place-name download', 'Only when an admin asks for new offline place names.', 'settings-display'],
    mqtt: ['MQTT broker', 'Only while publishing is on (this page).', null],
    tesla: ['Tesla API', 'Only while a Tesla account is added and not paused: sign-in, the Owner API and its live stream (Tesla account, this page).', null],
    teslafi: ['TeslaFi API', 'Only while a TeslaFi token is added and not paused: reads your cars\' data from TeslaFi', null, 'teslafi', 'TeslaFi'],
    teslamate_mqtt: ['Local TeslaMate (MQTT)', 'Only while a local TeslaMate is added and not paused: subscribes to its topics, never publishes', null, 'teslamate', 'Local TeslaMate'],
    teslamate_db: ['Local TeslaMate (database)', 'Only while its database catch-up is on: reads what MQTT missed', null, 'teslamate', 'Local TeslaMate'],
    release_check: ['Release check', 'Asks whether a newer Sentry Mate exists, when turned on.', 'settings-display'],
    upload: ['CAN Bridge uploads', 'The car’s CAN Bridge sends its logs here.', null],
    web: ['This app and the API', 'Browsers, apps and the API.', null],
  };
  // every host of a row: the map pictures name each server (the chosen map, its names and roads, the maps people may switch to)
  const hostsOf = (r) => (Array.isArray(r.hosts) && r.hosts.length ? r.hosts : r.host ? [r.host] : []);
  function paintNetwork() {
    if (!net) { replace(netSlot, note(icon('lock', 'ico--sm'), admin ? ' The connection list could not be read.' : ' The list of connections is shown to admins. By default Sentry Mate makes no outbound connection at all.')); return; }
    const rows = [
      ...net.outbound.map((o) => ({ dir: 'Out', ...o, port: null, tls: null })),
      ...net.inbound.map((i) => ({ dir: 'In', purpose: i.purpose, host: null, enabled: true, last_used_ms: null, port: i.port, tls: i.tls })),
    ];
    const t = dataTable(ctx, {
      title: 'Every connection', icon: 'globe', rows, csvName: 'network-connections', label: 'Network connections',
      note: `For a firewall: ${net.outbound.filter((o) => o.enabled).length} of ${net.outbound.length} outbound connections are on now; everything off makes no request. Inbound, only the ports below are open.`,
      columns: [
        { key: 'dir', label: 'Way', sort: true, render: (r) => tag(r.dir === 'In' ? 'Inbound' : 'Outbound', r.dir === 'In' ? 'live' : 'plain'), csv: (r) => r.dir },
        { key: 'purpose', label: 'For', sort: true, render: (r) => h('span', null, h('b', null, PURPOSE[r.purpose]?.[0] || r.purpose), h('span', { class: 'si-why' }, PURPOSE[r.purpose]?.[1] || '')), csv: (r) => PURPOSE[r.purpose]?.[0] || r.purpose },
        { key: 'host', label: 'Host or port', render: (r) => (r.port != null ? code(`port ${r.port}${r.tls ? ', TLS' : ''}`) : hostsOf(r).length ? h('span', { class: 'si-hosts' }, hostsOf(r).map((x) => code(x))) : h('span', { class: 'faint' }, 'not set')), csv: (r) => hostsOf(r).join(' ') || (r.port != null ? `port ${r.port}` : '') },
        { key: 'enabled', label: 'State', sort: true, render: (r) => (r.enabled ? pill(r.dir === 'In' ? 'Open' : 'On', { tone: 'live' }) : pill('Off')), csv: (r) => (r.enabled ? 'on' : 'off') },
        { key: 'last_used_ms', label: 'Last used', num: true, sort: true, render: (r) => (r.last_used_ms ? h('span', { class: 'nowrap', title: dateTime(r.last_used_ms, ctx.tz) }, ago(r.last_used_ms)) : h('span', { class: 'faint' }, r.dir === 'In' ? 'always listening' : 'never')), csv: (r) => (r.last_used_ms ? new Date(r.last_used_ms).toISOString() : '') },
        { key: 'change', label: 'Change it', render: (r) => (PURPOSE[r.purpose]?.[3] ? h('a', { class: 'link', href: `${buildUrl('settings-integrations')}#${PURPOSE[r.purpose][3]}` }, PURPOSE[r.purpose][4]) : PURPOSE[r.purpose]?.[2] ? h('a', { class: 'link', href: buildUrl(PURPOSE[r.purpose][2]) }, 'Settings') : h('span', { class: 'faint' }, r.purpose === 'mqtt' || r.purpose === 'tesla' ? 'Above' : '—')), csv: false },
      ],
    });
    replace(netSlot, t.el);
  }
  const netPanel = h('section', { class: 'si-section', id: 'si-network', 'aria-label': 'Network use', cover: 'faq.network-flows' },
    h('div', { class: 'si-section-head' }, h('h2', { class: 'si-h2' }, icon('globe'), 'Network use')),
    h('p', { class: 'note' }, 'Sentry Mate needs no Tesla account and no cloud: the car’s CAN Bridge (SMO) uploads to it, and everything else is off until you turn it on. A Tesla account (above) is an optional second input; without one the server never connects to Tesla.'), netSlot);

  /* ---------- Works with */
  const works = (titleText, cid, text, uses) => h('div', { class: 'si-card', cover: cid }, h('h3', null, titleText), h('p', null, text), h('p', { class: 'setting-note' }, 'Uses ', uses));
  const worksPanel = panel({ title: 'Works with', icon: 'share', id: 'si-works' },
    h('p', { class: 'note', cover: 'projects.disclaimer' }, icon('info', 'ico--sm'), ' Other people’s tools, named so you know what fits. Sentry Mate does not make, check or vouch for them. Anything built on the MQTT topics or the API keeps working; anything that reads the storage directly will break.'),
    h('div', { class: 'si-cards' },
      works('Route planners', 'projects.abrp', 'A planner that takes the live battery level plans with the real charge. Sentry Mate never sends to a planner by itself: a small MQTT bridge of yours forwards soc, position, speed and power, if you want that.', [code('soc'), ', ', code('location'), ', ', code('speed'), ', ', code('power'), '; about 75 s behind live.']),
      works('Home Assistant Tesla add-ons', 'projects.ha-tesla', 'Integrations that can read a car’s values from MQTT instead of asking Tesla’s servers can use these topics directly.', [code(`${ns()}/cars/<id>/#`), '.']),
      works('Desk gauges and lights', 'projects.gaussmeter', 'A light or model on the desk whose colour follows the state and whose level follows the battery.', [code('state'), ' and ', code('soc'), '.']),
      works('Smart mirrors and wall screens', 'projects.magic-mirror', 'A status tile on a mirror or wall display, from MQTT or the API.', [code('soc'), ', ', code('range'), ', ', code('state'), ', ', code('updated'), '.']),
      works('Garage door openers', 'projects.geogdo', 'Openers that act on arrival and departure, for several cars, by zone or saved place. Values are about 75 s late: good for closing behind you, late for opening.', [code('location'), ', ', code('place'), ' and drive events.'])));

  /* ---------- put it together */
  replace(ctx.view,
    pageHead({ title: ctx.info.title, purpose: ctx.info.purpose, actions: headSlot }),
    jumpNav,
    h('div', { class: 'si-stack' },
      h('p', { class: 'note saving-note' }, icon('check', 'ico--sm'), ' Switches and single fields save at once and a short note confirms it; the broker connection saves with its button. Nothing leaves the server until publishing is on.'),
      tesla.el, teslafi.el, teslamate.el, brokerPanel, topicsPanel, valuesPanel, haPanel, entitiesPanel, eventsPanel, apiPanel, netPanel, worksPanel));
  paintHead(); paintBroker(); paintStatus(); paintTopics(); paintValues(); paintHa(); paintEntities();
  eventsPanel.append(...eventsBody());
  paintEvents(); paintNetwork();
  await Promise.all([valuesTable?.ready, eventsTable?.ready, paintLog(), tesla.load(), teslafi.load(), teslamate.load()].filter(Boolean));
  if (!ctx.alive()) return;
  ctx.ready();
}

// a discovery topic is <prefix>/<domain>/<node>/<key>/config, and the prefix may have several levels
function haDomainOf(topic) { const p = String(topic || '').split('/'); return p.length >= 5 ? p[p.length - 4] : null; }
function haPrefixOf(topic) { const p = String(topic || '').split('/'); return p.length >= 5 ? p.slice(0, -4).join('/') : null; }

function slug(s) {
  return String(s || '').toLowerCase().normalize('NFKD').replace(/[̀-ͯ]/g, '').replace(/[^a-z0-9]+/g, '_').replace(/^_+|_+$/g, '') || 'car';
}
