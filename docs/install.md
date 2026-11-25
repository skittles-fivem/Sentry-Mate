# Install

[← Back to the README](../README.md)

Sentry Mate runs as one Docker container. It takes the Screenmate CAN
Bridge's uploads on port 10100 (HTTPS, TLS 1.3 only), serves its web UI on
port 10101 (HTTPS, the same certificate) and keeps everything in two
folders. This guide covers a Linux server with Docker Compose and TrueNAS
SCALE, then the CAN Bridge app, updates, backups, restoring and moving.

Every command is in [commands.md](commands.md). The bridge app in detail is in
[cars-and-bridge.md](cars-and-bridge.md).

- [What you need](#what-you-need)
- [The two folders](#the-two-folders)
- [Docker Compose](#docker-compose)
- [TrueNAS SCALE](#truenas-scale)
- [The CAN Bridge app](#the-can-bridge-app)
- [Add a Tesla account](#add-a-tesla-account)
- [Add a TeslaFi token](#add-a-teslafi-token)
- [Connect a local TeslaMate](#connect-a-local-teslamate)
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
  A container memory limit below that can make these steps fail. Memory in
  use and the limit to set: [Memory](#memory).
- No Rust and no clone of this repo: the image is published,
  `ghcr.io/skittles-fivem/sentry-mate`, and you only download the compose
  file. Internet is needed to pull the image and its updates. While it runs,
  Sentry Mate makes no outgoing connections unless an admin turns one on
  (MQTT, a Tesla account, a move: [security](security.md#security)).
  Building the image yourself is
  optional ([development.md](development.md#running-your-own-build)).
- Disk: the plan measured about 0.23 MB a minute parked and awake, and about
  0.76 MB a minute driving. That is roughly 30 to 100 GB a year for a car
  awake 6 hours a day. Logs are kept; no command deletes them yet. A new
  upload is refused while free space is under twice its size plus 2 GiB.
- Logs: Sentry Mate takes up to 200,000 per car key. The bridge writes about
  one log a minute while the car is awake, so that is about 18 months at 6
  hours a day, and 4 to 5 months for a car awake all day (for example in
  Sentry Mode). After that every new log is refused, and no command frees
  room yet.
- The Screenmate CAN Bridge app on the car's Screenmate, build 149 or later
  ([The CAN Bridge app](cars-and-bridge.md#the-can-bridge-app)).
- Optional: a Tesla account, for reading the cars from Tesla's API as well
  ([Add a Tesla account](#add-a-tesla-account)).
- Optional: a TeslaFi token, if you pay for TeslaFi, for reading the polls
  TeslaFi already makes of your car ([Add a TeslaFi token](#add-a-teslafi-token)).
- Optional: your own TeslaMate on the local network, for reading its MQTT
  topics ([Connect a local TeslaMate](#connect-a-local-teslamate)).
- It can share the server with Sentry Six Manager
  ([Ports and firewalls](#ports-and-firewalls)).

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
update, and the key file with it. The image has no `VOLUME`, so nothing
goes into a folder that would be lost with the container.

**Both belong to the user Sentry Mate runs as**, and nobody else needs them:
1000:1000 with Docker Compose (the image's own user), 568:568 on TrueNAS (its
apps user). A folder Docker makes because it was missing belongs to root;
Sentry Mate then stops and says what to run
([When it does not start](#when-it-does-not-start)).

**A new vault is made only when there is neither a vault nor a key file**,
as on the first start. A data folder with no vault next to a keys folder
with a key file (a wrong path, a disk that isn't there, a backup not put
back) stops it: it never makes a new vault over a key file, which may open
your data. Fix the path, or put the data back. To start over with no data
on purpose, delete the key file.

The compose file runs the image as uid 1000 (group 1000), with a read-only
root and no capabilities, and publishes port 10100. The TrueNAS file does
the same as 568:568.

## Docker Compose

1. Make a folder and get the compose file:

   ```bash
   mkdir sentry-mate
   ```

   ```bash
   cd sentry-mate
   ```

   ```bash
   curl -fsSLO https://raw.githubusercontent.com/skittles-fivem/Sentry-Mate/main/docker-compose.yml
   ```

2. Make the two folders and give them to the user 1000:

   ```bash
   mkdir -p data keys
   ```

   ```bash
   sudo chown 1000:1000 data keys
   ```

   ```bash
   sudo chmod 700 data keys
   ```

   For keys on another disk (a second drive, a USB stick that stays in),
   make the folder there the same way and put its path instead of `./keys`
   in `docker-compose.yml`, for example `- /mnt/usb/sentry-mate-keys:/keys`.

3. Start it:

   ```bash
   docker compose up -d
   ```

   ```bash
   docker compose ps
   ```

   The first start makes the vault. That takes a few seconds, with only the
   version line in the log until it is done. Then the status says
   `(healthy)`, and the log shows the upload address, the certificate
   SHA-256 and the vault's id:

   ```bash
   docker logs sentry-mate
   ```

   ```
   ... WARN sentry_mate: new vault made: its recovery key is in /keys/RECOVERY-KEY.txt — copy it somewhere safe off this server, then delete that file
   ... INFO sentry_mate: CAN Bridge uploads on https://0.0.0.0:10100/api/can/logs (TLS 1.3), certificate SHA-256 <64 hex digits>, vault <id>
   ... INFO sentry_mate: no upload token yet: make one with `docker exec sentry-mate /sentry-mate token new --car "My car"`
   ... INFO sentry_mate: web UI on https://0.0.0.0:10101/ (TLS 1.3), the same certificate
   ... WARN web: No account yet: open the web page and type this one-time code there (valid 24 hours, also in /keys/SETUP-CODE.txt). first-run set-up code: SETUP-<code>
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
   the certificate SHA-256 for the bridge. You can also make it in the web UI
   after step 6 (**Settings → Cars**), or with this command:

   ```bash
   docker exec sentry-mate /sentry-mate token new --car "Model Y"
   ```

   Then fill in [the CAN Bridge app](#the-can-bridge-app).

6. Open the web UI: `https://<this machine's address>:10101/`. The browser
   warns about the certificate once (it is the server's own): check that the
   SHA-256 it shows is the one in the log, then go on. On the first page,
   type the setup code from the log (or `sudo cat keys/SETUP-CODE.txt`)
   with a name and password for the first admin. Any password that isn't
   empty is taken; the page warns when one is easy to guess and lets you
   use it anyway. See [The web UI](commands.md#the-web-ui).

7. Check the time zone. Set-up takes your browser's time zone as the one
   new cars start in (days, weeks and prices by time of day are counted in
   it, and every page shows the car's times in it). Change it in
   **Settings → Cars** ("Time zone for new cars"). Each car
   keeps its own after that, in **Settings → Cars** → the car → **Time
   zone**; the clock in the page header names the zone the page shows
   (`2:52 PM CST`).

8. To see map pictures under the routes, either add an offline map file
   (below: nothing leaves the server) or have an admin turn on online tiles
   in **Settings → Maps and addresses** (off by default: no browser asks an
   outside server until then) and pick a source (OpenStreetMap standard,
   OpenTopoMap or your own address). Until then every map says "No map
   tiles yet", and for an admin offers "Use the online map" (OpenStreetMap) in
   one click (after a confirm). See [Map tiles](security.md#map-tiles).

If it doesn't start, the log says why and what to do. For example `Sentry
Mate runs as uid 1000 (group 1000) and can't write to the data folder /data:
it belongs to uid 0 (group 0), root.` means the folders belong to root
(Docker makes missing ones owned by root). Run the `chown` and `chmod` of
step 2, then `docker compose restart`. The other messages are in
[When it does not start](#when-it-does-not-start).

If your user is not in the `docker` group, put `sudo` before the `docker`
commands.

To build the image yourself instead, see
[Running your own build](development.md#running-your-own-build).

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
   ```

   ```bash
   sudo chmod 700 /mnt/tank/apps/sentry-mate /mnt/tank/apps/sentry-mate-keys
   ```

2. **Install.** *Apps → Discover Apps →* the three dots at the top right
   *→ Install via YAML*. Name it `sentry-mate` and paste the file. Change the
   two lines marked `CHANGE` to your datasets' paths. Save.

3. **Certificate SHA-256.** *Apps → Installed → sentry-mate → Workloads →*
   the log icon shows the line `CAN Bridge uploads on ... certificate
   SHA-256 <64 hex digits>`. Once you are signed in as the admin, the web
   pages show it too, with a *Copy* button: *Settings → Server and access →
   Certificate, recovery key and backups*. A new certificate (for example
   after you add a host name) is made there too, with *Make a new
   certificate*; then enter its SHA-256 in the CAN Bridge again.

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

5. **Upload token** for the car: in the web UI after step 6 (**Settings →
   Cars**), or in *System → Shell*:

   ```bash
   sudo docker exec sentry-mate /sentry-mate token new --car "Model Y"
   ```

   Then fill in [the CAN Bridge app](#the-can-bridge-app).

6. **Web UI.** Open `https://<TrueNAS address>:10101/`, check the
   certificate's SHA-256 against the log, and set up the first admin with
   the setup code from the log (or
   `sudo cat /mnt/tank/apps/sentry-mate-keys/SETUP-CODE.txt`).

The `docker exec` and `docker logs` commands in this guide and in
[commands.md](commands.md) work the same way in *System → Shell*, with
`sudo` before `docker`. The recovery key file is in the keys dataset.

The `docker compose` steps don't work there: there is no compose file in
the shell's folder. TrueNAS has its own steps for them: *Stop* and *Start*
the app, and the `docker run` restore in [Restore](#restore). Use the
datasets' paths instead of `data/...` and `keys/...`.

`init` is for Docker Compose only: on TrueNAS the vault is made on the
first start.

## Memory

Measured on one car with about 23,000 CAN logs and 30 days of Tesla API data, in the image
([performance.md](plans/performance.md#memory-one-car-23000-logs-in-the-image) has the table and how):

| | process (RSS) |
|---|---:|
| started, idle, pages open, uploads arriving | 150 to 170 MB, and it doesn't grow |
| log checks, the GPS layout fit, an export, a move, a TeslaMate import | 250 to 300 MB, up to about 600 MB for a few seconds |
| a full rebuild | about 300 MB while it decodes, one core |
| the end of a full rebuild of a car with Tesla API (or TeslaFi, or local TeslaMate) data | about 300 MB plus 100 KB for every CAN log inside the days that API data covers, for about 5 seconds: 2.6 GB with 30 days of it beside 700 logs a day |
| making the vault (the first start, a restore, a new recovery key, the new server of a move) | 1 GiB for a few seconds (Argon2id) |

The container's own figure (in TrueNAS, or `docker stats`) is often around 1 GB more than this: the data files the
server read stay in the kernel's file cache, counted to the container. The kernel gives that back first when the
container reaches its limit, so it doesn't need room of its own. Sentry Mate also keeps the days it read, opened, in
a cache of its own of up to a quarter of the memory left under the limit (at most 4 GB): with a limit, it keeps
less.

**Recommended TrueNAS limit: 4 GiB** for one car. Why:

- Everything except the two short peaks above stays under 600 MB, so 1 GiB would do on most days.
- The vault's 1 GiB is needed whatever the limit: below about 1.5 GiB a restore, a new recovery key or a move to
  this server can fail.
- The end of a full rebuild is what sets it. With about 22,500 logs, all inside the Tesla API's days, it is about
  2.6 GB; 4 GiB covers that with room for the logs read ahead on the other cores (measured before that came: an
  estimated 10 to 20 MB more per thread decoding at a real car's log sizes, 8 threads at most). If the server is killed there, the rebuild carries on after the restart into
  the same step, and is killed again.
- A car with no Tesla API, TeslaFi or local TeslaMate data doesn't have that step: 2 GiB is enough for it.
- Many more logs inside the API's days (a year of both, say) need more: about 100 KB a log. Leave the limit off, or
  raise it, until the rebuild builds that step in pieces (performance.md, the same section).

On TrueNAS, set it in the app's YAML (*Apps → Installed → sentry-mate → Edit*), under the service:

```yaml
    deploy:
      resources:
        limits:
          memory: 4G
```

## The CAN Bridge app

On the Screenmate, open the CAN Bridge app (build 149 or later). Under
**Second Sentry Six Manager (optional)**, put the upload address, the token
and the certificate SHA-256 that `token new` printed, then press
**Apply & restart**. The fields, how to check it works and how the bridge
uploads are in [cars-and-bridge.md](cars-and-bridge.md#the-can-bridge-app).

## Add a Tesla account

Optional. With a Tesla account added, the server also reads your cars from
Tesla's Owner API (the unofficial API Tesla's own app uses): for a car with
no Screenmate, and for what SMO does not see (the charge limit, the software
version, the charger's brand). Where SMO and the Tesla API both have data,
the two make one history; every drive, charge and value says which it came
from. Nothing connects to Tesla until you add an account.

You need an admin account on Sentry Mate, and the server must be able to
reach `auth.tesla.com`, `owner-api.teslamotors.com` and
`streaming.vn.teslamotors.com` on port 443 (for a China account,
`auth.tesla.cn` and `owner-api.vn.cloud.tesla.cn`). A proxy for outgoing
connections is set in Settings (`outbound.proxy`).

**Sign in at Tesla** (the usual way):

1. In the web UI, open **Settings → Integrations → Tesla** and press
   **Sign in at Tesla**. Tesla's own sign-in page opens in a new tab.
2. Sign in there with your Tesla e-mail, password and two-step code. Sentry
   Mate never sees them.
3. The page ends on an address that does not load (it starts with
   `tesla://auth/callback?code=`). Copy that whole address from the address
   bar, paste it into Sentry Mate and press **Add**, with your own Sentry
   Mate password again. Do it within ten minutes.

**Or paste a refresh token** you already have from another tool: **Settings
→ Integrations → Tesla → Add with a refresh token**.

Either way the server checks it with Tesla before it keeps anything. Then:

4. Match each car of the account: to a car Sentry Mate already has (the one
   with the same VIN prefix is suggested), to a new car, or leave it out. A
   car is read only once it is matched.
5. Pick how often it asks (Settings → Integrations → Tesla account, Change):
   driving every 2.5 s, charging every 5 s, awake and idle every 60 s, and
   asleep the list of cars every 30 s are the defaults and the fastest; each
   can be made slower. A car that is asleep is never woken; only the list of
   cars is read then. There is no daily cap on this side: Tesla does the
   limiting. When Tesla answers 429 the server waits as long as Tesla asks,
   and the Tesla account section, Data health and the notices say "Tesla is
   limiting requests" with the time of the next try.

4. Pick how often TeslaFi is read: `normal` (the default: driving or
   charging every minute, awake every 3 min, asleep every 10 min), `often`
   (as fast as TeslaFi itself refreshes: every 15 s while driving or
   charging, every minute awake, every 3 min asleep) or `gentle` (2 min,
   5 min, 20 min). TeslaFi only has new data as often as it polls the car, so
   `often` is as fast as is useful. Whatever the pace, the server waits as
   long as TeslaFi asks it to (`Retry-After`) and backs off on errors.

To stop for a while, pause the account; to stop for good, remove it. Removing
deletes its sealed tokens; the history already stored stays. Also sign the
server out at Tesla (your Tesla account's security settings). If Tesla stops
taking the sign-in (after a password change, for example), a notice says so:
sign in again. What is sent, and where: [security](security.md#the-tesla-api-input).

The built-in designs are still getting their Tesla page. Until yours has it,
the same steps are API calls, listed in
[ui-api.md](ui-api.md#integrations-tesla).

## Add a TeslaFi token

Optional, for owners who pay for TeslaFi. With a TeslaFi token added, the
server also reads the polls TeslaFi makes of your car (about one a minute
while it is awake) from TeslaFi's API: for a car with no Screenmate, or for
the time SMO was offline. Where SMO or the Tesla API have data for the same
time, theirs win; TeslaFi's fills the rest, and every drive, charge and value
says which it came from ("Data from TeslaFi"). Nothing connects to TeslaFi
until you add a token, and the server only reads: it never wakes the car and
never sends it a command.

You need an admin account on Sentry Mate, and the server must be able to
reach `www.teslafi.com` on port 443. A proxy for outgoing connections is set
in Settings (`outbound.proxy`).

1. Sign in at TeslaFi and open **Settings → TeslaFi API**. Make a token there
   if you have none (TeslaFi shows it as a long string of letters and
   digits). Sentry Mate needs no command ticked on TeslaFi's **Commands** tab:
   it only reads.
2. In the Sentry Mate web UI, open **Settings → Integrations → TeslaFi** and
   paste the token, with a name for it and your own Sentry Mate password
   again. If TeslaFi shows your times in another time zone than the server's
   default, pick that zone too.
3. The server checks the token with TeslaFi before it keeps anything, then
   names the car TeslaFi polls (its VIN prefix). Match it to a car Sentry Mate
   already has (the one with the same VIN prefix is suggested), to a new car,
   or leave it out. Nothing is read into a car before it is matched.

To stop for a while, pause the account; to stop for good, remove it. Removing
deletes its sealed token; the history already stored stays. Also make a new
token on TeslaFi's API page, so the old one stops working there. If TeslaFi
stops taking the token (a new token was made there, or the subscription
ended), a notice says so: paste a new token. What is sent, and where:
[security](security.md#the-teslafi-input).

Every built-in design has the TeslaFi section on Settings: Integrations,
right after the Tesla account. The same steps are also API calls, listed in
[ui-api.md](ui-api.md#integrations-teslafi).

## Connect a local TeslaMate

Optional, for owners who already run TeslaMate on their own network. The
server subscribes to the MQTT topics TeslaMate publishes (`teslamate/cars/+/#`
unless you changed TeslaMate's `MQTT_NAMESPACE`) and reads each car's values
from them, as one more live input. It only reads: it never publishes to the
broker, never wakes the car and never sends a command. Where SMO, the Tesla API
or TeslaFi cover the same time, theirs win; the rest says "Data from local
TeslaMate". Nothing connects to TeslaMate until an admin adds it.

1. In the Sentry Mate web UI, open **Settings → Integrations → Local
   TeslaMate**. Type the MQTT broker TeslaMate publishes to (for example
   `192.0.2.10`, port 1883, or 8883 with TLS), its user name and password if
   the broker has accounts, and the topic prefix (`teslamate` by default).
2. Press **Test**: the server connects, listens a few seconds and lists the
   TeslaMate cars it saw. Nothing is saved by a test. Then press **Save**; both
   ask for your own Sentry Mate password again.
3. Match each TeslaMate car to a car Sentry Mate already has (the one with the
   same VIN prefix is suggested), to a new car, or leave it out. Nothing is
   read into a car before it is matched; press **Save matches**.

Optionally, turn on **Catch up from TeslaMate's database** in the same section
and give TeslaMate's PostgreSQL host, port (5432), database (`teslamate`) and a
user that may read its tables (a read-only user is enough). Every few minutes
(300 s by default) the server reads the drives, charges and positions MQTT
missed and stores them as a TeslaMate import. It is off by default.

**Passwords and plain text.** A password sent without TLS can be read by anyone
on the network between the two machines. So the server refuses to save a
broker or database password with TLS off unless you also tick **Allow
unencrypted (local network only)**, which is off by default; tick it only when
both machines are on your own local network. Without a password, plain TCP
needs no tick. Passwords are sealed on the server and never shown again; a
saved password is only sent to the host it was typed for, so changing the
host, port or TLS asks for it again. To pin a self-signed certificate, turn
TLS on and paste its SHA-256.

To stop for a while, pause it; to stop for good, remove it (its sealed
settings are deleted; the history already stored stays). Every built-in design
has the section on Settings: Integrations, right after TeslaFi. The same steps
are also API calls, listed in
[ui-api.md](ui-api.md#integrations-local-teslamate).

## Updating

Docker Compose, in the folder with `docker-compose.yml`:

```bash
docker compose pull
```

```bash
docker compose up -d
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
can't open ([No way back without a backup](security.md#no-way-back-without-a-backup)).
Back up the data before an update if you want to be able to go back.

- A start after a version that stored files in the clear seals them once.
  Unfinished uploads stored in the clear are dropped; the bridge sends them
  again.
- Built from a clone instead: see
  [Running your own build](development.md#running-your-own-build).

## Backups

**The data folder.** Back it up like any folder. It is sealed, so the copy
can go anywhere: another disk, another NAS, a cloud drive. Stop Sentry Mate
for a clean copy:

```bash
docker compose stop
```

```bash
sudo tar -C data -cpf ~/sentry-mate-data-$(date +%F).tar .
```

```bash
docker compose start
```

Or copy the folder whole while it is stopped:
`sudo cp -a data <backup folder>/`. Then copy the backup off the server.

- The copy stays sealed. It opens with the key file or the recovery key,
  and needs its own `vault.json`.
- Keep the files' names and layout: each sealed file is bound to its path
  inside the data folder. The folder as a whole can move.
- Stopping drops the chunk being sent at that moment. What had arrived is
  kept, and the bridge carries on from there.
- There is no backup command yet.

On TrueNAS, give the data dataset a periodic snapshot task (*Data
Protection → Periodic Snapshot Tasks*), and a replication or cloud sync task
for a copy elsewhere. A snapshot is taken in one instant, like a power cut,
which Sentry Mate handles, so the app can keep running. Leave the keys
dataset out of those tasks (no recursive task on a dataset that holds both).

**The keys folder.** It needs no backup if you keep the recovery key: a
restore makes a new key file. If you keep a copy anyway, keep it apart from
the data backups (a USB stick in a drawer), never in the same backup.

**The recovery key.** In a password manager or on paper, apart from both the
data backups and the key file. With a data backup, it is all you need to get
everything back.

A data backup opens with the key file or the recovery key it had when it
was made:

- A restore writes a new key file, so data backups from before it open only
  with the recovery key: after putting one back, run the restore again.
- `recovery-key new` replaces the recovery key, so data backups from before
  it need the old one. Keep the old recovery key as long as you keep those
  backups.

To make a new recovery key (the old one stops working; the new one is shown
once, in the terminal, not in the log, and waits in `keys/RECOVERY-KEY.txt`
until you delete it). Settings: Server and access does the same from the web
pages:

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
```

```bash
docker compose run --rm -it sentry-mate restore
```

It asks `Recovery key:`. Type or paste it, then press Enter. It answers
`Restored: a new key file is in /keys.`

Then start it:

```bash
docker compose up -d
```

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
   ```

   ```bash
   sudo mv data data.old
   ```

   ```bash
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
   ```

   ```bash
   sudo chmod 700 data
   ```

   ```bash
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

### A car's history file is damaged

If a file of one car's history can no longer be read (a disk fault, a copy
cut short), admins see a notice at the top of every page: *A file of
\<car\>'s history is damaged and can't be read*, with the file's name. Pages
that need the file answer with an error until it is mended. The notice says
which of two kinds it is:

- **It holds only what was decoded from the stored logs** (a day of decoded
  samples). A rebuild makes it again from the logs: *Rebuild all data* on
  the car's Data health page, or *Rebuild the period* on its Drives page
  with the period set over that time. No backup is needed.
- **The car's drives table** (`drives.snap` and `drives.log`). Repair it first: *Repair* on the notice, or
  `POST /api/v1/cars/<car>/repair` with `{"table": "drives"}`, or a rebuild or a full rebuild of the car, which
  mends it as they meet it. Repair keeps what still reads, reads the imports' kept uploads again for the imported
  rows that don't, makes the drives that come from the logs again by a rebuild from where the damage begins, and
  puts the purposes you typed back on the drives that are theirs now. The damaged files are not deleted: they are
  put aside in `db/cars/<id>/aside/<time>/` (with a note), and backups and moves carry that folder. What could not
  be recovered is said afterwards, with the time it covered, in a notice (*data_repaired*) and in
  `GET /api/v1/cars/<car>/repair`: imported rows no kept upload holds (an import whose upload was discarded, rows
  read from a local TeslaMate, rows that moved off the car to another), the changes a damaged journal record held
  (when they were is not known), and purposes with no drive. **Restore from a backup only for what that report
  lists.** The way back for those is the car's folder from a backup, as below.
- **Another table, or an import's or an API's series** (the car's charges, states and updates; samples read from an
  import or the Tesla API). They also hold what no stored log gives, so no rebuild makes them again, and none
  starts them empty: that would lose those rows without a word. The way back is the car's folder from a backup.
  This is why the data folder needs backups even though the logs are kept.

To put one car's folder back, Docker Compose, in the folder with
`docker-compose.yml`. The notice names the folder: `db/cars/<id>`.

1. Stop Sentry Mate and put the car's folder aside:

   ```bash
   docker compose stop
   ```

   ```bash
   sudo mv data/db/cars/<id> ~/sentry-mate-car-<id>.old
   ```

2. Take that folder out of the backup. Use your backup file's name in place
   of `sentry-mate-data-<date>.tar`:

   ```bash
   sudo tar -C data -xpf ~/sentry-mate-data-<date>.tar ./db/cars/<id>
   ```

3. Give it to the user Sentry Mate runs as, and start it:

   ```bash
   sudo chown -R 1000:1000 data/db/cars/<id>
   ```

   ```bash
   docker compose up -d
   ```

The logs that came since the backup are decoded again by themselves, so
drives and charges from logs come back whole. What was imported or typed in
for that car after the backup was made (an import, a cost, a purpose) is
not in it: do those again. When the notice is gone and the car's pages
open, delete the folder you put aside.

Use a backup this server made with its present keys. If the car's pages
still answer with an error, put the whole data folder back instead
([The data from a backup](#the-data-from-a-backup)). On TrueNAS, stop the
app, copy `db/cars/<id>` out of a snapshot of the data dataset over the
one in the dataset (a snapshot's files are under the dataset's
`.zfs/snapshot/<name>/`), then start the app.

## Moving to a new server

The easy way is [docs/moving.md](moving.md): a one-time move code on the old
server, or one export file that opens with the recovery key. Both work from
the web pages (Settings on the old server, the first page of the new one) or
with two lines in the new server's settings (TrueNAS: no shell). The new server copies everything while the
old one keeps taking uploads, then takes over by itself.

The steps below are the manual way: stop the old server and copy its two
folders by hand.

The certificate and the tokens are in the data folder, so the bridge keeps
working with the same token and certificate SHA-256. Change only its
address, if the server's address changed.

1. On the old server, in the folder with `docker-compose.yml`, stop
   Sentry Mate and copy what is in the two folders, each to its own file in
   your home folder. The keys file is then made yours and readable by you
   only:

   ```bash
   docker compose stop
   ```

   ```bash
   sudo tar -C data -cpf ~/sentry-mate-data.tar .
   ```

   ```bash
   sudo tar -C keys -cpf ~/sentry-mate-keys.tar .
   ```

   ```bash
   sudo chown "$(id -un)" ~/sentry-mate-data.tar ~/sentry-mate-keys.tar
   ```

   ```bash
   chmod 600 ~/sentry-mate-keys.tar
   ```

   Or take only the data: on the new server you then
   [restore](#restore) with the recovery key.

2. Copy the files into your home folder on the new server (`scp`, a USB
   stick). Carry the keys file apart from the data file (another USB stick,
   or a separate `scp`), never in the same backup. To TrueNAS: copy them
   into an SMB share on it (for example one at `/mnt/tank/share`), or turn on SSH
   (*System → Services*) for `scp`. TrueNAS has SSH off at first and does
   not mount USB sticks.

3. On the new server, do Docker Compose steps 1 and 2 (the compose file and
   the two folders), or TrueNAS step 1 (the datasets). Put the copies in,
   and give them to the user Sentry Mate runs as there, with `-R` for
   everything inside:

   ```bash
   sudo tar -C data -xpf ~/sentry-mate-data.tar
   ```

   ```bash
   sudo tar -C keys -xpf ~/sentry-mate-keys.tar
   ```

   ```bash
   sudo chown -R 1000:1000 data keys
   ```

   ```bash
   sudo chmod 700 data keys
   ```

   ```bash
   sudo ls data
   ```

   On TrueNAS the user is 568, the folders are your datasets, and the files
   are where you copied them (here the share at `/mnt/tank/share`). Delete
   them from the share right after, the keys one above all:

   ```bash
   sudo tar -C /mnt/tank/apps/sentry-mate -xpf /mnt/tank/share/sentry-mate-data.tar
   ```

   ```bash
   sudo tar -C /mnt/tank/apps/sentry-mate-keys -xpf /mnt/tank/share/sentry-mate-keys.tar
   ```

   ```bash
   sudo chown -R 568:568 /mnt/tank/apps/sentry-mate /mnt/tank/apps/sentry-mate-keys
   ```

   ```bash
   sudo chmod 700 /mnt/tank/apps/sentry-mate /mnt/tank/apps/sentry-mate-keys
   ```

   ```bash
   sudo rm /mnt/tank/share/sentry-mate-data.tar /mnt/tank/share/sentry-mate-keys.tar
   ```

   ```bash
   sudo ls /mnt/tank/apps/sentry-mate
   ```

   The last line must show `vault.json`. If it doesn't, the data didn't go
   in: fix that before you start it.

4. Start it (`docker compose up -d`, or *Install via YAML*), and check that
   `fingerprint` prints the same SHA-256 as the CAN Bridge app has, and
   `token list` shows your tokens. Then delete the `.tar` files on both
   servers (`rm ~/sentry-mate-data.tar ~/sentry-mate-keys.tar`), and leave
   the old server stopped. If the server's address changed, change the
   app's upload address and press **Apply & restart**.

## Moving the folders on this server

To put the keys on a second disk after the install, or the data on a bigger
one. Copy first, then change the volume line: pointed at an empty folder,
Sentry Mate finds nothing to open and stops.

1. Note what `fingerprint` prints. Then stop Sentry Mate, and copy the
   folder into a new one (here the keys, to `/mnt/disk2/sentry-mate-keys`;
   the data the same way, from `data/`):

   ```bash
   docker exec sentry-mate /sentry-mate fingerprint
   ```

   ```bash
   docker compose stop
   ```

   ```bash
   sudo mkdir -p /mnt/disk2/sentry-mate-keys
   ```

   ```bash
   sudo cp -a keys/. /mnt/disk2/sentry-mate-keys/
   ```

   ```bash
   sudo chown -R 1000:1000 /mnt/disk2/sentry-mate-keys
   ```

   ```bash
   sudo chmod 700 /mnt/disk2/sentry-mate-keys
   ```

2. In `docker-compose.yml`, put the new path in that volume line, for
   example `- /mnt/disk2/sentry-mate-keys:/keys`. Then start it:

   ```bash
   docker compose up -d
   ```

   ```bash
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
| 10101/tcp | The web UI, HTTPS, TLS 1.3 only, the same certificate | Published: open it in a browser. |

- Outgoing: none unless you turn one on. With a Tesla account, HTTPS (443)
  to Tesla's hosts ([Add a Tesla account](#add-a-tesla-account)); with
  MQTT, the broker's port.
- Sentry Six Manager uses 9000 and 10000, so both run on one server
  ([Running next to TeslaMate and Sentry Six Manager](cars-and-bridge.md#running-next-to-teslamate-and-sentry-six-manager)).
- Another app already on 10101 or 10100: change only the left number (for example
  `"10200:10100"`) and use it in the bridge's address. The address that
  `token new` prints shows the port inside the container (10100 unless
  `SENTRY_MATE_UPLOAD_PORT` is set), not the host port.
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
- The web UI tells the home network from outside by the connection's
  address. On a server with a public IPv6 address, Docker passes a connection
  to that address on from its own bridge address (172.x.0.1), which looks like
  the home network: the "two-step sign-in from outside" rule then doesn't hold
  for visitors over IPv6. Keep 10101 closed to IPv6 from the internet, or turn
  IPv6 on for the compose network ([security](security.md#security)).

## Settings

The compose and TrueNAS files set everything. The environment variables it
reads, and what each does, are in [Settings](commands.md#settings). Times in
the log are UTC. `TZ` (an IANA zone such as `America/Chicago`) only sets the
zone new cars start in until one is saved, at set-up from the first admin's
browser or in **Settings → Cars**.

## Offline map

A map of your own area, kept in the data folder, gives every map streets, water, parks and buildings with no tile
request leaving the server. Sentry Mate never downloads one itself: you make it once on your own computer and copy
it in.

1. Get the free `pmtiles` tool from Protomaps ([go-pmtiles releases](https://github.com/protomaps/go-pmtiles/releases):
   one file for Windows, macOS or Linux).
2. Cut your area out of a Protomaps world build (pick a date listed on
   [build.protomaps.com](https://build.protomaps.com/)), giving the box as west, south, east, north in degrees:

   ```bash
   pmtiles extract https://build.protomaps.com/20261101.pmtiles my-area.pmtiles \
     --bbox=-122.6,37.6,-122.3,37.9 --maxzoom=15
   ```

   Only the tiles inside the box are downloaded, by your computer. A city is tens of MB, a country a few GB;
   `--maxzoom=14` makes it smaller, with less detail close in.
3. Copy `my-area.pmtiles` into the `maps` folder of the data folder (`./data/maps/` with the compose file; make the
   folder if it is not there, owned by the same user as the rest of the data folder).
4. In **Settings → Maps and addresses**, Map tiles is **Offline** (the default). The "Offline map file" row names the
   file in use at once; no restart. With several files the largest is used, or the one you pick there.

A Protomaps file is a vector map: the pages draw land, water, parks, roads and buildings in a plain style, without
names (Sentry Mate serves no fonts). A raster `.pmtiles` file (map pictures) is shown as made, names included. The
file goes with a direct move to a new server; an export file leaves it out (copy it again after a restore). See [Offline map file](security.md#offline-map-file).

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
| `Sentry Manager's certificate doesn't match the SHA-256 entered here` | Copy the SHA-256 again (`fingerprint`). It changes only when the data folder is replaced by a new install's, when a new certificate was made (*Settings → Server and access*, which shows the SHA-256 to copy), or when `data/tls/cert.pem` was deleted or replaced ([Changing the certificate](commands.md#changing-the-certificate)). |
| `Sentry Manager refused the token` | A typo, or the token was revoked: make a new one. |
| `Sentry Manager answered 403 wrong_car` | That token is tied to another car: one token per car. |
| `Sentry Manager doesn't know this address` | The address must end in `/api/can/logs`. |
