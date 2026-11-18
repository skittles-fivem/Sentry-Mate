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
| `/api/v1/cars/{car}/signals/latest` | GET | `?search=&limit=&after=` -> `{ items: [{ series, message_id: "0x243", bus, value, unit, value_name, comment, at_ms, has_series }], next }` from the newest log | Data health and storage |
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
| `/api/v1/cars/{car}/purpose-rules[/{rule}]` | GET, POST, DELETE | `{ id, from_place, to_place, days, purpose }`; DELETE removes one rule. | Logbook |
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
| `/api/v1/charging-stats` | GET | `?cars=&from_ms=&to_ms=&places=&min_duration_min=` -> `{ count, energy_added_kwh, by_kind: { ac, dc, unknown } each { kwh, ms }, cost_by_currency, price_per_kwh: { all, ac, dc, basis, priced }, supercharger_cost, cost_per_100km: { value, method }, start_bands: [{ band, count }], end_bands, heatmap: { start: [], end: [] }, runs: [{ charges, start_ms, min_start_soc_pct, max_end_soc_pct }], share_above_80_pct, share_below_20_pct }`. `/api/v1/cars/{car}/charging-stats` is the same for one car. | Charges |
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
| `/api/v1/imports` | GET, POST (admin) | GET lists the imports (newest first, same fields as one import). POST starts a resumable upload of a TeslaMate backup (plain, gzip or xz). | Import |
| `/api/v1/imports/{import}` | PATCH, GET, DELETE (admin) | PATCH sends a chunk with `Upload-Offset`, `Upload-Length`, `Upload-SHA256`. GET -> `{ status: "uploading" \| "checking" \| "importing" \| "done" \| "failed", pass, bytes_read, bytes_total, error: { code, message }, report: { tables: { name: { read, imported, skipped: { reason: n }, bad_values: { column: n } } }, span } }`. DELETE discards. | Import |
| `/api/v1/imports/{import}/cars` | GET, PUT (admin) | `[{ tm_car, vin_prefix, match: car \| "new" \| "leave_out" }]` | Import |
| `/api/v1/imports/{import}/start` | POST (admin) | Starts the import after the whole file has been checked. | Import |

## Deliberately absent

- No endpoint wakes the car, pauses logging or sends a command: Sentry Mate is read-only to the car.
- No Tesla account, token or vehicle-list endpoint.
- No raw SQL, database tuning or index statistics.
- No VIN serial anywhere: only the VIN prefix is stored.

## Decisions

Points this document left open, decided while building the dev server (`ui/devserver`). Pages can rely on them.

### Requests and answers

