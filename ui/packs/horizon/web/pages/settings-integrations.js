// Integrations (/settings/integrations): send the cars' live values to other systems, if the owner wants to.
// The page is the car's transmitter. At the top the ON AIR face says whether the broker takes messages, with
// trip counters for what was delivered, what waits and what was dropped. Then: the broker (switch, connection
// form with Save and Test, the broker log), the topic layout as a tree on a radio face and its rules, every
// value Sentry Mate publishes for a chosen car (its payload now, unit, status and where it comes from),
// Home Assistant discovery with the device and every entity, dashboard cards to paste, the events automations
// listen to with recent ones and ideas, the read API, every network connection, and other people's tools.
// First of all the Tesla account (pages/settings-tesla.js): the Tesla API as a second live input beside SMO, and
// TeslaFi (pages/settings-teslafi.js): TeslaFi's polls of the car, read with the owner's TeslaFi token, as a third.
//
// Broker settings, the network list and the broker log are for admins; anyone reads the status, the preview and
// the events of the cars they see. Switches and single fields save at once (PUT /settings/mqtt with the whole
// setting and its version); the connection saves with its button, since host, port, TLS, pin and password are
// checked together.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { icon, hud, plate, btn, tag, kv, note } from '../core/ui.js';
import { counters, band, csvBtn } from '../core/kit.js';
import { tracks } from '../core/tracks.js';
import { field, form } from '../core/form.js';
import { emptyState } from '../core/states.js';
import { confirmDialog, toast } from '../core/layer.js';
import { dateTime, ago, num } from '../core/fmt.js';
import { chan, hint, recLamp, presets, toggle, copyBtn, code, sig, errText } from '../core/setting.js';
import { teslaSection } from './settings-tesla.js';
import { teslafiSection } from './settings-teslafi.js';
import { teslamateSection } from './settings-teslamate.js';

export const id = 'settings-integrations';
export const css = true;

const enc = encodeURIComponent;
const DAY = 86_400_000;

/* ---------------------------------------------------------------------- what is sent, and why */

// what a consumer can count on, per value
const STATUS = {
  published: ['Sent', 'grn', 'Published now: retained, as text in the server units.'],
  api: ['From Tesla API / TeslaFi', 'violet', 'No CAN message carries it; the Tesla API or TeslaFi\'s polls give it, tagged with where it came from, and it goes stale (sent empty) when the input stops.'],
  device: ['On the device', 'cyan', 'Sent once with the Home Assistant device, not as its own topic.'],
  logged: ['Kept, not sent yet', 'cyan', 'Sentry Mate has it (from the car, or worked out); it is not its own topic yet.'],
  signal: ['Needs a new signal', 'org', 'The car sends it; Sentry Mate does not decode it yet. Nothing is guessed meanwhile.'],
  none: ['Not on the CAN bus', null, 'No message on the car\'s buses carries it, so it is never sent.'],
  never: ['Never sent', 'red', 'Left out on purpose.'],
};
const ORDER = ['published', 'api', 'device', 'logged', 'signal', 'none', 'never'];
// the live inputs an `api` value can come from, the preferred one first
const API_INPUTS = ['tesla_api', 'teslafi_api'];
const INPUT_NAME = { smo: 'SMO', tesla_api: 'Tesla API', teslafi_api: 'TeslaFi' };
// a status word; for `api` it names the inputs that can give the value
const statusWord = (s, inputs = API_INPUTS) => (s === 'api' ? `From ${inputs.map((k) => INPUT_NAME[k] || k).join(' / ')}` : STATUS[s][0]);
const statusTag = (s, inputs) => tag(statusWord(s, inputs), STATUS[s][1]);

