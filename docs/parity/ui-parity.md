# Sentry Mate: TeslaMate parity plan

This plan makes sure everything a TeslaMate user can see has a place in Sentry Mate, on a page that is clearer and better. It was written from our own inventories of what each TeslaMate page shows and does, described in our own words. No TeslaMate code, text or layout is copied.

Sentry Mate gets its data from the car's CAN bus (through the CAN Bridge), not from Tesla's servers. That changes what can be shown: some things become exact, a few things are not on the bus at all, and many TeslaMate settings stop being needed.

Scope: one web UI (UI packs; the two designs are Instrument and Logbook), desktop first, any number of cars, sharp at native 4K. A phone layout comes later.

## In numbers

| | Count |
|---|---:|
| TeslaMate tabs inventoried (dashboards, drill-downs, reports, web pages, documented features) | 51 |
| Inventory items | 1161 |
| Items Sentry Mate already has the data for | 318 |
| Items worked out from stored data | 490 |
| Items that need a new CAN signal decoded | 84 |
| Items not on the CAN bus (alternative chosen) | 31 |
| Items not applicable, each replaced by something named below | 238 |
| Sentry Mate pages (including the shared frame) | 23 |

Every item id appears exactly once: in one page section below, or in the not-applicable list. The machine-readable version is `coverage-matrix.json`; the API the pages need is in [`docs/ui-api.md`](../ui-api.md).

Data status words: **have** (already stored), **derive** (computed from stored data), **new signal** (a CAN signal to add to decoding), **not on CAN** (the car does not send it; an alternative is chosen), **not applicable** (a TeslaMate mechanism Sentry Mate does not need).

## Rules every page follows

- **One period picker.** The same date-range control on every report page, in the car's own time zone, with presets (24 h, 7 days, 30 days, this year, all). The chosen period and car carry over when you change page.
- **Units set once.** Each person picks distance, temperature and pressure units and rated or ideal range once. The API always sends metric values; the UI converts. Every number shows its unit.
- **Explanations next to numbers.** A short line under a figure says how it was worked out. A blank figure says why it is blank. No method hidden in a hover-only help text.
- **Asleep is not unknown.** Asleep means the car went quiet. Unknown means no log, a paused recording or a log that failed its check. Charts draw them differently and never carry a value across unknown time.
- **Measured, then estimated.** Energy comes from the pack (volts x amps, lifetime counters). Where a figure is estimated (imported history), it is marked.
- **Every value has a time.** Live values show their own age; nothing is silently hidden after a timeout.
- **Source is visible.** Rows from CAN and rows imported from TeslaMate are marked; overlaps are shown, not dropped.
- **Download everywhere.** Every table and chart has a visible Download CSV button.
- **Any number of cars.** Lists are searched and paged by cursor; reports can show one car, several, or all, instead of repeating a page per car.
- **Private by default.** Offline map tiles and addresses. No outside request unless the owner turns it on.
- **Access respected.** A person only ever sees the cars their account may see; an unknown or hidden car looks the same (not found).

## TeslaMate tab to Sentry Mate page

Pages are listed with how many of the tab's items each one takes, main page first. 'n/a' counts items that are not applicable, and names what they are; the replacement for each kind is in the not-applicable section.

| TeslaMate tab | Kind | Sentry Mate page(s) | Items | n/a |
|---|---|---|---:|---:|
| Battery Health (`dash.battery-health`) | dashboard | Battery (34); n/a: separate-app links | 36 | 2 |
| Projected Range (`dash.projected-range`) | dashboard | Battery (12); n/a: separate-app links | 14 | 2 |
| Vampire Drain (`dash.vampire-drain`) | dashboard | Battery (15); n/a: separate-app links | 17 | 2 |
| Efficiency (`dash.efficiency`) | dashboard | Efficiency and temperatures (20); n/a: separate-app links | 22 | 2 |
| Temperatures (`dash.temperatures`) | dashboard | Efficiency and temperatures (20); n/a: separate-app links | 22 | 2 |
| Mileage (`dash.mileage`) | dashboard | Statistics (3); n/a: separate-app links | 5 | 2 |
| Home / car overview (`web.cars`) | web-page | Fleet (5); n/a: Tesla account and tokens | 10 | 5 |
| Car summary card (`web.car-summary`) | web-page | Car overview (41); n/a: polling and sleep control, database internals | 51 | 10 |
| App frame (`web.shell`) | web-page | App frame (20), Settings: Display and units (4), Settings: Server and access (2); n/a: separate-app links, database internals | 30 | 4 |
| Car logging REST endpoints (`web.api.logging`) | integration | Not applicable: replaced by Car overview 'Why the car is awake' and data age; Timeline per-bus states. The CAN Bridge only listens, so the logger never wakes the car or keeps it awake | 3 | 3 |
| Charges (`dash.charges`) | dashboard | Charges (42); n/a: separate-app links, database internals | 45 | 3 |
| Charge Details (`dash.charge-details`) | drilldown | Charge detail (31); n/a: separate-app links | 33 | 2 |
| Charging Stats (`dash.charging-stats`) | dashboard | Charges (37); n/a: separate-app links | 39 | 2 |
| Charge Level (`dash.charge-level`) | dashboard | Battery (14); n/a: separate-app links | 16 | 2 |
| MQTT integration (`docs.mqtt`) | integration | Settings: Integrations (68); n/a: add-on plumbing | 70 | 2 |
| Home Assistant integration (`docs.home-assistant`) | integration | Settings: Integrations (38); n/a: add-on plumbing | 46 | 8 |
| Node-RED integration (`docs.node-red`) | integration | Car overview (11), Settings: Integrations (6), Data health and storage (4); n/a: add-on plumbing | 33 | 12 |
| Environment variables: secrets, database, time zone, import, geofence default (`docs.env.core`) | setting-section | Settings: Cars (1), Settings: Integrations (1); n/a: database internals, TeslaFi folder import | 17 | 15 |
| Environment variables: web interface address and protection (`docs.env.web`) | setting-section | Settings: Server and access (2); n/a: hosting variables | 5 | 3 |
| Environment variables: MQTT broker (`docs.env.mqtt`) | setting-section | Settings: Integrations (9) | 9 | 0 |
| Environment variables: Tesla API, streaming, polling and outbound connections (`docs.env.tesla-api`) | setting-section | Settings: Display and units (1); n/a: Tesla account and tokens, polling and sleep control, hosting variables | 19 | 18 |
| Using the Tesla Fleet API and Telemetry Streaming (`docs.tesla-api`) | flow | Not applicable: replaced by Sentry Mate accounts (Settings > Server and access) and CAN Bridge pairing with per-car upload tokens and keys (Settings > Cars) | 18 | 18 |
| Generating Tokens (`docs.tokens`) | flow | Not applicable: replaced by Sentry Mate accounts (Settings > Server and access) and CAN Bridge pairing with per-car upload tokens and keys (Settings > Cars) | 5 | 5 |
| Frequently Asked Questions (`docs.faq`) | report | Timeline (5), Data health and storage (2), Fleet (2), Settings: Integrations (2), Settings: Cars (1), Import (1), Efficiency and temperatures (1), Places (1); n/a: Tesla account and tokens, polling and sleep control | 18 | 3 |
| Screenshots (`docs.screenshots`) | web-page | Battery (4), Car overview (2), Charges (2), Drives (2), Efficiency and temperatures (2), Travel (2), Statistics (2), Timeline (2), Settings: Display and units (1), Charge detail (1), Data health and storage (1), Drive detail (1), Trip (1), Software (1) | 24 | 0 |
| Projects using TeslaMate (`docs.projects`) | integration | Settings: Integrations (9), Places (1), Software (1), Statistics (1), Timeline (1); n/a: hosting variables | 17 | 4 |
| Drives (`dash.drives`) | dashboard | Drives (39); n/a: separate-app links | 41 | 2 |
| Drive Details (`dash.drive-details`) | drilldown | Drive detail (29); n/a: separate-app links | 31 | 2 |
| Drive Stats (`dash.drive-stats`) | dashboard | Drives (18); n/a: separate-app links | 20 | 2 |
| Trip (`dash.trip`) | dashboard | Trip (46); n/a: separate-app links | 48 | 2 |
| Drives - Dutch tax (`dash.dutch-tax`) | report | Logbook (13); n/a: separate-app links | 15 | 2 |
| Home (`dash.home`) | dashboard | Not applicable: replaced by App frame navigation: one app, one sign-in; pages grouped by task, the chosen car and period carried from page to page | 6 | 6 |
| Overview (`dash.overview`) | dashboard | Car overview (29); n/a: separate-app links, panel menus | 32 | 3 |
| States (`dash.states`) | dashboard | Timeline (7); n/a: separate-app links, panel menus | 10 | 3 |
| Timeline (`dash.timeline`) | dashboard | Timeline (31); n/a: separate-app links, panel menus | 34 | 3 |
| Statistics (`dash.statistics`) | dashboard | Statistics (25); n/a: separate-app links, panel menus | 28 | 3 |
| Locations (`dash.locations`) | dashboard | Travel (27); n/a: separate-app links | 29 | 2 |
| Visited (`dash.visited`) | dashboard | Travel (46); n/a: separate-app links | 48 | 2 |
| Updates (`dash.updates`) | dashboard | Software (14); n/a: separate-app links | 16 | 2 |
| Database Information (`dash.database-info`) | dashboard | Data health and storage (17); n/a: database internals, separate-app links | 52 | 35 |
| Settings: per-car section (`web.settings.car`) | setting-section | Settings: Cars (6), Car overview (1); n/a: polling and sleep control | 12 | 5 |
| Settings: General Settings (`web.settings.general`) | setting-section | Settings: Display and units (12), Fleet (1); n/a: hosting variables, Tesla account and tokens | 19 | 6 |
| Settings: version, links and sign out (`web.settings.about`) | setting-section | Help and about (4); n/a: Tesla account and tokens | 5 | 1 |
| Sign in (`web.signin`) | web-page | Not applicable: replaced by Sentry Mate accounts (Settings > Server and access) and CAN Bridge pairing with per-car upload tokens and keys (Settings > Cars) | 7 | 7 |
| TeslaFi Import (`web.import`) | flow | Import (11); n/a: TeslaFi folder import | 28 | 17 |
| Geo-Fences (`web.geofences.list`) | web-page | Places (9) | 9 | 0 |
| Geo-Fence create / edit (`web.geofences.form`) | web-page | Places (17) | 17 | 0 |
| Charge Cost (`web.charge-cost`) | drilldown | Charge detail (17) | 17 | 0 |
| Drive GPX export (`web.drive-gpx`) | integration | Drive detail (6) | 6 | 0 |
| Legal texts (`web.legal`) | web-page | Help and about (2) | 2 | 0 |
| Health check and logging control API (`web.health-api`) | integration | Data health and storage (3); n/a: polling and sleep control | 5 | 2 |

## Information architecture

TeslaMate spreads its views over 20 dashboards, 3 internal dashboards (two drill-downs and a landing page), 1 report and 7 web pages (plus a GPX download and two licence pages), in two separate tools. Sentry Mate has 22 pages in one app. Pages are merged where TeslaMate splits one question over several dashboards; nothing is lost.

