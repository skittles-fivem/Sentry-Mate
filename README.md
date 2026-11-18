# Sentry Mate

A self-hosted car logger for Teslas, fed by the **Screenmate CAN Bridge**. The bridge app records the car's own CAN
bus and uploads the logs. Sentry Mate is meant to do what TeslaMate does, from those logs instead of Tesla's API: no
Tesla account, nothing that wakes the car, one Rust program in one small Docker image, no database or dashboard
containers.

**Status: early.** Today the program receives the bridge's logs, stores them sealed, and checks each one's
signature. You can decode one stored log at a time to CSV from the command line. There are no drives, charges, maps
or web pages yet. The parts that work those out, store them, and handle places and accounts are built and tested but
not yet wired into the program, and neither are the TeslaMate import and MQTT. The web UI is being built. The plan is
in [docs/plans/rust-can-logger.md](docs/plans/rust-can-logger.md).

| Part | State |
|---|---|
| Upload API the bridge speaks (ping, status, resumable chunks), TLS 1.3 only | done |
| Logs kept exactly as uploaded, never replaced | done |
| Upload tokens: no limit, each tied to one car key (one bridge install) on its first upload | done |
| Any number of cars: kept per car, bounded memory, cars upload in parallel, an upload never reads a car's folder | done |
| Signature check, pairing with the car's key, chain report (gaps, swaps, restarts) | done |
| Reading both log formats (candump text, columns) | done |
| Encryption at rest (pure Rust): every file's contents sealed (not the names), key file + recovery key | done |
| Command line: tokens, certificate, chain report, signing keys, log export and decode, restore, recovery key | done |
| Docker image: one static binary on `scratch`, non-root, health check; run read-only by the compose file | done |
| Published image for amd64 and arm64 (`ghcr.io/skittles-fivem/sentry-mate`), a compose file and a TrueNAS file | done |
| Refusing folders it can't keep: not mounted, the keys inside the data, root's | done |
| Decoding signals (DBC sets per model, bus split, the Screenmate's GPS), same values as Sentry Six Manager | done; used only by `log decode` |
| Decoding each log as it arrives, after its check | planned |
| Choosing the DBC set from the car's VIN | done in `log decode` |
| Drives, charges, states, positions, charge curves from the decoded signals; the car's model from its VIN | built, not wired in yet; not yet checked against real drives and charges |
| Storage for everything worked out (sealed, compressed columns, any number of cars, backups) | built, not wired in yet |
| Places, geofences, charge costs with time-of-use rates, addresses offline or online | built, not wired in yet |
| Accounts: admin and watch-only, per-car access, two-step sign-in, sessions, audit log | built, not wired in yet; passkeys planned |
| TeslaMate import from TeslaMate's own backup file (any number of cars, streamed, again later adds only what's new) | built, not wired in yet; a live connection to TeslaMate's database is planned |
| MQTT for Home Assistant (TLS unless plain is turned on, never a password in the clear, discovery, any number of cars on one connection) | built, not wired in yet |
| Trusting a new signing key for a car | done (`keys trust`) |
| Using a failed log anyway | only `log decode --any`, one log at a time |
| Listing stored logs and their check results, deleting logs, a backup command | not built |
| Web server and the `/api/v1` the pages need ([`docs/ui-api.md`](docs/ui-api.md)), on its own HTTPS port | planned |
| Web UI: UI packs, two designs as prototypes on made-up data in [`ui/`](ui/), maps; every TeslaMate tab gets a page, planned in [`docs/parity/ui-parity.md`](docs/parity/ui-parity.md) | being built |
| `cargo deny` and `cargo audit`, fuzzing in CI | planned |
| Moving to a new server in one step (a move code, or an export file) | being built |
| Sealing the key file to the host's TPM | optional, later |

### In progress, not in this repository yet (paused 2026-11-18)

Built and tested on separate working copies, waiting for a last check before they come here:

- **Published image and install.** An image workflow that builds for amd64 and arm64, checks each image (no
  capabilities, read-only root, TLS 1.3 only, as uid 1000 and 568) and only then publishes
  `ghcr.io/skittles-fivem/sentry-mate` as `latest`, `build-N` and `sha-<commit>`. A compose file that pulls it, a
  TrueNAS SCALE file for Install via YAML, and a step-by-step install guide. Done and tested in Docker; the last check
  of the README and the image was cut short.
- **Safety fixes.** A new vault is never made over an existing key file (a start with an empty or unmounted data
  folder stops and says so). A forgotten mount, or a keys folder inside the data folder, stops with a plain message.
  Folders Sentry Mate can't write to name the chown to run. `keys` and `keys trust` trust a new signing key;
  `log decode` uses only trusted logs and the signal set from the car's VIN. Built and tested; same last check.
- **Moving to another server in one step.** A one-time move code on the old server, and two lines in the new server's
  compose or TrueNAS file (address and code): the new server copies everything over the pinned TLS connection, keeps
  up with new uploads, then takes over, with the same certificate and tokens so the bridge keeps working. Also one
  export file that opens with the recovery key, never holding the key file. Designed, reviewed, built; the test with
  two servers in Docker and the security and crash reviews are still to run.
