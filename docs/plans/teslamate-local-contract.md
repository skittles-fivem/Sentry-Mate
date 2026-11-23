# Local TeslaMate: the shared contract (working notes for the parallel pieces)

Clean room: never read TeslaMate's code. Its public user documentation (the MQTT topic list, that its database is
PostgreSQL) and this repo's own TeslaMate import (`crates/import`, which already parses TeslaMate's schema) are fine.
Made-up data only: hosts `192.0.2.x`, made-up VINs (as `crates/import/tests/fixtures` uses), no real tokens.

## Names

- Live input: `model::Input::TeslamateLocal` (`teslamate_local`), `Inputs::TESLAMATE_LOCAL`, `Origin::TeslamateLocal`,
  stream `Stream::TeslamateLocal` (dir `teslamate-local`). Label "Data from local TeslaMate", short "local TeslaMate".
  Order: SMO > Tesla API > TeslaFi API > local TeslaMate > imports. (Done in `crates/model`; engine/timeline by lead.)
- Filter word `source=teslamate_local`; sources `{ kind: "teslamate_local", label: "Data from local TeslaMate" }`.
- DB catch-up rows are stored as a TeslaMate import: `Source::Teslamate`, origin `import:teslamate`.
- Crate `crates/tmlocal` (package `tmlocal`): `lib.rs` (Service, Options, Sink, errors), `store.rs` (sealed config,
  vault like `crates/teslafi/src/store.rs`), `mqtt.rs` (subscriber), `map.rs` (topic -> `model::Sample`),
  `catchup.rs` (Postgres catch-up; DB piece owns it and `tokio-postgres`).
- Network `/api/v1/network` outbound purposes: `teslamate_mqtt` ("Local TeslaMate (MQTT)") and `teslamate_db`
  ("Local TeslaMate (database)"), `host` = `host:port` as saved, `enabled` while used.
- Audit actions: `teslamate-local-added`, `teslamate-local-changed`, `teslamate-local-paused`,
  `teslamate-local-resumed`, `teslamate-local-car-matched`, `teslamate-local-removed`. None holds a password.

## API (admin only, reads included; PUT, PATCH and DELETE take `confirm_password` and `code?` like TeslaFi)

The test and the car matches take the admin session alone (they store and show no password).

`/api/v1/integrations/teslamate` GET, PUT, PATCH, DELETE

GET answers TeslamateLocal:

```json
{
  "enabled": true,              // false: not added (then everything below is defaults, status "off")
  "paused": false,
  "mqtt": { "host": "192.0.2.10", "port": 1883, "tls": false, "pin_sha256": null, "username": null,
            "password_set": false, "prefix": "teslamate", "allow_unencrypted": false },
  "database": { "enabled": false, "host": "192.0.2.10", "port": 5432, "database": "teslamate",
                "username": "teslamate", "password_set": false, "tls": false, "pin_sha256": null,
                "allow_unencrypted": false, "interval_s": 300 },
  "status": {
    "mqtt": { "state": "off" | "connecting" | "connected" | "error" | "paused",
              "connected_since_ms": null, "last_message_ms": null, "messages_today": 0, "last_error": null },
    "database": { "state": "off" | "syncing" | "ok" | "error" | "paused",
                  "last_sync_ms": null, "next_sync_ms": null, "drives": 0, "charges": 0, "positions": 0,
                  "last_error": null }
  },
  "cars": [ { "teslamate_car": "1", "display_name": "Made-up Car", "vin_prefix": "5YJ3E1EA0P",
              "match": 3 | "new" | "leave_out" | null, "suggested": 3 | null, "last_message_ms": 0 } ],
  "hosts": ["192.0.2.10:1883"],
  "version": 4
}
```

`last_error`: `{ code, message, at_ms }` or null. Codes: `connection_refused`, `timeout`, `auth_failed`, `tls_failed`,
`pin_mismatch`, `not_teslamate` (database has no TeslaMate tables), `query_failed`.

- **PUT** `{ version, mqtt: {...}, database: {...}, confirm_password, code? }` adds (version 0 / not added: 201) or
  replaces the settings. `password`: a string sets it, `null` removes it, left out keeps it, but a kept password
  goes only to the host it was typed for: changing `host`, `port` or `tls` while a password is saved and not sending
  `password` again is `422` on `mqtt.password` / `database.password` (`password_resend`).
- **Plain text rule (422 `plaintext_password`, field `mqtt.password` / `database.password`):** a password (new or kept)
  with `tls: false` needs `allow_unencrypted: true`. Without a password, plain TCP is allowed. `pin_sha256` (64 hex,
  colons allowed) needs `tls`. `prefix`: no `+`, `#`, leading `$`, empty level. `host`: a host name or IP, no scheme.
