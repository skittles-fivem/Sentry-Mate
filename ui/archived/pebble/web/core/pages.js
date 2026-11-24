// Sentry Mate, "Pebble" UI pack: the page catalogue.
//
// One entry per page id: its title, the short label of the dock and the More sheet, the icon, a one-line
// blurb, the purpose line of the page plan (docs/parity/ui-parity.md), the default period (null: the page
// shows none) and whether it needs a car ('required'), can show one ('optional') or shows none ('none').
// DOCK is the five pages of the floating dock; SHEET the groups of the More sheet. Detail pages (drive,
// charge) are in neither: the dock marks their list page.

export const PAGES = Object.freeze({
  fleet: { title: 'Fleet', label: 'Fleet', icon: 'fleet', period: null, car: 'none', blurb: 'Every car at once.', purpose: 'See every car at once, find the one that needs attention, and open it.' },
  car: { title: 'Car', label: 'Car', icon: 'car', period: '24h', car: 'required', blurb: 'What the car is doing now.', purpose: 'What the car is doing now, why it is awake, and the last day at a glance.' },
  timeline: { title: 'Timeline', label: 'Timeline', icon: 'timeline', period: '7d', car: 'required', blurb: 'Every drive, charge and stop in order.', purpose: 'Everything the car did, as one list: drives, charges, parked spells, gaps and updates.' },
  drives: { title: 'Drives', label: 'Drives', icon: 'route', period: '7d', car: 'required', blurb: 'Find and compare drives.', purpose: 'Find and compare drives, and see driving patterns.' },
  drive: { title: 'Drive', label: 'Drive', icon: 'route', period: null, car: 'required', detail: 'drives', blurb: 'One drive in full.', purpose: 'One drive in full: route, charts and figures.' },
  trip: { title: 'Trip', label: 'Trip', icon: 'trip', period: '7d', car: 'required', blurb: 'Any stretch of time as one trip.', purpose: 'Any stretch of time as a trip: where the car went, what it cost, and how it charged.' },
  logbook: { title: 'Logbook', label: 'Logbook', icon: 'book', period: 'year', car: 'required', blurb: 'A trip log for tax and expenses.', purpose: 'A trip log for tax and expense claims, with a purpose per drive.' },
  charges: { title: 'Charges', label: 'Charges', icon: 'bolt', period: '30d', car: 'optional', blurb: 'Charging sessions, habits and costs.', purpose: 'Find charging sessions and understand charging habits and costs.' },
  charge: { title: 'Charge', label: 'Charge', icon: 'bolt', period: null, car: 'required', detail: 'charges', blurb: 'One charging session in full.', purpose: 'One charging session in full, with its cost editor on the same page.' },
  battery: { title: 'Battery', label: 'Battery', icon: 'battery', period: '30d', car: 'required', blurb: 'Battery health and where energy goes.', purpose: 'How healthy the battery is, how the charge level is used, and where energy goes while parked.' },
  efficiency: { title: 'Efficiency', label: 'Efficiency', icon: 'leaf', period: '30d', car: 'required', blurb: 'Energy per km, by temperature and speed.', purpose: 'How much energy driving takes, how temperature changes it, and the temperatures themselves.' },
  statistics: { title: 'Statistics', label: 'Statistics', icon: 'bars', period: 'year', car: 'optional', blurb: 'Totals per week, month or year.', purpose: 'Totals per day, week, month or year, and the odometer over time.' },
  travel: { title: 'Travel', label: 'Travel', icon: 'globe', period: 'year', car: 'none', blurb: 'Where the cars have been.', purpose: 'Where the cars have been: stops, places, cities, regions and countries, with time spent there.' },
  software: { title: 'Software', label: 'Software', icon: 'chip', period: 'all', car: 'required', blurb: 'Versions and when they came.', purpose: 'The car’s software history and the version it runs now, honest about what is known.' },
  places: { title: 'Places', label: 'Places', icon: 'pin', period: null, car: 'none', blurb: 'Named places and their prices.', purpose: 'Name the places that matter, give them charging prices, and see what a change will do before saving.' },
  'settings-cars': { title: 'Cars', label: 'Cars', icon: 'car', period: null, car: 'optional', blurb: 'Per-car settings, the CAN Bridge, the zone new cars start in.', purpose: 'Per-car settings, and how each car’s CAN Bridge is tied to it.' },
  'settings-display': { title: 'Display', label: 'Display', icon: 'sun', period: null, car: 'none', blurb: 'Theme, units, map style and the UI pack.', purpose: 'How numbers, words and maps look to you: your own choices, and the size on this device.' },
  'settings-integrations': { title: 'Integrations', label: 'Integrations', icon: 'plug', period: null, car: 'none', blurb: 'Send live values to other systems.', purpose: 'Send the cars’ live values to other systems, if the owner wants to.' },
  'settings-maps': { title: 'Maps and addresses', label: 'Maps', icon: 'map', period: null, car: 'none', blurb: 'Map tiles, the offline map and address lookup.', purpose: 'Where maps and addresses come from, for everyone on this server.' },
  'settings-server': { title: 'Server', label: 'Server', icon: 'server', period: null, car: 'none', blurb: 'Accounts, access, moving and updates.', purpose: 'Who can sign in, what they may see, how the server is reached, moving it, and its updates.' },
  import: { title: 'Import', label: 'Import', icon: 'upload', period: null, car: 'none', blurb: 'Bring in a TeslaMate backup.', purpose: 'Bring in the history from a TeslaMate backup, checked before anything is stored.' },
  health: { title: 'Health', label: 'Health', icon: 'pulse', period: '30d', car: 'optional', blurb: 'Is the car’s data complete?', purpose: 'Is the data complete and trustworthy, and how much room does it take.' },
  help: { title: 'Help', label: 'Help', icon: 'help', period: null, car: 'none', blurb: 'Version, licences and answers.', purpose: 'Version, licences, data credits and short answers to common questions.' },
  'sign-in': { title: 'Sign in', label: 'Sign in', icon: 'person', period: null, car: 'none', auth: true, blurb: '', purpose: '' },
  setup: { title: 'Set up', label: 'Set up', icon: 'spark', period: null, car: 'none', auth: true, blurb: '', purpose: '' },
});
for (const [id, p] of Object.entries(PAGES)) { p.id = id; Object.freeze(p); }

