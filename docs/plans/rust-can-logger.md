# Plan: a Rust car logger fed by the Screenmate CAN Bridge (SMO)

Status: plan only, nothing built yet (2026-11-16). Working name below is **"the logger"**; it can't be
called TeslaMate (see TRADEMARK.md), so the owner picks a name.

## Goal

A TeslaMate-style logger (drives, charges, battery health, efficiency, sleep, locations, statistics),
rebuilt around the CAN logs the SMO app already records and uploads, instead of the Tesla API:

- **One app, one UI.** TeslaMate needs a web app, Postgres and Grafana. The logger is one Rust binary
  that serves its own web UI with every page in it. No Grafana, no Postgres, no extra containers.
- **Full Rust**: server, ingest, decoder, storage, analysis and the UI (Leptos, compiled to WASM).
- **Best compression** of what we keep, measured on the owner's real logs.
- **Security first**: TLS 1.3 only, signed logs checked, no Tesla account, nothing leaves the box unless
  turned on.
- **Read-only to the car**, like SMO: the logger never talks to the car or Tesla at all.

What we gain over TeslaMate: no polling, so zero vampire drain and no "let the car sleep" logic; data at
CAN rate (speed, power, pedal, gear every 10-100 ms) instead of every few seconds; signals the Tesla API
never had (motor temps, coolant temps, brick voltages, 12 V system, TPMS from the bus).
What we lose: data is about 75 s behind live (one-minute logs + 15 s upload check), and anything only
the Tesla API knows (car name, paint, some settings) must be typed in or skipped.

## Where the data comes from

SMO already supports **two upload servers** ("Second Sentry Six Manager", every log goes to both, each
tracked separately). The logger implements the same upload API (Sentry-Manager `docs/can-bridge-api.md`):

- `GET /api/can/logs/ping`, `POST /api/can/logs/status`, `POST /api/can/logs/<name>` chunked and
  resumable (`Upload-Offset`, `Upload-Length`, `Upload-SHA256`), per-car bearer token, its own port.
- `ping` lists `canbridge-columns-xz/2` in `formats` (so SMO keeps writing the small format; it writes
  /2 only while **both** servers list it).
- No change needed in SMO to start. Sentry Manager keeps working as it is; the two don't depend on
  each other. The Raspberry Pi logger (`SMO/pi/`) uploads the same way and works too.

Alternative considered: pull decoded signals from Sentry Manager's export API. Rejected: it ties the
logger to Sentry Manager being up, needs a new read token there, and loses the raw-log evidence.

## Architecture (one binary, Cargo workspace)

| Crate | Job |
|---|---|
| `logfmt` | Read `.log.xz` (xz via `xz2`/liblzma, or pure-Rust `lzma-rs` if fast enough), both formats: candump text /1 and columns /2 (port of `SMO/tools/columns-to-candump.js`, with all its refuse rules). `# clock`, `# helper-clock` (wall-clock correction formula), `# dropped`, `# not recorded`, `# rolled over`. Fuzzed. |
| `trust` | Sidecar check: ECDSA P-256 signature over the fixed message (`p256` crate), SHA-256 and length, hash chain (`index`, `prev_name`, `prev_sha256`), signing key pinned per car on first upload or confirmed by fingerprint. Chain gaps and unsigned logs shown, never hidden. |
| `dbc` | DBC parser + decoder (Intel/Motorola, signed, mux, VAL_). Same DBC set and rules as Sentry Manager: model3dbc + `model3y-opendbc.dbc` + `override.dbc`, per-model sets from the VIN (0x405 / 0x508), each bus decoded only with its own messages, can3 only 0x04F GPS, never 0x311 / 0x238. |
| `ingest` | The upload API above (axum + rustls), token per car (only SHA-256 stored), size limits, `.part` files, atomic rename, fsync. Stores each log **byte for byte as received** (the signature covers those bytes). |
| `store` | SQLite (`rusqlite`, WAL) for cars, drives, charges, states, places, users, audit log; signal segments on disk for time series (see Compression). |
| `timeline` | Turns decoded signals into the car's history (see below). Pure functions, re-runnable for any time range. |
| `web` | JSON API + serves the UI, sessions, auth, live updates over WebSocket/SSE. |
| `ui` | Leptos (SSR + hydrate) pages; charts with uPlot and maps with MapLibre through small JS bindings (the only non-Rust parts, vendored, no CDN). |
| `mqtt` (optional) | Home Assistant / MQTT publish, off by default, `mqtts://` only (`rumqttc` + rustls). |

