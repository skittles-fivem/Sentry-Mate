// Every page of the app, in menu order: what it is called, where it sits in the menu, its one-line
// description, the words the page finder matches, and how the router treats it. Page modules live in
// pages/<id>.js; core/router.js loads them on first use.
//
//   id        page id (the keys of docs/parity/coverage-matrix.json "pages", plus sign-in and setup)
//   title     the page's name (menu, heading, tab title)
//   nav       the shorter name in the sidebar (defaults to title)
//   group     menu group: cars | driving | battery | reports | setup (null: not in the menu)
//   icon      sprite icon id
//   desc      one line in the menu
//   purpose   the line under the page title (from the page plan)
//   words     extra words the finder matches ('drain', 'cost')
//   car       'required' (the route has {car}), 'optional' (works with or without), or null
//   period    the default period preset when the URL and the tab have none; null: the page has no period
//   parent    the list a detail page belongs to (breadcrumb)
//   sections  what the page will hold (shown on pages not built yet)

export const GROUPS = Object.freeze([
  { id: 'cars', title: 'Cars' },
  { id: 'driving', title: 'Driving' },
  { id: 'battery', title: 'Battery and energy' },
  { id: 'reports', title: 'Reports' },
  { id: 'setup', title: 'Set-up' },
]);

export const PAGES = Object.freeze([
  { id: 'fleet', title: 'Fleet', nav: 'All cars', group: 'cars', icon: 'fleet', car: null, period: null,
    desc: 'Every car at once, and the ones that need a look.',
    purpose: 'See every car at once, find the one that needs attention, and open it.',
    words: 'all cars home list attention state stale order pin',
    sections: ['Car list', 'Attention', 'Fleet totals', 'Empty state', 'Car order'] },
  { id: 'car', title: 'Car overview', nav: 'Overview', group: 'cars', icon: 'gauge', car: 'required', period: '24h',
    desc: 'What the car is doing now, and the last day.',
    purpose: 'What the car is doing now, why it is awake, and the last day at a glance.',
    words: 'now live battery range map tyres tires doors sentry awake charging status',
    sections: ['Header', 'Map', 'State', 'Why the car is awake', 'Status flags', 'Tyres', 'Battery and range', 'Charging now', 'Live tiles', 'Software', 'Data age and units', 'Window controls', 'Window summary', 'Battery chart', 'Charging chart', 'Activity strip'] },
  { id: 'timeline', title: 'Timeline', group: 'cars', icon: 'timeline', car: 'required', period: '7d',
    desc: 'Drives, charges, parked time and gaps in one list.',
    purpose: 'Everything the car did, as one list and one strip: drives, charges, parked spells, gaps and updates.',
    words: 'states asleep awake parked gaps history day',
    sections: ['Filters', 'State summary', 'State strip', 'Timeline rows', 'Row links', 'Why the car stayed awake'] },

  { id: 'drives', title: 'Drives', group: 'driving', icon: 'route', car: 'required', period: '30d',
    desc: 'Find and compare drives; driving patterns.',
    purpose: 'Find and compare drives, and see driving patterns.',
    words: 'trips speed consumption efficiency destinations',
    sections: ['Filters', 'Summary tiles', 'Drive list', 'Drives with missing data', 'Driving patterns'] },
  { id: 'drive', title: 'Drive', group: null, icon: 'route', car: 'required', period: '30d', parent: 'drives',
    desc: 'One drive in full: route, charts and figures.',
    purpose: 'One drive in full: route, charts and figures.',
    words: 'gpx route map',
    sections: ['Header and zoom', 'Route map', 'Drive chart', 'Climate', 'Tyres', 'Figures', 'Export'] },
  { id: 'trip', title: 'Trip', group: 'driving', icon: 'flag', car: 'required', period: '7d',
    desc: 'Any stretch of time as one trip, with its costs.',
    purpose: 'Any stretch of time as a trip: where the car went, what it cost, and how it charged.',
    words: 'journey road trip holiday cost saved trips',
    sections: ['Window and cars', 'Route map', 'Trip figures', 'Activity strip', 'Drives', 'Charges', 'Battery and elevation'] },
  { id: 'logbook', title: 'Logbook', group: 'driving', icon: 'book', car: 'required', period: 'year',
    desc: 'A trip log for tax and expense claims.',
    purpose: 'A trip log for tax and expense claims, with a purpose per drive.',
    words: 'tax business private commute expenses mileage claim pdf',
    sections: ['Filters', 'Logbook table', 'Export'] },

  { id: 'charges', title: 'Charges', group: 'battery', icon: 'bolt', car: 'optional', period: '30d',
    desc: 'Charging sessions, habits and costs.',
    purpose: 'Find charging sessions and understand charging habits and costs.',
    words: 'cost price supercharger dc ac kwh charging stats',
    sections: ['Filters', 'Summary tiles', 'Session list', 'Charges with missing data', 'Costs and totals', 'Charging habits', 'AC and DC split', 'Charging places', 'Fast-charging curve'] },
  { id: 'charge', title: 'Charge', group: null, icon: 'bolt', car: 'required', period: '30d', parent: 'charges',
    desc: 'One charging session, with its cost.',
    purpose: 'One charging session in full, with its cost editor on the same page.',
    words: 'cost curve',
    sections: ['Header', 'Charge chart', 'Supply chart', 'Power curve', 'Figures', 'Cost'] },

  { id: 'battery', title: 'Battery', group: 'battery', icon: 'battery', car: 'required', period: '30d',
    desc: 'Battery health, charge level and parked drain.',
    purpose: 'How healthy the battery is, how the charge level is used, and where energy goes while parked.',
    words: 'drain vampire degradation capacity health range charge level soc',
    sections: ['Now', 'Capacity and health', 'Full-charge range', 'Lifetime totals', 'Reference values (per car)', 'Projected range', 'Charge level history', 'Parked drain'] },
  { id: 'efficiency', title: 'Efficiency and temperatures', nav: 'Efficiency', group: 'battery', icon: 'leaf', car: 'required', period: '30d',
    desc: 'Energy per distance, and how temperature changes it.',
    purpose: 'How much energy driving takes, how temperature changes it, and the temperatures themselves.',
    words: 'consumption wh temperature cold weather',
    sections: ['Filters', 'Consumption', 'By temperature', 'Energy factor', 'Temperatures'] },

  { id: 'statistics', title: 'Statistics', group: 'reports', icon: 'stats', car: 'optional', period: 'year',
    desc: 'Totals per day, week, month or year.',
    purpose: 'Totals per day, week, month or year, and the odometer over time.',
    words: 'mileage odometer monthly yearly totals cost',
    sections: ['Filters', 'Period table', 'Period links', 'Odometer and mileage', 'Milestones'] },
  { id: 'travel', title: 'Travel', group: 'reports', icon: 'globe', car: null, period: 'year',
    desc: 'Where the cars have been, and for how long.',
    purpose: 'Where the cars have been: stops, places, cities, regions and countries, with time spent there.',
    words: 'visited locations countries cities stops map',
    sections: ['Filters', 'Summary', 'Map', 'Countries', 'Cities and regions', 'Days in a city', 'Places visited', 'Recent stops'] },
  { id: 'software', title: 'Software', group: 'reports', icon: 'chip', car: 'optional', period: 'all',
    desc: 'Software history and the version now.',
    purpose: "The car's software history and the version it runs now, honest about what is known.",
    words: 'updates firmware version',
    sections: ['Filters', 'Summary', 'Update history'] },

  { id: 'places', title: 'Places', group: 'setup', icon: 'pin', car: null, period: null,
    desc: 'Named places and their charging prices.',
    purpose: 'Name the places that matter, give them charging prices, and see what a change will do before saving.',
    words: 'geofence home work price tariff cost location',
    sections: ['Place list', 'Place editor', 'Prices', 'Effect on history'] },
  { id: 'settings-cars', title: 'Settings: Cars', nav: 'Cars', group: 'setup', icon: 'car', car: 'optional', period: null,
    desc: 'Per-car settings and CAN Bridge keys.',
    purpose: "Per-car settings, and how each car's CAN Bridge is tied to it.",
    words: 'token key bridge battery chemistry time zone new cars default zone free supercharging',
    sections: ['Car and switcher', 'Upload tokens and keys', 'Time zone', 'Time zone for new cars', 'Battery', 'Charging'] },
  { id: 'settings-display', title: 'Settings: Display and units', nav: 'Display and units', group: 'setup', icon: 'monitor', car: null, period: null,
    desc: 'Your units, theme, language, map style and interface size.',
    purpose: 'How numbers, words and maps look to you: your own choices, and the size on this device.',
    words: 'units miles km fahrenheit celsius psi bar theme dark light language size zoom map colours style satellite pack main car',
    sections: ['Units and range', 'Look and language', 'Server defaults', 'Saving'] },
  { id: 'settings-integrations', title: 'Settings: Integrations', nav: 'Integrations', group: 'setup', icon: 'share', car: null, period: null,
    desc: 'A Tesla account as a second input; MQTT and Home Assistant, if you want them.',
    purpose: "Read the cars from a Tesla account as a second input, and send their live values to other systems, if the owner wants to.",
    words: 'tesla account owner api sign in refresh token mqtt home assistant node-red api events automation',
    sections: ['Tesla account', 'MQTT broker', 'MQTT topics and rules', 'MQTT published values', 'Home Assistant discovery', 'Home Assistant entities', 'Events for automations', 'API access', 'Network use', 'Works with'] },
  { id: 'settings-maps', title: 'Settings: Maps and addresses', nav: 'Maps and addresses', group: 'setup', icon: 'globe', car: null, period: null,
    desc: 'Map tiles, the offline map and address lookup, for everyone.',
    purpose: 'Where maps and addresses come from, for everyone on this server.',
    words: 'map tiles satellite offline map file pmtiles online tile server credit attribution place names roads overlay styles address lookup geocoding nominatim rounding address language outside links release notes',
    sections: ['Map tiles', 'Place names and roads', 'Maps people can switch to', 'Offline map file', 'Address lookup', 'Address language', 'Outside links'] },
  { id: 'settings-server', title: 'Settings: Server and access', nav: 'Server and access', group: 'setup', icon: 'server', car: null, period: null,
    desc: 'Accounts, access, how the server is reached and its updates.',
    purpose: 'Who can sign in, what they may see, how the server is reached, and its updates.',
    words: 'accounts users password sign in certificate https sessions audit release check new version update',
    sections: ['Access and hosting', 'Server updates'] },
  { id: 'import', title: 'Import', group: 'setup', icon: 'upload', car: null, period: null,
    desc: 'Bring in TeslaMate history.',
    purpose: 'Bring in TeslaMate history.',
    words: 'teslamate backup history upload',
    sections: ['Upload', 'Progress', 'Report', 'Overlap with CAN history'] },
  { id: 'health', title: 'Data health and storage', nav: 'Data health', group: 'setup', icon: 'pulse', car: 'optional', period: '30d',
    desc: 'Is the data complete, and how much room it takes.',
    purpose: 'Is the data complete and trustworthy, and how much room does it take.',
    words: 'storage disk logs trust gaps signals system server health',
    sections: ['Per-car health', 'Per-car totals', 'Storage', 'Server health', 'API errors', 'Signals'] },
  { id: 'help', title: 'Help and about', nav: 'Help', group: 'setup', icon: 'question', car: null, period: null,
    desc: 'Version, licences and short answers.',
    purpose: 'Version, licences, data credits and short answers to common questions.',
    words: 'about version licence license credits faq questions',
    sections: ['About', 'Licence and notices', 'Questions and answers'] },

  { id: 'sign-in', title: 'Sign in', group: null, icon: 'user', car: null, period: null, bare: true,
    desc: 'Sign in to Sentry Mate.', purpose: 'Sign in to see your cars.', words: '', sections: [] },
  { id: 'setup', title: 'Set up', group: null, icon: 'settings', car: null, period: null, bare: true,
    desc: 'Create the first account.', purpose: 'Create the first admin account with the set-up code.', words: '', sections: [] },
]);

const BY_ID = new Map(PAGES.map((p) => [p.id, p]));
/** The registry entry of a page id (or undefined). */
export const pageInfo = (id) => BY_ID.get(id);
/** The pages of a menu group, in order. */
export const pagesIn = (group) => PAGES.filter((p) => p.group === group);
