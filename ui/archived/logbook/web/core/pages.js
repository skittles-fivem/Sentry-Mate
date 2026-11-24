// Sentry Mate, "Logbook" UI pack: the page catalogue.
//
// One entry per page id (contract 3): its name, menu group, a one-line blurb for the menus, the purpose
// line from docs/parity/ui-parity.md, finder keywords, the sections the page will hold, the default
// period (null: the page shows no period), and whether it needs a car ('required'),
// can show one ('optional') or shows none ('none'). Detail pages (drive, charge) are not in the menus.
// The frame builds the menus, the finder and the tab titles from this; pages read their own entry.

export const GROUPS = Object.freeze([
  Object.freeze({ id: "cars", name: "Cars", pages: Object.freeze(["fleet", "car", "timeline"]) }),
  Object.freeze({ id: "driving", name: "Driving", pages: Object.freeze(["drives", "trip", "logbook"]) }),
  Object.freeze({ id: "battery", name: "Battery and energy", pages: Object.freeze(["charges", "battery", "efficiency"]) }),
  Object.freeze({ id: "reports", name: "Reports", pages: Object.freeze(["statistics", "travel", "software"]) }),
  Object.freeze({ id: "setup", name: "Set-up", pages: Object.freeze(["places", "settings-cars", "settings-display", "settings-integrations", "settings-maps", "settings-server", "import", "health", "help"]) }),
]);