Flow: upload → stored as received → trust check → decode (background, idle priority) → 1 Hz car samples
+ full-rate signal segments → timeline (drives, charges, states, positions) → UI / MQTT.

Logs can arrive late or out of order (car out of Wi-Fi range for hours, catch-up). The timeline works per
car on a window: when a minute arrives, the window around it (back to the last settled state, e.g. the
last "parked and asleep") is rebuilt. Everything derived can be thrown away and rebuilt from the raw
logs (Settings → Rebuild, also after a DBC or rules change), like Sentry Manager's index.

## What the timeline works out

From the decoded signals (names from Sentry Manager's `canNow.js` stat list, first signal with a value
wins):

| Thing | How |
|---|---|
| Online / asleep / unknown | Frames present = online. A log closed `quiet` and no frames after = asleep. `# not recorded` or a gap with no log = **unknown**, never counted as sleep. |
| Drive | Gear (`DI_gear`) leaves P until back in P for over N s (N set, default 60). Distance from odometer (`UI_odometer` / `Odometer3B6`), not GPS. Start/end SOC, range, energy used (integrated battery power), max speed, max/min power, ascent/descent, temps. |
| Positions | GPS 0x04F (lat, long, accuracy) at 1 Hz while driving, plus speed, power, SOC, elevation. One position every few minutes while parked. |
| Charge | Charge status (`BMS_uiChargeStatus` / `CP_hvChargeStatus*` / `PCS_hvChargeStatus`) charging. Energy added from the battery side (integrated battery power into the pack), energy used from the AC/DC charger side, curve (power, voltage, current, SOC) at 1 Hz, AC vs DC, start/end SOC and range, cost from location tariffs. |
| Battery health | Max range at 100 % from rated range vs SOC over time; brick voltage spread; capacity estimate from energy in vs SOC change on long charges. |
| Efficiency | Wh/km per drive, vs speed and outside temp. |
| Vampire drain | SOC and range lost while parked, split by asleep / awake / unknown. |
| Software updates | Only if a firmware-version signal is found in the logs (to check on real data); else left out. |
| Places | Reverse geocode + geofences (home, work, chargers). |

Every rule above gets checked against the owner's real drives and charges before it's trusted (Phase 3).

## The UI (one app, every page)

Phone and desktop layouts for every page (screenshots of both with every change, as in Sentry Manager).

- **Now**: the car's newest values (SOC, range, state, location, temps, tires, charging), live as logs
  arrive, with how old they are.
- **Drives**: list with filters; drive page with map track, speed / power / SOC / elevation charts on one
  zoomable time axis, energy, efficiency, and the raw signals for that drive.
- **Charges**: list; charge page with the charge curve, energy added/used, losses, cost.
- **Timeline**: states, drives, charges and gaps for a day, week, month.
- **Battery**: health, degradation, brick voltages and temps.
- **Efficiency, Vampire drain, Statistics, Mileage, Temperatures, Tires, Visited places, Locations**:
  the TeslaMate dashboards rebuilt as pages (not copied).
- **Signals**: any decoded signal over any range (like Sentry Manager's decoded export, on screen), and
  CSV export.
- **Logs**: every uploaded log with its trust result, chain gaps, format and size.
- **Settings**: cars (upload token, address, certificate fingerprint, signing key), people and
  passkeys, units, tariffs, geofences, maps and geocoding, MQTT, backups, rebuild.

## Compression

1. **Raw logs: kept exactly as uploaded** (`columns-xz/2`, ~0.76 MB per driving minute). They can't be
   recompressed without breaking the signature, and they are the evidence. If a better format is wanted,
   it's added in SMO as a `/3` and listed in `ping` (proposal: per-message signal-aware columns + xz
   preset 9e or zstd-19 with long window; only if it beats /2 on real logs by a clear margin).
