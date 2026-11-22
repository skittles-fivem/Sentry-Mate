# Progress

What Sentry Mate does today. What is left: [To do](todo.md). Back to the [README](../README.md).

## Working now

| Part | State |
|---|---|
| Upload API the bridge speaks (ping, status, resumable chunks), TLS 1.3 only | done |
| Logs kept exactly as uploaded, never replaced | done |
| Upload tokens: no limit, each tied to one car key (one bridge install) on its first upload | done |
| Any number of cars: kept per car, bounded memory, cars upload in parallel, an upload never reads a car's folder | done |
| Signature check, pairing with the car's key, chain report (gaps, swaps, restarts) | done |
| Trusting a new signing key for a car | done (`keys trust`) |
| Reading both log formats (candump text, columns) | done |
| Encryption at rest (pure Rust): every file's contents sealed (not the names), key file + recovery key | done |
| Command line: tokens, certificate, chain report, signing keys, log export and decode, restore, recovery key, move, export, import, health | done |
| Docker image: one static binary on `scratch`, non-root, health check; run read-only by the compose file | done |
| Published image for amd64 and arm64 (`ghcr.io/skittles-fivem/sentry-mate`), a compose file and a TrueNAS file | done |
| Refusing folders it can't keep: not mounted, the keys inside the data, root's | done |
| Moving to another server in one step: a one-time move code, or one export file that opens with the recovery key ([docs/moving.md](docs/moving.md)) | done |
| Decoding signals (DBC sets per model, bus split, the Screenmate's GPS), same values as Sentry Six Manager | done |
| Choosing the DBC set from the car's VIN (Model S and X by the VIN's 8th character, so a 2021 car of either kind gets the right set) | done |
| Vehicle info offline: model, year, plant, body, battery and chemistry, pack energy, motors, trim, EPA range, wheels, paint and more, from the VIN's first 11 characters, the car's own CAN configuration, the Tesla API, an import and what an admin types, each with its source and confidence (`vehicle` in `/cars/{car}`, [cars-and-bridge.md](cars-and-bridge.md#what-the-server-knows-of-each-car)) | done on the server; the pages show it later; the CAN configuration not yet checked on a real log |
| Each checked log decoded in the background into the car's history (states, drives, charges, positions, charge curves), late and out-of-order logs rebuilding only what they touch, picked up again after a restart; a failed log never used | done; not yet checked against real drives and charges |
| Storage for the history (sealed, compressed columns, any number of cars), rebuilt from the logs if lost | done |
| Car firmware fingerprints from the logs (the front body controller's, the battery's and both drive units' CRCs and git hashes on the vehicle bus, a value taken once seen 3 times in an awake span), their history, and "Tesla update installed" found where they changed, with the changed modules; the owner names the version, later updates to the same fingerprints get it by themselves; old logs too, rebuilt if lost (`/cars/{car}/firmware`, `/cars/{car}/updates`) | done, server side; not yet in the designs, not yet checked against a real update |
| Web server on its own HTTPS port (10101, TLS 1.3, the same certificate): the built-in UI packs with a strict CSP, sign-in with a one-time setup code for the first admin, accounts (admin and watch-only, per-car access, two-step sign-in, audit log), live changes as server-sent events, the reads of [`docs/ui-api.md`](docs/ui-api.md) over the history and its changes | done |
| From the web UI: places, geofences and charge costs with time-of-use rates, car settings, upload tokens and car keys, server settings, MQTT, Tesla accounts, the TeslaMate import, UI packs | done |
| The Tesla Owner API as a second live input: off until an admin adds a Tesla account (sign in on Tesla's page, or a refresh token), tokens sealed, read-only, lets the car sleep, merged with SMO into one history, every row saying where it came from | done; tested against a made-up Tesla server, not yet against a real account |
| Moving from the web UI: a move code or an export file on the old server, "move here" or "restore from an export file" on the new server's first page (the file read as it uploads, nothing stored twice) | done |
| Map tiles: none from outside until an admin turns online tiles on | done |
| Using a failed log anyway | only `log decode --any`, one log at a time |
| TeslaFi's API as a third live input, for owners who pay for TeslaFi: off until an admin adds a TeslaFi token, token sealed, read-only (two fixed reads, never a wake or a command), only `www.teslafi.com`, merged after SMO and the Tesla API, every row saying "Data from TeslaFi" | server and dev server done; tested against a made-up TeslaFi server, not yet against a real account; the pages in a later step |
| TeslaMate import from TeslaMate's own backup file, uploaded in the web UI (any number of cars, streamed, again later adds only what's new) | done; a live connection to TeslaMate's database is planned |
| TeslaFi import from TeslaFi's data download (its monthly CSVs, one or many in a zip; drives, charges, states and updates worked out from the polls; units and time zone from the file or the owner; again later adds only what's new) | server and dev server done; the Import pages show it in a later step |
| Import of the CAN Bridge logs Sentry Six Manager already has, from its CAN export bundle (`.zip`), uploaded in the web UI: each log checked (signature, pairing, chain) and stored as if the bridge had uploaded it, refused when unsigned or changed, never twice ([how](docs/cars-and-bridge.md#bringing-in-the-logs-sentry-six-manager-already-has)) | done; not yet tried on a real export |
| MQTT for Home Assistant (TLS unless plain is turned on, never a password in the clear, discovery, any number of cars on one connection), set up in the web UI; besides state, battery, range, charging, position and temperatures it sends doors, trunk, frunk, Sentry mode, screen mode, climate and preconditioning, cable and charge port, the charge status, limit, current, voltage, phases and time to full, the build (paint, wheels, roof, spoiler), heading, elevation, a low tyre warning, navigation and the logger's health, each a Home Assistant sensor or on/off sensor ([topics](docs/ui-api.md)) | done; lock and windows are not on CAN and never sent |
| A dev API server that answers [`docs/ui-api.md`](docs/ui-api.md) on made-up data, shared browser modules, and a page checker that loads every page at laptop and 4K sizes against [`docs/parity/coverage-matrix.json`](docs/parity/coverage-matrix.json) | done |

## Being built

| Part | State |
|---|---|
| Web UI: five designs in [`ui/`](ui/), built into the server; every TeslaMate tab gets a page, planned in [`docs/parity/ui-parity.md`](docs/parity/ui-parity.md) | being built: every page in all five designs, checked item by item, with screenshots |