/** The system page is the health page without a car (/system). */
export const SYSTEM = Object.freeze({ id: 'system', title: 'System', label: 'System', icon: 'server', period: '30d', car: 'none', blurb: 'The server’s own health and storage.', purpose: 'Is the server healthy, and how much room do all the cars take.' });

/** The floating dock, left to right (the car switcher before them, More after). */
export const DOCK = Object.freeze(['fleet', 'car', 'drives', 'charges', 'battery']);

/** The More sheet: every other page, in groups. 'system' is /system. */
export const SHEET = Object.freeze([
  { name: 'Activity', pages: ['timeline', 'trip', 'logbook'] },
  { name: 'Insights', pages: ['efficiency', 'statistics', 'travel', 'places'] },
  { name: 'Car', pages: ['software', 'health'] },
  { name: 'Settings', pages: ['settings-cars', 'settings-display', 'settings-integrations', 'settings-maps', 'settings-server', 'import'] },
  { name: 'More', pages: ['system', 'help'] },
]);

/** The entry of a page id (or 'system'). */
export const pageInfo = (id) => (id === 'system' ? SYSTEM : PAGES[id] || null);
/** Which entry the frame marks for a page: a detail page marks its list, /system marks System. */
export function navId(page, hasCar) {
  if (!page) return null;
  if (page === 'health' && !hasCar) return 'system';
  const p = PAGES[page];
  return p && p.detail ? p.detail : page;
}
