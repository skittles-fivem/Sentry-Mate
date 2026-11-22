# Cars and the CAN Bridge

[← Back to the README](../README.md)

- [Supported cars](#supported-cars)
- [The CAN Bridge app](#the-can-bridge-app)
- [Check it works](#check-it-works)
- [How the bridge uploads](#how-the-bridge-uploads)
- [Running next to TeslaMate and Sentry Six Manager](#running-next-to-teslamate-and-sentry-six-manager)
- [Bringing in TeslaFi's history](#bringing-in-teslafis-history)
- [Bringing in the logs Sentry Six Manager already has](#bringing-in-the-logs-sentry-six-manager-already-has)

## Supported cars

| Car (VIN 4th character) | Maker codes (VIN 1-3) | Plants (VIN 11th) | Model years in the tables | DBC set | Checked on a real car |
|---|---|---|---|---|---|
| Model 3 (`3`) | `5YJ` (US), `LRW` (Shanghai) | `F` Fremont, `C` Shanghai | 2017-2027 | `tesla-3y` | no |
| Model Y (`Y`) | `5YJ`, `7SA` (US, from 2022), `LRW` (Shanghai), `XP7` (Berlin) | `F` Fremont, `A` Austin, `C` Shanghai, `B` Berlin | 2020-2027 | `tesla-3y` | Model Y |
| Model S and Model X after the 2021 refresh (VIN 8th `5` or `6`) | `5YJ`, `7SA` (X from 2022) | `F` Fremont | 2021-2026 | `tesla-3y` | no |
| Model S and Model X before the refresh (VIN 8th `1`-`4`; 2012-13 S: `C`, `G`, `N`, `P`) | `5YJ` | `F` Fremont (`P` Palo Alto, 2012 S) | S 2012-2021, X 2016-2021 | `tesla-sx-legacy` | no |
| Cybertruck (`C`) | `7G2` | `A` Austin | 2024-2027 | `tesla-3y` (not checked) | no |
| Semi (`T`), Cybercab (`A`) | `7G2`, `5YJ` | `N` Reno, `A` Austin | 2023-2027, 2026-2027 | none: shown from the VIN only | no |

Today only `log decode` decodes ([Logs](commands.md#logs)). It picks the set from the car's VIN (the log's own
frames, its sidecar, or the car key's newest logs), else `tesla-3y`. Model year 2021 had both kinds of Model S and
X, so the VIN's 8th character decides, not the year (a car that sends its VIN in 0x508 is a legacy one). `--model`
picks one by hand. The decoded values are the same as Sentry Six Manager's, the Screenmate's GPS included. The DBC
files and their sources are listed in [crates/dbc/files/README.md](../crates/dbc/files/README.md).

### What the server knows of each car

`/cars/{car}` has a `vehicle` part ([ui-api.md](ui-api.md#vehicle-info)): model, model year, plant, body, restraint
set, battery type and chemistry, pack energy, motors and drive, performance, trim, EPA range, wheels, paint, roof,
steering side and map region, each with where it came from and how sure it is. It is worked out offline from built-in
tables, nothing is looked up:

- **The VIN's first 11 characters** (the server never keeps the last six, the serial number), read against every
  VIN decoder Tesla filed with NHTSA (49 CFR Part 565, 2009-2026) and Tesla's service-manual VIN pages (the only
  source for Shanghai and Berlin cars). For cars built in Shanghai and Berlin the 10th character is the calendar year
  built, not a model year (`year_kind: "build_year"`).
- **The car's own configuration on its CAN bus**: the gateway's `0x7FF` pages (model, drive, performance package,
  pack code, wheels and tyres, paint, roof, spoiler, steering side, map region, Supercharging access) and the pack's
  energy. These win over the VIN. (The pack's brick count is not used for the chemistry until its rules are
  checked on real logs; when they are, it comes after the VIN.) The CAN configuration does not tell a base car's
  trim (an RWD from a Long Range RWD, the 2022-24 Austin "Model Y AWD" from the Long Range AWD): the VIN's
  candidates then stay listed.
- **The EPA's rated ranges** (every Tesla row of the EPA's dataset) for cars built in the US, and typical pack
  energies when new.
- **The Tesla API's `vehicle_config`** and a **TeslaMate import's car row**, labelled as such.
- **What an admin types** (`PATCH /cars/{car}` with `config`), which beats everything.

What the tables don't know is left unknown and listed (`unknown_codes`), never guessed. A US-built car's VIN never
tells its chemistry (Tesla files `E` for every pack, LFP included); only Shanghai and Berlin VINs do. For a US car
the chemistry shown is only the usual pack for its model, year and drive, at low confidence. A VIN whose maker
code is not Tesla's gives no model. Where the car's
CAN and its VIN disagree, both are listed (`disagreements`). The tables are in
[crates/model/data/vehicle/](../crates/model/data/vehicle/), one source note per row.

## The CAN Bridge app

You need the Screenmate CAN Bridge app, build 149 or later (it has the **Second Sentry Six Manager (optional)**
section), able to reach this server. The build number is at the top of the app. Update it in the app's **Updates**
section.

First make an upload token on the server ([Tokens](commands.md#tokens)). It prints the three fields for the app.

`<server>` in the address below is the server's name or LAN address, not the `0.0.0.0` in the log. This shows
its addresses:

```bash
hostname -I
```

Give the server a fixed address in your router, so uploads don't stop when it changes.

On the Screenmate, open the CAN Bridge app. Under **Second Sentry Six Manager (optional)**, fill in what
`token new` printed:

| App field | What to put in |
|---|---|
| Upload address (empty = off) | `https://<server>:10100/api/can/logs`, for example `https://192.168.1.20:10100/api/can/logs`. |
| Token | The line that starts with `smt_`. |
| Certificate SHA-256 | The 64 hex digits that `token new` printed. |

Then press **Apply & restart** at the top of the app. The app saves the fields only then.

- `token new` prints the port inside the container (10100 unless `SENTRY_MATE_UPLOAD_PORT` is set). If you map
  another host port (for example `"10200:10100"`), use the host port.
- The certificate names only `localhost` unless `SENTRY_MATE_NAMES` was set when it was made, but the app pins the
  SHA-256 and does not check names.
- No Sentry Six Manager? Sentry Mate speaks the same upload API, so the app's first section, **Sentry Six Manager
  (CAN log upload)**, should work the same way. This is not tested yet.
- The bridge's messages say "Sentry Manager" in both sections.

## Check it works

First check that the server can be reached from the car's Wi-Fi: on a phone or laptop on that network, open
`https://<server>:10100/api/can/logs/ping` and accept the certificate warning. An answer with `bad_token` means the
address, the port and the firewall are right (the browser has no token). The upload address itself, without
`/ping`, answers `Nothing here.`, and that is normal too.

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

What the bridge's messages mean, and what to do: [When it does not start](install.md#when-it-does-not-start), the
second table.

## How the bridge uploads

- The bridge sends every finished log to Sentry Mate as well as to Sentry Six Manager, each on its own.
- **At once**: the bridge may upload up to 16 logs at once from one address, each on its own connection, in pieces
  up to 16 MiB. Sentry Mate takes 32 connections from a car's address (one that showed a valid token in the last 30
  minutes) and 6 from any other, so 16 at once fits with room for keep-alive. Before the bridge's first request with
  its token (after a start, or 30 minutes without one), its address gets 6 like a stranger's: start with a ping.
- **Over a limit**, Sentry Mate answers `503` with the code `too_many_connections` and `Retry-After: 5` as soon as
  the request's headers are in, without reading its body. The bridge should wait that long and send fewer at once.
  With `Expect: 100-continue` on each upload the bridge gets that answer before it sends any of the piece. Without
  it, Sentry Mate reads what keeps coming for 2 s after the answer and then closes the connection, so a large piece
  sent before the bridge reads the answer is cut a few seconds in (`connection closed while sending ... at X of N
  KB`, quickly, and with little sent for nothing). The same line `at 0 of N KB` came from older Sentry Mate
  versions, which closed connections over 6 per address without an answer. Each upload stops counting against the
  6 as soon as its token is checked, before its body arrives, so connections without a token can't cut an upload in
  flight. Ports and limits: [commands.md](commands.md#ports-and-limits).
- **Upload only on Wi-Fi**, in the app's **Sentry Six Manager (CAN log upload)** section, holds uploads to Sentry
  Mate too.
- Sentry Mate accepts both log formats, so the bridge keeps writing the smaller columns format. It writes that
  format only while both servers accept it.
- While a server can't be reached, logs wait on the Screenmate up to the app's **Keep up to (GB)** (default 15).
  Then the oldest are removed, first those both servers have. So if Sentry Mate is down for long, it can miss logs.
- The CAN Bridge's Raspberry Pi logger uploads the same way. It has one upload address, so it sends either to
  Sentry Six Manager or to Sentry Mate. It needs a token of its own: its car key is not the Screenmate's.
- A reinstalled bridge app or a new Screenmate has a new car key, and needs a new token
  ([When a log fails its check](security.md#when-a-log-fails-its-check)).

## Running next to TeslaMate and Sentry Six Manager

- **Sentry Six Manager** uses ports 9000 (web) and 10000 (CAN uploads). Sentry Mate uses 10100, and 10101 for its web
  UI, so both run on one server. Sentry Six Manager needs no change: the bridge sends to each on its
  own.
- **TeslaMate**: Sentry Mate doesn't talk to TeslaMate, its database, Tesla or the car. Keep TeslaMate running as
  long as you like. Its history comes in from TeslaMate's own backup file: Settings → Import in the web UI.
- **TeslaFi**: the same for TeslaFi's history, from the data TeslaFi lets you download
  ([Bringing in TeslaFi's history](#bringing-in-teslafis-history)). Sentry Mate never contacts TeslaFi.

## Bringing in TeslaFi's history

TeslaFi keeps a poll of the car about once a minute. Its data download has them all, one file per month:

1. **In TeslaFi**, open **Settings → Account → Advanced → Download TeslaFi Data**, pick a month and a year and press
   Submit. TeslaFi gives one month at a time (`TeslaFi82019.csv` is August 2019). Do it for every month you want;
   you can put all the files in one `.zip` (any order, any names), or upload them one at a time.
2. **In Sentry Mate**, open **Settings → Import** as an admin and choose the `.csv` (or the `.zip`, or a file gzipped).
   It uploads in chunks over HTTPS and can stop and go on.
3. **The check** reads it all and stores nothing. Its report says how many rows it read and would use, and why it
   would pass any over, the drives and charges it found, the date range, the car (matched to a car here by its VIN,
   or a new one) and the units and time zone it read the file in. TeslaFi writes its times without a time zone:
   check that the report's time zone is the one your TeslaFi account is set to (TeslaFi's Settings → Account), and
   say otherwise when you start. The units are worked out from the file (miles or km, °F or °C); you can say
   otherwise too. The check reads the times in the car's time zone; a zone you name at Start is used for the import
   itself (its report says which zone it used). A `.zip` may hold other files too (a readme, folders): they are
   passed over.
4. **Start the import.** Drives come from the gear, charges from the charging state, sleep from the car's state and
   software updates from the version, all marked "Imported from TeslaFi". Where they meet SMO's or the Tesla API's
   history, the live side is shown, as with a TeslaMate import; where a TeslaMate import covers the same days, only
   one of the two imports is shown ([Which history is shown](#which-history-is-shown)).

Importing the same files again adds nothing; a later month, or the rest of a month downloaded before it ended, adds
only what is new, and a drive over the end of a month becomes one drive once both months are in. **Imported in the
wrong time zone or units?** Upload the same files again and name the right ones at Start: each month imported before
in another zone or other units is read again, and what it stored is replaced, never doubled (the report's
`read_again` counts them, with a warning). Upload all the months at once (the whole download) so a drive or charge
over the end of a month is rebuilt whole on both sides. TeslaFi's own
drive and charge lists (Drives → Drive Search And Download) are refused: they have no positions. TeslaFi's costs,
tags and notes are not in its monthly files, so they don't come over.

## Which history is shown

A car can have history of the same time from several places: SMO (the CAN Bridge's logs), the Tesla API, a TeslaMate
import and a TeslaFi import. Every list, detail, total, chart, the timeline and the statistics show each drive and
charge once, chosen in this order:

1. **SMO.** Where SMO's logs cover a time, its data is used.
2. **The Tesla API.** It fills what SMO doesn't have (a value SMO doesn't send, or a time SMO wasn't recording).
   Together they are the *live* history.
3. **One import per span.** Imported history is shown where there is no live history. Where a TeslaMate import and a
   TeslaFi import cover the same days, only one of them is shown there, never both: at first the one with more data
   in that span (more drives and charges; if they have the same, more positions; if those are equal too,
   TeslaMate's).

**The switches** (Settings → Cars → overlaps, admins only, each change in the audit log):

- A span where live and imported history meet shows the live side at first; flip it to show the imported side
  there instead.
- A span both imports cover shows the import picked above; flip it to show the other import there. Inside live
  history this choice decides which import shows once that span is flipped to the imported side.

Asking for one source only (a page's source filter: SMO, the Tesla API, TeslaMate or TeslaFi) always shows that
source's rows alone, whatever the switches say; "imported" shows the chosen import in each span.

## Bringing in the logs Sentry Six Manager already has

The bridge sends Sentry Mate only the logs it finishes from the day its second server is set up. The logs Sentry
Six Manager received before that come over in one file, and become drives, charges and history here as if the
bridge had uploaded them:

1. **In Sentry Six Manager** (an admin account), open the **Nerd panel** and choose **Logs + ids.csv (.zip, any
   range)**: the CAN export bundle (`GET /api/can/export/bundle`). Pick the car's CAN Bridge **car key**, and for all
   time set **From** to a day before its first log and **To** to now. Leave "as candump text" off: the import uses the signed `.log.xz`
   files and only counts the text copies. One zip per car key; it can be any size (past 4 GB it is a Zip64 file,
   which is read the same).
2. **In Sentry Mate**, open **Settings → Import** as an admin and choose that `.zip` as it was downloaded (don't
   unpack or zip it again). It uploads in chunks over HTTPS and can stop and go on.
3. **The check** reads the whole zip and stores nothing. Its report says how many logs it would import, how many are
   here already (the bridge uploaded them to both servers) and how many it would refuse and why, per car key, and
   which car each key goes to: its car, a new car, the only car here, or waiting for an admin.
4. **Start the import.** Each log is checked as an upload is (the signature, the signing key paired with the car key,
   the hash chain), stored and decoded into the car's history. A key a car has goes to that car; a new key goes
   where a new key's first upload would (Settings → Cars). The rows are the car's own CAN data, not "imported"
   rows.

What it never does: store a log that is not signed by the car's CAN Bridge, or changed since it was signed (those
are refused and counted, with why), or store a log twice (a log already here, by name and SHA-256, is skipped).
Stopping it part way loses nothing: start it again, or import the same zip again later, and what is here is skipped
(a log it was storing when it stopped goes on from where it stopped).
A gap or fork in a key's chain shows on Settings → Cars as it does for uploads.

Once a key's logs are imported, the bridge's upload token for it must be made on its car: on Settings → Cars,
that car's upload tokens (or `token new --car NAME --key <car key>`). A token made for a new car may not take a car
key with logs here (car keys are not secret), so its first upload is refused with `wrong_car`. The import's report
says so for each key no token is tied to yet. The details:
[`docs/ui-api.md`](ui-api.md#sentry-six-managers-can-export).