// [item, topic(s), the value, unit kind, status, comes from, note]
const VALUES = [
  ['mqtt.display-name', null, 'Car name', null, 'device', 'Typed in Car settings', 'The Home Assistant device name. Topics use the car id, so a new name never moves them.'],
  ['mqtt.state', 'state', 'State', null, 'published', 'The timeline', 'driving, charging, online, asleep or unknown. Unknown (no log, recording paused) is never called asleep.'],
  ['mqtt.since', 'since', 'State since', 'ISO time, UTC', 'published', 'The start of the timeline span', 'Exact to the CAN frame.'],
  ['mqtt.healthy', 'healthy', 'Logger healthy', 'true / false', 'published', 'The Data health rule', 'False when no log came while the car was awake, the newest log failed its check, or frames were lost in the last hour.'],
  ['mqtt.version', 'version', 'Software version', null, 'api', 'Tesla API, else TeslaFi (no version text on CAN)', 'The version word, like 2026.38.4. software_changed still fires when module builds change.'],
  ['mqtt.update-available', 'update_available', 'Update waiting', 'true / false', 'api', 'Tesla API, else TeslaFi (only Tesla\'s servers know)', 'True while an update is offered, downloading or scheduled.'],
  ['mqtt.update-version', 'update_version', 'Waiting update version', null, 'api', 'Tesla API, else TeslaFi', 'Not sent while no update waits.'],
  ['mqtt.download-perc', 'update_download', 'Update download %', '%', 'api', 'Tesla API, else TeslaFi (no progress on CAN)', ''],
  ['mqtt.install-perc', 'installing', 'Update install %', 'true / false', 'published', 'Update status (0x343), else the Tesla API / TeslaFi', 'No percentage on CAN: an installing true or false instead.'],
  ['mqtt.vin', null, 'VIN', null, 'never', 'Only the first 11 characters are kept', 'Never sent, so a broker or Home Assistant never holds it.'],
  ['mqtt.model', null, 'Model', null, 'device', 'The VIN start', 'Model and year are the Home Assistant device model.'],
  ['mqtt.trim-badging', 'trim', 'Trim', null, 'published', 'The VIN start (drive units, battery)', 'As Car settings shows it; the car\'s build message can confirm it.'],
  ['mqtt.exterior-color', 'paint', 'Paint', null, 'published', 'Build message (0x7FF)', 'Newer paints come as a bare code; you can type the name.'],
  ['mqtt.wheel-type', 'wheels', 'Wheels', null, 'published', 'Build message (0x7FF)', ''],
  ['mqtt.spoiler-type', 'spoiler', 'Spoiler', null, 'published', 'Build message (0x7FF)', 'None or passive.'],
  ['mqtt.geofence', 'place', 'Place', null, 'published', 'Your saved places', 'The place the car is in; the no-place text when it is in none.'],
  ['mqtt.latitude-longitude-deprecated', ['latitude', 'longitude'], 'Latitude, longitude', '°', 'published', 'The car\'s GPS', 'Only with location sending on; empty topics while it is off.'],
  ['mqtt.location', 'location', 'Position (JSON)', 'JSON', 'published', 'The car\'s GPS, with its accuracy', 'Only with location sending on. Latitude, longitude and gps_accuracy in m.'],
  ['mqtt.shift-state', 'gear', 'Gear', null, 'published', 'Drive inverter (0x118)', 'P, R, N or D. Held while parked, when the bus is quiet.'],
  ['mqtt.power', 'power', 'Battery power', 'kW', 'published', 'Pack volts times amps (0x132)', 'Positive out of the pack, negative while charging or braking.'],
  ['mqtt.speed', 'speed', 'Speed', 'speed', 'published', 'Wheel speed (0x257)', ''],
  ['mqtt.heading', 'heading', 'Heading', '°', 'published', 'GPS heading (0x3D9, chassis bus)', 'Only while driving: the chassis bus is quiet when parked.'],
  ['mqtt.elevation', 'elevation', 'Elevation', 'elevation', 'published', 'The car\'s GPS height (0x3D8)', 'Kept with every drive; from the car, no map lookup.'],
  ['mqtt.locked', 'locked', 'Locked', 'true / false', 'api', 'Tesla API, else TeslaFi (no lock state on CAN)', 'Never from a lock request: a request is not the lock state. Never guessed.'],
  ['mqtt.sentry-mode', 'sentry_mode', 'Sentry mode', 'true / false', 'published', 'Vehicle modes (0x284)', ''],
  ['mqtt.windows-open', 'windows_open', 'Any window open', 'true / false', 'api', 'Tesla API, else TeslaFi', 'Never from window switch presses on CAN: a press cannot tell open from closed.'],
  ['mqtt.window-each', ['window_driver_front', 'window_passenger_front', 'window_driver_rear', 'window_passenger_rear'], 'Each window', 'true / false', 'api', 'Tesla API, else TeslaFi', 'Named by driver and passenger side.'],
  ['mqtt.doors-open', 'doors_open', 'Any door open', 'true / false', 'published', 'Door latches (0x102, 0x103)', ''],
  ['mqtt.door-each', ['door_front_left', 'door_front_right', 'door_rear_left', 'door_rear_right'], 'Each door', 'true / false', 'published', 'Door latches (0x102 left, 0x103 right)', 'Named by side; right-hand drive comes from the build message.'],
  ['mqtt.sun-roof-state', 'sunroof_state', 'Sunroof position', null, 'api', 'Tesla API, else TeslaFi (no roof position on CAN)', 'The car\'s word, like closed or vent. Only cars with an opening roof.'],
  ['mqtt.sun-roof-installed', 'roof', 'Roof type', null, 'published', 'Build message (0x7FF)', 'Metal, fixed glass or panoramic.'],
  ['mqtt.sun-roof-percent-open', 'sunroof_open', 'Sunroof % open', '%', 'api', 'Tesla API, else TeslaFi', ''],
  ['mqtt.trunk-open', 'trunk_open', 'Trunk open', 'true / false', 'published', 'Trunk latch (0x103), liftgate (0x142)', 'A power liftgate also gives its angle.'],
  ['mqtt.frunk-open', 'frunk_open', 'Frunk open', 'true / false', 'published', 'Frunk latch (0x2E1)', ''],
  ['mqtt.is-user-present', 'user_present', 'Someone in the car', 'true / false', 'published', 'Occupancy (0x3B3)', 'A user_present event is sent too when someone is seen.'],
  ['mqtt.is-climate-on', 'climate_on', 'Climate on', 'true / false', 'published', 'Climate status (0x243)', ''],
  ['mqtt.inside-temp', 'inside_temp', 'Inside temperature', 'temperature', 'published', 'Cabin estimate (0x243)', ''],
  ['mqtt.outside-temp', 'outside_temp', 'Outside temperature', 'temperature', 'published', 'Outside sensor (0x2E1, 0x343)', ''],
  ['mqtt.is-preconditioning', ['preconditioning_cabin', 'preconditioning_battery'], 'Preconditioning', 'true / false', 'published', 'Climate status (0x243), battery heating (0x082, 0x3B3)', 'Cabin and battery apart, not one flag.'],
  ['mqtt.odometer', 'odometer', 'Odometer', 'distance', 'published', 'The car\'s odometer (0x3F3, 0x3B6)', 'Only ever grows.'],
  ['mqtt.est-battery-range-km', 'range_estimated', 'Estimated range', 'distance', 'published', 'Energy left over your recent use', 'Sentry Mate\'s own estimate, labelled as such.'],
  ['mqtt.rated-battery-range-km', 'range', 'Rated range', 'distance', 'published', 'The car\'s range (0x33A)', '1 mile steps on the wire.'],
  ['mqtt.ideal-battery-range-km', 'range_ideal', 'Ideal range', 'distance', 'published', 'The car\'s ideal range (0x33A)', ''],
  ['mqtt.battery-level', 'soc', 'Battery level', '%', 'published', 'Charge level as the screen shows it (0x33A)', ''],
  ['mqtt.usable-battery-level', 'soc_usable', 'Usable battery level', '%', 'published', 'Usable energy (0x33A)', 'Lower than the plain level when the pack is cold.'],
  ['mqtt.plugged-in', 'plugged_in', 'Plugged in', 'true / false', 'published', 'Charge cable (0x25D)', 'A cable is in, charging or not; charge_kind still says ac or dc.'],
  ['mqtt.charging-state', 'charge_status', 'Charging status', null, 'published', 'Charge status word (0x212)', 'One of disconnected, no_power, about_to_charge, charging, complete or stopped.'],
  ['mqtt.charge-energy-added', 'charge_energy_added', 'Energy added', 'kWh', 'published', 'The pack\'s lifetime charge counter (0x3D2)', 'The lifetime counter, so it only grows.'],
  ['mqtt.charge-limit-soc', 'charge_limit', 'Charge limit', '%', 'published', 'Charge request (0x333)', ''],
  ['mqtt.charge-port-door-open', 'charge_port_open', 'Charge port open', 'true / false', 'published', 'Charge port (0x25D)', ''],
  ['mqtt.charger-actual-current', 'charger_current', 'Charger current', 'A', 'published', 'AC line current (0x264), DC charger current (0x29D)', 'Sent while charging; also kept on every charge curve.'],
  ['mqtt.charger-phases', 'charger_phases', 'Phases in use', '1 to 3', 'published', 'Charger phases (0x204)', ''],
  ['mqtt.charger-power', 'charge_power', 'Charge power', 'kW', 'published', 'Pack side, volts times amps', 'Always kW.'],
  ['mqtt.charger-voltage', 'charger_voltage', 'Charger voltage', 'V', 'published', 'AC line voltage (0x264), DC charger voltage', 'Sent while charging; also kept on every charge curve.'],
  ['mqtt.charge-current-request', 'charge_current_request', 'Current asked for', 'A', 'published', 'The current set on the screen (0x333)', ''],
  ['mqtt.charge-current-request-max', 'charge_current_limit', 'Current limit', 'A', 'published', 'Charger pilot, cable and car limits (0x21D, 0x13D)', 'Says which of the three applies.'],
  ['mqtt.scheduled-charging-start-time', 'scheduled_charge_start', 'Scheduled charge start', 'ISO time, UTC', 'api', 'Tesla API, else TeslaFi (only on or off flags on CAN)', 'Not sent while no charge is scheduled.'],
  [null, 'scheduled_departure', 'Scheduled departure', 'ISO time, UTC', 'api', 'Tesla API, else TeslaFi', 'Not sent while no departure is set.'],
  ['mqtt.time-to-full-charge', 'time_to_full', 'Time to full', 'min', 'published', 'Time remaining (0x132)', ''],
  ['mqtt.tpms-pressure', ['tire_pressure_fl', 'tire_pressure_fr', 'tire_pressure_rl', 'tire_pressure_rr'], 'Tyre pressures', 'pressure', 'published', 'Tyre sensors (0x219)', 'Front left, front right, rear left, rear right.'],
  ['mqtt.tpms-soft-warning', 'tire_low', 'Tyre pressure low', 'true / false', 'published', 'Pressure against the asked-for pressure (0x219)', ''],
  ['mqtt.active-route', ['route_destination', 'route_distance', 'route_minutes', 'route_traffic_delay'], 'Route (destination, distance, minutes, traffic)', 'route', 'api', 'Tesla API, else TeslaFi (not on CAN)', 'Distance in the server unit, minutes left, minutes lost to traffic. Stale after 10 min.'],
  ['mqtt.active-route-payload-routing', 'energy_at_destination', 'Energy at destination', 'kWh', 'published', 'Trip planning (0x082)', 'While navigating.'],
  ['mqtt.active-route-payload-idle', 'navigating', 'Navigating', 'true / false', 'published', 'Trip planning (0x082), map data (0x238)', 'A plain false with no destination, not an error text.'],
  ['mqtt.center-display-state', 'display_mode', 'Screen mode', null, 'published', 'Display on (0x00C), modes (0x284), keep climate (0x2F3)', 'Named modes: off, on, sentry, dog, camp.'],
  ['mqtt.service-mode', 'service_mode', 'Service mode', 'true / false', 'published', 'Vehicle modes (0x284)', ''],
  [null, 'driving', 'Driving', 'true / false', 'published', 'The timeline', 'Sentry Mate extra.'],
  [null, 'charging', 'Charging', 'true / false', 'published', 'Charge status (0x212)', 'Sentry Mate extra.'],
  [null, 'charge_kind', 'Charger', 'ac / dc', 'published', 'Charger type (0x13D)', 'Sentry Mate extra.'],
  [null, 'energy_remaining', 'Energy remaining', 'kWh', 'published', 'The pack (0x352)', 'Sentry Mate extra.'],
  [null, 'battery_temp', 'Battery temperature', 'temperature', 'published', 'Cell temperatures', 'Sentry Mate extra.'],
  [null, 'lv_voltage', '12 V battery', 'V', 'published', 'Low-voltage battery', 'Sentry Mate extra.'],
  [null, 'updated', 'Last data', 'ISO time, UTC', 'published', 'The newest decoded frame', 'Every set of values carries its time: read it before trusting a value.'],
].map(([item, keys, what, unit, status, from, why]) => ({ item, keys: keys == null ? [] : [].concat(keys), what, unit, status, from, why, inputs: status === 'api' ? API_INPUTS : [] }));

