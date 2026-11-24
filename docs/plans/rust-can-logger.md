# Plan: a Rust car logger fed by the Screenmate CAN Bridge (SMO)

Status: plan only, nothing built yet (2026-11-16). Working name: **Sentry Mate** (owner, not final;
"the logger" below means the same app). It can't be called TeslaMate (see TRADEMARK.md). Before it's
final, check it isn't too close to TeslaMate or to Screenmate (the device's maker) to be confused.

## Goal

A TeslaMate-style logger (drives, charges, battery health, efficiency, sleep, locations, statistics),
rebuilt around the CAN logs the SMO app already records and uploads, instead of the Tesla API:

- **One app, one UI.** TeslaMate needs a web app, Postgres and Grafana. The logger is one Rust binary
  that serves its own web UI with every page in it. No Grafana, no Postgres, no extra containers.
- **Full Rust** on the server: ingest, decoder, storage, analysis, security and the API. The interface is a UI pack
  (HTML/CSS/JS, or Rust compiled to WebAssembly) so anyone can make their own (see UI packs).
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
tracked separately). **The second slot is free (owner, 2026-11-16): the logger takes it.** The logger implements the same upload API (Sentry-Manager `docs/can-bridge-api.md`):

- `GET /api/can/logs/ping`, `POST /api/can/logs/status`, `POST /api/can/logs/<name>` chunked and
  resumable (`Upload-Offset`, `Upload-Length`, `Upload-SHA256`), per-car bearer token, its own port.
- `ping` lists `canbridge-columns-xz/2` in `formats` (so SMO keeps writing the small format; it writes
  /2 only while **both** servers list it).
- No change needed in SMO to start. Sentry Manager keeps working as it is; the two don't depend on
  each other. The Raspberry Pi logger (`SMO/pi/`) uploads the same way and works too.

Alternative considered: pull decoded signals from Sentry Manager's export API. Rejected: it ties the
logger to Sentry Manager being up, needs a new read token there, and loses the raw-log evidence.

## What we take from Sentry Manager (reference, not copied)

Sentry Manager (`skittles-fivem/Sentry-Manager`: `docs/can-bridge-api.md`, `server/canLogs.js`,
`canTrust.js`, `canIndex.js`, `canDbc.js`, `carModels.js`, `canNow.js`, `places.js`, `powerPrice.js`)
already handles these logs and several cars. The logger behaves the same way, rewritten in Rust (its
code is JavaScript, so nothing is copied; only the rules and the API, which SMO already speaks).

### Handling the data

- **Kept as received, never replaced**: `<data>/can/<vehicleKey>/`, a finished file can't be overwritten
  (`409 exists_different`), only added; resending the same bytes is a harmless `201 already`.
- **Uploads**: same API, limits and errors (`maxChunk` 16 MiB, `maxFile` 1 GiB, `maxSidecar` 1 MiB,
  2,000 names per `status`), port of its own that is **closed while uploads are off**, every other path
  `404`, `403 https_only` over plain HTTP. Connection guards: a TLS handshake and the headers within 10 s
  each, a request at most 10 min, 6 connections per address (an IPv6 /64 is one address; 256 in all), idle
  connection closed after 15 s. Connections that have not shown a valid token are closed oldest first to make room
  (at most 192), so connections without a token can't keep the bridge out.
- **A log's check** (judged when log and sidecar are both in): `verified` (paired key, bytes match),
  `failed` (kept, listed with the reason, never used unless the owner picks *Use anyway*, then marked
  in the UI wherever that data shows), `could not sign` (bridge said so; *use all* per key, password
  asked), `unsigned` (car not paired: used, with a quiet note), `waiting` (no sidecar yet; judged
  unsigned after 10 min). **Nothing is decoded until its check allows it.**