- **Web UI groundwork.** A dev API server that answers all of [`docs/ui-api.md`](docs/ui-api.md) on made-up data
  (139 tests), shared browser modules, a page checker that loads every page at laptop and 4K sizes and checks each
  TeslaMate item against [`docs/parity/coverage-matrix.json`](docs/parity/coverage-matrix.json), and both designs
  split into one module per page on the real API. The review was cut short.
- **Next after those:** every page of [`docs/parity/ui-parity.md`](docs/parity/ui-parity.md) built in both designs
  and checked item by item, with screenshots.

## What you can do today

- Receive every log the CAN Bridge uploads and keep it sealed, exactly as uploaded.
- See each upload token and the car key it is tied to (`token list`).
- See a car key's chain report: gaps, failed logs, restarts (`chain`).
- See which keys signed a car key's logs, and trust a new one (`keys`, `keys trust`).
- Get a stored log back as uploaded (`log export`), or its decoded signals as CSV (`log decode`).
- Turn a log file into candump text (`log candump`), or check its signature (`log check`).

No command lists cars or logs yet. File and folder names are not sealed, so you find them on the host (see
[Commands](#commands)).

## Requirements

- A server that is on when the car is home: Linux with Docker and the Compose plugin (`docker compose`), or TrueNAS
  SCALE 24.10 or newer. The image is for amd64 and arm64 (for example a Raspberry Pi 4 or 5 with a 64-bit system).
- No Rust and no clone of this repo: the image is published, and you only download the compose file. Internet is
  needed to pull the image and its updates. While it runs, Sentry Mate makes no outgoing connections.
- Building the image yourself is optional (the OPTIONAL `build: .` line in `docker-compose.yml`, see
  [Development](#development)). That needs a clone, internet for the Rust build image and crates, and about 3 GB of
  free disk.
- About 1 GiB of free memory for a few seconds when the vault is made (the first start), and for `restore` and
  `recovery-key new`: the recovery key is stretched with Argon2id at 1 GiB. A container memory limit below that can
  make these steps fail.
- Disk: the plan measured about 0.23 MB a minute parked and awake, and about 0.76 MB a minute driving. That is
  roughly 30 to 100 GB a year for a car awake 6 hours a day. Logs are kept; no command deletes them yet. A new upload
  is refused while free space is under twice its size plus 2 GiB.
- Logs: Sentry Mate takes up to 200,000 per car key. The bridge writes about one log a minute while the car is
  awake, so that is about 18 months at 6 hours a day, and 4 to 5 months for a car awake all day (for example in
  Sentry Mode). After that every new log is refused, and no command frees room yet.
- The Screenmate CAN Bridge app, build 149 or later (it has the **Second Sentry Six Manager (optional)** section),
  able to reach this server. The build number is at the top of the app. Update it in the app's **Updates** section.

## Run it

**Step by step, with TrueNAS, backups, restore and moving: [docs/install.md](docs/install.md).** This is the short
version for Docker Compose; TrueNAS is [further down](#truenas-scale).

Sentry Mate is a published image, `ghcr.io/skittles-fivem/sentry-mate`. There is nothing to clone or build. Make a
folder for it:

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

It runs the image as uid 1000 (group 1000), with a read-only root and no capabilities, and publishes port 10100. It
mounts two folders: `./data` at `/data` (everything, sealed) and `./keys` at `/keys` (the key file that opens it).
Both lines are needed. Put the keys on another disk if you can: change `./keys` to that folder's path.

Make the two folders and give them to the container's user, readable by nobody else:

```bash
mkdir -p data keys
```

```bash
sudo chown 1000:1000 data keys
```

```bash
sudo chmod 700 data keys
```

Start it:

```bash
docker compose up -d
```

The first start makes the vault. That takes a few seconds, with only the version line in the log until it is done.
Then the log shows the upload address, the certificate SHA-256 and the vault's id:

```bash
docker logs sentry-mate
```

If it doesn't start, the log says why and what to do. For example `Sentry Mate runs as uid 1000 (group 1000) and
can't write to the data folder /data: it belongs to uid 0 (group 0), root.` means the folders belong to root (Docker
makes missing ones owned by root). Run the `chown` and `chmod` above, then restart it:

```bash
docker compose restart
```

The other messages are in the install guide's [When it does not start](docs/install.md#when-it-does-not-start). If
your user is not in the `docker` group, put `sudo` before the `docker` commands.

### The recovery key

On the first start Sentry Mate puts the **recovery key** in `keys/RECOVERY-KEY.txt` (not in the log). With the key
file it is the only way to open the data; lose both and nobody can. Read it:

```bash
sudo cat keys/RECOVERY-KEY.txt
```

Copy the line that starts with `SMRK-` somewhere safe away from the server (a password manager, paper), then delete
the file:

```bash
sudo rm keys/RECOVERY-KEY.txt
```

Every start logs a warning while that file is still there.

### An upload token

Make a token for the car (shown once):

```bash
docker exec sentry-mate /sentry-mate token new --car "Model Y"
```

It prints the token's id, the token (it starts with `smt_`), and the three fields for the app.

### The CAN Bridge app

`<this server>` in the address below is the server's name or LAN address, not the `0.0.0.0` in the log. This shows
its addresses:

```bash
hostname -I
```

Give the server a fixed address in your router, so uploads don't stop when it changes.

Fill in the app's **Second Sentry Six Manager (optional)** section:

| App field | What to put in |
|---|---|
| Upload address (empty = off) | `https://<this server>:10100/api/can/logs` |
| Token | the token |
| Certificate SHA-256 | the SHA-256 that `token new` printed |

Then press **Apply & restart** at the top of the app. The app saves the fields only then.

- The bridge then sends every log to Sentry Mate as well as to Sentry Six Manager, each on its own.
- **Upload only on Wi-Fi**, in the app's **Sentry Six Manager (CAN log upload)** section, holds uploads to Sentry
  Mate too.
- `token new` prints the port inside the container. If you map another host port (for example `"10200:10100"`), use
  the host port.
- The certificate names only `localhost` unless `SENTRY_MATE_NAMES` was set when it was made, but the app pins the
  SHA-256 and does not check names.
- Sentry Mate accepts both log formats, so the bridge keeps writing the smaller columns format. It writes that
  format only while both servers accept it.
- While a server can't be reached, logs wait on the Screenmate up to the app's **Keep up to (GB)** (default 15).
  Then the oldest are removed, first those both servers have. So if Sentry Mate is down for long, it can miss logs.
- The CAN Bridge's Raspberry Pi logger uploads the same way. It has one upload address, so it sends either to
  Sentry Six Manager or to Sentry Mate. It needs a token of its own: its car key is not the Screenmate's.
- No Sentry Six Manager? Sentry Mate speaks the same upload API, so the app's first slot should work too. This is
  not tested.

The bridge finishes a log about every minute while the car is awake, and none while it sleeps. Wake the car (open a
door) and wait a minute or two. The app's **Second Sentry Manager upload** card shows how it is going;
**Upload now** tries at once.

Check that the bridge reached Sentry Mate. The token's last column changes from `not tied to a car key yet` to the
car key:

```bash
docker exec sentry-mate /sentry-mate token list
```

The token is tied when the first piece of a log comes in, before it is stored. So this shows that the bridge got
through with this token, not that a whole log arrived. The logs that arrived are in the car key's folder:

```bash
sudo ls data/can/<car key>
```

### TrueNAS SCALE

On TrueNAS SCALE 24.10 or newer it is a custom app, from [`truenas/sentry-mate.yaml`](truenas/sentry-mate.yaml).
The install guide's [TrueNAS SCALE](docs/install.md#truenas-scale) part has every step.

- Make two datasets, one for the data and one for the keys (on another pool if you have one, never inside the data
  one). Give both to the apps user: `sudo chown 568:568` and `sudo chmod 700` on their paths, in *System → Shell*.
- *Apps → Discover Apps →* the three dots *→ Install via YAML*. Name it `sentry-mate`, paste the file and change
  the two lines marked `CHANGE` to your datasets' paths.
- The commands in this README work in *System → Shell* with `sudo` in front, for example
  `sudo docker exec sentry-mate /sentry-mate token new --car "Model Y"`. The recovery key file is in the keys
  dataset.

### Updates

Docker Compose, in the folder with `docker-compose.yml`:

```bash
docker compose pull && docker compose up -d
```

TrueNAS: fetch the new image in *System → Shell*, then *Stop* and *Start* the app (if the version didn't change,
*Edit* the app and *Save*):

```bash
sudo docker pull ghcr.io/skittles-fivem/sentry-mate:latest
```

Every image has a build number. `--version` shows it, for example `sentry-mate 0.1.0 (build-1001, sha-6f83191,
2026-11-17T19:06:35Z)`, and the log shows the same at every start:

```bash
docker exec sentry-mate /sentry-mate --version
```

Each image is also tagged `build-N` and `sha-<commit>`. To stay on a version, pin one of those tags: in a `.env` file
next to `docker-compose.yml` (`SENTRY_MATE_TAG=build-1001`), or on TrueNAS in the app's `image:` line in place of
`latest`. More in [Upgrades](#upgrades).

## Commands

Run them in the running container with `docker exec sentry-mate /sentry-mate ...`. That works from any folder, and
on TrueNAS in *System → Shell* with `sudo` in front. `init` and `restore` run while Sentry Mate is stopped, with
`docker compose run --rm` in the folder with `docker-compose.yml` (the TrueNAS restore is in
[docs/install.md](docs/install.md#restore)). `log check` runs in a container of its own (`docker run --rm`).

Exit code 0 means it worked, 1 that it failed, 2 a usage error. The reason for a failure is on standard error, except
`log check`, which prints it on standard output, and `health`, which prints nothing.

| Command | What it does |
|---|---|
| `serve` | Runs the upload server until stopped (the image's default). Makes the vault on its first start. |
| `init` | Makes the vault now and prints the recovery key on screen, not into a file. Exit 1 if there is a vault or a key file already. |
| `restore` | Reads the recovery key from standard input and writes a new key file. Run it with Sentry Mate stopped. |
| `recovery-key new` | Prints a new recovery key. The old one stops working. |
| `token new --car NAME` | Makes an upload token (shown once) and prints the three fields for the app. |
| `token list` | One line per token: id, car name, `in use` or `revoked`, the car key it is tied to. |
| `token revoke ID` | Revokes a token. It stops working at its next request, with the server still running. |
| `fingerprint` | Prints the certificate SHA-256 for the app. |
| `chain KEY` | Prints a car key's chain report as JSON. Exit 1 for a car key with no logs here. |
| `keys KEY` | Lists the signing keys of a car key's logs, and which one is trusted. |
| `keys trust KEY SIGNING-KEY` | Trusts another signing key for a car key. It asks first; `--yes` doesn't. |
| `log export NAME` | Writes a stored log or sidecar, unsealed, to standard output. |
| `log decode NAME` | Writes a stored log's decoded signals as CSV to standard output. Only a verified or unsigned log, unless `--any`. `--model 3y` or `--model sx-legacy` picks the signal set. |
| `log candump FILE` | Writes a plain log file of either format as candump text. |
| `log check FILE [SIDECAR]` | Says what a plain log file's signature says. |
| `health` | Exit 0 when the upload port accepts a TCP connection on 127.0.0.1. Docker's health check runs it. |

### Tokens

A new token works at once, with no restart. List them:

```bash
docker exec sentry-mate /sentry-mate token list
```

Revoke one by its id:

```bash
docker exec sentry-mate /sentry-mate token revoke <id>
```

Revoked tokens stay in the list. No command deletes a token or ties it to another car key.

### The certificate

```bash
docker exec sentry-mate /sentry-mate fingerprint
```

### The chain report

The car key is the last column of `token list`:

```bash
docker exec sentry-mate /sentry-mate chain <car key>
```

It covers only logs signed by the key the car key is paired with, and names that key first (`signing_key`, as
`keys` lists it). Then: count, first and last index, issues (`gap`, `duplicate`, `failed`, `mismatch`,
`reordered`), restarts and segments. A car key with no signed logs gives an empty report with no `signing_key`. An
unknown or mistyped car key gives `no car key <key> here` and exit 1.

### Signing keys

The CAN Bridge signs each log with a key kept in the Screenmate. The first signed log of a car key pairs it with that
key. A log signed by another key is kept, marked failed, and not used. List the keys a car key's logs name:

```bash
docker exec sentry-mate /sentry-mate keys <car key>
```

Each key shows as the CAN Bridge app shows it under **Log signing key**, with how many logs it signed, when its
first and last log came, and whether it is trusted.

A new key can be right: the Screenmate's keystore made a new one. It can also be someone else's, who has the car's
upload token. Compare it with **Log signing key** in the app first. Only if they match, trust it:

```bash
docker exec -it sentry-mate /sentry-mate keys trust <car key> <signing key>
```

It shows what will change and asks you to type `yes` (`-it` lets you answer; `--yes` skips the question). Then the
logs that key signed are used, and so are its later logs. The key trusted before stays trusted for the logs that came
before; its later logs fail. It works with Sentry Mate running. Only a key that signed at least one of the car key's
logs can be trusted.

### Logs

Find a log's name on the host. Each car key has a folder:

```bash
sudo ls data/can
```

```bash
sudo ls data/can/<car key>
```

On TrueNAS, use your data dataset's path in place of `data`. A log is named
`<car key>_<UTC start, yyyyMMddTHHmmssZ>_<seq>.log.xz`, and its sidecar the same plus `.json`. Don't add `-t` or
`-it` when you send output to a file: a terminal would change the bytes.

What you export is not sealed, and logs hold the car's GPS position. Write it to a folder of your own, not into the
data or keys folder:

```bash
mkdir -p ~/sentry-mate-logs
```

A stored log, exactly as uploaded:

```bash
docker exec sentry-mate /sentry-mate log export <log name> > ~/sentry-mate-logs/<log name>
```

Its sidecar:

```bash
docker exec sentry-mate /sentry-mate log export <log name>.json > ~/sentry-mate-logs/<log name>.json
```

Its decoded signals:

```bash
docker exec sentry-mate /sentry-mate log decode <log name> > ~/sentry-mate-logs/signals.csv
```

The CSV columns are `timestamp_utc,signal,value,unit,value_name`, with a row each time a value changes.
`timestamp_utc` is Unix seconds with six decimals. On standard error it says the log's check result and which signal
set it used.

- It decodes only a log whose check is `verified` or `unsigned`. For any other it says the result and stops (exit
  1). `--any` decodes it anyway.
- It picks the signal set from the car's VIN: from the log's own frames, else its sidecar, else the car key's 10
  newest usable logs. Without a VIN it uses the Model 3/Y set. `--model 3y` or `--model sx-legacy` picks one (see
  [Supported cars](#supported-cars)).

`log candump` and `log check` read plain files, not the sealed store, and the container sees only `/data` and
`/keys`. Give `log candump` the file on standard input (`-i` passes it on):

```bash
docker exec -i sentry-mate /sentry-mate log candump /dev/stdin < ~/sentry-mate-logs/<log name> > ~/sentry-mate-logs/<log name>.txt
```

It writes a summary on standard error: the format, the lines, and how many long lines it skipped.

`log check` needs the file under its CAN Bridge name, with its sidecar next to it (or named as the second
argument). Mount the folder that holds them into a container of its own (it needs no data or keys):

```bash
docker run --rm -v ~/sentry-mate-logs:/check:ro ghcr.io/skittles-fivem/sentry-mate log check /check/<log name>
```

It prints `signed and intact: key <fingerprint>, chain index <n>`, `not signed`, or `failed: <reason>` (exit 1). A
missing sidecar gives only `No such file or directory`.

### The vault

Make the vault before the first start, with the recovery key shown on screen instead of written to a file. Only for
an empty data folder and an empty keys folder: it writes a new key file too. In the folder with `docker-compose.yml`:

```bash
docker compose run --rm sentry-mate init
```

New machine, or the key file lost: bring the data folder, then type the recovery key. It shows on screen as you
type. Dashes, spaces and case don't matter. Stop Sentry Mate first:

```bash
docker compose stop
```

```bash
docker compose run --rm -it sentry-mate restore
```

Then start it again:

```bash
docker compose up -d
```

It answers `Restored: a new key file is in /keys.` After a restore, older copies of the key file no longer open the
data folder. Keep only the new one. On TrueNAS, see [Restore](docs/install.md#restore) in the install guide.

Replace the recovery key (the old one stops working):

```bash
docker exec sentry-mate /sentry-mate recovery-key new
```

It prints the new key on screen. It does not touch `keys/RECOVERY-KEY.txt`: if that file is still there, it holds
the old key, which no longer works. Delete it.

### Health

`docker ps` shows `(healthy)` once the upload port answers, and `Restarting` while it keeps stopping:

```bash
docker ps
```

## Settings

Each is an environment variable (under `environment:` in `docker-compose.yml`: turn on the OPTIONAL lines there) or
a flag. The compose and TrueNAS files need none of them.

| Variable | Flag | Default | What it is |
|---|---|---|---|
| `SENTRY_MATE_DATA` | `--data` | `/data` | The data folder. |
| `SENTRY_MATE_KEYS` | `--keys` | `/keys` | The keys folder. Refused inside the data folder, and when it is the data folder or a folder right inside it on the server (compared by device and inode, through the mounts). |
| `SENTRY_MATE_UPLOAD_PORT` | `--upload-port` | `10100` | The upload port. Also the port `token new` prints and `health` checks. Change the port mapping with it. |
| `SENTRY_MATE_BIND` | `--bind` (`serve`) | `0.0.0.0` | The address to listen on. `health` connects to 127.0.0.1, so in Docker keep the default: another address makes the container unhealthy. |
| `SENTRY_MATE_NAMES` | `--names` (`serve`) | none | Names or addresses for the certificate, comma separated. Used only when the certificate is made. |
| `SENTRY_MATE_REQUIRE_MOUNTS` | | not set (`1` in the image) | When set to anything but `0`, `false`, `no` or `off`: every command that uses the data and keys folders first checks, in `/proc/self/mountinfo`, that each is a mount of its own, and stops with the volume line to add if not. |
| `RUST_LOG` | | `info` | How much to log, for example `debug`. The log goes to standard error. |

The image sets `SENTRY_MATE_DATA`, `SENTRY_MATE_KEYS` and `SENTRY_MATE_REQUIRE_MOUNTS`, and has no `VOLUME`: a
folder left out of the compose file stops it at once instead of going into an anonymous volume. The certificate is
normally made by the first `serve`; `token new` and `fingerprint` make it without names if it isn't there yet. Times
in the log are UTC: it does not read `TZ`.

## Ports and files

| Port | What | State |
|---|---|---|
| 10100/tcp | CAN Bridge uploads: HTTPS, TLS 1.3 and HTTP/1.1 only | done |
| 9100/tcp | Web UI, HTTPS | planned |

The upload port takes 6 connections per address and 256 in all. A TLS handshake must finish within 15 s, the
headers within 20 s, a request within 10 minutes. Any other path than the upload API gets `404`. The protocol is
Sentry Six Manager's [docs/can-bridge-api.md](https://github.com/skittles-fivem/Sentry-Manager/blob/main/docs/can-bridge-api.md).

| Limit | Value |
|---|---|
| A chunk | 16 MiB |
| A log | 1 GiB |
| A sidecar | 1 MiB |
| Names in one status request | 2000 |
| Unfinished uploads per car key | 16, dropped after 2 days |
| Logs per car key | 200,000, then `507 car_full` (no command deletes logs yet) |
| Free disk | a new upload needs twice its size plus 2 GiB free, else `507 no_space` |
| Wrong tokens | 20 per address per 10 minutes (IPv6: per /64), then `429` |
| Requests per token | 1200 per 10 minutes, then `429` |

A log whose sidecar hasn't come after 10 minutes is judged without it (checked every minute).

The data folder (contents sealed unless said):

| Path | What |
|---|---|
| `vault.json` | The data key, wrapped by the key file and by the recovery key. `restore` needs it. |
| `migrated.json` | A note that the one-time check for files an older version stored in the clear has run, so later starts skip it. Made on the first start, a new install too. |
| `tokens/<sha256>.json`, `tokens/<sha256>.key` | Each token, and the car key it is tied to. |
| `tls/cert.pem` | The certificate, in the clear (it is public). |
| `tls/key.sealed` | Its private key. |
| `can/<car key>/<name>.log.xz`, `<name>.log.xz.json` | The logs and sidecars, as uploaded. |
| `can/<car key>/counts.json` | How many logs the car key has. |
| `can/<car key>/trust.head`, `trust-<g>.records`, `trust-<g>.journal` | Each log's check result and the car key's pairing, with the keys trusted before. |
| `can/<car key>/trust.lock` | Empty. The server and a command such as `keys trust` take turns with it. |
| `can/<car key>/uploading/` | Uploads still arriving. |
| `can/.waiting/` | Empty files naming the car keys with logs waiting for their sidecar. |

The keys folder:

| Path | What |
|---|---|
| `sentry-mate.key` | 32 random bytes that open the vault, readable only by the user it runs as (1000, or 568 on TrueNAS). |
| `RECOVERY-KEY.txt` | The recovery key, written on the first start only. Delete it once copied. |

A running server keeps the certificate it started with. To change it, stop the container first and start it again
after (TrueNAS: *Stop* and *Start* the app):

```bash
docker compose stop
```

- Your own certificate: put `cert.pem` and `key.pem` in `data/tls/`, readable by the user it runs as. On the next
  start the key is sealed into `key.sealed`, and `key.pem` is overwritten and removed.
- A new self-signed one: delete `data/tls/cert.pem`. The next start makes a new one with `SENTRY_MATE_NAMES`.

```bash
docker compose start
```

Then `fingerprint` shows the SHA-256 the app needs. Don't delete `cert.pem` while the server runs: `fingerprint` and
`token new` would make a new one at once, without the names, and print a SHA-256 the server doesn't use yet.

## Backups, upgrades, moving

### A folder that is missing, empty or wrong

The data and keys folders must each be mounted from the server. Sentry Mate checks that before it writes anything:

- A volume line left out of the compose file stops it at once, and the log gives the line to add. The image has no
  `VOLUME`, so nothing goes into a folder that would be lost with the container.
- A keys folder inside the data folder on the server, or the same folder at both, is refused: the key file would be
  in every backup of the data.
- A data folder with no vault next to a keys folder with a key file (a wrong path, a disk that isn't there, a backup
  not put back) stops it. It never makes a new vault over a key file, which may open your data. Fix the path, or put
  the data back. To start over with no data on purpose, delete the key file.

It makes a new vault only when there is neither a vault nor a key file, as on the first start. The install guide's
[When it does not start](docs/install.md#when-it-does-not-start) lists each message and what to do.

### Backups

There is no backup command yet. Stop the container, copy the data folder whole, then start it again:

```bash
docker compose stop
```

```bash
sudo cp -a data <backup folder>/
```

```bash
docker compose start
```

- The copy stays sealed. It opens with the key file or the recovery key, and needs its own `vault.json`.
- Keep the key file out of that backup, and the recovery key apart from both.
- Keep the files' names and layout: each sealed file is bound to its path inside the data folder. The folder as a
  whole can move.
- A backup made before `recovery-key new` still opens with the old recovery key.
- Stopping drops the chunk being sent at that moment. What had arrived is kept, and the bridge carries on from there.
- On TrueNAS, give the data dataset a periodic snapshot task instead, and leave the keys dataset out of it
  ([Backups](docs/install.md#backups) in the install guide).

### Upgrades

Make a backup first (see [Backups](#backups)). Going back to an older version needs one from before the upgrade
(see [Security](#security), "No way back without a backup"). Then, in the folder with `docker-compose.yml`:

```bash
docker compose pull && docker compose up -d
```

On TrueNAS, `sudo docker pull ghcr.io/skittles-fivem/sentry-mate:latest` in *System → Shell*, then *Stop* and
*Start* the app (see [Updates](#updates) for staying on one build).

- A start after a version that stored files in the clear seals them once. Unfinished uploads stored in the clear are
  dropped; the bridge sends them again.
- Built from a clone instead (the OPTIONAL `build: .` line): `git pull`, then `docker compose up -d --build`. The
  build leaves out `data/`, `keys/` and `.env` (`.dockerignore`).

### Moving to a new server

Stop it on the old server:

```bash
docker compose down
```

On the new server, get the compose file and make the two folders (see [Run it](#run-it)). Copy the data folder in,
owned by the user it runs as (`sudo chown -R 1000:1000 data`; TrueNAS: 568). Put it in place **before** the first
start there (see [A folder that is missing, empty or wrong](#a-folder-that-is-missing-empty-or-wrong)). Bring the
keys folder too, the same way, or leave it empty and type the recovery key:

```bash
docker compose run --rm -it sentry-mate restore
```

Then start it:

```bash
docker compose up -d
```

The certificate and the tokens move with the data folder, so the app's token and SHA-256 stay the same. If the
server's address changed, change the app's upload address and press **Apply & restart**. Every step, TrueNAS
included, is in the install guide's [Moving to a new server](docs/install.md#moving-to-a-new-server).

A one-step move (a move code, or an export file) is being built.

## When a log fails its check or comes from a new key

Every log is kept as uploaded, whatever its check says. Each gets one of these results:

| Result | When |
|---|---|
| verified | Signed by the key the car key is paired with. The first signed log pairs it. A key trusted before keeps the logs that came while it was trusted. |
| unsigned | Not signed, and the car key isn't paired yet. |
| could not sign | Not signed, from a paired car key, and the bridge said why it couldn't sign. |
| failed | A bad signature or sidecar, a changed file, not signed although the car key is paired, or signed by another key. |
| waiting | The sidecar hasn't come. After 10 minutes the log is judged without it. |

- No command lists each log's result yet. `chain` shows the paired key's failed logs and the gaps. `log decode`
  says one log's result.
- `log decode` decodes only `verified` and `unsigned` logs, unless `--any`.
- A log signed by another key is marked failed. `keys` lists that key. Check it in the app, then trust it with
  `keys trust` ([Signing keys](#signing-keys)): its logs are verified from then on.
- A reinstalled bridge app or a new Screenmate has a new car key. Its uploads with the old token get
  `403 wrong_car`, so give it a new token. Its logs are kept under the new car key, which is paired by its own first
  signed log. Linking several car keys to one car is planned.

## Running next to TeslaMate and Sentry Six Manager

- **Sentry Six Manager** uses ports 9000 (web) and 10000 (CAN uploads). Sentry Mate uses 10100, and 9100 for its web
  UI when that lands, so both run on one server. Sentry Six Manager needs no change: the bridge sends to each on its
  own.
- **TeslaMate**: Sentry Mate doesn't talk to TeslaMate, its database, Tesla or the car. Keep TeslaMate running as
  long as you like. Importing its history from TeslaMate's own backup file is built but not wired in yet.

## Supported cars

| Car | DBC set | Checked on a real car |
|---|---|---|
| Model 3 and Model Y, every year | `tesla-3y` | Model Y only |
| Model S and Model X from 2021 | `tesla-3y` | no |
| Model S and Model X before 2021 | `tesla-sx-legacy` | no |

Today only `log decode` decodes. It picks the set from the car's VIN (the log's own frames, its sidecar, or the car
key's newest logs), else `tesla-3y`. `--model` picks one by hand. The DBC files and their sources are listed in
[crates/dbc/files/README.md](crates/dbc/files/README.md).

## Security

- TLS 1.3 only on the upload port; plain HTTP and older TLS are refused. The certificate is self-signed and made on
  first start, its private key sealed in the vault; the app pins it by SHA-256.
- Tokens are write-only: they can upload and ask which uploads arrived, nothing else. They are stored only as their
  SHA-256 and tied to one car key on the first upload. Wrong tokens are rate limited per address; a right
  token is never refused because of wrong ones.
- Every log's ECDSA P-256 signature is checked against the key the car key is paired with. A log signed by another
  key is kept and marked failed, and not used until the owner trusts that key (`keys trust`, after a `yes`). Only
  a key that signed some of the car key's logs can be trusted. Deleted, swapped or reordered logs show in the
  chain.
- **Encryption at rest, in Rust.** One random data key seals the contents of everything stored (logs, sidecars,
  uploads still arriving, trust records, tokens, the TLS key) with XChaCha20-Poly1305, in 64 KiB chunks, each file
  with its own key (HKDF-SHA256) and its path bound in, so a file that is changed, cut short, or moved under another
  name doesn't open. Files that grow (uploads still arriving, journals) give every piece its own random nonce, so a
  piece written again after a crash never reuses one, and bind each piece to the one before it, so pieces can't be
  reordered, dropped from the middle or mixed with those of an older copy of the file (an older copy put back whole,
  or pieces missing at the end, can't be told from a crash). The data key is kept only wrapped: by the key file in
  `/keys` (a separate volume, refused inside the data folder) for restarts with nothing to type, and by the recovery
  key through Argon2id at 1 GiB. Keys are wiped from memory when no longer needed and core dumps are off. This
  protects a stolen or copied disk or backup; it can't protect a server taken over while it runs. File and folder
  names, sizes and times are not sealed (see Privacy).
- **No way back without a backup.** This version writes growing files in a newer format (format 2) as soon as it
  opens them to add to, from its first start. Older versions can't open that format, so once it has run on a data
  folder, going back to an older image needs a backup of the data folder made before the upgrade.
- Pure Rust where it matters (`unsafe` forbidden in this code, the xz reader included); files from the network are
  read with fixed limits. The image is one static binary on `scratch`.
- **The container.** The image runs as 1000:1000; the TrueNAS file runs it as TrueNAS's apps user, 568:568. The
  compose and TrueNAS files drop every capability, make the root read-only and set `no-new-privileges`, so it can
  write only to its two mounted folders.
- **Folders it can't keep are refused**, before anything is written: a data or keys folder that is not mounted (it
  would be lost with the container, the key file with it), a keys folder inside the data folder or the same folder
  at both (the key file would be in the data's backups), and a folder it can't write to (most often root's).
- **A new vault is never made over an existing key file.** An empty or wrong data folder next to a key file stops it
  instead, so the key file that opens your data is never replaced.
- Nothing leaves the server: while it runs, Sentry Mate makes no outgoing connections.

## Privacy

- Logs hold the car's GPS position. Treat the data folder as private.
- Contents are sealed, names are not. A copied disk shows each car key, and each log's UTC start time (it is in the
  log's name), so it shows when each car was recording.
- The certificate (`tls/cert.pem`) is in the clear. It is public anyway.
- At the default log level (`info`) the server writes no log line per upload. With `RUST_LOG=debug`, the TLS
  library logs a few lines per connection (no names or addresses).

## Web UI and UI packs

- [`ui/README.md`](ui/README.md): the two packs, Instrument and Logbook. They run on made-up data from
  `web/mock-api.js`; no server serves them yet.
- [`docs/parity/ui-parity.md`](docs/parity/ui-parity.md): every TeslaMate tab (51 tabs, 1161 items) placed on 23
  Sentry Mate pages, described in our own words. [`docs/parity/coverage-matrix.json`](docs/parity/coverage-matrix.json)
  is the same, machine-readable.
- [`docs/ui-api.md`](docs/ui-api.md): the `/api/v1` the pages need. Not built yet.

Preview a pack with any static file server:

```bash
python3 -m http.server -d ui/packs/instrument/web 8000
```

```bash
python3 -m http.server -d ui/packs/logbook/web 8000
```

Then open `http://127.0.0.1:8000/`. Add `?cars=200000` to try a large fleet, `?latency=0` to drop the simulated
network delay. The previews load map tiles from `tile.openstreetmap.org`.

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

CI is [`.github/workflows/image.yml`](.github/workflows/image.yml). It runs on a push to `main` that changes code (a
push that changes only `README.md`, `docs/` or `truenas/` runs nothing), and by hand (*Actions → Image → Run
workflow*). First the gates, once:

```bash
cargo fmt --all --check
```

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
```

```bash
cargo test --workspace --locked
```

At the same time it builds the image for amd64 and arm64, each on a machine of its kind. It checks each one with
[`.github/check-image.sh`](.github/check-image.sh), as 1000:1000 and as TrueNAS's 568:568: folders it can't keep
are refused (root's, not mounted, the keys inside the data, the same folder at both), no capabilities, a read-only
root, healthy, TLS 1.3 only, the recovery key file, a token made with `docker exec`, a clean stop, and the same
certificate after a restart. Only when all of that passed does it publish both as one image,
`ghcr.io/skittles-fivem/sentry-mate`, tagged `latest`, `build-N` (the workflow's run number) and `sha-<commit>`.
`latest` is tagged last. A run by hand for another branch gets the branch's name as its tag, never `latest` or a
`build-N`.

Check an image you built yourself the same way. The check needs docker, curl and openssl, and sudo when not run as
root. It uses port 10100 on 127.0.0.1 (`CHECK_PORT` picks another, for a server that runs Sentry Mate already):

```bash
docker build -t sentry-mate:check .
```

```bash
.github/check-image.sh sentry-mate:check
```

```bash
.github/check-image.sh sentry-mate:check 568
```

A local build shows no build number in `--version`. To run your own build instead of the published one, turn on the
OPTIONAL `build: .` line in `docker-compose.yml` (in a clone) and start it with `docker compose up -d --build`. A
later `docker compose pull` puts the published image back.

Rust 1.88 or newer (`rust-version`: the locked `time` and `sha2` crates need 1.88), edition 2024. `unsafe` is
forbidden in every crate here, and clippy denies `unwrap`.

### After the first publish (the repo owner, once)

GitHub makes a new package private. Until it is public, every pull fails with `denied`, on Docker Compose and on
TrueNAS. Make it public once: on GitHub, *Packages → sentry-mate → Package settings → Change visibility →
Public*. Then check that it pulls without a login:

```bash
docker pull ghcr.io/skittles-fivem/sentry-mate:latest
```

The aim is pure Rust. Today only `ring` (used by rustls and rcgen for TLS) builds C code. Check that nothing else
does:

```bash
cargo tree -e normal,build -i cc --depth 1
```

| Crate | Job | In the program |
|---|---|---|
| `logfmt` | Reads the logs (both formats, xz in pure Rust) | yes |
| `trust` | Signatures, pairing, chain report | yes |
| `vault` | Encryption at rest | yes |
| `dbc` | Decodes signals | only `log decode` |
| `ingest` | Upload API, storage, tokens, trust records, TLS server | yes |
| `model` | The shared types | only the signal set from the VIN |
| `timeline` | States, drives, charges from the signals | only the VIN reader, in `log decode` |
| `db` | Sealed storage for what the timeline works out | not yet |
| `places` | Geofences, prices, addresses, map settings | not yet |
| `accounts` | People and sign-in | not yet |
| `import` | TeslaMate history | not yet |
| `mqtt` | Home Assistant | not yet |
| `sentry-mate` | The program | |

| Path | What |
|---|---|
| `crates/` | The Rust workspace |
| `crates/dbc/files/` | The DBC files, with their licences |
| `docs/plans/` | The plan |
| `docs/parity/`, `docs/ui-api.md` | The UI plan and the API it needs |
| `ui/packs/` | The UI packs |
| `Dockerfile`, `docker-compose.yml` | The image and how to run it |
| `truenas/sentry-mate.yaml` | The TrueNAS SCALE app |
| `docs/install.md` | The install guide |
| `.github/workflows/image.yml`, `.github/check-image.sh` | CI: the gates, the image, its check, publishing |

## Licence

No licence has been chosen yet; the repo has no LICENSE file. Bundled third-party files keep their own:

- DBC files from model3dbc, opendbc and Open Vehicle Monitoring System's Model S module: MIT. `override.dbc` is from
  Sentry Six Manager. See [crates/dbc/files/README.md](crates/dbc/files/README.md).
- In the UI packs: MapLibre GL JS 6.18.0 (BSD-3-Clause), uPlot 1.6.32 (MIT), Inter (SIL Open Font License 1.1),
  JetBrains Mono (Apache License 2.0, per its bundled licence file), Bitstream Charter (Bitstream's free licence).

Sentry Mate is written from scratch and copies no code from TeslaMate; it is not affiliated with TeslaMate, Tesla,
or Screenmate.