// Home Assistant: what it gets, by group
const GROUPS = [
  ['ha.entity.location-tracker', 'Device tracker', 'The car on the Home Assistant map, home or away by zone', 'location', 'published', 'Only with location sending on. About 75 s behind live.'],
  ['ha.entity.active-route-tracker', 'Destination tracker', 'Where the car navigates to', 'route_destination', 'api', 'The destination\'s name, from the Tesla API or TeslaFi.'],
  ['ha.entity.status-sensors', 'Status sensors', 'State, state since, charging, screen mode, last data', 'state, since, charging, display_mode, updated', 'published', ''],
  [null, 'Software sensors', 'Version, update waiting, its version and download', 'version, update_available, update_version, update_download', 'api', 'Unavailable while stale.'],
  ['ha.entity.config-sensors', 'Build sensors', 'Trim, paint, wheels, spoiler, roof', 'trim, paint, wheels, spoiler, roof', 'published', 'Model and year are on the device already.'],
  ['ha.entity.motion-sensors', 'Motion sensors', 'Place, gear, power, speed, heading, elevation', 'place, gear, power, speed, heading, elevation', 'published', 'Heading only while driving: the chassis bus is quiet when parked.'],
  ['ha.entity.parking-brake', 'In park', 'On while the gear is P', 'in_park', 'published', 'From the gear.'],
  ['ha.entity.temperature-sensors', 'Temperature sensors', 'Inside, outside and battery', 'inside_temp, outside_temp, battery_temp', 'published', 'Battery temperature is extra.'],
  ['ha.entity.range-sensors', 'Range sensors', 'Odometer, rated, ideal and estimated range', 'odometer, range, range_ideal, range_estimated', 'published', 'Estimated range is Sentry Mate\'s own, labelled as such.'],
  ['ha.entity.charging-sensors', 'Charging sensors', 'Battery, usable level, charge status, limit, charge power, charger, current, voltage, phases, current asked for, current limit, time to full, energy remaining', 'soc, soc_usable, charge_status, charge_limit, charge_power, charge_kind, charger_*, charge_current_*, time_to_full, energy_remaining', 'published', 'Current, voltage, phases and time to full while charging.'],
  ['ha.entity.charge-energy-added.counter', 'Energy added counter', 'A running total for Home Assistant\'s Energy view', 'charge_energy_added', 'published', 'Sent as the pack\'s lifetime counter, which never resets.'],
  ['ha.entity.tpms-sensors', 'Tyre sensors', 'Four tyre pressures, pressure low', 'tire_pressure_fl … rr, tire_low', 'published', 'Which sensor is which wheel is still checked on new firmware.'],
  ['ha.entity.active-route-destination', 'Route destination', 'Destination name', 'route_destination', 'api', 'Not on CAN.'],
  ['ha.entity.active-route-energy', 'Route energy', 'Navigating, battery expected on arrival', 'navigating, energy_at_destination', 'published', 'A real 0 shows as 0; unavailable while not navigating.'],
  ['ha.entity.active-route-distance', 'Route distance', 'Distance left', 'route_distance', 'api', 'Not on CAN.'],
  ['ha.entity.active-route-minutes', 'Route minutes', 'Minutes left', 'route_minutes', 'api', 'Not on CAN.'],
  ['ha.entity.active-route-traffic', 'Route traffic', 'Minutes lost to traffic', 'route_traffic_delay', 'api', 'Not on CAN.'],
  ['ha.entity.binary-status', 'Health on/off', 'Logger healthy, update installing, service mode', 'healthy, installing, service_mode', 'published', 'A waiting update is under Software sensors.'],
  ['ha.entity.binary-security', 'Security on/off', 'Doors (any and each), trunk, frunk, Sentry mode, someone in the car', 'doors_open, door_*, trunk_open, frunk_open, sentry_mode, user_present', 'published', 'Lock and windows are under Lock and windows.'],
  [null, 'Lock and windows', 'Locked, any and each window open, sunroof position and % open', 'locked, windows_open, window_*, sunroof_state, sunroof_open', 'api', 'Never from a lock request.'],
  [null, 'Schedule sensors', 'Scheduled charge start and departure', 'scheduled_charge_start, scheduled_departure', 'api', 'Timestamps; unavailable while none is set.'],
  ['ha.entity.binary-climate-charge', 'Climate and charging on/off', 'Climate, preconditioning, cable, charge port', 'climate_on, preconditioning_*, plugged_in, charge_port_open', 'published', ''],
  ['ha.proximity', 'Distance to home', 'Home Assistant\'s own Proximity helper, fed by the tracker', 'location', 'published', 'Needs location sending on.'],
].map(([item, what, gives, topics, status, why]) => ({ item, what, gives, topics, status, why, inputs: status === 'api' ? API_INPUTS : [] }));

const EVENTS = [
  ['drive_started', 'A drive starts', 'drive id, start time and place, battery, range and odometer'],
  ['drive_ended', 'A drive ends', 'the whole drive: distance, time, energy, efficiency, top speed, end place, battery and range, temperatures, climb'],
  ['charge_started', 'A charge starts', 'charge id, ac or dc, place, battery at the start'],
  ['charge_ended', 'A charge ends', 'energy added, battery at the end, battery temperature, cost when the place has a price'],
  ['software_changed', 'Module builds changed', 'the update id and, when known, the version'],
  ['user_present', 'Someone is seen in the car', 'the time and place'],
];

// every host a connection reaches (map pictures: the preset, its overlays and the styles viewers may switch to)
const hostsOf = (r) => (Array.isArray(r.hosts) && r.hosts.length ? r.hosts : r.host ? [r.host] : []);
const ERROR_WORDS = { connection_refused: 'the broker refused the connection', pin_mismatch: 'the certificate did not match the pin', no_host: 'no broker host set', tls_failed: 'TLS failed', auth_failed: 'user name or password refused' };

const PURPOSE = {
  geocoding: ['Address lookup', 'Only when an admin turns online addresses on; positions are rounded first.', 'settings-display'],
  tiles: ['Map pictures', 'Asked by each viewer\'s browser when online tiles are on.', 'settings-display'],
  geonames_download: ['Place-name download', 'Only when an admin asks for new offline place names.', 'settings-display'],
  mqtt: ['MQTT broker', 'Only while sending is on (this page).', null],
  release_check: ['Release check', 'Asks GitHub once a day whether a newer Sentry Mate build is published, when turned on.', 'settings-display'],
  upload: ['CAN Bridge uploads', 'The car\'s CAN Bridge sends its logs here.', null],
  web: ['This app and the API', 'Browsers, apps and the API.', null],
  tesla: ['Tesla API', 'Only once an admin adds a Tesla account (above): its sign-in, Owner API and live stream hosts.', null],
  teslafi: ['TeslaFi API', 'Only while a TeslaFi token is added and not paused: reads your cars\' data from TeslaFi', null],
  teslamate_mqtt: ['Local TeslaMate (MQTT)', 'Only while a local TeslaMate is added and not paused: subscribes to its topics, never publishes', null],
  teslamate_db: ['Local TeslaMate (database)', 'Only while its database catch-up is on: reads what MQTT missed', null],
};