| Menu group | Page | Route | Takes over (main TeslaMate tabs) |
|---|---|---|---|
| Cars | [Fleet](#page-fleet) | `/` | Home / car overview |
| Cars | [Car overview](#page-car) | `/cars/{car}` | Car summary card, Overview |
| Cars | [Timeline](#page-timeline) | `/cars/{car}/timeline` | Timeline, States |
| Driving | [Drives](#page-drives) | `/cars/{car}/drives` | Drives, Drive Stats |
| Driving | [Drive detail](#page-drive) | `/cars/{car}/drives/{drive}` | Drive Details, Drive GPX export |
| Driving | [Trip](#page-trip) | `/cars/{car}/trip?from=&to=` | Trip |
| Driving | [Logbook](#page-logbook) | `/cars/{car}/logbook` | Drives - Dutch tax |
| Charging | [Charges](#page-charges) | `/cars/{car}/charges  (also /charges?cars=all)` | Charges, Charging Stats |
| Charging | [Charge detail](#page-charge) | `/cars/{car}/charges/{charge}` | Charge Details, Charge Cost |
| Battery and energy | [Battery](#page-battery) | `/cars/{car}/battery` | Battery Health, Vampire Drain, Charge Level, Projected Range |
| Battery and energy | [Efficiency and temperatures](#page-efficiency) | `/cars/{car}/efficiency` | Efficiency, Temperatures |
| Reports | [Statistics](#page-statistics) | `/cars/{car}/statistics  (also /statistics?cars=all)` | Statistics, Mileage |
| Reports | [Travel](#page-travel) | `/travel  (one car or many)` | Visited, Locations |
| Reports | [Software](#page-software) | `/cars/{car}/software  (also fleet versions)` | Updates |
| Set-up | [Places](#page-places) | `/places` | Geo-Fence create / edit, Geo-Fences |
| Set-up | [Settings: Cars](#page-settings-cars) | `/settings/cars/{car}` | Settings: per-car section |
| Set-up | [Settings: Display and units](#page-settings-display) | `/settings/display` | Settings: General Settings, App frame |
| Set-up | [Settings: Integrations](#page-settings-integrations) | `/settings/integrations` | MQTT integration, Home Assistant integration, Environment variables: MQTT broker |
| Set-up | [Settings: Server and access](#page-settings-server) | `/settings/server` | App frame |
| Set-up | [Import](#page-import) | `/settings/import` | TeslaFi Import |
| Set-up | [Data health and storage](#page-health) | `/cars/{car}/health  and  /system` | Database Information, Health check and logging control API |
| Set-up | [Help and about](#page-help) | `/help` | Settings: version, links and sign out, Legal texts |
| Every page | [App frame](#page-frame) | `(shared by every page)` | App frame |

## Pages

<a id="page-fleet"></a>

### Fleet

**Route:** `/`  
**Purpose:** See every car at once, find the one that needs attention, and open it.

**Covers items from:** Home / car overview (`web.cars`, 5), Frequently Asked Questions (`docs.faq`, 2), Settings: General Settings (`web.settings.general`, 1).

**Data:** 2 have, 6 derive.

**Filters:** Search; State; Sort column.

**Drill-downs:** Car row -> Car overview; Attention item -> the page that shows the cause; Waiting key -> Settings: Cars.

**Sections**

- **Car list.** Compact sortable list, one row per car: name, model and year, state and since when, battery %, range, place, plugged in, charging power, data age. Search by name; paged by cursor.
  Covers: `cars.list.cards`, `cars.list.order`, `cars.list.live_refresh`, `faq.null-panels`
- **Attention.** Cars that need a look, with the cause: data stopped mid-drive or mid-charge, no upload for a long time, logs failing their check, low battery, a tyre below the recommended pressure, a door open while asleep.
  New in Sentry Mate (no TeslaMate item).
- **Fleet totals.** Counts per state (driving, charging, parked, asleep, unknown); stale cars counted once.
  New in Sentry Mate (no TeslaMate item).
- **Empty state.** When no car shows, say exactly why and link the fix: no upload token yet, token made but never used, a CAN Bridge key waiting to be assigned, logs waiting for trust.
  Covers: `cars.empty.title`, `cars.empty.hint`
- **Car order.** Each person can pin cars or set their own order. Default order is by last activity.
  Covers: `faq.reorder-cars`, `web.settings.general.car-order`

**Better than TeslaMate**

- Works for any number of cars: a list you can search, sort and page, not one large card per car.
- An attention list says which car needs a look and why, instead of making you open each card.
- The empty state names the real cause and links the fix. There is no 'reload vehicles' button to guess with.
- Car order is per person and stored by Sentry Mate, not a browser setting.

**Data gaps**

Not on the CAN bus, and what Sentry Mate does instead:

- Car display order: A per-person order stored by Sentry Mate; default by last activity.


<a id="page-car"></a>

### Car overview

**Route:** `/cars/{car}`  
**Purpose:** What the car is doing now, why it is awake, and the last day at a glance. Merges TeslaMate's live car card and its Overview dashboard.

**Covers items from:** Car summary card (`web.car-summary`, 41), Overview (`dash.overview`, 29), Node-RED integration (`docs.node-red`, 11), Screenshots (`docs.screenshots`, 2), Settings: per-car section (`web.settings.car`, 1).

**Data:** 39 have, 16 derive, 21 new signal, 8 not on CAN.

**Filters:** Car (header switcher); Period for the charts (default 24 h).

**Drill-downs:** Activity strip -> Timeline; Battery chart -> Battery; Charging chart -> Charges or the open charge; Odometer tile -> Statistics; Software tile -> Software; Data age -> Data health.

**Sections**

- **Header.** Name, model and year from the VIN prefix, and the trim from the car's own configuration message, each with its source.
  Covers: `summary.header.name`, `summary.header.model`, `summary.header.trim`
- **Map.** Current position on offline map tiles, with GPS accuracy and heading. Full-screen and zoom. An outside map link only if the owner turns it on.
  Covers: `summary.map.position`, `summary.map.attribution`, `summary.map.fullscreen`, `summary.map.external_link`, `summary.map.zoom`
- **State.** Driving, charging, parked, asleep or unknown, and since when. Shows when the vehicle bus is awake but the chassis bus sleeps (parked but kept awake). Service mode shown here.
  Covers: `summary.stat.status`, `summary.state.in_service`, `summary.state.drive_offline`
- **Why the car is awake.** Plain facts from the car: Sentry mode, keep-climate mode (keep, dog, camp), cabin or battery preconditioning, someone in the car, charging, battery kept warm, a door open, accessory power.
  Covers: `web.settings.car.sleep-blockers`
- **Status flags.** Each door, trunk, frunk and liftgate by name; Sentry mode and a labelled guess at a Sentry event; plugged in; charge port open; reduced range in the cold. Lock and window state are not on the CAN bus and are left out; a pending software update becomes 'software changed' (see Software).
  Covers: `summary.flag.preconditioning`, `summary.flag.dog_mode`, `summary.flag.reduced_range`, `summary.flag.driver_present`, `summary.flag.plugged_in`, `summary.flag.service_mode`, `summary.flag.windows_open`, `summary.flag.doors_open`, `summary.flag.sentry_mode`, `summary.flag.sentry_recording`, `summary.flag.lock`, `summary.flag.software_update`
- **Tyres.** All four pressures at all times, with the car's recommended pressure and a warning per tyre.
  Covers: `summary.flag.tire_pressure_warning`
- **Battery and range.** Battery %, usable % when it differs, rated or ideal range (the person's choice), energy remaining in kWh, range at a full charge (shown in the open, not only on hover), and Sentry Mate's own estimated range with its basis.
  Covers: `summary.stat.range_preferred`, `summary.stat.range_estimated`, `summary.stat.soc`
- **Charging now.** Charging status as a word, AC or DC, power, volts, amps, phases, energy added, charge limit, energy still to add, time to full and finish time with date in the car's time zone.
  Covers: `summary.stat.remaining_time`, `summary.stat.expected_finish`, `summary.stat.energy_added`, `summary.stat.charger_power`, `summary.stat.scheduled_charging`, `summary.stat.charge_limit`
- **Live tiles.** Speed, power, outside and cabin temperature, climate set point, odometer, 12 V battery.
  Covers: `summary.stat.speed`, `summary.stat.outside_temp`, `summary.stat.inside_temp`, `summary.stat.odometer`, `nr.dash.panel.car-status`, `nr.dash.panel.charge-status`, `nr.dash.time-to-full`, `nr.dash.charge-limit`, `nr.dash.battery-level`, `nr.dash.battery-level.bands`, `nr.dash.charging-current`, `nr.dash.charging-power`, `nr.dash.ideal-range`, `nr.dash.est-range`, `nr.dash.last-update`, `overview.stat.battery_level`, `overview.stat.ac_voltage`, `overview.stat.charging_power`, `overview.stat.range`, `overview.stat.odometer`, `overview.link.mileage`, `overview.stat.driver_temp`, `overview.stat.outside_temp`, `overview.stat.inside_temp`
- **Software.** Last known version with its date and source (imported or typed by the owner), and 'software changed since' when the car's module build ids change.
  Covers: `summary.stat.version`, `overview.stat.firmware`, `overview.link.updates`
- **Data age and units.** Every value shows its own time. A line explains the normal delay (about 75 s) and when values are older. Units follow the person's preferences.
  Covers: `summary.data.live_push`, `summary.data.restored_values`, `summary.data.prefs_snapshot`
- **Window controls.** Shared period picker (default last 24 hours), legend toggles and a shared hover line across charts. The page updates itself when a new log has been processed; there is no timed re-query.
  Covers: `overview.filter.car`, `overview.control.time_range`, `overview.control.refresh`, `overview.control.shared_crosshair`, `overview.chart.legend_toggle`
- **Window summary.** Distance driven, net and gross consumption for the period, each with a one-line explanation.
  Covers: `overview.stat.consumption_net`, `overview.stat.consumption_gross`, `overview.stat.distance_logged`
- **Battery chart.** Battery % and range over the period, with asleep and unknown spans drawn differently.
  Covers: `overview.chart.charge_level`, `overview.link.charge_level`
- **Charging chart.** Charging power, energy added, AC volts and amps (DC charger volts and amps for DC) and battery heating over the period; lines break between sessions.
  Covers: `overview.chart.charging_details.power`, `overview.chart.charging_details.energy`, `overview.chart.charging_details.ac_current`, `overview.chart.charging_details.ac_voltage`, `overview.chart.charging_details.battery_heater`, `overview.link.charging_stats`
- **Activity strip.** The same state strip as the Timeline for the period, linking to it.
  Covers: `overview.chart.states`, `overview.link.states`
- **Whole page (docs screenshot).** The documentation gallery picture of this TeslaMate page.
  Covers: `shots.web-interface`, `shots.overview`

**Better than TeslaMate**

- Says why the car is awake from the car's own frames (Sentry, keep-climate, preconditioning, someone present, charging). TeslaMate can only guess and offers a sleep button.
- The logger never polls, so it adds no drain and needs no sleep settings, idle timers or 'try to sleep' button.
- Names which door, trunk, frunk or liftgate is open, not one generic warning.
- All four tyre pressures always visible, with the car's own recommended pressure.
- Every value carries its own time and survives restarts; nothing is hidden after 30 minutes asleep (as on the TeslaMate car card) or left blank after 60 minutes (as on its overview dials).
- Charging shows AC or DC from the charge port itself, phases, energy still to add and the finish time with its date.
- Cabin and battery preconditioning are shown apart, and every keep-climate mode is named.
- The live card and the 24-hour overview are one page with one period picker.

**Data gaps**

New CAN signals to decode:

- Sentry mode on: Model3CAN.dbc 0x284 UIvehicleModes: UIsentryMode284. Bus: Vehicle bus (can0). Check: seen.
- Service mode: Model3CAN.dbc 0x284 UIvehicleModes: UIserviceMode284. Bus: Vehicle bus (can0). Check: present, to check.
- Each door, trunk, frunk and liftgate: Model3CAN.dbc 0x102 / 0x103 latch status, 0x2E1 page 0 VCFRONT_frunkLatchStatus, 0x142 page 0 VCLEFT_liftgateState (page 1 liftgate angle). Bus: Vehicle bus (can0). Check: seen.
- Climate on, preconditioning, overheat protection, fan, set points: override.dbc 0x243 page 0 VCRIGHT_hvacPowerState, hvacBlowerSegment, hvacACRunning; Model3CAN.dbc 0x2F3 UI_hvacReqUserPowerState, UI_hvacReqTempSetpointLeft/Right (raw 0 = LO, 26 = HI). Bus: Vehicle bus (can0). Check: seen.
- Keep-climate mode (keep, dog, camp): Model3CAN.dbc 0x2F3 UI_hvacReqKeepClimateOn. Bus: Vehicle bus (can0). Check: present, to check.
- Battery preconditioning (apart from the cabin): Model3CAN.dbc 0x3B3 UI_batteryPreconditioningRequest; 0x082 UI_requestActiveBatteryHeating. Bus: Vehicle bus (can0). Check: present, to check.
- Someone in the car: Model3CAN.dbc 0x3B3 UI_userPresent; 0x3C2 page 0 VCLEFT_frontOccupancySwitch (0x3A1 is blanked by override.dbc). Bus: Vehicle bus (can0). Check: present, to check.
- Charging status word, cable connected, charge port open: Model3CAN.dbc 0x212 BMS_uiChargeStatus (already decoded, kept only as true/false), 0x25D CP_chargeCableState, CP_chargeDoorOpen. Bus: Vehicle bus (can0). Check: present, to check.
- Charge limit, AC current set on the screen, scheduled-departure flags: Model3CAN.dbc 0x333 UI_chargeTerminationPct, UI_acChargeCurrentLimit, UI_scheduledDepartureEnabled, UI_smartAcChargingEnabled. Bus: Vehicle bus (can0). Check: present, to check.
- Energy still to add, time to full: override.dbc 0x352 page 1 BMS_energyToChargeComplete; Model3CAN.dbc 0x132 ChargeHoursRemaining132 (minutes despite the name). Bus: Vehicle bus (can0). Check: seen.
- Full-pack energy, buffer, expected (cold-limited) energy: override.dbc 0x352 page 0 BMS_nominalFullPackEnergy, page 1 BMS_energyBuffer, BMS_expectedEnergyRemaining. Bus: Vehicle bus (can0). Check: seen.
- Usable battery % (cold-locked part removed): Model3CAN.dbc 0x33A UI_uSOE; fallback from 0x352 energy and buffer. Bus: Vehicle bus (can0). Check: present, to check.
- Phases in use, pilot current, which current limit applies, AC line power: Model3CAN.dbc 0x204 PCS_chgPHA/B/CEnable and PCS_gridConfig; 0x31C CC_numPhases; 0x21D CP_pilotCurrent, CP_cableCurrentLimit; 0x13D CP_internalMaxAcCurrentLimit; 0x264 ChargeLinePower264, ChargeLineCurrentLimit264. Bus: Vehicle bus (can0). Check: present, to check.
- Car build: trim, drivetrain, pack size, wheels, paint, roof, spoiler, right-hand drive, map region, Supercharging access: Model3CAN.dbc 0x7FF carConfig GTW_* (paged). Bus: Vehicle bus (can0). Check: present, to check.
- Software changed (module build ids): Model3CAN.dbc page 17 of each info message: 0x300 BMS_appGitHash, 0x301 VCFRONT_infoAppGitHash, 0x51E FC_infoAppGitHashBytes, 0x335 DIR_appGitHash, 0x656 DIF_appGitHash. Bus: Vehicle bus (can0). Check: present, to check.
- Heading: Model3CAN.dbc 0x3D9 UI_gpsVehicleHeading (never decode can3's 0x3D9). Bus: Chassis bus (can1). Check: not seen yet.
- Battery heating on, kept warm, heating power: Model3CAN.dbc 0x2E1 page 0 VCFRONT_isActiveHeatingBattery; 0x212 BMS_keepWarmRequest; 0x266 RearHeatPower266, 0x2E5 FrontHeatPower2E5, 0x268 SystemHeatPower268. Bus: Vehicle bus (can0). Check: present, to check.
- Recommended tyre pressure: override.dbc 0x219 page 4 VCSEC_TPMSRecommendedPressure0/1 (0x123 TPMS soft warning is blanked by override.dbc). Bus: Vehicle bus (can0). Check: seen.
- Centre display on, camera active (Sentry event guess): Model3CAN.dbc 0x00C and 0x353 UI_displayOn and UI_cameraActive. Bus: Vehicle bus (can0). Check: present, to check.
- Accessory power kept on: Model3CAN.dbc 0x273 UI_accessoryPowerRequest. Bus: Vehicle bus (can0). Check: not seen yet.
- Parking brake engaged: Model3CAN.dbc 0x228 EPBRunitStatus228, 0x288 EPBLunitStatus288. Bus: Vehicle bus (can0). Check: not seen yet.
- Most charge current the pack accepts, DC station limit: Model3CAN.dbc 0x2D2 MaxChargeCurrent2D2; 0x27D CP_evseMaxDcCurrentLimit. Bus: Vehicle bus (can0). Check: not seen yet.
- Legacy Model S/X doors, frunk, boot, driver present, update in progress: opendbc-tesla_can.dbc 0x318 GTW_carState, 0x348 GTW_status. Bus: Legacy set (as the DBC crate maps it). Check: not seen yet.

Not on the CAN bus, and what Sentry Mate does instead:

- Locked or unlocked: Leave out. No bundled DBC has a lock state (0x273 is only a request). Record locking and unlocking to hunt for it among the undecoded messages.
- Window open or closed: Leave out. Only switch presses and requests exist. Doors, trunk, frunk and liftgate cover 'left open' reminders.
- Update available, download and install progress: Leave out. Show 'installing' from the car's update status (once confirmed) and 'software changed' after the fact.
- The software version text and the update history: Show imported TeslaMate updates with their version text; let the owner type the current version; mark 'software changed' when module build ids change, and remember each set of build ids with its version so other cars on the same build get it. Never show a guessed version.
- Scheduled charging start time: Show 'plugged in, waiting' from the charge status and the schedule flags, and 'usually starts at HH:MM here' learned from past charges at that place.
- Booked in at a service centre: Show service mode from the car, and the place name when parked at a place of kind 'service centre'.
- The car's own 'estimated range': Sentry Mate's estimate: energy remaining divided by recent measured consumption, labelled as Sentry Mate's.


<a id="page-timeline"></a>

### Timeline

**Route:** `/cars/{car}/timeline`  
**Purpose:** Everything the car did, as one list and one strip: drives, charges, parked spells, gaps and updates. Merges TeslaMate's Timeline and States dashboards.

**Covers items from:** Timeline (`dash.timeline`, 31), States (`dash.states`, 7), Frequently Asked Questions (`docs.faq`, 5), Screenshots (`docs.screenshots`, 2), Projects using TeslaMate (`docs.projects`, 1).

**Data:** 20 have, 24 derive, 1 new signal, 1 not on CAN.

**Filters:** Period (default last 7 days); Kinds of entry; Place; Text search.

**Drill-downs:** Row -> Drive detail, Charge detail or Software; Row time -> Trip for that span; Place cell -> Places editor.

**Sections**

- **Filters.** Kinds of entry, place, a text search that only hides rows and never changes their numbers, and the shared period picker.
  Covers: `states.filter.car`, `states.control.time_range`, `timeline.filter.car`, `timeline.filter.action`, `timeline.filter.address`, `timeline.control.time_range`, `timeline.table.column_filter`
- **State summary.** Current state, last change, and the share of time parked, asleep, awake and unknown.
  Covers: `states.stat.last_change`, `states.stat.current`, `states.stat.parked_pct`
- **State strip.** Driving, charging, online, online with only the vehicle bus, asleep, unknown and updating, in fixed colours with a legend.
  Covers: `states.chart.states`, `states.chart.legend`
- **Timeline rows.** Newest first, paged. Drive, charge, parked (split into asleep, awake and unknown time, with the real average temperature), gap (with its reason: no log, recording paused, failed check, dropped frames) and update rows. Columns: start time, kind, start and end place, odometer, duration, distance, range at the end and its change, energy change (measured or estimated), battery % at the end and its change, average outside temperature.
  Covers: `projects.dash`, `timeline.table`, `timeline.row.drive`, `timeline.row.charging`, `timeline.row.parking`, `timeline.row.missing`, `timeline.row.parking_gap_overlap`, `timeline.row.updating`, `timeline.col.start`, `timeline.col.end`, `timeline.col.action`, `timeline.col.start_address`, `timeline.col.end_address`, `timeline.col.odometer`, `timeline.col.duration`, `timeline.col.distance`, `timeline.col.range`, `timeline.col.range_diff`, `timeline.col.energy_diff`, `timeline.col.soc`, `timeline.col.soc_diff`, `timeline.col.temp`
- **Row links.** Open the drive or charge, open the Trip for exactly the row's span, or create or edit the place, prefilled from the recorded position. Only a cell that shows a place carries a place link.
  Covers: `timeline.link.trip_span`, `timeline.link.slot_details`, `timeline.link.geofence_start`, `timeline.link.geofence_end`, `timeline.link.place_cells_other_rows`
- **Why the car stayed awake.** On parked rows: the reasons from the car (Sentry, climate, preconditioning, charging, keep-warm, accessory power), with short help next to them.
  Covers: `faq.no-sleep.accessory`, `faq.no-sleep.mcu1`, `faq.no-sleep.log-timeline`, `faq.no-sleep.state-image`, `faq.no-sleep.other-loggers`
- **Whole page (docs screenshot).** The documentation gallery picture of this TeslaMate page.
  Covers: `shots.states`, `shots.timeline`

**Better than TeslaMate**

- Drives, charges, parked time and gaps on one timeline, with the state strip above it.
- Asleep and unknown are different things: unknown means no log, a paused recording or a failed check. TeslaMate folds them into 'offline'.
- Gap rows say why the data is missing, not only that the odometer moved.
- Parked rows split asleep, awake and unknown time, and show their own average temperature.
- The text filter never changes the numbers it shows.
- Late uploads fill gaps by themselves; nothing needs repairing by hand.

**Data gaps**

New CAN signals to decode:

- Sentry mode on: Model3CAN.dbc 0x284 UIvehicleModes: UIsentryMode284. Bus: Vehicle bus (can0). Check: seen.
- Climate on, preconditioning, overheat protection, fan, set points: override.dbc 0x243 page 0 VCRIGHT_hvacPowerState, hvacBlowerSegment, hvacACRunning; Model3CAN.dbc 0x2F3 UI_hvacReqUserPowerState, UI_hvacReqTempSetpointLeft/Right (raw 0 = LO, 26 = HI). Bus: Vehicle bus (can0). Check: seen.
- Keep-climate mode (keep, dog, camp): Model3CAN.dbc 0x2F3 UI_hvacReqKeepClimateOn. Bus: Vehicle bus (can0). Check: present, to check.
- Someone in the car: Model3CAN.dbc 0x3B3 UI_userPresent; 0x3C2 page 0 VCLEFT_frontOccupancySwitch (0x3A1 is blanked by override.dbc). Bus: Vehicle bus (can0). Check: present, to check.
- Update being installed: Model3CAN.dbc 0x2E1 page 1 VCFRONT_vehicleStatusDBG = VEHICLE_STATUS_OTA, VCFRONT_batterySMState = OTA_STANDBY; model3y-opendbc.dbc 0x343 VCRIGHT_OTAState. Bus: Vehicle bus (can0). Check: present, to check.
- Accessory power kept on: Model3CAN.dbc 0x273 UI_accessoryPowerRequest. Bus: Vehicle bus (can0). Check: not seen yet.

Not on the CAN bus, and what Sentry Mate does instead:

- The software version text and the update history: Show imported TeslaMate updates with their version text; let the owner type the current version; mark 'software changed' when module build ids change, and remember each set of build ids with its version so other cars on the same build get it. Never show a guessed version.
- 'Offline' as Tesla's cloud reports it: Report what the logs show: online, asleep, or unknown (no log, recording paused, failed check).


<a id="page-drives"></a>

### Drives

**Route:** `/cars/{car}/drives`  
**Purpose:** Find and compare drives, and see driving patterns. Merges TeslaMate's Drives and Drive Stats dashboards.

**Covers items from:** Drives (`dash.drives`, 39), Drive Stats (`dash.drive-stats`, 18), Screenshots (`docs.screenshots`, 2).

**Data:** 15 have, 43 derive, 1 new signal.

**Filters:** Period; Places; Text; Minimum distance; Minimum average speed; Source; Flags; Leave out places (patterns); Efficiency basis.

**Drill-downs:** Drive row -> Drive detail; Place cell -> Places editor; Destination bar -> Drives filtered to that place; Select rows -> Trip.

**Sections**

- **Filters.** Shared period, places (start, end or either), text search, minimum distance, minimum average speed, source (CAN or imported), flags (gap, in progress, reduced range), places to leave out of patterns, and the efficiency basis.
  Covers: `drives.filter.car`, `drives.filter.time_range`, `drives.filter.geofence`, `drives.filter.location`, `drives.filter.location.hint`, `drives.filter.min_distance`, `drives.filter.min_speed`, `drives.setting.efficiency_method`, `ds.filter.car`, `ds.filter.time_range`, `ds.filter.exclude_locations`, `ds.filter.exclude_locations.hint`
- **Summary tiles.** Count, distance, time, energy and average consumption of the listed drives, saying how many drives have measured energy.
  Covers: `drives.summary.energy`, `drives.summary.duration`, `drives.summary.distance`, `drives.summary.consumption`
- **Drive list.** Sortable list with date, start and end place, duration, distance, battery % at start and end, outside temperature, average and top speed, peak power, energy (marked measured or estimated), Wh/km and efficiency. Empty results say which filter removed everything.
  Covers: `drives.table`, `drives.table.date`, `drives.table.start`, `drives.table.destination`, `drives.table.duration`, `drives.table.distance`, `drives.table.soc_start`, `drives.table.soc_end`, `drives.table.outside_temp`, `drives.table.speed_avg`, `drives.table.speed_max`, `drives.table.power_max`, `drives.table.reduced_range`, `drives.table.efficiency`, `drives.table.energy`, `drives.table.consumption`, `drives.table.link.details`, `drives.table.link.geofence_start`, `drives.table.link.geofence_end`, `drives.empty_states`
- **Drives with missing data.** Drives that overlap a gap, are still in progress, or wait for late logs, with a rebuild action for admins.
  Covers: `drives.incomplete.note`, `drives.incomplete.table`, `drives.incomplete.id`, `drives.incomplete.start`, `drives.incomplete.end`, `drives.incomplete.distance`, `drives.incomplete.duration`
- **Driving patterns.** Count, distance and energy for the period with day bars, median drive length, distance and energy per day, top speed (period, last 30 days, last 7 days), seconds spent in each speed band, projected monthly and yearly distance, and the most frequent destinations.
  Covers: `ds.stat.drive_count`, `ds.stat.distance_total`, `ds.stat.energy_total`, `ds.stat.median_distance`, `ds.stat.distance_per_day`, `ds.stat.energy_per_day`, `ds.stat.max_speed_period`, `ds.stat.max_speed_30d`, `ds.stat.max_speed_7d`, `ds.chart.speed_histogram`, `ds.stat.monthly_mileage`, `ds.stat.annual_mileage`, `ds.chart.top_destinations`, `ds.empty_states`
- **Whole page (docs screenshot).** The documentation gallery picture of this TeslaMate page.
  Covers: `shots.drive-stats`, `shots.drives`

**Better than TeslaMate**

- Drive energy is measured at the pack every second, with regen taken off. TeslaMate multiplies range lost by a learned factor.
- Each drive says whether its energy is measured or estimated (imported history).
- Speed bands count real seconds at 1 Hz, so averages and top speeds are exact.
- One filter bar drives both the list and the patterns; leaving out home or work is a pick list, not a text box.
- Drives with missing data are listed with the cause and fixed by a rebuild, not a database repair guide.

**Data gaps**

New CAN signals to decode:

- Full-pack energy, buffer, expected (cold-limited) energy: override.dbc 0x352 page 0 BMS_nominalFullPackEnergy, page 1 BMS_energyBuffer, BMS_expectedEnergyRemaining. Bus: Vehicle bus (can0). Check: seen.
- Usable battery % (cold-locked part removed): Model3CAN.dbc 0x33A UI_uSOE; fallback from 0x352 energy and buffer. Bus: Vehicle bus (can0). Check: present, to check.
- Climate energy used per drive: Model3CAN.dbc 0x381 page 8 VCFRONT_cabinHeatEnergyDuringDri, VCFRONT_compEnergyDuringDrive (150 Wh steps). Bus: Vehicle bus (can0). Check: present, to check.

Not on the CAN bus, and what Sentry Mate does instead:

- Car mass for hill-corrected efficiency: Typical mass per model and year from the VIN prefix, with an owner override; or drop the correction because energy is measured.


<a id="page-drive"></a>

### Drive detail

**Route:** `/cars/{car}/drives/{drive}`  
**Purpose:** One drive in full: route, charts and figures. Replaces TeslaMate's Drive Details and the GPX export.

**Covers items from:** Drive Details (`dash.drive-details`, 29), Drive GPX export (`web.drive-gpx`, 6), Screenshots (`docs.screenshots`, 1).

**Data:** 20 have, 12 derive, 4 new signal.

**Filters:** Zoom window inside the drive.

**Drill-downs:** Previous / next drive; Start or end place -> Places editor; Charge before or after -> Charge detail; Drive span -> Trip.

**Sections**

- **Header and zoom.** Car and drive in a breadcrumb, previous and next drive, and zoom by dragging across a chart; the figures follow the zoom.
  Covers: `dd.context.drive`, `dd.filter.car`, `dd.control.zoom`
- **Route map.** Route at 1 Hz with GPS accuracy per point, coloured by speed or power.
  Covers: `dd.map.route`
- **Drive chart.** Speed, battery power, battery % and usable %, range, elevation and battery heating power on one time axis with one hover readout.
  Covers: `dd.chart.drive`, `dd.chart.drive.tooltip`, `dd.chart.drive.speed`, `dd.chart.drive.power`, `dd.chart.drive.range`, `dd.chart.drive.soc`, `dd.chart.drive.battery_heater`, `dd.chart.elevation`
- **Climate.** Outside and cabin temperature, both set points (LO and HI shown as words), climate on, preconditioning or overheat protection, and fan level.
  Covers: `dd.chart.temps.air`, `dd.chart.temps.setpoints`, `dd.chart.temps.climate_state`
- **Tyres.** All four tyre pressures through the drive.
  Covers: `dd.chart.tire_pressure`
- **Figures.** Odometer at start and end, duration, distance, climb and descent, net energy, regen energy, Wh/km, average speed and seconds per speed band. A blank figure says why it is blank.
  Covers: `dd.stat.odometer`, `dd.stat.drive_duration`, `dd.stat.selected_duration`, `dd.chart.speed_histogram`, `dd.stat.distance`, `dd.stat.elevation_up`, `dd.stat.elevation_down`, `dd.stat.energy_net`, `dd.stat.energy_recovered`, `dd.stat.consumption_net`, `dd.stat.speed_avg`, `dd.empty_states`
- **Export.** GPX and GeoJSON of the route with time, elevation and accuracy, optionally speed, power and battery %.
  Covers: `dd.export.gpx`, `web.drive-gpx.entry`, `web.drive-gpx.file`, `web.drive-gpx.point-coordinates`, `web.drive-gpx.point-elevation`, `web.drive-gpx.point-time`, `web.drive-gpx.not-found`
- **Whole page (docs screenshot).** The documentation gallery picture of this TeslaMate page.
  Covers: `shots.drive-details`

**Better than TeslaMate**

- Regen energy is measured, not guessed.
- Elevation is always the car's own reading; no outside terrain lookup.
- Battery heating is shown as kW spent, not only on or off.
- Climate shows both set points, preconditioning and overheat protection, not one temperature line.
- The export is a visible button with a choice of format and extras.

**Data gaps**

New CAN signals to decode:

- Climate on, preconditioning, overheat protection, fan, set points: override.dbc 0x243 page 0 VCRIGHT_hvacPowerState, hvacBlowerSegment, hvacACRunning; Model3CAN.dbc 0x2F3 UI_hvacReqUserPowerState, UI_hvacReqTempSetpointLeft/Right (raw 0 = LO, 26 = HI). Bus: Vehicle bus (can0). Check: seen.
- Usable battery % (cold-locked part removed): Model3CAN.dbc 0x33A UI_uSOE; fallback from 0x352 energy and buffer. Bus: Vehicle bus (can0). Check: present, to check.
- Heading: Model3CAN.dbc 0x3D9 UI_gpsVehicleHeading (never decode can3's 0x3D9). Bus: Chassis bus (can1). Check: not seen yet.
- Battery heating on, kept warm, heating power: Model3CAN.dbc 0x2E1 page 0 VCFRONT_isActiveHeatingBattery; 0x212 BMS_keepWarmRequest; 0x266 RearHeatPower266, 0x2E5 FrontHeatPower2E5, 0x268 SystemHeatPower268. Bus: Vehicle bus (can0). Check: present, to check.
- Recommended tyre pressure: override.dbc 0x219 page 4 VCSEC_TPMSRecommendedPressure0/1 (0x123 TPMS soft warning is blanked by override.dbc). Bus: Vehicle bus (can0). Check: seen.
- Climate energy used per drive: Model3CAN.dbc 0x381 page 8 VCFRONT_cabinHeatEnergyDuringDri, VCFRONT_compEnergyDuringDrive (150 Wh steps). Bus: Vehicle bus (can0). Check: present, to check.


<a id="page-trip"></a>

### Trip

**Route:** `/cars/{car}/trip?from=&to=`  
**Purpose:** Any stretch of time as a trip: where the car went, what it cost, and how it charged. Replaces TeslaMate's Trip dashboard and adds saved trips.

**Covers items from:** Trip (`dash.trip`, 46), Screenshots (`docs.screenshots`, 1).

**Data:** 19 have, 28 derive.

**Filters:** Window; Second car to compare.

**Drill-downs:** Drive row -> Drive detail; Charge row -> Charge detail (cost editor); Place cell -> Places editor.

**Sections**

- **Window and cars.** Any window from the shared picker, shortcuts (last three drives, a day, a statistics period, a saved trip), and a second car for side-by-side comparison.
  Covers: `trip.filter.time_range`, `trip.control.per_car`, `trip.control.zoom`, `trip.link.last_three_drives`
- **Route map.** One line per drive, with markers for start, end, charges and overnight stops.
  Covers: `trip.map.route`
- **Trip figures.** Distance by odometer and by drives, time split into driving, AC charging, DC charging, parked awake, asleep and not recorded, average speed moving and including fast-charging, net and gross consumption, charging cost and cost per 100 km. Blank figures say why.
  Covers: `trip.stat.mileage`, `trip.chart.time_spent`, `trip.stat.speed_moving`, `trip.stat.speed_incl_dc`, `trip.stat.consumption_net`, `trip.stat.consumption_gross`, `trip.stat.charging_cost`, `trip.stat.cost_per_100`, `trip.chart.energy`, `trip.empty_states`
- **Activity strip.** The state strip for the window.
  Covers: `trip.chart.states`
- **Drives.** Every drive of the trip: start time, start and end place, duration (hours and minutes), distance, battery % at start and end, and Wh/km (blank with the reason when it would be unreliable), with links.
  Covers: `trip.drives.table`, `trip.drives.date`, `trip.drives.start`, `trip.drives.destination`, `trip.drives.duration`, `trip.drives.distance`, `trip.drives.soc_start`, `trip.drives.soc_end`, `trip.drives.consumption`, `trip.drives.link.details`, `trip.drives.link.geofence_start`, `trip.drives.link.geofence_end`, `trip.drives.link.duration_hint`
- **Charges.** Every charge of the trip: start time, place, charging time, cost, energy added and drawn, average power, range added, battery % at start and end, and a link to edit the cost.
  Covers: `trip.charges.table`, `trip.charges.date`, `trip.charges.location`, `trip.charges.duration`, `trip.charges.cost`, `trip.charges.energy_added`, `trip.charges.energy_used`, `trip.charges.power_avg`, `trip.charges.range_added`, `trip.charges.soc_start`, `trip.charges.soc_end`, `trip.charges.link.details`, `trip.charges.link.geofence`, `trip.charges.link.cost`
- **Battery and elevation.** Battery % and range, and elevation, over the trip.
  Covers: `trip.chart.battery.soc`, `trip.chart.battery.range`, `trip.chart.elevation`
- **Whole page (docs screenshot).** The documentation gallery picture of this TeslaMate page.
  Covers: `shots.trip`

**Better than TeslaMate**

- Gross consumption works the same for any window length; there is no method switch at 48 hours.
- Parked time is split into awake, asleep and not recorded, so unrecorded time is never counted as sleep.
- Trips can be named and saved, and opened from any timeline or statistics row.
- With several cars, pick one or compare two instead of repeating the whole page per car.


<a id="page-logbook"></a>

### Logbook

**Route:** `/cars/{car}/logbook`  
**Purpose:** A trip log for tax and expense claims, with a purpose per drive. Replaces TeslaMate's Dutch tax report and works for any country.

**Covers items from:** Drives - Dutch tax (`dash.dutch-tax`, 13).

**Data:** 7 have, 6 derive.

**Filters:** Year or period; Purpose.

**Drill-downs:** Row -> Drive detail; Continuity gap -> Timeline at that time.

**Sections**

- **Filters.** Year or any period; one car at a time with the car's name in the heading.
  Covers: `dt.filter.time_range`, `dt.control.per_car`
- **Logbook table.** Number, start and end time, odometer at start and end, start and end address and place, distance, duration, purpose (business, private, commute), relation and note, and a continuity check that flags kilometres missing between two drives.
  Covers: `dt.table`, `dt.col.id`, `dt.col.start_time`, `dt.col.start_odometer`, `dt.col.start_address`, `dt.col.end_time`, `dt.col.end_odometer`, `dt.col.end_address`, `dt.col.duration`, `dt.col.distance`
- **Export.** CSV and PDF buttons; totals per purpose for the period.
  Covers: `dt.export.csv`

**Better than TeslaMate**

- Purpose per drive, with rules such as 'Home to Office on weekdays is commute'. Purposes survive rebuilds.
- Odometer at 1 m resolution and a continuity check, so the rows add up.
- Export is a visible button (CSV and PDF), not a hidden panel menu.

**Data gaps**

Not on the CAN bus, and what Sentry Mate does instead:

- Purpose of each drive for a tax log: Entered by the owner per drive, or by rules; stored apart from rebuilt rows.


<a id="page-charges"></a>

### Charges

**Route:** `/cars/{car}/charges  (also /charges?cars=all)`  
**Purpose:** Find charging sessions and understand charging habits and costs. Merges TeslaMate's Charges and Charging Stats dashboards.

**Covers items from:** Charges (`dash.charges`, 42), Charging Stats (`dash.charging-stats`, 37), Screenshots (`docs.screenshots`, 2).

**Data:** 24 have, 56 derive, 1 new signal.

**Filters:** Period; Cars; Places; AC/DC; Text; Minimum cost; Minimum duration; Source; Flags (missing data, under way).

**Drill-downs:** Session row -> Charge detail; Cost cell -> Charge detail cost editor; Place -> Places editor; Curve dot -> Charge detail.

**Sections**

- **Filters.** Shared period, cars (one, several or all), places, place kind, AC/DC/unknown, text search, minimum cost (with or without unpriced sessions), minimum duration and source.
  Covers: `charges.filter.car`, `charges.filter.time_range`, `charges.filter.geofence`, `charges.filter.location`, `charges.filter.charge_type`, `charges.filter.min_cost`, `charges.filter.min_duration`, `cs.filter.car`, `cs.filter.time_range`, `cs.filter.min_duration`, `cs.filter.geofence`
- **Summary tiles.** Energy added, energy drawn from the grid (AC, measured), cost per currency, average duration; each tile says how many sessions it counts.
  Covers: `charges.stat.energy_added`, `charges.stat.energy_used`, `charges.stat.cost`, `charges.stat.avg_duration`
- **Session list.** Date, place, AC or DC, charging time and plugged-in time, cost and price per kWh (with the basis), energy added and drawn, efficiency (AC only; DC shown as unknown with the reason), outside and battery temperature, average power, range added and range added per hour of charging, battery % at start and end, odometer. Links to edit cost and place.
  Covers: `charges.table`, `charges.text.list_help`, `charges.table.date`, `charges.link.view_details`, `charges.table.location`, `charges.link.geofence`, `charges.table.type`, `charges.table.duration`, `charges.table.cost`, `charges.link.set_cost`, `charges.table.cost_per_kwh`, `charges.table.energy_added`, `charges.table.energy_used`, `charges.table.efficiency`, `charges.table.temp`, `charges.table.avg_power`, `charges.table.charge_rate`, `charges.table.range_added`, `charges.table.start_soc`, `charges.table.end_soc`, `charges.table.odometer`
- **Charges with missing data.** Charges that overlap an unknown span or dropped frames, whose logs failed their check, or that are still under way: start, last recorded second and where the gap lies, energy added and drawn, battery % at start and end, duration, each linked to its charge page, with a rebuild action for admins. A short note explains each mark.
  Covers: `charges.text.incomplete_help`, `charges.incomplete.table`, `charges.incomplete.col.id`, `charges.incomplete.col.start_date`, `charges.incomplete.col.end_date`, `charges.incomplete.col.energy_added`, `charges.incomplete.col.energy_used`, `charges.incomplete.col.start_soc`, `charges.incomplete.col.end_soc`, `charges.incomplete.col.duration`
- **Costs and totals.** Sessions, energy, total cost per currency, Supercharger cost, price per kWh overall, AC and DC, and cost per 100 km with its method.
  Covers: `cs.stat.count`, `cs.stat.energy_added`, `cs.stat.supercharger_cost`, `cs.stat.total_cost`, `cs.stat.cost_per_100`, `cs.stat.cost_per_kwh`, `cs.stat.cost_per_kwh_dc`, `cs.stat.cost_per_kwh_ac`
- **Charging habits.** Where sessions usually start and end (bands and a heatmap over time), the start-to-end band per session against the recommended daily band, and runs of back-to-back sessions. Every chart follows the shared period.
  Covers: `cs.filter.heatmap_window`, `cs.chart.heatmap`, `cs.filter.charge_delta_window`, `cs.chart.charge_delta`, `cs.chart.charge_delta.band`, `cs.control.shared_crosshair`, `cs.table.charge_stats`, `cs.table.charge_stats.soc`, `cs.table.charge_stats.count`, `cs.table.discharge_stats`, `cs.text.discharge_stats_help`, `cs.table.discharge_stats.soc`, `cs.table.discharge_stats.count`
- **AC and DC split.** Energy and time by AC, DC and unknown.
  Covers: `cs.chart.ac_dc_energy`, `cs.chart.ac_dc_duration`
- **Charging places.** Map and tables of places by energy and by cost, grouped by saved place or position, paged.
  Covers: `cs.map.energy_by_location`, `cs.table.top_energy`, `cs.table.top_energy.location`, `cs.table.top_energy.charged`, `cs.table.top_cost`, `cs.table.top_cost.location`, `cs.table.top_cost.cost`
- **Fast-charging curve.** Power against battery % for every DC session, coloured by battery temperature, with this car's median curve; a dot opens its charge.
  Covers: `cs.chart.dc_curve_sessions`, `cs.chart.dc_curve_median`, `cs.link.dc_curve_details`
- **Whole page (docs screenshot).** The documentation gallery picture of this TeslaMate page.
  Covers: `shots.charges`, `shots.charging-stats`

**Better than TeslaMate**

- AC or DC comes from the charge port's own signal, not a guess from phase counts; a session with no type shows as Unknown, never as DC.
- Charging time and plugged-in time are separate, so average power is not diluted by waiting time.
- Energy added comes from the pack's lifetime counter where it covers the session.
- DC efficiency is shown as unknown with the reason, never as a fake 100 %.
- Costs are per currency, and every habit chart follows the chosen period instead of fixed six-month windows.
- Places are grouped by saved place or position, never by address text, so one spot is never split in two.
- Sessions with missing data are listed with the cause and rebuilt from the logs, not repaired by hand from a guide.

**Data gaps**

New CAN signals to decode:

- Charge limit, AC current set on the screen, scheduled-departure flags: Model3CAN.dbc 0x333 UI_chargeTerminationPct, UI_acChargeCurrentLimit, UI_scheduledDepartureEnabled, UI_smartAcChargingEnabled. Bus: Vehicle bus (can0). Check: present, to check.
- Lifetime AC, DC, regen and drive energy: Model3CAN.dbc 0x3F2 pages 0-3 BMS_acChargerKwhTotal, BMS_dcChargerKwhTotal, BMS_kwhRegenChargeTotal, BMS_kwhDriveDischargeTotal. Bus: Vehicle bus (can0). Check: seen.
- Phases in use, pilot current, which current limit applies, AC line power: Model3CAN.dbc 0x204 PCS_chgPHA/B/CEnable and PCS_gridConfig; 0x31C CC_numPhases; 0x21D CP_pilotCurrent, CP_cableCurrentLimit; 0x13D CP_internalMaxAcCurrentLimit; 0x264 ChargeLinePower264, ChargeLineCurrentLimit264. Bus: Vehicle bus (can0). Check: present, to check.
- Supercharger or other DC charger, and the energy it bills: Model3CAN.dbc 0x217 FC_brand, FC_class (page 0), FC_billingEnergy (page 1); 0x214 FC_type; 0x21D CP_teslaDcState. Bus: Vehicle bus (can0). Check: not seen yet.
- Car build: trim, drivetrain, pack size, wheels, paint, roof, spoiler, right-hand drive, map region, Supercharging access: Model3CAN.dbc 0x7FF carConfig GTW_* (paged). Bus: Vehicle bus (can0). Check: present, to check.
- Most charge current the pack accepts, DC station limit: Model3CAN.dbc 0x2D2 MaxChargeCurrent2D2; 0x27D CP_evseMaxDcCurrentLimit. Bus: Vehicle bus (can0). Check: not seen yet.

Not on the CAN bus, and what Sentry Mate does instead:

- The price paid at a Supercharger or public charger: A price per place (per kWh, per minute, time-of-use, session fee) or a per-session override; imported TeslaMate costs are kept.
- Grid-side energy of a DC charge: Show DC efficiency as unknown with the reason; show the energy delivered at the inlet (charger volts x amps) and the Supercharger's billed energy once confirmed.
- A clean battery-chemistry flag: Per-car setting: automatic from brick voltages and pack size (with the evidence), or set by the owner; prefilled from a TeslaMate import.


<a id="page-charge"></a>

### Charge detail

**Route:** `/cars/{car}/charges/{charge}`  
**Purpose:** One charging session in full, with its cost editor on the same page. Replaces TeslaMate's Charge Details and its separate Charge Cost page.

**Covers items from:** Charge Details (`dash.charge-details`, 31), Charge Cost (`web.charge-cost`, 17), Screenshots (`docs.screenshots`, 1).

**Data:** 25 have, 21 derive, 3 new signal.

**Filters:** Zoom inside the session.

**Drill-downs:** Previous / next session; Place -> Places editor; Drive before or after -> Drive detail.

**Sections**

- **Header.** Car and session in a breadcrumb (the car comes with the session; no car picker that could mix two cars), previous and next session, zoom inside the session.
  Covers: `cd.context.session`, `cd.filter.car`, `cd.filter.time_range`
- **Charge chart.** Battery % and usable %, power, range, battery temperature, battery heating and outside temperature on one time axis at 1 Hz.
  Covers: `cd.chart.timeline`, `cd.chart.soc`, `cd.chart.power`, `cd.chart.battery_heater`, `cd.chart.range`, `cd.chart.outdoor_temp`
- **Supply chart.** AC line volts and amps, phases in use, the station's pilot current and which limit applies (station, cable or car); for DC, the charger's own volts and amps.
  Covers: `cd.chart.ac_voltage`, `cd.chart.phases`, `cd.chart.ac_current`, `cd.chart.pilot_current`
- **Power curve.** Power against battery % for this session, over this car's typical DC curve.
  Covers: `cd.chart.curve_points`, `cd.chart.curve_avg`
- **Figures.** Charging time and plugged-in time, place and map, energy added and drawn, efficiency (AC), battery % and range at start, added and at end, average power, average outside and battery temperature, energy still to add when it was stopped.
  Covers: `cd.stat.duration`, `cd.stat.location`, `cd.stat.energy_added`, `cd.stat.energy_used`, `cd.stat.efficiency`, `cd.map.location`, `cd.stat.soc_start`, `cd.stat.soc_added`, `cd.stat.soc_end`, `cd.stat.avg_power`, `cd.stat.avg_temp`, `cd.stat.range_start`, `cd.stat.range_added`, `cd.stat.range_end`
- **Cost.** Cost with a breakdown per rate and session fee. Edit it here: a total, a price per kWh or per minute, currency, and grid or pack energy as the basis. The override survives rebuilds and can be removed to go back to the place price.
  Covers: `cd.stat.cost`, `cd.link.set_cost`, `web.charge-cost.breadcrumb`, `web.charge-cost.entry`, `web.charge-cost.map`, `web.charge-cost.dates`, `web.charge-cost.car`, `web.charge-cost.energy`, `web.charge-cost.duration`, `web.charge-cost.location`, `web.charge-cost.mode`, `web.charge-cost.amount`, `web.charge-cost.rate-precision`, `web.charge-cost.current-cost`, `web.charge-cost.save`, `web.charge-cost.clear-and-negative`, `web.charge-cost.error`, `web.charge-cost.not-found`, `web.charge-cost.back`
- **Whole page (docs screenshot).** The documentation gallery picture of this TeslaMate page.
  Covers: `shots.charge-details`

**Better than TeslaMate**

- 1 Hz curves instead of polling every few seconds.
- DC sessions show the charger's own volts and amps, not an empty AC chart.
- Phases, pilot current and the limit that applies are shown, not a single 'phases' number.
- The cost editor is part of the charge page, explains the price per rate and keeps a currency.

**Data gaps**

New CAN signals to decode:

- Charging status word, cable connected, charge port open: Model3CAN.dbc 0x212 BMS_uiChargeStatus (already decoded, kept only as true/false), 0x25D CP_chargeCableState, CP_chargeDoorOpen. Bus: Vehicle bus (can0). Check: present, to check.
- Charge limit, AC current set on the screen, scheduled-departure flags: Model3CAN.dbc 0x333 UI_chargeTerminationPct, UI_acChargeCurrentLimit, UI_scheduledDepartureEnabled, UI_smartAcChargingEnabled. Bus: Vehicle bus (can0). Check: present, to check.
- Energy still to add, time to full: override.dbc 0x352 page 1 BMS_energyToChargeComplete; Model3CAN.dbc 0x132 ChargeHoursRemaining132 (minutes despite the name). Bus: Vehicle bus (can0). Check: seen.
- Phases in use, pilot current, which current limit applies, AC line power: Model3CAN.dbc 0x204 PCS_chgPHA/B/CEnable and PCS_gridConfig; 0x31C CC_numPhases; 0x21D CP_pilotCurrent, CP_cableCurrentLimit; 0x13D CP_internalMaxAcCurrentLimit; 0x264 ChargeLinePower264, ChargeLineCurrentLimit264. Bus: Vehicle bus (can0). Check: present, to check.
- Supercharger or other DC charger, and the energy it bills: Model3CAN.dbc 0x217 FC_brand, FC_class (page 0), FC_billingEnergy (page 1); 0x214 FC_type; 0x21D CP_teslaDcState. Bus: Vehicle bus (can0). Check: not seen yet.
- Battery heating on, kept warm, heating power: Model3CAN.dbc 0x2E1 page 0 VCFRONT_isActiveHeatingBattery; 0x212 BMS_keepWarmRequest; 0x266 RearHeatPower266, 0x2E5 FrontHeatPower2E5, 0x268 SystemHeatPower268. Bus: Vehicle bus (can0). Check: present, to check.

Not on the CAN bus, and what Sentry Mate does instead:

- The price paid at a Supercharger or public charger: A price per place (per kWh, per minute, time-of-use, session fee) or a per-session override; imported TeslaMate costs are kept.
- Grid-side energy of a DC charge: Show DC efficiency as unknown with the reason; show the energy delivered at the inlet (charger volts x amps) and the Supercharger's billed energy once confirmed.


<a id="page-battery"></a>

### Battery

**Route:** `/cars/{car}/battery`  
**Purpose:** How healthy the battery is, how the charge level is used, and where energy goes while parked. Merges TeslaMate's Battery Health, Projected Range, Charge Level and Vampire Drain dashboards.

**Covers items from:** Battery Health (`dash.battery-health`, 34), Vampire Drain (`dash.vampire-drain`, 15), Charge Level (`dash.charge-level`, 14), Projected Range (`dash.projected-range`, 12), Screenshots (`docs.screenshots`, 4).

**Data:** 17 have, 56 derive, 6 new signal.

**Filters:** Period (for history sections); Minimum parked hours; Chart resolution; Charge level: bucket size, moving lines on or off, moving window.

**Drill-downs:** Parked spell -> Trip for exactly that span (battery and state charts), and Timeline at that time; Capacity point -> Charge detail; Not-logged distance -> Data health gap list; Totals -> Drives / Charges.

**Sections**

- **Now.** Battery %, usable %, energy stored in kWh, and the everyday band with the car's own charge limit as a tick.
  Covers: `bh.filter.car`, `bh.soc.current`, `bh.soc.band`, `bh.bar.stored_energy`
- **Capacity and health.** Full-pack and usable capacity from the battery management system, capacity when new with its source, difference, health % and a warranty line, and capacity against distance or time with a trend and projection. Brick voltage spread as a balance indicator.
  Covers: `bh.capacity.usable_new`, `bh.capacity.usable_now`, `bh.capacity.difference`, `bh.capacity.help`, `bh.gauge.degradation`, `bh.gauge.degradation_help`, `bh.bar.health`, `bh.stat.efficiency`, `bh.chart.capacity_by_distance.points`, `bh.chart.capacity_by_distance.median`
- **Full-charge range.** Range at 100 % now and when new, and range lost, each with its source.
  Covers: `bh.ranges.max_new`, `bh.ranges.max_now`, `bh.ranges.lost`
- **Lifetime totals.** Logged distance, odometer, distance not logged (with a link to each gap), charging sessions, energy added and drawn, AC/DC/regen split from the pack's own lifetime counters, and equivalent full cycles.
  Covers: `bh.drive.logged`, `bh.drive.mileage`, `bh.drive.odometer`, `bh.drive.data_lost`, `bh.drive.help`, `bh.drive.link_stats`, `bh.chart.ac_dc`, `bh.charging.count`, `bh.charging.cycles`, `bh.charging.energy_added`, `bh.charging.energy_used`, `bh.charging.efficiency`, `bh.charging.help`, `bh.charging.link_stats`
- **Reference values (per car).** Capacity when new, full range when new and a warranty threshold, typed once per car and checked on input.
  Covers: `bh.setting.capacity_new_override`, `bh.setting.range_new_override`, `bh.setting.override_help`
- **Projected range.** Range at a full charge over time, from energy and from battery %, next to odometer, battery temperature and outside temperature.
  Covers: `pr.section.per_car`, `pr.filter.time_range`, `pr.control.resolution`, `pr.control.shared_crosshair`, `pr.mileage.projected`, `pr.mileage.odometer`, `pr.battery.projected_usable`, `pr.battery.projected_total`, `pr.battery.levels`, `pr.temp.projected`, `pr.temp.outdoor`, `pr.axis.titles`
- **Charge level history.** Battery % and usable % over the period with asleep and unknown spans drawn apart, moving average, median and percentile lines, the recommended band, and the share of time above 80 % and below 20 %. Controls: bucket size (hour or day), moving lines on or off, and a fixed moving window (7 or 30 days) that does not change when the period does.
  Covers: `cl.filter.car`, `cl.filter.time_range`, `cl.control.bucket_width`, `cl.control.include_moving`, `cl.control.window_width`, `cl.chart.charge_level`, `cl.chart.battery_level`, `cl.chart.usable_level`, `cl.chart.band`, `cl.chart.moving_p075`, `cl.chart.moving_avg`, `cl.chart.moving_median`, `cl.chart.moving_p925`, `cl.chart.legend_stats`
- **Parked drain.** Each parked spell: start and end, length, time asleep, awake and unknown, energy lost in kWh, average power, battery % and range lost, range lost per hour, the reasons it stayed awake, and a cold-battery mark.
  Covers: `vd.section.per_car`, `vd.filter.time_range`, `vd.filter.min_idle`, `vd.table`, `vd.col.start`, `vd.col.end`, `vd.col.period`, `vd.col.standby`, `vd.col.soc_diff`, `vd.flag.cold`, `vd.col.range_loss`, `vd.col.energy`, `vd.col.avg_power`, `vd.col.range_per_hour`, `vd.link.start_details`
- **Whole page (docs screenshot).** The documentation gallery picture of this TeslaMate page.
  Covers: `shots.battery-health`, `shots.charge-level`, `shots.projected-range`, `shots.vampire-drain`

**Better than TeslaMate**

- Capacity comes straight from the battery management system every awake minute, not from range times a voted factor at a few charge ends. There is no 1 kWh placeholder.
- 'When new' is the owner's value, or the median of the earliest readings, always with its source; one odd sample cannot inflate it.
- Lifetime AC, DC and regen energy cover the car's whole life, including years before logging, and add up exactly.
- Parked drain is measured in kWh, not whole percent steps, and unknown time is never counted as sleep.
- Reference values are stored per car and checked, not free-text boxes shared by all cars.
- Capacity can be seen against time or distance, with a trend line and a projection.

**Data gaps**

New CAN signals to decode:

- Sentry mode on: Model3CAN.dbc 0x284 UIvehicleModes: UIsentryMode284. Bus: Vehicle bus (can0). Check: seen.
- Climate on, preconditioning, overheat protection, fan, set points: override.dbc 0x243 page 0 VCRIGHT_hvacPowerState, hvacBlowerSegment, hvacACRunning; Model3CAN.dbc 0x2F3 UI_hvacReqUserPowerState, UI_hvacReqTempSetpointLeft/Right (raw 0 = LO, 26 = HI). Bus: Vehicle bus (can0). Check: seen.
- Charge limit, AC current set on the screen, scheduled-departure flags: Model3CAN.dbc 0x333 UI_chargeTerminationPct, UI_acChargeCurrentLimit, UI_scheduledDepartureEnabled, UI_smartAcChargingEnabled. Bus: Vehicle bus (can0). Check: present, to check.
- Full-pack energy, buffer, expected (cold-limited) energy: override.dbc 0x352 page 0 BMS_nominalFullPackEnergy, page 1 BMS_energyBuffer, BMS_expectedEnergyRemaining. Bus: Vehicle bus (can0). Check: seen.
- Lifetime AC, DC, regen and drive energy: Model3CAN.dbc 0x3F2 pages 0-3 BMS_acChargerKwhTotal, BMS_dcChargerKwhTotal, BMS_kwhRegenChargeTotal, BMS_kwhDriveDischargeTotal. Bus: Vehicle bus (can0). Check: seen.
- Usable battery % (cold-locked part removed): Model3CAN.dbc 0x33A UI_uSOE; fallback from 0x352 energy and buffer. Bus: Vehicle bus (can0). Check: present, to check.
- Brick voltage spread (balance, LFP hint): override.dbc 0x332 page 1 BattBrickVoltageMax332 / Min332. Bus: Vehicle bus (can0). Check: seen.

Not on the CAN bus, and what Sentry Mate does instead:

- Usable capacity when new, for a car logged late: Owner value per car; otherwise the median of the earliest readings or imported charges, with the pack-size code as a labelled hint. Always say which.
- Full-charge range when new, for a car logged late: Owner value per car; otherwise the median full-charge range of the first 30 days.
- A clean battery-chemistry flag: Per-car setting: automatic from brick voltages and pack size (with the evidence), or set by the owner; prefilled from a TeslaMate import.


<a id="page-efficiency"></a>

### Efficiency and temperatures

**Route:** `/cars/{car}/efficiency`  
**Purpose:** How much energy driving takes, how temperature changes it, and the temperatures themselves. Merges TeslaMate's Efficiency and Temperatures dashboards.

**Covers items from:** Efficiency (`dash.efficiency`, 20), Temperatures (`dash.temperatures`, 20), Screenshots (`docs.screenshots`, 2), Frequently Asked Questions (`docs.faq`, 1).

**Data:** 6 have, 36 derive, 1 new signal.

**Filters:** Period; Minimum distance; Temperature group width; Cars.

**Drill-downs:** Temperature group -> its drives, listed on this page under the table (each drive opens Drive detail; the Drives page has no temperature filter, so its link shows every drive of the period); Charging temperature dot -> Charge detail.

**Sections**

- **Filters.** Shared period, minimum drive distance, temperature group width, one car or compare cars.
  Covers: `ef.section.per_car`, `ef.filter.time_range`, `ef.filter.min_distance`, `tp.filter.car`, `tp.filter.time_range`, `tp.control.shared_crosshair`
- **Consumption.** Net consumption (driving only), gross consumption (everything, parked included) and logged distance, each with a one-line method note.
  Covers: `ef.stat.consumption_net`, `ef.stat.consumption_gross`, `ef.stat.logged_distance`, `ef.help.headline_stats`, `faq.no-consumption`
- **By temperature.** Consumption, range efficiency, distance and average speed per outside-temperature group, plus drives with no temperature.
  Covers: `ef.table.temperature`, `ef.temp.col.temperature`, `ef.temp.col.efficiency`, `ef.temp.col.consumption`, `ef.temp.col.distance`, `ef.temp.col.speed`
- **Energy factor.** The car's rated and ideal Wh per km, each from energy and range read in the same second, with how many readings back it.
  Covers: `ef.stat.current_factor`, `ef.table.derived_ideal`, `ef.ideal.col.efficiency`, `ef.ideal.col.count`, `ef.table.derived_rated`, `ef.rated.col.efficiency`, `ef.rated.col.count`
- **Temperatures.** Outside, cabin and battery temperature: lowest, average, highest and how much of the period has data; charts over time; set points while climate runs; outside temperature during charges; daily range and typical band.
  Covers: `tp.sections`, `tp.outside.min`, `tp.outside.avg`, `tp.outside.max`, `tp.outside.coverage`, `tp.inside.min`, `tp.inside.avg`, `tp.inside.max`, `tp.inside.coverage`, `tp.summary.help`, `tp.chart.inside`, `tp.chart.outside`, `tp.chart.setpoint`, `tp.chart.charging_outside`, `tp.chart.daily_envelope`, `tp.chart.daily_coverage`, `tp.chart.daily_envelope_help`
- **Whole page (docs screenshot).** The documentation gallery picture of this TeslaMate page.
  Covers: `shots.efficiency`, `shots.temperatures`

**Better than TeslaMate**

- Consumption is measured, so no energy factor or rated/ideal choice is needed to get it; the factor is shown only as a cross-check.
- Gross consumption uses the pack's lifetime counters and is exact for any period.
- Coverage is shown next to every temperature figure, so a short period with little data is obvious.
- Battery temperature sits next to outside and cabin temperature.

**Data gaps**

New CAN signals to decode:

- Climate on, preconditioning, overheat protection, fan, set points: override.dbc 0x243 page 0 VCRIGHT_hvacPowerState, hvacBlowerSegment, hvacACRunning; Model3CAN.dbc 0x2F3 UI_hvacReqUserPowerState, UI_hvacReqTempSetpointLeft/Right (raw 0 = LO, 26 = HI). Bus: Vehicle bus (can0). Check: seen.
- Lifetime AC, DC, regen and drive energy: Model3CAN.dbc 0x3F2 pages 0-3 BMS_acChargerKwhTotal, BMS_dcChargerKwhTotal, BMS_kwhRegenChargeTotal, BMS_kwhDriveDischargeTotal. Bus: Vehicle bus (can0). Check: seen.
- The car's rated Wh per mile: Model3CAN.dbc 0x33A UI_ratedWHpM. Bus: Vehicle bus (can0). Check: present, to check.


<a id="page-statistics"></a>

### Statistics

**Route:** `/cars/{car}/statistics  (also /statistics?cars=all)`  
**Purpose:** Totals per day, week, month or year, and the odometer over time. Merges TeslaMate's Statistics and Mileage dashboards.

**Covers items from:** Statistics (`dash.statistics`, 25), Mileage (`dash.mileage`, 3), Screenshots (`docs.screenshots`, 2), Projects using TeslaMate (`docs.projects`, 1).

**Data:** 5 have, 26 derive.

**Filters:** Grouping; Period; Cars.

**Drill-downs:** Period -> Trip; Counts -> Drives / Charges filtered to the period.

**Sections**

- **Filters.** Group by day, week (ISO), month or year in each car's time zone, or the whole period as one row; shared period; one car, several or all. There is no precision switch: one line says gross consumption is exact either way.
  Covers: `statistics.filter.car`, `statistics.filter.period`, `statistics.filter.high_precision`, `statistics.control.time_range`
- **Period table.** Per period: time driven, drives, distance (odometer and logged), average outside temperature, driving efficiency, energy added and drawn, charges, energy per charge, cost per currency, price per kWh, cost per 100 km, net and gross consumption, overhead, and how complete the data is (recorded share, unknown time, dropped stretches, open drive or charge, imported rows).
  Covers: `statistics.table`, `statistics.col.period`, `statistics.col.time_driven`, `statistics.col.distance`, `statistics.col.avg_temp`, `statistics.col.drives_count`, `statistics.col.driving_efficiency`, `statistics.col.energy_used`, `statistics.col.avg_energy_per_charge`, `statistics.col.costs`, `statistics.col.charges_count`, `statistics.col.cost_per_kwh`, `statistics.col.cost_per_distance`, `statistics.col.consumption_net`, `statistics.col.consumption_gross`, `statistics.col.consumption_overhead`, `statistics.col.data_complete`
- **Period links.** Period -> Trip; drive count -> Drives; charge count -> Charges; energy -> Charges habits.
  Covers: `statistics.link.trip`, `statistics.link.drives`, `statistics.link.charges`, `statistics.link.charging_stats`
- **Odometer and mileage.** Odometer over time and distance per period, with logged and not-logged distance and a yearly projection.
  Covers: `mi.section.per_car`, `mi.filter.time_range`, `mi.chart.odometer`
- **Milestones.** Optional: firsts and records (longest drive, most efficient month, first 10,000 km), computed from stored data.
  Covers: `projects.achievements`
- **Whole page (docs screenshot).** The documentation gallery picture of this TeslaMate page.
  Covers: `shots.mileage`, `shots.statistics`

**Better than TeslaMate**

- Completeness per period is exact (recorded share, unknown time, dropped stretches), not a flag that stays raised.
- No 'high precision' switch: gross consumption is exact for every period.
- Periods follow each car's own time zone and ISO weeks; costs are summed per currency.
- All cars in one table with a car column, instead of the whole page repeated per car.


<a id="page-travel"></a>

### Travel

**Route:** `/travel  (one car or many)`  
**Purpose:** Where the cars have been: stops, places, cities, regions and countries, with time spent there. Merges TeslaMate's Locations and Visited dashboards.

**Covers items from:** Visited (`dash.visited`, 46), Locations (`dash.locations`, 27), Screenshots (`docs.screenshots`, 2).

**Data:** 12 have, 63 derive.

**Filters:** Cars; Period; Text; Place; Country or region; City.

**Drill-downs:** Day -> Drives and Charges for that day; Stop -> Places editor; City marker -> Days in a city.

**Sections**

- **Filters.** Cars (one, several, all), shared period, text search, place, country or region, and the chosen city for the day list.
  Covers: `locations.filter.car`, `locations.filter.time_range`, `locations.filter.address`, `locations.control.refresh`, `visited.filter.time_range`, `visited.control.refresh`, `visited.filter.car`, `visited.section.car_header`, `visited.state.selected_city`
- **Summary.** Places, cities, regions, countries, days out, first and last visit, unnamed stops, distance, energy added and drawn, charging efficiency and cost per currency.
  Covers: `locations.stat.addresses`, `locations.stat.cities`, `locations.stat.states`, `locations.stat.countries`, `visited.stat.distance`, `visited.stat.charging_cost`, `visited.stat.energy_added`, `visited.stat.energy_drawn`, `visited.stat.charging_efficiency`, `visited.stat.countries`, `visited.stat.cities`, `visited.text.shared_visits_help`, `visited.stat.first_visit`, `visited.stat.last_visit`, `visited.stat.days`
- **Map.** Offline map with the route at 1 Hz (one line per drive) and one marker per city or place, with details on hover.
  Covers: `visited.map`, `visited.map.base_map`, `visited.text.map_help`, `visited.map.route`, `visited.map.city_markers`, `visited.map.marker_details`
- **Countries.** Per country: cities, days, time spent, first and last visit.
  Covers: `visited.table.countries`, `visited.countries.country`, `visited.countries.cities`, `visited.countries.days`, `visited.countries.first_visit`, `visited.countries.last_visit`
- **Cities and regions.** Per city and region: country, visits, places, days, time spent, first and last visit, with counts printed on the ranking bars; narrow by country or region; pick a city to list its days.
  Covers: `locations.chart.top_cities`, `locations.chart.top_regions`, `locations.chart.rank_shading`, `visited.link.city_days`, `visited.table.cities`, `visited.cities.city`, `visited.cities.region`, `visited.cities.country`, `visited.cities.days`, `visited.cities.first_visit`, `visited.cities.last_visit`, `visited.cities.column_filter`
- **Days in a city.** Per day in the chosen city: first and last time seen, drives, charges and places, in the car's time zone.
  Covers: `visited.table.city_days`, `visited.text.city_days_help`, `visited.city_days.date`, `visited.city_days.first_seen`, `visited.city_days.last_seen`, `visited.city_days.drives`, `visited.city_days.charges`, `visited.city_days.places`, `visited.link.day_drives`
- **Places visited.** Stops grouped by saved place or position, with address parts, visits and time spent; create or edit a saved place from a row.
  Covers: `locations.table.addresses`, `locations.addresses.name`, `locations.addresses.neighbourhood`, `locations.addresses.city`, `locations.addresses.state`, `locations.addresses.country`, `locations.addresses.updated_at`, `locations.link.new_geofence`, `locations.table.geofences`, `locations.geofences.name`, `locations.geofences.time`, `locations.link.edit_geofence`
- **Recent stops.** Most recent stops with arrival and departure time.
  Covers: `locations.table.last_visited`, `locations.last_visited.date`, `locations.last_visited.address`, `locations.last_visited.city`
- **Whole page (docs screenshot).** The documentation gallery picture of this TeslaMate page.
  Covers: `shots.locations`, `shots.visited`

**Better than TeslaMate**

- Visits are real stops with arrival and departure times, so time spent per place, city and country is known.
- One definition of a visit and one city key, so Locations and Visited can no longer disagree.
- Stops are grouped by position (about 150 m) and saved places, so one spot never counts twice.
- Maps and addresses work offline by default; nothing is sent to an outside map service unless the owner turns it on.
- Days are counted in the car's own time zone.

**Data gaps**

New CAN signals to decode:

- Heading: Model3CAN.dbc 0x3D9 UI_gpsVehicleHeading (never decode can3's 0x3D9). Bus: Chassis bus (can1). Check: not seen yet.
- Navigating, energy at destination, in a Supercharger geofence: Model3CAN.dbc 0x082 UI_tripPlanningActive, UI_energyAtDestination; 0x238 UI_navRouteActive, UI_inSuperchargerGeofence. Bus: Vehicle bus (0x082), chassis bus (0x238). Check: not seen yet.


<a id="page-software"></a>

### Software

**Route:** `/cars/{car}/software  (also fleet versions)`  
**Purpose:** The car's software history and the version it runs now, honest about what is known. Replaces TeslaMate's Updates dashboard.

**Covers items from:** Updates (`dash.updates`, 14), Screenshots (`docs.screenshots`, 1), Projects using TeslaMate (`docs.projects`, 1).

**Data:** 2 have, 5 derive, 6 new signal, 3 not on CAN.

**Filters:** Period; Cars.

**Drill-downs:** Update -> Timeline at that time; Charges count -> Charges for that span.

**Sections**

- **Filters.** Shared period; one car or the fleet version list.
  Covers: `updates.filter.time_range`, `updates.filter.car`, `updates.control.refresh`, `updates.section.car_header`
- **Summary.** Updates in the period, median time between updates, last known version with its date and source, and 'software changed since'.
  Covers: `projects.telegram-bot`, `updates.stat.count`, `updates.stat.median_gap`
- **Update history.** Per update: start, end and duration (from the car's update status when recorded), time since the previous one, version text (imported or typed by the owner) or 'build changed' with the modules that changed, charges while on it, capacity and range at 100 % while on it. Release notes link only where a version is known and the owner allows outside links.
  Covers: `updates.table`, `updates.table.date`, `updates.table.duration`, `updates.table.since_previous`, `updates.table.version`, `updates.link.release_notes`, `updates.table.charge_count`, `updates.table.avg_range`
- **Whole page (docs screenshot).** The documentation gallery picture of this TeslaMate page.
  Covers: `shots.updates`

**Better than TeslaMate**

- Updates are detected from the car itself (update status and module build ids), even with no outside service.
- Never shows a guessed version; says where each version came from.
- Battery capacity per version comes from the battery management system, not range at 100 % per hour bucket.
- Charges are counted per version by exact time.

**Data gaps**

New CAN signals to decode:

- Full-pack energy, buffer, expected (cold-limited) energy: override.dbc 0x352 page 0 BMS_nominalFullPackEnergy, page 1 BMS_energyBuffer, BMS_expectedEnergyRemaining. Bus: Vehicle bus (can0). Check: seen.
- Software changed (module build ids): Model3CAN.dbc page 17 of each info message: 0x300 BMS_appGitHash, 0x301 VCFRONT_infoAppGitHash, 0x51E FC_infoAppGitHashBytes, 0x335 DIR_appGitHash, 0x656 DIF_appGitHash. Bus: Vehicle bus (can0). Check: present, to check.
- Update being installed: Model3CAN.dbc 0x2E1 page 1 VCFRONT_vehicleStatusDBG = VEHICLE_STATUS_OTA, VCFRONT_batterySMState = OTA_STANDBY; model3y-opendbc.dbc 0x343 VCRIGHT_OTAState. Bus: Vehicle bus (can0). Check: present, to check.

Not on the CAN bus, and what Sentry Mate does instead:

- The software version text and the update history: Show imported TeslaMate updates with their version text; let the owner type the current version; mark 'software changed' when module build ids change, and remember each set of build ids with its version so other cars on the same build get it. Never show a guessed version.
- Release-notes link: Only where a version is known, only when the owner allows outside links, and opened by the viewer's click (the server sends nothing).
- Update available, download and install progress: Leave out. Show 'installing' from the car's update status (once confirmed) and 'software changed' after the fact.


<a id="page-places"></a>

### Places

**Route:** `/places`  
**Purpose:** Name the places that matter, give them charging prices, and see what a change will do before saving. Replaces TeslaMate's Geo-Fences pages.

**Covers items from:** Geo-Fence create / edit (`web.geofences.form`, 17), Geo-Fences (`web.geofences.list`, 9), Frequently Asked Questions (`docs.faq`, 1), Projects using TeslaMate (`docs.projects`, 1).

**Data:** 14 have, 14 derive.

**Filters:** Search; Kind.

**Drill-downs:** Place -> Drives / Charges at that place; Suggestion -> new place prefilled.

**Sections**

- **Place list.** Name, kind (home, work, charger, service centre, other), position, radius, drives and charges using it; sorted by name; create, edit and delete (a delete shows first what loses the name); suggestions from where the cars park most.
  Covers: `faq.geofence-purpose`, `web.geofences.list.breadcrumb`, `web.geofences.list.empty-state`, `web.geofences.list.create`, `web.geofences.list.col-name`, `web.geofences.list.col-position`, `web.geofences.list.col-radius`, `web.geofences.list.order`, `web.geofences.list.edit`, `web.geofences.list.delete`
- **Place editor.** Map with a circle to move and resize, name, kind, and the match rule (the nearest centre wins when places overlap). New places start from a recorded position.
  Covers: `web.geofences.form.breadcrumb`, `web.geofences.form.entry-from-dashboards`, `web.geofences.form.defaults`, `web.geofences.form.position-map`, `web.geofences.form.match-rule`, `web.geofences.form.name`, `web.geofences.form.validation`, `web.geofences.form.not-found`, `web.geofences.form.back`, `web.geofences.form.save`
- **Prices.** Currency, price per kWh or per minute, time-of-use rates by day and hour in the place's time zone, and a session fee.
  Covers: `projects.agile`, `web.geofences.form.billing-type`, `web.geofences.form.cost-per-unit`, `web.geofences.form.session-fee`, `web.geofences.form.negative-price`, `web.geofences.form.auto-cost`
- **Effect on history.** Before saving: which drives and charges gain or lose the name and which charges would be repriced, with counts and examples. Reprice past charges on request, for a chosen span.
  Covers: `web.geofences.form.relabel-history`, `web.geofences.form.retro-costs`

**Better than TeslaMate**

- Time-of-use prices with a currency and daylight saving handled, instead of one price.
- A preview shows what saving will relabel or reprice before anything changes.
- When places overlap, the one with the nearest centre wins, and the editor shows which; TeslaMate does not say which place wins.
- Suggestions from where the cars actually park.

**Data gaps**

New CAN signals to decode:

- Supercharger or other DC charger, and the energy it bills: Model3CAN.dbc 0x217 FC_brand, FC_class (page 0), FC_billingEnergy (page 1); 0x214 FC_type; 0x21D CP_teslaDcState. Bus: Vehicle bus (can0). Check: not seen yet.


<a id="page-settings-cars"></a>

### Settings: Cars

**Route:** `/settings/cars/{car}`  
**Purpose:** Per-car settings, and how each car's CAN Bridge is tied to it.

**Covers items from:** Settings: per-car section (`web.settings.car`, 6), Environment variables: secrets, database, time zone, import, geofence default (`docs.env.core`, 1), Frequently Asked Questions (`docs.faq`, 1).

**Data:** 2 have, 4 derive, 2 new signal.

**Filters:** Car.

**Drill-downs:** Waiting key -> assign to this car; Upload status -> Data health.

**Sections**

- **Car and switcher.** Car picker, name, and an empty state that explains how a car appears. Every change is saved at once, with a short confirmation.
  Covers: `web.settings.car.breadcrumb`, `web.settings.car.car-switcher`, `web.settings.car.empty-state`, `web.settings.car.autosave`
- **Upload tokens and keys.** Upload tokens (made here, shown once), CAN Bridge keys tied to the car, a preferred key when two record at once, and keys waiting to be assigned, each with first and last upload.
  Covers: `faq.car-missing.outcomes`
- **Time zone.** The car's time zone, used for days, weeks and tariffs.
  Covers: `docs.env.core.tz`
- **Battery.** Chemistry: automatic (with the evidence), LFP, or NCA/NMC.
  Covers: `web.settings.car.lfp-battery`
- **Charging.** Free Supercharging as a dated range, with the car's own entitlement shown as a hint.
  Covers: `web.settings.car.free-supercharging`

**Better than TeslaMate**

- No Tesla account or tokens: a car appears when its CAN Bridge uploads, and the owner names it.
- Several keys per car (Screenmate, reinstall, Raspberry Pi), with a preferred one.
- Battery chemistry can be detected from brick voltages, with the evidence shown.
- Free Supercharging is a dated range, not a switch that changes all history.

**Data gaps**

New CAN signals to decode:

- Full-pack energy, buffer, expected (cold-limited) energy: override.dbc 0x352 page 0 BMS_nominalFullPackEnergy, page 1 BMS_energyBuffer, BMS_expectedEnergyRemaining. Bus: Vehicle bus (can0). Check: seen.
- Supercharger or other DC charger, and the energy it bills: Model3CAN.dbc 0x217 FC_brand, FC_class (page 0), FC_billingEnergy (page 1); 0x214 FC_type; 0x21D CP_teslaDcState. Bus: Vehicle bus (can0). Check: not seen yet.
- Car build: trim, drivetrain, pack size, wheels, paint, roof, spoiler, right-hand drive, map region, Supercharging access: Model3CAN.dbc 0x7FF carConfig GTW_* (paged). Bus: Vehicle bus (can0). Check: present, to check.
- Brick voltage spread (balance, LFP hint): override.dbc 0x332 page 1 BattBrickVoltageMax332 / Min332. Bus: Vehicle bus (can0). Check: seen.

Not on the CAN bus, and what Sentry Mate does instead:

- When free Supercharging started and ended: A dated range per car typed by the owner, with the car's entitlement shown as a hint.
- Cars on the Tesla account and their Tesla names: A car appears when its CAN Bridge uploads; the owner names it; model and year come from the VIN prefix.
- A clean battery-chemistry flag: Per-car setting: automatic from brick voltages and pack size (with the evidence), or set by the owner; prefilled from a TeslaMate import.


<a id="page-settings-display"></a>

### Settings: Display and units

**Route:** `/settings/display`  
**Purpose:** How numbers, words and maps look, per person, with server defaults set by an admin.

**Covers items from:** Settings: General Settings (`web.settings.general`, 12), App frame (`web.shell`, 4), Environment variables: Tesla API, streaming, polling and outbound connections (`docs.env.tesla-api`, 1), Screenshots (`docs.screenshots`, 1).

**Data:** 3 have, 15 derive.

**Drill-downs:** Address status -> Data health.

**Sections**

- **Units and range.** Distance, temperature and pressure units, and rated or ideal range. Defaults can come from the car's own distance-unit setting.
  Covers: `web.settings.general.preferred-range`, `web.settings.general.unit-length`, `web.settings.general.unit-temperature`, `web.settings.general.unit-pressure`
- **Look and language.** Light, dark or device theme, interface language, interface size, and UI pack.
  Covers: `shell.theme`, `shell.locale`, `shots.theme-note`, `web.settings.general.ui-language`, `web.settings.general.theme`, `web.shell.locale-negotiation`
- **Addresses and maps.** Address lookup: offline (bundled place names) or an online service with rounding and a cache; address language; map tiles offline or an online source with its credit.
  Covers: `docs.env.tesla-api.nominatim-proxy`, `web.settings.general.address-language`, `web.settings.general.address-language-error`, `web.settings.general.address-language-invalid`, `web.settings.general.address-lookup`, `web.shell.map-tiles`
- **Saving.** Changes save at once; per-person choices override the server defaults.
  Covers: `web.settings.general.divider`, `web.settings.general.autosave`

**Better than TeslaMate**

- Units are set once per person and applied everywhere; the API always sends metric values.
- Addresses and maps work offline by default.
- Language and theme are per person, not per browser.

**Data gaps**

New CAN signals to decode:

- The car's distance unit: Model3CAN.dbc 0x293 UI_distanceUnits. Bus: Vehicle bus (can0). Check: seen.


<a id="page-settings-integrations"></a>

### Settings: Integrations

**Route:** `/settings/integrations`  
**Purpose:** Send the cars' live values to other systems, if the owner wants to. Covers what TeslaMate documents for MQTT, Home Assistant and Node-RED, and the read API.

**Covers items from:** MQTT integration (`docs.mqtt`, 68), Home Assistant integration (`docs.home-assistant`, 38), Environment variables: MQTT broker (`docs.env.mqtt`, 9), Projects using TeslaMate (`docs.projects`, 9), Node-RED integration (`docs.node-red`, 6), Frequently Asked Questions (`docs.faq`, 2), Environment variables: secrets, database, time zone, import, geofence default (`docs.env.core`, 1).

**Data:** 60 have, 18 derive, 37 new signal, 18 not on CAN.

**Filters:** Car for the preview.

**Drill-downs:** Status -> broker log; Preview -> Car overview.

**Sections**

- **MQTT broker.** Host, port, user, password (write-only), TLS with certificate pinning, namespace, on or off; a test button and live status (connected, delivered, waiting, dropped, last error).
  Covers: `nr.setup.mqtt`, `docs.env.mqtt.disable-mqtt`, `docs.env.mqtt.mqtt-host`, `docs.env.mqtt.mqtt-port`, `docs.env.mqtt.mqtt-username`, `docs.env.mqtt.mqtt-password`, `docs.env.mqtt.mqtt-tls`, `docs.env.mqtt.mqtt-tls-accept-invalid-certs`, `docs.env.mqtt.mqtt-ipv6`, `docs.env.mqtt.mqtt-namespace`
- **MQTT topics and rules.** Topic layout, the text used when the car is at no saved place, how missing values are published, and a timestamp with every value set.
  Covers: `mqtt.topic-scheme`, `mqtt.note-car-id`, `mqtt.note-empty-values`, `docs.env.core.default-geofence`
- **MQTT published values.** Every published value with its unit and source, previewed for a chosen car. Location only when the owner opts in. The VIN is never published.
  Covers: `mqtt.display-name`, `mqtt.state`, `mqtt.since`, `mqtt.healthy`, `mqtt.version`, `mqtt.update-available`, `mqtt.update-version`, `mqtt.download-perc`, `mqtt.install-perc`, `mqtt.vin`, `mqtt.model`, `mqtt.trim-badging`, `mqtt.exterior-color`, `mqtt.wheel-type`, `mqtt.spoiler-type`, `mqtt.geofence`, `mqtt.latitude-longitude-deprecated`, `mqtt.location`, `mqtt.shift-state`, `mqtt.power`, `mqtt.speed`, `mqtt.heading`, `mqtt.elevation`, `mqtt.locked`, `mqtt.sentry-mode`, `mqtt.windows-open`, `mqtt.window-each`, `mqtt.doors-open`, `mqtt.door-each`, `mqtt.sun-roof-state`, `mqtt.sun-roof-installed`, `mqtt.sun-roof-percent-open`, `mqtt.trunk-open`, `mqtt.frunk-open`, `mqtt.is-user-present`, `mqtt.is-climate-on`, `mqtt.inside-temp`, `mqtt.outside-temp`, `mqtt.is-preconditioning`, `mqtt.odometer`, `mqtt.est-battery-range-km`, `mqtt.rated-battery-range-km`, `mqtt.ideal-battery-range-km`, `mqtt.battery-level`, `mqtt.usable-battery-level`, `mqtt.plugged-in`, `mqtt.charging-state`, `mqtt.charge-energy-added`, `mqtt.charge-limit-soc`, `mqtt.charge-port-door-open`, `mqtt.charger-actual-current`, `mqtt.charger-phases`, `mqtt.charger-power`, `mqtt.charger-voltage`, `mqtt.charge-current-request`, `mqtt.charge-current-request-max`, `mqtt.scheduled-charging-start-time`, `mqtt.time-to-full-charge`, `mqtt.tpms-pressure`, `mqtt.tpms-soft-warning`, `mqtt.active-route`, `mqtt.active-route-payload-routing`, `mqtt.active-route-payload-idle`, `mqtt.center-display-state`, `mqtt.service-mode`
- **Home Assistant discovery.** Discovery on or off, prefix, one device per car, and clean-up the moment a car is deleted or renamed.
  Covers: `ha.discovery.enable`, `ha.discovery.url`, `ha.discovery.prefix`, `ha.discovery.device-payload`, `ha.device.identity`, `ha.entity.id-scheme`, `ha.discovery.removed-car-cleanup`, `ha.discovery.disabled-cleanup`, `ha.discovery.cleanup-policy`, `ha.discovery.limits-note`, `ha.discovery.coverage-gap`
- **Home Assistant entities.** The entities Home Assistant gets, one per published value, read-only.
  Covers: `ha.entity.location-tracker`, `ha.entity.active-route-tracker`, `ha.entity.status-sensors`, `ha.entity.config-sensors`, `ha.entity.motion-sensors`, `ha.entity.parking-brake`, `ha.entity.temperature-sensors`, `ha.entity.range-sensors`, `ha.entity.charging-sensors`, `ha.entity.charge-energy-added.counter`, `ha.entity.tpms-sensors`, `ha.entity.active-route-destination`, `ha.entity.active-route-energy`, `ha.entity.active-route-distance`, `ha.entity.active-route-minutes`, `ha.entity.active-route-traffic`, `ha.entity.binary-status`, `ha.entity.binary-security`, `ha.entity.binary-climate-charge`, `ha.proximity`, `ha.lovelace.glance`, `ha.lovelace.map`, `ha.lovelace.entity-list`
- **Events for automations.** Explicit drive started and ended, charge started and ended, software changed, someone in the car, left-open reminders: no spurious events after a restart.
  Covers: `ha.automation.garage`, `ha.automation.left-open`, `ha.automation.left-open-cancel`, `ha.automation.plug-in-reminder`, `nr.overview`, `nr.notify.state-change`, `nr.notify.driver-present`, `nr.notify.time-to-full`, `nr.notify.geofence`
- **API access.** The read API for other apps, with per-account car access.
  Covers: `faq.public-api`, `projects.teslamateapi`, `projects.custom-dashboards`, `projects.matedroid`
- **Network use.** Every outbound connection in use (address lookup, tiles, MQTT broker) and the inbound upload port.
  Covers: `faq.network-flows`
- **Works with.** Notes on using the published values with route planners, garage doors, desk gauges and smart mirrors.
  Covers: `projects.disclaimer`, `projects.abrp`, `projects.ha-tesla`, `projects.gaussmeter`, `projects.magic-mirror`, `projects.geogdo`

**Better than TeslaMate**

- A preview of every message and Home Assistant entity before turning anything on.
- Values carry units and an 'updated' time; power is stated in kW.
- The VIN is never published, and deleting or renaming a car updates Home Assistant at once.
- Events are explicit, so automations do not fire on a restart.
- Broker status and last error are visible in the app.

**Data gaps**

New CAN signals to decode:

- Sentry mode on: Model3CAN.dbc 0x284 UIvehicleModes: UIsentryMode284. Bus: Vehicle bus (can0). Check: seen.
- Service mode: Model3CAN.dbc 0x284 UIvehicleModes: UIserviceMode284. Bus: Vehicle bus (can0). Check: present, to check.
- Each door, trunk, frunk and liftgate: Model3CAN.dbc 0x102 / 0x103 latch status, 0x2E1 page 0 VCFRONT_frunkLatchStatus, 0x142 page 0 VCLEFT_liftgateState (page 1 liftgate angle). Bus: Vehicle bus (can0). Check: seen.
- Climate on, preconditioning, overheat protection, fan, set points: override.dbc 0x243 page 0 VCRIGHT_hvacPowerState, hvacBlowerSegment, hvacACRunning; Model3CAN.dbc 0x2F3 UI_hvacReqUserPowerState, UI_hvacReqTempSetpointLeft/Right (raw 0 = LO, 26 = HI). Bus: Vehicle bus (can0). Check: seen.
- Keep-climate mode (keep, dog, camp): Model3CAN.dbc 0x2F3 UI_hvacReqKeepClimateOn. Bus: Vehicle bus (can0). Check: present, to check.
- Battery preconditioning (apart from the cabin): Model3CAN.dbc 0x3B3 UI_batteryPreconditioningRequest; 0x082 UI_requestActiveBatteryHeating. Bus: Vehicle bus (can0). Check: present, to check.
- Someone in the car: Model3CAN.dbc 0x3B3 UI_userPresent; 0x3C2 page 0 VCLEFT_frontOccupancySwitch (0x3A1 is blanked by override.dbc). Bus: Vehicle bus (can0). Check: present, to check.
- Charging status word, cable connected, charge port open: Model3CAN.dbc 0x212 BMS_uiChargeStatus (already decoded, kept only as true/false), 0x25D CP_chargeCableState, CP_chargeDoorOpen. Bus: Vehicle bus (can0). Check: present, to check.
- Charge limit, AC current set on the screen, scheduled-departure flags: Model3CAN.dbc 0x333 UI_chargeTerminationPct, UI_acChargeCurrentLimit, UI_scheduledDepartureEnabled, UI_smartAcChargingEnabled. Bus: Vehicle bus (can0). Check: present, to check.
- Energy still to add, time to full: override.dbc 0x352 page 1 BMS_energyToChargeComplete; Model3CAN.dbc 0x132 ChargeHoursRemaining132 (minutes despite the name). Bus: Vehicle bus (can0). Check: seen.
- Full-pack energy, buffer, expected (cold-limited) energy: override.dbc 0x352 page 0 BMS_nominalFullPackEnergy, page 1 BMS_energyBuffer, BMS_expectedEnergyRemaining. Bus: Vehicle bus (can0). Check: seen.
- Usable battery % (cold-locked part removed): Model3CAN.dbc 0x33A UI_uSOE; fallback from 0x352 energy and buffer. Bus: Vehicle bus (can0). Check: present, to check.
- Phases in use, pilot current, which current limit applies, AC line power: Model3CAN.dbc 0x204 PCS_chgPHA/B/CEnable and PCS_gridConfig; 0x31C CC_numPhases; 0x21D CP_pilotCurrent, CP_cableCurrentLimit; 0x13D CP_internalMaxAcCurrentLimit; 0x264 ChargeLinePower264, ChargeLineCurrentLimit264. Bus: Vehicle bus (can0). Check: present, to check.
- Car build: trim, drivetrain, pack size, wheels, paint, roof, spoiler, right-hand drive, map region, Supercharging access: Model3CAN.dbc 0x7FF carConfig GTW_* (paged). Bus: Vehicle bus (can0). Check: present, to check.
- Software changed (module build ids): Model3CAN.dbc page 17 of each info message: 0x300 BMS_appGitHash, 0x301 VCFRONT_infoAppGitHash, 0x51E FC_infoAppGitHashBytes, 0x335 DIR_appGitHash, 0x656 DIF_appGitHash. Bus: Vehicle bus (can0). Check: present, to check.
- Update being installed: Model3CAN.dbc 0x2E1 page 1 VCFRONT_vehicleStatusDBG = VEHICLE_STATUS_OTA, VCFRONT_batterySMState = OTA_STANDBY; model3y-opendbc.dbc 0x343 VCRIGHT_OTAState. Bus: Vehicle bus (can0). Check: present, to check.
- Heading: Model3CAN.dbc 0x3D9 UI_gpsVehicleHeading (never decode can3's 0x3D9). Bus: Chassis bus (can1). Check: not seen yet.
- Recommended tyre pressure: override.dbc 0x219 page 4 VCSEC_TPMSRecommendedPressure0/1 (0x123 TPMS soft warning is blanked by override.dbc). Bus: Vehicle bus (can0). Check: seen.
- Centre display on, camera active (Sentry event guess): Model3CAN.dbc 0x00C and 0x353 UI_displayOn and UI_cameraActive. Bus: Vehicle bus (can0). Check: present, to check.
- Navigating, energy at destination, in a Supercharger geofence: Model3CAN.dbc 0x082 UI_tripPlanningActive, UI_energyAtDestination; 0x238 UI_navRouteActive, UI_inSuperchargerGeofence. Bus: Vehicle bus (0x082), chassis bus (0x238). Check: not seen yet.
- Legacy Model S/X doors, frunk, boot, driver present, update in progress: opendbc-tesla_can.dbc 0x318 GTW_carState, 0x348 GTW_status. Bus: Legacy set (as the DBC crate maps it). Check: not seen yet.

Not on the CAN bus, and what Sentry Mate does instead:

- The software version text and the update history: Show imported TeslaMate updates with their version text; let the owner type the current version; mark 'software changed' when module build ids change, and remember each set of build ids with its version so other cars on the same build get it. Never show a guessed version.
- Update available, download and install progress: Leave out. Show 'installing' from the car's update status (once confirmed) and 'software changed' after the fact.
- Locked or unlocked: Leave out. No bundled DBC has a lock state (0x273 is only a request). Record locking and unlocking to hunt for it among the undecoded messages.
- Window open or closed: Leave out. Only switch presses and requests exist. Doors, trunk, frunk and liftgate cover 'left open' reminders.
- Sunroof state and percent open: Show the roof type from the car configuration message instead.
- Scheduled charging start time: Show 'plugged in, waiting' from the charge status and the schedule flags, and 'usually starts at HH:MM here' learned from past charges at that place.
- Navigation destination, distance, minutes, traffic: Publish only 'navigating' and energy at destination; leave the rest out.


<a id="page-settings-server"></a>

### Settings: Server and access

**Route:** `/settings/server`  
**Purpose:** Who can sign in, what they may see, and how the server is reached.

**Covers items from:** Environment variables: web interface address and protection (`docs.env.web`, 2), App frame (`web.shell`, 2).

**Data:** 2 have, 2 derive.

**Drill-downs:** Session -> sign out; Audit entry -> the changed item.

**Sections**

- **Access and hosting.** Accounts with Admin and Watch-only roles, per-account car lists, two-step sign-in, sessions (sign out here or everywhere), the audit log, host names for the certificate, the HTTPS port, and backups and the recovery key.
  Covers: `docs.env.web.virtual-host`, `docs.env.web.port`, `web.shell.access-model`, `web.shell.hosting`

**Better than TeslaMate**

- Real sign-in with roles, per-account car lists and two-step codes; TeslaMate has no sign-in of its own.
- HTTPS only, CSRF protection and a sealed audit log.
- Everything is encrypted at rest, always.


<a id="page-import"></a>

### Import

**Route:** `/settings/import`  
**Purpose:** Bring in TeslaMate history. Replaces TeslaMate's TeslaFi import page.

**Covers items from:** TeslaFi Import (`web.import`, 11), Frequently Asked Questions (`docs.faq`, 1).

**Data:** 7 have, 5 derive.

**Drill-downs:** Report -> Timeline at an overlap; Car match -> Settings: Cars.

**Sections**

- **Upload.** Upload a TeslaMate backup through the browser (resumable, HTTPS, admins only), match each imported car to a car by VIN prefix or create it, then start. Discard an upload at any time.
  Covers: `web.import.breadcrumb`, `web.import.start`, `web.import.start-error`, `web.import.discard`, `web.import.run-error`
- **Progress.** Status (uploading, checking, importing, done, failed), pass and bytes; safe to leave the page.
  Covers: `web.import.files.col-status`, `web.import.restart-note`
- **Report.** Per table and per car: rows read, imported and skipped, with a reason for every skipped count. No row values or positions in the report.
  Covers: `web.import.rejected-count`, `web.import.rejected.col-reason`
- **Overlap with CAN history.** Spans where imported and CAN history both exist, which one is shown, and a switch; addresses are filled in for imported places.
  Covers: `faq.no-backfill`, `web.import.docs-overlap`, `web.import.docs-backfill`

**Better than TeslaMate**

- Reads TeslaMate's own backup, checks the whole file before storing anything, and can be re-run safely.
- Handles any number of cars and matches them by VIN prefix.
- Overlap with CAN history is shown and can be flipped, not silently dropped.


<a id="page-health"></a>

### Data health and storage

**Route:** `/cars/{car}/health  and  /system`  
**Purpose:** Is the data complete and trustworthy, and how much room does it take. Replaces TeslaMate's Database Information dashboard and its health check.

**Covers items from:** Database Information (`dash.database-info`, 17), Node-RED integration (`docs.node-red`, 4), Health check and logging control API (`web.health-api`, 3), Frequently Asked Questions (`docs.faq`, 2), Screenshots (`docs.screenshots`, 1).

**Data:** 9 have, 16 derive, 1 new signal, 1 not on CAN.

**Filters:** Car; Period (for gaps).

**Drill-downs:** Gap -> Timeline at that time; Waiting key -> Settings: Cars; Rebuild -> runs for the chosen window.

**Sections**

- **Per-car health.** Last upload, last log check (verified, unsigned, failed, with the reason), chain gaps, dropped frames, unknown spans, logs waiting, rebuild pending; each with an in-app fix.
  Covers: `faq.stopped-recording`
- **Per-car totals.** Logged distance, odometer, drives, charges and updates per car, per source (CAN and imported).
  Covers: `dbinfo.filter.car`, `dbinfo.control.refresh`, `dbinfo.stat.logged_distance`, `dbinfo.stat.odometer`, `dbinfo.stat.charge_count`, `dbinfo.stat.drive_count`, `dbinfo.stat.update_count`, `dbinfo.stat.current_version`
- **Storage.** Bytes by kind in plain words (raw logs, samples, charge curves, tables, places and settings, address cache, offline maps), free disk space, growth and 'full in N days'.
  Covers: `dbinfo.section.database`, `dbinfo.table.sizes`, `dbinfo.sizes.table`, `dbinfo.sizes.data`, `dbinfo.sizes.total`, `dbinfo.table.row_counts`, `dbinfo.rows.table_name`, `dbinfo.rows.row_count`, `dbinfo.stat.total_size`
- **Server health.** Uptime, versions, memory and cache use, last housekeeping, timings (decode per log, rebuild per window, API response times), and a health address for container checks that reveals nothing.
  Covers: `faq.docker-timestamps`, `web.health-api.health-check`
- **API errors.** The one error shape every API call uses, and what an unknown or hidden car returns.
  Covers: `web.health-api.unknown-car`, `web.health-api.errors`
- **Signals.** Every decoded signal from the newest log with its name, unit, value names and time, searchable; a chosen series over a period, with CSV.
  Covers: `nr.dash.state-table`, `nr.dash.state-table.parameter`, `nr.dash.state-table.value`, `nr.dash.state-table.sort-scroll`
- **Whole page (docs screenshot).** The documentation gallery picture of this TeslaMate page.
  Covers: `shots.database-info`

**Better than TeslaMate**

- Shows whether the data is complete and trusted, with causes and fixes, instead of database internals.
- Nothing needs repairing by hand: anything derived is rebuilt from the stored logs.
- Storage is explained by kind, with free space and a growth trend.
- A searchable table of every decoded signal, with names, units and value names.

**Data gaps**

New CAN signals to decode:

- Software changed (module build ids): Model3CAN.dbc page 17 of each info message: 0x300 BMS_appGitHash, 0x301 VCFRONT_infoAppGitHash, 0x51E FC_infoAppGitHashBytes, 0x335 DIR_appGitHash, 0x656 DIF_appGitHash. Bus: Vehicle bus (can0). Check: present, to check.
- Update being installed: Model3CAN.dbc 0x2E1 page 1 VCFRONT_vehicleStatusDBG = VEHICLE_STATUS_OTA, VCFRONT_batterySMState = OTA_STANDBY; model3y-opendbc.dbc 0x343 VCRIGHT_OTAState. Bus: Vehicle bus (can0). Check: present, to check.

Not on the CAN bus, and what Sentry Mate does instead:

- The software version text and the update history: Show imported TeslaMate updates with their version text; let the owner type the current version; mark 'software changed' when module build ids change, and remember each set of build ids with its version so other cars on the same build get it. Never show a guessed version.


<a id="page-help"></a>

### Help and about

**Route:** `/help`  
**Purpose:** Version, licences, data credits and short answers to common questions.

**Covers items from:** Settings: version, links and sign out (`web.settings.about`, 4), Legal texts (`web.legal`, 2).

**Data:** 6 derive.

**Drill-downs:** Answer -> the page it is about.

**Sections**

- **About.** Version, API version, UI pack and version, links to source code and documentation, and an opt-in notice when a newer release exists.
  Covers: `web.settings.about.version`, `web.settings.about.update-available`, `web.settings.about.source-link`, `web.settings.about.docs-link`
- **Licence and notices.** Licence text, notices and data credits (OpenStreetMap, GeoNames, DBC sources).
  Covers: `web.legal.notice`, `web.legal.license`
- **Questions and answers.** Why a car is missing, why data is late, why a span is unknown, how pairing works.
  New in Sentry Mate (no TeslaMate item).

**Better than TeslaMate**

- Data credits and licences in one place; release checks only if the owner turns them on.


<a id="page-frame"></a>

### App frame

**Route:** `(shared by every page)`  
**Purpose:** The navigation, notice bar and footer that every page shares, so moving between pages never loses the car or the period.

**Covers items from:** App frame (`web.shell`, 20).

**Data:** 8 have, 12 derive.

**Filters:** Car switcher (keeps the current page); Shared period picker in the car's time zone; Page finder.

**Drill-downs:** Any page, keeping car and period.

**Sections**

- **Navigation, notices and footer.** Menu grouped by task (Cars, Driving, Charging, Battery and energy, Reports, Set-up), each entry with a one-line description and a type-to-find box ('drain', 'cost'). A car switcher and the shared period picker sit in the header. Notices (vault, certificate, recovery key file, opt-in release notice) appear in one bar with the cause and the fix. Short success or error notes after an action. Footer with version, licence and notices. Tab titles name the page and the car. Home routing: first run goes to set-up, then to Fleet.
  Covers: `shell.nav.home`, `shell.nav.geofences`, `shell.nav.settings`, `shell.nav.mobile_menu`, `shell.banner.flash`, `shell.footer.update_available`, `shell.footer.legal`, `shell.page_title`, `shell.installable`, `web.shell.logo-home`, `web.shell.menu-toggle`, `web.shell.geofences-nav`, `web.shell.settings-nav`, `web.shell.flash`, `web.shell.update-footer`, `web.shell.legal-footer`, `web.shell.page-title`, `web.shell.installable`, `web.shell.home-routing`, `web.shell.document-language`

**Better than TeslaMate**

- One app and one sign-in. There is no second dashboard tool with its own menu, look and links back.
- The chosen car and period travel with you from page to page. TeslaMate's dashboard menu drops both.
- The menu is grouped by task with a short description per page, and a finder box searches page names and keywords.
- Notices name the cause and the next step (for example 'certificate expires in 12 days: renew in Settings > Server').
- Desktop first, sharp at native 4K, with an interface size preference per person.


## Not applicable, and what replaces it

These TeslaMate items exist because of how TeslaMate works (polling Tesla's servers, a separate dashboard tool, a database server). Sentry Mate does not need them. Each group names its replacement.

### Links back to a separate web app, the dashboard menu and the dashboard landing page (54)

Replaced by: App frame navigation: one app, one sign-in; pages grouped by task, the chosen car and period carried from page to page.

`bh.link.web_app`, `bh.link.dashboards`, `pr.link.web_app`, `pr.link.dashboards`, `vd.link.web_app`, `vd.link.dashboards`, `ef.link.web_app`, `ef.link.dashboards`, `tp.link.web_app`, `tp.link.dashboards`, `mi.link.web_app`, `mi.link.dashboards`, `shell.nav.dashboards`, `charges.link.home`, `charges.link.dashboards`, `cd.link.home`, `cd.link.dashboards`, `cs.link.home`, `cs.link.dashboards`, `cl.link.home`, `cl.link.dashboards`, `drives.link.web_app`, `drives.link.dashboards`, `dd.link.web_app`, `dd.link.dashboards`, `ds.link.web_app`, `ds.link.dashboards`, `trip.link.web_app`, `trip.link.dashboards`, `dt.link.web_app`, `dt.link.dashboards`, `home.nav.dashboard_list`, `home.nav.search`, `home.nav.favorite_toggle`, `home.hero.image`, `home.control.no_time_picker`, `home.panel_menu`, `overview.link.webapp`, `overview.link.dashboards`, `states.link.webapp`, `states.link.dashboards`, `timeline.link.webapp`, `timeline.link.dashboards`, `statistics.link.webapp`, `statistics.link.dashboards`, `locations.link.web_app`, `locations.link.dashboards`, `visited.link.web_app`, `visited.link.dashboards`, `updates.link.web_app`, `updates.link.dashboards`, `dbinfo.link.web_app`, `dbinfo.link.dashboards`, `web.shell.dashboards-menu`

### Charting-tool panel menus (inspect, export, share) (4)

Replaced by: A visible Download CSV button on every table and chart, plus CSV on every list endpoint.

`overview.panel_menu`, `states.panel_menu`, `timeline.table.panel_menu`, `statistics.table.panel_menu`

### Polling, sleep attempts, wake probes, streaming switches and logging pause/resume (26)

Replaced by: Car overview 'Why the car is awake' and data age; Timeline per-bus states. The CAN Bridge only listens, so the logger never wakes the car or keeps it awake.

`summary.flag.fetching`, `summary.state.wake_probe`, `summary.state.rate_limited`, `summary.state.sleep_exit`, `summary.action.try_sleep`, `summary.action.cancel_sleep`, `summary.status.sleep_error`, `summary.data.sleep_rules`, `summary.data.idle_timer`, `api.logging.suspend`, `api.logging.resume`, `api.logging.lookup_errors`, `docs.env.tesla-api.polling-asleep-interval`, `docs.env.tesla-api.polling-charging-interval`, `docs.env.tesla-api.polling-driving-interval`, `docs.env.tesla-api.polling-online-interval`, `docs.env.tesla-api.polling-default-interval`, `docs.env.tesla-api.polling-minimum-interval`, `faq.polling-gap`, `web.settings.car.data-collection-enabled`, `web.settings.car.sleep-backoff`, `web.settings.car.sleep-idle`, `web.settings.car.sleep-require-locked`, `web.settings.car.streaming-enabled`, `web.health-api.suspend-logging`, `web.health-api.resume-logging`

### Tesla account sign-in, tokens, API providers and manual car discovery (49)

Replaced by: Sentry Mate accounts (Settings > Server and access) and CAN Bridge pairing with per-car upload tokens and keys (Settings > Cars).

`cars.empty.link_settings`, `cars.empty.reload`, `cars.empty.reload_result`, `cars.entry.redirects`, `cars.entry.base_url_capture`, `docs.env.tesla-api.tesla-api-host`, `docs.env.tesla-api.tesla-api-pool-size`, `docs.env.tesla-api.tesla-auth-host`, `docs.env.tesla-api.tesla-auth-path`, `docs.env.tesla-api.tesla-auth-client-id`, `docs.env.tesla-api.tesla-wss-host`, `docs.env.tesla-api.tesla-wss-tls-accept-invalid-certs`, `docs.env.tesla-api.tesla-wss-use-vin`, `docs.env.tesla-api.token`, `api.which-api`, `api.fleet-cutoff-signal`, `api.fleet-limits`, `api.owner-limits-warning`, `api.telemetry-differences`, `api.how-to`, `api.providers.intro`, `api.provider.myteslamate-fleet`, `api.provider.myteslamate-streaming`, `api.provider.teslemetry-fleet`, `api.provider.teslemetry-streaming`, `api.direct.requirements`, `api.direct.region-hosts`, `api.direct.client-id`, `api.direct.commands`, `api.direct.switch`, `api.direct.telemetry`, `api.direct.streaming-off-warning`, `tokens.types`, `tokens.no-builtin`, `tokens.tool-desktop`, `tokens.tool-ios`, `tokens.tool-nix`, `faq.tokens`, `faq.car-missing.reload`, `web.settings.general.reload-vehicles`, `web.settings.general.reload-result`, `web.settings.about.sign-out`, `web.signin.access-token`, `web.signin.refresh-token`, `web.signin.validation`, `web.signin.submit`, `web.signin.error`, `web.signin.provider-mode`, `web.signin.token-help`

### Database server internals, repair guides and unfinished-session lists (50)

Replaced by: Data health: storage by kind, cache and timing figures, chain gaps and Unknown spans, and in-app fixes (trust a key, upload, rebuild a window).

`summary.flag.health`, `shell.banner.encryption_key`, `charges.link.fix_guide`, `docs.env.core.encryption-key`, `docs.env.core.database-user`, `docs.env.core.database-pass`, `docs.env.core.database-name`, `docs.env.core.database-host`, `docs.env.core.database-port`, `docs.env.core.database-socket-dir`, `docs.env.core.database-pool-size`, `docs.env.core.database-timeout`, `docs.env.core.database-ssl`, `docs.env.core.database-ssl-ca-cert-file`, `docs.env.core.database-ssl-sni`, `docs.env.core.database-ipv6`, `dbinfo.flag.open_charges`, `dbinfo.flag.open_drives`, `dbinfo.link.review_open_charges`, `dbinfo.link.review_open_drives`, `dbinfo.text.open_sessions_help`, `dbinfo.stat.db_version`, `dbinfo.sizes.indexes`, `dbinfo.table.indexes`, `dbinfo.indexes.table`, `dbinfo.indexes.index`, `dbinfo.indexes.scans`, `dbinfo.indexes.tuples_read`, `dbinfo.indexes.tuples_fetched`, `dbinfo.indexes.size`, `dbinfo.link.reindex_guide`, `dbinfo.stat.shared_buffers`, `dbinfo.link.shared_buffers_docs`, `dbinfo.stat.db_timezone`, `dbinfo.section.query_stats`, `dbinfo.flag.query_stats_enabled`, `dbinfo.stat.query_stats_reset`, `dbinfo.stat.query_stats_count`, `dbinfo.text.query_stats_about`, `dbinfo.table.top_by_mean`, `dbinfo.top_by_mean.calls`, `dbinfo.top_by_mean.mean_time`, `dbinfo.top_by_mean.total_time`, `dbinfo.top_by_mean.query`, `dbinfo.table.top_by_total`, `dbinfo.top_by_total.calls`, `dbinfo.top_by_total.mean_time`, `dbinfo.top_by_total.total_time`, `dbinfo.top_by_total.query`, `web.shell.encryption-warning`

### Hosting variables, link addresses and hosted or add-on variants (14)

Replaced by: Settings > Server and access: one container, HTTPS on its own port, host names for the certificate.

`docs.env.web.url-path`, `docs.env.web.check-origin`, `docs.env.web.http-binding-address`, `docs.env.tesla-api.http-pool-size`, `docs.env.tesla-api.http-pool-timeout`, `docs.env.tesla-api.ulimit-max-nofile`, `projects.guru`, `projects.grafana-dashboards`, `projects.ha-addon`, `projects.myteslamate`, `web.settings.general.url-web-app`, `web.settings.general.url-dashboards`, `web.settings.general.url-placeholders`, `web.settings.general.url-errors`

### TeslaFi import folder, file scan and time-zone choice (19)

Replaced by: Import: a TeslaMate backup uploaded through the browser, checked whole before anything is stored, with a per-table report.

`docs.env.core.import-dir`, `docs.env.core.teslafi-import-vehicle-id`, `web.import.import-mode`, `web.import.files-found`, `web.import.rescan`, `web.import.files.col-month`, `web.import.file-name-pattern`, `web.import.files.col-path`, `web.import.rejected.col-file`, `web.import.rejected.col-row`, `web.import.rejected-truncated`, `web.import.timezone`, `web.import.timezone-locked`, `web.import.docs-recovery-rules`, `web.import.docs-preparation`, `web.import.docs-bulk-export`, `web.import.vehicle-id-default`, `web.import.import-dir`, `web.import.apiscraper-path`

### Third-party add-on plumbing (Node-RED set-up, hand-written Home Assistant config, controls, chat channels) (22)

Replaced by: Settings > Integrations: MQTT with Home Assistant discovery, a preview of every message, connection status, and explicit drive/charge events.

`mqtt.active-route-legacy`, `mqtt.note-vin-retained`, `ha.discovery.legacy-cleanup`, `ha.discovery.duplicate-note`, `ha.screenshot`, `ha.current-status`, `ha.official-component`, `ha.manual-config`, `ha.lovelace.lock-buttons`, `ha.lovelace.thermostat`, `nr.setup.container`, `nr.setup.modules`, `nr.setup.extra-modules`, `nr.setup.import`, `nr.setup.telegram`, `nr.setup.email`, `nr.notify.other-channels`, `nr.notify.test`, `nr.notify.redeploy-noise`, `nr.screenshot.notifications`, `nr.screenshot.dashboard`, `nr.screenshot.setup`

## New CAN signals: one list for all pages

'Seen' means the signal was in the owner's real Model Y log with sensible values. Decode the seen ones first; decode the others too, but show them with a 'to be confirmed' note until a log checks them.

| Signal need | Source | Bus | Check | Next check | Pages |
|---|---|---|---|---|---|
| Sentry mode on | Model3CAN.dbc 0x284 UIvehicleModes: UIsentryMode284 | Vehicle bus (can0) | seen | Read 1 while parked with Sentry on; confirm 0 with Sentry off. | Car overview, Timeline, Battery, Settings: Integrations |
| Service mode | Model3CAN.dbc 0x284 UIvehicleModes: UIserviceMode284 | Vehicle bus (can0) | present, to check | Read 0; UI_limitMode (0x334) said LIMIT_SERVICE in the same minute, so check once in service mode. | Car overview, Settings: Integrations |
| Each door, trunk, frunk and liftgate | Model3CAN.dbc 0x102 / 0x103 latch status, 0x2E1 page 0 VCFRONT_frunkLatchStatus, 0x142 page 0 VCLEFT_liftgateState (page 1 liftgate angle) | Vehicle bus (can0) | seen | All read closed with clear value names; open each one once while recording. | Car overview, Settings: Integrations |
| Climate on, preconditioning, overheat protection, fan, set points | override.dbc 0x243 page 0 VCRIGHT_hvacPowerState, hvacBlowerSegment, hvacACRunning; Model3CAN.dbc 0x2F3 UI_hvacReqUserPowerState, UI_hvacReqTempSetpointLeft/Right (raw 0 = LO, 26 = HI) | Vehicle bus (can0) | seen | 0x243 layout checked on the owner's car; set points read 23.5 C both sides. | Car overview, Timeline, Drive detail, Battery, Efficiency and temperatures, Settings: Integrations |
| Keep-climate mode (keep, dog, camp) | Model3CAN.dbc 0x2F3 UI_hvacReqKeepClimateOn | Vehicle bus (can0) | present, to check | Read OFF; it is the screen's request, so check once with Dog mode on. | Car overview, Timeline, Settings: Integrations |
| Battery preconditioning (apart from the cabin) | Model3CAN.dbc 0x3B3 UI_batteryPreconditioningRequest; 0x082 UI_requestActiveBatteryHeating | Vehicle bus (can0) | present, to check | 0x3B3 present; check during a navigated drive to a Supercharger. | Car overview, Settings: Integrations |
| Someone in the car | Model3CAN.dbc 0x3B3 UI_userPresent; 0x3C2 page 0 VCLEFT_frontOccupancySwitch (0x3A1 is blanked by override.dbc) | Vehicle bus (can0) | present, to check | Read 0 with nobody in; check with someone seated. | Car overview, Timeline, Settings: Integrations |
| Charging status word, cable connected, charge port open | Model3CAN.dbc 0x212 BMS_uiChargeStatus (already decoded, kept only as true/false), 0x25D CP_chargeCableState, CP_chargeDoorOpen | Vehicle bus (can0) | present, to check | CP_chargeCableState said CONNECTED while CP_chargeCablePresent said NOT_PRESENT in the same minute; check plugged and unplugged once. | Car overview, Charge detail, Settings: Integrations |
| Charge limit, AC current set on the screen, scheduled-departure flags | Model3CAN.dbc 0x333 UI_chargeTerminationPct, UI_acChargeCurrentLimit, UI_scheduledDepartureEnabled, UI_smartAcChargingEnabled | Vehicle bus (can0) | present, to check | Limit read 100 % while the screen state is unknown; compare with the screen once. | Car overview, Charges, Charge detail, Battery, Settings: Integrations |
| Energy still to add, time to full | override.dbc 0x352 page 1 BMS_energyToChargeComplete; Model3CAN.dbc 0x132 ChargeHoursRemaining132 (minutes despite the name) | Vehicle bus (can0) | seen | 0x352 page layout checked (1.18 kWh seen); read ChargeHoursRemaining132 during a charge and compare. | Car overview, Charge detail, Settings: Integrations |
| Full-pack energy, buffer, expected (cold-limited) energy | override.dbc 0x352 page 0 BMS_nominalFullPackEnergy, page 1 BMS_energyBuffer, BMS_expectedEnergyRemaining | Vehicle bus (can0) | seen | 68.08 and 67.54 kWh full pack, 3.05 kWh buffer seen; check expected energy on a cold day. | Car overview, Drives, Battery, Software, Settings: Cars, Settings: Integrations |
| Lifetime AC, DC, regen and drive energy | Model3CAN.dbc 0x3F2 pages 0-3 BMS_acChargerKwhTotal, BMS_dcChargerKwhTotal, BMS_kwhRegenChargeTotal, BMS_kwhDriveDischargeTotal | Vehicle bus (can0) | seen | AC + DC + regen added up exactly to TotalChargeKWh3D2. | Charges, Battery, Efficiency and temperatures |
| Usable battery % (cold-locked part removed) | Model3CAN.dbc 0x33A UI_uSOE; fallback from 0x352 energy and buffer | Vehicle bus (can0) | present, to check | Read 102 % and the same message gave a 580 mi ideal range, so 0x33A's layout is in doubt. Use the 0x352 fallback until checked. | Car overview, Drives, Drive detail, Battery, Settings: Integrations |
| Phases in use, pilot current, which current limit applies, AC line power | Model3CAN.dbc 0x204 PCS_chgPHA/B/CEnable and PCS_gridConfig; 0x31C CC_numPhases; 0x21D CP_pilotCurrent, CP_cableCurrentLimit; 0x13D CP_internalMaxAcCurrentLimit; 0x264 ChargeLinePower264, ChargeLineCurrentLimit264 | Vehicle bus (can0) | present, to check | Single phase read correctly; check the enable bits and pilot current on a three-phase charge. | Car overview, Charges, Charge detail, Settings: Integrations |
| Supercharger or other DC charger, and the energy it bills | Model3CAN.dbc 0x217 FC_brand, FC_class (page 0), FC_billingEnergy (page 1); 0x214 FC_type; 0x21D CP_teslaDcState | Vehicle bus (can0) | not seen yet | Only an AC home log so far; record one Supercharger session (CCS ones may not send these). | Charges, Charge detail, Places, Settings: Cars |
| Car build: trim, drivetrain, pack size, wheels, paint, roof, spoiler, right-hand drive, map region, Supercharging access | Model3CAN.dbc 0x7FF carConfig GTW_* (paged) | Vehicle bus (can0) | present, to check | Plausible names (PACK_74_KWH) seen; value tables are old, so let the owner correct them. | Car overview, Charges, Settings: Cars, Settings: Integrations |
| Software changed (module build ids) | Model3CAN.dbc page 17 of each info message: 0x300 BMS_appGitHash, 0x301 VCFRONT_infoAppGitHash, 0x51E FC_infoAppGitHashBytes, 0x335 DIR_appGitHash, 0x656 DIF_appGitHash | Vehicle bus (can0) | present, to check | BMS, VCFRONT and FC ids present; compare raw bytes (56 bits do not fit a float). Gives a change, never the version text. | Car overview, Software, Settings: Integrations, Data health and storage |
| Update being installed | Model3CAN.dbc 0x2E1 page 1 VCFRONT_vehicleStatusDBG = VEHICLE_STATUS_OTA, VCFRONT_batterySMState = OTA_STANDBY; model3y-opendbc.dbc 0x343 VCRIGHT_OTAState | Vehicle bus (can0) | present, to check | Series present while parked; record one real update to confirm the values. | Timeline, Software, Settings: Integrations, Data health and storage |
| Heading | Model3CAN.dbc 0x3D9 UI_gpsVehicleHeading (never decode can3's 0x3D9) | Chassis bus (can1) | not seen yet | Chassis bus sleeps when parked; check while driving. Bearing between 1 Hz positions works meanwhile. | Car overview, Drive detail, Travel, Settings: Integrations |
| Battery heating on, kept warm, heating power | Model3CAN.dbc 0x2E1 page 0 VCFRONT_isActiveHeatingBattery; 0x212 BMS_keepWarmRequest; 0x266 RearHeatPower266, 0x2E5 FrontHeatPower2E5, 0x268 SystemHeatPower268 | Vehicle bus (can0) | present, to check | 0x212 present; heat power not seen while parked. Check on a cold charge and during preconditioning. | Car overview, Drive detail, Charge detail |
| Recommended tyre pressure | override.dbc 0x219 page 4 VCSEC_TPMSRecommendedPressure0/1 (0x123 TPMS soft warning is blanked by override.dbc) | Vehicle bus (can0) | seen | 2.9 bar seen; which wheel each slot is still unknown. | Car overview, Drive detail, Settings: Integrations |
| The car's distance unit | Model3CAN.dbc 0x293 UI_distanceUnits | Vehicle bus (can0) | seen | Read miles. | Settings: Display and units |
| The car's rated Wh per mile | Model3CAN.dbc 0x33A UI_ratedWHpM | Vehicle bus (can0) | present, to check | Read 1023 (all bits set, likely 'not available'); work the factor out from energy and range instead. | Efficiency and temperatures |
| Centre display on, camera active (Sentry event guess) | Model3CAN.dbc 0x00C and 0x353 UI_displayOn and UI_cameraActive | Vehicle bus (can0) | present, to check | An event during Sentry is only an inference; check against a real Sentry event. | Car overview, Settings: Integrations |
| Climate energy used per drive | Model3CAN.dbc 0x381 page 8 VCFRONT_cabinHeatEnergyDuringDri, VCFRONT_compEnergyDuringDrive (150 Wh steps) | Vehicle bus (can0) | present, to check | Present while parked; check how it resets per drive. | Drives, Drive detail |
| Navigating, energy at destination, in a Supercharger geofence | Model3CAN.dbc 0x082 UI_tripPlanningActive, UI_energyAtDestination; 0x238 UI_navRouteActive, UI_inSuperchargerGeofence | Vehicle bus (0x082), chassis bus (0x238) | not seen yet | Check during a navigated drive. | Travel, Settings: Integrations |
| Accessory power kept on | Model3CAN.dbc 0x273 UI_accessoryPowerRequest | Vehicle bus (can0) | not seen yet | Check with the accessory power setting on. | Car overview, Timeline |
| Parking brake engaged | Model3CAN.dbc 0x228 EPBRunitStatus228, 0x288 EPBLunitStatus288 | Vehicle bus (can0) | not seen yet | Check once parked and once driving. | Car overview |
| Brick voltage spread (balance, LFP hint) | override.dbc 0x332 page 1 BattBrickVoltageMax332 / Min332 | Vehicle bus (can0) | seen | Present with the override layout; LFP shows about 3.4-3.65 V at the top near full, NCA/NMC about 4.1-4.2 V. | Battery, Settings: Cars |
| Most charge current the pack accepts, DC station limit | Model3CAN.dbc 0x2D2 MaxChargeCurrent2D2; 0x27D CP_evseMaxDcCurrentLimit | Vehicle bus (can0) | not seen yet | Not seen yet; the car's highest recorded charge power is a working fallback. | Car overview, Charges |
| Legacy Model S/X doors, frunk, boot, driver present, update in progress | opendbc-tesla_can.dbc 0x318 GTW_carState, 0x348 GTW_status | Legacy set (as the DBC crate maps it) | not seen yet | No legacy car checked yet. | Car overview, Settings: Integrations |

Decoding order: (1) seen: Sentry mode, doors and closures, climate state and set points, energy to full, full-pack energy and buffer, lifetime counters, recommended tyre pressure, distance unit, brick voltages; (2) present, to check: charge status word and cable, charge limit, phases and pilot current, keep-climate, battery preconditioning, occupancy, service mode, car configuration, module build ids, update status, battery heating, display, climate energy, usable %, rated Wh/mi; (3) not seen yet: Supercharger identity and billing, heading, navigation, accessory power, parking brake, pack charge limit, legacy S/X.

## Not on the CAN bus: one list for all pages

| Need | What Sentry Mate does instead | Pages |
|---|---|---|
| The software version text and the update history | Show imported TeslaMate updates with their version text; let the owner type the current version; mark 'software changed' when module build ids change, and remember each set of build ids with its version so other cars on the same build get it. Never show a guessed version. | Car overview, Timeline, Software, Settings: Integrations, Data health and storage |
| Update available, download and install progress | Leave out. Show 'installing' from the car's update status (once confirmed) and 'software changed' after the fact. | Car overview, Software, Settings: Integrations |
| Release-notes link | Only where a version is known, only when the owner allows outside links, and opened by the viewer's click (the server sends nothing). | Software |
| Locked or unlocked | Leave out. No bundled DBC has a lock state (0x273 is only a request). Record locking and unlocking to hunt for it among the undecoded messages. | Car overview, Settings: Integrations |
| Window open or closed | Leave out. Only switch presses and requests exist. Doors, trunk, frunk and liftgate cover 'left open' reminders. | Car overview, Settings: Integrations |
| Sunroof state and percent open | Show the roof type from the car configuration message instead. | Settings: Integrations |
| Scheduled charging start time | Show 'plugged in, waiting' from the charge status and the schedule flags, and 'usually starts at HH:MM here' learned from past charges at that place. | Car overview, Settings: Integrations |
| Booked in at a service centre | Show service mode from the car, and the place name when parked at a place of kind 'service centre'. | Car overview |
| Navigation destination, distance, minutes, traffic | Publish only 'navigating' and energy at destination; leave the rest out. | Settings: Integrations |
| The price paid at a Supercharger or public charger | A price per place (per kWh, per minute, time-of-use, session fee) or a per-session override; imported TeslaMate costs are kept. | Charges, Charge detail |
| Grid-side energy of a DC charge | Show DC efficiency as unknown with the reason; show the energy delivered at the inlet (charger volts x amps) and the Supercharger's billed energy once confirmed. | Charges, Charge detail |
| A clean battery-chemistry flag | Per-car setting: automatic from brick voltages and pack size (with the evidence), or set by the owner; prefilled from a TeslaMate import. | Charges, Battery, Settings: Cars |
| Usable capacity when new, for a car logged late | Owner value per car; otherwise the median of the earliest readings or imported charges, with the pack-size code as a labelled hint. Always say which. | Battery |
| Full-charge range when new, for a car logged late | Owner value per car; otherwise the median full-charge range of the first 30 days. | Battery |
| The car's own 'estimated range' | Sentry Mate's estimate: energy remaining divided by recent measured consumption, labelled as Sentry Mate's. | Car overview |
| Purpose of each drive for a tax log | Entered by the owner per drive, or by rules; stored apart from rebuilt rows. | Logbook |
| When free Supercharging started and ended | A dated range per car typed by the owner, with the car's entitlement shown as a hint. | Settings: Cars |
| Cars on the Tesla account and their Tesla names | A car appears when its CAN Bridge uploads; the owner names it; model and year come from the VIN prefix. | Settings: Cars |
| 'Offline' as Tesla's cloud reports it | Report what the logs show: online, asleep, or unknown (no log, recording paused, failed check). | Timeline |
| Car mass for hill-corrected efficiency | Typical mass per model and year from the VIN prefix, with an owner override; or drop the correction because energy is measured. | Drives |
| Car display order | A per-person order stored by Sentry Mate; default by last activity. | Fleet |

## Coverage by page

| Page | Items | have | derive | new signal | not on CAN |
|---|---:|---:|---:|---:|---:|
| Fleet | 8 | 2 | 6 | 0 | 0 |
| Car overview | 84 | 39 | 16 | 21 | 8 |
| Timeline | 46 | 20 | 24 | 1 | 1 |
| Drives | 59 | 15 | 43 | 1 | 0 |
| Drive detail | 36 | 20 | 12 | 4 | 0 |
| Trip | 47 | 19 | 28 | 0 | 0 |
| Logbook | 13 | 7 | 6 | 0 | 0 |
| Charges | 81 | 24 | 56 | 1 | 0 |
| Charge detail | 49 | 25 | 21 | 3 | 0 |
| Battery | 79 | 17 | 56 | 6 | 0 |
| Efficiency and temperatures | 43 | 6 | 36 | 1 | 0 |
| Statistics | 31 | 5 | 26 | 0 | 0 |
| Travel | 75 | 12 | 63 | 0 | 0 |
| Software | 16 | 2 | 5 | 6 | 3 |
| Places | 28 | 14 | 14 | 0 | 0 |
| Settings: Cars | 8 | 2 | 4 | 2 | 0 |
| Settings: Display and units | 18 | 3 | 15 | 0 | 0 |
| Settings: Integrations | 133 | 60 | 18 | 37 | 18 |
| Settings: Server and access | 4 | 2 | 2 | 0 | 0 |
| Import | 12 | 7 | 5 | 0 | 0 |
| Data health and storage | 27 | 9 | 16 | 1 | 1 |
| Help and about | 6 | 0 | 6 | 0 | 0 |
| App frame | 20 | 8 | 12 | 0 | 0 |
| Not applicable | 238 | | | | |
| **Total** | **1161** | 318 | 490 | 84 | 31 |

## Checking coverage

`coverage-matrix.json` (next to this file) lists every inventory item with its tab, page, section and data status, and an `unmapped` list that must stay empty. When the plan was written, a script checked that every item of the inventories is in the matrix exactly once and listed exactly once in this document. The inventories themselves are working notes about TeslaMate and are not kept in this repository; this plan and the matrix are what the pages are built and checked against.

Each page marks the part of the page that covers an item with `data-cover="<item id>"`, so a test can load every page of every UI pack and check that every item mapped to that page is there.