- **CSRF header:** every POST, PUT, PATCH and DELETE carries `X-Sentry-Mate: 1`. Without it: `403 { error: { code: "csrf" } }`. Sign-in and set-up need it too.
- **Session cookie:** `HttpOnly`, `SameSite=Strict`, `Path=/`, 30 days. The real server names it `__Host-sentry-mate` and adds `Secure`. The dev server names it `sentry-mate` without `Secure`, because it is plain HTTP on 127.0.0.1.
- **The server's clock:** every `/api/` answer carries `X-Sentry-Mate-Now-Ms`, the server's "now" in epoch ms. Pages count "5 min ago" and resolve the period presets from it (`api.now()`), so a server whose clock differs from the browser's (the dev server's virtual clock) still shows the right times.
- **Car-scoped answers** carry `car` and `time_zone` at the top. In answers that mix cars, every row carries its own `car` and `time_zone`.
- **Ids:** cars are `c1`, `c2`, ...; places `p1`, ...; drive, charge and update ids are decimal strings above 2^53. Timeline rows use `<kind>:<start_ms>`.
- **Creating** answers `201` with the new object. **Deleting** answers `200` with a small JSON body (`{ deleted: true, id }`, or the changed object), never an empty body.
- **Versions:** objects that need `version` on a change (PATCH `cars/{car}`, `cars/{car}/settings`, `accounts/{account}`, `places/{place}`, `settings`; PUT `settings/mqtt`; optional `?version=` on DELETE `places/{place}`) return it on GET. The server makes one change at a time, so of two changes sent with the same version one wins and the other gets 409. A stale one gives `409 { error: { code: "stale_version", version } }` with the current version.
- **400 or 422:** a wrong type, an unknown field or a bad query value is `400 bad_input`. A well-formed value that breaks a rule (an unknown time zone, a password too short) is `422 invalid`. Both name the fields in `fields`, keyed by path (`units.distance`, `free_supercharging.0.from_date`).
- **Error codes** in use: `bad_input` 400; `not_signed_in`, `wrong_credentials`, `wrong_code`, `sign_in_expired` 401; `csrf`, `not_allowed`, `wrong_setup_code` 403; `no_such_car`, `no_such_drive`, `no_such_charge`, `no_such_account`, `no_such_session`, `no_such_token`, `no_such_key`, `no_such_rule`, `no_such_trip`, `no_such_update`, `no_such_place`, `no_such_overlap`, `no_such_import`, `not_found` 404; `method_not_allowed` 405 (with `Allow`); `stale_version`, `name_taken`, `last_admin`, `setup_done`, `import_running`, `not_uploading`, `wrong_offset` (with `offset` and an `Upload-Offset` header), `not_checked`, `not_matchable`, `not_ready`, `cars_unmatched` 409; `too_large` 413 (bodies over 1 MB); `invalid`, `checksum_mismatch` 422; `throttled` 429 (with `Retry-After`); `server_error` 500.
- **Who may read:** "(admin)" after the methods of a row means changes need an admin. Reading is admin-only too for accounts, the audit log, upload tokens, CAN Bridge keys, `settings/mqtt`, `network`, `system/storage`, `system/health` and imports. A watch account may read the rest for the cars it sees; places only where its cars have been. There is no separate export permission: whoever may read a resource may also take it as CSV, GPX or PDF (the "export permission" for GPX tracks is this read access). It never meets a hidden car's id either: `car_order` and `pinned_cars` in `/me/preferences` (and the `prefs` event), and `car_order` in `/settings`, list only the cars the account sees, so a car taken off its list drops out of its order and pins.
- **Sign-in throttle:** five wrong tries for one name within 60 s, then `429` until the oldest try is 60 s old.

### Lists, periods and CSV