- **PATCH** `{ version, paused, confirm_password, code? }`.
- **DELETE** (body `{ confirm_password, code? }`) stops at once and deletes the sealed file: `{ deleted: true }`.
  History already stored stays.
- `/api/v1/integrations/teslamate/test` POST `{ target: "mqtt" | "database", mqtt?, database? }`: tries the draft
  (or the saved settings when no draft; a draft without `password` and the same host/port/tls uses the saved one) and
  answers `{ ok, error, message, latency_ms, at_ms, cars_seen }` (`cars_seen`: the TeslaMate car ids seen in a few
  seconds on MQTT, or in the `cars` table). Subscribes only; publishes nothing.
- `/api/v1/integrations/teslamate/cars` GET `{ items: [car], version }`, PUT `{ version, items: [{ teslamate_car,
  match }] }`; `409 car_taken` when that car here is matched to another TeslaMate car.
- `/system/health` gains `teslamate_local: { enabled, mqtt_state, database_state, cars, last_message_ms, last_sync_ms }`.

## MQTT

Read only: subscribe `<prefix>/cars/+/#`, QoS 0, never publish (no will, no retain). Values per the TeslaMate docs:
`display_name`, `state`, `since`, `latitude`, `longitude`, `heading`, `speed` (km/h), `power` (kW), `elevation`,
`odometer` (km), `battery_level`, `usable_battery_level`, `rated_battery_range_km`, `est_battery_range_km`,
`ideal_battery_range_km`, `charger_power`, `charger_voltage`, `charger_actual_current`, `charger_phases`,
`charge_energy_added`, `charge_limit_soc`, `time_to_full_charge`, `plugged_in`, `inside_temp`, `outside_temp`,
`shift_state`, `locked`, `sentry_mode`, `doors_open`, `windows_open`, `trunk_open`, `frunk_open`, `is_climate_on`,
`version`, `tpms_pressure_*` (bar). Map only what `model::Sample` holds. VIN is not on MQTT in every version: match by
display name / VIN prefix when the DB is on (its `cars` table has `vin`, keep only the prefix).

## UI

Settings: Integrations, a "Local TeslaMate" section right after TeslaFi, anchor `#teslamate` (a tab where the pack
has tabs). Forms for MQTT and database with a Test button each, the "Allow unencrypted (local network only)" tick
(off by default, shown with a warning, only meaningful with a password and TLS off), TLS off on port 1883 by
default (the TLS tick switches 1883 and 8883 while the port holds the other default), the pin shown only with TLS, status, car match table with
Save matches visible when a suggestion is preselected, Pause, Remove. Source labels/filters: "Data from local
TeslaMate".

## Server side as built (deviations and details)

- `match` / `suggested` are car ids as strings (`"c3"`), as TeslaFi's car table writes them; `last_message_ms` is
  `null` for a car not heard from since the server started.
- PUT: `database` may be left out (the saved one, or the defaults, is kept with its password). The database's fields
  are checked only while `database.enabled` (or a database password is sent). `password_set` is accepted in a body
  and ignored. A stale or wrong `version` is `409 stale_version`; adding answers 201, replacing 200.
- PATCH / DELETE / cars before it is added: `404 teslamate_not_added`. An unknown `teslamate_car` in cars PUT: `422
  invalid` on `items.<i>.teslamate_car`.
- Test with no draft and nothing saved: `400 bad_input` on `mqtt` / `database`. The draft is checked for the side
  under test only (the database side as if enabled). The answer's `error` uses the `last_error` codes.
- The audit entries' target is `settings:teslamate_local` (a car match: `teslamate_local:<car>:<tm id>`).
- `tmlocal::Sink` gained a default method `meta(&self, car, &tesla::ApiMeta)`: MQTT values a `Sample` has no field
  for (`locked`, `version`, `update_available`, `update_version`, `scheduled_charging_start_time`,
  `charge_energy_added`, `est_battery_range_km`, `display_name`), at most every 10 s per car. `TeslamateLocalSink`
  forwards the off-CAN values to `Engine::off_can` and `charge_energy_added` and `est_battery_range_km` to
  `Engine::api_meta(Input::TeslamateLocal, ..)` (kept apart from the Tesla API's, named `teslamate_local`). `Sink::changed`
  takes the status now; `TeslamateLocalSink::on_change` gets it and the web side turns `Status::problems()` into
  notices. `windows_open`, `doors_open` (one flag), the sunroof and `active_route_*` are
  left out (no field yet); `map::target` is the per-topic table.
- The DB timer (`database.enabled`, not paused) runs at start and every `interval_s`: `catchup::test` (merges the
  `cars` table into the car list), then for matched cars `catchup::dump` to `<data>/teslamate-local/catchup.dump`,
  `Sink::catch_up` on `spawn_blocking`, and the cursor (sealed in the settings file) is saved only if that returned
  Ok; the dump is deleted either way. The database Test uses `catchup::test`.