export const PAGES = Object.freeze({
  "fleet": Object.freeze({
    id: "fleet", name: "Fleet", group: "cars", period: null, car: "none", detail: false,
    blurb: "Every car at once: state, battery, place and what needs a look.",
    purpose: "See every car at once, find the one that needs attention, and open it.",
    keywords: "all cars register list attention home order pin",
    sections: [
      ["Car list", "Compact sortable list, one row per car: name, model and year, state and since when, battery %, range, place, plugged in, charging power, data age."],
      ["Attention", "Cars that need a look, with the cause: data stopped mid-drive or mid-charge, no upload for a long time, logs failing their check, low battery, a tyre below the recommended pressure, a door open while asleep."],
      ["Fleet totals", "Counts per state (driving, charging, parked, asleep, unknown); stale cars counted once."],
      ["Empty state", "When no car shows, say exactly why and link the fix: no upload token yet, token made but never used, a CAN Bridge key waiting to be assigned, logs waiting for trust."],
      ["Car order", "Each person can pin cars or set their own order."],
    ],
  }),
  "car": Object.freeze({
    id: "car", name: "Car overview", group: "cars", period: "24h", car: "required", detail: false,
    blurb: "What one car is doing now, why it is awake, and its last day.",
    purpose: "What the car is doing now, why it is awake, and the last day at a glance.",
    keywords: "now live state awake sentry tyres tires doors battery range map where",
    sections: [
      ["Header", "Name, model and year from the VIN prefix, and the trim from the car's own configuration message, each with its source."],
      ["Map", "Current position on offline map tiles, with GPS accuracy and heading."],
      ["State", "Driving, charging, parked, asleep or unknown, and since when."],
      ["Why the car is awake", "Plain facts from the car: Sentry mode, keep-climate mode (keep, dog, camp), cabin or battery preconditioning, someone in the car, charging, battery kept warm, a door open, accessory power."],
      ["Status flags", "Each door, trunk, frunk and liftgate by name; Sentry mode and a labelled guess at a Sentry event; plugged in; charge port open; reduced range in the cold."],
      ["Tyres", "All four pressures at all times, with the car's recommended pressure and a warning per tyre."],
      ["Battery and range", "Battery %, usable % when it differs, rated or ideal range (the person's choice), energy remaining in kWh, range at a full charge (shown in the open, not only on hover), and Sentry Mate's own estimated range with its basis."],
      ["Charging now", "Charging status as a word, AC or DC, power, volts, amps, phases, energy added, charge limit, energy still to add, time to full and finish time with date in the car's time zone."],
      ["Live tiles", "Speed, power, outside and cabin temperature, climate set point, odometer, 12 V battery."],
      ["Software", "Last known version with its date and source (imported or typed by the owner), and 'software changed since' when the car's module build ids change."],
      ["Data age and units", "Every value shows its own time."],
      ["Window controls", "Shared period picker (default last 24 hours), legend toggles and a shared hover line across charts."],
      ["Window summary", "Distance driven, net and gross consumption for the period, each with a one-line explanation."],
      ["Battery chart", "Battery % and range over the period, with asleep and unknown spans drawn differently."],
      ["Charging chart", "Charging power, energy added, AC volts and amps (DC charger volts and amps for DC) and battery heating over the period; lines break between sessions."],
      ["Activity strip", "The same state strip as the Timeline for the period, linking to it."],
    ],
  }),
  "timeline": Object.freeze({
    id: "timeline", name: "Timeline", group: "cars", period: "7d", car: "required", detail: false,
    blurb: "Drives, charges, parked spells and gaps as one list and strip.",
    purpose: "Everything the car did, as one list and one strip: drives, charges, parked spells, gaps and updates.",
    keywords: "history day states asleep sleep awake gaps strip log",
    sections: [
      ["Filters", "Kinds of entry, place, a text search that only hides rows and never changes their numbers, and the shared period picker."],
      ["State summary", "Current state, last change, and the share of time parked, asleep, awake and unknown."],
      ["State strip", "Driving, charging, online, online with only the vehicle bus, asleep, unknown and updating, in fixed colours with a legend."],
      ["Timeline rows", "Newest first, paged."],
      ["Row links", "Open the drive or charge, open the Trip for exactly the row's span, or create or edit the place, prefilled from the recorded position."],
      ["Why the car stayed awake", "On parked rows: the reasons from the car (Sentry, climate, preconditioning, charging, keep-warm, accessory power), with short help next to them."],
    ],
  }),
  "drives": Object.freeze({
    id: "drives", name: "Drives", group: "driving", period: "7d", car: "required", detail: false,
    blurb: "Find and compare drives, and see driving patterns.",
    purpose: "Find and compare drives, and see driving patterns.",
    keywords: "trips routes distance efficiency speed destinations",
    sections: [
      ["Filters", "Shared period, places (start, end or either), text search, minimum distance, minimum average speed, source (CAN or imported), flags (gap, in progress, reduced range), places to leave out of patterns, and the efficiency basis."],
      ["Summary tiles", "Count, distance, time, energy and average consumption of the listed drives, saying how many drives have measured energy."],
      ["Drive list", "Sortable list with date, start and end place, duration, distance, battery % at start and end, outside temperature, average and top speed, peak power, energy (marked measured or estimated), Wh/km and efficiency."],
      ["Drives with missing data", "Drives that overlap a gap, are still in progress, or wait for late logs, with a rebuild action for admins."],
      ["Driving patterns", "Count, distance and energy for the period with day bars, median drive length, distance and energy per day, top speed (period, last 30 days, last 7 days), seconds spent in each speed band, projected monthly and yearly distance, and the most frequent destinations."],
    ],
  }),
  "drive": Object.freeze({
    id: "drive", name: "Drive detail", group: "driving", period: null, car: "required", detail: true,
    blurb: "One drive in full: route, charts and figures.",
    purpose: "One drive in full: route, charts and figures.",
    keywords: "gpx route export",
    sections: [
      ["Header and zoom", "Car and drive in a breadcrumb, previous and next drive, and zoom by dragging across a chart; the figures follow the zoom."],
      ["Route map", "Route at 1 Hz with GPS accuracy per point, coloured by speed or power."],
      ["Drive chart", "Speed, battery power, battery % and usable %, range, elevation and battery heating power on one time axis with one hover readout."],
      ["Climate", "Outside and cabin temperature, both set points (LO and HI shown as words), climate on, preconditioning or overheat protection, and fan level."],
      ["Tyres", "All four tyre pressures through the drive."],
      ["Figures", "Odometer at start and end, duration, distance, climb and descent, net energy, regen energy, Wh/km, average speed and seconds per speed band."],
      ["Export", "GPX and GeoJSON of the route with time, elevation and accuracy, optionally speed, power and battery %."],
    ],
  }),
  "trip": Object.freeze({
    id: "trip", name: "Trip", group: "driving", period: "7d", car: "required", detail: false,
    blurb: "Any stretch of time as a trip: route, cost and charging.",
    purpose: "Any stretch of time as a trip: where the car went, what it cost, and how it charged.",
    keywords: "journey road trip window saved",
    sections: [
      ["Window and cars", "Any window from the shared picker, shortcuts (last three drives, a day, a statistics period, a saved trip), and a second car for side-by-side comparison."],
      ["Route map", "One line per drive, with markers for start, end, charges and overnight stops."],
      ["Trip figures", "Distance by odometer and by drives, time split into driving, AC charging, DC charging, parked awake, asleep and not recorded, average speed moving and including fast-charging, net and gross consumption, charging cost and cost per 100 km."],
      ["Activity strip", "The state strip for the window."],
      ["Drives", "Every drive of the trip: start time, start and end place, duration (hours and minutes), distance, battery % at start and end, and Wh/km (blank with the reason when it would be unreliable), with links."],
      ["Charges", "Every charge of the trip: start time, place, charging time, cost, energy added and drawn, average power, range added, battery % at start and end, and a link to edit the cost."],
      ["Battery and elevation", "Battery % and range, and elevation, over the trip."],
    ],
  }),
  "logbook": Object.freeze({
    id: "logbook", name: "Logbook", group: "driving", period: "year", car: "required", detail: false,
    blurb: "A trip log for tax and expense claims, with a purpose per drive.",
    purpose: "A trip log for tax and expense claims, with a purpose per drive.",
    keywords: "tax business private commute purpose expenses pdf",
    sections: [
      ["Filters", "Year or any period; one car at a time with the car's name in the heading."],
      ["Logbook table", "Number, start and end time, odometer at start and end, start and end address and place, distance, duration, purpose (business, private, commute), relation and note, and a continuity check that flags kilometres missing between two drives."],
      ["Export", "CSV and PDF buttons; totals per purpose for the period."],
    ],
  }),
  "charges": Object.freeze({
    id: "charges", name: "Charges", group: "battery", period: "30d", car: "optional", detail: false,
    blurb: "Charging sessions, habits and costs.",
    purpose: "Find charging sessions and understand charging habits and costs.",
    keywords: "charging cost price kwh supercharger sessions ac dc curve",
    sections: [
      ["Filters", "Shared period, cars (one, several or all), places, place kind, AC/DC/unknown, text search, minimum cost (with or without unpriced sessions), minimum duration and source."],
      ["Summary tiles", "Energy added, energy drawn from the grid (AC, measured), cost per currency, average duration; each tile says how many sessions it counts."],
      ["Session list", "Date, place, AC or DC, charging time and plugged-in time, cost and price per kWh (with the basis), energy added and drawn, efficiency (AC only; DC shown as unknown with the reason), outside and battery temperature, average power, range added and range added per hour of charging, battery % at start and end, odometer."],
      ["Charges with missing data", "Charges that overlap an unknown span or dropped frames, whose logs failed their check, or that are still under way: start, last recorded second and where the gap lies, energy added and drawn, battery % at start and end, duration, each linked to its charge page, with a rebuild action for admins."],
      ["Costs and totals", "Sessions, energy, total cost per currency, Supercharger cost, price per kWh overall, AC and DC, and cost per 100 km with its method."],
      ["Charging habits", "Where sessions usually start and end (bands and a heatmap over time), the start-to-end band per session against the recommended daily band, and runs of back-to-back sessions."],
      ["AC and DC split", "Energy and time by AC, DC and unknown."],
      ["Charging places", "Map and tables of places by energy and by cost, grouped by saved place or position, paged."],
      ["Fast-charging curve", "Power against battery % for every DC session, coloured by battery temperature, with this car's median curve; a dot opens its charge."],
    ],
  }),
  "charge": Object.freeze({
    id: "charge", name: "Charge detail", group: "battery", period: null, car: "required", detail: true,
    blurb: "One charging session in full, with its cost.",
    purpose: "One charging session in full, with its cost editor on the same page.",
    keywords: "cost session",
    sections: [
      ["Header", "Car and session in a breadcrumb (the car comes with the session; no car picker that could mix two cars), previous and next session, zoom inside the session."],
      ["Charge chart", "Battery % and usable %, power, range, battery temperature, battery heating and outside temperature on one time axis at 1 Hz."],
      ["Supply chart", "AC line volts and amps, phases in use, the station's pilot current and which limit applies (station, cable or car); for DC, the charger's own volts and amps."],
      ["Power curve", "Power against battery % for this session, over this car's typical DC curve."],
      ["Figures", "Charging time and plugged-in time, place and map, energy added and drawn, efficiency (AC), battery % and range at start, added and at end, average power, average outside and battery temperature, energy still to add when it was stopped."],
      ["Cost", "Cost with a breakdown per rate and session fee."],
    ],
  }),
  "battery": Object.freeze({
    id: "battery", name: "Battery", group: "battery", period: "30d", car: "required", detail: false,
    blurb: "Battery health, charge level and drain while parked.",
    purpose: "How healthy the battery is, how the charge level is used, and where energy goes while parked.",
    keywords: "capacity degradation health drain vampire parked charge level projected range soc",
    sections: [
      ["Now", "Battery %, usable %, energy stored in kWh, and the everyday band with the car's own charge limit as a tick."],
      ["Capacity and health", "Full-pack and usable capacity from the battery management system, capacity when new with its source, difference, health % and a warranty line, and capacity against distance or time with a trend and projection."],
      ["Full-charge range", "Range at 100 % now and when new, and range lost, each with its source."],
      ["Lifetime totals", "Logged distance, odometer, distance not logged (with a link to each gap), charging sessions, energy added and drawn, AC/DC/regen split from the pack's own lifetime counters, and equivalent full cycles."],
      ["Reference values (per car)", "Capacity when new, full range when new and a warranty threshold, typed once per car and checked on input."],
      ["Projected range", "Range at a full charge over time, from energy and from battery %, next to odometer, battery temperature and outside temperature."],
      ["Charge level history", "Battery % and usable % over the period with asleep and unknown spans drawn apart, moving average, median and percentile lines, the recommended band, and the share of time above 80 % and below 20 %."],
      ["Parked drain", "Each parked spell: start and end, length, time asleep, awake and unknown, energy lost in kWh, average power, battery % and range lost, range lost per hour, the reasons it stayed awake, and a cold-battery mark."],
    ],
  }),
  "efficiency": Object.freeze({
    id: "efficiency", name: "Efficiency and temperatures", group: "battery", period: "30d", car: "required", detail: false,
    blurb: "Energy per distance, and how temperature changes it.",
    purpose: "How much energy driving takes, how temperature changes it, and the temperatures themselves.",
    keywords: "consumption wh temperature weather cold cabin",
    sections: [
      ["Filters", "Shared period, minimum drive distance, temperature group width, one car or compare cars."],
      ["Consumption", "Net consumption (driving only), gross consumption (everything, parked included) and logged distance, each with a one-line method note."],
      ["By temperature", "Consumption, range efficiency, distance and average speed per outside-temperature group, plus drives with no temperature."],
      ["Energy factor", "The car's rated and ideal Wh per km, each from energy and range read in the same second, with how many readings back it."],
      ["Temperatures", "Outside, cabin and battery temperature: lowest, average, highest and how much of the period has data; charts over time; set points while climate runs; outside temperature during charges; daily range and typical band."],
    ],
  }),
  "statistics": Object.freeze({
    id: "statistics", name: "Statistics", group: "reports", period: "year", car: "optional", detail: false,
    blurb: "Totals per day, week, month or year, and the odometer.",
    purpose: "Totals per day, week, month or year, and the odometer over time.",
    keywords: "stats monthly yearly mileage odometer totals cost",
    sections: [
      ["Filters", "Group by day, week (ISO), month or year in each car's time zone, or the whole period as one row; shared period; one car, several or all."],
      ["Period table", "Per period: time driven, drives, distance (odometer and logged), average outside temperature, driving efficiency, energy added and drawn, charges, energy per charge, cost per currency, price per kWh, cost per 100 km, net and gross consumption, overhead, and how complete the data is (recorded share, unknown time, dropped stretches, open drive or charge, imported rows)."],
      ["Period links", "Period -> Trip; drive count -> Drives; charge count -> Charges; energy -> Charges habits."],
      ["Odometer and mileage", "Odometer over time and distance per period, with logged and not-logged distance and a yearly projection."],
      ["Milestones", "Optional: firsts and records (longest drive, most efficient month, first 10,000 km), computed from stored data."],
    ],
  }),
  "travel": Object.freeze({
    id: "travel", name: "Travel", group: "reports", period: "year", car: "none", detail: false,
    blurb: "Where the cars have been: places, cities and countries.",
    purpose: "Where the cars have been: stops, places, cities, regions and countries, with time spent there.",
    keywords: "visited locations cities countries map stops",
    sections: [
      ["Filters", "Cars (one, several, all), shared period, text search, place, country or region, and the chosen city for the day list."],
      ["Summary", "Places, cities, regions, countries, days out, first and last visit, unnamed stops, distance, energy added and drawn, charging efficiency and cost per currency."],
      ["Map", "Offline map with the route at 1 Hz (one line per drive) and one marker per city or place, with details on hover."],
      ["Countries", "Per country: cities, days, time spent, first and last visit."],
      ["Cities and regions", "Per city and region: country, visits, places, days, time spent, first and last visit, with counts printed on the ranking bars; narrow by country or region; pick a city to list its days."],
      ["Days in a city", "Per day in the chosen city: first and last time seen, drives, charges and places, in the car's time zone."],
      ["Places visited", "Stops grouped by saved place or position, with address parts, visits and time spent; create or edit a saved place from a row."],
      ["Recent stops", "Most recent stops with arrival and departure time."],
    ],
  }),
  "software": Object.freeze({
    id: "software", name: "Software", group: "reports", period: "all", car: "required", detail: false,
    blurb: "Software history and the version each car runs now.",
    purpose: "The car's software history and the version it runs now, honest about what is known.",
    keywords: "updates firmware version release",
    sections: [
      ["Filters", "Shared period; one car or the fleet version list."],
      ["Summary", "Updates in the period, median time between updates, last known version with its date and source, and 'software changed since'."],
      ["Update history", "Per update: start, end and duration (from the car's update status when recorded), time since the previous one, version text (imported or typed by the owner) or 'build changed' with the modules that changed, charges while on it, capacity and range at 100 % while on it."],
    ],
  }),
  "places": Object.freeze({
    id: "places", name: "Places", group: "setup", period: null, car: "none", detail: false,
    blurb: "Named places and their charging prices.",
    purpose: "Name the places that matter, give them charging prices, and see what a change will do before saving.",
    keywords: "geofence home work charger price tariff cost",
    sections: [
      ["Place list", "Name, kind (home, work, charger, service centre, other), position, radius, drives and charges using it; sorted by name; create, edit and delete (a delete shows first what loses the name); suggestions from where the cars park most."],
      ["Place editor", "Map with a circle to move and resize, name, kind, and the match rule (the nearest centre wins when places overlap)."],
      ["Prices", "Currency, price per kWh or per minute, time-of-use rates by day and hour in the place's time zone, and a session fee."],
      ["Effect on history", "Before saving: which drives and charges gain or lose the name and which charges would be repriced, with counts and examples."],
    ],
  }),
  "settings-cars": Object.freeze({
    id: "settings-cars", name: "Settings: Cars", group: "setup", period: null, car: "optional", detail: false,
    blurb: "Per-car settings and how each CAN Bridge is tied to it.",
    purpose: "Per-car settings, and how each car's CAN Bridge is tied to it.",
    keywords: "upload token key bridge pairing time zone new cars default zone chemistry lfp supercharging",
    sections: [
      ["Car and switcher", "Car picker, name, and an empty state that explains how a car appears."],
      ["Upload tokens and keys", "Upload tokens (made here, shown once), CAN Bridge keys tied to the car, a preferred key when two record at once, and keys waiting to be assigned, each with first and last upload."],
      ["Time zone", "The car's time zone, used for days, weeks and tariffs."],
      ["Time zone for new cars", "The zone a new car starts in (the server's default), on the page without a car; an admin changes it."],
      ["Battery", "Chemistry: automatic (with the evidence), LFP, or NCA/NMC."],
      ["Charging", "Free Supercharging as a dated range, with the car's own entitlement shown as a hint."],
    ],
  }),
  "settings-display": Object.freeze({
    id: "settings-display", name: "Settings: Display and units", group: "setup", period: null, car: "none", detail: false,
    blurb: "Your units, theme, language, map style and interface size.",
    purpose: "How numbers, words and maps look to you: your own choices, and the size on this device.",
    keywords: "units miles km fahrenheit psi bar theme dark light language size zoom map colours style satellite pack main car",
    sections: [
      ["Units and range", "Distance, temperature and pressure units, and rated or ideal range."],
      ["Look and language", "Light, dark or device theme, map colours and map style, interface language, interface size (also on this device only), UI pack and main car."],
      ["Server defaults", "The units and range for accounts that have not chosen, set by an admin."],
      ["Saving", "Changes save at once; per-person choices override the server defaults."],
    ],
  }),
  "settings-integrations": Object.freeze({
    id: "settings-integrations", name: "Settings: Integrations", group: "setup", period: null, car: "none", detail: false,
    blurb: "Send live values to MQTT and Home Assistant.",
    purpose: "Send the cars' live values to other systems, if the owner wants to.",
    keywords: "mqtt home assistant node-red api broker",
    sections: [
      ["MQTT broker", "Host, port, user, password (write-only), TLS with certificate pinning, namespace, on or off; a test button and live status (connected, delivered, waiting, dropped, last error)."],
      ["MQTT topics and rules", "Topic layout, the text used when the car is at no saved place, how missing values are published, and a timestamp with every value set."],
      ["MQTT published values", "Every published value with its unit and source, previewed for a chosen car."],
      ["Home Assistant discovery", "Discovery on or off, prefix, one device per car, and clean-up the moment a car is deleted or renamed."],
      ["Home Assistant entities", "The entities Home Assistant gets, one per published value, read-only."],
      ["Events for automations", "Explicit drive started and ended, charge started and ended, software changed, someone in the car, left-open reminders: no spurious events after a restart."],
      ["API access", "The read API for other apps, with per-account car access."],
      ["Network use", "Every outbound connection in use (address lookup, tiles, MQTT broker) and the inbound upload port."],
      ["Works with", "Notes on using the published values with route planners, garage doors, desk gauges and smart mirrors."],
    ],
  }),
  "settings-maps": Object.freeze({
    id: "settings-maps", name: "Settings: Maps and addresses", group: "setup", period: null, car: "none", detail: false,
    blurb: "Map tiles, the offline map and address lookup, for everyone.",
    purpose: "Where maps and addresses come from, for everyone on this server.",
    keywords: "map tiles satellite offline map file pmtiles online tile server credit attribution place names roads overlay styles address lookup geocoding nominatim rounding address language proxy outside links release notes",
    sections: [
      ["Map tiles", "Offline (from this server) or an online source: a preset such as satellite pictures, or your own address with its credit and zoom."],
      ["Place names and roads", "Town names, borders and roads drawn over satellite pictures."],
      ["Maps people can switch to", "The online maps each person may pick on a map page."],
      ["Offline map file", "Which offline map file is in use, and how to make one for your area."],
      ["Address lookup", "Offline (bundled place names) or an online service with rounding and a cache, its status, and the outbound proxy."],
      ["Address language", "The language street and place names are written in."],
      ["Outside links", "Whether pages may link to an outside map and to release notes."],
    ],
  }),
  "settings-server": Object.freeze({
    id: "settings-server", name: "Settings: Server and access", group: "setup", period: null, car: "none", detail: false,
    blurb: "Accounts, roles, sessions, the audit log, HTTPS and updates.",
    purpose: "Who can sign in, what they may see, how the server is reached, and its updates.",
    keywords: "accounts users password sign out sessions audit certificate https recovery key backup release check new version update",
    sections: [
      ["Access and hosting", "Accounts with Admin and Watch-only roles, per-account car lists, two-step sign-in, sessions (sign out here or everywhere), the audit log, host names for the certificate, the HTTPS port, and backups and the recovery key."],
      ["Server updates", "This build, and the release check: once a day, if on, ask whether a newer Sentry Mate exists, with Check now."],
    ],
  }),
  "import": Object.freeze({
    id: "import", name: "Import", group: "setup", period: null, car: "none", detail: false,
    blurb: "Bring in history from a TeslaMate backup.",
    purpose: "Bring in the history from a TeslaMate backup, checked before anything is stored.",
    keywords: "teslamate backup upload history",
    sections: [
      ["Progress", "Status (uploading, checking, importing, done, failed), pass and bytes; safe to leave the page."],
      ["Report", "Per table and per car: rows read, imported and skipped, with a reason for every skipped count."],
      ["Overlap with CAN history", "Spans where imported and CAN history both exist, which one is shown, and a switch; addresses are filled in for imported places."],
    ],
  }),
  "health": Object.freeze({
    id: "health", name: "Data health and storage", group: "setup", period: "30d", car: "optional", detail: false,
    blurb: "Whether the data is complete and trusted, and the room it takes.",
    purpose: "Is the data complete and trustworthy, and how much room does it take.",
    keywords: "storage disk gaps trust signature logs rebuild signals system server",
    sections: [
      ["Per-car health", "Last upload, last log check (verified, unsigned, failed, with the reason), chain gaps, dropped frames, unknown spans, logs waiting, rebuild pending; each with an in-app fix."],
      ["Per-car totals", "Logged distance, odometer, drives, charges and updates per car, per source (CAN and imported)."],
      ["Storage", "Bytes by kind in plain words (raw logs, samples, charge curves, tables, places and settings, address cache, offline maps), free disk space, growth and 'full in N days'."],
      ["Server health", "Uptime, versions, memory and cache use, last housekeeping, timings (decode per log, rebuild per window, API response times), and a health address for container checks that reveals nothing."],
      ["API errors", "The one error shape every API call uses, and what an unknown or hidden car returns."],
      ["Signals", "Every decoded signal from the newest log with its name, unit, value names and time, searchable; a chosen series over a period, with CSV."],
    ],
  }),
  "help": Object.freeze({
    id: "help", name: "Help and about", group: "setup", period: null, car: "none", detail: false,
    blurb: "Version, licences, data credits and answers.",
    purpose: "Version, licences, data credits and short answers to common questions.",
    keywords: "about version licence license credits questions faq",
    sections: [
      ["About", "Version, API version, UI pack and version, links to source code and documentation, and an opt-in notice when a newer release exists."],
      ["Licence and notices", "Licence text, notices and data credits (OpenStreetMap, GeoNames, DBC sources)."],
      ["Questions and answers", "Why a car is missing, why data is late, why a span is unknown, how pairing works."],
    ],
  }),
});

