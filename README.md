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
| Command line: tokens, certificate, chain report, log export and decode, restore, recovery key | done |
| Docker image: one static binary on `scratch`, non-root, health check; run read-only by the compose file | done; built from a clone, no published image yet |
| Decoding signals (DBC sets per model, bus split, the Screenmate's GPS), same values as Sentry Six Manager | done; used only by `log decode`, always with the Model 3/Y set |
| Decoding each log as it arrives, after its check | planned |
| Choosing the DBC set from the car's VIN | built, not wired in yet |
| Drives, charges, states, positions, charge curves from the decoded signals; the car's model from its VIN | built, not wired in yet; not yet checked against real drives and charges |
| Storage for everything worked out (sealed, compressed columns, any number of cars, backups) | built, not wired in yet |
| Places, geofences, charge costs with time-of-use rates, addresses offline or online | built, not wired in yet |
| Accounts: admin and watch-only, per-car access, two-step sign-in, sessions, audit log | built, not wired in yet; passkeys planned |
| TeslaMate import from TeslaMate's own backup file (any number of cars, streamed, again later adds only what's new) | built, not wired in yet; a live connection to TeslaMate's database is planned |
| MQTT for Home Assistant (TLS unless plain is turned on, never a password in the clear, discovery, any number of cars on one connection) | built, not wired in yet |
| Trusting a new signing key for a car | in the library, no command yet |
| Using a failed log anyway | not built |
| Listing stored logs and their check results, deleting logs, a backup command | not built |
| Web server and the `/api/v1` the pages need ([`docs/ui-api.md`](docs/ui-api.md)), on its own HTTPS port | planned |
| Web UI: UI packs, two designs as prototypes on made-up data in [`ui/`](ui/), maps; every TeslaMate tab gets a page, planned in [`docs/parity/ui-parity.md`](docs/parity/ui-parity.md) | being built |
| Published images (amd64 and arm64), `cargo deny` and `cargo audit`, fuzzing in CI | planned |
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
- Get a stored log back as uploaded (`log export`), or its decoded signals as CSV (`log decode`).
- Turn a log file into candump text (`log candump`), or check its signature (`log check`).

