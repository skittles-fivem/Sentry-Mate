// settings/integrations: the Tesla account (a second live input beside SMO, pages/tesla.js), TeslaFi (a third, pages/teslafi.js), and sending the cars'
// live values to other systems, if the owner wants to. One block per integration, each with a status tag: MQTT (the broker as 'key = value' lines, a test, live counts), its
// topics and rules, every value it publishes (previewed for a chosen car), Home Assistant discovery and the
// entities it gets, events for automations, the read API, every network connection, and tools that use them.
// Broker settings, the network list and the broker log are for admins; anyone reads the status, the preview
// and the events of their cars. The connection lines (host, port, TLS, pin, password, prefix) are saved
// together with Enter, because the server checks them together; single switches save at once.
// API: GET/PUT settings/mqtt, GET settings/mqtt/status, settings/mqtt/preview?car=, POST settings/mqtt/test,
// GET network, cars/{car}/events, audit.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { head, paint, section, note, link, btn, kv } from '../core/ui.js';
import { carKeys as soldAwareKeys } from '../core/carpick.js';
import { table } from '../core/table.js';
import { editLines, editGroup } from '../core/kvedit.js';
import { empty, errorWords } from '../core/states.js';
import { tag, dateTime, ago, DASH } from '../core/fmt.js';
import { formatNumber } from '../shared/i18n.js';
import { copyBtn } from '../core/secret.js';
import { toast } from '../core/toast.js';
import { src } from '../core/source.js';
import { teslaSection } from './tesla.js';
import { teslafiSection } from './teslafi.js';
import { teslamateSection } from './teslamate.js';

export const id = 'settings-integrations';
export const title = () => 'Settings: integrations';

const enc = encodeURIComponent;
const DAY = 86_400_000;

/* ---------------------------------------------------------------------- what is published, and why */

// status of a value: what a consumer can count on
const STATUS = {
  published: ['published', 'ok', 'Sent now, retained, in the server units.'],
  api: [null, 'accent', 'No CAN message carries it; the Tesla API or TeslaFi\u2019s polls give it, tagged with where it came from, and it goes stale (sent empty) when the input stops.'],
  device: ['on the device', 'accent', 'Sent once as part of the Home Assistant device, not as a topic.'],
  logged: ['logged', 'muted', 'Sentry Mate has the value (from the car or worked out); it is not its own topic yet.'],
  signal: ['needs a signal', 'warn', 'The car sends it; Sentry Mate does not decode it yet. Nothing is guessed meanwhile.'],
  none: ['not on can', 'muted', 'No message on the car\u2019s buses carries it, so it is never published.'],
  never: ['never', 'muted', 'Left out on purpose.'],
};
const STATUS_ORDER = ['published', 'api', 'device', 'logged', 'signal', 'none', 'never'];

// the inputs other than SMO that can give a value; an 'api' row names them in its status word
const API_INPUTS = ['tesla_api', 'teslafi_api'];
const INPUT_NAME = { smo: 'SMO', tesla_api: 'Tesla API', teslafi_api: 'TeslaFi' };
const apiWord = (inputs = API_INPUTS) => `from ${inputs.map((k) => INPUT_NAME[k] || k).join(' / ')}`.toLowerCase();

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
  ['mqtt.version', 'version', 'Software version', null, 'api', 'Tesla API car_version, TeslaFi\u2019s poll', 'No version text on CAN. The first word only, such as 2026.32.4; the software_changed event still says when module builds change.'],
  ['mqtt.update-available', 'update_available', 'Update waiting', 'true / false', 'api', 'Tesla API software_update.status, TeslaFi newVersionStatus', 'Only Tesla\u2019s servers know; the APIs pass it on.'],
  ['mqtt.update-version', 'update_version', 'Waiting update version', null, 'api', 'Tesla API software_update.version, TeslaFi newVersion', 'Not published while no update waits.'],
  ['mqtt.download-perc', 'update_download', 'Update download %', '%', 'api', 'Tesla API software_update.download_perc', 'No progress on CAN.'],
  ['mqtt.install-perc', 'installing', 'Update install %', 'true / false', 'published', 'Update status (0x343), else the Tesla API / TeslaFi', 'No percentage: an installing true/false flag instead. CAN first; the APIs\u2019 word only while CAN has none.'],
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
  ['mqtt.locked', 'locked', 'Locked', 'true / false', 'api', 'Tesla API vehicle_state.locked, TeslaFi locked', 'No lock state on CAN. Never from a lock request: a request is not the lock state.'],
  ['mqtt.sentry-mode', 'sentry_mode', 'Sentry mode', 'true / false', 'published', 'Vehicle modes (0x284 UIsentryMode284)', ''],
  ['mqtt.windows-open', 'windows_open', 'Any window open', 'true / false', 'api', 'Tesla API fd/fp/rd/rp_window, TeslaFi the same', 'CAN only has switch presses, which cannot tell open from closed.'],
  ['mqtt.window-each', ['window_driver_front', 'window_passenger_front', 'window_driver_rear', 'window_passenger_rear'], 'Each window', 'true / false', 'api', 'Tesla API fd/fp/rd/rp_window, TeslaFi the same', 'Named by seat: driver front, passenger front, driver rear, passenger rear. Open is anything but fully closed.'],
  ['mqtt.doors-open', 'doors_open', 'Any door open', 'true / false', 'published', 'Door latches (0x102, 0x103)', ''],
  ['mqtt.door-each', ['door_front_left', 'door_front_right', 'door_rear_left', 'door_rear_right'], 'Each door', 'true / false', 'published', 'Door latches (0x102 left, 0x103 right)', 'Named by side; right-hand drive comes from the car configuration.'],
  ['mqtt.sun-roof-state', 'sunroof_state', 'Sunroof position', null, 'api', 'Tesla API sun_roof_state, TeslaFi the same', 'The input\u2019s own word (open, closed, vent). Not on CAN; the roof type below says whether there is one.'],
  ['mqtt.sun-roof-installed', 'roof', 'Roof type', null, 'published', 'Car configuration (0x7FF GTW_roofType)', 'Metal, fixed glass or panoramic.'],
  ['mqtt.sun-roof-percent-open', 'sunroof_open', 'Sunroof % open', '%', 'api', 'Tesla API sun_roof_percent_open, TeslaFi the same', 'No roof position on CAN.'],
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
  ['mqtt.scheduled-charging-start-time', 'scheduled_charge_start', 'Scheduled charge start', 'ISO time, UTC', 'api', 'Tesla API charge_state.scheduled_charging_start_time, TeslaFi the same', 'CAN only has schedule on/off flags. Not published while no charge is scheduled.'],
  [null, 'scheduled_departure', 'Scheduled departure', 'ISO time, UTC', 'api', 'Tesla API charge_state.scheduled_departure_time, TeslaFi the same', 'Not on CAN. Not published while no departure is set.'],
  ['mqtt.time-to-full-charge', 'time_to_full', 'Time to full', 'min', 'published', 'Time remaining (0x132)', ''],
  ['mqtt.tpms-pressure', ['tire_pressure_fl', 'tire_pressure_fr', 'tire_pressure_rl', 'tire_pressure_rr'], 'Tyre pressures', 'pressure', 'published', 'Tyre sensors (0x219)', 'Front left, front right, rear left, rear right.'],
  ['mqtt.tpms-soft-warning', 'tire_low', 'Tyre pressure low', 'true / false', 'published', 'Pressure against the recommended pressure (0x219 page 4)', ''],
  ['mqtt.active-route', ['route_destination', 'route_distance', 'route_minutes', 'route_traffic_delay'], 'Route (destination, distance, minutes, traffic)', null, 'api', 'Tesla API drive_state.active_route_*, TeslaFi the same', 'Destination name, distance left in the server unit, minutes left, minutes lost to traffic. Not on CAN; empty when the input stops for 10 min.'],
  ['mqtt.active-route-payload-routing', 'energy_at_destination', 'Energy at destination', 'kWh', 'published', 'Trip planning (0x082 UI_energyAtDestination)', 'While navigating. The destination and minutes come from the APIs (Route above).'],
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
].map(([cid, keys, what, unitKind, status, source, why]) => ({ cid, keys: keys == null ? [] : Array.isArray(keys) ? keys : [keys], what, unitKind, status, source, why, inputs: status === 'api' ? API_INPUTS : [] }));

