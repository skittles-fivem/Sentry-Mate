# Sentry Mate

A self-hosted car logger for Teslas, fed by the **Screenmate CAN Bridge**. The bridge app records the car's own CAN
bus and uploads the logs. Sentry Mate is meant to do what TeslaMate does, from those logs instead of Tesla's API: no
Tesla account, nothing that wakes the car, one Rust program in one small Docker image, no database or dashboard
containers.

**Status: early.** Today it receives the bridge's logs, stores them sealed and checks each one's signature. You can
decode one stored log at a time to CSV from the command line. There are no drives, charges, maps or web pages yet.
The parts that work those out are built and tested but not yet wired in. All of it:
[Status in detail](#status-in-detail).

## Contents

| Guide | What is in it |
|---|---|
| [docs/install.md](docs/install.md) | What you need, Docker Compose and TrueNAS step by step, updates, backups, restore, moving, ports and firewalls, when it does not start |
| [docs/commands.md](docs/commands.md) | Every command, the settings (environment variables and flags), ports and limits, the files it keeps, changing the certificate |
| [docs/cars-and-bridge.md](docs/cars-and-bridge.md) | Supported cars, setting up the CAN Bridge app, running next to TeslaMate and Sentry Six Manager |
| [docs/security.md](docs/security.md) | Security, privacy, when a log fails its check, a new signing key |
| [docs/development.md](docs/development.md) | Building, the gates, CI and the image check, the crates and the layout |
| [ui/README.md](ui/README.md) | The UI packs |
| [docs/plans/rust-can-logger.md](docs/plans/rust-can-logger.md) | The plan |

## Quick start

On TrueNAS SCALE, follow [TrueNAS SCALE](docs/install.md#truenas-scale) instead: a custom app from
[`truenas/sentry-mate.yaml`](truenas/sentry-mate.yaml), with two datasets owned by 568:568. The steps below are for
Docker Compose.

You need Linux with Docker and the Compose plugin, or TrueNAS SCALE 24.10 or newer, on amd64 or arm64, and the
Screenmate CAN Bridge app build 149 or later. All of it: [What you need](docs/install.md#what-you-need).

The image is published (`ghcr.io/skittles-fivem/sentry-mate`), so there is nothing to clone or build. If your user
is not in the `docker` group, put `sudo` before the `docker` commands. Make a folder for it:

```bash
mkdir sentry-mate
```

```bash
cd sentry-mate
```

Get the compose file:

```bash
curl -fsSLO https://raw.githubusercontent.com/skittles-fivem/Sentry-Mate/main/docker-compose.yml
```

It mounts two folders: `./data` at `/data` (everything, sealed) and `./keys` at `/keys` (the key file that opens it).
Both lines are needed. Put the keys on another disk if you can: change `./keys` to that folder's path. Make the two
folders and give them to the container's user, readable by nobody else:

```bash
mkdir -p data keys
```

```bash
sudo chown 1000:1000 data keys
```

```bash
sudo chmod 700 data keys
```

Start it, then read the log. It shows the upload address, the certificate SHA-256 and the vault's id, or why it
didn't start and what to do:

```bash
docker compose up -d
```

```bash
docker logs sentry-mate
```

**The recovery key.** The first start puts it in `keys/RECOVERY-KEY.txt` (not in the log). With the key file it is
the only way to open the data; lose both and nobody can. Read it, copy the line that starts with `SMRK-` somewhere
safe away from the server (a password manager, paper), then delete the file:

```bash
sudo cat keys/RECOVERY-KEY.txt
```

```bash
sudo rm keys/RECOVERY-KEY.txt
```

Every start logs a warning while that file is still there.

**An upload token** for the car (shown once). It prints the token's id, the token (it starts with `smt_`), and the
three fields for the app:

```bash
docker exec sentry-mate /sentry-mate token new --car "Model Y"
```

**The CAN Bridge app.** Fill in its **Second Sentry Six Manager (optional)** section, then press **Apply &
restart**. The app saves the fields only then.

| App field | What to put in |
|---|---|
| Upload address (empty = off) | `https://<this server>:10100/api/can/logs` |
| Token | The line that starts with `smt_`. |
| Certificate SHA-256 | The 64 hex digits that `token new` printed. |

`<this server>` is the server's name or LAN address, not the `0.0.0.0` in the log. `hostname -I` shows its
addresses; give it a fixed address in your router ([The CAN Bridge app](docs/cars-and-bridge.md#the-can-bridge-app)).

How to check it works: [Check it works](docs/cars-and-bridge.md#check-it-works). The full guide, with updates,
backups, restore and moving: [docs/install.md](docs/install.md).

## Web UI

Not served yet. The two UI packs run on made-up data.

- [`ui/README.md`](ui/README.md): the two packs, Instrument and Logbook. They run on made-up data from
  `web/mock-api.js`. To preview them: [Previewing the UI packs](docs/development.md#previewing-the-ui-packs).
- [`docs/parity/ui-parity.md`](docs/parity/ui-parity.md): every TeslaMate tab (51 tabs, 1161 items) placed on 23
  Sentry Mate pages, described in our own words. [`docs/parity/coverage-matrix.json`](docs/parity/coverage-matrix.json)
  is the same, machine-readable.
- [`docs/ui-api.md`](docs/ui-api.md): the `/api/v1` the pages need. Not built yet.

## Roadmap

From the plan's phases and progress:

1. Wire the built parts into the program: decode each log in the background after its check, with the DBC set chosen
   from the VIN, then work out and store states, drives and charges.
2. Check the timeline by hand against real drives and charges.
3. A web server for the UI packs and `/api/v1`, and the packs moved off their made-up data. Desktop first, phone
   layouts later.
4. Accounts, places, MQTT and the TeslaMate import in the program.
5. First release: the docs, a licence and the final name.

Open questions: two battery % signals disagree (0x33A and 0x292) and need checking against the car's screen. A
firmware-version signal for software updates is still to be found.

## Development

Rust 1.88 or newer, edition 2024. The gates CI runs, what each crate does, CI, the image check and the layout:
[docs/development.md](docs/development.md).

Crates: `logfmt` (reads the logs), `trust` (signatures, chain), `vault` (encryption at rest), `dbc` (signals),
`ingest` (upload API, storage), `model`, `timeline`, `db`, `places`, `accounts`, `import`, `mqtt`, and the program,
`sentry-mate`.

## Status in detail

| Part | State |
|---|---|
| **Working now** | |
| Upload API the bridge speaks (ping, status, resumable chunks), TLS 1.3 only | done |
| Logs kept exactly as uploaded, never replaced | done |
| Upload tokens: no limit, each tied to one car key (one bridge install) on its first upload | done |
| Any number of cars: kept per car, bounded memory, cars upload in parallel, an upload never reads a car's folder | done |
| Signature check, pairing with the car's key, chain report (gaps, swaps, restarts) | done |
| Trusting a new signing key for a car | done (`keys trust`) |
| Reading both log formats (candump text, columns) | done |
| Encryption at rest (pure Rust): every file's contents sealed (not the names), key file + recovery key | done |
| Command line: tokens, certificate, chain report, signing keys, log export and decode, restore, recovery key | done |
| Docker image: one static binary on `scratch`, non-root, health check; run read-only by the compose file | done |
| Published image for amd64 and arm64 (`ghcr.io/skittles-fivem/sentry-mate`), a compose file and a TrueNAS file | done |
| Refusing folders it can't keep: not mounted, the keys inside the data, root's | done |
| Decoding signals (DBC sets per model, bus split, the Screenmate's GPS), same values as Sentry Six Manager | done; used only by `log decode` |
| Choosing the DBC set from the car's VIN | done in `log decode` |
| Using a failed log anyway | only `log decode --any`, one log at a time |
| **Built and tested, not wired in yet** | |
| Drives, charges, states, positions, charge curves from the decoded signals; the car's model from its VIN | built; not yet checked against real drives and charges |
| Storage for everything worked out (sealed, compressed columns, any number of cars, backups) | built |
| Places, geofences, charge costs with time-of-use rates, addresses offline or online | built |
| Accounts: admin and watch-only, per-car access, two-step sign-in, sessions, audit log | built; passkeys planned |
| TeslaMate import from TeslaMate's own backup file (any number of cars, streamed, again later adds only what's new) | built; a live connection to TeslaMate's database is planned |
| MQTT for Home Assistant (TLS unless plain is turned on, never a password in the clear, discovery, any number of cars on one connection) | built |
| **Being built here** | |
| Web UI: UI packs, two designs as prototypes on made-up data in [`ui/`](ui/), maps; every TeslaMate tab gets a page, planned in [`docs/parity/ui-parity.md`](docs/parity/ui-parity.md) | being built; next, after the groundwork below: every page built in both designs and checked item by item, with screenshots |
| **Being built on separate working copies** (paused 2026-11-18, waiting for their last checks before they come into this repository) | |
| Moving to another server in one step: a one-time move code on the old server, and two lines in the new server's compose or TrueNAS file (address and code). The new server copies everything over the pinned TLS connection, keeps up with new uploads, then takes over, with the same certificate and tokens so the bridge keeps working. Also one export file that opens with the recovery key, never holding the key file. | designed, reviewed, built; the test with two servers in Docker and the security and crash reviews are still to run |
| Web UI groundwork: a dev API server that answers all of [`docs/ui-api.md`](docs/ui-api.md) on made-up data (139 tests), shared browser modules, a page checker that loads every page at laptop and 4K sizes and checks each TeslaMate item against [`docs/parity/coverage-matrix.json`](docs/parity/coverage-matrix.json), and both designs split into one module per page on the real API | the review was cut short |
| **Not built yet** | |
| Decoding each log as it arrives, after its check | planned |
| Web server and the `/api/v1` the pages need ([`docs/ui-api.md`](docs/ui-api.md)), on its own HTTPS port | planned |
| Listing stored logs and their check results, deleting logs, a backup command | not built |
| `cargo deny` and `cargo audit`, fuzzing in CI | planned |
| Sealing the key file to the host's TPM | optional, later |

## Licence

No licence has been chosen yet; the repo has no LICENSE file. Bundled third-party files keep their own:

- DBC files from model3dbc, opendbc and Open Vehicle Monitoring System's Model S module: MIT. `override.dbc` is from
  Sentry Six Manager. See [crates/dbc/files/README.md](crates/dbc/files/README.md).
- In the UI packs: MapLibre GL JS 6.18.0 (BSD-3-Clause), uPlot 1.6.32 (MIT), Inter (SIL Open Font License 1.1),
  JetBrains Mono (Apache License 2.0, per its bundled licence file), Bitstream Charter (Bitstream's free licence).

Sentry Mate is written from scratch and copies no code from TeslaMate; it is not affiliated with TeslaMate, Tesla,
or Screenmate.