- **Cursors** are opaque strings (base64url of the last row's sort key and a hash of the filters). A cursor sent with other filters or another sort gives `400` on `after`. Start again without it. So does a cursor the server did not give out, made or changed by hand.
- **Bodies:** a `Content-Length` that is not a whole number gives `400` on `Content-Length`. An id in a body that is not text (a car, a place) is a wrong type: `400`.
- `limit` is 1 to 500, default 50. `total` is sent only where it is cheap: `/cars` without `search` or `state`, and `/accounts`.
- **Period:** `from_ms` is inclusive, `to_ms` exclusive, both epoch ms. A missing edge means the start of the history or now. Some resources default to a window when `from_ms` is missing: `series` and `activity` 24 hours, `signals` 1 hour, `trip` 7 days, `battery-level` and `health` 30 days. The window ends at `to_ms` when it is given (else now). Rows belong to the period by their start and keep their own start, end and id; only where a resource says it clips (`/parked`, `/activity`, shares) are they cut at the edges. Local days, weeks and hours use the car's `time_zone` as it is now: when an admin changes it, every grouping of that car (statistics, visits and their days, charging heatmaps, storage) follows at once.
- **`cars=`** on report resources: absent or `all` means every car the account sees. A list with an unknown or hidden car gives `404 no_such_car`.
- **CSV:** `?format=csv`, or `Accept: text/csv` without JSON in it. A CSV answer carries every matching row, not one page. Nested fields become dotted columns (`start.place.name`), lists of plain values are joined with `;`, null is an empty cell. The file starts with a UTF-8 byte order mark and comes with `Content-Disposition: attachment`. Columns (series, curves, signals) give one row per `t_ms`. Asking CSV of something that is not a list gives `400` on `format`.
- **Car list sorts:** `sort=last_activity` (default newest first), `name` (A to Z), `soc` (highest first), and `custom`: the person's `pinned_cars`, then their `car_order`, then the rest by last activity. `search` matches the name, the model and the VIN prefix.

### Rows

- **In progress:** a drive, charge or gap still going has `end_ms: null`, its figures up to now, and `flags.in_progress: true`. The current state span in `/states` has `end_ms: null`.
- **Where:** a Drive's `start` and `end` are `{ lat, lon, place: { id, name } | null, address: { short, locality, region, country_code }, label }`. `label` is what to show: the place name, else the short address. A drive whose data stopped has the last known point and the label "Data stopped here". Timeline points are the same with `address` as the short text.
- **Purpose** on a Drive is `{ value: "business" | "private" | "commute", relation, note, source: "owner" | "rule" }` or null. An owner's choice wins over a rule.
- **Overlaps:** where CAN and TeslaMate history both exist, lists with `source=all` show only the side chosen for that span (CAN at first). `source=can` or `source=teslamate` shows that side only.
- **Power signs:** `power_kw` in `now` and in series is the pack's power: positive out of the pack (driving, heating), negative into it (charging, regenerating). Charge figures and the charge curve's `power_kw` are charging power, positive.
- **Tyres** are always ordered front left, front right, rear left, rear right.
- **Price per kWh:** basis `grid` is the cost over the grid energy (AC, by the meter). Basis `pack` is the cost over the energy added to the pack (fast charging, where the grid energy is unknown). `fast_charger.billed_kwh` is what the charger delivered (about the car's inlet energy).
- **Attention items** carry `detail` as an object in metric units, for example `{ tyre, pressure_kpa, recommended_kpa }`, `{ last_upload_ms }`, `{ soc_pct }`, `{ door }`, `{ reason, log_start_ms }`, `{ last_data_ms }`. The page writes the sentence.
- **Stale** means no upload for more than one hour.
- **Saved trips** belong to the account that saved them; a watch account may save its own. Purposes and purpose rules change car data and need an admin.
- **Upload tokens and CAN keys** are admin-only, reading included. DELETE on a token revokes it (it stays listed with `revoked: true`). The secret comes once, in the POST answer.

### Live values

- In `/cars/{car}/now`, every value read from the car is `{ value, at_ms }` or null: the signals, each `location` part, each tyre, each flag and door, each `charge` part. The snapshot's own facts are plain: `state`, `since_ms`, `at_ms` (the newest sample), `received_ms`, `delay_s`, `stale`, `buses` (each `{ bus: "vehicle" | "chassis", awake, at_ms }`), `driving`, `est_basis`, `charge.finish_ms`, `charge.charge_id`, `drive`, `awake_reasons`, `software`, `location.place` and `location.address`.
- `state` in the car list and in `now` is one of `driving`, `charging`, `parked`, `asleep`, `unknown`. In `/states` it is `online`, `asleep` or `unknown`, with the awake buses; vehicle bus only is `online` with `buses: ["vehicle"]`.
- `charge.status` is one of `disconnected`, `no_power`, `about_to_charge`, `charging`, `complete`, `stopped`.
- `/states` `share_pct`: `parked` is the share of time not driving (known time only), `asleep` asleep, `awake` online (driving included), `unknown` no data. They overlap on purpose and need not add up to 100.
- `/awake-reasons` joins awake spans that touch into one, with the reasons of both and a time-weighted average power.

### Events (`/api/v1/events`)

- Standard server-sent events: `event:` name, `id:` (a counter), `data:` JSON. The stream starts with `retry: 3000`. A comment line every 15 s keeps it open. There is no replay for `Last-Event-ID`; after a reconnect, load what you show again.
- `now`: `{ car, now }`. The first one for a car on a stream carries the whole object; later ones carry only what changed, to be merged field by field. A `{ value, at_ms }` pair is always sent whole.
- `rows`: `{ car, kinds, from_ms, to_ms }` when a drive, charge, parked spell or gap starts or ends. `fleet`: `{ car, change: "renamed" | "added" | "removed" | "list_changed", name }`. When an admin changes an account's car list, that account's streams get `added` or `removed` per car (`name` null on `removed`), or one `list_changed` with `car: null` when more than 100 changed, then a `prefs` event; a car given back starts again from a whole `now`. `prefs`: the whole preferences object, to that person only. `notice`: one notice.
- A stream ends when its session ends (sign-out, account removed), and when its reader falls 1000 events behind (it would miss a delta); the browser then reconnects, and the page loads what it shows again.

### Time series

- `values` holds one column per field. With one `agg` (or none), the key is the field name. With several, every agg is also under `<field>.<agg>` and the plain key holds the first one.
- Without `agg`, the plain column is the mean (the last value for `lat`, `lon`, `accuracy_m`, `odometer_km`, `heading_deg`), and when buckets are longer than 1 s, `<field>.min` and `<field>.max` come too. That is how downsampling keeps minima and maxima.
- `max_points` is 2 to 20000, default 1000. Buckets are 1 s, 2 s, 5 s, 10 s, 15 s, 30 s, 1, 2, 5, 10, 15, 30 min, 1, 2, 3, 6, 12 h, 1, 2 or 7 days (beyond that, a whole number of days), aligned to UTC, the smallest that fits. The first bucket holds `from_ms`, so it can start before it; the answer still never holds more than `max_points` buckets.
- `state` per bucket is the state that took most of it. `coverage_pct` is the share of the bucket with known data (asleep counts as known). A bucket with no sample is null in every column.
- `/signals` needs `series` (names from `/signals/latest`) and takes `max_points` (2 to 20000, default 2000). Rows of `/signals/latest` carry `has_series`: false for signals with named values (`gear`, `sentry_mode`, `charge_status`), which have a latest value only; asking `/signals` for them is 400.
- The charge curve is 1 Hz up to `max_points` (default 4000); longer charges come at a coarser `step_ms`, which the answer states.

### Formats

- **GeoJSON track:** a FeatureCollection with one LineString per stretch without dropped frames. Coordinates are `[lon, lat, elevation_m]`; `properties` holds parallel `t_ms` and `accuracy_m` arrays, plus `speed_kph`, `power_kw`, `soc_pct` when asked in `extensions`. `simplify_m` thins points (Douglas-Peucker) and keeps the arrays aligned.
- **GPX track:** GPX 1.1, one `trkseg` per stretch, `ele` and `time` on every point, extensions in our own namespace.
- **Logbook PDF:** a plain table, landscape, times in the car's time zone, with the continuity warnings and the totals per purpose.

### Battery and efficiency

- **`/battery`:** `capacity.new_source` is the first that exists: `owner` (`capacity_new_kwh` in the car's settings), `early_bms` (the pack's readings in its first 30 days), `imported_charges`, `pack_code`. `range.new_source` is `owner`, `first_30_days` or `pack_code`. `notes` codes: `new_value_typed`, `new_value_from_early_readings`, `new_value_from_imported_charges`, `new_value_from_pack_code`, `cold_battery_locks_part`, `lfp_charge_to_100_weekly`, `imported_history_estimated`, `no_history`. `lifetime` adds `standing_kwh` (used while parked) and `basis`; `charged_kwh` is `ac_kwh + dc_kwh`. A car without history answers the same keys with null.
- **Capacity follows the charges:** `group=charge` (the default) gives one reading per finished charge that added at least 10 %: `full_pack_kwh` = `energy_added_kwh` / (`end_soc_pct` - `start_soc_pct`) x 100 + `buffer_kwh`, from the figures that charge's row shows (imported charges included), with `charge` set to that charge's id. `group=day` gives the pack's own reading per awake day (`method: "bms"`); `group=month` the median of those per month, with `readings`. Imported days have no pack readings. `x=odometer` orders by odometer. `trend.projection` is `{ warranty_pct, full_pack_kwh_at_warranty, odometer_km, t_ms, method: "linear" }` or null when the pack is not losing capacity.
- **Days are the car's own:** wherever these resources bucket by day or week (`/battery-level` and `/projected-range` with `bucket=day` or `week`, `/temperatures` `daily`), a bucket runs from local midnight to local midnight (weeks from Monday), so the day the clocks change is 23 or 25 hours long and `t_ms` is always a local midnight. `bucket_ms` then names the nominal length (86400000 for a day). Hour buckets and buckets of a number of ms are fixed lengths.
- **`/battery-level`:** `from_ms` defaults to 30 days back. `bucket` defaults to `hour` up to 31 days, else `day`; the answer names `bucket` and `bucket_ms`. `moving` lines have one value per bucket (a window of daily means ending that day; `window` defaults to `7d`); `moving=false` gives empty lists. `band.high_pct` is 100 for an LFP pack. `charge_limit` is `[{ t_ms, pct }]`, a step line. `stats` is `{ mean, median, min, max }`.
- **`/projected-range`:** columns like `/series` (`t_ms`, `values`, `bucket_ms`, `coverage_pct`, `state`). `bucket` is `hour`, `day`, `week` or a number of ms; without it, the smallest that fits `max_points`.
- **`/parked`:** a spell is a stay parked off a charger at one spot, newest first, until the car drives, charges or moves (updates and gaps where it did not move stay inside). Rows also carry `car`, `duration_ms`, `clipped`, `range_lost_km`, `range_lost_per_hour_km` and `source`. `energy_method` is `counter` (the pack's own energy counters) for CAN rows, `range` for imported ones. `summary` adds `hours`; `summary.cost` is `{ amount, currency, basis }` at the home price, or null.
- **`/efficiency`:** `min_distance_km` defaults to 1, `temp_group_c` to 5 (1 to 20). A group's `temp_c` is its lower edge; `temp_to_c` its upper edge. `factor.source` is `energy_and_range`, or `owner` when the car's settings override it.
- **`/temperatures`:** `daily` rows also carry `t_ms`, the local midnight that starts the day; `coverage_pct` is the share of that day (as far as the period goes) the car was awake. A car that does not send a value (the old Model S has no cabin temperature) has nulls there.

### Reports

- **`/statistics`:** rows are newest first and clipped to the period: the first and last rows can be part periods. `label` is `2026-11-16` (day), `2026-W47` (ISO week), `2026-11` (month), `2026` (year) or `All`. Rows also carry `time_zone`, `energy_used_kwh` (driving) and `energy_standing_kwh` (parked). Money is per currency: `cost_by_currency: [{ currency, amount }]`, `price_per_kwh: [{ currency, value }]`, `cost_per_100km: [{ currency, amount }]`. The answer names `period` and, for several cars, `cars`. Drives count by start time, charges by start time: the months add up to the Drives and Charges lists for the same period.
- **`/mileage`:** rows also carry `period_end_ms` and `label`; `projection` adds `basis_days`. `distance_km` is by odometer; `not_logged_km` is what the car drove in gaps.
- **`/gaps`:** `cause` is `no_log` and `not_recorded` as they are, `chain_gap` for a log missing inside a charge (`inside: { charge }`), and `unknown` for a log that failed its check or lost frames. Rows also carry `duration_ms`, `gap_reason` (the timeline's word), `source` and `inside`. A gap still going has `end_ms: null`.
- **`/compare`:** `{ metric, unit, cars, series: [{ car, name, time_zone, t_ms: [], values: [] }] }`, one point per day with data, or per ISO week when the period is longer than 120 days (`capacity` in kWh, `full_range` and `odometer` in km, `wh_per_km` in Wh/km).
- **Visits:** a visit is one stop, from arriving to leaving; the stop going on now has `left_ms: null`. Rows also carry `label` (the place name, else the short address), `time_zone`, `unknown_ms` and `source`. `sort` is `arrived_desc` (default), `arrived_asc` or `dwell_desc`. `q` matches the place name and the address. `city=` takes a `city_key` (`US/California/San Francisco`).
- **Visit groups** (`/visits/places`, `cities`, `regions`, `countries`) carry `level` and the address parts of their level; `sort` is `visits` (default), `dwell`, `last` or `name`. `/visits/summary` adds `visits` and `cars`; `days` counts each car's days.
- **`/tracks`:** `max_points` (2 to 200000, default 10000) is shared over the drives by duration, at least two points each, and the answer never holds more; too many drives for the points gives `400` on `max_points`. The answer adds `points` (the number sent) and each drive's `source`.
- **`/updates`:** rows also carry `car`, `build_id` and `until_ms` (when the next update came; null for the current one). `detected_by: "build_change"` with `version: null` is an update seen only as changed module builds. PATCH `{ version }` takes `2026.38.4` style versions, or null to forget a typed version; the version then also names other updates with the same `build_id` (`version_source: "build_match"`).
- **`/cars/{car}/events`:** rows are `{ id, type, at_ms, topic, retained, payload }`, newest first; `payload` is exactly what MQTT would publish, in the server's units, with positions only when location publishing is on. An unknown `types` value gives `400`.

### Places

- **Matching** is live: a renamed, moved or removed place changes the name on every drive, charge, stop, timeline row, event and `now` at once. Of the places whose circle holds a point, the nearest centre wins (the lowest id on a tie). Filters by place use the same live match as the rows they filter (`places=` on drives and charges, `place=` on the timeline and on visits): a new place finds its drives at once, a removed one finds nothing. On drives, charges and the timeline an unknown place id gives `400`.
- **Costs stay put:** a charge keeps the cost it had when it ended. Changing a place's price changes only charges still going; `POST /places/{place}/reprice` applies today's price to past charges and answers `{ place, from_ms, to_ms, only_without_cost, dry_run, charges, repriced, unchanged, examples, cost_before, cost_after }`. A cost typed on a charge is never repriced.
- **Writes:** POST needs `name`, `lat`, `lon`; `kind` defaults to `other`, `radius_m` to 100 (10 to 5000), `time_zone` to the nearest known place's. PATCH needs `version` and sends only what changes. DELETE takes an optional `?version=`. Create, change and delete answer the place (or `{ deleted, id }`) plus `effect`: the same counts as the preview. `price.rates[].days` is `all`, `mon-fri` or `sat,sun` style; `from` and `to` are `HH:MM` (`24:00` ends the day). A price needs at least one of `per_kwh`, `per_minute`, a rate or `session_fee`; send `price: null` for none.
- **`/places/preview`** takes a draft place (with `id` to preview an edit) or `{ place, delete: true }` and answers `{ drives_named, drives_unnamed, charges_named, charges_unnamed, charges_repriced, examples, place, delete }`. `charges_repriced` counts the past charges a reprice after saving would change. It changes nothing and may be used by any account; a watch account may name only places its cars have used (others are `422` on `place` or `id`, as if there were no such place).
- **Lists:** `sort` is `name` (default), `visits`, `last_visit` or `created`; `total` is sent. Places also carry `address` and `created_ms`.
- **`/places/suggestions`:** `{ items: [{ lat, lon, radius_m, visits, dwell_ms, cars, label, address, last_visit_ms, kind }], next: null }`, where no saved place is, most visited first. **`/places/at`:** `{ lat, lon, place: Place | null, candidates: [{ id, name, kind, distance_m, radius_m }], rule: "nearest_centre", address }`.

### Integrations

- **`settings/mqtt`:** PUT sends the whole setting with `version`; leave out `password` to keep it, send null to remove it. GET gives `password: null` and `password_set`. Rules (422): a password needs `tls`; a `pin_sha256` (64 hex digits, colons allowed) needs `tls`; `namespace` and `ha_discovery.prefix` take no `+`, `#`, leading `$` or empty level; `ha_discovery.url` is `https://` or null.
- **`settings/mqtt/status`** adds `enabled`, `broker` and `tls`. **`/preview`** (`car` defaults to the first car the account sees) adds `car`, `name`, `units`, `device`, and each entity's `state_topic` and `config_topic`; a value the car did not send is not published, and position topics are empty while location publishing is off. **`/test`** takes no body (it uses the saved settings) and answers `{ ok, error, message, latency_ms, at_ms }`; `error` is `no_host`, `connection_refused` or `pin_mismatch`.
- **`/network`:** `inbound` rows also carry `tls`. Geocoding is listed but off while address lookup is offline.

### Server, settings and health

- **`/settings`:** PATCH with `version` sends only the parts that change (`{ units: { pressure: "psi" } }` keeps the other units). Units, range and car order are the defaults of everyone's preferences: a change pushes a `prefs` event to each person, and a person's own choices stay. Turning the release check on adds the `release_available` notice (and pushes a `notice` event).
- **`/notices`:** `certificate_expiring` carries `detail: { expires_ms, days }`. **`/maps/config`** and **`/geocode/status`** follow `/settings` at once.
- **`/cars/{car}/health`:** `from_ms` defaults to 30 days back. `reasons` codes: `upload_silence`, `trust_failed`, `logs_waiting_trust`, `rebuild_pending`. `last_log_trust` adds `log_start_ms`; keys add `preferred`, `device` and `logs_waiting`; `unknown_spans` are `[{ start_ms, end_ms, reason }]` (`end_ms` null while still unknown), with their total in `unknown_ms`.
- **`/rebuild`** answers `{ car, job, status: "done", from_ms, to_ms, kinds, logs_read, took_ms }` and pushes a `rows` event; `rebuild_pending` stays true for a few seconds after.
- **`/overlaps`:** rows are `{ id: "overlap:<start_ms>", start_ms, end_ms, shown, can: { drives, charges }, teslamate: { drives, charges } }`. PATCH `{ id, shown }` flips one span: Drives, Charges, Timeline, Statistics and Travel then show that side for it. It answers the list plus `changed`.
- **Storage:** `/cars/{car}/storage` answers `{ kinds: [{ kind, label, bytes }], total_bytes, items: [{ series, source, date, bytes }], next }`, newest day first, only days with data; `kinds=logs,series,curves,rows` filters the days. `/system/storage` is the sum of the cars (imported history apart), plus offline map tiles; `growth` is the last 30 full days, and `full_in_days` is free space over the mean growth.

### Import

- **Upload:** POST `{ name, bytes_total, compression? }` (or an `Upload-Length` header) answers 201 with the import, `upload_url` and `chunk_max_bytes` (just under 1 MB). Each PATCH body is the next chunk (`Content-Type: application/offset+octet-stream`) with `Upload-Offset`, `Upload-Length` and `Upload-SHA256` (hex or base64). A wrong offset gives `409 wrong_offset` with the offset to send from; a chunk that does not match its SHA-256 gives `422 checksum_mismatch`. One import at a time: another POST while one is unfinished gives `409 import_running`.
- **Status** moves `uploading` -> `checking` (pass 1) -> `ready` -> `importing` (pass 2) -> `done`, or `failed` with `error`. `ready` means checked and waiting for the cars to be matched and the import started. While checking or importing, `bytes_read` grows. Answers also carry `id`, `name`, `compression`, `uploaded_bytes`, `started_ms`, `checked_ms`, `finished_ms` and `cars_to_match`.
- **Cars:** GET `/imports/{import}/cars` answers `{ import, items: [{ tm_car, name, vin_prefix, drives, charges, first_ms, last_ms, match, suggested }], next }` once the whole file is uploaded (`409 not_checked` before). A car here with the same VIN prefix is suggested and matched at first. PUT `{ items: [{ tm_car, match }] }` changes only the cars it names; one car here takes at most one imported car.
- **Start** needs `ready` and every car matched (`409 not_ready`, `409 cars_unmatched`). The report counts only cars not left out. DELETE discards the job (history already imported stays). `/setup/status` sets `import_running` while an import is unfinished, with a fix `{ cause: "import_running", link, import }`.

### Additions

- `/api/v1/cars/{car}/charging-stats` is `/api/v1/charging-stats` for one car.
- `/me` gives `cars: "all"` for an account that sees every car, as in `/accounts`.
- `/me/sessions` rows also have `device`. `DELETE /me/sessions` ends every other session; this one stays (sign out with `DELETE /session`).
- `interface_size` is a percent, 50 to 200. `ui_pack` defaults to the pack being served.
- `follow_screen` (true or false, default true) is the person's "Follow screen size" switch. Packs size the interface as Auto x `interface_size`: Auto grows with the screen while `follow_screen` is true and is 1 when it is false.
- `/setup/status` adds `fixes: [{ cause: "no_token" | "token_unused" | "key_waiting" | "logs_waiting_trust" | "import_running", link }]`.
- `/cars/{car}/trip` also lists the `drives` and `charges` it counted.
- In `/charging-stats`: `heatmap.start` and `heatmap.end` are 7 rows (Monday first) of 24 hours in the car's local time. A run is two or more charges, each starting within a day of the one before. `share_above_80_pct` is the share of charges that ended above 80 %; `share_below_20_pct` of those that started below 20 %.
