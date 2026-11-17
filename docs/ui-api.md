# Sentry Mate API: what the pages need

This is the `/api/v1` the pages in [`parity/ui-parity.md`](parity/ui-parity.md) need. It merges the needs of every page into one set of resources, without duplicates. Each resource names the pages that use it.

## Conventions (every endpoint)

- **Ids are strings.** Car, drive, charge, state, update, place, visit, token, key and import ids are all strings. Drive and charge ids pass 2^53, so a JSON number would lose digits.
- **Units are metric, named in the field.** `_m`, `_km`, `_kph`, `_kw`, `_kwh`, `_wh_per_km`, `_c` (degrees Celsius), `_kpa` (tyre pressure; stored bar x 100), `_v`, `_a`, `_pct`, `_ms` (durations and times). The API never converts. The UI applies the person's unit preferences.
- **Times** are milliseconds since the epoch, UTC (`*_ms`). Every car-scoped response also carries the car's `time_zone`, and anything grouped by day, week, month or year is grouped in that zone (ISO weeks).
- **Lists are paged by cursor.** `limit` (default 50, max 500) and `after` (an opaque string from the previous page's `next`). A list response is `{ items: [...], next: string | null }`. Optional `total` only where it is cheap.
- **Period filter** on every time-based resource: `from_ms` and `to_ms`. A row belongs to the period by its start time unless the resource says otherwise. Rows that cross an edge are clipped where noted, never dropped.
- **Several cars:** report resources accept `cars=all` or `cars=1,7` where the page offers it; each row then has `car`.
- **Access:** every car-scoped route checks the account's car list. An unknown car and a car the account may not see give the same answer: `404 { error: { code: "no_such_car" } }`.
- **Errors** use one shape: `{ error: { code, message, fields? } }`. `400` bad input (with `fields`), `401` not signed in, `403` not allowed (role), `404` unknown or hidden, `409` conflict (stale `version`), `422` invalid value, `429` with `Retry-After`.
- **Changes** need the CSRF header, are recorded in the audit log, and admin-only changes say so below.
- **Source:** rows carry `source: "can" | "teslamate"`. Estimated values carry a `basis` (for example `energy_basis: "measured" | "estimated"`).
- **Null means unknown.** A missing value is `null`, never `0`. Live values carry their own `at_ms`.
- **CSV:** every list resource also answers `Accept: text/csv` or `?format=csv` with the same filters.

## Sign-in, accounts and preferences

| Resource | Methods | Fields and notes | Pages |
|---|---|---|---|
| `/api/v1/setup` | GET, POST | GET `{ needs_setup }`. POST `{ code, name, password }` once, with the one-time set-up code. | App frame |
| `/api/v1/session` | POST, DELETE | POST `{ name, password }` -> `{ status: "done" \| "two_step_pending", pending? }`. Same answer for an unknown name. Throttled. | App frame |
| `/api/v1/session/two-step` | POST | `{ pending, code }` | App frame |
| `/api/v1/me` | GET | `{ id, name, role: "admin" \| "watch", cars: [id] }` | App frame |
| `/api/v1/me/sessions[/{session}]` | GET, DELETE | `{ id, created_ms, last_seen_ms, this_one }`; delete one or all. | Settings: Server and access |
| `/api/v1/me/preferences` | GET, PATCH | `{ units: { distance: "km" \| "mi", temperature: "c" \| "f", pressure: "bar" \| "psi" }, range: "rated" \| "ideal", theme: "light" \| "dark" \| "device", language, interface_size, ui_pack, car_order: [id], pinned_cars: [id] }`. Units default from the server settings, which can default from the car's own distance unit. | Every page, Fleet, Settings: Display and units |
| `/api/v1/accounts[/{account}]` | GET, POST, PATCH, DELETE (admin) | `{ id, name, role, cars: [id] \| "all", two_step_required, created_ms }` | Settings: Server and access |
| `/api/v1/audit` | GET (admin) | `{ items: [{ at_ms, account, action, target, summary }], next }` | Settings: Server and access |

## Fleet and cars

| Resource | Methods | Fields and notes | Pages |
|---|---|---|---|
| `/api/v1/cars` | GET | `?search=&state=&sort=last_activity\|name\|soc&dir=&limit=&after=`. Item: `{ id, name, model, year, vin_prefix, trim, profile, time_zone, last_activity_ms, now: { state, since_ms, at_ms, received_ms, stale, soc_pct, range_km, place: { id, name } \| null, plugged, charge_power_kw }, attention: [code] }`. Read from the fleet index; no car store is opened. | Fleet, every car picker |
| `/api/v1/fleet/summary` | GET | `{ cars, by_state: { driving, charging, parked, asleep, unknown }, stale }` | Fleet |
| `/api/v1/fleet/attention` | GET | `?limit=&after=` -> `{ items: [{ car, code: "stopped_mid_drive" \| "stopped_mid_charge" \| "upload_silence" \| "trust_failed" \| "low_battery" \| "tyre_low" \| "door_open_asleep", since_ms, detail }], next }` | Fleet |
| `/api/v1/cars/{car}` | GET, PATCH | GET: the list item plus `config: { trim, drivetrain, performance_package, pack_kwh_code, wheel, paint, roof, spoiler, right_hand_drive, map_region, supercharging_access }`, each `{ value, source: "can" \| "vin" \| "owner" \| "teslamate" }`, and `version`. PATCH (admin) with `version`: `name, time_zone, config overrides`. | Car overview, Settings: Cars |
| `/api/v1/cars/{car}/settings` | GET, PATCH (admin) | `{ version, battery_chemistry: { setting: "auto" \| "lfp" \| "nca_nmc", detected, evidence }, free_supercharging: [{ from_date, to_date }], capacity_new_kwh, full_range_new_km, warranty_threshold_pct, efficiency_wh_per_km_override, software_version_typed }`. Field errors on 422. | Settings: Cars, Battery, Software |
| `/api/v1/cars/{car}/upload-tokens[/{token}]` | GET, POST, DELETE (admin) | `{ id, created_ms, revoked, key: string \| null, last_upload_ms }`. The secret is returned once, on POST. | Settings: Cars |
| `/api/v1/can-keys` | GET (admin) | `?assigned=false` -> `{ items: [{ key, first_seen_ms, last_upload_ms, logs_waiting, car: id \| null, preferred }], next }` | Settings: Cars, Fleet (empty state) |
| `/api/v1/can-keys/{key}` | PATCH (admin) | `{ car, preferred }` assigns a waiting key or sets the preferred key. | Settings: Cars |
| `/api/v1/setup/status` | GET | `{ cars, tokens: { count, first_used_ms }, keys_waiting, logs_waiting_trust, import_running }` with a fix link per cause. | Fleet (empty state) |

## Live values

| Resource | Methods | Fields and notes | Pages |
|---|---|---|---|
| `/api/v1/cars/{car}/now` | GET | One object; every value `{ value, at_ms }` or null. `state, since_ms, received_ms, delay_s, buses: [{ bus, awake }], driving, gear, speed_kph, power_kw, soc_pct, soc_bms_pct, usable_soc_pct, range_km, ideal_range_km, est_range_km + est_basis, energy_remaining_kwh, odometer_km, outside_c, cabin_c, battery_c, setpoint_left_c, setpoint_right_c (LO/HI as strings), lv_v, location: { lat, lon, accuracy_m, elevation_m, heading_deg, place, address }, tyres_kpa: [fl, fr, rl, rr], tyres_slots_mapped, tyre_recommended_kpa, flags: { sentry, sentry_event_guess, service_mode, user_present, keep_climate: "off" \| "on" \| "dog" \| "camp", climate: "off" \| "on" \| "precondition" \| "overheat_protect", battery_preconditioning, keep_warm, accessory_power, parking_brake, reduced_range, doors: { fl, fr, rl, rr }, trunk, frunk, liftgate_deg, charge_port_open, display_on }, charge: { status, plugged, kind: "ac" \| "dc" \| null, power_kw, volts_v, amps_a, phases, pilot_a, limit_source, added_kwh, limit_pct, to_full_kwh, to_full_min, finish_ms, charge_id }, drive: { drive_id, start_ms, distance_km, energy_kwh }, awake_reasons: [code], software: { version, version_source, version_since_ms, changed_since_ms }`. | Car overview, Fleet |
| `/api/v1/events` | GET (server-sent events) | Events: `now` `{ car, now }` (delta), `fleet` (car added, removed, renamed), `rows` `{ car, kinds: [drives\|charges\|updates\|visits\|states], from_ms, to_ms }` after a rebuild, `prefs`, `notice`. Filtered by the account's cars. | Every page |
| `/api/v1/cars/{car}/awake-reasons` | GET | `?from_ms=&to_ms=&limit=&after=` -> awake spans `{ start_ms, end_ms, buses, reasons: [sentry \| keep_climate_on \| keep_climate_dog \| keep_climate_camp \| user_present \| preconditioning_cabin \| preconditioning_battery \| charging \| keep_warm \| door_open \| accessory_power], avg_power_kw }` | Car overview, Timeline, Battery |

## Time series

| Resource | Methods | Fields and notes | Pages |
|---|---|---|---|
| `/api/v1/cars/{car}/series` | GET | `?fields=soc_pct,usable_soc_pct,range_km,ideal_range_km,energy_remaining_kwh,speed_kph,power_kw,odometer_km,outside_c,cabin_c,battery_c,setpoint_left_c,elevation_m,tyre_fl_kpa,...,lat,lon,accuracy_m&from_ms=&to_ms=&max_points=&agg=last,min,max,mean,p15,p85&source=can\|teslamate\|all`. Columns: `{ t_ms: [], values: { field: [] }, bucket_ms, coverage_pct: [] }`. Downsampling keeps minima and maxima. `null` where there was no sample, and a parallel `state: []` (driving, charging, parked, asleep, unknown) so charts draw gaps instead of holding values. | Car overview, Drive detail, Trip, Battery, Efficiency and temperatures |
| `/api/v1/cars/{car}/signals/latest` | GET | `?search=&limit=&after=` -> `{ items: [{ series, message_id: "0x243", bus, value, unit, value_name, comment, at_ms }], next }` from the newest log | Data health and storage |
| `/api/v1/cars/{car}/signals` | GET | `?series=a,b&from_ms=&to_ms=` -> columns; CSV available. | Data health and storage |

## Timeline and states

| Resource | Methods | Fields and notes | Pages |
|---|---|---|---|
| `/api/v1/cars/{car}/timeline` | GET | `?from_ms=&to_ms=&kinds=drive,charge,parked,gap,update&place=&q=&limit=&after=`, newest first. Row: `{ id: "<kind>:<start_ms>", kind, start_ms, end_ms, start: { place, address, lat, lon }, end: {...} \| null, odometer_km, distance_km, duration_ms, soc_end_pct, soc_delta_pct, range_end_km, range_delta_km, energy_delta_kwh, energy_basis, outside_avg_c, cost: { amount, currency } \| null, ref: { drive \| charge \| update }, parked_ms: { asleep, awake, awake_vehicle_bus_only, unknown }, awake_reasons, gap_reason: "no_log" \| "not_recorded" \| "failed_check" \| "dropped_frames" \| null, source }`. `q` only hides rows. | Timeline, Trip |
| `/api/v1/cars/{car}/states` | GET | `?from_ms=&to_ms=&limit=&after=` -> `{ items: [{ id, state: "online" \| "asleep" \| "unknown", start_ms, end_ms, buses: [], source }], next, current: { state, since_ms, at_ms }, last_change_ms, share_pct: { parked, asleep, awake, unknown } }` | Timeline |
| `/api/v1/cars/{car}/activity` | GET | `?from_ms=&to_ms=` -> segments for the strip `[{ kind: "driving" \| "charging" \| "online" \| "online_vehicle_bus_only" \| "asleep" \| "unknown" \| "updating", start_ms, end_ms, ref, source }]` | Car overview, Timeline, Trip |

## Drives

| Resource | Methods | Fields and notes | Pages |
|---|---|---|---|
| `/api/v1/cars/{car}/drives` | GET | `?from_ms=&to_ms=&places=&place_side=start\|end\|any&q=&min_distance_km=&min_avg_kph=&source=&flags=gap,in_progress,reduced_range&efficiency=plain\|hill_corrected&sort=start\|distance\|duration\|energy\|wh_per_km\|speed_max&dir=&limit=&after=` -> `{ items: [Drive], next, summary: { count, distance_km, duration_ms, energy_kwh, energy_measured_kwh, energy_estimated_kwh, wh_per_km, without_energy } }`. A bad filter value -> 400 with `fields`. | Drives, Trip, Logbook |
| Drive | | `{ id, car, start_ms, end_ms, duration_ms, start: { lat, lon, place, address, label }, end: {...}, start_odometer_km, end_odometer_km, distance_km, speed_avg_kph, speed_max_kph, power_max_kw, power_min_kw, energy_kwh, energy_basis, regen_kwh, wh_per_km, efficiency_pct, start_soc_pct, end_soc_pct, start_range_km, end_range_km, outside_avg_c, cabin_avg_c, climb_m, descent_m, hvac_kwh, source, flags: { in_progress, gap_ms, lost_frames, reduced_range }, purpose }` | |
| `/api/v1/cars/{car}/drives/{drive}` | GET | Drive plus `prev, next, speed_bands: [{ from_kph, to_kph, seconds }], gaps: [{ start_ms, end_ms, kind }], rated_wh_per_km` | Drive detail |
| `/api/v1/cars/{car}/drives/{drive}/track` | GET | `?format=geojson\|gpx&simplify_m=&extensions=speed,power,soc,accuracy` (GPX needs the export permission). 1 Hz points with `t_ms`, elevation and accuracy. | Drive detail, Travel |
| `/api/v1/cars/{car}/drives/{drive}/purpose` | PUT, DELETE | `{ purpose: "business" \| "private" \| "commute", relation, note }`; kept apart from rebuilt rows. | Logbook |
| `/api/v1/cars/{car}/purpose-rules` | GET, POST, DELETE | `{ id, from_place, to_place, days, purpose }` | Logbook |
| `/api/v1/cars/{car}/drive-stats` | GET | `?from_ms=&to_ms=&exclude_places=` -> `{ count, distance_km, energy_kwh, median_distance_km, per_day: [{ date, count, distance_km, energy_kwh }], max_speed: { period, last_30d, last_7d } each { kph, drive }, speed_bands: [{ from_kph, to_kph, seconds, share_pct }], projection: { per_month_km, per_year_km, basis_days }, top_destinations: [{ place, label, count }] }` | Drives |
| `/api/v1/cars/{car}/logbook` | GET | `?from_ms=&to_ms=&format=json\|csv\|pdf` -> `{ rows: [{ seq, drive, start_ms, end_ms, start_odometer_km, end_odometer_km, distance_km, start_address, end_address, start_place, end_place, duration_ms, purpose, note, continuity_gap_km }], totals_by_purpose }` | Logbook |
| `/api/v1/cars/{car}/trip` | GET | `?from_ms=&to_ms=` -> `{ distance: { odometer_km, drives_km }, time_ms: { driving, charging_ac, charging_dc, parked_awake, asleep, not_recorded }, speed_kph: { moving, incl_dc }, energy_kwh: { drive_net, gross, added_ac, added_dc }, wh_per_km: { net, gross }, cost: { by_currency: [{ currency, amount }], per_100km, priced, unpriced }, markers: [{ kind: "start" \| "end" \| "charge" \| "overnight", lat, lon, t_ms }] }`. Drives and charges come from their own lists with the same period. | Trip |
| `/api/v1/trips[/{trip}]` | GET, POST, PATCH, DELETE | Saved trips `{ id, car, from_ms, to_ms, name }` | Trip |

## Charges

| Resource | Methods | Fields and notes | Pages |
|---|---|---|---|
| `/api/v1/charges` | GET | `?cars=all\|ids&from_ms=&to_ms=&places=&place_kind=&kind=ac\|dc\|unknown&q=&min_cost=&include_unpriced=&min_duration_min=&source=&flags=gap,in_progress&sort=&dir=&limit=&after=` -> `{ items: [Charge], next, totals: { count, energy_added_kwh, energy_grid_kwh, energy_grid_known, dc_inlet_kwh, cost_by_currency, unpriced, avg_duration_ms } }`. `/api/v1/cars/{car}/charges` is the same for one car. | Charges, Trip, Statistics |
| Charge | | `{ id, car, start_ms, end_ms, charging_ms, plugged_ms, kind, fast_charger: { type, brand, class, billed_kwh } \| null, place: { id, name, kind } \| null, address: { short, locality, region, country_code }, position: { lat, lon, accuracy_m }, odometer_km, energy_added_kwh, energy_grid_kwh, dc_inlet_kwh, losses_kwh, efficiency_pct, cost: { amount, currency, source: "tariff" \| "manual" \| "free_supercharging" \| "teslamate" } \| null, price_per_kwh: { value, basis: "grid" \| "pack" } \| null, start_soc_pct, end_soc_pct, start_range_km, end_range_km, range_added_km, range_per_hour_km (range added per hour of charging), power_max_kw, power_avg_kw, outside_avg_c, battery_avg_c, source, has_gaps }` | |
| `/api/v1/cars/{car}/charges/{charge}` | GET | Charge plus `prev, next, cost_breakdown: { parts: [{ rate, per_kwh, kwh, cost }], session_fee, total, gap_ms, mode }, gaps: [{ start_ms, end_ms, kind: "unknown" \| "dropped" \| "chain" }]` | Charge detail |
| `/api/v1/cars/{car}/charges/{charge}/curve` | GET | `?fields=power_kw,volts_v,amps_a,soc_pct,usable_soc_pct,battery_c,range_km,outside_c,phases,pilot_a,ac_line_kw,heater_kw,dc_volts_v,dc_amps_a` -> columns at 1 Hz | Charge detail, Car overview |
| `/api/v1/cars/{car}/charges/{charge}/cost` | PUT, DELETE (admin) | `{ mode: "total" \| "per_kwh" \| "per_minute", amount, currency, energy_basis: "grid" \| "pack" }`; survives rebuilds. DELETE goes back to the place price. | Charge detail, Charges, Trip |
| `/api/v1/charging-stats` | GET | `?cars=&from_ms=&to_ms=&places=&min_duration_min=` -> `{ count, energy_added_kwh, by_kind: { ac, dc, unknown } each { kwh, ms }, cost_by_currency, price_per_kwh: { all, ac, dc, basis, priced }, supercharger_cost, cost_per_100km: { value, method }, start_bands: [{ band, count }], end_bands, heatmap: { start: [], end: [] }, runs: [{ charges, start_ms, min_start_soc_pct, max_end_soc_pct }], share_above_80_pct, share_below_20_pct }` | Charges |
| `/api/v1/charging-stats/places` | GET | `?cars=&from_ms=&to_ms=&sort=energy\|cost&limit=&after=` -> `{ items: [{ place, label, lat, lon, sessions, energy_added_kwh, cost_by_currency, price_per_kwh }], next }` | Charges |
| `/api/v1/cars/{car}/charging-stats/dc-curve` | GET | `?from_ms=&to_ms=&places=` -> `{ median: [{ soc_pct, power_kw }], sessions: [{ charge, label, start_ms, points: [{ soc_pct, power_kw, battery_c }] }] }` | Charges, Charge detail |

## Battery and efficiency

| Resource | Methods | Fields and notes | Pages |
|---|---|---|---|
| `/api/v1/cars/{car}/battery` | GET | `{ as_of_ms, capacity: { full_pack_kwh, buffer_kwh, usable_kwh, spread_kwh, readings, new_kwh, new_source: "owner" \| "early_bms" \| "imported_charges" \| "pack_code", difference_kwh, degradation_pct, health_pct, warranty_pct }, range: { full_now_km, full_now_from_energy_km, full_new_km, new_source, lost_km }, energy_now_kwh, soc_pct, usable_soc_pct, band: { low_pct, high_pct, charge_limit_pct }, chemistry: { value, source }, brick_spread_v, lifetime: { charged_kwh, discharged_kwh, ac_kwh, dc_kwh, regen_kwh, drive_kwh, equivalent_cycles }, notes: [code] }` | Battery |
| `/api/v1/cars/{car}/battery/capacity` | GET | `?from_ms=&to_ms=&group=day\|charge\|month&x=time\|odometer&limit=&after=` -> `{ items: [{ t_ms, odometer_km, full_pack_kwh, usable_kwh, source, method: "bms" \| "charge", charge }], trend: { slope_kwh_per_10000_km, projection }, next }` | Battery, Software |
| `/api/v1/cars/{car}/battery-level` | GET | `?from_ms=&to_ms=&bucket=hour\|day&moving=true\|false&window=7d\|30d` -> `{ buckets: [{ t_ms, soc_pct, usable_soc_pct }], moving: { mean, median, p7_5, p92_5 }, spans: [{ state: "asleep" \| "unknown", start_ms, end_ms }], band, charge_limit: [], share_above_80_pct, share_below_20_pct, stats }` | Battery |
| `/api/v1/cars/{car}/projected-range` | GET | `?from_ms=&to_ms=&bucket=` -> columns `t_ms, full_range_km, full_range_from_energy_km, full_range_usable_km, soc_pct, soc_bms_pct, odometer_km, outside_c, battery_c, state` | Battery |
| `/api/v1/cars/{car}/parked` | GET | `?from_ms=&to_ms=&min_hours=&limit=&after=` -> `{ items: [{ id, start_ms, end_ms, asleep_ms, awake_ms, awake_vehicle_bus_only_ms, unknown_ms, soc_start_pct, soc_end_pct, range_start_km, range_end_km, energy_kwh, energy_method: "counter" \| "bms_energy" \| "range", avg_power_w, awake_avg_power_w, battery_min_c, outside_avg_c, cold, place, address, awake_reasons }], summary: { spells, energy_kwh, kwh_per_day, cost }, next }`; spells crossing the edges are clipped. | Battery |
| `/api/v1/cars/{car}/efficiency` | GET | `?from_ms=&to_ms=&min_distance_km=&temp_group_c=5` -> `{ net_wh_per_km, gross_wh_per_km, parked_kwh, distance_km, drives, estimated_drives, by_temperature: [{ temp_c, drives, distance_km, energy_kwh, wh_per_km, range_efficiency_pct, avg_kph }], no_temperature: { drives, distance_km }, factor: { rated_wh_per_km, ideal_wh_per_km, source, readings, as_of_ms } }` | Efficiency and temperatures |
| `/api/v1/cars/{car}/temperatures` | GET | `?from_ms=&to_ms=` -> `{ outside, cabin, battery } each { min_c, avg_c, max_c, coverage_pct }, missing_ms: { asleep, unknown }, daily: [{ date, min_c, p15_c, p85_c, max_c, coverage_pct }]` | Efficiency and temperatures |

## Reports

| Resource | Methods | Fields and notes | Pages |
|---|---|---|---|
| `/api/v1/statistics` | GET | `?cars=&period=day\|week\|month\|year\|all&from_ms=&to_ms=&limit=&after=` (`all`: the whole period as one row) -> rows `{ car, period_start_ms, period_end_ms, label, time_driven_ms, drives, distance_odometer_km, distance_logged_km, outside_avg_c, driving_efficiency_pct, energy_added_kwh, energy_grid_kwh, charges, energy_per_charge_kwh, cost_by_currency, price_per_kwh, cost_per_100km, net_wh_per_km, gross_wh_per_km, overhead_pct, completeness: { recorded_pct, unknown_ms, dropped_stretches, open_drive, open_charge, teslamate_rows } }`. `/api/v1/cars/{car}/statistics` for one car. | Statistics, Car overview |
| `/api/v1/cars/{car}/mileage` | GET | `?from_ms=&to_ms=&group=day\|week\|month\|year` -> `{ items: [{ period_start_ms, distance_km, odometer_start_km, odometer_end_km, logged_km, not_logged_km }], projection: { km_per_year } }` | Statistics, Battery |
| `/api/v1/cars/{car}/totals` | GET | `?source=can\|teslamate\|all` -> whole-history totals from the fleet summary: `{ drives, distance_km, odometer_first_km, odometer_last_km, odometer_last_ms, not_logged_km, charges, energy_added_kwh, energy_grid_kwh, grid_coverage_pct, charging_efficiency_pct, ac: { charges, kwh }, dc: { charges, kwh }, updates }` | Battery, Data health and storage |
| `/api/v1/cars/{car}/gaps` | GET | `?from_ms=&to_ms=&limit=&after=` -> `{ items: [{ id, start_ms, end_ms, odometer_jump_km, from_place, to_place, cause: "no_log" \| "not_recorded" \| "chain_gap" \| "unknown" }], next }` | Battery, Drives, Charges, Data health and storage |
| `/api/v1/compare` | GET | `?cars=1,7&metric=capacity\|full_range\|wh_per_km\|odometer&from_ms=&to_ms=` -> one series per car | Battery, Efficiency and temperatures, Trip |
| `/api/v1/visits` | GET | `?cars=&from_ms=&to_ms=&q=&place=&country=&region=&city=&sort=arrived_desc&limit=&after=` -> `{ items: [{ id, car, arrived_ms, left_ms, lat, lon, accuracy_m, place, address: { short, street, neighbourhood, locality, region, country_code, country, source }, city_key, drive_in, drive_out, charges: [id] }], next }` | Travel |
| `/api/v1/visits/summary` | GET | same filters -> `{ places, cities, regions, countries, days, first_ms, last_ms, unnamed_stops, distance_km, energy_added_kwh, energy_grid_kwh, efficiency: { value, charges_counted }, cost_by_currency, charges_without_cost }` | Travel |
| `/api/v1/visits/places`, `/visits/cities`, `/visits/regions`, `/visits/countries` | GET | Same filters as `/visits` (including `country=` and `region=`). Grouped rows `{ key, label, place?, locality?, region?, country_code?, lat, lon, visits, places, days, dwell_ms, first_ms, last_ms, cars }`, paged | Travel |
| `/api/v1/visits/days` | GET | `?cars=&city=&from_ms=&to_ms=` -> `{ items: [{ date, car, first_ms, last_ms, drives: [id], charges: [id], places: [label] }], next }` | Travel |
| `/api/v1/tracks` | GET | `?cars=&from_ms=&to_ms=&max_points=` -> `{ drives: [{ car, drive, t_ms: [], lat: [], lon: [], accuracy_m: [] }] }`, simplified on the server | Travel, Trip |
| `/api/v1/cars/{car}/updates` | GET | `?from_ms=&to_ms=&limit=&after=` -> `{ items: [{ id, start_ms, end_ms, duration_ms, since_previous_ms, version, version_source: "teslamate" \| "owner" \| "build_match" \| null, detected_by: "ota_status" \| "build_change" \| "teslamate", changed_modules: ["bms", "vcfront", "di_front", "di_rear", "fc"], charges, full_range_km, full_pack_kwh, source }], next, summary: { count, median_gap_ms, current_version, current_version_source, current_version_since_ms, build_changed_since } }` | Software, Timeline |
| `/api/v1/cars/{car}/updates/{update}` | PATCH (admin) | `{ version }`; also remembers the build-id set for that version. | Software |
| `/api/v1/fleet/versions` | GET | `?search=&limit=&after=` -> `{ items: [{ car, version, version_source, build_changed_since, last_update_ms }], next }` | Software |
| `/api/v1/cars/{car}/events` | GET | `?types=drive_started,drive_ended,charge_started,charge_ended,software_changed,user_present&after=` -> the same JSON as the MQTT events, paged | Settings: Integrations |

## Places

| Resource | Methods | Fields and notes | Pages |
|---|---|---|---|
| `/api/v1/places[/{place}]` | GET, POST, PATCH, DELETE (admin for changes) | `?search=&kind=&sort=name&limit=&after=`. `{ id, name, kind: "home" \| "work" \| "charger" \| "service" \| "other", lat, lon, radius_m, time_zone, price: { currency, per_kwh, per_minute, rates: [{ name, per_kwh, per_minute, days, from, to }], session_fee } \| null, counts: { drives, charges, visits }, last_visit_ms, version }`. Field errors on 422. | Places, Charges, Travel |
| `/api/v1/places/preview` | POST | A draft place, or `{ place, delete: true }` -> `{ drives_named, drives_unnamed, charges_named, charges_repriced, examples: [id] }` | Places |
| `/api/v1/places/{place}/reprice` | POST (admin) | `{ from_ms, to_ms, only_without_cost, dry_run }` | Places |
| `/api/v1/places/suggestions` | GET | `?limit=` -> where the cars park most, with a position and visit count | Places, Fleet |
| `/api/v1/places/at` | GET | `?lat=&lon=` -> the matching place by the nearest-centre rule | Places, Timeline, Drives, Charges |

## Integrations

| Resource | Methods | Fields and notes | Pages |
|---|---|---|---|
| `/api/v1/settings/mqtt` | GET, PUT (admin) | `{ enabled, host, port, username, password (write-only), tls, pin_sha256, namespace, publish_location, ha_discovery: { enabled, prefix, url }, no_place_text, version }` | Settings: Integrations |
| `/api/v1/settings/mqtt/status` | GET | `{ connected, delivered, waiting, dropped, last_error, last_error_ms }` | Settings: Integrations |
| `/api/v1/settings/mqtt/preview` | GET | `?car=` -> `{ messages: [{ topic, payload, retained }], ha_entities: [{ id, name, unit, device_class }] }` | Settings: Integrations |
| `/api/v1/settings/mqtt/test` | POST (admin) | Connects and publishes one test message; returns the result. | Settings: Integrations |
| `/api/v1/network` | GET (admin) | `{ outbound: [{ purpose: "geocoding" \| "tiles" \| "geonames_download" \| "mqtt" \| "release_check", host, enabled, last_used_ms }], inbound: [{ purpose: "upload" \| "web", port }] }` | Settings: Integrations |

## Server, settings and health

| Resource | Methods | Fields and notes | Pages |
|---|---|---|---|
| `/api/v1/settings` | GET, PATCH (admin) | Server defaults: `{ units, range, car_order, geocode: { mode: "offline" \| "online", dataset, url, language, rounding_m }, tiles: { mode, online_url, attribution, max_zoom }, release_check: { enabled }, outside_links: { maps, release_notes }, host_names, version }` | Settings: Display and units, Settings: Server and access |
| `/api/v1/maps/config` | GET | `{ tiles: { mode, offline_url, online_template, attribution, max_zoom }, geocoding: { mode, credit, offline_data_present } }`; offline PMTiles are served by Sentry Mate. | Every map |
| `/api/v1/geocode/status` | GET | `{ mode, offline_data, cache_size, online: { last_error, retry_ms } }` | Settings: Display and units, Data health and storage |
| `/api/v1/notices` | GET | `{ items: [{ code, level, message, since_ms, fix_link }] }`: vault, certificate, recovery key file, opt-in release notice | App frame |
| `/api/v1/about` | GET | `{ version, api_version, ui_pack: { name, version, api }, licences: [], data_credits: [] }` | Help and about |
| `/api/v1/cars/{car}/health` | GET | `{ healthy, reasons: [code], last_upload_ms, last_log_trust: { result: "verified" \| "unsigned" \| "failed", reason }, keys: [{ key, last_upload_ms }], chain_gaps: [{ from_ms, to_ms }], dropped_frames: [{ start_ms, end_ms }], unknown_spans, logs_waiting, rebuild_pending, uploads_enabled }`; the same rule feeds MQTT `healthy`. | Data health and storage, Car overview, Fleet |
| `/api/v1/cars/{car}/rebuild` | POST (admin) | `{ from_ms, to_ms }` rebuilds drives, charges, states and visits from stored logs | Data health and storage, Drives, Charges |
| `/api/v1/cars/{car}/overlaps` | GET, PATCH (admin) | Spans where imported and CAN history both exist: `{ items: [{ start_ms, end_ms, shown: "can" \| "teslamate" }], next }`; PATCH flips one. | Import, Timeline |
| `/api/v1/cars/{car}/storage` | GET | Per-kind bytes and paged days `[{ series, source, date, bytes }]` | Data health and storage |
| `/api/v1/system/storage` | GET (admin) | `{ kinds: [{ kind, label, bytes }], total_bytes, free_bytes, growth: [{ date, bytes }], full_in_days }` | Data health and storage |
| `/api/v1/system/health` | GET (admin) | `{ read_ms, uptime_ms, versions: { server, api, db_format, timeline_rules, import_mapping }, db: { cars, fleet_bytes, open_cars, open_bytes, cars_opened, tables_read }, ingest: { memory, cached_cars }, geocoder, housekeeping_last_ms, timings_ms: { decode_per_log, rebuild_per_window, api_p50, api_p95 } }` | Data health and storage |
| `/health` | GET, no sign-in | `200` with no body while serving; reveals nothing. | Container checks |

## Import

| Resource | Methods | Fields and notes | Pages |
|---|---|---|---|
| `/api/v1/imports` | POST (admin) | Starts a resumable upload of a TeslaMate backup (plain, gzip or xz). | Import |
| `/api/v1/imports/{import}` | PATCH, GET, DELETE (admin) | PATCH sends a chunk with `Upload-Offset`, `Upload-Length`, `Upload-SHA256`. GET -> `{ status: "uploading" \| "checking" \| "importing" \| "done" \| "failed", pass, bytes_read, bytes_total, error: { code, message }, report: { tables: { name: { read, imported, skipped: { reason: n }, bad_values: { column: n } } }, span } }`. DELETE discards. | Import |
| `/api/v1/imports/{import}/cars` | GET, PUT (admin) | `[{ tm_car, vin_prefix, match: car \| "new" \| "leave_out" }]` | Import |
| `/api/v1/imports/{import}/start` | POST (admin) | Starts the import after the whole file has been checked. | Import |

## Deliberately absent

- No endpoint wakes the car, pauses logging or sends a command: Sentry Mate is read-only to the car.
- No Tesla account, token or vehicle-list endpoint.
- No raw SQL, database tuning or index statistics.
- No VIN serial anywhere: only the VIN prefix is stored.
