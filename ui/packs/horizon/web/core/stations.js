// The stations: every page of the app, in the order the radio tunes through them, and the six bands
// (menu groups) they sit in. The dashboard strip, the phone tuner, the scan box and the router all read
// this one list. Page modules live in pages/<id>.js.
//
//   id       page id (docs/parity/coverage-matrix.json "pages", plus sign-in and setup)
//   name     the page's name: radio display, heading, tab title
//   short    the name on a tuner chip or a band menu (defaults to name)
//   band     cars | driving | charging | energy | reports | setup; null: a detail page, not tuned to
//   icon     sprite icon id (index.html, <symbol id="i-...">)
//   line     one line under the name in a band menu and the scan box
//   aim      the line under the page heading
//   words    extra words the scan box matches
//   car      'needs' (the route has {car}), 'either' (with or without one), null (never)
//   period   the default period when the address and the tab have none; null: no period
//   parent   the list a detail page belongs to
//   parts    what the page holds (the page finder searches these words too)

export const BANDS = Object.freeze([
  { id: 'cars', name: 'Cars', icon: 'car', side: 'left' },
  { id: 'driving', name: 'Driving', icon: 'route', side: 'left' },
  { id: 'charging', name: 'Charging', icon: 'bolt', side: 'left' },
  { id: 'energy', name: 'Energy', full: 'Battery and energy', icon: 'battery', side: 'right' },
  { id: 'reports', name: 'Reports', icon: 'stats', side: 'right' },
  { id: 'setup', name: 'Set-up', icon: 'sliders', side: 'right' },
]);