2. **Derived signals: own segment files**, per car per day, per signal: times as delta-of-delta zigzag
   varints, values as XOR (floats, Gorilla style) or delta (ints/enums), only on change plus once a
   second, then zstd level 19 with a dictionary trained on the car's own data. Pick the final codec by
   measuring zstd-19, zstd --ultra -22 and xz 9e on the owner's real data; decode speed matters too (the
   UI reads these).
3. **SQLite** for the small stuff (drives, charges, positions at 1 Hz) with `page_size` tuned and a
   periodic `VACUUM`; positions can move to segments if they grow.
4. Retention settings: raw logs forever by default (the owner has the space), segments and SQLite
   always (small, rebuildable).

## Security

- **TLS 1.3 only** (rustls) on both ports; self-signed certificate made on first start with its
  SHA-256 shown for SMO to pin, or the owner's own / Let's Encrypt. No plain HTTP, except a redirect.
- **Logs**: signature + chain checked on every upload; key pinned per car; a log signed by another key
  is kept but marked and left out of history until the owner says otherwise.
- **Upload tokens**: one per car, write-only, tied to one `vehicleKey` on first upload, only SHA-256
  stored, revocable.
- **Accounts**: argon2id passwords, passkeys (WebAuthn) optional, first-run setup over HTTPS only,
  rate limits and lockout, session cookies `Secure; HttpOnly; SameSite=Strict`, CSRF tokens, strict CSP
  (no inline scripts, nothing from a CDN), audit log of every setting change.
- **At rest**: SQLite and segments encrypted (XChaCha20-Poly1305 per file / SQLCipher-style), key in a
  key file in the data volume or a passphrase asked at start (owner choice: key file means it restarts
  on its own). Raw logs encrypted the same way, the signature still checks after decrypting.
- **Privacy**: logs hold GPS. Nothing leaves the container by default: offline reverse geocoding
  (bundled dataset) and offline map tiles (one PMTiles file); online geocoding / OSM tiles only if
  turned on. No telemetry. Real coordinates never in code, tests or commits.
- **Code**: `#![forbid(unsafe_code)]` in our crates, `cargo deny` + `cargo audit` in CI, fuzzing of
  `logfmt`, `dbc` and the upload parser, minimal Docker image (distroless/scratch, non-root, read-only
  root filesystem), signed release images.
- Nothing in the logger can transmit on a CAN bus or talk to Tesla.

## Phases

0. **Decide** (questions below). Get a real bundle from the owner (Sentry Manager → Nerd → export
   bundle, a few drives and a charge; cloud sessions can't reach the home network) for tests.
1. **Ingest + trust**: upload API, storage, signature/chain, tokens, TLS. Done when SMO uploads to it as
   the second server with no errors and Sentry Manager still gets everything.
2. **Decode**: `logfmt` + `dbc`. Done when its output matches Sentry Manager's
   `/api/can/export/decoded` on the same logs, value for value.
3. **Timeline**: states, drives, positions, then charges. Checked by hand against the owner's real drives
   and charges (and against TeslaMate history if the owner still has it).
4. **Compression**: segment format, measured, picked.
5. **UI**: Now, Drives, Charges, Timeline first; then the other pages; phone + desktop.
6. **Accounts, encryption at rest, backups, MQTT/Home Assistant**.
7. **Docker image, docs, first release**. Optional: importer for an existing TeslaMate database.

## Open questions for the owner

1. **Name** of the app (it can't be TeslaMate).
2. **This repo**: replace the TeslaMate fork's contents with the new Rust app, or start a new repo? (A
   from-scratch rewrite that copies no TeslaMate code can pick its own license; porting TeslaMate's code
   or SQL keeps it AGPL.)
3. **SMO's second upload slot**: is it free, or used for a second Sentry Manager? If used, SMO needs a
   third slot.
4. **Encryption at rest**: key file (restarts by itself) or passphrase at every start?
5. **Maps and addresses**: offline only (bigger image / one-time download) or allow OSM online?
6. **Import** the old TeslaMate history, if there is any?
7. **Several cars / several people**, or one car, one owner for now?