- **Pairing**: the first signed log pins the key; another key's logs are kept, not used, and offered for
  trust by fingerprint; *Forget the pairing*. **Chain report**: gaps, mismatches, duplicates, reorders,
  restarts at 0 (with the bridge's `chain_note`), deleted logs still counted as links.
- **Rules carry a version**: trust records and decoded data are stamped with the version of the rules
  and the DBC set they were made with; after an update, older ones are judged / decoded again in the
  background, and what an older decoding wrote is taken out. Same idea for the timeline (drives and
  charges are stamped with the timeline rules' version and rebuilt when they change).
- **Safe reading**: lines over 512 bytes skipped, a log that unpacks past 4 GB stopped there, columns
  files refused on any bad count (SMO's spec) or past 8 million frames, and no log unpacks to more than 200
  times its packed size (at least 1 MiB; real logs unpack to under 10 times): candump text is cut there; a
  columns body is refused, which makes the log a hole, never decoded again. A columns decode holds the body and 9 bytes
  per frame, so a crafted file can't exhaust memory.
- **Audit log** for every trust, token, delete and setting change.

### Several cars (owner: yes, as Sentry Manager does it)

- **A car** is the logger's own record: name, model and year (Automatic from the VIN, or set), time
  zone, units, battery and price settings, places.
- **CAN keys**: each SMO install (and each Raspberry Pi logger) has its own `vehicleKey`. A car can have
  **several keys** over time (new Screenmate, app reinstalled, a Pi as well); a key belongs to exactly
  one car. The map key → car is kept in the database; a key no car claims goes to the car when there is
  only one, and otherwise waits in Settings → Cars ("New CAN Bridge seen: which car?").
- **Tokens**: one per car, write-only, tied to that car's key on its first upload (`403 wrong_car` for
  another key, other keys' names answered `forbidden` in `status`), no cap, revoked one by one.
  `ping` answers the key the token is tied to.
- **DBC set per car** from its model and year (Automatic: VIN from 0x405, or 0x508 on a pre-2021 S/X;
  4th character the model, 10th the year), the same sets: `tesla-3y` and `tesla-sx-legacy`; changing it
  decodes that car's logs again. Only the Model Y is checked on a real car so far; others are marked
  "not checked on a real car" in the UI, as Sentry Manager does.
- **Two keys at once on one car** (a Screenmate and a Pi both recording): the car's timeline uses one
  source per span (the car's preferred key, set in Settings, else the verified one with fewer drops);
  the other is kept and viewable in Signals.
- **Every page has a car picker**, plus an "All cars" view for Statistics, Mileage and the Timeline.
  Now and the MQTT topics are per car.
- **People**: roles as in Sentry Manager: **Admin** (everything) and **Watch only** (sees drives,
  charges, maps, stats, exports; can't change settings, trust, tokens or delete; can't read logs). At
  least one admin always; adding or changing an account asks for the password again (and the two-step
  code when the acting account has it on). Two-step sign-in
  (authenticator app) per account, and an option to require it from outside the home network. New
  here: an account can be limited to some cars (e.g. a family member sees only their car).
- **Places and prices**: saved places with their own radius (50 m to 2 km, default 200 m), suggestions
  from where the cars park most; electricity price as one price or a time-of-use plan, costs summed at
  the price of each moment of a charge, per place (home vs work vs a supercharger price typed in).

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

## Unlimited cars, desktop first (owner, 2026-11-16)

- **No limit on the number of cars.** Nothing may hold every car in memory or scan every car to answer one:
  everything is kept per car (a folder each) and loaded when used (a bounded cache of recently used cars); lists of
  cars, the fleet view and totals come from small per-car summaries; each car is worked on independently (its own
  lock and work queue), so cars upload and rebuild in parallel; per-car history files are appended to, never
  rewritten whole on every upload. Tokens: no cap (one per car, as many as there are cars). The UI's car picker is
  a search, not a short list, and there is an **All cars** (fleet) page that pages through any number of cars.
  Accounts can be limited to a set of cars of any size. MQTT publishes all cars over one connection.
- **Desktop first.** The UI is made for desktop browsers now; phone layouts come later (the packs are written so
  they can be added without starting over). Screenshots with changes: desktop only, until phone support is added.

## Display: native 4K and scaling (owner, 2026-11-16)

- **Native 4K** (3840 x 2160 at 100 %): the interface uses the space (more columns, wider maps and charts, more rows
  in lists) instead of a narrow column in the middle, and text and controls grow with the screen so they read well at
  a distance. Crisp at any pixel density: canvas charts drawn at the device pixel ratio and redrawn when it changes,
  icons and logos as SVG, the map's vector tiles (offline PMTiles) sharp at any zoom.
- **Scaling**: works with the operating system's scaling (100 %, 125 %, 150 %, 175 %, 200 %) and browser zoom
  (50 % to 200 %) without overlapping, cut-off or tiny elements: sizes in rem/em, layouts that reflow, no fixed
  pixel widths that break; charts and the map resize with their space.
- **Interface size** setting (per person, 75 % to 200 %, remembered with the account) on top of the system's, for a
  dashboard on a wall-mounted 4K screen or a small laptop.
- Checked with screenshots at 1440 x 900 at 2x, 1920 x 1080 at 1x and 2x (a 4K screen at 200 %), 2560 x 1440,
  3840 x 2160 at 1x, and browser zoom 67 % and 150 %. Packs are expected to do the same; the built-in ones show how.

## UI packs: any interface, installed from a zip (owner, 2026-11-16)

Every interface is a **UI pack**, the built-in ones included, the way Sentry Six Manager's interface packs work
(`docs/interface-packs.md` there). The server (Rust) owns the data, the rules and security; a pack is only how it
looks and works in the browser. So a pack you upload can do everything the built-in ones do: no second-class UIs.

- **A pack is a `.zip`**: `ui.json` (`name`, `description`, `author`, `version`, `api`: the API version it was made
  for) and `web/` (`index.html` and its files). Its files take the place of the built-in pack's files with the same
  path, so a pack can change one stylesheet or replace the whole interface.
- **The API it uses** is the same documented, versioned JSON API the built-in packs use (`/api/v1/...`, written up
  in `docs/ui-api.md`): cars, now, drives, charges, timeline, positions, charge curves, signals, logs and trust,
  places, prices, settings, accounts, and a live stream (server-sent events) for the Now page. Cookies carry the
  session; every change needs the CSRF header. A pack made for an older `api` keeps working while that version
  is served; the UI shows a note when a pack targets a version the server no longer has.
- **Built in**: Instrument (the default; an admin can choose an uploaded pack for everyone). Logbook, Horizon,
  Console, Pebble and Manager (Simple) were built in too; they are archived in `ui/archived`, not built, served or
  tested, and a saved choice of one serves Instrument (`ui/archived/README.md`) and **Download the
  built-in pack to start from**.
- **Choosing**: Settings → Interface, for everyone (admins), and each person can pick their own on their Account
  page. A switch applies at once.
- **If a pack breaks**: `https://<server>/original` shows the built-in interface in that browser (until **Show the
  chosen interface**), and from there Settings → Interface can switch back for everyone.
- **Safety** (a pack's scripts run as whoever is signed in and can do anything they can):
  - only admins upload or choose packs, after their password (and two-step sign-in when on); every install,
    choice and removal is in the audit log, with the pack's SHA-256;
  - the zip is checked before anything is kept: at most 50 MB and 2,000 files, no absolute paths, no `..`, no
    links, names limited to safe characters, only web file types (html, css, js, mjs, json, svg, png, jpg, webp,
    avif, gif, ico, woff, woff2, ttf, wasm, txt, md), and a `web/index.html`;
  - pack files are served with a strict Content-Security-Policy: scripts and styles only from the pack's own files
    (no inline scripts, nothing from other sites), `connect-src 'self'`, images and map tiles only from the server
    and the tile server the owner chose, `wasm-unsafe-eval` only so a pack can be written in Rust (WebAssembly),
    `frame-ancestors 'none'`, `nosniff`, exact MIME types, no directory listings;
  - packs are kept sealed with the vault like everything else.
- **Every page gets the server's own notices** whatever the pack: a fixed bar for critical ones (the recovery key
  file still on the server, a vault or certificate problem, an update that matters), from `/api/v1/notices`, which
  packs should show too.

The built-in packs are plain HTML, CSS and JavaScript with no build step, so anyone can open the downloaded pack
and change it. Charts use uPlot (MIT) and maps MapLibre GL with PMTiles (BSD-3), all inside the pack.

## The UI (one app, every page)

Desktop layouts for every page now (phone layouts later; see Unlimited cars, desktop first).

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
- **At rest**: see "Encryption at rest" below.
- **Privacy**: logs hold GPS. No telemetry. Real coordinates never in code, tests or commits.
- **Maps and addresses: both offline and online** (owner's choice, 2026-11-16). Offline: map tiles from
  one PMTiles file and reverse geocoding from a bundled dataset, so maps and addresses work with no
  internet. Online: OpenStreetMap tiles and Nominatim addresses (or a server the owner sets), for
  fresher detail. Settings → Maps and addresses picks Offline, Online, or Online with offline fallback
  (used when the internet is down), separately for maps and for addresses. Offline data is a one-time
  download into the data volume, not baked into the image. Online requests send only tile numbers or one
  coordinate, over HTTPS, nothing else; addresses found online are cached so each place is asked once.
- **Code**: `#![forbid(unsafe_code)]` in our crates, `cargo deny` + `cargo audit` in CI, fuzzing of
  `logfmt`, `dbc` and the upload parser, minimal Docker image (distroless/scratch, non-root, read-only
  root filesystem), signed release images. The image workflow pins every action to a commit id and the base
  image to a digest (Dependabot proposes bumps; `crates/sentry-mate/tests/repo_hygiene.rs` checks the pins).
- Nothing in the logger can transmit on a CAN bus or talk to Tesla.

## Importing old TeslaMate history (owner: yes, 2026-11-16)

The owner's existing TeslaMate data comes in, so history starts years before the CAN Bridge.

- **What to upload**: TeslaMate's normal backup, made with its documented command
  (`docker compose exec -T database pg_dump -U teslamate teslamate > teslamate.bck`), plain SQL, or
  the same file gzipped / xz'd. Settings → Import → upload (resumable, chunked, HTTPS, admins only). No
  Postgres needed: the importer (Rust) reads the `COPY ... FROM stdin` blocks of the dump itself and
  ignores everything else (no SQL is ever run). A newer or older TeslaMate schema is matched by column
  names, not positions; unknown columns are skipped and listed in the import report.
- **Optional second way**: connect to a running TeslaMate Postgres (host, database, user, password,
  TLS required) and read the same tables, for owners who still run it.
- **Tables taken**: `cars` (VIN, model, trim, name, efficiency), `positions`, `drives`,
  `charging_processes`, `charges`, `states`, `updates`, `addresses`, `geofences`, charge costs and
  tariffs, car settings that still mean something (units, names). Tesla tokens (`tokens`, `private.*`)
  are **never** read or kept, even encrypted.
- **Matching cars**: by VIN to the car the CAN Bridge reports (VIN from 0x405 / 0x508). A car with no
  CAN data yet is created from the import and joined later when its VIN shows up.
- **Kept apart, marked**: every imported row is tagged `source = teslamate` (unsigned, API resolution,
  a point every few seconds). The UI shows a small "from TeslaMate" mark on those drives and charges.
  Where imported and CAN history overlap (the owner ran both at once), CAN wins for that span and the
  imported rows for it are hidden, not deleted; Settings shows the overlap and can flip it.
- **Re-runnable**: importing the same or a newer backup again adds only what's new (rows keyed by
  TeslaMate's own ids + car), so the owner can keep TeslaMate running until happy, then stop it.
- **Stored small**: imported positions and charge points go into the same compressed segment files as
  CAN data; drives and charges into SQLite.
- **Report**: rows read, imported, skipped (with why), overlap found, time taken; kept in the audit log.
- **Tests**: synthetic TeslaMate dumps made by the official TeslaMate image in CI (see This repo), fake
  coordinates, several TeslaMate versions; then the owner's real backup, uploaded to a session
  only if the owner chooses (it holds every place the car has been).

## Encryption at rest (owner: set once, extremely hard to break, 2026-11-16)

Set up once on first start; after that the logger unlocks itself on every restart, with nothing to type.
Stealing the disk, the data folder or a backup gives no readable contents. File and folder names, sizes and times are
not sealed yet: they show each car key and when each car recorded (see docs/security.md, Privacy).

- **Keys in layers.** One random 256-bit data key encrypts everything. It is never stored as it is: it is
  kept wrapped (encrypted) twice, and either wrapper opens it:
  1. **Machine unlock (daily use), in Docker**: by default a **key file in its own volume, `/keys`**,
     separate from the data volume `/data` (ideally on another disk, and never in the same backup), made
     on first start, readable only by the container's user. Copying `/data` or a backup of it gives
     nothing. If the host has a **TPM 2.0** chip and `/dev/tpmrm0` is passed to the container, the key
     file itself is sealed to that chip, so even `/data` + `/keys` together only open on that machine.
     The logger says at start which of the two it uses.
  2. **Recovery key**: shown **once** at setup (a long random key, also as a printable sheet), turned
     into a wrapping key with Argon2id at heavy settings (1 GiB memory, several passes) so guessing is
     out of reach. Needed only for a new machine, a dead TPM or a restore. The logger never stores it.
- **What is encrypted**: the SQLite database (SQLCipher: AES-256 per page with an HMAC on every page,
  the one C library, chosen because it is widely audited), the signal segments and the raw uploaded logs
  (XChaCha20-Poly1305 per file chunk, with a per-file key derived from the data key with HKDF, file name
  and chunk number bound in, so a swapped, cut or changed chunk is refused). Uploaded logs still verify
  against their signature after decryption. Temporary and `.part` files are encrypted too; nothing is
  ever written in the clear.
- **Backups** are encrypted with the same data key; a restore on another machine asks for the recovery
  key once, then seals to the new machine.
- **In memory**: keys held in locked memory (`mlock`), wiped on exit (`zeroize`), core dumps off.
- **Changing it**: new recovery key, or move to a new machine (re-seal), from Settings, password and
  two-step sign-in asked. The data key itself can be rotated (everything re-encrypted in the background).
- **Honest limits**: this protects data on a stolen or copied disk or backup. It can't protect against
  someone who has taken over the running server (the key is in memory while it runs): that is what the
  rest of the Security section is for. **Lose both the machine (or its TPM / key file) and the recovery
  key and the data is gone for good**, by design; the setup page says so and asks the owner to confirm
  they saved the recovery key.

## This repo (owner: replace the fork entirely, 2026-11-16)

The TeslaMate fork's contents go; the Rust app takes the whole repo.

- **Clean-room**: no TeslaMate code, SQL, dashboards, translations, logos or text are copied into the
  new app. Its pages, rules and formulas are written fresh from this plan and from the car's data. That
  keeps the app free of TeslaMate's AGPL code and its trademark. (The importer only reads TeslaMate's
  backup *format*, which is data, not code.)
- **One removal commit**: the first commit of the build deletes everything except `docs/plans/`, then
  the Cargo workspace goes in. Git history stays as it is (no force-push, no rewritten `main`). If the
  owner wants the TeslaMate history gone too, that's a fresh repo or a force-push the owner does.
- **Import test files** can't come from the migrations once they're deleted, and copying them would
  copy TeslaMate code. Instead CI runs the official `teslamate/teslamate` image against a throwaway
  Postgres, lets it make its own schema, inserts fake rows, and `pg_dump`s that as the fixture (for a
  few TeslaMate versions). Nothing of TeslaMate is committed.
- **Repo name**: `teslamate---Sentry-manager` has TeslaMate's name in it; the owner renames it in GitHub
  (Settings → General) once the app has a name. GitHub keeps the old address redirecting. If GitHub
  still shows it as "forked from teslamate-org/teslamate", only GitHub support (or a new repo) removes
  that link.
- **Licence**: decided later (owner, 2026-11-16); clean-room keeps every choice open. Third-party
  bits keep their own: model3dbc (MIT), opendbc (MIT), uPlot (MIT), MapLibre (BSD-3), map and address
  data (ODbL, credited in the UI).
- **No new rules file** (owner, 2026-11-16): this repo gets no branch rules of its own.
- **GitHub Actions**: TeslaMate's workflows are removed with everything else (they'd publish TeslaMate
  images); new ones: `cargo test`, clippy, `cargo deny`/`audit`, fuzz smoke run, Docker image.

## Docker (owner: it runs in Docker, 2026-11-16)

One container, one image, nothing else to run (no database or Grafana container).

```yaml
services:
  sentry-mate:
    image: ghcr.io/skittles-fivem/sentry-mate:latest   # name follows the final app name
    restart: unless-stopped
    ports:
      - "10101:10101"     # web UI, HTTPS
      - "10100:10100"   # CAN Bridge uploads, HTTPS, closed while uploads are off
    volumes:
      - ./data:/data    # database, segments, raw logs (all encrypted)
      - ./keys:/keys    # the key file: keep it apart from data and its backups
    # devices: ["/dev/tpmrm0"]   # optional: seal the key to the host's TPM 2.0
    environment:
      - TZ=America/Chicago       # default time zone; each car can have its own
    read_only: true
    tmpfs: ["/tmp"]
    cap_drop: [ALL]
    security_opt: ["no-new-privileges:true"]
    user: "1000:1000"
```

- **Ports** differ from Sentry Manager's (9000 / 10000) so both can run on the same server; both are
  settings. SMO's second slot points at `https://<server>:10100/api/can/logs`.
- **Image**: static Rust binary (musl) on `scratch` / distroless, non-root, read-only root filesystem,
  amd64 and arm64, built and signed by GitHub Actions; tiny (target well under 50 MB without map data).
- **Offline map and address data** is downloaded once into `/data` from Settings, not baked into the
  image.
- **Health check** built in (`sentry-mate health`), graceful shutdown on `SIGTERM` (finish the upload
  chunk being written, flush SQLite).
- **Updates**: pull the new image and restart; database changes run by themselves at start, after an
  automatic encrypted backup.
- **First start**: the log prints a one-time setup code; the web UI (HTTPS) asks for it, creates the
  admin account, makes the keys and shows the recovery key once.

## Findings from the first real log (2026-11-16)

One log and its sidecar from a parked Model Y (columns /2, about a minute). Analysed in a session's
scratch space only; the log, its name, the VIN, the position and the car's readings stay out of the repo
(only rounded or relative figures here).

- **Integrity**: SHA-256, length, key fingerprint and ECDSA signature all check.
  The `trust` crate's first test vector can be built the same way from a synthetic log.
- **What the car was doing**: parked, awake, not driving, charge status signals all present. Only **can0** (~2,000 frames/s) and **can3** sent;
  **can1 (chassis) was silent** and there was no gear or speed message (0x118 / 0x257). So "asleep" can't
  just mean "no frames": the timeline needs per-bus states (vehicle bus awake + chassis asleep = parked,
  awake, e.g. charging or keeping the battery warm).
- **Battery % disagrees**: `UI_SOC` (0x33A) is the screen's %, while `SOCUI292` (0x292) says 100 % and
  `SOCave292` nearly that, far above it. Sentry Manager's `canNow` takes `SOCUI292` first, so its Battery
  stat may show 100 % here. `UI_idealRange` (about twice `UI_Range`) is also not believable (`UI_Range` is). Before the timeline trusts
  any SOC or range signal, check them against the car's screen at several levels; likely a 0x292 layout
  change in newer firmware (a candidate for `override.dbc`), or 0x292 is relative to the charge limit.
  Reported here only; nothing changed in Sentry Manager.
- **Decoded share**: 55,966 of 125,645 frames (45 %) decode with the Model 3/Y set (1,475 signals);
  200 of 286 IDs are unknown. Raw logs must stay the source of truth so these can be decoded later.
- **Clocks**: the wall-clock to monotonic offset is steady to about 1 ms across the whole file, and the
  helper's clock is within 7 ms after 4 days running. But the file has **20 `# clock … jump` lines**, in
  pairs about 425 ms apart in monotonic time with the same wall time: not real jumps. They look like the
  logger being paused (about 0.4 s, roughly every 17 s, plus one 1.55 s gap between frames) between
  reading one clock and the other. Sentry Mate must not treat a lone `jump` pair as a clock change; use
  the helper-clock lines and the median offset. Worth fixing in SMO (read monotonic, wall, monotonic and
  keep the pair only when the two monotonic reads are close); not done here.
- **Frames start 1.5 s before `start_wall_ms`** (queued frames): file spans overlap by a moment; the
  timeline orders by frame time, not by file.
- **Compression, measured on this log**:

  | What | Bytes |
  |---|---|
  | candump text | 5,781,525 |
  | candump text, xz -6 / -9e | 490,356 / 500,484 |
  | columns /2 as uploaded (xz -6) | 232,920 |
  | columns /2 body, xz -9e | 232,840 (0.04 % smaller) |
  | ... of which: frame times | ~134,800 (58 %) |
  | ... which key each frame is | ~44,300 |
  | ... payload bytes | ~51,900 |
  | every decoded signal, on change + once a second, ms, xz -9e | 99,828 |
  | every decoded signal, on change only, ms, xz -9e | 87,652 |

  Conclusions: **/2 is already at the floor for lossless.** The frame times carry about 9 bits of real
  information each (helper-receive jitter at microsecond resolution), so no layout can shrink them
  without losing exactness. Per-message delta-of-delta times and XOR'd payloads were tried and came out
  **worse** (~350 KB). No `/3` format for SMO, then; keep the uploaded logs as they are. The savings are
  in what Sentry Mate derives: decoded signals at ms resolution are ~38 % of the raw log, and the 1 Hz
  car samples and drives/charges far smaller. zstd wasn't on this machine; it gets measured in Phase 4
  for decode speed (likely zstd for derived data the UI reads often, xz for cold archives).
- **Storage estimate**: about 0.23 MB/min parked and awake, ~0.76 MB/min driving (SMO's figure), plus
  ~0.1-0.3 MB/min derived. A year of a car that is awake ~6 h a day is roughly 50-120 GB raw; retention
  settings matter more than codecs.

## Progress

- 2026-11-16: TeslaMate contents removed. Built and tested: `logfmt` (both log formats, decodes SMO's columns
  fixture and a real log byte for byte like SMO's reference decoder, 0.06 s per one-minute log), `trust`
  (signatures, pairing, chain report; verifies a real Screenmate signature), `ingest` (upload API, storage, tokens,
  trust records, TLS 1.3 server), the `sentry-mate` program and its Docker image (6.6 MB, `scratch`, non-root,
  read-only). A real log and sidecar uploaded end to end in chunks came out verified and paired. Phase 1 is done
  except encryption at rest (Phase 6). Next: Phase 2, decoding signals (DBC).
- 2026-11-16: encryption at rest done in pure Rust (owner: no SQLCipher), crate `vault`: one data key wrapped by
  the key file (`/keys`, its own volume, refused inside the data folder) and by the recovery key (Argon2id, 1 GiB,
  3 passes; about 3 s once). XChaCha20-Poly1305 STREAM in 64 KiB chunks, a key per file (HKDF-SHA256), the file's
  path bound in; growing files (uploads in progress) as sealed records that survive a crash mid-write. Everything
  stored is sealed: logs, sidecars, partial uploads, trust records, tokens, the TLS key (the certificate itself is
  public). Files from an older version are sealed on start. The recovery key goes to `/keys/RECOVERY-KEY.txt`, not
  the log. Not done: TPM sealing of the key file (optional, later). When a database arrives (Phase 3) it is sealed
  the same way (sealed segment files, no C library).
- 2026-11-16: Phase 2 done, crate `dbc`: Sentry Six Manager's DBC sets (`files/`, with their licences), parser,
  Intel/Motorola, signed, multiplexed, value names, series names, the bus split and the internal bus's 0x04F rule.
  Checked bit for bit against Sentry Six Manager's own decoder: a synthetic log reaching every message of the Model
  3/Y set (1,568 series), SMO's columns fixture, and a real Model Y log (1,486 series, 470,944 values; checked
  locally, not committed). `sentry-mate log decode` gives a stored log's signals as CSV (0.14 s a minute). Next:
  Phase 3, the timeline (states, drives, charges), and choosing the DBC set from the VIN.
- 2026-11-16: built in parallel, each in its own crate, tested, not yet wired into the program:
  - `timeline`: one sample a second from the decoded signals (each value from the best signal that has it, units
    taken from the DBC), the VIN and the car's model, states (Online/Asleep/Unknown, Unknown never counted as sleep),
    drives (gear out of P until 60 s back in P), charges (pauses under 3 min merged, a drive always splits), positions,
    charge curves, and rebuilding only the window a late log touches (same result as a full build). 2,000 cars in
    parallel: about 82 µs a car.
  - `db`: per-car folders, a bounded cache of open cars, a lock per car, append-only journals folded into snapshots,
    a fleet summary that lists and searches without opening any car; time series as columns then brotli then sealed
    (a driving day about 73 KB, a parked day asleep about 4 KB); sealed backups. zstd has no pure-Rust compressor, so
    brotli (measured against deflate, lz4 and xz).
  - `places`: geofences on a grid (any number of places, about 2 µs a lookup), parking suggestions, prices with
    time-of-use rates in the car's time zone (daylight saving handled), addresses offline (GeoNames, downloaded on
    the owner's request) or online (Nominatim, at most one request a second, positions rounded to about 11 m), map
    tile settings.
  - `accounts`: admin and watch-only, per-account car lists of any size, Argon2id passwords, sessions (`__Host-`
    cookie, CSRF), two-step codes with recovery codes, throttling per address and per account (home and outside
    counted apart), one-time setup code, a sealed audit log. 2,000 accounts tested.
- 2026-11-16: the upload side scales to any number of cars (`ingest`). Tokens: no cap, one sealed file per token
  named by its SHA-256 (finding one opens one file; a revoke from the command line counts at the next request), a
  bounded cache. Trust records: per key a small sealed head (pairing, logs waiting, overrides), a sealed records file
  in name order and an append-only sealed journal (`vault::records`, a crash-cut record is dropped), folded into the
  next generation every 2,048 changes (streamed, the head is the commit point); only heads are cached (4,096), a lock
  per key; the sweep looks only at keys with a "waiting" marker; old whole `trust.json` files are moved over on first
  use. Store: each car's log count in a sealed `counts.json` (a car stored before is counted once), unfinished uploads
  in the car's own `uploading/` folder, so an upload never lists the car's folder and `clean_stale` looks only there.
  Rate windows keep at most limit + 1 times per address or token and at most 100,000 entries (IPv6 by /64). The
  start-up sealing pass of older files runs once. Measured in a debug build: 2,000 cars with a token each, uploading
  in parallel through the router, about 4 ms a car with every cache at 64; a car with 50,000 logs uploads as fast as
  an empty one and its folder is never listed.
- 2026-11-16: `import` and `mqtt` built, reviewed, not yet wired in.
  - `import`: reads TeslaMate's own backup (`pg_dump` plain SQL, also gzip or xz) as a stream, without running any
    SQL: only the `COPY` blocks, columns matched by name so any TeslaMate version works, the private schema (tokens)
    skipped unread. Cars, drives, charges and their curves, states, updates, positions, places, all marked as
    imported so a CAN rebuild never deletes them; units checked against TeslaMate's schema. Importing a newer backup
    again adds only what is new, and a backup of a different TeslaMate database is flagged rather than skipped. xz is
    read with a bounded window (one-block xz files stay at a few MB of memory).
  - `mqtt`: off unless turned on. TLS only (plain `mqtt://` only with an explicit setting), the broker's certificate
    checked or pinned by SHA-256, Home Assistant discovery, location published only if chosen separately. One
    connection for every car; while the broker is away only each car's latest values wait (events are kept, bounded),
    so thousands of cars can't flood it. Reconnects back off for real, messages are kept under the packet limit.
  - Reviews also fixed the four crates merged earlier (timeline, db, places, accounts) and the upload side.
  - Found in review and fixed (`vault::records`, format 2): a growing sealed file cut by a crash could seal the next
    piece under a nonce already used. Now every record has its own random 24-byte nonce, and its tag is bound to its
    number, the label and the tag of the record before it, so records can't be moved, dropped from the middle,
    copied between files or mixed from two versions of a file. Appends count only what reached the disk. Readers
    open read-only (never create, cut or change a file). Format 1 files are read and rewritten in format 2 when
    next added to; once this version runs on a data folder, older versions can't open it (keep a backup from before).

## Phases

0. **Decide** (questions below). Then the one removal commit (see This repo). Get a real bundle from the owner (Sentry Manager → Nerd → export
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
7. **TeslaMate import** (backup upload, then the optional live Postgres connection).
8. **Docker image, docs, first release**.

## Decisions (owner, 2026-11-16)

1. Name: **Sentry Mate** for now, not final.
2. Repo: replace the fork entirely, clean-room. Licence: later.
3. No new set of repo rules.
4. SMO's second upload slot is free: the logger uses it.
5. Encryption at rest: set once, unlocks by itself, very hard to break (see Encryption at rest).
6. Maps and addresses: offline and online.
7. Import old TeslaMate history: yes.
8. Several cars and accounts: yes, the Sentry Manager way.
9. Runs in **Docker**: one container (see Docker).

Phase 0 has its first real log. Still wanted from the owner: a bundle with a few drives and a charge in
progress, and the battery % shown on the car's screen at the time of a log (to settle the SOC signals).
