# Install

[← Back to the README](../README.md)

Sentry Mate runs as one Docker container. It takes the Screenmate CAN
Bridge's uploads on port 10100 (HTTPS, TLS 1.3 only) and keeps everything in
two folders. This guide covers a Linux server with Docker Compose and TrueNAS
SCALE, then the CAN Bridge app, updates, backups, restoring and moving.

- [What you need](#what-you-need)
- [The two folders](#the-two-folders)
- [Docker Compose](#docker-compose)
- [TrueNAS SCALE](#truenas-scale)
- [The CAN Bridge app](#the-can-bridge-app)
- [A new signing key](#a-new-signing-key)
- [Updating](#updating)
- [Backups](#backups)
- [Restore](#restore)
- [Moving to a new server](#moving-to-a-new-server)
- [Moving the folders on this server](#moving-the-folders-on-this-server)
- [Ports and firewalls](#ports-and-firewalls)
- [Settings](#settings)
- [When it does not start](#when-it-does-not-start)

## What you need

- A server that is on when the car is home: Linux with Docker and the
  Compose plugin, or TrueNAS SCALE 24.10 or newer. The image is for amd64 and
  arm64 (for example a Raspberry Pi 4 or 5 with a 64-bit system).
- About 1 GiB of free memory for a few seconds when the vault is made (the
  first start), for a restore and for a new recovery key: the recovery key is
  protected with Argon2id at 1 GiB. The rest of the time it needs very little.
- The Screenmate CAN Bridge app on the car's Screenmate.

It runs next to Sentry Six Manager on the same server: that one uses ports
9000 and 10000, Sentry Mate 10100 (and 9100 later, for its web UI).

## The two folders

| Folder | In the container | What is in it |
| --- | --- | --- |
| data | `/data` | Everything Sentry Mate stores, sealed: the logs exactly as uploaded, the tokens, the trust records, the certificate's key. |
| keys | `/keys` | The key file that opens the data (`sentry-mate.key`). On first start also `RECOVERY-KEY.txt`, until you delete it. |

**Keep keys apart from data.** Give it its own folder, on another disk if
you can, and never put it in the same backup as the data. With both, anyone
can open the data. With the data alone, nobody can without the recovery key.
Sentry Mate refuses a keys folder that is the data folder, or a folder right
inside it on the server. It can't see one deeper inside: don't do that
either.

**Both must be mounted**, each with its own line under `volumes:`. Without
one, Sentry Mate does not start, and the log says which line to add. A
folder that is not mounted would be lost with the container at the next
update, and the key file with it.

**Both belong to the user Sentry Mate runs as**, and nobody else needs them:
1000:1000 with Docker Compose (the image's own user), 568:568 on TrueNAS (its
apps user). A folder Docker makes because it was missing belongs to root;
Sentry Mate then stops and says what to run
([When it does not start](#when-it-does-not-start)).

## Docker Compose

1. Make a folder and get the compose file:

   ```bash
   mkdir sentry-mate && cd sentry-mate
   curl -fsSLO https://raw.githubusercontent.com/skittles-fivem/Sentry-Mate/main/docker-compose.yml
   ```

2. Make the two folders and give them to the user 1000:

   ```bash
   mkdir -p data keys
   sudo chown 1000:1000 data keys
   sudo chmod 700 data keys
   ```

   For keys on another disk (a second drive, a USB stick that stays in),
   make the folder there the same way and put its path instead of `./keys`
   in `docker-compose.yml`, for example `- /mnt/usb/sentry-mate-keys:/keys`.

3. Start it:

   ```bash
   docker compose up -d
   docker compose ps
   ```

   After a few seconds the status says `(healthy)`. The log has the
   certificate SHA-256:

   ```bash
   docker logs sentry-mate
   ```

   ```
   ... WARN sentry_mate: new vault made: its recovery key is in /keys/RECOVERY-KEY.txt — copy it somewhere safe off this server, then delete that file
   ... INFO sentry_mate: CAN Bridge uploads on https://0.0.0.0:10100/api/can/logs (TLS 1.3), certificate SHA-256 <64 hex digits>, vault <id>
   ... INFO sentry_mate: no upload token yet: make one with `docker exec sentry-mate /sentry-mate token new --car "My car"`
   ```

4. Take the recovery key:

   ```bash
   sudo cat keys/RECOVERY-KEY.txt
   ```

   Copy the line that starts with `SMRK-` into a password manager, or write
   it on paper, and keep it away from the server. Then delete the file:

   ```bash
   sudo rm keys/RECOVERY-KEY.txt
   ```

   With the key file, it is the only way to open the data. Lose both and
   nobody can. Until the file is gone, the log says so at every start.

5. Make an upload token for the car. It is shown once, with the address and
   the certificate SHA-256 for the bridge:

   ```bash
   docker exec sentry-mate /sentry-mate token new --car "Model Y"
   ```

   Then fill in [the CAN Bridge app](#the-can-bridge-app).

If your user is not in the `docker` group, put `sudo` before the `docker`
commands.

To build the image yourself instead (in a clone of the repository), turn on
the `build: .` line in `docker-compose.yml` and start it with
`docker compose up -d --build`. Your build takes the published image's name,
and `--version` shows no build number. A later `docker compose pull`
replaces it with the published image.

## TrueNAS SCALE

On TrueNAS SCALE 24.10 or newer, install it as a custom app with
[`truenas/sentry-mate.yaml`](../truenas/sentry-mate.yaml).

1. **Datasets.** In *Datasets*, add two with the preset *Generic*: one for
   the data (for example `tank/apps/sentry-mate`) and one for the keys. Put
   the keys dataset on another pool if you have one (for example
   `ssd/sentry-mate-keys`). Otherwise put it next to the data one (for
   example `tank/apps/sentry-mate-keys`), not inside it. Then, in
   *System → Shell* (it asks for your password), give both to the apps user
   (568) and to nobody else. Put your datasets' paths in these lines (the
   keys on another pool: for example `/mnt/ssd/sentry-mate-keys`):

   ```bash
   sudo chown 568:568 /mnt/tank/apps/sentry-mate /mnt/tank/apps/sentry-mate-keys
   sudo chmod 700 /mnt/tank/apps/sentry-mate /mnt/tank/apps/sentry-mate-keys
   ```

2. **Install.** *Apps → Discover Apps →* the three dots at the top right
   *→ Install via YAML*. Name it `sentry-mate` and paste the file. Change the
   two lines marked `CHANGE` to your datasets' paths. Save.

3. **Certificate SHA-256.** *Apps → Installed → sentry-mate → Workloads →*
   the log icon shows the line `CAN Bridge uploads on ... certificate
   SHA-256 <64 hex digits>`.

4. **Recovery key.** In *System → Shell*, with your keys dataset's path
   (on another pool: for example `/mnt/ssd/sentry-mate-keys`):

   ```bash
   sudo cat /mnt/tank/apps/sentry-mate-keys/RECOVERY-KEY.txt
   ```

   Copy the line that starts with `SMRK-` into a password manager, or write
   it on paper, and keep it away from TrueNAS. Then delete the file:

   ```bash
   sudo rm /mnt/tank/apps/sentry-mate-keys/RECOVERY-KEY.txt
   ```

5. **Upload token** for the car, in *System → Shell*:

   ```bash
   sudo docker exec sentry-mate /sentry-mate token new --car "Model Y"
   ```

   Then fill in [the CAN Bridge app](#the-can-bridge-app).

The other commands in this guide work the same way in *System → Shell*,
with `sudo` before `docker`.

## The CAN Bridge app

On the Screenmate, open the CAN Bridge app. Under **Second Sentry Six
Manager (optional)**, fill in what `token new` printed:

| Field | What to put |
| --- | --- |
| Upload address | `https://<server>:10100/api/can/logs`, with the server's address on your network, for example `https://192.168.1.20:10100/api/can/logs`. If you changed the left port number in the compose file, use that number. |
| Token | The line that starts with `smt_`. |
| Certificate SHA-256 | The 64 hex digits. |

Then *Apply & restart*. The bridge sends every finished log to Sentry Six
Manager and to Sentry Mate, each on its own. While one of them can't be
reached, the logs wait on the Screenmate (up to *Keep up to (GB)*). Without
Sentry Six Manager, use the first section, **Sentry Six Manager (CAN log
upload)**, the same way. The bridge's messages say "Sentry Manager" in both
sections.

**Check it works.** First check that the server can be reached from the
car's Wi-Fi: on a phone or laptop on that network, open
`https://<server>:10100/api/can/logs/ping` and accept the certificate
warning. An answer with `bad_token` means the address, the port and the
firewall are right (the browser has no token). The upload address itself,
without `/ping`, answers `Nothing here.`, and that is normal too.

The bridge uploads a log once it is finished. After the first one,
`token list` shows the car's key in place of `not tied to a car key yet`:

```bash
docker exec sentry-mate /sentry-mate token list
```

A token is tied to the car's key on its first upload: one token per car.
For another car, make another token. To stop a token (its id is the first
column of `token list`):

```bash
docker exec sentry-mate /sentry-mate token revoke <id>
```

The certificate SHA-256 again, any time:

```bash
docker exec sentry-mate /sentry-mate fingerprint
```

It stays the same across restarts, updates and moves: the certificate's key
is in the data folder.

## A new signing key

The CAN Bridge signs every log with a key kept in the Screenmate. The first
signed log ties the car to that key. Logs signed by another key are kept,
but not used. That happens when the Screenmate makes a new key, or when
someone else uploads with the car's token.

See the keys of a car. The car key is the last column of `token list`:

```bash
docker exec sentry-mate /sentry-mate keys <car key>
```

It lists each key, how many logs it signed, when they came, and which key
is trusted. A new key says `not trusted: its logs are kept, not used`.

Then open the CAN Bridge app and look at **Log signing key**:

- **It is the new key.** Trust it. Copy the key from the list:

  ```bash
  docker exec -it sentry-mate /sentry-mate keys trust <car key> <signing key>
  ```

  It says what will change. Type `yes` and press Enter. Its logs are used
  from then on, and those it signed before too. The key trusted before
  still counts for the logs that came before. Sentry Mate keeps running.

- **It is not the new key.** Don't trust it: someone else signed those
  logs. Revoke the car's token, make a new one and put it in the app
  ([The CAN Bridge app](#the-can-bridge-app)).

## Updating

Docker Compose, in the folder with `docker-compose.yml`:

```bash
docker compose pull && docker compose up -d
```

TrueNAS often keeps the copy of `latest` it already has, so fetch the new
one in *System → Shell*:

```bash
sudo docker pull ghcr.io/skittles-fivem/sentry-mate:latest
```

Then *Apps → Installed → sentry-mate → Stop*, then *Start*. If the version
didn't change, *Edit* the app and *Save* to have TrueNAS recreate it.

The version you run:

```bash
docker exec sentry-mate /sentry-mate --version
```

It shows the build, for example `sentry-mate 0.1.0 (build-1001, sha-6f83191,
2026-11-17T19:06:35Z)`, and the log shows the same line at every start. The
data, the tokens and the certificate stay.

Every image also has a `build-N` and a `sha-` tag. To stay on one with
Docker Compose, put it in a `.env` file next to `docker-compose.yml`, then
run `docker compose up -d`:

```bash
echo SENTRY_MATE_TAG=build-1001 > .env
```

On TrueNAS, *Edit* the app and change `latest` in the `image:` line.

**Going back to an older version needs a backup of the data made before the
update.** A newer version can store files in a newer way that an older one
can't open (the README's Security part has the details). Back up the data
before an update if you want to be able to go back.

## Backups

**The data folder.** Back it up like any folder. It is sealed, so the copy
can go anywhere: another disk, another NAS, a cloud drive. Stop Sentry Mate
for a clean copy:

```bash
docker compose stop
sudo tar -C data -cpf ~/sentry-mate-data-$(date +%F).tar .
docker compose start
```

Then copy that file off the server. On TrueNAS, give the data dataset a
periodic snapshot task (*Data Protection → Periodic Snapshot Tasks*), and a
replication or cloud sync task for a copy elsewhere. A snapshot is taken in
one instant, like a power cut, which Sentry Mate handles, so the app can keep
running. Leave the keys dataset out of those tasks (no recursive task on a
dataset that holds both).

**The keys folder.** It needs no backup if you keep the recovery key: a
restore makes a new key file. If you keep a copy anyway, keep it apart from
the data backups (a USB stick in a drawer), never in the same backup.

**The recovery key.** In a password manager or on paper. With a data backup,
it is all you need to get everything back.

A data backup opens with the key file or the recovery key it had when it
was made:

- A restore writes a new key file, so data backups from before it open only
  with the recovery key: after putting one back, run the restore again.
- `recovery-key new` replaces the recovery key, so data backups from before
  it need the old one. Keep the old recovery key as long as you keep those
  backups.

To make a new recovery key (the old one stops working; the new one is shown
once, in the terminal, not in the log):

```bash
docker exec sentry-mate /sentry-mate recovery-key new
```

## Restore

### The key file is lost, or a new server with only the data

You need the recovery key. The restore writes a new key file into the keys
folder. It takes a few seconds and about 1 GiB of memory.

Docker Compose, in the folder with `docker-compose.yml`:

```bash
docker compose stop
docker compose run --rm -it sentry-mate restore
docker compose up -d
```

It asks `Recovery key:`. Type or paste it, then press Enter. It answers
`Restored: a new key file is in /keys.`

TrueNAS: *Apps → Installed → sentry-mate → Stop*. Then, in *System →
Shell*, with your two datasets' paths:

```bash
sudo docker run --rm -it --user 568:568 -v /mnt/tank/apps/sentry-mate:/data -v /mnt/tank/apps/sentry-mate-keys:/keys ghcr.io/skittles-fivem/sentry-mate:latest restore
```

Type the recovery key when it asks, then *Start* the app.

### The data from a backup

Docker Compose, in the folder with `docker-compose.yml`:

1. Stop Sentry Mate and put the data folder aside:

   ```bash
   docker compose stop
   sudo mv data data.old
   mkdir data
   ```

2. Put the backup in. Use your backup file's name in place of
   `sentry-mate-data-<date>.tar`:

   ```bash
   sudo tar -C data -xpf ~/sentry-mate-data-<date>.tar
   ```

3. Check that the backup went in: this must show `vault.json`. If it
   doesn't, fix step 2 before you start it.

   ```bash
   sudo ls data
   ```

4. Give it to the user Sentry Mate runs as, and start it:

   ```bash
   sudo chown -R 1000:1000 data
   sudo chmod 700 data
   docker compose up -d
   ```

5. It works when `fingerprint` prints the same SHA-256 as the CAN Bridge
   app has, and `token list` shows your tokens. Only then delete `data.old`:

   ```bash
   sudo rm -r data.old
   ```

If the log says `the key file doesn't open this vault`, the backup is from
before a restore: run the restore above.

On TrueNAS, stop the app, roll the data dataset back to a snapshot (in the
dataset's snapshots, *Rollback*), then start it. If the log then says `the
key file doesn't open this vault`, the snapshot is from before a restore:
run the TrueNAS restore above.

## Moving to a new server

The certificate and the tokens are in the data folder, so the bridge keeps
working with the same token and certificate SHA-256. Change only its
address, if the server's address changed.

1. On the old server, in the folder with `docker-compose.yml`, stop
   Sentry Mate and copy what is in the two folders, each to its own file in
   your home folder. The keys file is then made yours and readable by you
   only:

   ```bash
   docker compose stop
   sudo tar -C data -cpf ~/sentry-mate-data.tar .
   sudo tar -C keys -cpf ~/sentry-mate-keys.tar .
   sudo chown "$(id -un)" ~/sentry-mate-data.tar ~/sentry-mate-keys.tar
   chmod 600 ~/sentry-mate-keys.tar
   ```

   Or take only the data: on the new server you then
   [restore](#restore) with the recovery key.

2. Copy the files into your home folder on the new server (`scp`, a USB
   stick). Carry the keys file apart from the data file (another USB stick,
   or a separate `scp`), never in the same backup. To TrueNAS: copy them into an
   SMB share on it (for example one at `/mnt/tank/share`), or turn on SSH
   (*System → Services*) for `scp`. TrueNAS has SSH off at first and does
   not mount USB sticks.

3. On the new server, do Docker Compose steps 1 and 2 (the compose file and
   the two folders), or TrueNAS step 1 (the datasets). Put the copies in,
   and give them to the user Sentry Mate runs as there, with `-R` for
   everything inside:

   ```bash
   sudo tar -C data -xpf ~/sentry-mate-data.tar
   sudo tar -C keys -xpf ~/sentry-mate-keys.tar
   sudo chown -R 1000:1000 data keys
   sudo chmod 700 data keys
   sudo ls data
   ```

   On TrueNAS the user is 568, the folders are your datasets, and the files
   are where you copied them (here the share at `/mnt/tank/share`). Delete
   them from the share right after, the keys one above all:

   ```bash
   sudo tar -C /mnt/tank/apps/sentry-mate -xpf /mnt/tank/share/sentry-mate-data.tar
   sudo tar -C /mnt/tank/apps/sentry-mate-keys -xpf /mnt/tank/share/sentry-mate-keys.tar
   sudo chown -R 568:568 /mnt/tank/apps/sentry-mate /mnt/tank/apps/sentry-mate-keys
   sudo chmod 700 /mnt/tank/apps/sentry-mate /mnt/tank/apps/sentry-mate-keys
   sudo rm /mnt/tank/share/sentry-mate-data.tar /mnt/tank/share/sentry-mate-keys.tar
   sudo ls /mnt/tank/apps/sentry-mate
   ```

   The last line must show `vault.json`. If it doesn't, the data didn't go
   in: fix that before you start it.

4. Start it (`docker compose up -d`, or *Install via YAML*), and check that
   `fingerprint` prints the same SHA-256 as the CAN Bridge app has, and
   `token list` shows your tokens. Then delete the `.tar` files on both
   servers (`rm ~/sentry-mate-data.tar ~/sentry-mate-keys.tar`), and leave
   the old server stopped.

## Moving the folders on this server

To put the keys on a second disk after the install, or the data on a bigger
one. Copy first, then change the volume line: pointed at an empty folder,
Sentry Mate finds nothing to open and stops.

1. Note what `fingerprint` prints. Then stop Sentry Mate, and copy the
   folder into a new one (here the keys, to `/mnt/disk2/sentry-mate-keys`;
   the data the same way, from `data/`):

   ```bash
   docker exec sentry-mate /sentry-mate fingerprint
   docker compose stop
   sudo mkdir -p /mnt/disk2/sentry-mate-keys
   sudo cp -a keys/. /mnt/disk2/sentry-mate-keys/
   sudo chown -R 1000:1000 /mnt/disk2/sentry-mate-keys
   sudo chmod 700 /mnt/disk2/sentry-mate-keys
   ```

2. In `docker-compose.yml`, put the new path in that volume line, for
   example `- /mnt/disk2/sentry-mate-keys:/keys`. Then start it:

   ```bash
   docker compose up -d
   docker exec sentry-mate /sentry-mate fingerprint
   ```

3. It must print the same SHA-256 as before. Then delete the old folder
   (`sudo rm -r keys`, or `data`).

On TrueNAS: stop the app, make the new dataset, then in *System → Shell*
copy into it with `sudo cp -a <old path>/. <new path>/`, and run
`sudo chown -R 568:568` and `sudo chmod 700` on the new path. *Edit* the
app, change the line marked `CHANGE` to the new path, and *Save*. Check
`fingerprint` the same way before you delete the old dataset.

## Ports and firewalls

| Port | What | |
| --- | --- | --- |
| 10100/tcp | CAN Bridge uploads, HTTPS, TLS 1.3 only | Published: the Screenmate connects to it. |
| 9100/tcp | The web UI | Kept for later: not built yet, nothing listens. |

- Sentry Six Manager uses 9000 and 10000, so both run on one server.
- Another app already on 10100: change only the left number (for example
  `"10200:10100"`) and use it in the bridge's address. The address that
  `token new` prints always shows 10100, the port inside the container.
- Docker opens a published port on the server itself, on most servers even
  past `ufw`. A firewall between the car's Wi-Fi and the server (a separate
  network for cars and gadgets, say) must let TCP 10100 through to the
  server.
- Away from home: the bridge uploads when it can reach the address. With an
  address on your home network, logs wait on the Screenmate until the car is
  home on Wi-Fi. To upload from anywhere, forward TCP 10100 on the router to
  the server, or let the Screenmate reach home through a VPN. The port speaks
  only TLS 1.3, takes uploads only with a token in use, and slows down wrong
  tokens.

## Settings

The compose and TrueNAS files set everything. These are the environment
variables it reads:

| Variable | What it does |
| --- | --- |
| `SENTRY_MATE_NAMES` | Names or addresses of the server for the certificate, comma separated (the OPTIONAL lines in both files). Read only when the certificate is made, on first start. The bridge pins the SHA-256 and does not check names, so it can stay off. |
| `RUST_LOG` | How much it logs: `info` (the default), or `warn` for less. `warn` also hides the line with the certificate SHA-256; `fingerprint` still prints it. |
| `SENTRY_MATE_DATA`, `SENTRY_MATE_KEYS`, `SENTRY_MATE_UPLOAD_PORT`, `SENTRY_MATE_BIND` | `/data`, `/keys`, `10100` and `0.0.0.0`. Leave them: change the folders in the volume lines and the port in the left number instead. |
| `SENTRY_MATE_REQUIRE_MOUNTS` | `1` in the image: it starts only when the data and keys folders are each mounted from the server. Leave it. |

Times in the log are UTC. It does not read `TZ`.

## When it does not start

The log says why: `docker logs sentry-mate` (TrueNAS: the app's log icon,
or `sudo docker logs sentry-mate` in *System → Shell*). While it keeps
stopping, `docker compose ps` shows `Restarting`.

| The log says | What to do |
| --- | --- |
| `Sentry Mate runs as uid 1000 (group 1000) and can't write to the data folder /data: it belongs to uid 0 (group 0), root.` (and the same for the keys folder) | The folders belong to root: Docker made them because they were missing, or they were made with `sudo`. Docker Compose, in the folder with `docker-compose.yml`: `sudo chown 1000:1000 data keys && sudo chmod 700 data keys` (or the paths in your volume lines). TrueNAS: `sudo chown 568:568` and `sudo chmod 700` on the two datasets' paths. It starts again by itself within a minute, or run `docker compose restart`. |
| `... it belongs to uid 1000 (group 1000).` and `Or run Sentry Mate as the folder's owner: user: "1000:1000"` | The folders come from an install that ran as another user (Docker Compose 1000, TrueNAS 568). Give them to the user it runs as now, with `sudo chown -R`. |
| `Permission denied (os error 13)`, then `the data and keys folders and everything in them must belong to it` | Something inside the folders belongs to another user, most often after a move with a `chown` without `-R`. Run the `sudo chown -R` it gives. |
| `the data folder /data and the keys folder /keys are not mounted` (or `the keys folder /keys is not mounted`, or the same for the data folder) | A volume line is missing, or its right side is not `:/data` or `:/keys`. Add the lines the log shows under `volumes:` (TrueNAS: with your datasets' paths), then `docker compose up -d` (TrueNAS: *Edit* the app, *Save*). Nothing was written. |
| `the keys folder /keys is a folder inside the data folder on the server (it is /data/keys here)` | The keys line points into the data folder (TrueNAS: the keys dataset is inside the data dataset). Make a folder of its own next to the data one, copy what is in the old keys folder into it, and put its path in the keys line ([Moving the folders on this server](#moving-the-folders-on-this-server)). Then delete the old keys folder from the data folder. |
| `the keys folder /keys and the data folder /data are the same folder of the server` | Both volume lines have the same path. Make a folder of its own for the keys, give it to the user (`sudo chown 1000:1000` and `sudo chmod 700`; TrueNAS: 568), and put its path in the keys line. If the data folder has `sentry-mate.key` in it, move that file into the new folder first (`sudo mv`), and `RECOVERY-KEY.txt` too if it is there. |
| `the data folder /data is read-only` | The volume line ends in `:ro`: remove it. |
| `the key file is missing: restore it with the recovery key` | The keys folder is empty: a wrong path in the keys line, a disk that isn't there, or the key file is lost. Fix the path, or [restore](#restore) (with Sentry Mate stopped: not with `docker exec`). |
| `the keys folder has a key file (/keys/sentry-mate.key), but the data folder /data has no vault` | The data folder is empty or not the right one: a wrong path in the data line, a disk that isn't there, or a backup that didn't go in. Sentry Mate makes no new vault over a key file, which may open your data. Fix the path, or put the data back (`sudo ls data` must show `vault.json`). To start over with no data on purpose, delete the key file. |
| `the key file doesn't open this vault (another machine's, or replaced)` | The keys folder is another install's, or the data is a backup from before a restore. Mount the right keys folder, or [restore](#restore) with this data's recovery key. |
| `that recovery key doesn't open this vault` (from `restore`) | A typo, the key of another install, or a key replaced with `recovery-key new`. |
| `/keys/RECOVERY-KEY.txt is still there` (it starts all the same) | Copy the recovery key somewhere safe and delete the file. |
| Only the first line, `Sentry Mate 0.1.0 ...`, again and again, and `docker compose ps` shows `Restarting (137)` | Out of memory: making the vault on first start and a restore need about 1 GiB for a few seconds. Free some memory, or raise the container's memory limit. |
| `docker compose up -d` says `Bind for 0.0.0.0:10100 failed: port is already allocated` | Another app uses 10100: change the left number ([Ports](#ports-and-firewalls)). |
| `docker compose up -d` or `docker compose pull` says `error from registry: denied` | The image can't be pulled: it is not public yet, or the `image:` line is mistyped. It is `ghcr.io/skittles-fivem/sentry-mate`. `docker pull ghcr.io/skittles-fivem/sentry-mate:latest` must work without a login. |
| `docker compose pull` says `not found` or `manifest unknown` for a `build-` or `sha-` tag | The tag in `.env` (`SENTRY_MATE_TAG`) doesn't exist: check its number, or delete `.env` for `latest`. |

What the bridge says, in its upload section:

| The bridge says | What to do |
| --- | --- |
| `Sentry Manager not reachable` | A wrong address or port, the server or the container is off, or a firewall in between. |
| `Sentry Manager's certificate doesn't match the SHA-256 entered here` | Copy the SHA-256 again (`fingerprint`). It changes only when the data folder is replaced by a new install's. |
| `Sentry Manager refused the token` | A typo, or the token was revoked: make a new one. |
| `Sentry Manager answered 403 wrong_car` | That token is tied to another car: one token per car. |
| `Sentry Manager doesn't know this address` | The address must end in `/api/can/logs`. |