// Home Assistant: what it gets, per group, with the same statuses
const ENTITY_GROUPS = [
  ['ha.entity.location-tracker', 'Device tracker', 'The car on the Home Assistant map, home or away by zone', 'location', 'published', 'Only with location publishing on. About 75 s behind live (one-minute logs and the upload).'],
  ['ha.entity.active-route-tracker', 'Destination tracker', 'Where the car is navigating to', null, 'none', 'Not on CAN; the APIs give the destination\u2019s name (Route destination), not its position.'],
  ['ha.entity.status-sensors', 'Status sensors', 'State, State since, Charging, Screen mode, Last data, Software version, Update version, Update download, Sunroof, Sunroof open', 'state, since, charging, display_mode, updated, version, update_version, update_download, sunroof_state, sunroof_open', 'published', 'Software, update and sunroof sensors come from the Tesla API or TeslaFi, and go unavailable when that input stops.'],
  ['ha.entity.config-sensors', 'Build sensors', 'Paint, wheels, spoiler, roof, Trim', 'paint, wheels, spoiler, roof, trim', 'published', 'Model and year are on the device already.'],
  ['ha.entity.motion-sensors', 'Motion sensors', 'Place, Gear, Power, Speed, Heading, Elevation', 'place, gear, power, speed, heading, elevation', 'published', 'Heading only while driving: the chassis bus is quiet when parked.'],
  ['ha.entity.parking-brake', 'In park', 'On while the gear is P', 'in_park', 'published', 'From the gear. The parking brake itself (0x228, 0x288) is on CAN as well.'],
  ['ha.entity.temperature-sensors', 'Temperature sensors', 'Inside, outside and battery temperature', 'inside_temp, outside_temp, battery_temp', 'published', 'Battery temperature is extra.'],
  ['ha.entity.range-sensors', 'Range sensors', 'Odometer, rated, ideal and estimated range', 'odometer, range, range_ideal, range_estimated', 'published', 'Estimated range is Sentry Mate’s own, labelled as such.'],
  ['ha.entity.charging-sensors', 'Charging sensors', 'Battery, Usable level, Charge status, Limit, Charge power, Charger, Current, Voltage, Phases, Current asked for, Current limit, Time to full, Energy remaining, Scheduled charge start, Scheduled departure', 'soc, soc_usable, charge_status, charge_limit, charge_power, charge_kind, charger_*, charge_current_*, time_to_full, energy_remaining, scheduled_charge_start, scheduled_departure', 'published', 'Current, voltage, phases and time to full are sent while charging. The two schedule times come from the Tesla API or TeslaFi.'],
  ['ha.entity.charge-energy-added.counter', 'Energy added counter', 'A running total for Home Assistant’s Energy view', 'charge_energy_added', 'published', 'Sent as the pack’s lifetime counter (total_increasing), which never resets, rather than a per-charge value that does.'],
  ['ha.entity.tpms-sensors', 'Tyre sensors', 'Four tyre pressures, Pressure low', 'tire_pressure_fl … rr, tire_low', 'published', 'Which sensor is which wheel is still to be confirmed on new firmware.'],
  ['ha.entity.active-route-destination', 'Route destination', 'Destination name', 'route_destination', 'api', 'Not on CAN.'],
  ['ha.entity.active-route-energy', 'Route energy', 'Navigating, Battery expected on arrival', 'navigating, energy_at_destination', 'published', 'Shown as a real 0; unavailable while not navigating.'],
  ['ha.entity.active-route-distance', 'Route distance', 'Distance left', 'route_distance', 'api', 'Not on CAN.'],
  ['ha.entity.active-route-minutes', 'Route minutes', 'Minutes left', 'route_minutes', 'api', 'Not on CAN.'],
  ['ha.entity.active-route-traffic', 'Route traffic', 'Minutes lost to traffic', 'route_traffic_delay', 'api', 'Not on CAN.'],
  ['ha.entity.binary-status', 'Health on/off', 'Logger healthy, Update installing, Update waiting, Service mode', 'healthy, installing, update_available, service_mode', 'published', 'Update waiting comes from the Tesla API or TeslaFi; the software_changed event says when one was installed.'],
  ['ha.entity.binary-security', 'Security on/off', 'Doors (any and each), trunk, frunk, Sentry mode, someone in the car, Locked, Windows (any and each)', 'doors_open, door_*, trunk_open, frunk_open, sentry_mode, user_present, locked, windows_open, window_*', 'published', 'Lock and windows are not on CAN: they come from the Tesla API or TeslaFi, never guessed.'],
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
const EVENT_TYPES = EVENTS.map((e) => e[0]);

/* ---------------------------------------------------------------------- small pieces */
const statusTag = (s, inputs) => tag(STATUS[s][0] ?? apiWord(inputs), STATUS[s][1]);
const code = (t) => h('span', { class: 'prompt break' }, t);
const errText = (e) => (e?.fields ? Object.values(e.fields).join(' ') : errorWords(e).text);
const ERROR_WORDS = { connection_refused: 'the broker refused the connection', pin_mismatch: 'the certificate did not match the pin', no_host: 'no broker host set', tls_failed: 'tls failed', auth_failed: 'user name or password refused', timeout: 'no answer in time', invalid_settings: 'the settings are not valid' };
const lines = (rows) => h('ul', { class: 'ln-list' }, rows.filter(Boolean).map(([t, text, cv]) => h('li', { cover: cv || null }, t, h('span', { class: 'ln-list-t' }, text))));
const haDomain = (topic) => { const p = String(topic || '').split('/'); return p.length >= 5 ? p[p.length - 4] : null; };
const haPrefix = (topic) => { const p = String(topic || '').split('/'); return p.length >= 5 ? p.slice(0, -4).join('/') : null; };
const slug = (s) => String(s || '').toLowerCase().normalize('NFKD').replace(/[̀-ͯ]/g, '').replace(/[^a-z0-9]+/g, '_').replace(/^_+|_+$/g, '') || 'car';
const prefixOk = (v) => !!v && !/[+#]/.test(v) && !v.startsWith('$') && !v.split('/').some((p) => !p);

export async function mount(ctx) {
  const admin = ctx.admin;
  const [cfg0, status0, net, cars] = await Promise.all([
    admin ? ctx.api.get('settings/mqtt').catch(() => null) : null,
    ctx.api.get('settings/mqtt/status').catch(() => null),
    admin ? ctx.api.get('network').catch(() => null) : null,
    ctx.api.get('cars', { sort: 'name', limit: 100 }).catch(() => ({ items: [] })),
  ]);
  if (!ctx.alive()) return;
  let cfg = cfg0, status = status0;
  const carList = cars?.items || [];
  let carId = (ctx.query.car && carList.find((c) => c.id === ctx.query.car)?.id) || carList.find((c) => c.id === state.lastCar())?.id || carList[0]?.id || null;
  let pv = null;
  const loadPreview = async () => {
    if (!carId) { pv = null; return; }
    try { pv = await ctx.api.get('settings/mqtt/preview', { car: carId }); } catch (e) { if (e.name === 'AbortError') throw e; pv = { error: e, messages: [], ha_entities: [] }; }
  };
  await loadPreview();
  if (!ctx.alive()) return;
  const ns = () => cfg?.namespace || (pv?.messages?.[0]?.topic || 'sentrymate/status').replace(/\/status$/, '');
  const units = () => pv?.units || { distance: 'km', temperature: 'c', pressure: 'bar' };
  const carName = () => pv?.name || carList.find((c) => c.id === carId)?.name || carId || 'car';

  /* ---------- saving: PUT the whole setting with its version */
  const body = (patch) => {
    const b = { ...cfg, ...patch, ha_discovery: { ...cfg.ha_discovery, ...(patch.ha_discovery || {}) } };
    delete b.password_set; delete b.password;
    if ('password' in patch) b.password = patch.password;
    return b;
  };
  async function put(patch) {
    try { cfg = await ctx.api.put('settings/mqtt', body(patch)); } catch (e) {
      if (e.status === 409) { cfg = await ctx.api.get('settings/mqtt').catch(() => cfg); throw new Error('Someone else changed these settings first; reload to see theirs.'); }
      throw e;
    }
    status = await ctx.api.get('settings/mqtt/status').catch(() => status);
    if (ctx.alive()) paintStatus();
  }

  /* ---------- the status tag in the header and on the block */
  const headTag = h('span');
  const mqttTag = h('span');
  const statusSlot = h('div');
  function paintStatus() {
    const s = status;
    const tg = !s ? tag('unknown', 'muted') : !s.enabled ? tag('off', 'muted') : s.connected ? tag('connected', 'ok') : tag('not connected', 'warn');
    replace(headTag, 'mqtt ', tg.cloneNode(true));
    replace(mqttTag, tg);
    if (!s) { replace(statusSlot, note('The broker status could not be read.')); return; }
    replace(statusSlot, kv([
      ['broker', s.broker ? [s.broker, ' ', s.tls ? tag('tls', 'ok') : tag('no tls', 'warn')] : h('span', { class: 'muted' }, 'none set')],
      ['delivered', formatNumber(s.delivered ?? 0)],
      ['waiting', [formatNumber(s.waiting ?? 0), h('span', { class: 'muted' }, s.enabled ? ' kept until the broker is back; the newest value of a topic wins' : ' nothing waits while off')]],
      ['dropped', [formatNumber(s.dropped ?? 0), h('span', { class: 'muted' }, ' older values replaced before the broker was back')]],
      ['last error', s.last_error ? [ERROR_WORDS[s.last_error] || s.last_error, h('span', { class: 'muted' }, ` ${dateTime(s.last_error_ms, ctx.tz)} (${ago(s.last_error_ms)})`)] : h('span', { class: 'muted' }, 'none since the server started'), { tone: s.last_error ? 'warn' : null }],
    ]));
  }

  /* ---------- the broker */
  const brokerSlot = h('div');
  const testOut = h('pre', { class: 'mono-box out-box', hidden: true, 'aria-live': 'polite' });
  async function test(b) {
    b.disabled = true;
    testOut.hidden = false; replace(testOut, '> connecting');
    try {
      const r = await ctx.api.post('settings/mqtt/test');
      if (!ctx.alive()) return;
      replace(testOut, `> test ${dateTime(r.at_ms, ctx.tz)}\n`, r.ok ? tag('ok', 'ok') : tag('bad', 'bad'), ` ${r.message || (r.ok ? 'connected and published' : ERROR_WORDS[r.error] || r.error)}`,
        r.ok && r.latency_ms != null ? `\n  ${formatNumber(Math.round(r.latency_ms))} ms` : '', r.ok && r.topic ? `\n  topic ${r.topic}` : '');
      status = await ctx.api.get('settings/mqtt/status').catch(() => status);
      paintStatus(); paintLog();
    } catch (e) { if (e.name !== 'AbortError') replace(testOut, tag('bad', 'bad'), ` ${errText(e)}`); } finally { b.disabled = false; }
  }
  function paintBroker() {
    if (!cfg) {
      replace(brokerSlot, editLines([
        { key: 'enabled', type: 'show', value: status?.enabled ? 'on' : 'off', cover: 'docs.env.mqtt.disable-mqtt' },
        { key: 'broker', type: 'show', value: status?.broker || '', empty: 'not set', cover: ['docs.env.mqtt.mqtt-host', 'docs.env.mqtt.mqtt-port', 'docs.env.mqtt.mqtt-ipv6'] },
        { key: 'tls', type: 'show', value: status?.tls ? 'on' : 'off', cover: ['docs.env.mqtt.mqtt-tls', 'docs.env.mqtt.mqtt-tls-accept-invalid-certs'] },
        { key: 'login', type: 'show', value: 'set by an admin', cover: ['docs.env.mqtt.mqtt-username', 'docs.env.mqtt.mqtt-password'] },
        { key: 'namespace', type: 'show', value: ns(), cover: 'docs.env.mqtt.mqtt-namespace' },
      ]).el, note('Only an admin sees and changes the broker settings. The status, the preview and the events below are yours to read.'));
      return;
    }
    const g = editGroup({ label: 'save connection', save: async (v) => {
      const patch = {};
      for (const [k, x] of Object.entries(v)) patch[k] = x;
      const tls = 'tls' in patch ? patch.tls : cfg.tls;
      if ((patch.password || cfg.password_set) && !tls) throw Object.assign(new Error('tls'), { fields: { tls: 'a password is only ever sent over tls: keep tls on, or remove the password' } });
      if ((patch.pin_sha256 ?? cfg.pin_sha256) && !tls) throw Object.assign(new Error('pin'), { fields: { pin_sha256: 'a certificate pin needs tls' } });
      if (patch.password === null) delete patch.password;
      await put(patch);
    } });
    const lines1 = editLines([
      { key: 'enabled', type: 'bool', value: cfg.enabled, cover: 'docs.env.mqtt.disable-mqtt',
        help: 'Off until you turn it on: nothing connects anywhere before that. Off stops every message; the broker keeps what it last got.',
        save: async (v) => { if (v && !cfg.host) throw new Error('set the broker host first, and save the connection'); await put({ enabled: v }); } },
    ]);
    const lines2 = editLines([
      { key: 'host', path: 'host', type: 'text', value: cfg.host || '', group: g, cover: ['docs.env.mqtt.mqtt-host', 'docs.env.mqtt.mqtt-ipv6'], attrs: { placeholder: 'broker.local' },
        help: 'A name or address. It may resolve to IPv4 or IPv6: the connection follows the address, so there is no IPv6 switch.' },
      { key: 'port', path: 'port', type: 'number', value: cfg.port, group: g, cls: 'ke--short', cover: 'docs.env.mqtt.mqtt-port', attrs: { min: '1', max: '65535' }, help: '8883 is usual for MQTT over TLS, 1883 without.' },
      { key: 'username', path: 'username', type: 'text', value: cfg.username || '', group: g, cover: 'docs.env.mqtt.mqtt-username', help: 'Empty for a broker without accounts.' },
      { key: 'password', path: 'password', type: 'password', value: '', group: g, cover: 'docs.env.mqtt.mqtt-password', attrs: { placeholder: cfg.password_set ? '(saved; empty keeps it)' : '(none)', autocomplete: 'new-password' },
        help: cfg.password_set ? 'A password is saved; it is never shown. A change of host, port, tls or pin needs it typed again.' : 'No password saved. It is only ever sent over TLS.',
        after: cfg.password_set ? btn('remove', async () => { try { await put({ password: null }); toast('Password removed.'); paintBroker(); } catch (e) { toast(errText(e), { kind: 'error' }); } }, { kind: 'quiet' }) : null },
      { key: 'tls', path: 'tls', type: 'bool', value: cfg.tls, group: g, cover: 'docs.env.mqtt.mqtt-tls', help: 'On by default. Without TLS no password is sent, and the server refuses to save one.' },
      { key: 'pin_sha256', path: 'pin_sha256', type: 'text', value: cfg.pin_sha256 || '', group: g, cover: 'docs.env.mqtt.mqtt-tls-accept-invalid-certs',
        parse: (t) => (t.trim() === '' ? { value: null } : /^([0-9a-f]{2}:?){31}[0-9a-f]{2}$/i.test(t.trim()) ? { value: t.trim() } : { error: '64 hex digits, colons allowed, or empty' }),
        help: 'For a broker with its own (self-signed) certificate: only that certificate is accepted. There is no "accept any certificate" switch. Empty: the certificate must be valid for the host.' },
      { key: 'namespace', path: 'namespace', type: 'text', value: cfg.namespace, group: g, cover: 'docs.env.mqtt.mqtt-namespace',
        parse: (t) => (prefixOk(t.trim()) ? { value: t.trim() } : { error: 'levels with /, no + or #, not starting with $, no empty level' }),
        help: 'Every topic starts with this. Two servers on one broker use different prefixes.' },
    ]);
    replace(brokerSlot, lines1.el, lines2.el, g.bar,
      h('div', { class: 'acts out-box' }, h('button', { class: 'btn', type: 'button', onclick: (e) => test(e.currentTarget) }, 'test the saved settings'),
        h('span', { class: 'muted' }, 'connects with the saved settings and publishes one message')), testOut);
  }

  const logSlot = h('div');
  async function paintLog() {
    if (!admin) { replace(logSlot); return; }
    let rows = [];
    try {
      const now = ctx.api.now();
      const p = ctx.api.pager('audit', { from_ms: now - 90 * DAY, to_ms: now + 60_000 }, { limit: 200 });
      for (let i = 0; i < 20 && rows.length < 8 && !p.done; i++) {
        const got = (await p.more()) || [];
        if (!ctx.alive()) return;
        rows = rows.concat(got.filter((e) => e.target === 'settings:mqtt'));
      }
    } catch (e) { if (e.name === 'AbortError') return; replace(logSlot, note('The broker log could not be read.')); return; }
    if (!ctx.alive()) return;
    replace(logSlot, h('p', { class: 'sub-title' }, 'broker log, last 90 days'),
      h('pre', { class: 'mono-box log-tail' }, rows.length ? rows.slice(0, 8).map((r) => `${dateTime(r.at_ms, ctx.tz)}  ${r.account || 'server'}  ${r.summary}`).join('\n') : 'nothing changed or tested in the last 90 days'),
      h('p', { class: 'note' }, link(state.buildUrl('settings-server'), 'whole audit log in settings/server')));
  }

  /* ---------- topics and rules */
  const topicsSlot = h('div');
  function paintTopics() {
    const n = ns(), cid = carId || 'c1';
    const ro = !cfg;
    replace(topicsSlot,
      h('pre', { class: 'mono-box', cover: 'mqtt.topic-scheme', 'aria-label': 'Topic layout' },
        `${n}/status                 online or offline; retained; the broker sets offline if the server goes away\n`,
        `${n}/cars/${cid}/<value>       one topic per value (below); retained, QoS 1, text in the server units\n`,
        `${n}/cars/${cid}/updated       the time of the newest data behind the values (ISO, UTC)\n`,
        `${n}/cars/${cid}/event         one JSON message per event; never retained, so nothing fires twice`),
      lines([
        [tag('ids', 'muted'), ['Each car has Sentry Mate\'s own id (', code(cid), ' here), never reused. A new CAN Bridge key for the same car joins that car, so topics never move. Ids are listed in ', link(state.buildUrl('settings-cars'), 'settings/cars'), '.'], 'mqtt.note-car-id'],
        [tag('missing', 'muted'), ['A value the car did not send is not published: the broker keeps the last one, and ', code('updated'), ' says how old it is. Speed and gear are held while parked; read ', code('state'), ' with them. Position topics are emptied when location publishing goes off.'], 'mqtt.note-empty-values'],
      ]),
      editLines([
        { key: 'no_place_text', type: 'text', value: cfg?.no_place_text ?? '', readOnly: ro, keepEmpty: true, cover: 'docs.env.core.default-geofence', attrs: { maxlength: '40' },
          help: 'Published as the place when the car is at no saved place, up to 40 characters. Empty publishes an empty place.', save: (v) => put({ no_place_text: (v ?? '').trim() }) },
        { key: 'publish_location', type: 'bool', value: cfg ? cfg.publish_location : !!pv?.messages?.some((m) => m.topic.endsWith('/location') && m.payload), readOnly: ro,
          help: 'Off by default. On: latitude, longitude and a JSON position with its accuracy, for maps and arrival automations.', save: (v) => put({ publish_location: v }) },
      ]).el);
  }

  /* ---------- the car for the preview */
  const carKeys = () => h('div', { class: 'carline', role: 'group', 'aria-label': 'Car for the preview' }, h('span', { class: 'muted' }, 'preview for:'),
    soldAwareKeys(carList.slice(0, 12), (c) => h('button', { class: 'key', type: 'button', 'aria-pressed': String(c.id === carId), onclick: () => pickCar(c.id) }, c.name || c.id), { keep: carId }));
  const carSlot = h('div');
  async function pickCar(next) {
    if (!next || next === carId) return;
    carId = next;
    ctx.setQuery({ car: carId });
    await loadPreview();
    if (!ctx.alive()) return;
    replace(carSlot, carKeys());
    paintTopics(); paintValues(); paintHa(); paintEvents();
  }
  replace(carSlot, carKeys());

  /* ---------- published values */
  const valuesSlot = h('div');
  let valuesT = null;
  function paintValues() {
    const byTopic = new Map((pv?.messages || []).map((m) => [m.topic.split('/').pop(), m.payload]));
    const u = units();
    const rows = VALUES.map((v) => ({ ...v, shown: v.keys.map((k) => [k, byTopic.has(k) ? byTopic.get(k) : undefined]), unit: v.unitKind ? unitText(v.unitKind, u) : '' }));
    const counts = Object.fromEntries(STATUS_ORDER.map((s) => [s, rows.filter((r) => r.status === s).length]));
    // the car's live inputs, when the server says (an older one does not: then nothing is claimed)
    const live = Array.isArray(pv?.inputs) ? new Set(pv.inputs.map((i) => i.kind)) : null;
    const noInput = (r) => r.inputs.length > 0 && live && !r.inputs.some((k) => live.has(k));
    // where each key's current value came from: '[tfi] data from TeslaFi', and [stale] when it is old
    const whence = (r) => {
      const got = r.keys.map((k) => pv?.sources?.[k]).filter(Boolean);
      if (!got.length) return null;
      const seen = new Map();
      for (const g of got) { const k = `${g.kind}|${g.label}`; const o = seen.get(k); seen.set(k, { ...g, stale: (o?.stale || false) || !!g.stale }); }
      return [...seen.values()].map((g) => h('span', { class: 'pay-src' }, src({ sources: [{ kind: g.kind, label: g.label }] }, { full: true }),
        g.stale ? [' ', h('span', { title: g.at_ms ? `newest ${dateTime(g.at_ms, ctx.tz)}` : null }, tag('stale', 'warn'))] : null));
    };
    // a key of a multi-topic row without the part all of them share: window_driver_front -> driver_front
    const short = (keys, k) => {
      const parts = keys.map((x) => x.split('_'));
      let n = 0;
      while (parts.every((x) => x.length > n + 1 && x[n] === parts[0][n])) n++;
      return k.split('_').slice(n).join('_');
    };
    const payload = (r) => {
      if (r.status === 'device') return r.cid === 'mqtt.model' ? pv?.device?.model || null : pv?.device?.name || pv?.name || null;
      if ((r.status !== 'published' && r.status !== 'api') || !r.keys.length) return null;
      if (noInput(r)) return h('span', { class: 'muted' }, 'no input gives this now');
      if (!r.shown.some(([, p]) => p !== undefined)) return h('span', { class: 'muted' }, 'not sent yet');
      return [r.shown.map(([k, p], i) => [i ? ' ' : '', r.shown.length > 1 ? h('span', { class: 'muted' }, `${short(r.keys, k)}=`) : null, p === undefined ? DASH : p === '' ? h('span', { class: 'muted' }, '(empty)') : p.length > 40 ? `${p.slice(0, 37)}...` : p]),
        r.status === 'api' ? whence(r) : null];
    };
    const sideWords = [[counts.published, 'published'], [counts.api, 'from the apis'], [counts.logged, 'logged'], [counts.signal, counts.signal === 1 ? 'needs a signal' : 'need a signal'], [counts.none + counts.never, 'left out']]
      .filter(([n]) => n > 0).map(([n, w]) => `${n} ${w}`).join(', ');
    valuesT = table(ctx, {
      title: 'values', rows, csv: false, limit: 500, label: 'Published values',
      side: h('span', { class: 'muted' }, sideWords),
      note: `What a broker gets now for ${carName()}, as text in the server units (${unitText('distance', u)}, ${unitText('temperature', u)}, ${unitText('pressure', u)}): MQTT has one set of units for everyone. The VIN is never one of them; the position only when you opt in.`,
      sort: { key: 'status', dir: 'asc' },
      columns: [
        { key: 'what', label: 'value', sort: true, ph: 'main', render: (r) => h('span', { cover: r.cid || null }, r.what, r.cid ? null : h('span', { class: 'muted' }, ' extra')) },
        { key: 'topic', label: 'topic', sort: true, ph: 'sub', value: (r) => r.keys[0] || '', render: (r) => (r.keys.length ? r.keys.join(' ') : h('span', { class: 'muted' }, 'no topic')) },
        { key: 'payload', label: 'payload now', render: payload },
        { key: 'unit', label: 'unit', render: (r) => r.unit || h('span', { class: 'muted' }, 'text') },
        { key: 'status', label: 'status', sort: true, ph: 'num', value: (r) => STATUS_ORDER.indexOf(r.status), render: (r) => statusTag(r.status, r.inputs) },
        { key: 'source', label: 'comes from', grow: true, cls: 'wrap-cell', render: (r) => [r.source, r.why ? h('span', { class: 'muted' }, ` ${r.why}`) : null] },
      ],
    });
    replace(valuesSlot,
      !pv || pv.error || !carId ? empty(carId ? 'the preview could not be made' : 'no car to preview', carId ? errText(pv?.error) : 'A car appears once its CAN Bridge uploads its first log. The list still says what each value is.') : null,
      h('dl', { class: 'kv kv--one kv--legend' }, STATUS_ORDER.map((k) => h('div', { class: 'kv-row' }, h('dt', { class: 'kv-k' }, statusTag(k)), h('dd', { class: 'kv-v muted' }, STATUS[k][2])))),
      valuesT.el);
  }

  /* ---------- Home Assistant */
  const haSlot = h('div');
  const haTag = h('span');
  function paintHa() {
    const ents = pv?.ha_entities || [];
    const ha = cfg?.ha_discovery || { enabled: ents.length > 0, prefix: haPrefix(ents[0]?.config_topic) || 'homeassistant', url: null };
    const ro = !cfg;
    replace(haTag, ha.enabled ? tag('on', 'ok') : tag('off', 'muted'));
    const d = pv?.device;
    const node = d?.identifiers?.[0] || `${ns().replace(/[^A-Za-z0-9_-]/g, '_')}_${carId || 'c1'}`;
    const published = new Set((pv?.messages || []).filter((m) => m.payload !== '').map((m) => m.topic.split('/').pop()).filter((k) => !['status', 'latitude', 'longitude'].includes(k)));
    const withEnt = new Set(ents.map((e) => e.state_topic.split('/').pop()));
    const missing = [...published].filter((k) => !withEnt.has(k));
    const name = slug(d?.name || pv?.name || 'car');
    const eid = (key) => { const e = ents.find((x) => x.state_topic.endsWith(`/${key}`)); return e ? `${haDomain(e.config_topic) || 'sensor'}.${name}_${slug(e.name)}` : null; };
    const glance = ['soc', 'state', 'charging', 'plugged_in', 'gear', 'speed', 'range', 'sentry_mode', 'doors_open', 'inside_temp', 'updated'].map(eid).filter(Boolean);
    const all = ents.filter((e) => !['event', 'device_tracker'].includes(haDomain(e.config_topic))).map((e) => `${haDomain(e.config_topic) || 'sensor'}.${name}_${slug(e.name)}`);
    const tracker = ents.find((e) => haDomain(e.config_topic) === 'device_tracker');
    const yGlance = ents.length ? ['type: glance', `title: ${pv?.name || 'Car'}`, 'entities:', ...glance.map((x) => `  - ${x}`)].join('\n') : '';
    const yMap = tracker ? ['type: map', 'hours_to_show: 24', 'entities:', `  - device_tracker.${name}_location`].join('\n') : '';
    const yList = ents.length ? ['type: entities', `title: ${pv?.name || 'Car'}`, 'entities:', ...all.map((x) => `  - ${x}`), ...(tracker ? ['  - entity: proximity.home', '    name: Distance to home'] : [])].join('\n') : '';
    const card = (t, text, yaml, cv, none) => h('div', { cover: cv }, h('p', { class: 'sub-title' }, t, yaml ? copyBtn(yaml, 'copy') : null), h('p', { class: 'fld-hint' }, text),
      h('pre', { class: 'mono-box yaml' }, yaml || none));
    const groupsT = table(ctx, {
      title: 'what home assistant gets', rows: ENTITY_GROUPS, csv: false, bare: false, label: 'Home Assistant entity groups',
      columns: [
        { key: 'what', label: 'group', ph: 'main', render: (r) => h('span', { cover: r.cid }, r.what) },
        { key: 'gives', label: 'entities', ph: 'sub' },
        { key: 'topics', label: 'from topics', render: (r) => r.topics || null },
        { key: 'status', label: 'status', ph: 'num', render: (r) => statusTag(r.status) },
        { key: 'why', label: 'notes', grow: true, cls: 'wrap-cell', render: (r) => r.why || null },
      ],
    });
    const entT = table(ctx, {
      title: `entities for ${carName()}`, rows: ents, csv: false, label: 'Home Assistant entities', sort: { key: 'name', dir: 'asc' }, limit: 200,
      columns: [
        { key: 'name', label: 'name', sort: true, ph: 'main' },
        { key: 'id', label: 'unique id', ph: 'sub' },
        { key: 'unit', label: 'unit', ph: 'num', render: (r) => r.unit || null },
        { key: 'device_class', label: 'kind', render: (r) => r.device_class || null },
        { key: 'state_topic', label: 'state topic', grow: true },
      ],
      empty: () => empty('no entities', cfg && !cfg.ha_discovery.enabled ? 'Discovery is off. Turn it on above and every entity listed here goes to Home Assistant.' : 'There is no car to preview yet.'),
    });
    S.haTables = [groupsT, entT];
    replace(haSlot,
      editLines([
        { key: 'ha_discovery.enabled', type: 'bool', value: ha.enabled, readOnly: ro, cover: 'ha.discovery.enable',
          help: 'On: each car appears in Home Assistant by itself with every sensor, no YAML to write. Needs publishing on.', save: async (v) => { await put({ ha_discovery: { enabled: v } }); await loadPreview(); paintHa(); } },
        { key: 'ha_discovery.prefix', type: 'text', value: ha.prefix, readOnly: ro, cover: 'ha.discovery.prefix', parse: (t) => (prefixOk(t.trim()) ? { value: t.trim() } : { error: 'no + or #, not starting with $, no empty level' }),
          help: 'Must match Home Assistant\'s own MQTT discovery prefix: homeassistant unless you changed it there.', save: (v) => put({ ha_discovery: { prefix: v } }) },
        { key: 'ha_discovery.url', type: 'text', value: ha.url || '', readOnly: ro, cover: 'ha.discovery.url', attrs: { placeholder: `https://${location.host}` },
          parse: (t) => (t.trim() === '' ? { value: null } : /^https:\/\/[^\s/]+(\/\S*)?$/.test(t.trim()) ? { value: t.trim() } : { error: 'an https:// address, or empty' }),
          help: 'This server\'s address as Home Assistant users reach it: each car\'s device then links to its overview. Empty: no link.', save: (v) => put({ ha_discovery: { url: v } }) },
      ]).el,
      h('p', { class: 'sub-title' }, 'the device'),
      h('div', { cover: ['ha.discovery.device-payload', 'ha.device.identity'] }, kv([
        ['name', d?.name || pv?.name || DASH], ['maker', 'Tesla'], ['model', d?.model || DASH], ['identifier', node],
        ['link', ha.url ? `${ha.url.replace(/\/$/, '')}/cars/${carId}` : h('span', { class: 'muted' }, 'none')], ['entities', String(ents.length)],
      ])),
      lines([
        [tag('one', 'muted'), 'One device per car holds all of its entities. Two servers with different topic prefixes on one broker never share a device.'],
        [tag('ids', 'muted'), ['Each entity\'s unique id is the identifier and the value: ', code(`${node}_soc`), '. Home Assistant names the visible id after the device: ', code(`sensor.${name}_battery`), '. Ids stay the same across versions.'], 'ha.entity.id-scheme'],
        [missing.length ? tag('gap', 'warn') : tag('ok', 'ok'), ents.length ? (missing.length ? `${withEnt.size} of the ${published.size} values published for this car have an entity; missing: ${missing.join(', ')}.` : `All ${published.size} values published for this car have an entity (position through the device tracker).`) : 'Discovery is off, so Home Assistant gets no entities; the topics are still published. The rule: every value topic gets an entity.', 'ha.discovery.coverage-gap'],
        [tag('removed', 'muted'), 'A car deleted (or taken off publishing) loses its device and every entity in Home Assistant at once. A rename sends the device again with the new name; entity ids stay.', 'ha.discovery.removed-car-cleanup'],
        [tag('off', 'muted'), 'Discovery turned off: every car\'s device and entities are removed the moment you save. The plain topics stay.', 'ha.discovery.disabled-cleanup'],
        [tag('clean', 'muted'), 'Nothing is remembered: on each change and at each start, removals are sent for whatever should not be there. A changed prefix sends removals under the old prefix first.', 'ha.discovery.cleanup-policy'],
        [tag('limits', 'muted'), 'Home Assistant only gets what MQTT has: a value missing from MQTT is missing there too.', 'ha.discovery.limits-note'],
      ]),
      groupsT.el, entT.el,
      h('p', { class: 'sub-title' }, 'dashboard cards to paste'),
      note('Made from the entities above, for a Home Assistant dashboard (edit dashboard, add card, manual). Visible ids follow the car\'s name; if you renamed an entity there, change it here too.'),
      h('div', { class: 'cards3' },
        card('at a glance', 'The facts checked most.', yGlance, 'ha.lovelace.glance', 'turn discovery on to get this card'),
        card('map', 'Where the car is, on Home Assistant\'s map.', yMap, 'ha.lovelace.map', 'needs location publishing on'),
        card('everything', 'Every entity, and the distance to home when the position is published.', yList, 'ha.lovelace.entity-list', 'turn discovery on to get this card')),
      h('p', { class: 'note', cover: 'ha.proximity' }, 'Distance to home comes from Home Assistant\'s own proximity helper, with the car\'s device tracker as the tracked device. It needs location publishing on and knows an arrival about 75 s late.'));
  }
  const S = {};

  /* ---------- events */
  const eventsSlot = h('div');
  let eventsT = null;
  function paintEvents() {
    if (!carId) { eventsT = null; replace(eventsSlot, empty('no car yet', 'Events appear once a car has uploaded logs.')); return; }
    const now = ctx.api.now();
    eventsT = table(ctx, {
      title: 'recent events', path: `cars/${enc(carId)}/events`, params: { from_ms: now - 30 * DAY, to_ms: now + 60_000, types: EVENT_TYPES.join(',') }, csvName: `events-${carId}`, limit: 12, label: 'Recent events',
      note: 'The last 30 days, newest first, as a subscriber to the event topic got them.',
      columns: [
        { key: 'at_ms', label: 'when', ph: 'sub', render: (r) => dateTime(r.at_ms, ctx.tz) },
        { key: 'type', label: 'event', ph: 'main' },
        { key: 'payload', label: 'payload', grow: true, cls: 'wrap-cell', render: (r) => Object.entries(r.payload || {}).filter(([k]) => !['event_type', 'car', 'units'].includes(k)).slice(0, 6)
          .map(([k, v]) => `${k}=${v == null ? DASH : typeof v === 'object' ? JSON.stringify(v) : v}`).join(' ') || null },
      ],
      empty: () => empty('no events in the last 30 days'),
    });
    replace(eventsSlot, eventsT.el);
  }

  /* ---------- network */
  const PURPOSE = {
    geocoding: ['address lookup', 'only while online addresses are on; positions rounded first', 'settings-display'],
    tiles: ['map pictures', 'asked by each viewer\'s browser while online tiles are on', 'settings-display'],
    geonames_download: ['place-name download', 'only when an admin asks for new offline place names', 'settings-display'],
    mqtt: ['mqtt broker', 'only while publishing is on (above)', null],
    tesla: ['tesla api', 'only while a Tesla account is added and not paused: sign-in, the Owner API and its live stream (tesla account, above)', null],
    teslafi: ['teslafi api', 'only while a TeslaFi token is added and not paused: reads your cars\' data from TeslaFi', '#teslafi'],
    teslamate_mqtt: ['local teslamate (mqtt)', 'only while a local TeslaMate is added and not paused: subscribes to its topics, never publishes', '#teslamate'],
    teslamate_db: ['local teslamate (database)', 'only while its database catch-up is on: reads what MQTT missed', '#teslamate'],
    release_check: ['release check', 'asks whether a newer Sentry Mate exists, when on', 'settings-server'],
    upload: ['can bridge uploads', 'the car\'s CAN Bridge sends its logs here', null],
    web: ['this app and the api', 'browsers, apps and the api', null],
  };
  const netRows = net ? [...net.outbound.map((o) => ({ dir: 'out', ...o })), ...net.inbound.map((i) => ({ dir: 'in', purpose: i.purpose, enabled: true, port: i.port, tls: i.tls }))] : [];
  const netT = net ? table(ctx, {
    title: 'every connection', rows: netRows, csv: false, label: 'Network connections',
    note: `For a firewall: ${net.outbound.filter((o) => o.enabled).length} of ${net.outbound.length} outbound connections are on now; one that is off makes no request.`,
    columns: [
      { key: 'dir', label: 'way', ph: 'sub', render: (r) => (r.dir === 'in' ? 'in' : 'out') },
      { key: 'purpose', label: 'for', ph: 'main', render: (r) => [PURPOSE[r.purpose]?.[0] || r.purpose, h('span', { class: 'muted' }, ` ${PURPOSE[r.purpose]?.[1] || ''}`)] },
      { key: 'host', label: 'host or port', render: (r) => {
        if (r.port != null) return `port ${r.port}${r.tls ? ', tls' : ''}`;
        // map pictures can come from several hosts (the chosen preset, its names and roads, the styles viewers may pick)
        const hosts = Array.isArray(r.hosts) && r.hosts.length ? r.hosts : r.host ? [r.host] : [];
        if (!hosts.length) return h('span', { class: 'muted' }, 'not set');
        return hosts.length === 1 ? hosts[0] : h('span', { class: 'net-hosts', dataset: { hosts: String(hosts.length) } }, hosts.flatMap((x, i) => (i ? [h('br'), x] : [x])));
      } },
      { key: 'enabled', label: 'state', ph: 'num', render: (r) => (r.enabled ? tag(r.dir === 'in' ? 'open' : 'on', 'ok') : tag('off', 'muted')) },
      { key: 'last', label: 'last used', render: (r) => (r.last_used_ms ? ago(r.last_used_ms) : h('span', { class: 'muted' }, r.dir === 'in' ? 'listening' : 'never')) },
      { key: 'go', label: '', grow: true, render: (r) => { const to = PURPOSE[r.purpose]?.[2]; return to ? link(to.startsWith('#') ? `${state.buildUrl('settings-integrations')}${to}` : state.buildUrl(to), 'change') : null; } },
    ],
  }) : null;

  /* ---------- put it together */
  paintStatus(); paintBroker(); paintTopics(); paintValues(); paintHa(); paintEvents();
  const tesla = teslaSection(ctx, { cars: carList });
  const teslafi = teslafiSection(ctx, { cars: carList });
  const teslamate = teslamateSection(ctx, { cars: carList });
  const recipe = (t, cv, uses, text, s) => [statusTag(s), [h('b', null, t), ': ', text, h('span', { class: 'muted' }, ` uses ${uses.join(', ')}.`)], cv];
  paint(ctx, head(ctx, { extra: h('span', { class: 'muted' }, headTag) }),
    h('p', { class: 'note' }, 'Switches and single lines save at once (Enter saves, Esc undoes); the broker connection lines save together. Nothing leaves the server until publishing is on.'),
    tesla.el,
    teslafi.el,
    teslamate.el,
    section({ title: 'mqtt', id: 'mqtt', side: mqttTag }, brokerSlot, h('p', { class: 'sub-title' }, 'status'), statusSlot, logSlot,
      h('p', { class: 'note', cover: 'nr.setup.mqtt' }, 'Connect a client: Node-RED, Home Assistant or any MQTT client uses this broker. Subscribe to ', code(`${ns()}/cars/+/#`), ' for every car, or ', code(`${ns()}/cars/<car id>/#`), ' for one. Use TLS as here.')),
    section({ title: 'mqtt topics', side: h('span', { class: 'muted' }, 'layout and rules') }, topicsSlot),
    section({ title: 'mqtt values', side: h('span', { class: 'muted' }, 'every value, before you turn anything on') }, carSlot, valuesSlot),
    section({ title: 'home assistant', id: 'home-assistant', side: haTag }, haSlot),
    section({ title: 'events for automations', side: tag('published', 'ok') },
      h('div', { cover: 'nr.overview' },
        h('p', { class: 'note' }, 'Automations should react to what happened, not to a value changing. One message per event goes to ', code(`${ns()}/cars/${carId || 'c1'}/event`), '; never retained, so a restart never fires an automation again. Sentry Mate only publishes; nothing here can control the car.'),
        lines(EVENTS.map(([t, w, c]) => [tag('event', 'accent'), [code(t), ` ${w.toLowerCase()}: ${c}.`]]))),
      eventsSlot,
      h('p', { class: 'sub-title' }, 'ideas that work with these'),
      note('Values reach Sentry Mate about 75 s after the car sent them: fine for reminders and logs, late to open a door as the car pulls up.'),
      lines([
        recipe('garage door', 'ha.automation.garage', ['drive_ended', 'end_place'], 'close the door, or log the arrival, when a drive ends at home', 'published'),
        recipe('left open', 'ha.automation.left-open', ['doors_open', 'trunk_open', 'frunk_open'], 'remind after five minutes with a door, the trunk or the frunk open, and every five minutes after', 'published'),
        recipe('stop the reminder', 'ha.automation.left-open-cancel', ['doors_open', 'trunk_open', 'frunk_open'], 'end the repeat once everything reads closed', 'published'),
        recipe('plug in tonight', 'ha.automation.plug-in-reminder', ['plugged_in', 'soc', 'range'], 'an evening reminder when the car is home and not plugged in', 'published'),
        recipe('state changed', 'nr.notify.state-change', ['drive_started', 'drive_ended', 'state'], 'a message when a drive starts or ends, with how far and how efficient', 'published'),
        recipe('someone got in', 'nr.notify.driver-present', ['user_present'], 'a message when someone is seen in the car', 'published'),
        recipe('time to full', 'nr.notify.time-to-full', ['time_to_full', 'charge_power', 'soc'], 'a progress message while charging', 'published'),
        recipe('arrived, left', 'nr.notify.geofence', ['place', 'start_place', 'end_place'], 'a message when the car leaves one saved place or reaches another', 'published'),
      ])),
    section({ title: 'api', id: 'api', side: tag('on', 'ok') }, lines([
      [tag('stable', 'ok'), ['Two things are supported and kept stable: the MQTT topics on this page, and the read api under ', code(`${location.origin}/api/v1/`), '. Every UI pack is built on that api. The storage behind them is sealed and private: never read it directly.'], 'faq.public-api'],
      [tag('json', 'muted'), ['Cars, live values, drives, charges, places and every report, as JSON or CSV, versioned. For example ', code('GET /api/v1/cars'), ' lists the cars you may see, and ', code('GET /api/v1/cars/<id>/drives'), ' their drives.'], 'projects.teslamateapi'],
      [tag('access', 'muted'), ['The api signs in like this app and answers for that account only. Make an account per app or person in ', link(state.buildUrl('settings-server'), 'settings/server'), '.']],
      [tag('more', 'muted'), ['Costs, battery health, charging by place and every gap are pages of this app: ', link(state.buildUrl('statistics', carId ? { car: carId } : {}), 'statistics'), ', ', link(carId ? state.buildUrl('battery', { car: carId }) : '/', 'battery'), ', ', link(state.buildUrl('charges', carId ? { car: carId } : {}), 'charges'), '. Each table has a csv link.'], 'projects.custom-dashboards'],
      [tag('phone', 'muted'), 'This app works in a phone\'s browser and can be added to the home screen. A phone app of your own can use the same api.', 'projects.matedroid'],
    ])),
    section({ title: 'network', cover: 'faq.network-flows', side: h('span', { class: 'muted' }, 'every connection the server makes') },
      note('Sentry Mate needs no Tesla account and no cloud: the CAN Bridge (SMO) uploads to it, and everything else is off until you turn it on. A Tesla account and a TeslaFi token (above) are optional inputs; without them the server never connects to Tesla or TeslaFi.'),
      netT ? netT.el : note(admin ? 'The connection list could not be read.' : 'The list of connections is shown to admins. By default Sentry Mate makes no outbound connection at all.')),
    section({ title: 'works with', side: h('span', { class: 'muted' }, 'other people\'s tools') },
      h('p', { class: 'note', cover: 'projects.disclaimer' }, 'Named so you know what fits. Sentry Mate does not make, check or vouch for them. Anything built on the MQTT topics or the api keeps working; anything that reads the storage directly will break.'),
      lines([
        [tag('mqtt', 'muted'), 'route planners: a planner that takes the live battery plans with the real charge. A small bridge of yours forwards soc, location, speed and power; Sentry Mate never sends to one by itself.', 'projects.abrp'],
        [tag('mqtt', 'muted'), ['Home Assistant Tesla add-ons that can read values from MQTT instead of Tesla\'s servers use ', code(`${ns()}/cars/<id>/#`), '.'], 'projects.ha-tesla'],
        [tag('mqtt', 'muted'), 'desk gauges and lights: a colour from state and a level from soc.', 'projects.gaussmeter'],
        [tag('api', 'muted'), 'smart mirrors and wall screens: a status tile from soc, range, state and updated.', 'projects.magic-mirror'],
        [tag('mqtt', 'muted'), 'garage door openers that act on arrival and departure, by place: good for closing behind you, late for opening.', 'projects.geogdo'],
      ])));
  paintLog();
  await Promise.all([valuesT?.ready, eventsT?.ready, netT?.ready, tesla.load(), teslafi.load(), teslamate.load(), ...(S.haTables || []).map((t) => t.ready)].filter(Boolean)).catch(() => {});
  if (ctx.alive()) ctx.ready();
}
