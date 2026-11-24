# Bug hunt

[← Back to the README](../../README.md)

The owner's instruction: find every problem and bug in every page and every feature, and assume everything is broken. This page records what was checked, what was found and fixed, and what was found and is still open. All data used was made up.

How it was done: the code was split into areas (below). In each round one hunter per area listed every feature, route, endpoint and job of its area and tried to break each one (edge cases, hostile input, concurrency, data correctness, every page at phone, laptop, 1440p and 4K sizes, light and dark, each interface size). A second pass then tried to refute every finding against the code and the docs; only what it could reproduce counted. Each confirmed bug got a failing test first where that was feasible, then the fix.

State: 1 round(s); 22 bugs fixed, 154 found and open, 4 reports refuted.

- [Found and not fixed](#found-and-not-fixed)
- [Fixed](#fixed)
- [Reported and refuted](#reported-and-refuted)
- [Checked and found working](#checked-and-found-working)

## Found and not fixed

### Inputs and ingest

- **TeslaFi import: one row with a wrong date drops the rest of the file and makes other months permanently 'already imported'** (medium; crates/import/src/teslafi/mod.rs:1275-1289 (already_imported / out_of_order), :1457-1483 (end_run builds the Piece from first_ms..last_ms), crates/import/src/teslafi/poll.rs:448-486 (parse_local takes). confirmed, fix not done yet
- **TeslaFi import: a row dated before 1970 is stored at 1970-01-01 00:00:00 instead of being skipped** (low; crates/import/src/teslafi/poll.rs:503 (to_utc: u64::try_from(d.timestamp_millis()).unwrap_or(0))). confirmed, fix not done yet
- **Local TeslaMate: the saved database password can be sent to another host without typing it again (password_resend rule bypass)** (medium; crates/tmlocal/src/lib.rs:225-246 (validate: database side checked only when db_used), :689-707 (Service::put), crates/tmlocal/src/store.rs:265-296 (put keeps the password); route PUT /integrations/te). confirmed, fix not done yet
- **A zipped TeslaFi month in which the car mostly stood asleep is refused as 'a zip bomb?' (the same file plain or gzipped imports)** (medium; crates/import/src/zip.rs:28 (MAX_RATIO = 100), :419-424 (the check in read_in), surfaced through crates/import/src/teslafi/mod.rs:779-830 (source) as Error::Unpack). confirmed, fix not done yet
- **TeslaFi import: a gzipped or xz'd upload unpacks without any ratio or size limit (a 310 KB file kept an import busy for 28 s; a zip of the same is refused)** (low; crates/import/src/teslafi/mod.rs:779-830 (Importer::source: gzip and xz readers fed straight into file()), crates/import/src/teslafi/csv.rs:28-68 (Lines::next)). confirmed, fix not done yet
- **TeslaFi charge costs: a cost written with a decimal comma is read 100 times too high; a cost of 0 and negative costs are kept** (low; crates/teslafi/src/costs.rs:130-139 (num), :143-146 (session)). confirmed, fix not done yet
- **TeslaFi import: a state span that ends exactly where a file ends is not extended to the next file's first poll** (low; crates/import/src/teslafi/seg.rs:575-600 (Run::push: 'asleep until seen again; online up to the next poll'), :607-625 (Run::finish), crates/import/src/teslafi/mod.rs:1593-1700 (join of edges)). confirmed, fix not done yet

### Background jobs and storage

- **Nothing can be moved or imported into a new server that has started once (move lines, 'Move here', and the documented `import`)** (high; crates/web/src/resources/imports.rs:1600-1613 (rehome writes the 'strays-v1' done mark on a fresh server, called from crates/sentry-mate/src/main.rs start_web via spawn_rehome); crates/moving/src/impo). confirmed, fix not done yet
- **A plain rebuild over one damaged series day file wipes the car's drives and charges, fails with a bare 500, and leaves no notice or audit entry** (high; crates/engine/src/jobs.rs:212-222 (run_rebuild: mark, clear_window, then reconcile_car); crates/engine/src/pipeline.rs:1122-1136 (the clearing is repeated at every start); crates/web/src/resources/sys). confirmed, fix not done yet
- **A full rebuild (and a plain one) reports done and switches to a poorer history when stored logs cannot be read** (high; crates/engine/src/pipeline.rs:1213-1236 (a log that fails is counted as done by j.step); crates/engine/src/full.rs:640-672 (full_car switches whatever reconcile_car's log errors were); crates/engine/s). confirmed, fix not done yet
- **A damaged drives table cannot be mended by a rebuild or a full rebuild; the Drives page answers 500 for good** (medium; crates/engine/src/full.rs:676-700 (full_seed copies the car's folder with the damaged table, then clear_window reads it); crates/engine/src/pipeline.rs:1252-1264 (clear_window); crates/db/src/shadow.r). confirmed, fix not done yet
- **A full rebuild forgets the logs it had done when the server restarts; progress and the audit summary count only the rest** (medium; crates/engine/src/jobs.rs:91-102 (JobState::step calls e.full_noted()); crates/engine/src/pipeline.rs:1234 (j.step(self, ...) where self is the shadow engine); crates/engine/src/full.rs:533-548 (full_). confirmed, fix not done yet
- **'Check now' whose request goes away mid-check leaves the release check 'checking' for good** (medium; crates/web/src/resources/release.rs:143-181 (Checker::run sets st.running = true and clears it only after the await), :107-111 (follow keeps `running` across off/on), :185-191 (check_now awaited insid). confirmed, fix not done yet
- **A rebuild whose request is cut (timeout, closed tab) runs to the end but is never written to the audit log; one that outlasts 60 s is audited as done while it runs** (medium; crates/web/src/resources/system.rs:209-257 (rebuild handler: waits up to REBUILD_WAIT in the handler, audits afterwards)). confirmed, fix not done yet
- **The rebuild's audit summary gives nonsense day counts ('20778 days', '1 days')** (low; crates/web/src/resources/system.rs:235-243). confirmed, fix not done yet
- **A full rebuild resumed after a restart, and any partial-window rebuild, decode about 8 times slower than a whole one** (low; crates/engine/src/pipeline.rs:1184-1211 (window_batch falls back to 1 when any row exists at or after the first wanted log)). confirmed, fix not done yet
- **MQTT: the retained 'energy at destination' value is never cleared when the route ends** (low; crates/mqtt/src/render.rs:517-523 (Field::EnergyAtDestination returns None when not navigating); crates/mqtt/src/worker.rs changed() skips a None payload). confirmed, fix not done yet
- **A full rebuild started in the instant the previous one's worker is ending gets no worker and stays 'running' in phase queued** (low; crates/engine/src/full.rs:485-513 (full_spawn: worker flag cleared after full_run returns, outside the full_start lock)). confirmed, fix not done yet

### Shared browser modules, dev server and tools

- **Audit log "Download CSV" gets JSON from the real server; the mock answers CSV and hides it** (medium; crates/web/src/accounts_admin.rs:357 (fn audit returns Json only); ui/devserver/mockapi (conventions CSV for every items list); packs: instrument/horizon/manager settings-server.js:562, console settin). confirmed, fix not done yet
- **Dev server sends `now` deltas with half a value pair ({"at_ms": ...} without value and sources)** (medium; ui/devserver/mockapi/events.py:28 (_leaf) and :32 (diff)). confirmed, fix not done yet
- **Car overview loses data-ready for good after an in-page #hash step when the tab remembers a period** (medium; ui/shared/state.js:333-338 (sameRoute) vs :319-327 (stateFromUrl); used by navigate() :363 and startRouting onPop :463; trigger e.g. ui/packs/instrument/web/pages/car.js:393 (href '#vehicle-info')). confirmed, fix not done yet
- **node ui/tools/check.js --self-test fails (2 of 102 checks) on a clean tree** (medium; ui/tools/check.js:1268 and :1274 (hover assertions), ui/tools/hover.js:222-234 (grid points) and the selector text of moved elements). confirmed, fix not done yet
- **Mock /cars/{car}/health lacks chain_gaps_total, dropped_frames_total and unknown_spans_total (and adds from_ms/to_ms the real server does not send)** (medium; ui/devserver/mockapi/resources/system.py (car health); docs/ui-api.md:615; packs counting list length: instrument/health.js:322, horizon/health.js:321-322, manager/health.js:321-322, console/health.js). confirmed, fix not done yet
- **Mock /cars/{car}/events payloads use string ids and another topic prefix than the real server** (low; ui/devserver/mockapi/resources/integrations.py (events) and ui/devserver/mockapi/dataset/extras.py:50 (namespace 'sentrymate'); real: crates/mqtt/src/render.rs:742-775, crates/mqtt/src/settings.rs:64). confirmed, fix not done yet
- **Mock leaves out fields the real server sends on /me, /me/sessions, /accounts, awake-reasons, upload tokens and reports** (low; ui/devserver/mockapi/resources/auth.py, fleet.py, reports.py, drives.py). confirmed, fix not done yet
- **Byte sizes print '1,000 kB', '1,000 B', '10.0 kB' at unit edges** (low; ui/shared/units.js:233-239 (bytes branch of parts)). confirmed, fix not done yet
- **Hour buckets repeat a key on the autumn clock change (and drift off the wall-clock hour in 30-minute-DST zones)** (low; ui/shared/time.js:196-204 (bucketKey), :175-177 (addPeriod hour), :210-222 (buckets)). confirmed, fix not done yet
- **Durations mix numbering systems and invoice month keys are not ASCII in locales with their own digits** (low; ui/shared/time.js:396-398 (word(): pad2 for the second part), ui/shared/invoices.js:92-99 (invoiceMonths key from a locale DateTimeFormat)). confirmed, fix not done yet
- **Recommended tyre pressure reads '42 psi front, 42 psi rear'** (low; ui/shared/tyres.js:56-61 (recommendedValue), also :103 (tyresTitle split)). confirmed, fix not done yet

### The real server end to end

- **A full rebuild cut off by a normal stop (SIGTERM, docker stop) switches every car to a half-made history and reports itself done** (high; crates/engine/src/full.rs full_car (about lines 636-668) and full_run; crates/engine/src/pipeline.rs:519 (log_locked) and :1147 (reconcile_car); crates/sentry-mate/src/site.rs EngineRun::stop). confirmed, fix not done yet
- **A brand-new server refuses both 'Restore from an export file' and 'Move here' with 409 not_empty** (high; crates/web/src/resources/imports.rs about line 1600-1612 (the one-time rehome pass, mark_done 'strays-v1'); crates/moving/src/import.rs:157 WEB_FIRST_START and unused_but (:566-587); page /setup). confirmed, fix not done yet
- **Instrument, Horizon, Manager and Logbook cannot add, edit or remove an account on the real server** (high; ui/packs/instrument/web/pages/settings-server.js:452-460 and :485 (the same lines in horizon and manager); ui/packs/logbook/web/pages/settings-server.js:623-627 and :651; ui/devserver/mockapi (account). confirmed, fix not done yet
- **A new upload token's answer has no `app` block, so the page cannot show the server address or certificate fingerprint** (medium; crates/sentry-mate/src/api.rs created() (about lines 246-265), create_car_token, create_new_car_token; ui/packs/instrument/web/pages/settings-cars.js:71-90). confirmed, fix not done yet
- **Event payloads carry drive and charge ids as JSON numbers; imported rows' ids pass 2^53 and lose digits** (medium; crates/mqtt/src/render.rs:746 and :775 (put "drive"/"charge" as numbers, also "car"); GET /api/v1/cars/{car}/events; MQTT event topic). confirmed, fix not done yet
- **/setup/status never reports a running import** (low; crates/engine/src/query/keys.rs:420 (import_running: false); crates/sentry-mate/src/api.rs setup_status; ui/packs/*/web/pages/fleet.js (the import_running fix text)). confirmed, fix not done yet
- **A car that only has imported history is flagged 'no upload' for good** (low; crates/engine/src/query/fleet.rs:145-152 (attention_of) and crates/engine/src/query/system.rs:1056-1060 (health reasons)). confirmed, fix not done yet
- **Fleet says 'No upload for 4 min' for a car flagged because its data is 38 hours old** (low; ui/packs/instrument/web/pages/fleet.js:34 (same in horizon and manager); rule in crates/engine/src/query/fleet.rs:145). confirmed, fix not done yet
- **Console's Travel page prints 'null' for stops with no town** (medium; ui/packs/console/web/pages/travel.js:122, :131, :152, :162, :170). archived (pack archived)
- **Pebble's Settings: Display shows the word 'null' under the release check** (low; ui/packs/pebble/web/pages/settings-display.js:266-271 (drawRel)). archived (pack archived)
- **Console's drive page labels the fourth tyre line 'undefined rr'** (low; ui/packs/console/web/pages/drive.js:174 (three legend glyphs) and ui/packs/console/web/core/chart.js:44 (DASHES has three entries)). archived (pack archived)
- **Charges for all cars on a server with no car asks for /cars/undefined/... and prints 'undefined'** (low; ui/packs/{instrument,horizon,manager,console,logbook}/web/pages/charges.js (the DC curve block); route /charges?cars=all). confirmed, fix not done yet
- **Import report 'Rows per car' says every TeslaFi poll was imported even when all were skipped** (medium; ui/packs/instrument/web/pages/import.js:841 (same line in horizon and manager); ui/packs/console/web/pages/import.js:535). confirmed, fix not done yet
- **Logbook's ', plugged in' on parked timeline rows never shows on the real server** (low; ui/packs/logbook/web/pages/timeline.js:466; crates/web/src/resources/timeline.rs (row fields); mock timeline rows). confirmed, fix not done yet
- **Six list resources ignore ?format=csv and Accept: text/csv** (low; crates/web/src/accounts_admin.rs (accounts, audit), auth.rs (me/sessions), packs/mod.rs (ui-packs), resources/notices.rs, resources/imports.rs (import-holds)). confirmed, fix not done yet
- **Pebble tells a car with no data 'it should fall asleep soon'** (low; ui/packs/pebble/web/pages/car.js (the 'Why it is awake' block)). archived (pack archived)
- **No pack offers a way to change one's own password** (low; ui/packs/*/web (no file names me/password); API PUT /api/v1/me/password in crates/web/src/auth.rs:41). confirmed, fix not done yet

### Instrument

- **Car overview: every live update destroys the focused control, keyboard focus falls to <body>** (high; ui/packs/instrument/web/pages/car.js:805-814 (live 'now' handler calls paintHead, paintInputs, paintPlace, paintLive every 2 s), :304-308 (mapTools replaced), :195/:257 (head)). confirmed, fix not done yet
- **Account menu opens but keyboard focus never enters it (149 Tab presses away)** (medium; ui/packs/instrument/web/core/account.js:138-149 (toggleAccountMenu), core/frame.js (button.account)). confirmed, fix not done yet
- **Failed API reads are shown as true 'there is none' statements on several pages** (medium; ui/packs/instrument/web/pages/car.js:575, drive.js:70, charge.js:107-108, logbook.js:299-300, fleet.js:237, drives.js (places filter), places.js:176/439, trip.js:84-85). confirmed, fix not done yet
- **Places page sends one GET /places?car= per car (196 extra requests with 200 cars), failures hidden** (medium; ui/packs/instrument/web/pages/places.js:58-71 (drivesByCar), :176). confirmed, fix not done yet
- **Connections overview rows for map tiles, address lookup and release check link to anchors that do not exist** (medium; ui/packs/instrument/web/pages/settings-display.js (only id 'release-check' exists); links come from crates/web/src/resources/overview.rs:579,591,612 and ui/devserver/mockapi/resources/overview.py:345,). confirmed, fix not done yet
- **A long car name without spaces makes car pages scroll sideways on a phone** (medium; ui/packs/instrument/web/app.css (.page-title, .eyebrow), pages/car.js:257 (head), core/page.js:18-31 (pageHead)). confirmed, fix not done yet
- **With no cars: eight menu items do nothing, and /charges asks for car 'undefined'** (medium; ui/packs/instrument/web/core/frame.js (nav link hrefs), pages/charges.js (fast-charging curve panel)). confirmed, fix not done yet
- **Dialogs opened by a button that disables itself first do not return focus on close** (low; ui/packs/instrument/web/core/dialog.js:17,37-40 (back = document.activeElement); callers pages/tesla-account.js:239 (signIn), pages/settings-cars.js:190,468 (Make upload token), settings-integrations.). confirmed, fix not done yet
- **Typing a latitude outside -90..90 in the place editor throws an uncaught error** (low; ui/packs/instrument/web/pages/places.js:778 (paintCircle -> marker.setLngLat), called from the input handler at :916). confirmed, fix not done yet
- **Timeline: filter bar grows 48 px after the page is ready and pushes everything down** (low; ui/packs/instrument/web/pages/timeline.js:153-158 (placeSel filled after ready), toolbar .tl-filters). confirmed, fix not done yet
- **Settings: Server export steps overlap and are cut off on a phone at interface size 110 and 125** (low; ui/packs/instrument/web/pages/settings-server.css (.xp-steps, .xp-step), section 'With an export file'). confirmed, fix not done yet
- **Travel on a phone at interface size 80: the map legend hangs out of the map over the help text** (low; ui/packs/instrument/web/pages/travel.css (.tv-map-panel map height, .tv-legend)). confirmed, fix not done yet
- **Empty state of a wide table is centred in the full table width and cut off at the edge** (low; ui/packs/instrument/web/core/table.js (.tbl-empty inside .tbl-scroll), seen on /statistics 'Per month'). confirmed, fix not done yet
- **Interface size 80 on a desktop gives unit text of 8.6 to 9.6 px** (low; ui/packs/instrument/web/app.css (.q-u unit size, .sub), seen on fleet, charges, software, health). confirmed, fix not done yet
- **/software is 'not found' although the page is registered as working without a car; the not-found screen has no heading** (low; ui/packs/instrument/web/core/pages.js:107 (software: car 'optional'), ui/shared/state.js:36 (only /cars/:car/software), core/router.js:232-237 and core/states.js (error panel)). confirmed, fix not done yet
- **Dev server: Software's 'Versions of every car' lists a car c4 that does not exist, so its link 404s** (low; ui/devserver/mockapi (GET /api/v1/fleet/versions); shown by ui/packs/instrument/web/pages/software.js:292). confirmed, fix not done yet

### Horizon

- **/charging shows a blank page forever (uncaught RangeError: unknown page: null)** (medium; ui/packs/horizon/web/core/router.js:221-223 (section short-address regex), core/frame.js sectionPage(), core/pages.js GROUPS; ui/packs/horizon/web/core/README.md:17). archived (pack archived)
- **Sign-in and set-up pages show the whole app header on a computer (sections, car switcher, account menu saying 'Signed in', 'Not live' warning)** (medium; ui/packs/horizon/web/app.css:1169 and :2071; route /sign-in and /setup at desktop widths). archived (pack archived)
- **Interface size 125 % on this device: the pack's own checker fails all 25 routes (sideways scroll on Charges, clipped filter counts on Logbook, clipped route legend, car button icon off centre on every page)** (medium; ui/packs/horizon/web/app.css (.car-switch.top-car, .lb-seg/.filter-bar, .route-legend, #habits .panel-side), pages/charges.css, pages/logbook.css, pages/drive.css, pages/trip.css). archived (pack archived)
- **Interface size 80 % on this device: 12 routes fail the checker (text under 10 px, labels cut off on Server and access and Data health on a phone)** (low; ui/packs/horizon/web/app.css (.q-u unit size, .setting-note .mono, .hl-code, .fr-line dt, .route-pin, .socmap-y, .su-crumb-n, .sc-src-rank)). archived (pack archived)
- **Default size: logbook, charge, places and settings-integrations fail the pixel checker** (low; ui/packs/horizon/web/pages/logbook.css (.lb-total-head), pages/charge.css (.charge-soc .r b), pages/places.css (.pl-sticky .pl-actions a.btn), pages/settings-integrations.css / conn-overview (.ov-err)). archived (pack archived)
- **Places list asks the server once per car (200 cars: 200 parallel /places?car= requests on every load)** (medium; ui/packs/horizon/web/pages/places.js:58-70 (drivesByCar) and :176). archived (pack archived)
- **Typing an out-of-range latitude in the place editor throws an uncaught error from the map** (low; ui/packs/horizon/web/pages/places.js (latitude/longitude input handlers that move the map circle); route /places?new=1). archived (pack archived)
- **A malformed #fragment in the address throws an uncaught URIError after every page mount** (low; ui/packs/horizon/web/core/router.js:355 (decodeURIComponent(location.hash.slice(1)))). archived (pack archived)
- **Ctrl+K opens the car switcher on top of an open 'Find a page' dialog (two modal dialogs stacked); focus is lost after closing them** (low; ui/packs/horizon/web/core/frame.js onKey() (the Ctrl/Cmd+K branch), core/finder.js, core/carswitch.js). archived (pack archived)
- **Keyboard focus drops to the page body after 'New place' and after saving a logbook purpose or a place** (low; ui/packs/horizon/web/pages/places.js (onQuery rebuild for ?new=1 / ?place=), pages/logbook.js (list redraw after the purpose dialog saves), core/router.js onState (onQuery path sets no focus)). archived (pack archived)
- **Signing in drops the #section of the page that was asked for** (low; ui/packs/horizon/web/core/router.js:213 (session.gate(id, s.path + location.search)) and core/app.js sessionEnded()). archived (pack archived)
- **With 200 cars: Travel's map legend overlaps the map credit and the help text on a phone; the empty Statistics text sticks 87 px out of its box** (low; ui/packs/horizon/web/pages/travel.css (.tv-legend, .tv-map-help), core/table.js / app.css (.tbl-empty .empty p); routes /travel and /cars/{car}/statistics). archived (pack archived)
- **Light theme: the 'Preferred' tag and the 'DC' pill are under 4.5:1 contrast** (low; ui/packs/horizon/web/app.css / theme.css (.tag--live, .kind-pill--dc in :root[data-theme="light"]); routes /cars/c1/health and /cars/c1/charges). archived (pack archived)
- **The pack's page guide describes six sections with a Charging section and the wrong storage key** (low; ui/packs/horizon/web/core/README.md:3-24). archived (pack archived)

### Manager

- **Short address /charging shows a blank page (uncaught RangeError), or All cars when no car was opened** (medium; ui/packs/manager/web/core/router.js:221-223; ui/packs/manager/web/core/pages.js GROUPS; ui/packs/manager/web/core/README.md:16). archived (pack archived)
- **A 64-character car name without spaces (allowed) makes almost every car page scroll sideways; a long place name pushes the Drives filter off a 1440 px window** (medium; ui/packs/manager/web/app.css (page head h1, .eyebrow, .charge-trail, filter selects); pages car, timeline, drives, drive, trip, logbook, charges, charge, battery, efficiency, statistics, travel, softw). archived (pack archived)
- **Failed requests are shown as true-looking empty states ('No route was recorded', 'No charging in this period', 'No rules yet', 'All 0 cars')** (medium; pages/drive.js:70,243; pages/car.js:845; pages/drives.js:126; pages/trip.js:122,157; pages/logbook.js:273; pages/charges.js:166; pages/battery.js:198; pages/places.js:369; pages/statistics.js, pages/c). archived (pack archived)
- **Charge page on a phone: the 'Download CSV' button covers the 'Power against battery level' heading** (low; ui/packs/manager/web/pages/charge.css:33 (.charge-page .charts-panel .panel-head .row), app.css:1460-1461; page /cars/{car}/charges/{charge} at 390 px). archived (pack archived)
- **Interface size on this device 80 % and 125 %: clipped, overlapping and sub-10 px text, and sideways scroll on a phone** (medium; ui/packs/manager/web/app.css and pages/{drives,drive,logbook,charges,travel,health,efficiency,trip,car,software}.css). archived (pack archived)
- **Settings: Cars number fields: unparseable input is saved as 'empty', erasing the stored value and saying 'saved'** (medium; ui/packs/manager/web/pages/settings-cars.js:580-586 (battery reference values) and 614-619 (tyre pressures); core/form.js field() read()). archived (pack archived)
- **All-cars Charges with no cars asks for /cars/undefined/charging-stats/dc-curve (404) and shows a load error in the curve panel** (low; ui/packs/manager/web/pages/charges.js:783-806). archived (pack archived)
- **/charges?cars=<unknown id> shows the raw id as heading, 'No charge matches' tiles and a Not found table, with nine 404 requests** (low; ui/packs/manager/web/pages/charges.js (cars= handling); compare pages/travel.js). archived (pack archived)
- **Places page sends one paged /places?car= request per car (200 cars: 200+ requests on every open)** (medium; ui/packs/manager/web/pages/places.js:58-71 (drivesByCar)). archived (pack archived)
- **Place editor: typing a latitude outside -90..90 throws an uncaught MapLibre error** (low; ui/packs/manager/web/pages/places.js:773-780 (paintCircle)). archived (pack archived)
- **Saving a place without touching its price still asks 'Update past charge costs too? ... would cost differently with this price'** (low; ui/packs/manager/web/pages/places.js:967-971). archived (pack archived)
- **A deep link with a section anchor loses the anchor when sign-in is needed** (low; ui/packs/manager/web/core/router.js:206 (session.gate(id, s.path + location.search)); core/session.js gate()). archived (pack archived)
- **With no cars, the Overview, Timeline, Software, Drives, Trip, Logbook, Battery and Efficiency tabs all link to '/' and silently show All cars** (low; ui/packs/manager/web/core/frame.js (tab links via state.linkTo); ui/shared/state.js:400). confirmed, fix not done yet
- **Page finder: Esc with text typed only clears the text; the hint says 'Esc close'** (low; ui/packs/manager/web/core/finder.js (search input in the menu panel dialog)). archived (pack archived)
- **After a failed save in a dialog (and after each set-up step) keyboard focus drops to the page body** (low; ui/packs/manager/web/core/form.js busy() (disables the focused submit button); pages/settings-server.js account dialogs; pages/setup.js step changes). archived (pack archived)
- **Logbook: the text of a purpose total with no drives has 2.5:1 contrast (light) and 2.9:1 (dark)** (low; ui/packs/manager/web/pages/logbook.css (.lb-total.is-zero, opacity 0.6)). archived (pack archived)

### Console

- **Battery: the "by month" table never shows a row (ReferenceError: warranty is not defined)** (high; ui/packs/console/web/pages/battery.js:354 (paintMonths), variable declared at line 146 in another function; route /cars/{car}/battery). archived (pack archived)
- **Places: saving an existing place leaves the form dead (no "Saved" note, save button stays disabled, old heading)** (high; ui/packs/console/web/pages/places.js:361-362 (save) with ui/shared/state.js navigate; route /places?place={id}). confirmed, fix not done yet
- **Places: deleting from the list leaves the row in the list and shows no confirmation** (medium; ui/packs/console/web/pages/places.js:99 (removePlace); route /places). archived (pack archived)
- **Travel: the [refresh] button throws and does nothing** (medium; ui/packs/console/web/pages/travel.js:102; scoped client built in ui/packs/console/web/core/router.js counted()). archived (pack archived)
- **Settings: car: a failed "mark as sold" / "not sold" / sold-date change shows no error** (medium; ui/packs/console/web/pages/settings-cars.js:396-400 (paintSold save)). archived (pack archived)
- **Error block says "the page tries again when it is back" but the page never retries** (medium; ui/packs/console/web/core/states.js:22 (text), core/router.js showError, core/frame.js showLive). archived (pack archived)
- **Interface size 125 % (and 110 % at 1280 px): every page scrolls sideways and the link row's settings/help are cut off** (medium; ui/packs/console/web/app.css:214-217 and the px breakpoints at 1109-1114; all routes). archived (pack archived)
- **A 64-character car name (allowed by the form) breaks the frame on desktop and phone** (medium; ui/packs/console/web/core/frame.js (link row label, car button), core/ui.js head(); app.css .lk-label, .cmd-btn, .ph). archived (pack archived)
- **In-page anchor links land under the fixed command bar** (medium; ui/packs/console/web/core/router.js (only a full mount offsets for the bar), app.css (no scroll-margin/scroll-padding); footer links, command-list settings keys, software [name it] / "name it below", ). archived (pack archived)
- **Footer "data credits" link points to an anchor that does not exist** (low; ui/packs/console/web/core/frame.js:122 (/help#credits) and pages/help.js:128 (section without id)). archived (pack archived)
- **Command list key "server.certificate" lands on nothing** (low; ui/packs/console/web/core/palette.js:39 and pages/settings-server.js:127, 786). archived (pack archived)
- **Charges with no car asks for cars/undefined and shows "There is no such car"** (low; ui/packs/console/web/pages/charges.js:515, 526; route /charges on an empty fleet). archived (pack archived)
- **Trip (and part of Efficiency) fails with "Some values are not valid" for a period that starts in the future or before 1970** (low; ui/packs/console/web/pages/trip.js:41 (to = min(period.to_ms, now)), pages/efficiency.js series call, core/period.js date inputs (no min/max)). archived (pack archived)
- **Settings: car: after typing a new time zone the line still describes the old zone and the header clock keeps the old zone** (low; ui/packs/console/web/pages/settings-cars.js:513 (save calls only paintTop), core/frame.js clockTz). archived (pack archived)
- **Settings: car: after a rename the tab title and the command bar car button keep the old name; the tab title names the car twice** (low; ui/packs/console/web/pages/settings-cars.js:31 (title) and patchCar; core/router.js docTitle; core/frame.js names map). archived (pack archived)
- **Logbook: a purpose that could not be saved stays selected in the row** (low; ui/packs/console/web/pages/logbook.js:61-68 (purposeField change handler)). archived (pack archived)
- **Filter line shows values it ignores as active, and reversed from/to gives a silently wrong range** (low; ui/packs/console/web/pages/drives.js:52-77 (toParams, onApply), pages/charges.js filter, core/filterline.js show()). archived (pack archived)
- **A long unbroken word in a filter line makes the page 1000 px wider than the window** (low; ui/packs/console/web/core/filterline.js show() / app.css .filt-on .tag; Drives and Charges). archived (pack archived)
- **Command bar clock shows the browser's clock, not the server's, for up to a minute on pages without a car** (low; ui/packs/console/web/core/frame.js:150-166 (paintClock, startClock)). archived (pack archived)
- **Places form: typing a latitude outside -90..90 throws an uncaught MapLibre error on every keystroke** (low; ui/packs/console/web/pages/places.js:336 (changed -> centre.setLngLat)). archived (pack archived)
- **Settings: car: the key efficiency_wh_per_km_override breaks in the middle of the word** (low; ui/packs/console/web/pages/settings-cars.js:567 and app.css .ke-k; /settings/cars/{car} reference values at 1440 px). archived (pack archived)
- **Interface size 80 and 125: checker failures besides the sideways scroll (fractional borders, text under 10 px, focus scroll)** (low; Logbook purpose select (.pur-cell select.in--cell), Travel tables, Import phone rows, Drives/Logbook sort select on phone; settings-cars sold date input on phone). archived (pack archived)
- **Logbook: rows of the trip log appear and vanish again right after the page is ready** (low; ui/packs/console/web/core/table.js windowTable / shared/vlist.js; /cars/{car}/logbook). archived (pack archived)
- **Private LAN addresses used as examples in page text (rule: use 192.0.2.x)** (low; ui/packs/console/web/pages/settings-server.js:134; pages/setup.js:215-216; ui/devserver/README.md (192.168.1.30 for the made-up new server)). confirmed, fix not done yet
- **Dev server: /fleet/versions lists a car that does not exist (and omits the sold car), so Software shows a dead link** (low; ui/devserver/mockapi/resources/software.py fleet_versions (line 180 on); seen on /cars/c1/software "every car" table). confirmed, fix not done yet
- **Dev server: GET /places?car=<synthetic car>&sort=drives answers 500** (low; ui/devserver/mockapi/resources/places.py:90 (_car_counts) -> mockapi/derive.py:37 (started)). confirmed, fix not done yet

### Pebble

- **The word "null" is drawn on six Pebble pages and sheets** (medium; ui/packs/pebble/web/pages/settings-display.js:266 (release check rows), pages/settings-cars.js:468 (Free Supercharging, watch account), pages/fleet.js:191 (Order sheet), core/settings.js:79-80 (pick s). archived (pack archived)
- **Interface size 125 % on a phone: dock and account button are cut by the screen edge and every page scrolls sideways** (medium; ui/packs/pebble/web/core/frame.js placeDock() and app.css (.dock, .head-bar); all routes). archived (pack archived)
- **Charges on a phone at the default size: "Per 100 km" and "DC per kWh" figures are cut off and the page scrolls sideways** (medium; ui/packs/pebble/web/pages/charges.js:300-301 (moneyTile), core/bits.js:302-307 money(), app.css:987 (.money); routes /cars/{car}/charges and /charges). archived (pack archived)
- **More sideways overflow at phone widths: Integrations Set up, Charge, Statistics** (low; ui/packs/pebble/web/pages/settings-integrations.js (.iv-top chip, Set up tab), pages/charge.js:76-78 (context bar date pill), pages/statistics.js (bucket segmented control in .page-bar)). archived (pack archived)
- **Segmented controls are cut off inside sheets on a phone (Charges Filters, Place editor price type)** (medium; ui/packs/pebble/web/pages/places.js:111 (billing segmented: No price / Per kWh / Per minute / Tiered (per kWh steps)), pages/charges.js Filters sheet; core/form.js segmented(), app.css .seg). archived (pack archived)
- **On a first visit the dock's Car, Drives and Battery and five More-sheet entries do nothing** (medium; ui/shared/state.js linkTo() ("with no car known it links to Fleet"), ui/packs/pebble/web/core/frame.js drawDock() and buildMoreSheet()). confirmed, fix not done yet
- **Links to page sections land at the top: #move, #habits, #release-check, #maps, #addresses, #overlaps-{car} do not exist in Pebble** (medium; ui/packs/pebble/web/pages/settings-server.js:442 (move tile has no id), pages/statistics.js:106 (links to charges#habits; pages/charges.js has no such id), pages/settings-server.js:176 with ui/shared/). archived (pack archived)
- **Tapping a page while the server is unreachable replaces the app with the browser's error page** (medium; ui/shared/build.js loadPage() lines 118-140 (copied to ui/packs/pebble/web/shared/build.js), used by ui/packs/pebble/web/core/router.js:185). confirmed, fix not done yet
- **Long car and account names push pages wider than the phone screen** (low; ui/packs/pebble/web/pages/settings-cars.js ("Tie to {car}" button in Keys waiting for a car), pages/travel.js (car filter pills and row sides), pages/settings-server.js accountsTile() (line 714-720)). archived (pack archived)
- **Place editor: name allows 80 characters but the server takes 64, and server field errors are shown as one unlabelled line** (low; ui/packs/pebble/web/pages/places.js:105 (maxlength: 80), :303-306 (save catch joins e.fields values), :296 (preview shows only e.message); server rule crates/engine/src/query/places.rs:997). archived (pack archived)
- **Typing a latitude outside -90..90 in the place editor throws uncaught errors** (low; ui/packs/pebble/web/pages/places.js drawSheetMap() (around lines 243-262)). archived (pack archived)
- **With miles and Fahrenheit chosen, three texts stay metric** (low; ui/packs/pebble/web/pages/trip.js:81, pages/charges.js:351, pages/places.js:107 and :278). archived (pack archived)
- **Logbook: "1 drives" in the purpose tiles, and keyboard focus is lost after saving a purpose** (low; ui/packs/pebble/web/pages/logbook.js:137 (totalsTiles) and :172-196 (edit / refresh)). archived (pack archived)
- **Logbook fails the stability check: rows flicker on every live rows event** (low; ui/packs/pebble/web/pages/logbook.js:198-207 (refresh on rows events), ui/shared/vlist.js). confirmed, fix not done yet
- **Battery page of a car without capacity data shows a broken sentence** (low; ui/packs/pebble/web/pages/battery.js:92). archived (pack archived)
- **Warning-tone text is below 4.5:1 in the light theme; "unnamed" firmware labels are below it in both themes** (medium; ui/packs/pebble/web/app.css (--sun as text colour: .chip.tone-sun, .check.is-warn .check-word, .tile-line.is-warn, .is-low, .is-paused; --sky on .chip.tone-sky), pages/software.css (.fwh-seg.is-unname). archived (pack archived)
- **Car switcher says "200 cars" where the header says "199 cars"** (low; ui/packs/pebble/web/core/switcher.js draw() (count.textContent, about line 84)). archived (pack archived)
- **Picking a date before 1970 in the period sheet replaces the page with an error** (low; ui/packs/pebble/web/core/period.js build() (date inputs without min/max, submit handler)). archived (pack archived)
- **Signing in loses the #section of the address asked for** (low; ui/packs/pebble/web/core/start.js:19 (here = pathname + search) and :33, :51). archived (pack archived)

### Logbook

- **Footer says "Release check off" on every page while the release check is on** (medium; ui/packs/logbook/web/core/frame.js drawFooter (line ~263): `release ? [...] : 'Release check off'`). archived (pack archived)
- **Interface size 125 % (and 110 %) on a phone: pages scroll sideways, controls are cut, axis labels overlap** (high; ui/packs/logbook/web/app.css:400-401 (.block-head / .block-actions never wrap), pages/drive.css (ul.climate-key), pages/drives.css (.cell-facts), pages/charge.css (.fig-trio .trio), pages/settings-dis). archived (pack archived)
- **Pointing at a chart makes the page grow and everything below jump (Drive +43 px, Statistics -20 px, Trip, Charge, Battery, Efficiency)** (medium; ui/packs/logbook/web/pages/drive.js:341 and pages/drive.css:70-73 (.readings / .reading dd, the Tyres reading); the readouts of pages/trip.js, charge.js, battery.js, efficiency.js, statistics.js). archived (pack archived)
- **A long car name (allowed up to 64 characters) makes pages wider than the window, on desktop too** (medium; ui/packs/logbook/web/app.css:384 (.page-title, crumbs, eyebrow have no overflow-wrap), pages/statistics.js and travel.js tables (car column), pages/charges.js (.ch-pick-wrap car picker), pages/logbook). archived (pack archived)
- **A long place name without spaces overflows Timeline and the place editor on a phone** (low; ui/packs/logbook/web/pages/timeline.css (a.tl-place.tl-place-link), pages/places.js / places.css (the editor's name spans)). archived (pack archived)
- **/charges (all cars) is 568 px wide on a 390 px phone** (medium; ui/packs/logbook/web/pages/charges.js (missing-data block head with CSV and "Rebuild these charges from the stored logs"; curve block head with the car select and CSV), app.css:400-401). archived (pack archived)
- **Page style sheets are unscoped and stay loaded, so a page's layout depends on which pages were visited before** (medium; ui/packs/logbook/web/pages/drives.css:139 (`.block-actions { ... flex-wrap: wrap; }`) and about 1200 other selectors without a .page-<id> prefix in car.css, charge.css, drive.css, drives.css, fleet.cs). archived (pack archived)
- **Informational text in --ink-4 fails WCAG AA contrast in both themes (2.6:1 light, 3.1:1 dark)** (medium; ui/packs/logbook/web/app.css:26, 68, 83 (--ink-4: #a0978a light, #686357 dark); used at app.css:362 (.colophon-update.is-empty), pages/drives.css:67 (.fact-name), pages/logbook.css:61 (.lb-id), pages/). archived (pack archived)
- **/charges with no car requests /api/v1/cars/undefined/charging-stats/dc-curve (404) and titles itself "All 0 cars"** (low; ui/packs/logbook/web/pages/charges.js:830-837 (curveCar falls back to viewCars[0], undefined when there is no car)). archived (pack archived)
- **Logbook throws an unhandled promise rejection when its first list request fails** (low; ui/packs/logbook/web/pages/logbook.js:474-481 with core/table.js windowLedger `ready`). archived (pack archived)
- **A failed Map / Satellite switch says nothing** (low; ui/packs/logbook/web/shared/maptiles.js:216-245 (mapStyleSwitch / save of me/preferences map_style), used by core/map.js:165). archived (pack archived)
- **Duplicate element ids on Settings: Display and units (c-theme-label, c-interface_size-label)** (low; ui/packs/logbook/web/core/form.js:119 (choice ids come from `name`), core/frame.js displayMenu (names 'theme', 'interface_size'), pages/settings-display.js (same names)). archived (pack archived)
- **In-page "Choose another car" opens the car switcher but Escape puts the focus on the masthead button, not back on the button used** (low; ui/packs/logbook/web/core/switcher.js (close refocuses the masthead trigger); openers on pages/logbook.js, software.js, health.js). archived (pack archived)
- **Settings: Cars saves a made-up currency ("dollars" becomes DOL) and a sold date in the future** (low; ui/packs/logbook/web/pages/settings-cars.js (default price form: currency cut to three letters; sold form: no date check)). archived (pack archived)
- **"Apply" on an import lot and "Jump to date" do nothing when nothing is chosen** (low; ui/packs/logbook/web/pages/import.js:1232-1234 (giveCar returns when no car is picked), core/table.js:364-365 (jump only focuses the date)). archived (pack archived)
- **Pixel check fails at the default size on fleet, charge, battery, travel and settings-integrations** (low; ui/packs/logbook/web/pages/fleet.css (page-head-aside), charge.css (.map-charge pin label), battery.css (.bt-cap-x ticks, .bt-split-key), travel.css (.tv-period), settings-integrations.css (.co-p rows). archived (pack archived)
- **Dev server: GET /api/v1/places?car=c4 answers 500 for a light synthetic car (--cars 200)** (low; ui/devserver mock API, places list with car= for cars above the three featured ones; seen from ui/packs/logbook/web/pages/timeline.js and drives.js). confirmed, fix not done yet

## Fixed

Each with a test unless the line says otherwise; the commits name them.

### Engine math and data correctness

- Grid energy of a three-phase AC charge is a third of the real figure, and the cost is priced on it (high). Test: crates/timeline/tests/energy.rs grid_energy_counts_every_phase, an_apis_phase_count_comes_with_its_current; crates/tesla/src/convert.rs an_ac_charge_says_how_many_phases_its_current_is_on; tmlocal map test (charger_phases 2 -> 3). Grid kW = V x A x phases: phases from the line's own power (ChargeLin
- A Supercharger visit read by both the Tesla API and TeslaFi costs double (high). Test: crates/engine/tests/invoices.rs one_visit_read_by_the_tesla_api_and_teslafi_costs_once; crates/engine/src/invoices.rs one_visit_read_by_two_inputs_counts_once. Both sessions stay matched to the charge; TeslaFi's has applied false. docs/ui-api.md updated.
- A software update present in a TeslaMate import and a TeslaFi import is listed and counted twice (medium). Test: crates/engine/tests/import_overlaps.rs an_update_both_imports_have_is_one_update (updates list count/gaps/items/last_update, /totals for all, import, import:teslamate, import:teslafi, smo; and the same-version-within-a-day rule outside pair spans). Also applied to the timeline, activity, events and 
- At the gentle pace (one reading every 2 minutes) charge and drive energy are about 26 % too low (medium). Test: crates/timeline/tests/energy.rs a_charge_seen_by_the_api_alone_has_its_energy_at_every_pace, a_drive_seen_by_the_api_alone_has_its_energy_at_every_pace (15/30/60/120 s). api_max_hold_ms default 90 s -> 150 s (as api_park_ms). charge_energy_added is still not used as a counter (a gap over 150 s insid
- A charge whose curve has no usable stretch is settled at 0.00 although its energy is known, and that cost stays (medium). Test: crates/places/tests/price.rs a_curve_with_no_energy_under_it_prices_the_known_grid_energy (also -0 gone); crates/engine/tests/costs.rs a_charge_whose_curve_has_no_usable_stretch_costs_its_energy_at_the_price (test file is in commit 36ac2d64). Charges already kept at 0.00 this way stay until a repric
- An imported drive that starts just before live coverage begins is counted together with the live drive of the same trip (medium). Test: crates/engine/tests/import_overlaps.rs a_drive_going_on_when_live_logging_began_counts_once, a_charge_going_on_when_live_logging_began_counts_once, an_imported_drive_that_ran_into_live_logging_with_no_live_drive_stays. The time the imported row covers becomes its own 'live' overlap (listed in /overl
- A cost typed per kWh (or per minute) is frozen at the energy the charge had when it was typed (medium). Test: crates/engine/tests/costs.rs a_rate_typed_per_kwh_follows_the_charge, a_rate_typed_per_minute_follows_the_charge_and_a_total_stays (recomputed where read and where the pipeline settles)
- A per-kWh cost typed on a charge without a known energy is saved as 0.00 (low). Test: crates/engine/tests/costs.rs a_rate_per_kwh_on_a_charge_without_energy_is_refused (QueryError::Invalid on mode -> 422); ui/devserver/tests/test_charges.py test_a_rate_per_kwh_needs_an_energy (mock has the same rule). The packs' cost forms already show a field error on mode (field path 'mode'); no pa
- A charge settled early (logs arriving late or out of order) keeps the cost of its first part for good (low). Test: crates/engine/tests/costs.rs a_charge_that_grew_after_it_was_settled_is_priced_whole (calls settle directly, as the hunter did; not driven through the pipeline end to end). A tariff cost now keeps the end and energy it was worked out from (CostEntry.of) and is priced again, at the price as it is the
- Energy sums stop a reading's hold at the next sample even when that sample has no value (low). Test: crates/timeline/tests/energy.rs a_reading_is_held_until_the_next_second_that_has_the_value, an_apis_power_beside_smo_seconds_without_it_is_held_as_the_apis; crates/engine/tests/tesla_input.rs an_apis_power_beside_smo_seconds_without_it_counts_from_poll_to_poll. Hold runs to the next sample that has 
- A drive whose odometer is first read part-way in gets only the distance from that reading on (low). Test: crates/timeline/tests/energy.rs a_drive_whose_odometer_comes_late_or_stops_early_has_its_whole_distance (late start, early end, no GPS fallback)
- Battery page lifetime AC energy includes charges of unknown kind, unlike /totals (low). Test: crates/engine/tests/battery_page.rs lifetime_energy_by_kind_is_the_totals_and_unknown_kinds_are_not_ac; ui/tools/battery-figures.test.js (all six packs); ui/devserver/tests/test_battery.py. Contract change: lifetime.unknown_kwh added, charged_kwh = ac + dc + unknown (docs/ui-api.md, mock, six packs)
- Battery health can read over 100 % with a negative degradation (low). Test: crates/engine/tests/battery_page.rs a_pack_is_never_healthier_than_new; ui/devserver/tests/test_battery.py test_a_pack_is_never_healthier_than_new; ui/tools/battery-figures.test.js (all six packs show the new note reading_above_new). degradation_pct stops at 0, health_pct at 100, difference_kwh stay

### API and security

- An uploaded UI pack whose name gives the id "chosen" can never be removed (DELETE answers 405) (medium). Test: crates/web/tests/ui_pack_uploads.rs: no_pack_is_named_chosen_and_one_that_was_can_be_removed (and the new DELETE route's case in tests/audit_log.rs)
- An admin whose password has a non-ASCII character can never upload a UI pack (medium). Test: crates/web/tests/confirm_header.rs: an_admin_whose_password_is_not_ascii_can_upload_a_pack; accounts_admin::tests::the_password_header_is_utf8_or_percent_encoded; ui/devserver/tests/test_tesla.py test_2b_a_password_that_is_not_ascii; ui/tools/confirm-password.test.js (failed on the five page lines b
- The audit log, accounts and other lists ignore ?format=csv and Accept: text/csv, and accept format=xml (low). Test: crates/web/tests/lists_csv.rs: the_lists_of_accounts_sessions_packs_notices_and_the_audit_log_answer_csv; added assertions in tests/tesla.rs, teslafi.rs, moving.rs, imports.rs, imports_teslafi.rs
- GET /integrations/invoices?after=18446744073709551615 drops the connection (overflow panic); its cursor is a plain unsigned number (low). Test: crates/web/tests/invoices.rs (new assertions in the existing list test: next/after round trip and three made-up cursors)
- Trip names and import file names accept NUL, CR/LF and bidi control characters (low). Test: crates/web/tests/drives.rs purposes_rules_logbook_and_saved_trips (slow test, run with --include-ignored: passes) and crates/web/tests/imports.rs upload_check_match_import_and_discard
- The documented sign-in throttle (5 per name or 30 per address in 60 s) is not what the server does (low). Test: ui/devserver/tests/test_conventions.py Throttle (three tests), tests/test_review.py SignInSpray
- 26 error codes the server answers are missing from the contract's list of error codes (low). Test: crates/web/tests/error_codes.rs: every_error_code_the_server_answers_is_in_the_contract (walks ApiError::new/conflict/not_found in crates/web/src and sentry-mate/src/api.rs, checks code and status against the doc line)
- A Content-Length that is not a number gets an empty 400, not the documented error shape (low). Test: docs only (hyper answers before the router; no handler test is possible)
- A pack zip without ui.json is refused with a misleading reason (low). Test: crates/web/tests/ui_pack_uploads.rs: a_zip_without_ui_json_is_refused_for_that

## Reported and refuted

What a hunter reported and the second pass could not stand up.

### API and security

- A watch account reads CAN Bridge key ids, token counts, the proxy and the certificate's host names although those reads are admin-only elsewhere: Reproduced the reads as a c1-only watch account (keys[] in /cars/c1/health, tokens.count 19 in /setup/status, full /settings incl. outbound.proxy, host_names, tiles.online_url), but each is documented and intended, not a defect: docs/ui-api.md:804 says for a watch account `tokens` counts its cars' tokens only; the /cars/{car}/health row (line 256, no admin mark, pages: Car overview, Fleet) and lin

### Shared browser modules, dev server and tools

- kindOf() calls every *_per_100km field money: Not a present defect. kindOf('consumption_kwh_per_100km') does return 'money_per_distance', but no such field exists: docs/ui-api.md has only cost_per_100km and cost_total_per_100km, and units.test.js:142 pins cost_per_100km as money_per_distance. No pack calls units.kindOf or formatField at all (the kindOf hits in import.js and pebble health.js are local functions), so the duration fallback for d
- Mock never sends ETag / 304 and marks every answer no-store: The observation is right (mock /api/v1/cars: Cache-Control: no-store, no ETag; real: private, no-cache with a weak ETag, and 304 on If-None-Match), but it is the mock's documented behaviour, not a bug: ui/devserver/README.md:78 says /api/ answers carry Cache-Control: no-store. No page can tell the difference: the browser handles the 304 itself and api.js never sees one (its comment at :263 says so

### Pebble

- UI hints use a private LAN address (192.168.1.20) as the example: The strings are there (settings-server.js:110, setup.js:287), but this is not a defect. 192.168.1.20 is a generic example of what a home-network address looks like, not anyone's real address, in a hint where the person must type their own home-network address; 192.0.2.20 would be a misleading example there. The project uses the same example on purpose throughout: the server's own error text and CL

## Checked and found working

### Engine math and data correctness

- Statistics day/week/month/year/all in Pacific/Auckland, Asia/Kolkata, Australia/Adelaide, America/St_Johns, Pacific/Kiritimati, America/Los_Angeles, Asia/Kathmandu, UTC, Pacific/Chatham: 1210 seeded drives over a DST year; every period starts at a local midnight (Monday / 1st / Jan 1), periods touch, each drive is in the period of its local start, labels (incl. ISO week) match, sums equal the drive count and km (hunt/tests/eng.rs statistics_periods_in_far_and_half_hour_zones passes)
- Mileage months add up to the odometer's whole distance in all nine zones; /totals drive count matches (same test)
- Statistics paging over two cars in different zones with limit 1, 3, 7: the pages together equal the whole list for day, week, month (eng3.rs statistics_pages_over_two_cars_are_the_whole_list passes)
- A month's /statistics row, the charges list totals, /charging-stats and the drives summary agree on counts, km, kWh, cost per currency, cost per 100 km and price per kWh (eng3.rs a_months_reports_agree passes)
- Empty car and one-row car: statistics (all kinds), totals, mileage, gaps, compare (4 metrics), battery, drives all answer without error (eng.rs empty_and_one_row_cars passes)
- Odd periods (0..1, u64::MAX, from == to, far future, open ends) on three cars (live+import, import only, empty) across statistics, mileage, gaps, compare, drives (6 sorts), drive_stats, charges (6 sorts), charging_stats, charging_places, dc_curve, capacity, battery_level, projected_range, efficiency, temperatures, parked, updates, trip, states, activity, timeline gaps, awake reasons, tracks, battery, totals: no panic, no 5xx, no NaN (smoke2.rs passes)
- Time-of-use pricing across midnight and both DST nights in Europe/Berlin: the night rate gets 7 real hours in spring and 9 in autumn, total kWh kept (pure.rs price_tou_across_midnight_and_dst passes)
- A rate past midnight belongs to the day it starts (Sunday-night rate reaches Monday 03:00, not Sunday 03:00) (pure.rs passes)
- Tiered prices: a charge crossing two steps with a household base, and billing cycle edges in America/St_Johns with cycle day 28 (pure.rs price_tiers_cross_steps passes)
- Tiered prices at a shared place follow on across two cars' charges in one cycle: 6.00, 10.00, 18.00 (eng.rs tiers_follow_on_across_two_cars passes)
- A curve with one 6-minute hole and known grid energy is scaled to the grid energy (3.00 for 10 kWh at 0.30) (pure.rs passes)
- Free Supercharging day edges follow the car's local date (America/Los_Angeles: 23:30 the day before no, 00:30 and 23:30 on the day yes, 00:30 after no) (eng2.rs free_supercharging_days_are_local passes)
- Tesla API-only charges and drives at 15 s and 30 s cadence (and drives at 60 s): energy and distance within 1 % of expected (pure.rs)
- Read without finding a defect: engine/tz.rs period_of/start_of, query/odometer.rs clean series, query/reports.rs slots and totals, places/price.rs validation and tier loop termination, timeline/fuse.rs layering and gps_cross_check, timeline/gapfill.rs, query/system.rs Shown

### API and security

- Inventory: every route of crates/web (156 route lines) plus crates/sentry-mate/src/api.rs written to (hunt scratch)/api-security/inventory.md; all attacks ran against the real binary (dev-seed build, 2 seeded cars, TLS) with scripts in that folder (smc.py client, t*.py, fuzz*.py)
- Role and car matrix: all 170 GET routes as admin, watch (c1 only), watch with no cars and signed out (matrix.py / matrix.out): signed out always 401, admin-only reads 403 for watch, c2 always 404 no_such_car for the c1 watch account
- Per-car isolation on fleet-wide reports: /charges, /charging-stats[/places], /compare, /statistics, /tracks, /visits/*, /fleet/*, /places/suggestions, mqtt preview: watch answer equals admin's ?cars=c1 answer (t4.py); cars=c2, car=c2, percent-encoded and ../ path tricks give 404
- Places isolation: a place only c2 used is 404 for the watch account, /places/at shows nothing, preview by id is 422, preview counts and place counts for a place covering both cars hold only c1's rows (t5.py, t6.py)
- Cross-car ids: c1's drive id under /cars/c2/... is no_such_drive; saved trips are per account (other accounts, admin included, get 404 on get/patch/delete); body car ids checked (422)
- Live stream: a c1-only account got no fleet/rows events of c2 (rename, rebuild), got removed/added when its car list changed, and the stream ended when the account was deleted (t18.py)
- CSRF: every change without X-Sentry-Mate: 1 is 403 (values 0, true, '1, 1' refused); text/plain cross-site style POST refused; no CORS headers on preflight (405)
- Sign-in: same answer and same timing for known and unknown names; 5 wrong tries per address and per account give 429 with Retry-After 30; right password refused while throttled (t7.py)
- Sessions: other accounts cannot see or end a session id; password change signs other sessions out; old password refused; cookie is __Host-, Secure, HttpOnly, SameSite=Strict
- Two-step sign-in: wrong code 401, a used TOTP code cannot be replayed (at confirm, at sign-in, in a second sign-in), recovery codes work once, /me is 401 while pending; an admin without two-step cannot turn the everyone rule on (409 two_step_first) (t8b.py, t20.py)
- Setup: wrong codes throttled on POST /setup (5) and /setup/claim (10 per minute); fresh, restore start, restore chunk and job need the claim (401 not_claimed); after setup every setup route is 409 setup_done; the code file is removed (t16.py, t17.py)
- Account admin: name rules (no unicode look-alikes, control characters, case-insensitive name_taken), confirm_password needed and checked, unknown cars 422, stale version 409, ids not reused after delete even across a server restart (t15.py, t23.py, t24a/b.py)
- Input fuzz of all 86 change routes with 18 malformed bodies each (wrong types, huge numbers, 1e400, deep nesting, invalid UTF-8, 70 KB strings): no 5xx, only 400/403/404/409/422 (fuzz1.py)
- Query fuzz (limit, from_ms/to_ms, bucket, max_points, q, search, places, offset, at_ms with 0, -1, 1e400, u64 max, 600 chars, %00) on 27 GET routes before it was stopped, plus hand-made after= cursors on every list: no 5xx except the invoices list (reported)
- Preferences, charge cost, drive purpose, purpose rules, trips, settings (tile url, geocode url, proxy, host names, time zone, car order): hidden cars, bad enums, overlong text, CSP-breaking tile origins, credentials in URLs all refused (t12.py, t14.py)
- CSV formula guard: =, +, -, @ values in trip names, drive notes and relations come out with a leading quote in /trips and /logbook CSV; quotes and newlines are quoted
- GPX and PDF injection: place and car names with < > & " ]]> ( ) \ are escaped in the GPX (parses as XML) and in the logbook PDF (pypdf reads it); download file names are reduced to safe characters
- UI pack upload: .., absolute paths, backslashes, symlinks, .exe, missing index, duplicate names by case, zip bomb (210 MB), truncated and garbage zips all 422; built-in name refused; api 2 pack cannot be chosen; watch upload 403; wrong or missing password 403 (t10.py)
- Static files: mock-api.js, ui.json, %2e%2e, ..%2f, NUL and trailing-slash paths are 404; pack files carry the strict CSP, API answers default-src 'none'; HSTS, nosniff, frame DENY on every answer
- Exports: password needed, file only for admins (watch 403, signed out 401), traversal ids 404, Range handled (206, 416 for reversed, beyond and multi-range), no-store (t21.py)
- Imports upload: wrong offset 409 with Upload-Offset, bad checksum 422, missing checksum 400, 1 MiB chunk 413, second import 409, start before ready refused, size 0, negative and 2^63 refused (t13.py)
- HTTP layer: TLS 1.2 refused, plain HTTP refused, 2 MB JSON body 413 (also chunked), 70 KB header refused on HTTP/2, unknown /api paths 404 in the error shape, OPTIONS/TRACE 405 with Allow
- Audit log: wrong sign-ins, confirms and changes recorded with account and address; group filter validated; entries cannot be forged through names (target brackets are escaped)
- Rust tests through web::testkit compile and run against a copy of the tree: (hunt scratch)/api-security/repro/api_security_hunt.rs (3 tests, all fail on the code as found; log in repro/test.log)
- Not tested: behaviour for addresses outside the home network (the sandbox has only loopback addresses), offline map tiles with a real .pmtiles file, a real move between two servers, Tesla/TeslaFi/TeslaMate inputs against live services

### Inputs and ingest

- Scratch harness: a cargo workspace outside the repo with path deps on the crates ((hunt scratch)/inputs-ingest/ws, run with `cargo test --offline --test <name> -- --nocapture`); inventory at (hunt scratch)/inputs-ingest/inventory.md; no tracked file changed, git status clean
- logfmt empty and truncated input: every prefix of a candump and a columns .log.xz is refused, never taken as a whole log (ws/tests/logfmt_fuzz.rs)
- logfmt bit flips: 6,000 mutated xz files of both formats never panic, and read_log + Line::parse gives the same lines and summary as read_lines (fast columns path)
- logfmt columns body: 40,000 mutated bodies (flips, inserts, truncation) never panic; accepted ones decode the same through both paths; odd keys (can00, trailing space, 8-digit ids, embedded newline) agree between fast path and text path
- logfmt text edge lines: CRLF, blank lines, overflowing seconds, bad hex, BOM, lone '(' all end as Other/Comment without panic
- ingest two uploads of the same log at once: 8 parallel whole-file uploads and 6 parallel chunk sequences give exactly one stored copy, the rest 409 busy / offset_mismatch, log count right, bytes identical (ws/tests/ingest_attack.rs)
- ingest header abuse: 20-digit offset 400, 19-digit offset 409 offset_mismatch, 19-digit length 413 too_large, '+0' offset 400, length 0 400; upper-case SHA accepted
- ingest name abuse: %2f traversal, NUL, slash, upper-case suffix all 400 bad_name or 404; percent-encoded valid names decode to the same file; years 0000 and 9999 store, pack and answer status correctly
- ingest status body abuse: {}, names as string, numbers in names, top-level array, not_held null, invalid UTF-8 all 400 bad_request
- ingest sidecars: over 1 MiB 413, 100,000-deep JSON 422 bad_sidecar (no stack overflow), BOM 422, bad public key judged failed/bad_signature
- ingest chain out of order (logs 3,2,1,0): all four verified, chain report clean; a verified log and its sidecar cannot be replaced (409 exists_different)
- ingest replacement of a failed log: resumable from the part (409 offset_mismatch tells the offset), verifies, quarantine dropped; three further bad copies keep only the first in quarantine; log count unchanged by replacements
- ingest sidecar arriving after its log was judged without one: re-judged to verified and the key pinned
- ingest code read: token claim rules, bad-token and per-token windows, write gate, store paths (.waiting cannot collide with a car key), missing/wanted flow; no defect found beyond those reported
- trust: read check(), judge(), chain() for overflow and index edge cases (idx[k-1]+1 cannot overflow, checked_sub/checked_add used); no defect found
- import zip reader: 12,000 mutated zips (classic and streamed with data descriptors) never panic or hang; 200 MB deflate bomb refused, lying sizes 'cut short', 65,535-byte name refused, garbage deflate refused, encrypted refused, 200,000 empty entries read in 0.35 s (ws/tests/zip_fuzz.rs)
- TeslaFi import, months in any order in one zip: same drives, charges, positions, state spans and updates as in order; the same file twice in a zip and again later adds nothing (ws/tests/tf_attack.rs)
- TeslaFi CSV quirks: LF, BOM, blank lines, every field quoted, quoted comma and doubled quotes in the name, upper-case header, spaces around header names, trailing comma, no final newline, US dates, ISO 'T' dates, missing seconds all import the same rows; semicolon, tab and UTF-16 files are refused with the NotTeslafi message
- TeslaFi DST: a minutely drive through the repeated hour stays one drive with every row used; 30-minute asleep polls through it all used; the spring-forward hour is skipped as dst_gap
- TeslaFi time zone option: unknown names, 'utc', 'GMT+2', '+02:00' refused; trailing space trimmed
- TeslaFi values: inf, soc 250 (clamped), lat/lon 0,0 and out of range, negative and huge odometer handled without wrong drives
- TeslaFi hostile zips: traversal and absolute names are only names (nothing is written by name), __MACOSX/.DS_Store/folders skipped, empty or junk-only zips refused, truncated zips error (rows of earlier entries handed over, no state saved, as documented)
- TeslaFi whole vs split files: 888 cuts of one poll sequence (either order, with and without a 20-row overlap) give identical drives, charges, positions and charge points; only state spans differ at 15 cuts (reported) (ws/tests/tf_split.rs)
- TeslaMate backup in UTF-8 with CRLF (PowerShell 7 style, not in the repo's tests): both fixtures import the same rows as with LF (ws/tests/tm_crlf.rs)
- TeslaMate import code read: input.rs (gzip/xz/UTF-16), copy.rs, value.rs timestamps and escapes; no defect found (repo tests already cover damaged backups, re-runs, column order)
- tesla crate read: convert.rs units (miles to km, never display units), poller gate/daily cap/back-off/401 once then forbidden/408 as asleep/429 wait clamp, hosts fixed per region, vehicle id digits-only in the path, HTTP timeouts and body caps; no defect found by reading (poller not driven against a fake server by me)
- teslafi crate read: classify() answers, poller data_id and time de-duplication, zone-change reset of last_poll_ms, timeouts and caps; costs parsing reported
- tmlocal read: MQTT subscriber is read-only with 64 KiB packet cap and doubling reconnect wait, topic split and prefix rules, map.rs value table and latitude/longitude pairing, catch-up uses Postgres' own COPY escaping; settings rule bypass reported
- dbc: Dbc::parse is only ever given the built-in DBC files (no caller outside the crate), so hostile DBC text is not reachable; decoder bit extraction read, no defect found
- Not exercised by running: ingest TLS server slots/refusals (server.rs), the CAN export bundle import in crates/engine/src/query/can_import.rs (needs a whole engine), tesla streaming socket, tmlocal against a live broker or Postgres, synth generators

### Background jobs and storage

- Plain rebuild (POST /cars/{car}/rebuild): three concurrent whole rebuilds of one car, partial windows, 1 ms window, only to_ms, far-future from_ms, bad bodies; drives and charges identical before/after each time
- Rebuild input checks: from>=to 422, negative/float 400, bad JSON 400, unknown car 404
- Full rebuild start race: three concurrent POSTs (system and car) gave one 202 and two 409 full_rebuild_running
- Full rebuild kill -9 mid-decoding then restart: resumed by itself, finished, shadow folder removed, history identical (only the progress figures are wrong, see findings)
- Full rebuild cancel (DELETE) mid-decoding: state cancelled, .full-rebuild removed, history untouched; DELETE with none running gives 409 full_rebuild_not_running
- Plain rebuild racing a full rebuild of the same car: both ended, history identical, no engine failures
- Compaction: two concurrent 'Compact now' start one pass; progress fields move; kill -9 mid-pass (with a rebuild running) then restart leaves all data readable and history identical; a complete pass keeps history identical and a rebuild from the packed logs gives the same rows
- A damaged SMO series day file is mended by a full rebuild (it copies the car without its SMO series)
- CLI export: beside a running server, broken pipe exits 1 cleanly, kill -9 leaves a snapshot in .export that the next export removes
- CLI import into empty folders: truncated file, wrong recovery key and a flipped byte are each refused with a clear message and nothing written; kill -9 mid-import then run again carries on; served data identical to the source
- Web export: POST/GET /system/exports, download matches the announced sha256, Range 0-9, suffix, open-ended give 206, past-the-end 416
- CLI edge cases: init twice, token revoke unknown, token new with control characters (stripped in list), chain/keys with unknown or path-like keys, log export with a traversal name, gps-layout unknown car and --days 0, import --cancel with none, move cancel/undo/status with none, health with right and wrong ports, second serve on the same data folder waits and says so
- Direct move kill -9 mid-copy: the staged copy and its state survive and the copy is carried on to 100 % (the switch then fails for another reason, see findings)
- Release check code read (ETag, rate limit, dev build, date parsing): no panic path found on odd input; only the cancellation defect below
- MQTT prefix checks, discovery payload building (serde-escaped names, clipped), clean-up on prefix/discovery change: read, no defect found beyond the stale value below; no live broker run

### Shared browser modules, dev server and tools

- ui/shared tests: node --test ui/shared/tests -> 161 pass; ui/devserver: python3 -m unittest discover -> passes (slow, >2 min)
- Pack copies: python3 ui/tools/sync_shared.py --check -> all copies match (6 packs, 27 modules)
- time.js day arithmetic: brute force of dayStart/dayKey/startOf/addPeriod(day)/toIsoZoned and day/week/month/year buckets against Intl for 26 zones (Chicago, Berlin, Lord Howe, Kathmandu, Chatham, Santiago, Cairo, Havana, Apia, Tehran, Troll, St Johns, Kiritimati, Beirut, ...) x 7 years: no error in 2018-2027 (probe2.mjs); midnight DST gaps resolve to 01:00 correctly
- units.js: km/mi, C/F (and deltas), kPa->bar/psi, Wh/km->Wh/mi, cost per 100 km->per 100 mi, negative zero ('-0.04 C' -> '0.0 C'), NaN/Infinity/null/strings -> dash, magnitude rollover (99.96 km -> '100 km'), currencies (EUR, USD, JPY 0 decimals, KWD 3 decimals, unknown code fallback), locales de/fr/en-GB/hi/ja/nb (probe1.mjs)
- csv.js: quoting, formula guard (=,+,-,@,tab,CR) with plain numbers left alone; server-side CSV of real and mock escape '=1+1', '+cmd', '@SUM' the same way (places created on both, CSV parsed back)
- tiers.js rules vs the real server: 1..8 steps, first step at 0, ascending up to 1,000,000 kWh, price 0..10, cycle day 1..28, base 0..1,000,000 all agree with POST /places on the real server and on the mock (writes2.py)
- password.js: COMMON, COMMON_WORDS, repeats and the 12 character rule read line by line against crates/accounts/src/password.rs: same lists and reasons
- Mock vs real server (target/debug/sentry-mate, dev seed, 2 cars x 3 days): key sets and types of ~100 GET answers diffed (crawl_all.py): drives, charges, timeline, states, parked, statistics, mileage, totals, efficiency, temperatures, battery, visits, tracks, settings, mqtt, network, storage, move, exports, setup all agree in shape
- Mock vs real PATCH /me/preferences: 23 bodies (bad units, theme, language, interface size, unknown car, unknown field, non-object): same status, code and field names on both
- Mock vs real list conventions on /cars/{car}/drives and /logbook: limit 0/500/501, bad sort/dir/after/from>to, offset, at_ms, purpose, gaps, q: same status and error fields; logbook pages carry offset/total on both (what vlist.js needs)
- Real server GET answers carry weak ETag + Cache-Control: private, no-cache as documented; error shape and 404 no_such_car identical on both
- api.js read through for dedupe, cache invalidation on write, abort handling, 429 retry, heavy-read queue and SSE reconnect/backoff/signed-out check: no defect found by reading; existing tests cover these
- check.js --self-test: ok/bad/pixel/stable packs all assert correctly (100 of 102 checks hold; the 2 hover ones are reported as a finding)

### The real server end to end

- Build and seed: used the dev-seed debug binary already built from HEAD (no source newer than it), `init` + `dev seed --days 4 --cars 2 --light --start 2026-11-19`, `serve` on scratch ports; 1217 logs decoded into 14 drives and 6 charges
- First admin: POST /api/v1/setup with the setup code (csrf 403, wrong code 403, done 200, second call 409 setup_done); SETUP-CODE.txt removed afterwards
- First-run page in Chromium on a second fresh server: wrong setup code message, claim with a lower-case spaced code, the three choices, wrong recovery key message (ui-setup.js)
- Accounts through the API: admin per pack, watch-only account, watch account limited to one car; confirm_password required
- Watch-only account: GET of 100 endpoints (admin ones 403, the rest 200), 21 change attempts refused except own preferences, password and trips (watch1.js, limited.js)
- Car-limited watch account: no other car's id, name, VIN prefix, place or key in any list, report, track, place lookup or CSV; other cars answer 404 no_such_car
- check.js --base for all six packs at 1440x900 and 390x844: every page ready, no console, CSP, API, overflow or small-text failure (only sign-in of non-default packs, as documented)
- Shape diff of 101 GET endpoints, mock vs real (shape.js): only the differences reported as findings mattered to pages
- Places through the API: create with tiers, time-of-use rate and session fee, 9 bad inputs with field errors, preview, /places/at, reprice dry run and real; tiered and time-of-use cost recomputed by hand (4.21 and 1.63 EUR match)
- Places form in Chromium (instrument, horizon, manager): tiered price with a rate saved for real; radius typed in feet stored as 152 m
- Typed charge costs through the API: total, per_kwh on pack and grid basis, per_minute, zero, 8 bad inputs, delete back to the tariff, wrong car 404; totals and statistics follow
- Car PATCH and car settings through the API: name, time zone (valid, invalid, stale version, missing version), config overrides, reference values, 6 bad values with field errors
- Settings: Cars in Chromium (instrument, imperial units): warranty, range and tyre pressure saves send only the changed field with no unit drift; rename; zone
- Units end to end: instrument pages as a watch account in mi, °F, psi and ideal range compared with the API by hand (odometer, range, consumption, tyres, temperatures, accuracy, estimate text)
- Time zones end to end: car zone set to Pacific/Auckland and America/St_Johns with the browser in another zone; pages show the car's zone; statistics days, heatmap hours and visit days grouped at the zone's midnight (03:30Z)
- Server settings and preferences through the API and in Chromium: units, default time zone (bad zone refused in page and API), tiles and geocode address rules, theme, range, interface size, release check on, Check now and off, pack switch from the page
- Upload tokens and CAN keys through the API: create, revoke twice, wrong car 404, key rename with trim, 61 characters and two lines refused
- Storage and compaction: POST compact, progress through logs, series, records, back to idle; 6.2 MB freed; also Compact now clicked in the page
- Export file: password required, wrong password 403, made and ready, download size and SHA-256 equal the answer, Range 206, suffix range, past-the-end 416, watch 403
- Full rebuild with kill -9 mid-job: carries on after the restart, finishes, history identical
- Restart with a tab open (instrument): 'Live updates paused, trying again', recovers without a reload, in-app navigation works after
- Missing-log scan: runs 2 minutes after a start, finds each car's log lost on the way, audit entry and health count; quiet while uploads are recent
- Import through the API: resumable upload with bad checksum, wrong offset, second import 409, cars before check 409, match rules, bad currency and zone, start, a new car made; the ready import survives a restart
- Import page in Chromium: file chosen, uploaded, checked, matched by VIN, started; the same file again is skipped as already imported, no row stored twice
- Import-only car on 14 pages of all six packs: no crash, no console error, no NaN, undefined or null text
- Empty server (no car) on 12 pages of all six packs: all ready; only the Charges defect reported
- MQTT settings: whole-setting PUT rules, test without a host, status, preview per car; packs list, pack download, chosen needs the password; network, sessions, recovery key show (409 once saved), release, audit summaries
- CSV on 43 resources with ?format=csv and Accept: text/csv

### Instrument

- Page checker --pixel at 390x844@3, 1440x900, 2560x1440, 3840x2160: 22 of 25 routes pass; the 3 fails are sub-pixel only (logbook baselines 1 px, places border fraction, settings-server step number 0.66 px). Log: (hunt scratch)/pack-instrument/check-pixel.log
- Page checker --hover: 25 of 25 routes pass (check-hover.log in the same folder)
- Page checker --stable 30 s at 1440x900 and 390x844: only timeline fails (reported); no flicker anywhere (check-stable.log)
- Page checker with --role watch, --cars 0, --cars 200 --latency-ms 300: no console errors, page errors, CSP violations or overflow; fails are missing cover ids in empty states plus the mock's 500 on /places?car= (check-watch.log, check-cars0.log, check-cars200.log)
- Every route as admin at 1440x900 (crawl.js): all ready, no console or page errors, no failed requests, one h1 each, every visible input labelled, every button and link has an accessible name, no duplicate ids, no 'undefined'/'NaN' text
- Same crawl as watch, with car c3, with synthetic car c4, dark scheme at 390x844, 200 cars, 1 car, --drives 3000, --own-certificate with another seed and now-ms: clean apart from the findings
- Mock modes crawled with no errors: --move-state (6 states), --export-state (4), --release-check (5), --teslamate-trouble (5), --full-rebuild (3), --map-tiles none/offline, --fresh (modes.log)
- 646 unique links collected from all admin pages, each opened (links.js): all load except the ones reported
- Units: after switching to miles/°F/psi every page was scanned for metric leftovers (units.js): values convert everywhere; only fixed wording such as '1 m steps' and '150 m' remains, and the Signals table's 'As sent' column is metric on purpose
- Per-car settings in imperial units (carset.js): labels change to mi/psi/Wh/mi and saved values convert back correctly (42 psi -> 289.6 kPa, 300 mi -> 482.8 km, 250 Wh/mi -> 155.3 Wh/km); out-of-range, bad zone, bad currency and empty name each show a field error
- Time zone: every car page gives identical text in browsers set to Los Angeles and Tokyo (tz.js); only car-less pages (Settings: Cars keys, Import, labelled) follow the browser
- Sign-in (signin.js): deep link kept in ?next= and restored after sign-in, empty/wrong password, dropped connection, 429, two-step with wrong then right code, /sign-in and /setup while signed in redirect to /, ?next=//evil.example is refused
- Place editor (places1.js, places2.js): empty submit, lat 999, 500, dropped connection, long RTL name, good save shows in the list, stale version gives a clear 409 message with a reload offer
- Add account dialog (accounts.js): required fields, weak-password advice dialog, duplicate name 409 shown under the field, labels and autocomplete on every field
- Charge cost editor (cost.js): empty, negative, non-number, too large and too many decimals are all refused with a message
- Auto-save controls with every save answering 500 (autosave.js) on Settings: Display, Integrations, Cars, Server, Fleet and Logbook: every failure shows the server's message and the control goes back to the saved value
- Navigation (nav.js, 150 ms latency): filter text survives reload and Back, period change with Back/Forward, six nav clicks in 400 ms end on the last page with one page style sheet, 401 on every call redirects to sign-in with next, server down shows 'No answer from the server' and Try again recovers
- Keyboard (kbd.js, kbd2.js): skip link, visible focus ring on every tabbed control, '/' opens the finder with combobox/listbox aria and returns focus on Esc, Ctrl+K car switcher returns focus, period popover is tabbable and Esc returns focus
- Contrast (contrast.js): computed text contrast on 21 pages in light and dark; nothing under 4.5:1 except the skip link (3.75:1, white on teal, shown only on focus)
- Phone 390x844 with 1.2 s latency (phone.js): aria-busy while loading, dock with 56 px targets, Menu dialog opens and navigates, no tap target under 32 px on Places
- Click-every-button pass on 24 routes (fuzz.js, fuzz-all.log): no page error or console error from any button; the 'NO EFFECT' lines were checked and are sort or already-selected buttons

### Horizon

- Checker --pixel at 390x844@3,1440x900,2560x1440,3840x2160 (default size): 21 of 25 routes pass; the 4 failures are reported as findings (output chk-pixel.txt in the scratch folder)
- Checker --hover at 390x844@3 and 1440x900: 25 of 25 pass (chk-hover.txt)
- Checker --stable 30 s at 390x844@3 and 1440x900: 25 of 25 pass, no shift/moved/flicker (chk-stable.txt)
- Checker --latency-ms 400: 24 of 25 pass (only the places phone border fraction failure, same as default)
- Checker --role watch: only missing admin-only cover items on settings-server and import (expected); no 403 requests on any page load
- Checker --cars 0: car pages fail only with 'no car to open' (expected); fleet, charges, statistics, travel, places, settings, system, help load without errors
- Every route in core/pages.js plus /system, /statistics, /charges, a drive and a charge loaded at 1440 and 390 as admin, watch, --cars 0, --cars 1, --map-tiles none: all reach ready, no console errors, no failed requests, no page-wide overflow, no unnamed buttons/inputs (crawl1.js, crawl*.txt)
- Unknown addresses: /nope, /cars/zz, unknown drive and charge ids show a clear Not found state with the right title and a way home
- Short addresses /car, /trips, /battery, /stats, /settings redirect to the section's last or first tab
- Sign-in: empty submit, wrong password, two-step with wrong/malformed/correct code, return to ?next= page, rate limit message with wait time, sign-out, back button after sign-out stays on sign-in (t-signin.js)
- Open redirect via ?next=: //host, /\\host, https://host, /sign-in, /api/... all land on / ; encoded //host stays on this origin as a 404 page
- Watch-only account: admin forms are disabled or replaced by 'Admins only' text (car settings, battery reference values, places New place, charge cost form, import, display server defaults, integrations); /cars/c2 and /settings/cars/c2 hidden as not found (s-watch.js, s-watch2.js)
- Units: after switching to miles/°F/psi no metric values remain in visible text, titles or aria-labels on any route except the raw signal values on Data health (shown as raw on purpose) (s-units.js)
- Car time zone change: clock, period label and row times follow the new zone on drives, charges, timeline, logbook and overview after a load (s-misc2.js)
- Period picker: presets, earlier/later steps (later disabled at now), custom range with reversed dates is swapped, empty date blocked, Escape closes and focus returns to the button; period carried between tabs; back/forward restores period and page (s-nav.js)
- Finder (/): search, Enter opens the match, no-match text; car switcher (Ctrl+K): arrow + Enter switches car keeping the page, no-match text (s-nav.js)
- Settings: Display and units on 500/403/409/dropped connection: the control goes back to the saved value and a flash says what could not be saved (s-fail.js)
- Settings: Cars on 500/409/422/dropped connection: error flash shown, 409 reloads the other person's values with a message (s-fail.js)
- Places editor: empty and whitespace name, radius and latitude out of range, priced place without a price all show field errors and focus the first bad field; a long mixed RTL/Latin name is cut at the field limit and does not overflow the page or list (s-places2.js)
- Logbook purpose dialog: opens with the current purpose selected, save updates the counts and totals at once, a 500 shows the server message in the dialog and keeps it open, Escape returns focus to the row button, purpose filter goes into the address (s-logbook.js)
- Charge cost editor: empty, negative, too large and too many decimals are refused with a message; per-kWh with 5 decimals saves and the cost panel shows the entered rate and keeps the invoice amount beside it (s-cost.js, s-cost2.js)
- Accounts: add account with empty fields, a taken name (409 shown under the field), over-long name with markup (cut to 64, shown as text, no HTML injection) (s-server2.js)
- Light and dark themes: computed text contrast on every route found nothing under 4.5:1 on solid backgrounds apart from two tags at 4.1-4.3 (reported); light theme screenshots of overview and drives look correct (s-contrast.js, contrast.txt)
- Drives summary numbers are consistent with the list (energy / distance = consumption; efficiency follows consumption)
- Click-every-button sweep at 1440 over 22 routes (about 850 controls): no uncaught exception, no 5xx and no failed request from any click; CSV buttons download files (clickall-1440.txt)
- Account menu: Escape closes it and aria-expanded goes back to false

### Manager

- Checker, every page, --pixel at 390x844@3/1440x900/2560x1440/3840x2160: 20 of 25 routes pass (logs: (hunt scratch)/pack-manager/check-*.log); the 5 failures are in the findings or are 1 px nits (logbook totals baseline, integrations ov-err icon, settings-server step numbers, places action buttons on fractional pixels)
- Checker --hover (390 and 1440): 25 of 25 pass; --stable 30 s (390 and 1440): 25 of 25 pass (7 with warnings); --latency-ms 400: all pass
- Checker --role watch: only coverage-mark misses on settings-server and import (admin-only content), no API failures; my own watch crawl (watch.js) found no 403s, no other cars' names leaking, no enabled admin controls (charge cost form is in a disabled fieldset, hidden cars answer Not found)
- crawl.js: all 26 routes x dark/light (prefers-color-scheme) x 390/1440 on default data, --role watch, --cars 1 --map-tiles none: every page ready, no console errors, no page errors, no failed requests, no horizontal overflow, every visible input labelled, every button and link has an accessible name
- inject.js: every GET endpoint of every route failed in turn with 500 and with a dropped connection (250 cases): no page hangs, no uncaught exception, no skeleton left behind (misleading empty states are a finding)
- clickall.js: every visible button on every route clicked as admin, and as watch with destructive ones included: no button does nothing, no page error, no 403 as watch
- kbd.js: Tab through every route (dark, 1280): every stop has a visible focus indicator, none off screen or covered
- units.js: with miles / °F / psi saved, no metric leftovers on any page except Data health's raw signal list (server units) and explanatory text
- Deep links and bad addresses (frame1.js): unknown page, unknown car, unknown drive/charge, wrong case, bad query values all give a clear Not found or fall back to defaults; /car /trips /battery /stats /settings short addresses work
- Period picker (period.js): earlier/later steps, back/forward, reversed custom range, All time, carried across tabs and sections, browser in Asia/Tokyo with the car in Los Angeles
- Sign-in (signin.js): empty, wrong password, two-step (bad format, wrong, right code), ?next kept and open-redirect variants refused, 429 with wait time, dropped connection, sign out, session ended in another tab sends to sign-in with the page kept
- Set-up on --fresh (setup2.js): empty/wrong/right code, start fresh, weak-password advice dialog with Esc returning focus, mismatched passwords, recovery key step
- Settings: Cars (setcars.js): name empty/spaces/RTL/64 chars, 500/403/409/422/dropped connection all shown and handled, time zone bogus/valid (clock follows), default price validation, range checks on numbers
- Places editor (places.js, places2.js): validation of name, radius, lat/lon, rate times; 500/403/409/422 (field errors placed, including rates)/dropped connection; rename shows at once in the list and on Drives; new place with HTML in the name is escaped; delete dialog shows effect, Esc returns focus, delete works
- Charge cost editor (cost.js): empty, negative, too large, step per mode, 500, 422 field errors, save and currency change, list shows the new cost afterwards
- Settings: Server accounts and sessions (server.js): add (empty, duplicate name, good), edit with 500/409/403, remove with 500, sign out everywhere else, host names dialog
- Settings: Integrations MQTT form (integ.js): port/host/pin validation, server field errors, 409, good save, publish switch reverts on 500
- Live refresh while editing (liveedit.js, --rows-every-s 2): typed cost, filter text, scroll position, open details, an open dialog, settings and place editor fields all survive
- Finder ('/'), car switcher (Ctrl+K, 200 cars), phone menu, account menu and period popover at 390: open, search, Enter, Esc, focus return, popovers inside the window
- Contrast scan (contrast.js) of all routes in both themes: only the logbook zero totals fail clearly (finding); a handful of tags sit at 4.3-4.4:1

### Console

- Checker, plain: `check.js --pack console --pixel --sizes 390x844@3,1440x900,2560x1440,3840x2160` passes every page (exit 0); with `--latency-ms 400` also passes. Logs: (hunt scratch)/pack-console/check-*.log
- Every route loads and reaches ready with no console error, page error or failed request: admin at 1440x900, watch role, car c3 at 390x844, a synthetic car (--cars 5, --cars 200), crawl.js
- Mock modes on fleet, settings/server, settings/integrations (both tabs), settings/display, system, help, car, health, import: --move-state (all 6), --export-state (all 4), --map-tiles none/offline, --release-check newer/error/checking/never, --teslamate-trouble auth_failed/catchup_failed, --full-rebuild running/failed/offered, --own-certificate, --cars 1/2, --seed 7, --now-ms: all ready, no console errors (modes.log)
- Every link of every page (578 unique hrefs) visited: no dead page apart from the findings; licence text files answer 200 text/plain (links.js)
- Every visible button clicked on 22 routes plus integrations set-up tab (buttons.js MODE=ok): none is dead (each makes a request, changes the page or opens a dialog) apart from the findings
- Server errors on writes (buttons.js MODE=500): settings/cars, settings/cars/c1, settings/display, settings/server, battery reference values, tokens, keys, sessions, compaction all show the server's message; exception is the sold section (finding)
- Units: miles / F / psi set, every page and drive and charge detail re-read: no metric leftovers in page values (units.js); MQTT preview and health raw signals stay metric on purpose
- Sign-in: wrong password, two-step wrong and right code, throttle message with wait, ?next= kept for in-app paths and refused for other sites / protocol-relative / javascript: / api paths, lost cookie goes to sign-in with next (signin.js)
- First-run set-up (--fresh): deep link goes to /setup, wrong code message, right code, reload keeps the claim, start fresh, password mismatch message, recovery key page (setup.js)
- Watch account: no enabled write control on drive, charge, places, integrations, import, server pages; other cars answer not found; no 403 in any page load or button click (watch-detail.js, buttons-watch.log)
- Keyboard: tab order starts at skip link, every focused control has a focus ring, skip link focuses main, dialogs keep focus inside, Escape closes and focus returns to the opener, g-chords, j/k/Enter, ?, / and Ctrl+K (frame.js, theme-focus.js)
- Labels: no visible input, select or textarea without a label or aria-label on any route in either theme; text contrast passes except decorative bar filler characters (a11y.js)
- Theme: prefers-color-scheme dark and light with theme auto, OS flip while open, switch in the account menu redraws charts and map without reload (theme-focus.js)
- Period: reversed dates in the picker are swapped, back button restores earlier periods, preset=bogus and from=abc fall back, unknown routes and unknown car/drive/charge ids show a not-found block with the right title (frame.js)
- Car rename shows at once in fleet list, link row, command list and car page after navigation; name validation (empty, spaces, 64-char cap, markup shown as text, RTL text accepted) (stale.js)
- Places: new place validation (name, radius, latitude), add, delete from the edit page, missing place redirect with message, 500 and dropped connection on save show a message (places.js)
- Software: name an update: bad and empty versions refused with a message, good version saved and shown (software.js)
- Import: empty file refused with a message, a made-up .sql uploads, is checked and reaches match-the-cars (import.js)
- Selects and search fields on all non-settings routes changed or filled with odd text: no page error or failed request (selects.js)
- 200 cars: fleet, charges and statistics for all cars, settings/cars, a synthetic car's pages all load

### Pebble

- Checker baseline: check.js --pack pebble --pixel at 390x844@3, 1440x900, 2560x1440, 3840x2160 passes 25 of 25; --hover passes 25 of 25; --device-size 80 passes; --server-args "--latency-ms 400" passes (outputs in (hunt scratch)/pack-pebble/check-*.txt)
- Checker with --role watch, --cars 0 and --cars 200: the only failures are 'missing' coverage items for admin-only controls or cars with no data, and 'no car to open'; no console, page, CSP or API errors
- Every route loads to its ready state with no console errors, page errors or failed requests: own sweep at 1440x900 light and OS-dark, 390x844, 360x740, with --cars 0/1/200, --role watch, and a combined trouble run (--map-tiles none, --release-check newer, --teslamate-trouble, --full-rebuild failed, --move-state, --export-state failed)
- Every button on every route clicked as admin and as watch at 390x844 (clicker.js): no page errors, no console errors, no HTTP 4xx/5xx, Escape closes every sheet, no unlabeled inputs or icon buttons without a name inside sheets
- Watch-only account: no admin controls enabled on any page (inputs disabled, save/delete buttons absent), no 403 requests on load or on any click, a car outside its list shows 'Nothing here'
- Sign-in: empty, wrong password, 429 with wait time, dropped connection, HTML 500, two-step with wrong and right code; errors shown with aria-invalid and role=alert; returns to ?next= address; safeNext rejects outside addresses by code reading
- Set-up wizard (--fresh): any address leads to /setup, empty and wrong code, start-fresh form (empty, mismatch, weak-password sheet with focus back in the field), recovery key step and reload in the middle, move form validation
- Session ended in another tab: next navigation and a save both go to /sign-in?next= with a toast, and signing in returns to the page
- Car settings form: rename with empty, spaces, 500, dropped connection, 403 and a stale version all show the right message; header pill, tab title and other pages show the new name; bad and good time zone saved and reflected in the header clock and day boundaries
- Charge cost editor: empty, negative, too large, 400 with field errors, dropped connection, total / per kWh / per minute previews, save, remove; the Charges list shows the new cost afterwards; ?edit=cost focuses the amount
- Places editor: empty, bad radius, missing price, bad currency, 500, dropped connection, deep links ?place=, ?place=missing, ?new=lat,lon, delete with confirmation; focus returns to the opener on Escape
- Logbook: purpose edit (500 shown in the sheet, then saved, totals and row updated), purpose filter in the address and kept on reload, search with no match, CSV export link and file, jump to date
- Accounts on Settings: Server: empty name, taken name, 64-character name, add and remove with password prompt (the mock does not check the confirm password, so the wrong-password path could not be judged)
- Rapid navigation through all routes under --latency-ms 500 and rapid period changes: final page and data always match the address, no errors; bad ?preset and ?from/to values fall back without a crash
- Period sheet: quick picks, Earlier/Later, reversed and empty dates validated, future range shows empty states, DST day resolves to 23 h, back button walks the periods, period carried between pages
- Fleet with 200 cars: Ctrl K switcher (search, arrows, Enter, no match), sort orders in the address, Show more
- Units: with miles, Fahrenheit, psi and de-DE numbers every page was scanned for metric leftovers; only the three in the units finding were found
- Keyboard: tab order on 18 routes starts at the skip link and ends at the dock, every stop has a visible focus style (outline or wrapper ring), nothing focusable is off screen
- Accessibility scan of every route: no inputs without labels, no buttons or links without a name, no duplicate ids, one h1 per page, no dangling aria references
- Empty states: car with no history (c4 with --cars 200) and zero cars: every page says why it is empty; only the Battery sentence in the findings is broken
- Import page: file upload of a wrong and an empty file runs through the mock's check step without errors

### Logbook

- Checker --pixel at 390x844@3, 1440x900, 2560x1440, 3840x2160: 20 of 25 routes pass (failures reported below); output in (hunt scratch)/pack-logbook/check-pixel.txt
- Checker --stable (30 s per page, 1440x900 and 390x844): 25 of 25 pass, no layout shift, moved blocks or flicker while live updates arrive (check-stable.txt)
- Checker with --server-args "--latency-ms 400": 25 of 25 pass (check-latency.txt)
- Checker with --role watch: no console, api, csp or overflow failures; the only 'missing' items are admin-only controls that are rightly hidden (check-watch.txt)
- Watch-only account, own crawl of all 26 routes in light and dark at 390 and 1440: every page ready, no 403 or other failed request, no console error, no admin write controls shown (crawl-watch.json, watchbtns.js, clickall-watch.txt)
- Every route in light and dark (prefers-color-scheme) at 390 and 1440 as admin: all reach data-ready with their own id, no console errors, no failed requests, no button or link without an accessible name, no input without a label (crawl.js, crawl-admin.json)
- Theme preference dark and language de-DE via PATCH me/preferences: applied on every page (units.js)
- Units mi / °F / psi: no leftover km, °C, bar, km/h or Wh/km in any page's own figures (units.js); only raw signal values on Data health and the exact-odometer tooltip in Logbook keep metric
- Clicked every button on every route as admin (about 900 buttons, clickall.js): no uncaught exception, no console error, no unexpected failed request; the buttons that looked dead were sort heads, already-selected radios or legend toggles and do work (dead.js)
- Writes failing with 500 (failwrites.js, 196 actions across car facts, trips, logbook purposes and rules, charge costs, rebuilds, settings, Tesla account, pins): the server's message is shown in the form or a flash every time, except the map style switch (reported)
- Page GETs failing with 500 and with a dropped connection (failgets.js): every page shows an error state or per-part errors with Try again, none hangs in loading, and Try again recovers once the server answers
- Session ending mid-use (every API answer 401): the next navigation goes to /sign-in?next=<page> (expire.js, failgets-401.txt)
- Sign-in on --signed-out: deep link kept in next, empty fields, wrong password, two-step with wrong and right code, open-redirect attempt next=//example.org refused, sign-out, Back after sign-out stays on sign-in (signin.js)
- First-run set-up on --fresh: empty and wrong code messages, code accepted in lower case with spaces, reload keeps the claimed step, Move form validates empty fields (setup.js, setup2.js)
- Period picker: presets, Earlier/Later, custom dates, from-after-to message, future dates give an empty state, period carried to other pages, Back and Forward restore it (period.js)
- Rapid in-app navigation through all routes and bursts of period changes at 1200 ms latency: the last page wins, no stale content, no errors (race.js, race2.js)
- Drives filters (minimum distance, minimum speed, find, source, show, order): each sends the right API parameter, lands in the address, survives a reload, summary tiles follow the filtered rows, empty state names the filter (drives.js)
- Stale data: a cost saved on a charge shows in the Charges list after Back and in Statistics; a purpose set in Logbook updates the totals at once; a renamed place shows in Places, Drives, Timeline, Travel, Logbook and Trip (stale.js, places.js)
- Place editor validation: empty and blank name, radius below 10 m and above 5000 m, latitude out of range or empty all show a field message and send nothing (places.js)
- Settings: Cars validation: empty name, unknown time zone, tyre pressure out of range, negative default price show messages and send nothing; a time zone change is reflected in the masthead clock at once; HTML in a car name is shown as text everywhere (setcars.js)
- Settings: Server new account dialog: empty name, blank name, duplicate name (409 name_taken shown in the form) (accounts.js)
- Modal dialogs (95 distinct openers across all pages): labelled, focus moves inside, Escape closes, focus returns to the opener, except the in-page 'Choose another car' buttons (dialogs.js, dialogs.txt)
- Keyboard: skip link first, visible focus ring on every one of the first 14 tab stops, Pages menu opens with Enter and Escape returns focus to its button, '/' focuses the finder and Enter opens the found page (frame.js)
- Zero cars (--cars 0): Fleet, Settings: Cars, Statistics, Travel, Places show explained empty states; car routes show 'no car with this id' with a link to Fleet (cars0.js)
- Release notice with --release-check newer: footer and Help show the newer build