// in the server's units: a broker has one set of units for everyone
function unitText(kind, u) {
  const mi = u?.distance === 'mi';
  switch (kind) {
    case 'distance': return mi ? 'mi' : 'km';
    case 'speed': return mi ? 'mph' : 'km/h';
    case 'temperature': return u?.temperature === 'f' ? '°F' : '°C';
    case 'pressure': return u?.pressure === 'psi' ? 'psi' : 'bar';
    case 'elevation': return mi ? 'ft' : 'm';
    case 'route': return `text, ${mi ? 'mi' : 'km'}, min`;
    default: return kind || '';
  }
}
const slug = (s) => String(s || '').toLowerCase().normalize('NFKD').replace(/[̀-ͯ]/g, '').replace(/[^a-z0-9]+/g, '_').replace(/^_+|_+$/g, '') || 'car';
// a discovery topic is <prefix>/<domain>/<node>/<key>/config; the prefix may have several levels
const haDomain = (t) => { const p = String(t || '').split('/'); return p.length >= 5 ? p[p.length - 4] : null; };
const haPrefix = (t) => { const p = String(t || '').split('/'); return p.length >= 5 ? p.slice(0, -4).join('/') : null; };
const topicOk = (v) => v && !/[+#]/.test(v) && !v.startsWith('$') && v.split('/').every(Boolean);

/* ====================================================================== the page */

export async function mount(ctx) {
  const admin = ctx.admin;
  let cfg, status, net, cars;
  try {
    [cfg, status, net, cars] = await Promise.all([
      admin ? ctx.api.get('settings/mqtt') : null,
      ctx.api.get('settings/mqtt/status').catch(() => null),
      admin ? ctx.api.get('network').catch(() => null) : null,
      ctx.api.get('cars', { sort: 'custom', limit: 100 }),
    ]);
  } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  const rec = recLamp();
  const carList = cars?.items || [];
  const tesla = teslaSection(ctx, { cars: carList });
  const teslafi = teslafiSection(ctx, { cars: carList });
  const teslamate = teslamateSection(ctx, { cars: carList });

  // the preview car: the address's, else the last car, else the first in the person's own order
  const wanted = ctx.query.car || null;
  let car = carList.find((c) => c.id === wanted) || null;
  if (wanted && !car) car = await ctx.api.get(`cars/${enc(wanted)}`).catch(() => null);
  const missing = wanted && !car ? wanted : null;
  if (!car) car = carList.find((c) => c.id === state.lastCar()) || carList[0] || null;
  if (!ctx.alive()) return;
  let pv = null;
  async function loadPreview() {
    if (!car) { pv = null; return; }
    try { pv = await ctx.api.get('settings/mqtt/preview', { car: car.id }); } catch (e) { if (e.name === 'AbortError') throw e; pv = { error: e, messages: [], ha_entities: [] }; }
  }
  try { await loadPreview(); } catch { return; }
  if (!ctx.alive()) return;
  if (missing) ctx.setQuery({ car: car?.id || null });

  const ns = () => cfg?.namespace || (pv?.messages?.[0]?.topic || 'sentrymate/status').replace(/\/(status|cars\/.*)$/, '');
  const units = () => pv?.units || { distance: 'km', temperature: 'c', pressure: 'bar' };
  const cid = () => car?.id || 'c1';

  /* ---------- saving the whole setting with its version */
  const body = (patch) => {
    const b = { ...cfg, ...patch, ha_discovery: { ...cfg.ha_discovery, ...(patch.ha_discovery || {}) } };
    delete b.password_set; delete b.password;
    if ('password' in patch) b.password = patch.password;
    return b;
  };
  async function save(patch, what) {
    try {
      cfg = await ctx.api.put('settings/mqtt', body(patch));
      rec.saved(what);
      await refreshAll();
      return null;
    } catch (e) {
      if (e?.name === 'AbortError') return e;
      if (e.status === 409) { cfg = await ctx.api.get('settings/mqtt'); toast('Someone else changed these settings; this page shows theirs now.', { kind: 'info' }); await refreshAll(); return e; }
      rec.failed(what, e);
      return e;
    }
  }
  async function refreshAll() {
    [status, net] = await Promise.all([ctx.api.get('settings/mqtt/status').catch(() => status), admin ? ctx.api.get('network').catch(() => net) : net]);
    await loadPreview().catch(() => {});
    if (ctx.alive()) paintAll();
  }

  /* ---------- the ON AIR face */
  const airSlot = h('div', { class: 'si-air-wrap' });
  function paintAir() {
    const s = status;
    const on = !!s?.enabled, up = !!s?.connected;
    const word = !s ? 'NO STATUS' : !on ? 'OFF AIR' : up ? 'ON AIR' : 'NO LINK';
    replace(airSlot,
      h('div', { class: ['si-air', on && up && 'is-on', on && !up && 'is-warn'], role: 'status', 'aria-label': `Sending: ${word.toLowerCase()}` },
        h('span', { class: 'si-air-lamp', 'aria-hidden': 'true' }),
        h('span', { class: 'si-air-word' }, word),
        h('span', { class: 'si-air-sub' }, !s ? 'The broker status could not be read.' : !on ? 'Nothing is sent anywhere.' : s.broker ? `${s.broker}${s.tls ? ' · TLS' : ' · no TLS'}` : 'No broker set')),
      s ? counters([
        { label: 'Delivered', value: s.delivered ?? 0, kind: 'count', how: 'confirmed since the server started' },
        { label: 'Waiting', value: s.waiting ?? 0, kind: 'count', how: on ? 'kept until the broker is back' : 'nothing waits while off' },
        { label: 'Dropped', value: s.dropped ?? 0, kind: 'count', how: 'replaced by a newer value' },
      ]) : null,
      s ? h('p', { class: ['si-err', s.last_error ? 'warn' : 'faint'] }, icon(s.last_error ? 'warn' : 'check', 'i--sm'),
        h('span', null, s.last_error ? ['Last error: ', h('b', null, ERROR_WORDS[s.last_error] || s.last_error), `, ${dateTime(s.last_error_ms, ctx.tz)} (${ago(s.last_error_ms)}).`] : 'No error since the server started.')) : null,
      btn('Read the status again', async () => { status = await ctx.api.get('settings/mqtt/status').catch(() => status); if (ctx.alive()) paintAir(); }, { icon: 'reset', sm: true, kind: 'ghost' }));
  }

  /* ---------- the broker */
  const brokerSlot = h('div', { class: 'chans' });
  function paintBroker() {
    const covers = ['docs.env.mqtt.disable-mqtt', 'docs.env.mqtt.mqtt-host', 'docs.env.mqtt.mqtt-port', 'docs.env.mqtt.mqtt-username', 'docs.env.mqtt.mqtt-password', 'docs.env.mqtt.mqtt-tls', 'docs.env.mqtt.mqtt-tls-accept-invalid-certs', 'docs.env.mqtt.mqtt-ipv6', 'docs.env.mqtt.mqtt-namespace'];
    if (!cfg) {
      replace(brokerSlot, chan({ no: 1, title: 'The broker', text: 'Only an admin sees and changes the broker settings. The status, the preview and the events are yours to read.', cover: covers },
        kv([['Sending', status?.enabled ? 'on' : 'off'], ['Broker', status?.broker ? `${status.broker}${status.tls ? ' over TLS' : ' without TLS'}` : 'not set'], ['Topics start with', code(ns())]])));
      return;
    }
    const sw = toggle('si-on', cfg.enabled, 'Send to the broker', async (on, input) => {
      if (on && !cfg.host) { input.checked = false; toast('Set the broker host first and save the connection.', { kind: 'info' }); hostF.input.focus(); return; }
      if (await save({ enabled: on }, on ? 'Sending on' : 'Sending off')) input.checked = cfg.enabled;
    }, { cover: 'docs.env.mqtt.disable-mqtt' });
    const hostF = field({ label: 'Host', name: 'host', value: cfg.host, hint: 'A name such as broker.local. It may lead to IPv4 or IPv6: the connection follows the address, so there is no IPv6 switch.', attrs: { autocomplete: 'off', spellcheck: 'false', placeholder: 'broker.local' } });
    const portF = field({ label: 'Port', name: 'port', type: 'number', value: cfg.port, hint: '8883 is the usual port for MQTT over TLS, 1883 without.', attrs: { min: '1', max: '65535', step: '1' } });
    const userF = field({ label: 'User name', name: 'username', value: cfg.username || '', hint: 'Empty for a broker without accounts.', attrs: { autocomplete: 'off' } });
    const passF = field({ label: 'Password', name: 'password', type: 'password', hint: cfg.password_set ? 'A password is saved. Leave this empty to keep it; it is never shown again.' : 'No password saved. It is only ever sent over TLS.', attrs: { autocomplete: 'new-password', placeholder: cfg.password_set ? '•••••••• (kept)' : '' } });
    const tlsF = field({ label: 'Encrypt the connection (TLS)', name: 'tls', type: 'checkbox', value: cfg.tls, hint: 'On by default. Without TLS no password is sent, and the server refuses to save one.' });
    const pinF = field({ label: 'Pinned certificate (SHA-256)', name: 'pin', path: 'pin_sha256', value: cfg.pin_sha256 || '', hint: 'For a broker with its own (self-signed) certificate: its SHA-256, 64 hex digits, colons allowed. Only that certificate is taken; there is no "accept any certificate" switch.', attrs: { autocomplete: 'off', spellcheck: 'false' } });
    const nsF = field({ label: 'Topic prefix', name: 'namespace', value: cfg.namespace, hint: 'Every topic starts with this. Two servers on one broker use different prefixes. No + or #, not starting with $.', attrs: { autocomplete: 'off', spellcheck: 'false' } });
    for (const [f, c] of [[hostF, 'docs.env.mqtt.mqtt-host docs.env.mqtt.mqtt-ipv6'], [portF, 'docs.env.mqtt.mqtt-port'], [userF, 'docs.env.mqtt.mqtt-username'], [passF, 'docs.env.mqtt.mqtt-password'], [tlsF, 'docs.env.mqtt.mqtt-tls'], [pinF, 'docs.env.mqtt.mqtt-tls-accept-invalid-certs'], [nsF, 'docs.env.mqtt.mqtt-namespace']]) f.el.setAttribute('data-cover', c);
    const testOut = h('div', { class: 'si-test', 'aria-live': 'polite' });
    const testBtn = btn('Test the saved settings', async () => {
      testBtn.disabled = true;
      replace(testOut, h('p', { class: 'faint small' }, 'Connecting…'));
      try {
        const r = await ctx.api.post('settings/mqtt/test');
        if (!ctx.alive()) return;
        replace(testOut, h('p', { class: ['si-test-line', r.ok ? 'ok' : 'warn'] }, icon(r.ok ? 'check' : 'warn', 'i--sm'),
          h('span', null, r.message, r.ok && r.latency_ms != null ? ` ${num(r.latency_ms)} ms.` : '', r.ok && r.topic ? [' Topic ', code(r.topic), '.'] : '', ` ${dateTime(r.at_ms, ctx.tz)}`)));
        status = await ctx.api.get('settings/mqtt/status').catch(() => status);
        paintAir(); paintLog();
      } catch (e) { if (e.name !== 'AbortError') replace(testOut, h('p', { class: 'si-test-line warn' }, icon('warn', 'i--sm'), h('span', null, errText(e)))); } finally { testBtn.disabled = false; }
    }, { icon: 'pulse' });
    const f = form({
      fields: [hostF, portF, userF, passF, tlsF, pinF, nsF], submit: 'Save connection', secondary: testBtn,
      async onSubmit(v) {
        const patch = { host: v.host.trim(), port: v.port === '' ? null : Math.round(Number(v.port)), username: v.username.trim() || null, tls: !!v.tls, pin_sha256: v.pin.trim() || null, namespace: v.namespace.trim() };
        if (v.password) patch.password = v.password;
        let bad = false;
        if (cfg.enabled && !patch.host) { hostF.setError('Sending is on, so the broker needs a host.'); bad = true; }
        if (patch.pin_sha256 && !patch.tls) { pinF.setError('A certificate pin needs TLS.'); bad = true; }
        if ((v.password || cfg.password_set) && !patch.tls) { tlsF.setError('A password is only ever sent over TLS. Keep TLS on, or remove the password.'); bad = true; }
        if (!topicOk(patch.namespace)) { nsF.setError('No + or #, not starting with $, and no empty level.'); bad = true; }
        if (bad) return;
        try { cfg = await ctx.api.put('settings/mqtt', body(patch)); } catch (e) {
          if (e.status === 409) { cfg = await ctx.api.get('settings/mqtt'); toast('Someone else changed these settings; this page shows theirs now.', { kind: 'info' }); await refreshAll(); return; }
          throw e;
        }
        rec.saved('Connection', 'Connection saved. Test it to hear the broker answer.');
        await refreshAll();
      },
    });
    replace(brokerSlot,
      chan({ no: 1, title: 'Sending', text: 'Off until you turn it on: nothing connects anywhere before that. Off stops every message; the broker keeps what it got last.' }, sw.el),
      chan({ no: 2, title: 'Connection', text: 'Host, port, TLS, pin and password are checked together, so they save with the button.', wide: false }, f.el,
        cfg.password_set ? btn('Remove the saved password', async () => {
          if (await confirmDialog('The broker password is deleted from Sentry Mate. Sending then connects without one.', { title: 'Remove the password?', ok: 'Remove', danger: true })) save({ password: null }, 'Password removal');
        }, { kind: 'ghost', icon: 'x', sm: true }) : null, testOut));
  }

  const logSlot = h('div', { class: 'si-log' });
  async function paintLog() {
    if (!admin) { replace(logSlot, note('The broker log (changes and tests) is shown to admins.')); return; }
    let rows = [];
    try {
      const now = ctx.api.now();
      const p = ctx.api.pager('audit', { from_ms: now - 90 * DAY, to_ms: now + 60_000 }, { limit: 200 });
      for (let i = 0; i < 50 && rows.length < 8 && !p.done; i++) {
        const got = (await p.more()) || [];
        if (!ctx.alive()) return;
        rows = rows.concat(got.filter((e) => e.target === 'settings:mqtt'));
      }
      rows = rows.slice(0, 8);
    } catch (e) { if (e.name !== 'AbortError') replace(logSlot, note('The broker log could not be read.')); return; }
    if (!ctx.alive()) return;
    replace(logSlot,
      h('div', { class: 'card-head' }, h('p', { class: 'card-title' }, 'Broker log, last 90 days'),
        rows.length ? csvBtn({ name: ['sentry-mate', 'mqtt-log'], rows, columns: [{ label: 'Time (UTC)', value: (r) => new Date(r.at_ms).toISOString() }, { label: 'Account', value: (r) => r.account }, { label: 'Action', value: (r) => r.action }, { label: 'What happened', value: (r) => r.summary }] }) : null),
      rows.length ? h('ol', { class: 'si-log-list' }, rows.map((r) => h('li', null, h('span', { class: 'si-log-t' }, dateTime(r.at_ms, ctx.tz)), h('span', null, r.summary), h('span', { class: 'faint' }, r.account || '')))) : hint('Nothing changed or tested in the last 90 days.'),
      h('a', { class: 'plc small', href: state.buildUrl('settings-server') }, 'The whole audit log is in Server and access'));
  }

  /* ---------- topics and rules */
  const topicsSlot = h('div', { class: 'stack' });
  function paintTopics() {
    const n = ns(), c = cid();
    const noPlace = field({ label: 'Text when the car is at no saved place', name: 'no_place', path: 'no_place_text', value: cfg?.no_place_text ?? '', hint: 'Sent as the place, up to 40 characters. Empty sends an empty place.', attrs: { maxlength: '40', disabled: !cfg } });
    if (cfg) noPlace.input.addEventListener('change', async () => { const e = await save({ no_place_text: noPlace.input.value.trim() }, 'No-place text'); if (e?.fields) noPlace.setError(e.fields.no_place_text || null); });
    const leaf = (t, what, deep = false) => h('li', { class: ['si-leaf', deep && 'is-deep'] }, h('span', { class: 'si-leaf-t' }, t), h('span', { class: 'si-leaf-w' }, what));
    replace(topicsSlot,
      h('ul', { class: 'si-tree', cover: 'mqtt.topic-scheme', 'aria-label': 'How the topics are laid out' },
        leaf(`${n}/status`, 'online or offline; retained; the broker says offline by itself if the server goes away'),
        leaf(`${n}/cars/${c}/`, 'one car'),
        leaf('<value>', 'one topic per value (below); retained, QoS 1, text in the server units', true),
        leaf('updated', 'the time of the newest data behind the values (ISO, UTC)', true),
        leaf('event', 'one JSON message per event; never retained, so a restart never repeats one', true)),
      h('div', { class: 'cards' },
        h('div', { class: 'card', cover: 'mqtt.note-car-id' }, h('p', { class: 'card-title' }, 'Car ids'),
          h('p', { class: 'card-text' }, 'Each car has Sentry Mate\'s own id (', code(c), ' here), never used again. A new CAN Bridge key for the same car joins that car, so topics never move. The ids are in ', h('a', { class: 'plc', href: state.buildUrl('settings-cars') }, 'Car settings'), '.')),
        h('div', { class: 'card', cover: 'mqtt.note-empty-values' }, h('p', { class: 'card-title' }, 'Missing values'),
          h('p', { class: 'card-text' }, 'A value the car did not send is not sent: the broker keeps the last one, and ', code('updated'), ' says how old it is. Speed and gear are held while parked; read ', code('state'), ' with them.')),
        h('div', { class: 'card', cover: 'docs.env.core.default-geofence' }, h('p', { class: 'card-title' }, 'At no saved place'), noPlace.el),
        h('div', { class: 'card' }, h('p', { class: 'card-title' }, 'Location'),
          cfg ? toggle('si-loc', cfg.publish_location, 'Send the position', async (on, input) => { if (await save({ publish_location: on }, on ? 'Location sending on' : 'Location sending off')) input.checked = cfg.publish_location; }).el
            : h('p', { class: 'card-text' }, 'Location sending is ', h('b', null, pv?.messages?.some((m) => m.topic.endsWith('/location') && m.payload) ? 'on' : 'off'), '.'),
          h('p', { class: 'card-uses' }, 'Off by default. On: latitude, longitude and a JSON position with its accuracy, for maps and arrivals. Turning it off empties those topics.'))));
  }

  /* ---------- the preview car */
  const carSel = h('select', { class: 'input si-car', 'aria-label': 'Car for the preview', onchange: () => pickCar(carSel.value) });
  function fillCars() {
    const list = car && !carList.some((c) => c.id === car.id) ? [car, ...carList] : carList;
    replace(carSel, list.map((c) => h('option', { value: c.id, selected: c.id === car?.id }, c.name || c.id)));
  }
  async function pickCar(next) {
    if (!next || next === car?.id) return;
    car = carList.find((c) => c.id === next) || { id: next, name: next };
    ctx.setQuery({ car: car.id });
    try { await loadPreview(); } catch { return; }
    if (ctx.alive()) { paintTopics(); paintValues(); paintHa(); paintEntities(); paintEvents(); }
  }

  /* ---------- every value */
  const valuesSlot = h('div', { class: 'stack' });
  let valuesList = null;
  function paintValues() {
    const sent = new Map((pv?.messages || []).map((m) => [m.topic.split('/').pop(), m.payload]));
    const u = units();
    const rows = VALUES.map((v) => ({ ...v, shown: v.keys.map((k) => [k, sent.has(k) ? sent.get(k) : undefined]), unitText: v.unit ? unitText(v.unit, u) : '' }));
    const count = Object.fromEntries(ORDER.map((s) => [s, rows.filter((r) => r.status === s).length]));
    const payload = (r) => {
      if (r.status === 'device') return h('span', { class: 'mono' }, r.item === 'mqtt.model' ? pv?.device?.model || '—' : pv?.device?.name || pv?.name || '—');
      const fromApi = r.status === 'api';
      if ((r.status !== 'published' && !fromApi) || !r.keys.length) return h('span', { class: 'faint' }, '—');
      // from an input: say so when none of the inputs that can give it is live for this car
      const live = Array.isArray(pv?.inputs) ? pv.inputs.map((i) => i.kind) : null;
      if (fromApi && live && !r.inputs.some((k) => live.includes(k))) return h('span', { class: 'faint' }, 'No input gives this now');
      const srcOf = (k) => (fromApi && pv?.sources?.[k]) || null;
      if (!r.shown.some(([k, p]) => p !== undefined || srcOf(k))) return h('span', { class: 'faint' }, 'Not sent by this car yet');
      const many = r.shown.length > 1;
      const short = (k) => (/^(window|route)_/.test(k) ? k.replace(/^(window|route)_/, '').replace(/_/g, ' ') : k.replace(/^.*_/, ''));
      const srcLine = (src) => h('span', { class: ['si-src', src.stale && 'is-stale'] }, src.label || INPUT_NAME[src.kind] || src.kind, src.at_ms ? [' · ', ago(src.at_ms)] : null,
        src.stale ? h('b', { class: 'si-src-stale' }, ' · STALE') : null);
      // several topics read together from one input: where they came from is said once
      const srcs = r.keys.map(srcOf);
      const one = many && srcs.every((x) => x && x.kind === srcs[0].kind && x.stale === srcs[0].stale && x.at_ms === srcs[0].at_ms) ? srcs[0] : null;
      return h('span', { class: 'si-payloads' }, r.shown.map(([k, p]) => [h('span', { class: 'mono' }, many ? h('span', { class: 'faint' }, `${short(k)} `) : null,
        p === undefined ? '—' : p === '' ? h('span', { class: 'faint' }, '(empty)') : p.length > 60 ? `${p.slice(0, 57)}…` : p), !one && srcOf(k) ? srcLine(srcOf(k)) : null]),
      one ? srcLine(one) : null);
    };
    valuesList = tracks(ctx, {
      title: `Every value for ${pv?.name || car?.name || 'this car'}`, icon: 'antenna', rows, csvName: `mqtt-values-${cid()}`, label: 'Values sent to the broker',
      note: `Payloads as a broker gets them now, as text in the server units (${unitText('distance', u)}, ${unitText('temperature', u)}, ${unitText('pressure', u)}): MQTT has one set of units for everyone. Sort by Status to group them.`,
      side: h('span', { class: 'faint small' }, `${count.published} sent · ${count.api} from an API · ${count.logged} kept · ${count.signal} need a signal · ${count.none + count.never} left out`),
      sort: { key: 'status', dir: 'asc' },
      columns: [
        { key: 'what', label: 'Value', lead: true, sort: true, value: (r) => r.what, render: (r) => h('span', { class: 'cell', cover: r.item || null }, h('b', null, r.what), r.item ? null : h('span', { class: 'cell-sub' }, 'Sentry Mate extra')), csv: (r) => r.what },
        { key: 'topic', label: 'Topic', sort: true, value: (r) => r.keys[0] || '', render: (r) => (r.keys.length ? h('span', { class: 'si-topics' }, r.keys.map((k) => code(k, { long: true }))) : h('span', { class: 'faint' }, 'no topic')), csv: (r) => r.keys.map((k) => `${ns()}/cars/${cid()}/${k}`).join(' ') },
        { key: 'payload', label: 'Payload now', render: payload, csv: (r) => (r.status === 'published' || r.status === 'api' ? r.shown.map(([, p]) => p ?? '').join(' ') : '') },
        { key: 'unit', label: 'Unit', cls: 'nowrap', render: (r) => r.unitText || h('span', { class: 'faint' }, 'text'), csv: (r) => r.unitText },
        { key: 'status', label: 'Status', cls: 'si-status', sort: true, value: (r) => ORDER.indexOf(r.status), render: (r) => statusTag(r.status, r.inputs), csv: (r) => statusWord(r.status, r.inputs) },
        { key: 'from', label: 'Comes from', sort: true, value: (r) => r.from, render: (r) => h('span', { class: 'cell' }, h('span', null, r.from), r.why ? h('span', { class: 'cell-sub' }, r.why) : null), csv: (r) => `${r.from}${r.why ? `. ${r.why}` : ''}` },
      ],
    });
    replace(valuesSlot,
      missing ? note(`The car "${missing}" in the address does not exist or is not visible to this account, so the preview shows ${car?.name || 'another car'}.`) : null,
      !car ? emptyState({ icon: 'car', title: 'No car to preview', text: 'A car appears once its CAN Bridge uploads its first log. The list below still says what each value is.' })
        : pv?.error ? emptyState({ icon: 'warn', title: 'The preview could not be made', text: errText(pv.error) }) : null,
      valuesList.el);
  }

  /* ---------- Home Assistant discovery */
  const haSlot = h('div');
  function paintHa() {
    const ents = pv?.ha_entities || [];
    const ha = cfg?.ha_discovery || { enabled: ents.length > 0, prefix: haPrefix(ents[0]?.config_topic) || 'homeassistant', url: null };
    const ro = !cfg;
    const sw = toggle('si-ha-on', ha.enabled, 'Home Assistant discovery', async (on, input) => { if (await save({ ha_discovery: { enabled: on } }, on ? 'Discovery on' : 'Discovery off')) input.checked = cfg.ha_discovery.enabled; }, { disabled: ro });
    const prefixF = field({ label: 'Discovery prefix', name: 'ha_prefix', path: 'ha_discovery.prefix', value: ha.prefix, hint: 'The same as Home Assistant\'s own MQTT discovery prefix: homeassistant unless you changed it there.', attrs: { disabled: ro, spellcheck: 'false', autocomplete: 'off' } });
    const urlF = field({ label: 'Link on the device page (https)', name: 'ha_url', path: 'ha_discovery.url', value: ha.url || '', hint: 'This server\'s address as Home Assistant users reach it. Each car\'s device then links to its Car overview. Empty: no link.', attrs: { disabled: ro, placeholder: `https://${location.host}`, spellcheck: 'false', autocomplete: 'off' } });
    if (!ro) {
      prefixF.input.addEventListener('change', async () => {
        const v = prefixF.input.value.trim();
        if (!topicOk(v)) { prefixF.setError('No + or #, not starting with $, and no empty level.'); return; }
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
    const node = d?.identifiers?.[0] || `${ns().replace(/[^A-Za-z0-9_-]/g, '_')}_${cid()}`;
    const sentKeys = new Set((pv?.messages || []).filter((m) => m.payload !== '').map((m) => m.topic.split('/').pop()).filter((k) => !['status', 'latitude', 'longitude'].includes(k)));
    const withEntity = new Set(ents.map((e) => String(e.state_topic || '').split('/').pop()));
    const lacking = [...sentKeys].filter((k) => !withEntity.has(k));
    replace(haSlot, h('div', { class: 'si-ha' },
      h('div', { class: 'chans' },
        chan({ no: 3, title: 'Discovery', text: 'On: each car shows up in Home Assistant by itself, with every sensor and no YAML to write. Needs sending on.', cover: 'ha.discovery.enable' }, sw.el, ro ? hint('An admin changes this.') : null),
        chan({ no: 4, title: 'Prefix', text: 'Where the discovery messages go.', cover: 'ha.discovery.prefix' }, prefixF.el),
        chan({ no: 5, title: 'Device link', text: 'Optional.', cover: 'ha.discovery.url' }, urlF.el),
        chan({ no: 6, title: 'Clean-up', text: 'Nothing is remembered: at each such change, and again at each start, removals go out for whatever should not be there.', cover: ['ha.discovery.removed-car-cleanup', 'ha.discovery.disabled-cleanup', 'ha.discovery.cleanup-policy', 'ha.discovery.limits-note'] },
          h('ul', { class: 'si-rules' },
            h('li', { cover: 'ha.discovery.removed-car-cleanup' }, h('b', null, 'A car deleted or renamed. '), 'Deleting a car (or taking it off sending) removes its device and every entity at once. A new name re-sends the device; entity ids stay.'),
            h('li', { cover: 'ha.discovery.disabled-cleanup' }, h('b', null, 'Discovery turned off. '), 'Every car\'s device and entities go from Home Assistant the moment you save, not at the next restart. The plain topics stay.'),
            h('li', { cover: ['ha.discovery.cleanup-policy', 'ha.discovery.limits-note'] }, h('b', null, 'A clean-up cut short '), 'is simply sent again. A new prefix sends removals under the old one first, so nothing is left behind there.')))),
      h('div', { class: 'stack' },
        h('div', { class: 'card', cover: ['ha.discovery.device-payload', 'ha.device.identity'] },
          h('p', { class: 'card-title' }, 'The device Home Assistant gets'),
          kv([['Name', d?.name || pv?.name || '—'], ['Maker', 'Tesla'], ['Model', d?.model || '—'], ['Identifier', code(node)],
            ['Link', ha.url ? code(`${ha.url.replace(/\/$/, '')}/cars/${cid()}`, { long: true }) : h('span', { class: 'faint' }, 'none')], ['Entities', String(ents.length)]]),
          h('p', { class: 'card-uses' }, 'One device per car, holding all of its entities. Two servers with different prefixes on one broker never share a device.')),
        h('div', { class: 'card', cover: 'ha.entity.id-scheme' }, h('p', { class: 'card-title' }, 'Entity ids'),
          h('p', { class: 'card-text' }, 'Each entity\'s hidden unique id is the identifier and the value: ', code(`${node}_soc`, { long: true }), '. Home Assistant names the visible id after the device: ', code(`sensor.${slug(d?.name || pv?.name || 'car')}_battery`, { long: true }), '. Several cars never clash.')),
        h('div', { class: 'card', cover: 'ha.discovery.coverage-gap' }, h('div', { class: 'card-head' }, h('p', { class: 'card-title' }, 'An entity for every value'), lacking.length ? tag(`${lacking.length} missing`, 'org') : tag('All there', 'grn')),
          h('p', { class: 'card-text' }, ents.length ? (lacking.length ? `${withEntity.size} of the ${sentKeys.size} values sent for this car have an entity; missing: ${lacking.join(', ')}.` : `All ${sentKeys.size} values sent for this car have an entity (the position through the device tracker).`) : 'Discovery is off, so Home Assistant gets no entities; the topics are still sent.'),
          h('p', { class: 'card-uses' }, 'The rule: every value topic gets an entity. A value is missing in Home Assistant only when it is missing from MQTT too.')))));
  }

  /* ---------- Home Assistant entities and cards */
  const entSlot = h('div', { class: 'stack' });
  function paintEntities() {
    const ents = pv?.ha_entities || [];
    const groups = tracks(ctx, {
      title: 'What Home Assistant gets', icon: 'home', rows: GROUPS, csvName: 'home-assistant-groups', label: 'Home Assistant entity groups',
      note: 'Read-only entities, one per value sent. A group that needs a new CAN signal shows up as soon as Sentry Mate decodes it.',
      columns: [
        { key: 'what', label: 'Group', lead: true, sort: true, value: (r) => r.what, render: (r) => h('b', { cover: r.item }, r.what) },
        { key: 'gives', label: 'Entities' },
        { key: 'topics', label: 'From topics', render: (r) => (r.topics ? code(r.topics, { long: true }) : h('span', { class: 'faint' }, '—')) },
        { key: 'status', label: 'Status', cls: 'si-status', sort: true, value: (r) => ORDER.indexOf(r.status), render: (r) => statusTag(r.status, r.inputs), csv: (r) => statusWord(r.status, r.inputs) },
        { key: 'why', label: 'Notes' },
      ],
    });
    const list = tracks(ctx, {
      title: `Entities for ${pv?.name || car?.name || 'this car'}`, icon: 'antenna', rows: ents, csvName: `home-assistant-entities-${cid()}`, label: 'Home Assistant entities',
      note: 'Exactly what discovery sends now. Units are the server units, named to Home Assistant so it never mixes them up.',
      sort: { key: 'name', dir: 'asc' },
      columns: [
        { key: 'name', label: 'Name', lead: true, sort: true, render: (r) => h('b', null, r.name) },
        { key: 'id', label: 'Unique id', sort: true, render: (r) => code(r.id, { long: true }) },
        { key: 'unit', label: 'Unit', render: (r) => r.unit || h('span', { class: 'faint' }, '—') },
        { key: 'device_class', label: 'Kind', sort: true, render: (r) => r.device_class || h('span', { class: 'faint' }, '—') },
        { key: 'state_topic', label: 'State topic', render: (r) => code(r.state_topic, { long: true }) },
        { key: 'config_topic', label: 'Discovery topic', render: (r) => code(r.config_topic, { long: true }) },
      ],
      empty: () => emptyState({ icon: 'home', title: 'No entities', text: cfg && !cfg.ha_discovery.enabled ? 'Discovery is off. Turn it on above and every entity listed here goes to Home Assistant.' : 'There is no car to preview yet.' }),
    });
    const name = slug(pv?.name || car?.name || 'car');
    const eid = (key) => { const e = ents.find((x) => String(x.state_topic).endsWith(`/${key}`)); return e ? `${haDomain(e.config_topic) || 'sensor'}.${name}_${slug(e.name)}` : null; };
    const tracker = ents.find((e) => haDomain(e.config_topic) === 'device_tracker');
    const all = ents.filter((e) => !['event', 'device_tracker'].includes(haDomain(e.config_topic))).map((e) => `${haDomain(e.config_topic) || 'sensor'}.${name}_${slug(e.name)}`);
    const title = pv?.name || car?.name || 'Car';
    const yGlance = ents.length ? ['type: glance', `title: ${title}`, 'entities:', ...['soc', 'state', 'charging', 'plugged_in', 'gear', 'speed', 'range', 'sentry_mode', 'doors_open', 'inside_temp', 'updated'].map(eid).filter(Boolean).map((x) => `  - ${x}`)].join('\n') : '';
    const yMap = tracker ? ['type: map', 'dark_mode: true', 'hours_to_show: 24', 'entities:', `  - device_tracker.${name}_location`].join('\n') : '';
    const yList = ents.length ? ['type: entities', `title: ${title}`, 'entities:', ...all.map((x) => `  - ${x}`), ...(tracker ? ['  - entity: proximity.home', '    name: Distance to home'] : [])].join('\n') : '';
    const card = (t, text, yaml, item, none) => h('div', { class: 'card', cover: item },
      h('div', { class: 'card-head' }, h('p', { class: 'card-title' }, t), yaml ? copyBtn(yaml, 'Copy YAML') : null),
      h('p', { class: 'card-text' }, text),
      yaml ? h('pre', { class: 'pre' }, yaml) : h('p', { class: 'pre pre--empty' }, none));
    replace(entSlot, groups.el, list.el,
      hud({ title: 'Dashboard cards to paste', icon: 'grid' },
        h('div', { class: 'stack' },
          note('Made from the entities above, for a Home Assistant dashboard (Edit dashboard, Add card, Manual). The visible ids follow the car\'s name; if you renamed an entity in Home Assistant, change it here too.'),
          h('div', { class: 'cards' },
            card('At a glance', 'The facts looked at most, including plugged in, Sentry mode and doors.', yGlance, 'ha.lovelace.glance', 'Turn discovery on to get this card.'),
            card('Map', 'Where the car is, on Home Assistant\'s map.', yMap, 'ha.lovelace.map', 'Needs location sending on (Topics and rules).'),
            card('Everything', 'One list of every entity, and the distance to home when the position is sent.', yList, 'ha.lovelace.entity-list', 'Turn discovery on to get this card.')),
          h('p', { class: 'note', cover: 'ha.proximity' }, icon('info', 'i--sm'), h('span', null, 'Distance to home and the direction of travel come from Home Assistant\'s own Proximity helper (Settings, Devices and services, Helpers) with the car\'s device tracker. It needs location sending on and knows of an arrival about 75 s late.')))));
  }

  /* ---------- events */
  const eventsSlot = h('div');
  let eventsList = null;
  const eventTopic = h('span');
  function paintEvents() {
    replace(eventTopic, code(`${ns()}/cars/${cid()}/event`, { long: true }));
    if (!car) { eventsList = null; replace(eventsSlot, emptyState({ icon: 'clock', title: 'No car yet', text: 'Events show here once a car has uploaded logs.' })); return; }
    const now = ctx.api.now();
    const fold = (p) => {
      const parts = Object.entries(p || {}).filter(([k]) => !['event_type', 'car', 'units'].includes(k)).map(([k, v]) => `${k}: ${v == null ? '—' : typeof v === 'object' ? JSON.stringify(v) : v}`);
      if (parts.length <= 4) return h('span', { class: 'mono si-json' }, parts.join(' · '));
      return h('span', { class: 'mono si-json', title: parts.join('\n') }, parts.slice(0, 4).join(' · '), h('span', { class: 'faint' }, ` +${parts.length - 4} more (CSV has all)`));
    };
    eventsList = tracks(ctx, {
      title: 'Recent events', icon: 'clock', path: `cars/${enc(car.id)}/events`, params: { from_ms: now - 30 * DAY, to_ms: now + 60_000, types: EVENTS.map((e) => e[0]).join(',') }, limit: 12,
      csvName: `events-${car.id}`, label: 'Recent events',
      note: 'The last 30 days, newest first, as a subscriber to the event topic got them (positions only with location sending on).',
      columns: [
        { key: 'at', label: 'When', lead: true, render: (r) => h('span', { class: 'nowrap' }, dateTime(r.at_ms, ctx.tz)), csv: (r) => new Date(r.at_ms).toISOString() },
        { key: 'type', label: 'Event', render: (r) => code(r.type), csv: (r) => r.type },
        { key: 'payload', label: 'Payload', render: (r) => fold(r.payload), csv: (r) => JSON.stringify(r.payload ?? {}) },
      ],
      empty: () => emptyState({ icon: 'clock', title: 'No events in the last 30 days', text: 'This car did not drive, charge or change software in that time.' }),
    });
    replace(eventsSlot, eventsList.el);
  }
  const idea = (t, item, uses, text, s) => h('div', { class: 'card', cover: item },
    h('div', { class: 'card-head' }, h('p', { class: 'card-title' }, t), statusTag(s)),
    h('p', { class: 'card-text' }, text), h('p', { class: 'card-uses' }, 'Uses ', uses.map((u, i) => [i ? ', ' : '', code(u)]), '.'));

  /* ---------- network */
  const netSlot = h('div');
  function paintNet() {
    if (!net) { replace(netSlot, note(admin ? 'The list of connections could not be read.' : 'The list of connections is shown to admins. By default Sentry Mate makes no outbound connection at all.')); return; }
    const rows = [
      ...net.outbound.map((o) => ({ dir: 'out', ...o })),
      ...net.inbound.map((i) => ({ dir: 'in', purpose: i.purpose, host: null, enabled: true, last_used_ms: null, port: i.port, tls: i.tls })),
    ];
    replace(netSlot, tracks(ctx, {
      title: 'Every connection', icon: 'globe', rows, csvName: 'network-connections', label: 'Network connections',
      note: `For a firewall: ${net.outbound.filter((o) => o.enabled).length} of ${net.outbound.length} outbound connections are on now; one that is off makes no request. Inbound, only these ports are open.`,
      columns: [
        { key: 'dir', label: 'Way', sort: true, render: (r) => tag(r.dir === 'in' ? 'Inbound' : 'Outbound', r.dir === 'in' ? 'cyan' : null), csv: (r) => r.dir },
        { key: 'purpose', label: 'For', lead: true, sort: true, value: (r) => PURPOSE[r.purpose]?.[0] || r.purpose, render: (r) => h('span', { class: 'cell' }, h('b', null, PURPOSE[r.purpose]?.[0] || r.purpose), h('span', { class: 'cell-sub' }, PURPOSE[r.purpose]?.[1] || '')), csv: (r) => PURPOSE[r.purpose]?.[0] || r.purpose },
        { key: 'host', label: 'Host or port', render: (r) => (r.port != null ? code(`port ${r.port}${r.tls ? ', TLS' : ''}`) : hostsOf(r).length ? h('span', { class: 'si-hosts' }, hostsOf(r).map((x) => code(x, { long: true }))) : h('span', { class: 'faint' }, 'not set')), csv: (r) => hostsOf(r).join(' ') || (r.port != null ? `port ${r.port}` : '') },
        { key: 'on', label: 'State', sort: true, value: (r) => (r.enabled ? 0 : 1), render: (r) => (r.enabled ? sig('ok', r.dir === 'in' ? 'Open' : 'On') : sig('off', 'Off')), csv: (r) => (r.enabled ? 'on' : 'off') },
        { key: 'last', label: 'Last used', num: true, sort: true, value: (r) => r.last_used_ms, render: (r) => (r.last_used_ms ? ago(r.last_used_ms) : h('span', { class: 'faint' }, r.dir === 'in' ? 'always listening' : 'never')), csv: (r) => (r.last_used_ms ? new Date(r.last_used_ms).toISOString() : '') },
        { key: 'change', label: 'Change it', csv: false, render: (r) => (PURPOSE[r.purpose]?.[2] ? h('a', { class: 'plc', href: state.buildUrl(PURPOSE[r.purpose][2]) }, 'Settings') : r.purpose === 'teslafi' ? h('a', { class: 'plc', href: state.buildUrl('settings-integrations') + '#teslafi' }, 'TeslaFi') : r.purpose === 'teslamate_mqtt' || r.purpose === 'teslamate_db' ? h('a', { class: 'plc', href: '#teslamate' }, 'Local TeslaMate') : r.purpose === 'tesla' ? h('a', { class: 'plc', href: '#tesla' }, 'Tesla account') : h('span', { class: 'faint' }, r.purpose === 'mqtt' ? 'Above' : '—')) },
      ],
    }).el);
  }

  /* ---------- put it together */
  function paintAll() { paintAir(); paintBroker(); paintTopics(); paintValues(); paintHa(); paintEntities(); paintEvents(); paintNet(); }
  fillCars();
  paintAll();
  const works = (t, item, text, uses) => h('div', { class: 'card', cover: item }, h('p', { class: 'card-title' }, t), h('p', { class: 'card-text' }, text), h('p', { class: 'card-uses' }, 'Uses ', uses));

  replace(ctx.view, h('div', { class: 'page si' },
    plate({ kicker: 'Set-up · integrations', title: 'Integrations', aim: ctx.info.aim, tools: rec.el }),
    h('section', { class: 'hud hud--plain jump', 'aria-label': 'Sections and saving' },
      presets([['tesla', 'Tesla account'], ['teslafi', 'TeslaFi'], ['teslamate', 'Local TeslaMate'], ['si-broker', 'Broker'], ['si-topics', 'Topics'], ['si-values', 'Values'], ['si-ha', 'Home Assistant'], ['si-events', 'Events'], ['si-api', 'API'], ['si-network', 'Network'], ['si-works', 'Works with']]),
      h('p', { class: 'note jump-note' }, icon('check', 'i--sm'), h('span', null, 'Switches and single fields save at once; the connection saves with its button. Nothing leaves the server until sending is on.'))),

    band(1, 'Live inputs: Tesla API, TeslaFi and local TeslaMate'),
    tesla.el,
    teslafi.el,
    teslamate.el,

    band(2, 'Transmitter'),
    hud({ title: 'MQTT broker', icon: 'antenna', id: 'si-broker', side: tag(admin ? 'Admin' : 'Read only', admin ? 'mag' : 'org') },
      h('div', { class: 'si-broker' }, brokerSlot,
        h('div', { class: 'stack' }, airSlot, logSlot,
          h('div', { class: 'card', cover: 'nr.setup.mqtt' }, h('p', { class: 'card-title' }, 'Connect a client'),
            h('p', { class: 'card-text' }, 'Node-RED, Home Assistant or any MQTT client uses the same broker. Subscribe to ', code(`${ns()}/cars/+/#`, { long: true }), ' for every car, or ', code(`${ns()}/cars/<car id>/#`, { long: true }), ' for one. Use TLS as here.'))))),
    hud({ title: 'Topics and rules', icon: 'route', id: 'si-topics' }, topicsSlot),

    band(3, 'Values sent', h('label', { class: 'si-car-pick' }, h('span', { class: 'faint small' }, 'Preview for'), carSel)),
    h('section', { id: 'si-values', class: 'stack', 'aria-label': 'Values sent' },
      h('div', { class: 'hud hud--plain si-legend' }, h('p', { class: 'small' }, 'Every value Sentry Mate sends, before you turn anything on. The VIN is never one of them; the position only when you opt in.'),
        h('dl', { class: 'si-legend-list' }, ORDER.map((k) => h('div', null, h('dt', null, statusTag(k)), h('dd', null, STATUS[k][2]))))),
      valuesSlot),

    band(4, 'Home Assistant'),
    hud({ title: 'Home Assistant discovery', icon: 'home', id: 'si-ha' }, haSlot),
    entSlot,

    band(5, 'Events'),
    hud({ title: 'Events for automations', icon: 'pulse', id: 'si-events' },
      h('div', { class: 'stack' },
        h('div', { class: 'si-two' },
          h('div', { class: 'stack', cover: 'nr.overview' },
            h('p', null, 'Automations should react to what happened, not to a value changing. Sentry Mate sends one message per event on ', eventTopic, '. It is never retained, so a restart of the server, the broker or Home Assistant never fires an automation again.'),
            hint('Any tool that reads MQTT can use them. Sentry Mate only sends; it never takes commands, and nothing here can control the car.'),
            h('ul', { class: 'si-evts', 'aria-label': 'Event types' }, EVENTS.map(([t, w, c]) => h('li', null, code(t), h('span', null, h('b', null, w), ` · ${c}`))))),
          eventsSlot),
        h('p', { class: 'note' }, icon('clock', 'i--sm'), h('span', null, 'Values reach Sentry Mate about 75 s after the car sent them (one-minute logs and the upload): fine for reminders and logs, too late to open a door as the car pulls up.')),
        h('div', { class: 'cards' },
          idea('Garage door', 'ha.automation.garage', ['drive_ended', 'end_place'], 'Close the garage door, or note the arrival, when a drive ends at your home place. Opening it on arrival is better left to the car itself.', 'published'),
          idea('Left open', 'ha.automation.left-open', ['doors_open', 'trunk_open', 'frunk_open'], 'Remind after five minutes with a door, the trunk or the frunk open, and every five minutes after. Add windows_open where the Tesla API or TeslaFi gives it.', 'published'),
          idea('Stop the reminder', 'ha.automation.left-open-cancel', ['doors_open', 'trunk_open', 'frunk_open'], 'End the repeat as soon as everything reads closed again.', 'published'),
          idea('Plug in tonight', 'ha.automation.plug-in-reminder', ['plugged_in', 'soc', 'range'], 'An evening reminder when the car is home and not plugged in.', 'published'),
          idea('State changed', 'nr.notify.state-change', ['drive_started', 'drive_ended', 'state'], 'A message when a drive starts or ends; drive_ended carries the whole drive.', 'published'),
          idea('Someone got in', 'nr.notify.driver-present', ['user_present'], 'A message when someone is seen in the car. The event and the user_present topic both say so.', 'published'),
          idea('Time to full', 'nr.notify.time-to-full', ['time_to_full', 'charge_power', 'soc'], 'Progress while charging.', 'published'),
          idea('Arrived, left', 'nr.notify.geofence', ['place', 'start_place', 'end_place'], 'A message when the car leaves one saved place or reaches another.', 'published')))),

    band(6, 'Outside'),
    hud({ title: 'API access', icon: 'server', id: 'si-api' },
      h('div', { class: 'cards' },
        h('div', { class: 'card', cover: 'faq.public-api' }, h('p', { class: 'card-title' }, 'What you can build on'),
          h('p', { class: 'card-text' }, 'Two things stay stable: the MQTT topics on this page, and the read API under ', code(`${location.origin}/api/v1/`, { long: true }), '. Every UI pack of this app is built on that same API. The storage behind them is sealed: never read it directly.')),
        h('div', { class: 'card', cover: 'projects.teslamateapi' }, h('p', { class: 'card-title' }, 'A JSON API, built in'),
          h('p', { class: 'card-text' }, 'Cars, live values, drives, charges, positions, places and every report, as JSON or CSV. For example ', code('GET /api/v1/cars'), ' lists the cars you may see, and ', code('GET /api/v1/cars/<id>/drives', { long: true }), ' their drives.')),
        h('div', { class: 'card' }, h('p', { class: 'card-title' }, 'Who sees what'),
          h('p', { class: 'card-text' }, 'The API signs in like this app and answers for that account only: a watch account sees just its own cars. Make an account per app or person in ', h('a', { class: 'plc', href: state.buildUrl('settings-server') }, 'Server and access'), '.')),
        h('div', { class: 'card', cover: 'projects.custom-dashboards' }, h('p', { class: 'card-title' }, 'More analysis, already here'),
          h('p', { class: 'card-text' }, 'Costs, battery health, charging by place and every gap in the record are pages of this app: ',
            h('a', { class: 'plc', href: state.buildUrl('statistics', car ? { car: car.id } : {}) }, 'Statistics'), ', ', h('a', { class: 'plc', href: car ? state.buildUrl('battery', { car: car.id }) : '/' }, 'Battery'), ', ', h('a', { class: 'plc', href: state.buildUrl('charges', car ? { car: car.id } : {}) }, 'Charges'), '. Each list and chart downloads as CSV.')),
        h('div', { class: 'card', cover: 'projects.matedroid' }, h('p', { class: 'card-title' }, 'On a phone'),
          h('p', { class: 'card-text' }, 'This app works in a phone\'s browser and can be put on the home screen. A phone app of your own can use the same API.')))),
    h('section', { id: 'si-network', class: 'stack', 'aria-label': 'Network use', cover: 'faq.network-flows' },
      h('div', { class: 'hud hud--plain si-legend' }, h('p', { class: 'small' }, 'Sentry Mate needs no Tesla account and no cloud: the car\'s CAN Bridge uploads to it, and everything else is off until you turn it on. The Tesla API connects only once an admin adds a Tesla account.')),
      netSlot),
    hud({ title: 'Works with', icon: 'share', id: 'si-works' },
      h('div', { class: 'stack' },
        h('p', { class: 'note', cover: 'projects.disclaimer' }, icon('info', 'i--sm'), h('span', null, 'Other people\'s tools, named so you know what fits. Sentry Mate does not make, check or vouch for them. Anything built on the topics or the API keeps working; anything that reads the storage directly will break.')),
        h('div', { class: 'cards' },
          works('Route planners', 'projects.abrp', 'A planner that takes the live battery level plans with the real charge. Sentry Mate never sends to one by itself: a small MQTT bridge of yours forwards the values, if you want that.', [code('soc'), ', ', code('location'), ', ', code('speed'), ', ', code('power'), '; about 75 s behind live.']),
          works('Home Assistant Tesla add-ons', 'projects.ha-tesla', 'Integrations that can read a car\'s values from MQTT instead of asking Tesla\'s servers can use these topics.', [code(`${ns()}/cars/<id>/#`, { long: true }), '.']),
          works('Desk gauges and lights', 'projects.gaussmeter', 'A light or model on the desk whose colour follows the state and whose level follows the battery.', [code('state'), ' and ', code('soc'), '.']),
          works('Smart mirrors and wall screens', 'projects.magic-mirror', 'A status tile on a mirror or wall screen, from MQTT or the API.', [code('soc'), ', ', code('range'), ', ', code('state'), ', ', code('updated'), '.']),
          works('Garage door openers', 'projects.geogdo', 'Openers that act on arrival and departure, by zone or saved place. Values are about 75 s late: good for closing behind you, late for opening.', [code('location'), ', ', code('place'), ' and drive events.']))))));

  await Promise.all([valuesList?.ready, eventsList?.ready, paintLog(), tesla.load(), teslafi.load(), teslamate.load()].filter(Boolean));
  if (ctx.alive()) ctx.ready();
}

