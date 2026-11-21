# Sentry Mate

A self-hosted car logger for Teslas, fed by the **Screenmate CAN Bridge** (SMO). The bridge app records the car's
own CAN bus and uploads the logs. Sentry Mate is meant to do what TeslaMate does, from those logs: nothing that wakes
the car, one Rust program in one small Docker image, no database or dashboard containers.

Two more inputs are optional. **The Tesla API:** an admin can add a Tesla account, and the server then reads the cars
from Tesla's Owner API too, for cars without SMO or for what SMO does not see. It is off until an account is added,
talks only to Tesla, and never wakes a car ([Add a Tesla account](docs/install.md#add-a-tesla-account)). **An
import** of TeslaMate's history. Every drive, charge and value says where it came from: SMO, the Tesla API, or an
import.

**Status: early.** It receives the bridge's logs, stores them sealed and checks each one's signature. Each checked
log is decoded in the background into the car's history (states, drives, charges, positions). The web UI on its own
HTTPS port (10101) shows that history and changes the settings: places and costs, car settings, upload tokens and car
keys, accounts, MQTT, Tesla accounts, the TeslaMate import and the import of Sentry Six Manager's CAN logs. The history is not yet checked against real drives and charges. All of
it: [Progress](docs/status.md). What's left: [To do](docs/todo.md).

## Contents

| Guide | What is in it |
|---|---|
| [docs/status.md](docs/status.md) | Progress: what works today |
| [docs/todo.md](docs/todo.md) | To do: what is being built now, what comes next, open questions |
| [docs/install.md](docs/install.md) | What you need, Docker Compose and TrueNAS step by step, updates, backups, restore, moving, ports and firewalls, when it does not start |
| [docs/commands.md](docs/commands.md) | Every command, the settings (environment variables and flags), ports and limits, the files it keeps, changing the certificate |
| [docs/moving.md](docs/moving.md) | Moving to another server in one step: a move code (TrueNAS: two lines), or one export file; the CAN Bridge app, undo, when it stops half way |
| [docs/cars-and-bridge.md](docs/cars-and-bridge.md) | Supported cars, setting up the CAN Bridge app, running next to TeslaMate and Sentry Six Manager, bringing in the logs Sentry Six Manager already has |
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

**An upload token** for the car (shown once). Make it in the web UI (**Settings → Cars**, see [Web UI](#web-ui)), or
with this command. It prints the token's id, the token (it starts with `smt_`), and the three fields for the app:

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

Open `https://<this server>:10101/`. It is HTTPS only (TLS 1.3), with the same certificate as the upload port, so the
browser warns once: check that the SHA-256 it shows is the one in the log.

**First start.** With no account yet, the log shows a one-time setup code, and the same code is in `SETUP-CODE.txt`
in the keys folder:

```bash
docker logs sentry-mate 2>&1 | grep 'first-run set-up code'
```

Type it on the first page, with a name and password for the first admin. The code is valid 24 hours; the server
makes a new one while nobody has set it up. Any password that isn't empty is taken: the page says when one is easy
to guess (short, close to the name, very common) and lets you use it anyway. Argon2id hashes, the sign-in throttles,
HTTPS only and optional two-step sign-in keep it out of reach from outside ([docs/security.md](docs/security.md)). More: [The web UI](docs/commands.md#the-web-ui).

**Moving.** To move to another server, use a move code or an export file: [docs/moving.md](docs/moving.md). It works
from the web pages (Settings, and the first page of the new server: move here, or restore from an export file) or
with the `move`, `export` and `import` commands.

- [`ui/README.md`](ui/README.md): the five packs built into the server: Instrument (dark glass, the default),
  Logbook (paper and ink), Horizon (neon and a dusk sky), Console (keyboard first, monospace) and Pebble (soft
  tiles, touch first). To preview them on made-up data: [Previewing the UI packs](docs/development.md#previewing-the-ui-packs).
- [`docs/parity/ui-parity.md`](docs/parity/ui-parity.md): every TeslaMate tab (51 tabs, 1161 items) placed on 23
  Sentry Mate pages, described in our own words. [`docs/parity/coverage-matrix.json`](docs/parity/coverage-matrix.json)
  is the same, machine-readable.
- [`docs/ui-api.md`](docs/ui-api.md): the `/api/v1` the pages use, reads and changes.

## Development

Rust 1.88 or newer, edition 2024. The gates CI runs, what each crate does, CI, the image check and the layout:
[docs/development.md](docs/development.md).

Crates: `logfmt` (reads the logs), `trust` (signatures, chain), `vault` (encryption at rest), `dbc` (signals),
`ingest` (upload API, storage), `gate` and `moving` (export, import, move), `model`, `timeline`, `db`, `engine` (the
history from the logs), `places`, `accounts`, `import`, `mqtt`, `tesla` (the Tesla Owner API input), `web` (the web UI and `/api/v1`), `synth` (made-up
cars for tests), and the program, `sentry-mate`.

## Licence

No licence has been chosen yet; the repo has no LICENSE file. Bundled third-party files keep their own:

- DBC files from model3dbc, opendbc and Open Vehicle Monitoring System's Model S module: MIT. `override.dbc` is from
  Sentry Six Manager. See [crates/dbc/files/README.md](crates/dbc/files/README.md).
- In the UI packs: MapLibre GL JS 6.18.0 (BSD-3-Clause), uPlot 1.6.32 (MIT), Inter, Orbitron, Chakra Petch, IBM Plex Sans and
  Manrope (SIL Open Font License 1.1), JetBrains Mono (Apache License 2.0, per its bundled licence file), Bitstream Charter (Bitstream's free licence).

Sentry Mate is written from scratch and copies no code from TeslaMate; it is not affiliated with TeslaMate, Tesla,
or Screenmate.