export const STATIONS = Object.freeze([
  { id: 'fleet', name: 'All cars', band: 'cars', icon: 'grid', car: null, period: null,
    line: 'Every car on one board, and which one needs you.',
    aim: 'Every car at a glance: what each is doing, and the ones that need a look.',
    words: 'fleet home list attention state order pin',
    parts: ['Cars', 'Needs a look', 'Totals', 'No cars yet', 'Car order'] },
  { id: 'car', name: 'Cluster', short: 'Cluster', band: 'cars', icon: 'gauge', car: 'needs', period: '24h',
    line: 'The car now: gauges, lights and the last day.',
    aim: 'What the car is doing right now, why it is awake, and its last day.',
    words: 'overview now live battery range map tyres tires doors sentry awake charging status',
    parts: ['Gauges', 'Warning lights', 'Map', 'Why it is awake', 'Tyres', 'Charging now', 'Last day'] },
  { id: 'timeline', name: 'Timeline', band: 'cars', icon: 'timeline', car: 'needs', period: '7d',
    line: 'Drives, charges, parked time and gaps in order.',
    aim: 'Everything the car did in one list: drives, charges, parked spells, gaps and updates.',
    words: 'states asleep awake parked gaps history day',
    parts: ['Filters', 'State totals', 'State strip', 'Rows', 'Why it stayed awake'] },

  { id: 'drives', name: 'Drives', band: 'driving', icon: 'route', car: 'needs', period: '30d',
    line: 'Find, sort and compare drives.',
    aim: 'Find and compare drives, and see how the car is driven.',
    words: 'trips speed consumption efficiency destinations',
    parts: ['Filters', 'Totals', 'Drive list', 'Drives with gaps', 'Patterns'] },
  { id: 'drive', name: 'Drive', band: null, icon: 'route', car: 'needs', period: '30d', parent: 'drives',
    line: 'One drive: route, charts and figures.',
    aim: 'One drive in full: the route, its charts and its figures.',
    words: 'gpx route map',
    parts: ['Route', 'Speed and power', 'Climate', 'Tyres', 'Figures', 'Export'] },
  { id: 'trip', name: 'Trip', band: 'driving', icon: 'flag', car: 'needs', period: '7d',
    line: 'Any stretch of days as one trip, with costs.',
    aim: 'A stretch of time as one trip: where the car went, what it cost and where it charged.',
    words: 'journey road trip holiday cost saved trips',
    parts: ['Window', 'Route', 'Trip counters', 'Activity', 'Drives', 'Charges', 'Battery and height'] },
  { id: 'logbook', name: 'Logbook', band: 'driving', icon: 'book', car: 'needs', period: 'year',
    line: 'A trip log for tax and expenses.',
    aim: 'A trip log for tax and expense claims, with a purpose for every drive.',
    words: 'tax business private commute expenses mileage claim pdf',
    parts: ['Filters', 'Log', 'Export'] },

  { id: 'charges', name: 'Charges', band: 'charging', icon: 'bolt', car: 'either', period: '30d',
    line: 'Charging sessions, habits and what they cost.',
    aim: 'Find charging sessions, and see charging habits and costs.',
    words: 'cost price supercharger dc ac kwh charging stats',
    parts: ['Filters', 'Totals', 'Sessions', 'Charges with gaps', 'Costs', 'Habits', 'AC and DC', 'Places', 'Fast-charge curve'] },
  { id: 'charge', name: 'Charge', band: null, icon: 'bolt', car: 'needs', period: '30d', parent: 'charges',
    line: 'One charging session and its cost.',
    aim: 'One charging session in full, with its cost.',
    words: 'cost curve',
    parts: ['Charge chart', 'Supply', 'Power curve', 'Figures', 'Cost'] },

  { id: 'battery', name: 'Battery', band: 'energy', icon: 'battery', car: 'needs', period: '30d',
    line: 'Battery health, charge level and parked drain.',
    aim: 'How healthy the battery is, how its charge is used, and what it loses while parked.',
    words: 'drain vampire degradation capacity health range charge level soc',
    parts: ['Now', 'Capacity', 'Full range', 'Lifetime', 'Reference values', 'Projected range', 'Charge level', 'Parked drain'] },
  { id: 'efficiency', name: 'Efficiency', full: 'Efficiency and temperatures', band: 'energy', icon: 'leaf', car: 'needs', period: '30d',
    line: 'Energy per distance, and what temperature does to it.',
    aim: 'How much energy driving takes, and how temperature changes it.',
    words: 'consumption wh temperature cold weather',
    parts: ['Filters', 'Consumption', 'By temperature', 'Energy factor', 'Temperatures'] },

  { id: 'statistics', name: 'Statistics', band: 'reports', icon: 'stats', car: 'either', period: 'year',
    line: 'Totals per day, week, month or year.',
    aim: 'Totals per day, week, month or year, and the odometer over time.',
    words: 'mileage odometer monthly yearly totals cost',
    parts: ['Filters', 'Totals table', 'Links', 'Odometer', 'Milestones'] },
  { id: 'travel', name: 'Travel', band: 'reports', icon: 'globe', car: null, period: 'year',
    line: 'Where the cars went, and how long they stayed.',
    aim: 'Where the cars have been: stops, places, cities, regions and countries, and the time spent there.',
    words: 'visited locations countries cities stops map',
    parts: ['Filters', 'Summary', 'Map', 'Countries', 'Cities and regions', 'Days in a city', 'Places', 'Recent stops'] },
  { id: 'software', name: 'Software', band: 'reports', icon: 'chip', car: 'either', period: 'all',
    line: 'The software history and the version now.',
    aim: 'The software the car runs now and every change before, said plainly when unsure.',
    words: 'updates firmware version',
    parts: ['Filters', 'Summary', 'History'] },

  { id: 'places', name: 'Places', band: 'setup', icon: 'pin', car: null, period: null,
    line: 'Named places and their charging prices.',
    aim: 'Name the places that matter, give them prices, and see what a change does first.',
    words: 'geofence home work price tariff cost location',
    parts: ['Place list', 'Editor', 'Prices', 'Effect on history'] },
  { id: 'settings-cars', name: 'Car settings', short: 'Cars', band: 'setup', icon: 'car', car: 'either', period: null,
    line: 'Each car, and how its CAN Bridge is tied to it.',
    aim: "Each car's settings, and how its CAN Bridge is tied to it.",
    words: 'settings token key bridge battery chemistry time zone free supercharging',
    parts: ['Car', 'Upload tokens and keys', 'Time zone', 'Battery', 'Charging'] },
  { id: 'settings-display', name: 'Display and units', short: 'Display', band: 'setup', icon: 'monitor', car: null, period: null,
    line: 'Units, look, language and size.',
    aim: 'How numbers, words and maps look for you, and the server defaults.',
    words: 'settings units miles km fahrenheit celsius psi bar theme dark light language size zoom maps background',
    parts: ['Units and range', 'Look and language', 'Background', 'Addresses and maps'] },
  { id: 'settings-integrations', name: 'Integrations', band: 'setup', icon: 'share', car: null, period: null,
    line: 'MQTT and Home Assistant, if you want them.',
    aim: "Send the cars' live values to other systems, if you want to.",
    words: 'settings mqtt home assistant node-red api events automation',
    parts: ['MQTT broker', 'Topics', 'Values sent', 'Home Assistant', 'Events', 'API access'] },
  { id: 'settings-server', name: 'Server and access', short: 'Server', band: 'setup', icon: 'server', car: null, period: null,
    line: 'Accounts, sign-in and moving the server.',
    aim: 'Who can sign in and what they see, how the server is reached, and moving it.',
    words: 'settings accounts users password sign in certificate https sessions audit move export recovery key',
    parts: ['Accounts', 'Sessions', 'Move to another server', 'Export file', 'Recovery key'] },
  { id: 'import', name: 'Import', band: 'setup', icon: 'upload', car: null, period: null,
    line: 'Bring in history from TeslaMate.',
    aim: 'Bring in history from a TeslaMate backup.',
    words: 'teslamate backup history upload',
    parts: ['Upload', 'Progress', 'Report', 'Overlap with CAN history'] },
  { id: 'health', name: 'Data health', full: 'Data health and storage', band: 'setup', icon: 'pulse', car: 'either', period: '30d',
    line: 'Is the data whole, and how much room it takes.',
    aim: 'Is the data whole and trustworthy, and how much room does it take.',
    words: 'storage disk logs trust gaps signals system server health',
    parts: ['Per car', 'Storage', 'Server', 'API errors', 'Signals'] },
  { id: 'help', name: 'Help', full: 'Help and about', band: 'setup', icon: 'question', car: null, period: null,
    line: 'Version, licences and short answers.',
    aim: 'The version, the licences, data credits and short answers.',
    words: 'about version licence license credits faq questions',
    parts: ['About', 'Licences', 'Questions'] },

  { id: 'sign-in', name: 'Sign in', band: null, icon: 'user', car: null, period: null, bare: true,
    line: 'Sign in to Sentry Mate.', aim: 'Sign in to see your cars.', words: '', parts: [] },
  { id: 'setup', name: 'Set up', band: null, icon: 'sliders', car: null, period: null, bare: true,
    line: 'Start this server.', aim: 'Start a new server, move one here, or restore from a file.', words: '', parts: [] },
]);

const BY_ID = new Map(STATIONS.map((s) => [s.id, s]));

/** The station of a page id (or undefined). */
export const station = (id) => BY_ID.get(id);
/** The stations of a band, in order. */
export const inBand = (band) => STATIONS.filter((s) => s.band === band);
/** The band of a band id. */
export const band = (id) => BANDS.find((b) => b.id === id);
/** Every station the radio tunes to, in order (detail pages and bare screens left out). */
export const DIAL = Object.freeze(STATIONS.filter((s) => s.band));
/** The full name: 'Efficiency and temperatures'. */
export const fullName = (s) => s?.full || s?.name || '';

/** The station `step` places away from `id` on the dial, wrapping round (a detail page counts as its list). */
export function tune(id, step) {
  const at = DIAL.findIndex((s) => s.id === (station(id)?.parent || id));
  const i = at < 0 ? (step > 0 ? 0 : DIAL.length - 1) : (at + step + DIAL.length) % DIAL.length;
  return DIAL[i];
}

/** 'FM 3' style channel number of a station on the dial (1-based), or null. */
export const channel = (id) => {
  const i = DIAL.findIndex((s) => s.id === (station(id)?.parent || id));
  return i < 0 ? null : i + 1;
};
