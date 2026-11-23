// Every page of the console, in the order the link row and the command list show them.
//
//   id       page id (the keys of docs/parity/coverage-matrix.json "pages", plus sign-in and setup)
//   name     the page's name (header line, tab title, command list)
//   link     the short word in the link row under the command bar
//   scope    'car' (in the current car's link row) | 'global' (right side of the link row) | 'settings' | null
//   car      'required' (the route has {car}), 'optional' (works with or without), or null
//   period   the default period when the address and the tab have none; null: the page has no period
//   parent   the list a detail page belongs to
//   go       the second key of a "g <key>" shortcut
//   desc     one line in the command list
//   words    more words the command list matches

export const PAGES = Object.freeze([
  { id: 'fleet', name: 'Fleet', link: 'fleet', scope: 'global', car: null, period: null, go: 'f',
    desc: 'every car at once: state, battery, range, place, last seen', words: 'all cars home list' },
  { id: 'car', name: 'Car overview', link: 'overview', scope: 'car', car: 'required', period: '24h',
    desc: 'what the car is doing now and the last 24 hours', words: 'now status live map tyres soc' },
  { id: 'timeline', name: 'Timeline', link: 'timeline', scope: 'car', car: 'required', period: '7d',
    desc: 'drives, charges, parked time and updates in time order', words: 'events history day log' },
  { id: 'drives', name: 'Drives', link: 'drives', scope: 'car', car: 'required', period: '30d', go: 'd',
    desc: 'every drive, sortable and filtered, with totals', words: 'trips distance speed' },
  { id: 'drive', name: 'Drive', link: 'drive', scope: null, car: 'required', period: '30d', parent: 'drives',
    desc: 'one drive: route, speed, power, battery, elevation', words: 'route samples' },
  { id: 'trip', name: 'Trip', link: 'trip', scope: 'car', car: 'required', period: '7d',
    desc: 'any stretch of time as numbered legs with totals', words: 'journey holiday legs' },
  { id: 'logbook', name: 'Logbook', link: 'logbook', scope: 'car', car: 'required', period: 'year',
    desc: 'the trip log for tax and expenses, as CSV', words: 'tax expense purpose csv' },
  { id: 'charges', name: 'Charges', link: 'charges', scope: 'car', car: 'optional', period: '30d', go: 'c',
    desc: 'every charge: energy added and used, cost, place', words: 'charging cost kwh' },
  { id: 'charge', name: 'Charge', link: 'charge', scope: null, car: 'required', period: '30d', parent: 'charges',
    desc: 'one charge: power and battery curve', words: 'curve samples' },
  { id: 'battery', name: 'Battery', link: 'battery', scope: 'car', car: 'required', period: '30d', go: 'b',
    desc: 'capacity against distance, range by month', words: 'degradation capacity health' },
  { id: 'efficiency', name: 'Efficiency', link: 'efficiency', scope: 'car', car: 'required', period: '30d',
    desc: 'consumption by temperature and by speed', words: 'consumption wh temperature' },
  { id: 'statistics', name: 'Statistics', link: 'statistics', scope: 'car', car: 'optional', period: 'year',
    desc: 'distance, energy, cost and time per period', words: 'totals month year week' },
  { id: 'travel', name: 'Travel', link: 'travel', scope: 'global', car: null, period: 'year',
    desc: 'places and countries visited, on one map', words: 'countries visited map' },
  { id: 'software', name: 'Software', link: 'software', scope: 'car', car: 'optional', period: 'all',
    desc: 'versions the car ran and for how long', words: 'updates firmware version' },
  { id: 'places', name: 'Places', link: 'places', scope: 'global', car: null, period: null,
    desc: 'named places: radius, visits, price per kWh', words: 'geofences home work' },
  { id: 'settings-cars', name: 'Settings: cars', link: 'cars', scope: 'settings', car: 'optional', period: null, go: 's',
    desc: 'per-car keys: name, time zone, tokens', words: 'car name token' },
  { id: 'settings-display', name: 'Settings: display', link: 'display', scope: 'settings', car: null, period: null,
    desc: 'units, theme, UI pack, interface size', words: 'units theme pack size language' },
  { id: 'settings-integrations', name: 'Settings: integrations', link: 'integrations', scope: 'settings', car: null, period: null,
    desc: 'Tesla account, TeslaFi, local TeslaMate, MQTT, Home Assistant, API access', words: 'mqtt home assistant api tesla teslafi teslamate token' },
  { id: 'settings-server', name: 'Settings: server', link: 'server', scope: 'settings', car: null, period: null,
    desc: 'accounts, access, moving and export files', words: 'accounts password move export certificate' },
  { id: 'import', name: 'Import', link: 'import', scope: 'settings', car: null, period: null,
    desc: 'bring in history from a file, step by step', words: 'teslamate teslafi upload history overlap' },
  { id: 'health', name: 'Health', link: 'health', scope: 'car', car: 'optional', period: '30d',
    desc: 'is the data complete; the server at /system', words: 'storage disk logs system server' },
  { id: 'help', name: 'Help', link: 'help', scope: 'global', car: null, period: null,
    desc: 'keys, version, licences, short answers', words: 'about licence keys credits' },

  { id: 'sign-in', name: 'Sign in', link: 'sign-in', scope: null, car: null, period: null, bare: true, desc: 'sign in', words: '' },
  { id: 'setup', name: 'Set up', link: 'setup', scope: null, car: null, period: null, bare: true, desc: 'first run', words: '' },
]);

const BY_ID = new Map(PAGES.map((p) => [p.id, p]));
/** The registry entry of a page id (or undefined). */
export const pageInfo = (id) => BY_ID.get(id);
/** Pages of one scope, in order. */
export const pagesOf = (scope) => PAGES.filter((p) => p.scope === scope);
/** The page a "g <key>" shortcut opens. */
export const pageForKey = (k) => PAGES.find((p) => p.go === k) || null;