No command lists cars or logs yet. File and folder names are not sealed, so you find them on the host (see
[Commands](#commands)).

## Requirements

- A Linux server with Docker, Docker Compose (`docker compose`) and git.
- Internet while building: the build pulls the `rust:1-alpine` image, one Alpine package (`musl-dev`) and the Rust
  crates. The image is built on the server, for the server's own CPU. While it runs, Sentry Mate makes no outgoing
  connections.
- About 3 GB of free disk for the build: the Rust build image and what it builds.
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

Get the code. No image is published yet, so you build it from this repo:

```bash
git clone https://github.com/skittles-fivem/sentry-mate.git
```

```bash
cd sentry-mate
```

Make the two folders and give them to the container's user (uid 1000):

```bash
mkdir -p data keys
```

```bash
sudo chown 1000:1000 data keys
```

The repo's `docker-compose.yml`:

```yaml
services:
  sentry-mate:
    build: .
    image: sentry-mate:local
    restart: unless-stopped
    ports:
      - "10100:10100"   # CAN Bridge uploads, HTTPS (TLS 1.3 only)
    volumes:
      - ./data:/data    # everything, sealed (logs exactly as uploaded, tokens, trust records, the TLS key)
      - ./keys:/keys    # the key file that opens it: keep it on another disk if you can, and out of data's backups
    environment:
      - TZ=UTC
      # host names or addresses of this server for its certificate, comma separated (optional:
      # the app pins the certificate by its SHA-256 and does not check names)
      # - SENTRY_MATE_NAMES=server.local,192.168.1.20
    read_only: true
    cap_drop: [ALL]
    security_opt: ["no-new-privileges:true"]
    user: "1000:1000"
```

Sentry Mate refuses a keys folder inside the data folder. Build and start it. The first build takes a few minutes
(about 3 on a 4-core PC, longer on a NAS or a small board):

```bash
docker compose up -d --build
```

The first start makes the vault. That takes a few seconds, with nothing in the log until it is done. Then the log
shows the upload address, the certificate SHA-256 and the vault's id:

```bash
docker compose logs sentry-mate
```

If the log shows only `Permission denied (os error 13)` and the container keeps restarting, the folders are not
owned by uid 1000 (Docker makes missing ones owned by root). Run the `chown` above, then restart it:

```bash
docker compose restart
```

### The recovery key

On the first start Sentry Mate puts the **recovery key** in `keys/RECOVERY-KEY.txt` (not in the log). With the key
file it is the only way to open the data; lose both and nobody can. Read it:

```bash
sudo cat keys/RECOVERY-KEY.txt
```

Copy it somewhere safe away from the server (a password manager, paper), then delete the file:

```bash
sudo rm keys/RECOVERY-KEY.txt
```

Every start logs a warning while that file is still there.

### An upload token

Make a token for the car (shown once):

```bash
docker compose exec sentry-mate /sentry-mate token new --car "Model Y"
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
- `token new` prints the port inside the container. If you map another host port (for example `"8443:10100"`), use
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
docker compose exec sentry-mate /sentry-mate token list
```

The token is tied when the first piece of a log comes in, before it is stored. So this shows that the bridge got
through with this token, not that a whole log arrived. The logs that arrived are in the car key's folder:

```bash
sudo ls data/can/<car key>
```

## Commands

Run them in the running container with `docker compose exec sentry-mate /sentry-mate ...`. `init`, `restore` and
`log check` use `docker compose run --rm` instead. Exit code 0 means it worked, 1 that it failed, 2 a usage error.
The reason for a failure is on standard error, except `log check`, which prints it on standard output, and `health`,
which prints nothing.

| Command | What it does |
|---|---|
| `serve` | Runs the upload server until stopped (the image's default). Makes the vault on its first start. |
| `init` | Makes the vault now and prints the recovery key on screen, not into a file. Exit 1 if there is a vault already. |
| `restore` | Reads the recovery key from standard input and writes a new key file. |
| `recovery-key new` | Prints a new recovery key. The old one stops working. |
| `token new --car NAME` | Makes an upload token (shown once) and prints the three fields for the app. |
| `token list` | One line per token: id, car name, `in use` or `revoked`, the car key it is tied to. |
| `token revoke ID` | Revokes a token. It stops working at its next request, with the server still running. |
| `fingerprint` | Prints the certificate SHA-256 for the app. |
| `chain KEY` | Prints a car key's chain report as JSON. |
| `log export NAME` | Writes a stored log or sidecar, unsealed, to standard output. |
| `log decode NAME` | Writes a stored log's decoded signals as CSV to standard output. |
| `log candump FILE` | Writes a plain log file of either format as candump text. |
| `log check FILE [SIDECAR]` | Says what a plain log file's signature says. |
| `health` | Exit 0 when the upload port accepts a TCP connection on 127.0.0.1. Docker's health check runs it. |

### Tokens

A new token works at once, with no restart. List them:

```bash
docker compose exec sentry-mate /sentry-mate token list
```

Revoke one by its id:

```bash
docker compose exec sentry-mate /sentry-mate token revoke <id>
```

Revoked tokens stay in the list. No command deletes a token or ties it to another car key.

### The certificate

```bash
docker compose exec sentry-mate /sentry-mate fingerprint
```

### The chain report

The car key is the last column of `token list`:

```bash
docker compose exec sentry-mate /sentry-mate chain <car key>
```

It covers only logs signed by the key the car key is paired with: count, first and last index, issues (`gap`,
`duplicate`, `failed`, `mismatch`, `reordered`), restarts and segments. An unknown or mistyped car key gives an empty
report, the same as a car key with no signed logs.

### Logs

Find a log's name on the host. Each car key has a folder:

```bash
sudo ls data/can
```

```bash
sudo ls data/can/<car key>
```

A log is named `<car key>_<UTC start, yyyyMMddTHHmmssZ>_<seq>.log.xz`, and its sidecar the same plus `.json`. Use
`-T` when you send output to a file, so no terminal changes the bytes.

Write what you export to a folder outside the clone. Files you put in the clone go into Docker's build cache at the
next build, and exported logs hold the car's GPS position:

```bash
mkdir -p ~/sentry-mate-logs
```

A stored log, exactly as uploaded:

```bash
docker compose exec -T sentry-mate /sentry-mate log export <log name> > ~/sentry-mate-logs/<log name>
```

Its sidecar:

```bash
docker compose exec -T sentry-mate /sentry-mate log export <log name>.json > ~/sentry-mate-logs/<log name>.json
```

Its decoded signals:

```bash
docker compose exec -T sentry-mate /sentry-mate log decode <log name> > ~/sentry-mate-logs/signals.csv
```

The CSV columns are `timestamp_utc,signal,value,unit,value_name`, with a row each time a value changes.
`timestamp_utc` is Unix seconds with six decimals. It always uses the Model 3/Y set (see
[Supported cars](#supported-cars)), and it decodes a log whatever its check said.

`log candump` and `log check` read plain files, not the sealed store, and the container sees only `/data` and
`/keys`. Give `log candump` the file on standard input:

```bash
docker compose exec -T sentry-mate /sentry-mate log candump /dev/stdin < ~/sentry-mate-logs/<log name> > ~/sentry-mate-logs/<log name>.txt
```

It writes a summary on standard error: the format, the lines, and how many long lines it skipped.

`log check` needs the file under its CAN Bridge name, with its sidecar next to it (or named as the second
argument). Mount the folder that holds them:

```bash
docker compose run --rm -v ~/sentry-mate-logs:/check:ro sentry-mate log check /check/<log name>
```

It prints `signed and intact: key <fingerprint>, chain index <n>`, `not signed`, or `failed: <reason>` (exit 1). A
missing sidecar gives only `No such file or directory`.

### The vault

Make the vault before the first start, with the recovery key shown on screen instead of written to a file. Only for
an empty data folder: it writes a new key file too.

```bash
docker compose run --rm sentry-mate init
```

New machine, or the key file lost: bring the data folder, then type the recovery key. It shows on screen as you
type. Dashes, spaces and case don't matter.

```bash
docker compose run --rm -it sentry-mate restore
```

After a restore, older copies of the key file no longer open the data folder. Keep only the new one.

Replace the recovery key (the old one stops working):

```bash
docker compose exec sentry-mate /sentry-mate recovery-key new
```

It prints the new key on screen. It does not touch `keys/RECOVERY-KEY.txt`: if that file is still there, it holds
the old key, which no longer works. Delete it.

### Health

`docker compose ps` shows `healthy` once the upload port answers:

```bash
docker compose ps
```

## Settings

Each is an environment variable (under `environment:` in `docker-compose.yml`) or a flag.

| Variable | Flag | Default | What it is |
|---|---|---|---|
| `SENTRY_MATE_DATA` | `--data` | `/data` | The data folder. |
| `SENTRY_MATE_KEYS` | `--keys` | `/keys` | The keys folder. Refused inside the data folder. |
| `SENTRY_MATE_UPLOAD_PORT` | `--upload-port` | `10100` | The upload port. Also the port `token new` prints and `health` checks. Change the port mapping with it. |
| `SENTRY_MATE_BIND` | `--bind` (`serve`) | `0.0.0.0` | The address to listen on. `health` connects to 127.0.0.1, so in Docker keep the default: another address makes the container unhealthy. |
| `SENTRY_MATE_NAMES` | `--names` (`serve`) | none | Names or addresses for the certificate, comma separated. Used only when the certificate is made. |
| `RUST_LOG` | | `info` | How much to log, for example `debug`. The log goes to standard error. |

The image sets `SENTRY_MATE_DATA` and `SENTRY_MATE_KEYS`. The certificate is normally made by the first `serve`;
`token new` and `fingerprint` make it without names if it isn't there yet. `TZ` in the compose file is not used by
the program yet.

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
| `can/<car key>/trust.head`, `trust-<g>.records`, `trust-<g>.journal` | Each log's check result and the car key's pairing. |
| `can/<car key>/uploading/` | Uploads still arriving. |
| `can/.waiting/` | Empty files naming the car keys with logs waiting for their sidecar. |

The keys folder:

| Path | What |
|---|---|
| `sentry-mate.key` | 32 random bytes that open the vault, readable only by uid 1000. |
| `RECOVERY-KEY.txt` | The recovery key, written on the first start only. Delete it once copied. |

A running server keeps the certificate it started with. To change it, stop the container first and start it again
after:

```bash
docker compose stop
```

- Your own certificate: put `cert.pem` and `key.pem` in `data/tls/`, readable by uid 1000. On the next start the key
  is sealed into `key.sealed`, and `key.pem` is overwritten and removed.
- A new self-signed one: delete `data/tls/cert.pem`. The next start makes a new one with `SENTRY_MATE_NAMES`.

```bash
docker compose start
```

Then `fingerprint` shows the SHA-256 the app needs. Don't delete `cert.pem` while the server runs: `fingerprint` and
`token new` would make a new one at once, without the names, and print a SHA-256 the server doesn't use yet.

## Backups, upgrades, moving

### Don't start it on an empty data folder

When `serve` or `init` finds no `vault.json` in the data folder, it makes a new vault and **replaces
`keys/sentry-mate.key` without asking**. A wrong path or a missing data mount does this: the image declares `/data`
a volume, so Docker then gives it an empty one. The real data then opens only with the recovery key. The log shows
`new vault made` and a new vault id. If it happens, stop the container, fix the mount, and run `restore`.

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

### Upgrades

Make a backup first (see [Backups](#backups)). Going back to an older version needs one from before the upgrade
(see [Security](#security), "No way back without a backup").

```bash
git pull
```

```bash
docker compose up -d --build
```

- A start after a version that stored files in the clear seals them once. Unfinished uploads stored in the clear are
  dropped; the bridge sends them again.
- `.dockerignore` does not leave out `keys/` yet. A rebuild sends the key file, and `RECOVERY-KEY.txt` if it is still
  there, into Docker's build cache on this server, and anything else you put in the clone (exported logs,
  `signals.csv`). They don't reach the image. To avoid that, keep the keys folder outside the clone and change the
  `./keys` line in `docker-compose.yml`, and export logs outside the clone (see [Logs](#logs)).

### Moving to a new server

Stop it on the old server:

```bash
docker compose down
```

On the new server, clone the repo and copy the data folder in, owned by uid 1000. Put it in place **before** the
first start there (see above). Bring the keys folder too, or make an empty one owned by uid 1000 and type the
recovery key:

```bash
docker compose run --rm -it sentry-mate restore
```

Then start it:

```bash
docker compose up -d --build
```

The certificate moves with the data folder, so the app's SHA-256 stays the same. If the server's address changed,
change the app's upload address and press **Apply & restart**.

## When a log fails its check or comes from a new key

Every log is kept as uploaded, whatever its check says. Each gets one of these results:

| Result | When |
|---|---|
| verified | Signed by the key the car key is paired with. The first signed log pairs it. |
| unsigned | Not signed, and the car key isn't paired yet. |
| could not sign | Not signed, from a paired car key, and the bridge said why it couldn't sign. |
| failed | A bad signature or sidecar, a changed file, not signed although the car key is paired, or signed by another key. |
| waiting | The sidecar hasn't come. After 10 minutes the log is judged without it. |

- No command lists each log's result yet. `chain` shows the paired key's failed logs and the gaps.
- `log decode` does not look at the result.
- A log signed by another key is marked failed, and that key is noted. The library can trust a new key, but no
  command does yet.
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

Today the program decodes only with `tesla-3y` (`log decode`). Choosing the set from the VIN is built but not wired
in, so an older S or X decodes with the wrong set for now. The DBC files and their sources are listed in
[crates/dbc/files/README.md](crates/dbc/files/README.md).

## Security

- TLS 1.3 only on the upload port; plain HTTP and older TLS are refused. The certificate is self-signed and made on
  first start, its private key sealed in the vault; the app pins it by SHA-256.
- Tokens are write-only: they can upload and ask which uploads arrived, nothing else. They are stored only as their
  SHA-256 and tied to one car key on the first upload. Wrong tokens are rate limited per address; a right
  token is never refused because of wrong ones.
- Every log's ECDSA P-256 signature is checked against the key the car key is paired with. A log signed by another
  key is kept and marked failed; no command trusts a new key yet. Deleted, swapped or reordered logs show in the
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
  read with fixed limits. The image is one static binary on `scratch`, run as a normal user.
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
5. First release: published images, the docs, a licence and the final name.

Open questions: two battery % signals disagree (0x33A and 0x292) and need checking against the car's screen. A
firmware-version signal for software updates is still to be found.

## Development

CI runs these on every push to `main`:

```bash
cargo fmt --all --check
```

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
```

```bash
cargo test --workspace --locked
```

```bash
docker build -t sentry-mate:ci .
```

```bash
docker run --rm sentry-mate:ci --version
```

CI publishes nothing. Rust 1.88 or newer (the locked `time` crate, used by rcgen, needs 1.88), edition 2024.
`unsafe` is forbidden in every crate here, and clippy denies `unwrap`.

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
| `model` | The shared types | not yet |
| `timeline` | States, drives, charges from the signals | not yet |
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
| `.github/workflows/ci.yml` | CI |

## Licence

No licence has been chosen yet; the repo has no LICENSE file. Bundled third-party files keep their own:

- DBC files from model3dbc, opendbc and Open Vehicle Monitoring System's Model S module: MIT. `override.dbc` is from
  Sentry Six Manager. See [crates/dbc/files/README.md](crates/dbc/files/README.md).
- In the UI packs: MapLibre GL JS 6.18.0 (BSD-3-Clause), uPlot 1.6.32 (MIT), Inter (SIL Open Font License 1.1),
  JetBrains Mono (Apache License 2.0, per its bundled licence file), Bitstream Charter (Bitstream's free licence).

Sentry Mate is written from scratch and copies no code from TeslaMate; it is not affiliated with TeslaMate, Tesla,
or Screenmate.
