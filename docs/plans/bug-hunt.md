# Bug hunt

[← Back to the README](../../README.md)

The owner's instruction: find every problem and bug in every page and every feature, and assume everything is broken. This page records what was checked, what was found and fixed, and what was found and is still open. All data used was made up.

How it was done: the code was split into areas (below). In each round one hunter per area listed every feature, route, endpoint and job of its area and tried to break each one (edge cases, hostile input, concurrency, data correctness, every page at phone, laptop, 1440p and 4K sizes, light and dark, each interface size). A second pass then tried to refute every finding against the code and the docs; only what it could reproduce counted. Each confirmed bug got a failing test first where that was feasible, then the fix.

State: 1 round(s); 141 bugs fixed, 5 found and open, 39 archived with their pack, 4 reports refuted.

- [Found and not fixed](#found-and-not-fixed)
- [Archived with their pack](#archived-with-their-pack)
- [Fixed](#fixed)
- [Reported and refuted](#reported-and-refuted)
- [Checked and found working](#checked-and-found-working)

## Found and not fixed

### Background jobs and storage

- **A damaged drives table cannot be mended by a rebuild or a full rebuild; the Drives page answers 500 for good** (medium; crates/engine/src/full.rs:676-700 (full_seed copies the car's folder with the damaged table, then clear_window reads it); crates/engine/src/pipeline.rs:1252-1264 (clear_window); crates/db/src/shadow.r). Partly fixed. Left: A damaged drives table is still not made again by a rebuild or a full rebuild; pages that need it answer 500 until the folder is put back from a backup. Needs the owner's decision. The table also holds imported rows and typed values (costs, purposes) that no stored log gives, so starting it empty would lose them silently. Today the admins get the data_damaged notice naming the file and saying to restore db/cars/<id> from a backup. If the owner wants a way without a backup, it would be an explicit 'make it again and lose the imported rows' action that sets the damaged file aside and says what could not be carried.
- **A full rebuild resumed after a restart, and any partial-window rebuild, decode about 8 times slower than a whole one** (low; crates/engine/src/pipeline.rs:1184-1211 (window_batch falls back to 1 when any row exists at or after the first wanted log)). Partly fixed. Left: Partial-window half: a plain rebuild of a period still rebuilds a window after every log (about 8 times slower than a whole rebuild). Left on purpose. Rows exist after the period, and the engine batches only where nothing is stored at or after the logs: crates/engine/src/pipeline.rs and crates/sentry-mate/src/window_batch.rs record that batching logs that fill a gap before stored rows gave a drive another id. Making it fast needs a new proof of the identical timeline (ids included) for partial windows, likely a change to how ids are kept across windows: days of engine work, and a slow answer is the only harm (the request answers 'running' after 60 s and the job carries on, audited at its end).

### Instrument

- **Light theme: link and warning text in the accent colours is under 4.5:1** (medium; ui/packs/instrument/web/app.css (--live and --warm used as text colours)). Needs the owner's decision on the light palette. Measured on drawn pixels: links in --live #0B9488 are 3.0 to 3.2:1 on the page, warning text in --warm #B4680A 3.6:1, a route pin's letter 3.2:1, the primary button's white label 4.3:1; the dark theme passes everywhere. Only the tags, pills and Logbook totals were fixed. The rest means darker --live and --warm for text in the light theme (about 150 rules, some on dark map glass where darker would be wrong), so each use needs a look.
- **Interface size 125 on a phone: the car overview's "Outside map links off" chip lies over the car's marker** (low; ui/packs/instrument/web/pages/car.js (the car map's framing)). Not fixed: the marker sits at the map's right edge under the chip (3.4 px by the checker at 125; at the edge at 100 too). It is the car map's framing and needs its own look.

### Console

- **Private LAN addresses used as examples in page text (rule: use 192.0.2.x)** (low; ui/packs/console/web/pages/settings-server.js:134; pages/setup.js:215-216; ui/devserver/README.md (192.168.1.30 for the made-up new server)). Needs the owner's decision: Instrument's settings-server.js and setup.js do say 192.168.1.20, but so do the server's own error text, the docs and the dev server tests (about 36 files), and the second pass refuted the same report for Pebble as a deliberate generic example of a home-network address. Change it everywhere in one pass or nowhere

## Archived with their pack

The owner archived every UI pack except Instrument while the hunt ran. Every finding from Horizon, Manager, Console, Pebble and Logbook was then tried on Instrument: what Instrument, the shared modules, the dev server or the server also had was fixed there (under Fixed); what exists only in an archived pack is kept here for the day a pack comes back, with what was seen on Instrument. Fixes made to those packs before the change are not on main: only Instrument, the shared modules and the server carry them.

### The real server end to end

- **Console's Travel page prints 'null' for stops with no town** (medium; ui/packs/console/web/pages/travel.js:122, :131, :152, :162, :170). archived (pack archived). the bug is in ui/packs/console only, which is now under ui/archived.
- **Pebble's Settings: Display shows the word 'null' under the release check** (low; ui/packs/pebble/web/pages/settings-display.js:266-271 (drawRel)). archived (pack archived). the bug is in the Pebble pack only.
- **Console's drive page labels the fourth tyre line 'undefined rr'** (low; ui/packs/console/web/pages/drive.js:174 (three legend glyphs) and ui/packs/console/web/core/chart.js:44 (DASHES has three entries)). archived (pack archived). the bug is in the Console pack only.
- **Pebble tells a car with no data 'it should fall asleep soon'** (low; ui/packs/pebble/web/pages/car.js (the 'Why it is awake' block)). archived (pack archived). the bug is in the Pebble pack only; Instrument already says 'Not known: no log for now.'

### Horizon

- **/charging shows a blank page forever (uncaught RangeError: unknown page: null)** (medium; ui/packs/horizon/web/core/router.js:221-223 (section short-address regex), core/frame.js sectionPage(), core/pages.js GROUPS; ui/packs/horizon/web/core/README.md:17). archived (pack archived). Instrument has no section short addresses; /charging, /car, /trips, /battery, /stats and /settings each open the Not found screen with its heading and no page error (probed on HEAD). The regex and the stale group exist only in ui/archived/horizon.
- **The pack's page guide describes six sections with a Charging section and the wrong storage key** (low; ui/packs/horizon/web/core/README.md:3-24). archived (pack archived). the stale text is in ui/archived/horizon/web/core/README.md (copied from Manager). Instrument's core/README.md names no Charging section, no short addresses and no other pack's storage key or --pack.

### Manager

- **Short address /charging shows a blank page (uncaught RangeError), or All cars when no car was opened** (medium; ui/packs/manager/web/core/router.js:221-223; ui/packs/manager/web/core/pages.js GROUPS; ui/packs/manager/web/core/README.md:16). archived (pack archived). same as the Horizon one; Instrument's router has no short addresses and answers Not found. Only ui/archived/manager has the code.

### Console

- **Battery: the "by month" table never shows a row (ReferenceError: warranty is not defined)** (high; ui/packs/console/web/pages/battery.js:354 (paintMonths), variable declared at line 146 in another function; route /cars/{car}/battery). archived (pack archived). Console's own battery.js
- **Places: saving an existing place leaves the form dead (no "Saved" note, save button stays disabled, old heading)** (high; ui/packs/console/web/pages/places.js:361-362 (save) with ui/shared/state.js navigate; route /places?place={id}). archived (pack archived). in Instrument two saves in a row both answer 200, go to the list and show 'Saved' (probed)
- **Places: deleting from the list leaves the row in the list and shows no confirmation** (medium; ui/packs/console/web/pages/places.js:99 (removePlace); route /places). archived (pack archived). in Instrument the row goes and a note says 'Deleted' (probed)
- **Travel: the [refresh] button throws and does nothing** (medium; ui/packs/console/web/pages/travel.js:102; scoped client built in ui/packs/console/web/core/router.js counted()). archived (pack archived). only Console calls ctx.api.invalidate
- **Settings: car: a failed "mark as sold" / "not sold" / sold-date change shows no error** (medium; ui/packs/console/web/pages/settings-cars.js:396-400 (paintSold save)). archived (pack archived). Instrument shows 'Could not save sold: ...' on a 500 (probed)
- **Error block says "the page tries again when it is back" but the page never retries** (medium; ui/packs/console/web/core/states.js:22 (text), core/router.js showError, core/frame.js showLive). archived (pack archived). Console's wording; Instrument's says 'then try again' and has a Try again button
- **Command list key "server.certificate" lands on nothing** (low; ui/packs/console/web/core/palette.js:39 and pages/settings-server.js:127, 786). archived (pack archived). Console's command list; Instrument's finder links to no such anchor
- **Settings: car: after typing a new time zone the line still describes the old zone and the header clock keeps the old zone** (low; ui/packs/console/web/pages/settings-cars.js:513 (save calls only paintTop), core/frame.js clockTz). archived (pack archived). in Instrument the clock and the zone text follow at once (probed)
- **Settings: car: after a rename the tab title and the command bar car button keep the old name; the tab title names the car twice** (low; ui/packs/console/web/pages/settings-cars.js:31 (title) and patchCar; core/router.js docTitle; core/frame.js names map). archived (pack archived). in Instrument title, sidebar and trail show the new name at once, and the title names the car once (probed)
- **Logbook: a purpose that could not be saved stays selected in the row** (low; ui/packs/console/web/pages/logbook.js:61-68 (purposeField change handler)). archived (pack archived). Console's inline select; Instrument edits the purpose in a dialog that keeps the error
- **Filter line shows values it ignores as active, and reversed from/to gives a silently wrong range** (low; ui/packs/console/web/pages/drives.js:52-77 (toParams, onApply), pages/charges.js filter, core/filterline.js show()). archived (pack archived). Console's filter line; Instrument has none
- **Command bar clock shows the browser's clock, not the server's, for up to a minute on pages without a car** (low; ui/packs/console/web/core/frame.js:150-166 (paintClock, startClock)). archived (pack archived). Console's clock; Instrument draws its clock after the first API answers, by the server's time
- **Settings: car: the key efficiency_wh_per_km_override breaks in the middle of the word** (low; ui/packs/console/web/pages/settings-cars.js:567 and app.css .ke-k; /settings/cars/{car} reference values at 1440 px). archived (pack archived). Console's key column
- **Interface size 80 and 125: checker failures besides the sideways scroll (fractional borders, text under 10 px, focus scroll)** (low; Logbook purpose select (.pur-cell select.in--cell), Travel tables, Import phone rows, Drives/Logbook sort select on phone; settings-cars sold date input on phone). archived (pack archived). Console's elements; Instrument's own runs at 80, 90, 110 and 125 pass apart from the car map chip named under l-size-125-110-phone-overflow

### Pebble

- **The word "null" is drawn on six Pebble pages and sheets** (medium; ui/packs/pebble/web/pages/settings-display.js:266 (release check rows), pages/settings-cars.js:468 (Free Supercharging, watch account), pages/fleet.js:191 (Order sheet), core/settings.js:79-80 (pick s). archived (pack archived). no 'null', 'undefined' or 'NaN' text on any Instrument route (probed), and no native append/replaceChildren with a conditional null in the pack (grep)
- **Interface size 125 % on a phone: dock and account button are cut by the screen edge and every page scrolls sideways** (medium; ui/packs/pebble/web/core/frame.js placeDock() and app.css (.dock, .head-bar); all routes). archived (pack archived). Pebble's dock; Instrument at 125 on a phone has no sideways scroll
- **Charges on a phone at the default size: "Per 100 km" and "DC per kWh" figures are cut off and the page scrolls sideways** (medium; ui/packs/pebble/web/pages/charges.js:300-301 (moneyTile), core/bits.js:302-307 money(), app.css:987 (.money); routes /cars/{car}/charges and /charges). archived (pack archived). Pebble's money tile; Instrument's Charges fits at 360 and 390 px, inner scrollers included (probed)
- **More sideways overflow at phone widths: Integrations Set up, Charge, Statistics** (low; ui/packs/pebble/web/pages/settings-integrations.js (.iv-top chip, Set up tab), pages/charge.js:76-78 (context bar date pill), pages/statistics.js (bucket segmented control in .page-bar)). archived (pack archived). every Instrument route, with Integrations Set up, fits a 360x740 window (probed; check.js --pixel at 360x740 passes)
- **Segmented controls are cut off inside sheets on a phone (Charges Filters, Place editor price type)** (medium; ui/packs/pebble/web/pages/places.js:111 (billing segmented: No price / Per kWh / Per minute / Tiered (per kWh steps)), pages/charges.js Filters sheet; core/form.js segmented(), app.css .seg). archived (pack archived). Pebble's sheets and .seg control; Instrument's place editor is a page and fits. I did not open every Instrument dialog at phone width
- **Links to page sections land at the top: #move, #habits, #release-check, #maps, #addresses, #overlaps-{car} do not exist in Pebble** (medium; ui/packs/pebble/web/pages/settings-server.js:442 (move tile has no id), pages/statistics.js:106 (links to charges#habits; pages/charges.js has no such id), pages/settings-server.js:176 with ui/shared/). archived (pack archived). in Instrument #move, #habits, #release-check, #release, #maps and #addresses all exist and are scrolled to (probed; overview-links.test.js), and no page links to #overlaps-{car}
- **Place editor: name allows 80 characters but the server takes 64, and server field errors are shown as one unlabelled line** (low; ui/packs/pebble/web/pages/places.js:105 (maxlength: 80), :303-306 (save catch joins e.fields values), :296 (preview shows only e.message); server rule crates/engine/src/query/places.rs:997). archived (pack archived). Instrument's name field has maxlength 64 and places field errors under their fields
- **With miles and Fahrenheit chosen, three texts stay metric** (low; ui/packs/pebble/web/pages/trip.js:81, pages/charges.js:351, pages/places.js:107 and :278). archived (pack archived). Pebble's three texts; Instrument's only 'per 100 km' is a CSV column that holds the per-100-km value
- **Logbook: "1 drives" in the purpose tiles, and keyboard focus is lost after saving a purpose** (low; ui/packs/pebble/web/pages/logbook.js:137 (totalsTiles) and :172-196 (edit / refresh)). archived (pack archived). Instrument writes 'drive' for one and returns the focus to the row's edit button (probed)

### Logbook

- **Footer says "Release check off" on every page while the release check is on** (medium; ui/packs/logbook/web/core/frame.js drawFooter (line ~263): `release ? [...] : 'Release check off'`). archived (pack archived). Logbook's own footer line; Instrument has no such line
- **Interface size 125 % (and 110 %) on a phone: pages scroll sideways, controls are cut, axis labels overlap** (high; ui/packs/logbook/web/app.css:400-401 (.block-head / .block-actions never wrap), pages/drive.css (ul.climate-key), pages/drives.css (.cell-facts), pages/charge.css (.fig-trio .trio), pages/settings-dis). archived (pack archived). Instrument passes check.js --pixel at 80, 90, 110 and 125 on 390x844 and 1440x900 except one thing I did not fix: at 125 on a phone the car overview's 'Outside map links off' chip lies over the car's marker (3.4 px by the checker; the marker sits at the map's right edge under the chip at 100 too). T
- **Pointing at a chart makes the page grow and everything below jump (Drive +43 px, Statistics -20 px, Trip, Charge, Battery, Efficiency)** (medium; ui/packs/logbook/web/pages/drive.js:341 and pages/drive.css:70-73 (.readings / .reading dd, the Tyres reading); the readouts of pages/trip.js, charge.js, battery.js, efficiency.js, statistics.js). archived (pack archived). Instrument passes check.js --hover on all 27 routes
- **/charges (all cars) is 568 px wide on a 390 px phone** (medium; ui/packs/logbook/web/pages/charges.js (missing-data block head with CSV and "Rebuild these charges from the stored logs"; curve block head with the car select and CSV), app.css:400-401). archived (pack archived). Instrument's /charges and /statistics are 390 and 360 px wide in those windows (probed)
- **Page style sheets are unscoped and stay loaded, so a page's layout depends on which pages were visited before** (medium; ui/packs/logbook/web/pages/drives.css:139 (`.block-actions { ... flex-wrap: wrap; }`) and about 1200 other selectors without a .page-<id> prefix in car.css, charge.css, drive.css, drives.css, fleet.cs). archived (pack archived). Logbook's router; Instrument's router drops the previous page's sheet
- **Logbook throws an unhandled promise rejection when its first list request fails** (low; ui/packs/logbook/web/pages/logbook.js:474-481 with core/table.js windowLedger `ready`). archived (pack archived). in Instrument a 500 on /logbook gives the error page with no page error (probed)
- **Duplicate element ids on Settings: Display and units (c-theme-label, c-interface_size-label)** (low; ui/packs/logbook/web/core/form.js:119 (choice ids come from `name`), core/frame.js displayMenu (names 'theme', 'interface_size'), pages/settings-display.js (same names)). archived (pack archived). no duplicate id on any Instrument route at 360, 390 or 1440 px (probed)
- **In-page "Choose another car" opens the car switcher but Escape puts the focus on the masthead button, not back on the button used** (low; ui/packs/logbook/web/core/switcher.js (close refocuses the masthead trigger); openers on pages/logbook.js, software.js, health.js). archived (pack archived). in Instrument Esc returns the focus to the in-page button (probed on Settings: Cars)
- **"Apply" on an import lot and "Jump to date" do nothing when nothing is chosen** (low; ui/packs/logbook/web/pages/import.js:1232-1234 (giveCar returns when no car is picked), core/table.js:364-365 (jump only focuses the date)). archived (pack archived). the Apply pattern is only in Logbook's import.js; Instrument's Jump to date with no date focuses the date field, which the second pass judged a response

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

### Inputs and ingest

- TeslaFi import: one row with a wrong date drops the rest of the file and makes other months permanently 'already imported' (medium). Test: crates/import/tests/teslafi.rs: one_row_with_a_wrong_date_is_passed_over_alone, a_real_gap_in_a_file_is_not_a_wrong_date, many_rows_out_of_order_are_said
- TeslaFi import: a row dated before 1970 is stored at 1970-01-01 00:00:00 instead of being skipped (low). Test: crates/import/tests/teslafi.rs: a_date_before_1970_is_a_bad_time; crates/import/src/teslafi/poll.rs: local_time_goes_to_utc_through_dst (Before1970 cases)
- Local TeslaMate: the saved database password can be sent to another host without typing it again (password_resend rule bypass) (medium). Test: crates/tmlocal/src/lib.rs: a_kept_database_password_never_follows_a_host_changed_while_the_database_is_off
- A zipped TeslaFi month in which the car mostly stood asleep is refused as 'a zip bomb?' (the same file plain or gzipped imports) (medium). Test: crates/import/tests/teslafi.rs: a_month_the_car_stood_asleep_reads_the_same_zipped_gzipped_or_plain; crates/import/src/zip.rs zip-bomb test (with_max_ratio(500) case added)
- TeslaFi import: a gzipped or xz'd upload unpacks without any ratio or size limit (a 310 KB file kept an import busy for 28 s; a zip of the same is refused) (low). Test: crates/import/tests/teslafi.rs: a_packed_file_that_unpacks_to_far_more_than_a_real_one_is_refused; crates/import/tests/import.rs: what_isnt_a_whole_backup_is_refused_before_anything_is_handed_over (gzip/xz bomb case for the TeslaMate backup path)
- TeslaFi charge costs: a cost written with a decimal comma is read 100 times too high; a cost of 0 and negative costs are kept (low). Test: crates/teslafi/src/costs.rs: a_cost_is_read_as_written
- TeslaFi import: a state span that ends exactly where a file ends is not extended to the next file's first poll (low). Test: crates/import/tests/teslafi.rs: polls_cut_into_two_files_anywhere_give_what_one_file_gives, a_state_span_reaches_the_next_files_first_poll_until_a_file_between_them_comes

### Background jobs and storage

- Nothing can be moved or imported into a new server that has started once (move lines, 'Move here', and the documented `import`) (high). Test: crates/sentry-mate/src/main.rs a_server_that_started_once_still_takes_data_in; crates/web/tests/first_start.rs; crates/moving/tests/streamed_import.rs a_web_servers_first_start_files_are_set_aside
- A plain rebuild over one damaged series day file wipes the car's drives and charges, fails with a bare 500, and leaves no notice or audit entry (high). Test: crates/engine/tests/pipeline.rs a_rebuild_mends_a_damaged_day_of_decoded_samples; crates/web/tests/system.rs a_damaged_day_of_decoded_samples_is_said_and_a_rebuild_mends_it, a_rebuild_that_fails_says_why_in_its_answer_the_audit_log_and_a_notice
- A full rebuild (and a plain one) reports done and switches to a poorer history when stored logs cannot be read (high). Test: crates/engine/tests/pipeline.rs a_full_rebuild_keeps_the_old_history_when_a_stored_log_cant_be_read, a_rebuild_that_cant_read_a_stored_log_ends_failed_and_says_so
- A damaged drives table cannot be mended by a rebuild or a full rebuild; the Drives page answers 500 for good (medium). Test: crates/web/tests/system.rs a_damaged_table_is_said_in_a_notice_with_what_to_do
- A full rebuild forgets the logs it had done when the server restarts; progress and the audit summary count only the rest (medium). Test: crates/engine/tests/pipeline.rs a_full_rebuild_cut_off_while_decoding_still_counts_every_log
- 'Check now' whose request goes away mid-check leaves the release check 'checking' for good (medium). Test: crates/web/tests/release_check.rs a_check_now_whose_request_goes_away_ends_all_the_same
- A rebuild whose request is cut (timeout, closed tab) runs to the end but is never written to the audit log; one that outlasts 60 s is audited as done while it runs (medium). Test: crates/web/tests/system.rs a_rebuild_is_audited_when_it_ends_whatever_became_of_its_request
- The rebuild's audit summary gives nonsense day counts ('20778 days', '1 days') (low). Test: crates/web/tests/system.rs a_rebuild_is_audited_when_it_ends_whatever_became_of_its_request
- A full rebuild resumed after a restart, and any partial-window rebuild, decode about 8 times slower than a whole one (low). Test: crates/sentry-mate/src/window_batch.rs a_full_rebuild_carried_on_after_a_restart_gives_the_same_timeline_per_run_of_logs, ..._over_a_week
- MQTT: the retained 'energy at destination' value is never cleared when the route ends (low). Test: crates/mqtt/src/worker.rs a_value_no_longer_known_is_cleared_once
- A full rebuild started in the instant the previous one's worker is ending gets no worker and stays 'running' in phase queued (low). Test: crates/engine/tests/pipeline.rs a_full_rebuild_asked_for_as_the_last_ones_worker_ends_runs

### Shared browser modules, dev server and tools

- Audit log "Download CSV" gets JSON from the real server; the mock answers CSV and hides it (medium). Test: crates/web/tests/lists_csv.rs: the_lists_of_accounts_sessions_packs_notices_and_the_audit_log_answer_csv (re-run on HEAD: passes; covers /audit CSV with every matching row and the group filter)
- Dev server sends `now` deltas with half a value pair ({"at_ms": ...} without value and sources) (medium). Test: ui/devserver/tests/test_live.py: Delta.test_a_pair_with_sources_goes_whole, Delta.test_only_what_changed_and_a_field_gone_is_null, EventDeltas.test_a_pair_is_always_sent_whole (seen failing first). The mock's diff now mirrors crates/sentry-mate/src/site.rs delta: a pair whole, arrays whole, null for
- Car overview loses data-ready for good after an in-page #hash step when the tab remembers a period (medium). Test: ui/shared/tests/state.test.js: 'on the car overview, which has a period of its own, a #hash is the same route while the tab remembers a period' (fails with the fix reverted). sameRoute and stateFromUrl now build the state from one helper (routeState); synced to ui/packs/instrument/web/shared.
- node ui/tools/check.js --self-test fails (2 of 102 checks) on a clean tree (medium). Test: node ui/tools/check.js --self-test on HEAD: 'self-test passed: 102 checks', exit 0
- Mock /cars/{car}/health lacks chain_gaps_total, dropped_frames_total and unknown_spans_total (and adds from_ms/to_ms the real server does not send) (medium). Test: ui/devserver/tests/test_system.py: Health.test_each_list_has_its_whole_count_and_no_period_of_its_own, HealthListsCapped.test_the_newest_are_kept_and_the_totals_count_all; ui/tools/health-totals.test.js (new, README row added; seen failing '3' !== '36' before the page fix)
- Mock /cars/{car}/events payloads use string ids and another topic prefix than the real server (low). Test: ui/devserver/tests/test_reports.py: Software.test_events (car, drive and charge are numbers); test_system.py: api_version and versions.api are numbers. The mock now mirrors the real MQTT payload and docs/ui-api.md says so. Left as is: the mock's MQTT namespace fixture 'sentrymate' (a setting, not a 
- Mock leaves out fields the real server sends on /me, /me/sessions, /accounts, awake-reasons, upload tokens and reports (low). Test: ui/devserver/tests/test_real_fields.py (new: RealFields, CarsNamedInTheQuery), test_places.py: PlaceNames.test_a_name_is_trimmed_first_and_then_one_plain_line; exact key sets in test_auth.py and test_live.py updated to include the new fields. Known remaining difference: the mock's logbook from_ms wi
- Byte sizes print '1,000 kB', '1,000 B', '10.0 kB' at unit edges (low). Test: ui/shared/tests/units.test.js: 'a byte size that rounds up to 1,000 steps to the next unit, and 10 and more has no decimal' (seen failing first; opts.digits kept)
- Hour buckets repeat a key on the autumn clock change (and drift off the wall-clock hour in 30-minute-DST zones) (low). Test: ui/shared/tests/time.test.js: 'hour buckets have one key each when the clocks go back, and the plain key on every other hour', 'hour buckets stay on the wall-clock hour where the clocks change by half an hour' (Lord Howe, ranges spanning the change in spring and autumn; seen failing first). An hour 
- Durations mix numbering systems and invoice month keys are not ASCII in locales with their own digits (low). Test: ui/shared/tests/time.test.js: 'a short duration has one numbering system where the language has digits of its own'; ui/shared/tests/invoices.test.js: 'the month keys are plain YYYY-MM where the language has digits of its own' (both seen failing first)
- Recommended tyre pressure reads '42 psi front, 42 psi rear' (low). Test: ui/shared/tests/tyres.test.js: 'front and rear that read the same in the person's unit are said once' (seen failing first). tyresTitle keeps its kPa rule on purpose (each tyre is still judged against the lower axle when the kPa values differ).

### The real server end to end

- A full rebuild cut off by a normal stop (SIGTERM, docker stop) switches every car to a half-made history and reports itself done (high). Test: crates/engine/tests/pipeline.rs: a_full_rebuild_whose_server_is_stopped_leaves_the_history_and_carries_on_at_the_next_start, a_full_rebuild_never_switches_to_a_history_that_lacks_logs_the_one_in_use_has (both failed first: status Done, history lost). Engine::stopping() is called by EngineRun::stop a
- A brand-new server refuses both 'Restore from an export file' and 'Move here' with 409 not_empty (high). Test: Already fixed before this run: crates/web/tests/first_start.rs and main.rs a_server_that_started_once_still_takes_data_in (pass in cargo test -p web / -p sentry-mate). Not re-run with the hunter's fresh-restore.sh against a built binary.
- Instrument, Horizon, Manager and Logbook cannot add, edit or remove an account on the real server (high). Test: ui/tools/account-password.test.js (failed first: no confirm_password field); ui/devserver/tests/test_auth.py test_an_account_change_asks_for_the_admins_own_password_again (failed first). Mock now answers 403 password_needed / wrong_password on POST, PATCH (when role, cars or password change) and DEL
- A new upload token's answer has no `app` block, so the page cannot show the server address or certificate fingerprint (medium). Test: crates/sentry-mate/src/e2e.rs two_seeded_cars_stay_two_cars_and_their_keys_and_tokens_are_served (ignored-by-default slow test, run with --include-ignored and dev-seed: passes). Assertions added with the fix, not seen failing first. certificate_sha256 is /network's colon-separated upper-case form; d
- Event payloads carry drive and charge ids as JSON numbers; imported rows' ids pass 2^53 and lose digits (medium). Test: crates/mqtt/src/render.rs drive_events (with an id over 2^53) and charge_events, failed first; ui/devserver/tests/test_reports.py test_events. Decision taken that the owner may want to review: drive and charge are now text in the MQTT event payload and in /cars/{car}/events (a change for existing MQ
- /setup/status never reports a running import (low). Test: crates/sentry-mate/src/e2e.rs two_seeded_cars... (failed first: import_running false). Fix shape { cause, link: /settings/import, import } as the mock's; contract updated. A watch account sees it too, as on the mock.
- A car that only has imported history is flagged 'no upload' for good (low). Test: crates/web/tests/fleet.rs a_car_that_only_has_imported_history_is_not_called_silent (failed first). Rule: a car that never uploaded is not silent only when it has no CAN key, no live upload token, and has imported or API history; cars with nothing at all stay silent, so the existing pinned assertion
- Fleet says 'No upload for 4 min' for a car flagged because its data is 38 hours old (low). Test: ui/tools/fleet-attention.test.js (new; passes with the fix, not run against the old page). Instrument only.
- Charges for all cars on a server with no car asks for /cars/undefined/... and prints 'undefined' (low). Test: Already fixed in Instrument before this run (charges.js shows 'No car yet, so no fast-charging curve'); checked by reading the code, not re-run. Other packs are archived.
- Import report 'Rows per car' says every TeslaFi poll was imported even when all were skipped (medium). Test: ui/tools/import-report.test.js (seen failing on the old page: 259 to import, 0 skipped); ui/devserver/tests/test_imports.py. The page reads the car's own tables.positions, which the real server already sends; the mock and docs/ui-api.md now carry it. Left as found: the real report's car still says d
- Logbook's ', plugged in' on parked timeline rows never shows on the real server (low). Test: ui/devserver/tests/test_real_fields.py test_a_parked_timeline_row_has_no_field_the_server_does_not_send (failed first). Only the mock-only field was removed; the Logbook pack is archived and untouched.
- Six list resources ignore ?format=csv and Accept: text/csv (low). Test: Already fixed before this run: crates/web/tests/lists_csv.rs and imports_teslafi.rs (import-holds); pass in cargo test -p web.
- No pack offers a way to change one's own password (low). Test: ui/tools/account-password.test.js second test; ui/devserver/tests/test_auth.py OwnPassword (failed first: 404). Adds a 'Change your password' button beside Your sessions in Instrument, PUT /me/password in the mock, and a row for it in docs/ui-api.md. With two-step on the real server answers 401 wron

### Instrument

- Car overview: every live update destroys the focused control, keyboard focus falls to <body> (high). Test: ui/tools/keyboard-focus.test.js: "the car overview's live updates leave the focused control in place"
- Account menu opens but keyboard focus never enters it (149 Tab presses away) (medium). Test: ui/tools/keyboard-focus.test.js: "the account menu takes the focus when it opens, keeps Tab inside and gives the focus back when it closes"
- Failed API reads are shown as true 'there is none' statements on several pages (medium). Test: ui/tools/page-faults.test.js: "a side read that fails is said as not loaded, never shown as 'there is none'" (ten cases; seen failing first on its first case, the others were not each run unfixed). Not covered, left as they were: the swallowed reads on battery, efficiency, health, help, software, th
- Places page sends one GET /places?car= per car (196 extra requests with 200 cars), failures hidden (medium). Test: ui/tools/page-faults.test.js: "Places asks for the drives per car of only the cars that have them, a few at a time" and "Places whose drives of one car cannot be read says so, and reads them again on Try again"; ui/devserver/tests/test_places.py test_counts_for_a_car_without_history (all seen failin
- Connections overview rows for map tiles, address lookup and release check link to anchors that do not exist (medium). Test: ui/tools/overview-links.test.js
- A long car name without spaces makes car pages scroll sideways on a phone (medium). Test: ui/tools/page-faults.test.js: "a car name of one long word stays inside a phone's window on the car's pages"
- With no cars: eight menu items do nothing, and /charges asks for car 'undefined' (medium). Test: ui/tools/page-faults.test.js: "with no car, a menu item of a car's page says there is no car yet, and Charges and Statistics ask for no car" and "before any car was opened, a menu item of a car's page asks which car and then opens that page for it" (both seen failing first)
- Dialogs opened by a button that disables itself first do not return focus on close (low). Test: ui/tools/keyboard-focus.test.js: "a dialog opened by a button that is disabled while its request runs gives the focus back to that button"
- Typing a latitude outside -90..90 in the place editor throws an uncaught error (low). Test: ui/tools/page-faults.test.js: "a latitude or longitude typed out of range in the place editor throws nothing"
- Timeline: filter bar grows 48 px after the page is ready and pushes everything down (low). Test: node ui/tools/check.js --pack instrument\|horizon\|manager --pages timeline --stable 8 --sizes 1440x900 (passes in all three on HEAD)
- Settings: Server export steps overlap and are cut off on a phone at interface size 110 and 125 (low). Test: ui/tools/interface-size.test.js: "on a phone at Interface size 110 and 125 the export file's steps and lines stay inside the card, apart" (seen failing first)
- Travel on a phone at interface size 80: the map legend hangs out of the map over the help text (low). Test: ui/tools/interface-size.test.js: "on a phone at Interface size 80 the Travel map's legend stays inside the map" (seen failing first in Instrument and Manager; Horizon got the same CSS)
- Empty state of a wide table is centred in the full table width and cut off at the edge (low). Test: ui/tools/page-faults.test.js: "the empty note of a wide table sits inside the part of the table that is in view" (seen failing without the fix)
- Interface size 80 on a desktop gives unit text of 8.6 to 9.6 px (low). Test: ui/tools/interface-size.test.js: "at Interface size 80 on a desktop no unit and no code text is under 10 px" (seen failing first). /setup has the same .mono fix in Horizon and Manager but is not in the test.
- /software is 'not found' although the page is registered as working without a car; the not-found screen has no heading (low). Test: ui/tools/page-faults.test.js: "an address that is no page, or no car, has one h1 that says so, and the tab title says it too"
- Dev server: Software's 'Versions of every car' lists a car c4 that does not exist, so its link 404s (low). Test: ui/devserver/tests/test_review.py (walks /api/v1/fleet/versions against /cars); checked by hand on HEAD

### Horizon

- Sign-in and set-up pages show the whole app header on a computer (sections, car switcher, account menu saying 'Signed in', 'Not live' warning) (medium). Test: ui/tools/sign-in.test.js: "the sign-in screen shows its card alone, nothing of the app's menus" (passes on HEAD)
- Interface size 125 % on this device: the pack's own checker fails all 25 routes (sideways scroll on Charges, clipped filter counts on Logbook, clipped route legend, car button icon off centre on every page) (medium). Test: ui/tools/interface-size.test.js (125 cases); node ui/tools/check.js --pack instrument --pixel --device-size 125
- Interface size 80 % on this device: 12 routes fail the checker (text under 10 px, labels cut off on Server and access and Data health on a phone) (low). Test: ui/tools/interface-size.test.js: "on a phone at Interface size 80 and 90 a tag's text, the charging week's labels and Travel's ranks are inside their boxes" (seen failing first); check.js --pixel --device-size 80 and 90 now pass every page
- Default size: logbook, charge, places and settings-integrations fail the pixel checker (low). Test: ui/tools/interface-size.test.js: "on a phone the charge page's Download CSV stands clear of ..."; node ui/tools/check.js --pack instrument --pixel --sizes 1440x900,3840x2160,390x844@3 (passes but for the trip map pin, see gates)
- Places list asks the server once per car (200 cars: 200 parallel /places?car= requests on every load) (medium). Test: ui/tools/page-faults.test.js: "Places asks for the drives per car of only the cars that have them, a few at a time"
- Typing an out-of-range latitude in the place editor throws an uncaught error from the map (low). Test: ui/tools/page-faults.test.js: "a latitude or longitude typed out of range in the place editor throws nothing"
- A malformed #fragment in the address throws an uncaught URIError after every page mount (low). Test: ui/tools/page-faults.test.js: "a #fragment in the address that is not valid percent-encoding throws nothing"
- Ctrl+K opens the car switcher on top of an open 'Find a page' dialog (two modal dialogs stacked); focus is lost after closing them (low). Test: ui/tools/keyboard-focus.test.js: "the car switcher's key opens no second dialog over the page finder, and the focus comes back"
- Keyboard focus drops to the page body after 'New place' and after saving a logbook purpose or a place (low). Test: ui/tools/keyboard-focus.test.js: "opening the place editor, saving a place and saving a drive's purpose leave the focus on the page"
- Signing in drops the #section of the page that was asked for (low). Test: ui/tools/sign-in.test.js: "signing in opens the page that was asked for at the section its address names"
- With 200 cars: Travel's map legend overlaps the map credit and the help text on a phone; the empty Statistics text sticks 87 px out of its box (low). Test: ui/tools/interface-size.test.js (Travel legend); ui/tools/page-faults.test.js: "the empty note of a wide table sits inside the part of the table that is in view"
- Light theme: the 'Preferred' tag and the 'DC' pill are under 4.5:1 contrast (low). Test: ui/tools/contrast.test.js (new; light and dark, measured on screenshots; seen failing first)

### Manager

- A 64-character car name without spaces (allowed) makes almost every car page scroll sideways; a long place name pushes the Drives filter off a 1440 px window (medium). Test: ui/tools/page-faults.test.js: "a car and a place whose names are one long word leave no page wider than the window of a phone / a computer" (20 routes each)
- Failed requests are shown as true-looking empty states ('No route was recorded', 'No charging in this period', 'No rules yet', 'All 0 cars') (medium). Test: ui/tools/page-faults.test.js: "a side read that fails is said as not loaded ..." and "more side reads that fail are said as not loaded: the other cars, a place's name, the battery's totals, a car's name" (seen failing first)
- Charge page on a phone: the 'Download CSV' button covers the 'Power against battery level' heading (low). Test: ui/tools/interface-size.test.js: "on a phone the charge page's Download CSV stands clear of \"Power against battery %\"" at 100, 80 and 125 (seen failing first)
- Interface size on this device 80 % and 125 %: clipped, overlapping and sub-10 px text, and sideways scroll on a phone (medium). Test: ui/tools/interface-size.test.js; check.js --pixel --device-size 80, 90, 110, 125
- Settings: Cars number fields: unparseable input is saved as 'empty', erasing the stored value and saying 'saved' (medium). Test: ui/tools/page-faults.test.js: "a number a field cannot read is said under the field, and the value stored stays" (seen failing first)
- All-cars Charges with no cars asks for /cars/undefined/charging-stats/dc-curve (404) and shows a load error in the curve panel (low). Test: ui/tools/page-faults.test.js: "with no car, a menu item of a car's page says there is no car yet, and Charges and Statistics ask for no car"
- /charges?cars=<unknown id> shows the raw id as heading, 'No charge matches' tiles and a Not found table, with nine 404 requests (low). Test: ui/tools/page-faults.test.js: "Charges and Statistics for cars that are not there say so once, and ask the server for none of them" (seen failing first)
- Places page sends one paged /places?car= request per car (200 cars: 200+ requests on every open) (medium). Test: ui/tools/page-faults.test.js: "Places asks for the drives per car of only the cars that have them, a few at a time"
- Place editor: typing a latitude outside -90..90 throws an uncaught MapLibre error (low). Test: ui/tools/page-faults.test.js: "a latitude or longitude typed out of range in the place editor throws nothing"
- Saving a place without touching its price still asks 'Update past charge costs too? ... would cost differently with this price' (low). Test: ui/tools/page-faults.test.js: "saving a place whose price and circle were not touched asks nothing about past charges" and "a place's price below zero, or above what the server takes, is said under its field and not sent" (both seen failing first)
- A deep link with a section anchor loses the anchor when sign-in is needed (low). Test: ui/tools/sign-in.test.js: "signing in opens the page that was asked for at the section its address names"
- With no cars, the Overview, Timeline, Software, Drives, Trip, Logbook, Battery and Efficiency tabs all link to '/' and silently show All cars (low). Test: ui/tools/page-faults.test.js: "with no car, a menu item of a car's page says there is no car yet ..."
- Page finder: Esc with text typed only clears the text; the hint says 'Esc close' (low). Test: ui/tools/keyboard-focus.test.js: "one Esc closes the page finder with text typed in it" (seen failing first)
- After a failed save in a dialog (and after each set-up step) keyboard focus drops to the page body (low). Test: ui/tools/keyboard-focus.test.js: "a save in a dialog that the server refuses leaves the focus in the dialog, on its button or the wrong field" and "each step of the first-run set-up takes the keyboard focus" (both seen failing first)
- Logbook: the text of a purpose total with no drives has 2.5:1 contrast (light) and 2.9:1 (dark) (low). Test: ui/tools/contrast.test.js (the .lb-total.is-zero rows, both themes; seen failing first)

### Console

- Interface size 125 % (and 110 % at 1280 px): every page scrolls sideways and the link row's settings/help are cut off (medium). Test: ui/tools/interface-size.test.js: on a 1280 px laptop at Interface size 125 ... (seen failing first, each half)
- A 64-character car name (allowed by the form) breaks the frame on desktop and phone (medium). Test: ui/tools/page-faults.test.js (same test)
- In-page anchor links land under the fixed command bar (medium). Test: ui/tools/page-edges.test.js: a link to a section of the open page lands below the top bar (seen failing first)
- Footer "data credits" link points to an anchor that does not exist (low). Test: probe on HEAD: /help#credits exists and is scrolled to; now also in page-edges.test.js (ae66fe10)
- Charges with no car asks for cars/undefined and shows "There is no such car" (low). Test: ui/tools/page-faults.test.js: with no car ... Charges and Statistics ask for no car (already fixed; re-checked on HEAD with --cars 0)
- Trip (and part of Efficiency) fails with "Some values are not valid" for a period that starts in the future or before 1970 (low). Test: ui/tools/page-edges.test.js: a period still to come gives Trip and Efficiency their empty pages (seen failing first)
- A long unbroken word in a filter line makes the page 1000 px wider than the window (low). Test: ui/tools/page-edges.test.js: a search for one very long word that finds nothing leaves the page as wide as the window (seen failing first)
- Places form: typing a latitude outside -90..90 throws an uncaught MapLibre error on every keystroke (low). Test: ui/tools/page-faults.test.js (same test)
- Logbook: rows of the trip log appear and vanish again right after the page is ready (low). Test: ui/shared/tests/vlist.test.js: rows the guessed height drew too many of are taken out in the same step (seen failing first); node ui/tools/check.js --pack instrument --stable 6 --pages logbook --sizes 3840x2160 failed before, passes now
- Dev server: /fleet/versions lists a car that does not exist (and omits the sold car), so Software shows a dead link (low). Test: ui/devserver/tests/test_review.py (HEAD answers c1, c2, c3, c0)
- Dev server: GET /places?car=<synthetic car>&sort=drives answers 500 (low). Test: ui/devserver/tests/test_places.py test_counts_for_a_car_without_history (curl on HEAD: 200)

### Pebble

- On a first visit the dock's Car, Drives and Battery and five More-sheet entries do nothing (medium). Test: ui/tools/page-faults.test.js: before any car was opened, a menu item of a car's page asks which car
- Tapping a page while the server is unreachable replaces the app with the browser's error page (medium). Test: ui/shared/tests/build.test.js: a page file that fails while the server gives no answer...; ui/tools/page-edges.test.js: a page opened while the server is away says so inside the app, and Try again opens it once the server is back (both seen failing first)
- Long car and account names push pages wider than the phone screen (low). Test: ui/tools/page-faults.test.js; my probe at 390 px with 64-letter car and account names found no page wider than the window
- Typing a latitude outside -90..90 in the place editor throws uncaught errors (low). Test: ui/tools/page-faults.test.js: a latitude or longitude typed out of range in the place editor throws nothing
- Logbook fails the stability check: rows flicker on every live rows event (low). Test: ui/shared/tests/vlist.test.js (same test); check.js --stable on logbook at 3840x2160
- Battery page of a car without capacity data shows a broken sentence (low). Test: ui/tools/page-edges.test.js: Battery of a car whose pack has reported nothing writes no sentence around a dash (seen failing first)
- Warning-tone text is below 4.5:1 in the light theme; "unnamed" firmware labels are below it in both themes (medium). Test: ui/tools/contrast.test.js: new parts for a warning, a problem, a link and a good word (seen failing first in the light theme)
- Car switcher says "200 cars" where the header says "199 cars" (low). Test: ui/tools/page-edges.test.js: the car switcher of a large fleet counts the cars not sold and says where the sold ones are (seen failing first)
- Picking a date before 1970 in the period sheet replaces the page with an error (low). Test: ui/shared/tests/time.test.js: a period that starts or ends before 1970, or after 2100, is no period; ui/tools/page-edges.test.js: period test (both seen failing first)
- Signing in loses the #section of the address asked for (low). Test: ui/tools/sign-in.test.js (passes on HEAD)

### Logbook

- A long car name (allowed up to 64 characters) makes pages wider than the window, on desktop too (medium). Test: ui/tools/page-faults.test.js: a car and a place whose names are one long word leave no page wider than the window (re-checked on HEAD at 390 px, also with a 64-letter account name)
- A long place name without spaces overflows Timeline and the place editor on a phone (low). Test: ui/tools/page-faults.test.js (same test)
- Informational text in --ink-4 fails WCAG AA contrast in both themes (2.6:1 light, 3.1:1 dark) (medium). Test: ui/tools/contrast.test.js (same parts)
- /charges with no car requests /api/v1/cars/undefined/charging-stats/dc-curve (404) and titles itself "All 0 cars" (low). Test: ui/tools/page-edges.test.js: Statistics on a server without a car does not count "0 cars" (seen failing first); page-faults.test.js covers the request
- A failed Map / Satellite switch says nothing (low). Test: ui/tools/page-edges.test.js: a Map or Satellite pick that could not be saved is said, and stays for the page (seen failing first)
- Settings: Cars saves a made-up currency ("dollars" becomes DOL) and a sold date in the future (low). Test: ui/tools/page-edges.test.js: Settings: Cars refuses a sold date after today and a currency that is none (seen failing first)
- Pixel check fails at the default size on fleet, charge, battery, travel and settings-integrations (low). Test: node ui/tools/check.js --pack instrument --pixel --pages trip at 390x844@3, 1440, 2560, 3840 and --device-size 80/90/110/125 (failed before, passes now); no node test
- Dev server: GET /api/v1/places?car=c4 answers 500 for a light synthetic car (--cars 200) (low). Test: ui/devserver/tests/test_places.py test_counts_for_a_car_without_history (curl on HEAD: 200)

### Reported from outside the hunt

- Each reason a log was left out has its own word and label (unreadable, other_vin, failed_check) (medium). Test: Reproduced: the engine itself mapped every hole that is not `not_stored` to `failed_check` in three query files, so the UI could not tell them apart. Engine now has one `hole_why()` (crates/engine/src/query/timeline.rs) giving `unreadable` and `other_vin` their own `reason` / `gap_reason` (gap `caus
- A page file that fails while its page is warmed ahead is no longer an uncaught error (medium). Test: The feature works (full load after an update, one retry, then the page_failed box with Reload all passed); the test failed only on its last line, `no uncaught errors`. Cause is in ui/packs/instrument/web/core/router.js `warm()`: it starts the page module import, then awaits the car read, so a failed
- Car and logbook pages keep their 50 ms frame budget while scrolling (budgets untouched) (medium). Test: Reproduced: car failed 4 of 5 runs (frames 83 to 117 ms), logbook 2 of 4 (83 ms) and sat at 50 ms otherwise. Car (d1906e63): c1 is driving, so its marker moved every 2 s and the map was drawn again mid-scroll (about 80 ms on this machine's software GL); the move now waits while the page scrolls or t
- PageRebuild test no longer races its clock (medium). Test: Still flaky on HEAD: 5 of 10 runs failed with `'days' not found in ['decoding', 'done']`. The test raced its clock, not a stream fault: the stream samples job status at most twice a second (less often when a look is slow), and the test's ticking clock passed the 23 virtual seconds of `days` in about
- `check.js --pack instrument --jumps` runs the jump check instead of crashing (medium). Test: Reproduced the TypeError in jumps.js parseArgs: run as the program, check.js set `module.exports` after calling main(), so jumps.js got an empty module. Exports now come before main; a bad jumps option is a usage error (exit 2). Test ui/tools/check-jumps-flag.test.js (README row), failed before and 

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

- Inventory: every route of crates/web (156 route lines) plus crates/sentry-mate/src/api.rs written to (hunt scratch); all attacks ran against the real binary (dev-seed build, 2 seeded cars, TLS) with scripts in that folder (smc.py client, t*.py, fuzz*.py)
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
- Rust tests through web::testkit compile and run against a copy of the tree: (hunt scratch) (3 tests, all fail on the code as found; log in repro/test.log)
- Not tested: behaviour for addresses outside the home network (the sandbox has only loopback addresses), offline map tiles with a real .pmtiles file, a real move between two servers, Tesla/TeslaFi/TeslaMate inputs against live services

### Inputs and ingest

- Scratch harness: a cargo workspace outside the repo with path deps on the crates ((hunt scratch), run with `cargo test --offline --test <name> -- --nocapture`); inventory at (hunt scratch); no tracked file changed, git status clean
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

- Page checker --pixel at 390x844@3, 1440x900, 2560x1440, 3840x2160: 22 of 25 routes pass; the 3 fails are sub-pixel only (logbook baselines 1 px, places border fraction, settings-server step number 0.66 px). Log: (hunt scratch)
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

- Checker, every page, --pixel at 390x844@3/1440x900/2560x1440/3840x2160: 20 of 25 routes pass (logs: (hunt scratch)*.log); the 5 failures are in the findings or are 1 px nits (logbook totals baseline, integrations ov-err icon, settings-server step numbers, places action buttons on fractional pixels)
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

- Checker, plain: `check.js --pack console --pixel --sizes 390x844@3,1440x900,2560x1440,3840x2160` passes every page (exit 0); with `--latency-ms 400` also passes. Logs: (hunt scratch)*.log
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

- Checker baseline: check.js --pack pebble --pixel at 390x844@3, 1440x900, 2560x1440, 3840x2160 passes 25 of 25; --hover passes 25 of 25; --device-size 80 passes; --server-args "--latency-ms 400" passes (outputs in (hunt scratch)*.txt)
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

- Checker --pixel at 390x844@3, 1440x900, 2560x1440, 3840x2160: 20 of 25 routes pass (failures reported below); output in (hunt scratch)
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