/** The catalogue entry of a page id (frame screens sign-in and setup have none). */
export const pageInfo = (id) => PAGES[id] || null;
/** The menu group a page belongs to. */
export const groupOf = (id) => GROUPS.find((g) => g.id === (PAGES[id] && PAGES[id].group)) || null;
/** Pages shown in the menus and the finder, in menu order. */
export const MENU_PAGES = Object.freeze(GROUPS.flatMap((g) => g.pages));

const words = (s) => String(s || '').toLowerCase().normalize('NFKD').replace(/[\u0300-\u036f]/g, '').split(/[^a-z0-9]+/).filter(Boolean);

/**
 * Pages that match what was typed, best first: findPages('drain') -> [battery entry]. Every typed word
 * must start a word of the page's name, group, blurb, purpose, sections or keywords; a match in the name
 * counts most, then keywords, then the rest. An empty query gives every menu page in menu order.
 */
export function findPages(query) {
  const q = words(query);
  if (!q.length) return MENU_PAGES.map((id) => PAGES[id]);
  const out = [];
  MENU_PAGES.forEach((id, order) => {
    const p = PAGES[id];
    const g = GROUPS.find((x) => x.id === p.group);
    const fields = [[words(p.name), 8], [words(p.keywords), 5], [words(g.name), 3], [words(p.blurb), 2],
      [words(p.purpose + ' ' + p.sections.map((s) => s.join(' ')).join(' ')), 1]];
    let score = 0;
    for (const w of q) {
      let best = 0;
      for (const [list, weight] of fields) if (list.some((x) => x.startsWith(w))) best = Math.max(best, weight);
      if (!best) return;
      score += best;
    }
    out.push({ p, score, order });
  });
  return out.sort((a, b) => b.score - a.score || a.order - b.order).map((x) => x.p);
}
