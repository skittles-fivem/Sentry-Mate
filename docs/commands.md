# Commands, settings and files

[← Back to the README](../README.md)

- [What you can do today](#what-you-can-do-today)
- [How to run a command](#how-to-run-a-command)
- [All commands](#all-commands)
- [Tokens](#tokens)
- [The certificate](#the-certificate)
- [The chain report](#the-chain-report)
- [Signing keys](#signing-keys)
- [Logs](#logs)
- [The vault](#the-vault)
- [Moving](#moving)
- [The web UI](#the-web-ui)
- [Health](#health)
- [Settings](#settings)
- [Ports and limits](#ports-and-limits)
- [Files](#files)
- [Changing the certificate](#changing-the-certificate)

## What you can do today

- Receive every log the CAN Bridge uploads and keep it sealed, exactly as uploaded.
- Turn the checked logs into each car's history (drives, charges, states, the time series) in the background, and
  read it in the web UI on port 10101 (see [The web UI](#the-web-ui)). The web UI also changes the settings: places
  and costs, car settings, upload tokens and car keys, accounts, MQTT, the TeslaMate import.
- See each upload token and the car key it is tied to (`token list`).
- See a car key's chain report: gaps, failed logs, restarts (`chain`).
- See which keys signed a car key's logs, and trust a new one (`keys`, `keys trust`).
- Get a stored log back as uploaded (`log export`), or its decoded signals as CSV (`log decode`).
- Turn a log file into candump text (`log candump`), or check its signature (`log check`).
- Move everything to another server in one step (`move code`, `move from`), or with one export file (`export`,
  `import`), from the commands or from the web pages. See [moving.md](moving.md).

No command lists cars or logs yet (the web UI does). File and folder names are not sealed, so you find them on the host (see
[Logs](#logs)).

## How to run a command

Run them in the running container with `docker exec sentry-mate /sentry-mate ...`. That works from any folder, and
on TrueNAS in *System → Shell* with `sudo` in front. `init` and `restore` run while Sentry Mate is stopped, with
`docker compose run --rm` in the folder with `docker-compose.yml` (the TrueNAS restore is in
[install.md](install.md#restore)). `log check` runs in a container of its own (`docker run --rm`).

Exit code 0 means it worked, 1 that it failed, 2 a usage error. The reason for a failure is on standard error, except
for `health`, which prints nothing. `log check` prints its result, `failed: <reason>` included, on standard output;
a file it can't read is on standard error.

## All commands

| Command | What it does |
|---|---|
| `serve` | Runs the server until stopped (the image's default): the upload port, the history (decoded in the background) and the web UI. Makes the vault on its first start. |
| `init` | Makes the vault now and prints the recovery key on screen, not into a file. Exit 1 if there is a vault or a key file already. |
| `restore` | Reads the recovery key from standard input and writes a new key file. Run it with Sentry Mate stopped. |
| `recovery-key new` | Prints a new recovery key and writes it to `keys/RECOVERY-KEY.txt`. The old one stops working. |
| `token new --car NAME [--key KEY]` | Makes an upload token (shown once) and prints the three fields for the app. `--key` (repeatable) lets it take over a car key in use already: a replacement for that car's token. |
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
| `export` | Writes the whole server as one export file to standard output. It opens on another server with this server's recovery key. With compose, use `exec -T`. |
| `import FILE` | A new server: brings an export file in (put it in the data folder first). Asks for the recovery key. Run it again after a stop: it carries on. `import --cancel` removes a half-done import. |
| `move code` | The old server: makes a one-time move code (shown once, valid 24 hours). Only from the home network, unless `--anywhere`. `--replace` ends a move under way and makes a new code (asks first). |
| `move from ADDRESS CODE` | A new server (empty data and keys folders): brings the data here from the old server. |
| `move status` | What a move is doing, on either server. |
| `move cancel` | The old server: the code stops working. The new server: the move stops and what was copied is removed. |
| `move undo` | The old server, after a move: takes uploads again (asks to type `yes`). Stop the new server first. |
| `move finish --recovery-key` | A new server, last resort when the old one is gone: switches to what was copied. Asks for the recovery key. The last changes may be missing. |
| `health` | Exit 0 when the upload port and the web port accept a TCP connection on 127.0.0.1 (only the upload port on a server whose data moved away), or while a move comes in. Docker's health check runs it. |
| `--version` | Prints the version and build, for example `sentry-mate 0.1.0 (build-1001, sha-6f83191, 2026-11-17T19:06:35Z)`. A local build shows no build number. |

## Tokens

Make a token for a car (shown once). It prints the token's id, the token (it starts with `smt_`), and the three
fields for the app:

```bash
docker exec sentry-mate /sentry-mate token new --car "Model Y"
```

A new token works at once, with no restart. A token is tied to one car key on its first upload: one token per car.
Its first upload may only take a car key no other token uses and with no logs here. To replace a car's lost or
revoked token, name the car's key (the last column of `token list`), or make the token on the car's page in the web UI:

```bash
docker exec sentry-mate /sentry-mate token new --car "Model Y" --key <car key>
```

For another car, make another token. List them:

```bash
docker exec sentry-mate /sentry-mate token list
```

Revoke one by its id (the first column of `token list`):

```bash
docker exec sentry-mate /sentry-mate token revoke <id>
```

Revoked tokens stay in the list. No command deletes a token or ties it to another car key.

## The certificate

```bash
docker exec sentry-mate /sentry-mate fingerprint
```

It stays the same across restarts, updates and moves: the certificate's key is in the data folder. To change it, see
[Changing the certificate](#changing-the-certificate).

## The chain report

The car key is the last column of `token list`:

```bash
docker exec sentry-mate /sentry-mate chain <car key>
```

It covers only logs signed by the key the car key is paired with, and names that key first (`signing_key`, as
`keys` lists it). Then: count, first and last index, issues (`gap`, `duplicate`, `failed`, `mismatch`,
`reordered`), restarts and segments. A car key with no signed logs gives an empty report, with `signing_key` set to
`null`. An unknown or mistyped car key gives `no car key <key> here` and exit 1.

## Signing keys

List the keys a car key's logs name:

```bash
docker exec sentry-mate /sentry-mate keys <car key>
```

Trust another one, after checking it in the app:

```bash
docker exec -it sentry-mate /sentry-mate keys trust <car key> <signing key>
```

What the list shows, when to trust a key and what trusting changes:
[A new signing key](security.md#a-new-signing-key).

## Logs

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
  [Supported cars](cars-and-bridge.md#supported-cars)).

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

It prints `signed and intact: key <fingerprint>, chain index <n>`, `not signed`, or `failed: <reason>` (exit 1), on
standard output. A missing log or sidecar gives `sentry-mate: can't read <file>: No such file or directory (os error
2)` on standard error (exit 1).

## The vault

Make the vault before the first start, with the recovery key shown on screen instead of written to a file. Only for
an empty data folder and an empty keys folder: it writes a new key file too. In the folder with `docker-compose.yml`
(Docker Compose only: on TrueNAS the vault is made on the first start):

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
data folder. Keep only the new one. On TrueNAS, see [Restore](install.md#restore) in the install guide.

Replace the recovery key (the old one stops working):

```bash
docker exec sentry-mate /sentry-mate recovery-key new
```

It prints the new key on screen, not in the log, and writes it to `keys/RECOVERY-KEY.txt` (in place of the old one,
if that file was still there). Copy it somewhere safe, then delete the file. Settings: Server and access does the
same from the web pages (password again), and its "I have saved it" tick deletes the file.

`init`, `restore` and `recovery-key new` need about 1 GiB of free memory for a few seconds (Argon2id at 1 GiB).

## Moving

The full guide, with TrueNAS and plain Docker, the CAN Bridge app, undo and what to do when it stops half way, is
[moving.md](moving.md). In short:

- Old server: `move code` (or `SENTRY_MATE_OFFER_MOVE=yes`, and the code is in its log).
- New server, with empty data and keys folders: `SENTRY_MATE_MOVE_FROM` and `SENTRY_MATE_MOVE_CODE` in its
  environment, or `move from ADDRESS CODE`. It copies everything, keeps up with new uploads, then takes over.
- The tokens, trust records and certificate move with the data, so the CAN Bridge keeps its token and certificate
  SHA-256. The new server writes a key file of its own; the recovery key stays the same.
- Commands that change data refuse on a server whose data moved away. `token list`, `fingerprint` and `export` still
  work there.

## The web UI

`serve` opens the web UI on port 10101 (`SENTRY_MATE_WEB_PORT`), HTTPS with TLS 1.3 only and the same certificate as
the upload port, so the browser warns once about a certificate it doesn't know: compare the SHA-256 it shows with
`fingerprint`. Open `https://<server address>:10101/`.

- **First start**: with no account yet, the log shows a one-time setup code, and the same code is in
  `SETUP-CODE.txt` in the keys folder. Type it on the first page with the first admin's name and password. It is
  valid 24 hours; a new one is made (and logged) when it runs out while no account exists, and on every start
  until setup is done. The file is removed once the first admin exists.
- **The history** is made from stored logs whose check allows their data, in the background: right after an upload,
  and on every start for logs not worked on yet (a first start over many logs takes a while; the pages fill in as
  it goes). A log whose check fails is never used.
- **What the pages do**: they read cars, the Now values, the timeline, drives, charges, battery, places, reports and
  data health, and change places and costs, car settings, upload tokens and car keys (**Settings → Cars**), accounts,
  server settings, MQTT and the TeslaMate import. Admin accounts change things; watch accounts only read their cars.
- **Moving** from the pages: a move code or an export file on the old server, and "move here" on the new server's
  first page ([moving.md](moving.md)). Restoring from an export file on the first page is not served yet: use
  `import`.
- A server whose data moved away serves no web UI: it runs on the new server.

## Health

`docker ps` shows `(healthy)` once the upload port and the web port answer, and `Restarting` while it keeps stopping:

```bash
docker ps
```

## Settings

Each is an environment variable (under `environment:` in `docker-compose.yml`: turn on the OPTIONAL lines there) or
a flag, except `SENTRY_MATE_TAG`, which goes in a `.env` file. The compose and TrueNAS files need none of them.

| Variable | Flag | Default | What it is |
|---|---|---|---|
| `SENTRY_MATE_DATA` | `--data` | `/data` | The data folder. Leave it: change the folder in the volume line instead. |
| `SENTRY_MATE_KEYS` | `--keys` | `/keys` | The keys folder. Refused inside the data folder, and when it is the data folder or a folder right inside it on the server (compared by device and inode, through the mounts). Leave it: change the folder in the volume line instead. |
| `SENTRY_MATE_UPLOAD_PORT` | `--upload-port` | `10100` | The upload port. Also the port `token new` prints and `health` checks. Change the port mapping with it. For only another host port, leave it and change the left number of the port mapping. |
| `SENTRY_MATE_WEB_PORT` | `--web-port` | `10101` | The web UI's port (HTTPS). `health` checks it too. Change the port mapping with it; for only another host port, leave it and change the left number of the port mapping. |
| `SENTRY_MATE_BIND` | `--bind` (`serve`) | `0.0.0.0` | The address both ports listen on. `health` connects to 127.0.0.1, so in Docker keep the default: another address makes the container unhealthy. |
| `SENTRY_MATE_NAMES` | `--names` (`serve`) | none | Names or addresses for the certificate, comma separated (the OPTIONAL lines in the compose and TrueNAS files). Used only when the certificate is made. The bridge pins the SHA-256 and does not check names, so it can stay off. |
| `SENTRY_MATE_MOVE_FROM` | `--move-from` (`serve`) | none | A new server: the old server's address, as the CAN Bridge uses it (`192.168.1.20`, `nas.local:10100`). With `SENTRY_MATE_MOVE_CODE`, it brings the data in as it starts. Ignored once the move is done. See [moving.md](moving.md). |
| `SENTRY_MATE_MOVE_CODE` | `--move-code` (`serve`) | none | A new server: the move code the old server showed. Not shown in `--help`. |
| `SENTRY_MATE_OFFER_MOVE` | `--offer-move` (`serve`) | none | The old server: `yes` keeps a move code ready and prints it in its log (home network only); `anywhere` the same for a new server anywhere. For TrueNAS, with no shell. |
| `SENTRY_MATE_REQUIRE_MOUNTS` | | not set (`1` in the image) | When set to anything but `0`, `false`, `no` or `off`: every command that uses the data and keys folders first checks, in `/proc/self/mountinfo`, that each is a mount of its own, and stops with the volume line to add if not. Leave it. |
| `SENTRY_MATE_TAG` | | `latest` | Read by `docker-compose.yml`, not the program: the image tag to run, set in a `.env` file (for example `build-1001`). See [Updating](install.md#updating). |
| `RUST_LOG` | | `info` | How much to log, for example `debug`, or `warn` for less. `warn` also hides the line with the certificate SHA-256; `fingerprint` still prints it. The log goes to standard error. |

The image sets `SENTRY_MATE_REQUIRE_MOUNTS` (the folders are the defaults, `/data` and `/keys`), and has no `VOLUME`: a
folder left out of the compose file stops it at once instead of going into an anonymous volume. The certificate is
normally made by the first `serve`; `token new` and `fingerprint` make it without names if it isn't there yet. Times
in the log are UTC. `TZ` (an IANA name such as `Europe/Berlin`) is only the time zone a new car's days are grouped
in until it has its own; without it, UTC.

## Ports and limits

| Port | What | State |
|---|---|---|
| 10100/tcp | CAN Bridge uploads: HTTPS, TLS 1.3 and HTTP/1.1 only | done |
| 10101/tcp | Web UI and its `/api/v1`: HTTPS, TLS 1.3 and HTTP/1.1 only | done |

Firewalls, another host port and uploads from away from home: [Ports and firewalls](install.md#ports-and-firewalls).

The upload port takes 6 connections per address (an IPv6 /64 counts as one address) and 256 in all. A TLS
handshake must finish within 10 s, the headers within 10 s, a request within 10 minutes. A connection counts as
anonymous until a request on it is answered with success (it carried a valid token); at most 192 are, and a new one
beyond closes the oldest anonymous one, so connections without a token can't keep the CAN Bridge out.
Any other path than the upload API gets `404`. The protocol is
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

## Files

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
| `.lock`, `.gate`, `.gate-door` | Empty. One server at a time on the folder, and the pause an export or a move takes for its snapshot. |
| `.move/` | Move notes: a move code waiting, a move done, an import done. |
| `.moving/`, `.importing/` | A move or an import coming in, until it switches. Removed then. |
| `.export/` | An export's snapshot while it is written. Removed after. |
| `db/` | Each car's history (drives, charges, states, time series), made from the logs: rebuilt from them if lost. |
| `accounts/`, `audit/` | The web UI's accounts, sessions and their audit log. |
| `web/` | The web UI's own files: uploaded UI packs, the pack chosen, preferences. |
| `*.smexport` | An export file put there to import. Delete it once the new server runs. |

The keys folder:

| Path | What |
|---|---|
| `sentry-mate.key` | 32 random bytes that open the vault, readable only by the user it runs as (1000, or 568 on TrueNAS). |
| `RECOVERY-KEY.txt` | The recovery key, written on the first start and by a new recovery key. Delete it once copied (or tick "I have saved it" on the web page). |
| `SETUP-CODE.txt` | The web UI's one-time setup code, while no account exists. Removed once the first admin exists. |
| `.moving/`, `.importing/` | The key file a move or an import writes for this server, until it switches. |

## Changing the certificate

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
