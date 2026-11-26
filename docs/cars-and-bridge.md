# Cars and the CAN Bridge

[← Back to the README](../README.md)

- [Supported cars](#supported-cars)
- [The CAN Bridge app](#the-can-bridge-app)
- [Check it works](#check-it-works)
- [How the bridge uploads](#how-the-bridge-uploads)
- [Running next to TeslaMate and Sentry Six Manager](#running-next-to-teslamate-and-sentry-six-manager)
- [Bringing in TeslaFi's history](#bringing-in-teslafis-history)
- [Data sources](#data-sources)
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
| Upload address (empty = off) | `https://<server>:10100/api/can/logs`, for example `https://192.0.2.20:10100/api/can/logs`. |
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
- **Name each key.** A car with two loggers (the Screenmate's app and a Raspberry Pi logger) has two car keys,
  and the uploads don't say which device sent them. An admin names each key on **Data health** (CAN Bridge keys)
  or in **Settings → Cars** (the car's keys): the pencil next to the key, then a name such as "Screenmate in the
  Model Y" or "Pi logger" (up to 60 characters; empty clears it). Until then the key shows, muted, as "CAN Bridge
  key" and its short id (`CAN Bridge key 8d41c0`). The name stays with the key when it moves to another car or
  waits for one, and through restarts, rebuilds and a move of the server; each change is in the audit log
  (`can-key-renamed`). API: [`PATCH /can-keys/{key}`](ui-api.md) with `{ "name": "..." }`.

## Did each log arrive intact

The CAN Bridge app asks the upload API which of its logs arrived (`POST /api/can/logs/status` with
`{ "names": [...] }`, up to 2000 names; the app sends up to 500). Each name gets its `state` (`complete`,
`partial`, `missing`, or `invalid` / `forbidden` for a name that is not a log name or is another car's) with
`received`, `length` and `size` (the whole file's size when known, else the bytes received; the same as `length`
once known), and a complete one also its `check`:

```json
{"files": {"k_20261122T101500Z_0042.log.xz": {"state": "complete", "received": 1834221, "length": 1834221, "size": 1834221, "check": "verified"}}}
```

| `check` | When |
|---|---|
| `verified` | The whole file's SHA-256 is the one its sidecar names, the ECDSA P-256 signature is the paired key's, and its chain link does not contradict the log before it: that log (`prev_name`, `prev_sha256`, index one less) is here and verified, or it is the first of its chain (`prev_name` `-`), or the first one here (a chain received from its middle), or the log before it failed its own check or did not come (see below). |
| `failed` | It failed its own check, with `check_reason`: `mismatch` (the file differs from what its sidecar names), `bad_signature` (a wrong signature, another key's, another car key's, or fields that can't be read), `unsigned` (no signature, or the bridge could not sign) or `chain_break` (its chain fields can't be read, or the log before it is here and verified but is not the one this log names: another SHA-256, or not the index before). |
| `pending` | Not judged yet: its sidecar hasn't come (a log is judged without it after 10 minutes), or the log before it is waiting for its own sidecar, is still arriving, or is missing but may still come (for 10 minutes after this log arrived). The app asks again next round. |

- **A break in the chain fails only the log that breaks it.** A log whose own file and signature are fine is never
  failed because of an earlier log: when the log before it failed its check (a changed file, a bad signature, not
  signed) or never came (the car no longer has it), this log is `verified` and starts a new segment of its chain.
  The break is reported once: as that earlier log's own failure, or as one gap in the chain report, on Data health
  ("chain gaps") and in the missing-log scan below. A log that came before the one it names (16 uploads at once
  finish in any order) is `pending` while that one is on its way, never `failed`. Earlier builds answered such a log
  `chain_break` once 10 minutes had passed, and the log after any failed one too: the app kept those as failed.
- **The chain is read afresh on every status call.** Nothing about the chain is stored with a log, so when the log
  before it arrives, or a failed one is sent again and verifies, the logs after it are answered as they now fit on
  the very next call. Judging logs again (a newer version's rules at the start, a full rebuild, `keys trust`) answers
  from the stored verdicts, or `pending` while a car key is held over 2 s: never a failure made up meanwhile. A log
  whose file or sidecar is gone from the disk keeps its verdict when it is judged again.
- **Failed logs judged by an older version are judged again once**, a couple of minutes after the start of a
  version with newer rules: only failed ones (verified logs are left as they are), all of a car key at once, without
  anything being sent again. A log that verifies now is decoded and its time stops being unknown time; the audit log
  gets a `can-logs-rechecked` entry on the car saying how many were checked and how many verify now.
- **Unsigned logs of a paired car** (made by an app from before it signed logs, sent after the first signed one, or
  that the bridge could not sign) are `failed` with `check_reason: unsigned`, and their data is not used. Data
  health lists each with why it is unsigned (no sidecar, made before the app signed logs, the app said it is not
  signed, the app could not sign it), and an admin can choose to use them, their password asked again: only those,
  never a log with a bad or another key's signature or a changed file ([Pairing and the owner's
  word](security.md#pairing-and-the-owners-word)). Their drives then appear; the app still sees them as failed.
- **Why logs failed**, without sharing data: `sentry-mate log-checks --car c1` prints the counts per
  `check_reason` and when the failed runs were ([Log checks](commands.md#log-checks)); Data health shows the same
  counts ("all log checks").

- A sidecar's name has its log's `check` (`pending` while its log isn't here). Partial and missing names have none.
- The answer is read from the stored verdicts (one pass over the car key's records, whatever the number of names):
  asking never judges a log or waits for one being judged (while a key is held over 2 s, its logs answer
  `pending`).
- **Resend replaces a failed log.** A failed log stays on the car, and the app's **Resend all** uploads it (and
  its sidecar) again under the same name. If the stored copy's check is `failed` (any reason), the new upload is
  taken as any upload is (in chunks, resumable); once its last byte is here and its SHA-256 matches, the bad copy is
  linked into `data/can/<car key>/quarantine/` (the first bad copy is kept there) and the new one takes its name.
  It is judged again; once it verifies its bad copy is dropped, its minutes stop being unknown time on the
  timeline, and the logs after it fit their chain again. The Data health page's "the last log failed its check"
  clears when that log's new copy verifies. The same bytes again answer as before (`201`, `complete`).
- **A log that is stored but could not be judged** (the car's trust files could not be locked or written, say a full
  disk) is answered `500 server_error`, not `201`, so the app sends it again (the CAN Bridge app treats any answer it does not know, a `500` included, as "will retry" and sends the log on its next pass). The same bytes again are stored already,
  and a stored log (or sidecar) with no verdict is judged then. A `201` always means the log has a verdict or is
  waiting for its sidecar.
- A `verified` or `pending` log is never overwritten: a different file under its name is `409 exists_different`,
  as before.
- **How many at once:** `GET /api/can/logs/ping` says `maxParallel` (16): the uploads to send at once. A car's
  address may have 32 connections open, so 16 uploads leave a spare connection each for keep-alive.
- **Logs the server lost are asked for again.** A log stays known after its file is gone: its trust record keeps
  the SHA-256 and length it was stored with (and whether it had a sidecar), and a signed log names the one before
  it. A scan, a couple of minutes after the start (in the background) and then once a day, compares those with what
  is on disk, per car key: a log (or sidecar) with a record whose file is gone, and a chain gap, where a verified
  log names a previous log the server has neither a record nor a file of while older logs of that signing key are
  here (a chain received from its middle is no gap). It reads no log, only a stat per file. What it finds is kept
  sealed in `data/can/<car key>/missing.json` with when it was found (a restart keeps it), and the audit log gets a
  `can-logs-missing` entry on the car.
- **`wanted` in the ping.** `GET /api/can/logs/ping` answers `"wanted": [names...]` for the car its token is tied
  to: the logs and sidecars the server is missing, oldest first, at most 500 (empty when none, or for a token not
  tied to a car yet). The app sends those it still holds again, as any upload. A name leaves the list once it has
  arrived again and been judged with the SHA-256 it was stored with (a sidecar: once it is back); an upload of a
  wanted name with another SHA-256 is refused, `409 wanted_mismatch`, unless the server never had its SHA-256. In
  its status call the app may add `"not_held": [names...]`: wanted names it no longer has. Those are no longer
  asked for, and Data health shows them as lost for good (their time stays unknown time on the timeline, as
  before). A name the app asks about again in a status call is asked for again. The app's side is in
  [can-bridge-wanted.md](can-bridge-wanted.md).
- **Data health** shows a "Missing on the server" card (how many, the oldest, that the car was asked to send them
  again, and those the car no longer has either), `logs_missing` among the reasons while any are asked for, and the
  connections overview's CAN Bridge row counts them.
- **Limits** (see [Ports and limits](commands.md#ports-and-limits)): 32 connections per car's address, 6
  anonymous ones from an address that is not a car's, 512 in all of which 192 anonymous. Over a limit the answer is
  `503 too_many_connections` with `Retry-After: 5`: wait that long and send fewer at once.

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

## Data sources

A car's data can come from four live inputs: SMO (the CAN Bridge's logs), the Tesla API, TeslaFi's API and a local
TeslaMate (its MQTT broker). Each is set up on its own page: a bridge key for SMO, a matched vehicle for the Tesla
API and TeslaFi (Settings → Integrations), a matched TeslaMate car for local TeslaMate. Imported history (TeslaMate
or TeslaFi) comes on top of them from the Import page.

**Priority order** (Settings → Cars → Data sources, admins only, in the audit log): the order the car's live inputs
win in. When two sources cover the same time, the higher one wins, value by value: its seconds are the car's
seconds, and a value it did not send that second comes from the next one down that has it. At first it is SMO, the
Tesla API, TeslaFi, local TeslaMate; any order of the four can be set (SMO need not be first), and "default" goes
back to it. The same order picks the car's newest values on the car page and which input's lock, windows and
update state are shown. A change rebuilds the car's history; the car shows "rebuild pending" until it is done.
Imports always come after every live input.

**Gap filling** (Settings → Cars: `import_gap_fill`): where no live input has a value at a time, the imported
history shown for that span fills it, labelled "Imported from TeslaMate" (or TeslaFi). The car follows the server's
setting (Settings → Server: `import_gap_fill`, on by default) until it sets its own.

**Overlaps:** where live and imported history, or two imports, cover the same span, which side is shown is chosen per
span (`GET /cars/{car}/overlaps`, from the Import page or the Timeline). See [Which history is
shown](#which-history-is-shown).

## When the history is wrong: a full rebuild

The server works out drives, charges, states, visits, positions and the battery figures from the stored CAN logs
and the Tesla API, TeslaFi and TeslaMate data it keeps, and redoes a part of it by itself when a late log or a new
setting changes it. When the history is wrong for another reason, a **full rebuild** makes all of it again: Data
health (one car) or Settings: Server and access (every car), "Full rebuild", with your password. Every stored log is
decoded again with the server's DBC sets and decoder, the APIs' stored data is fused in again, and every kept page
value is dropped. It is built beside the history in use (in `.full-rebuild` in the data folder, which a move or an
export never takes along; it needs about as much free disk as the car's history without its CAN samples) and each
car switches to it when it is done: until then the pages show the old history, and a failure, a cancel or a restart
before the switch leaves the old one as it was (a restart carries on with it). The stored logs, their signatures,
the APIs' data and imports are never changed, and what you entered stays: charge costs, places and prices, typed
versions and firmware names, drive purposes, car settings, overlap choices, sold status and source order. An import
is kept as the rows it left (its file is not kept on the server).

The server notes the decoding each car's history was made with. When a newer server decodes the stored logs
differently (a decoder fix, a corrected DBC signal), it rebuilds those cars fully by itself after its start, with a
notice; with Settings: Server and access "Rebuild automatically" off, the notice offers it instead. A history made
before the server noted this is rebuilt once the same way.

## Which history is shown

A car can have history of the same time from several places: SMO (the CAN Bridge's logs), the Tesla API, a TeslaMate
import and a TeslaFi import. Every list, detail, total, chart, the timeline and the statistics show each drive and
charge once, chosen in this order:

1. **The live inputs, in the car's order.** By default SMO first: where SMO's logs cover a time, its data is used
   ([Data sources](#data-sources) sets another order per car).
2. **Then the Tesla API, TeslaFi's API and local TeslaMate.** The Tesla API fills what SMO doesn't have (a value SMO
   doesn't send, or a time SMO wasn't recording), TeslaFi's API what both lack, and local TeslaMate what all three
   lack. Together they are the *live* history.
3. **One import per span.** Imported history is shown where there is no live history. Where a TeslaMate import and a
   TeslaFi import cover the same days, only one of them is shown there, never both: at first the one with more data
   in that span (more drives and charges; if they have the same, more positions; if those are equal too,
   TeslaMate's).
4. **The import fills the gaps.** Where the live history (every live input) has nothing for a value
   at a time, the import chosen for that span (by rule 3 and the switch below) fills it: a temperature, the
   elevation and climb, a position (in a chart, and a stretch of a drive's track where the live data has a gap), the odometer, battery level or range
   at a drive's or charge's ends. The drive or charge is still the live one, shown once; only its missing values
   come from the import, and each is labelled "Imported from TeslaMate" (or TeslaFi). A value live history has is
   never replaced. Distance, energy and costs are never filled, so totals and consumption don't change and a
   measured figure stays measured. Where nothing recorded a value, not even the import, it is unknown: a chart
   shows a gap there, never a line drawn across it or the last value held.

**The switches** (Settings → Cars → overlaps, admins only, each change in the audit log):

- A span where live and imported history meet shows the live side at first; flip it to show the imported side
  there instead.
- A span both imports cover shows the import picked above; flip it to show the other import there. Inside live
  history this choice decides which import shows once that span is flipped to the imported side, and which import
  fills the gaps there.
- Gap filling (rule 4) is on at first. Turn it off for the whole server (Settings → Server: `import_gap_fill`) or
  for one car (Settings → Cars: `import_gap_fill`, which wins over the server's); off, live rows and charts show
  only live values.

Asking for one source only (a page's source filter: SMO, the Tesla API, TeslaMate or TeslaFi) always shows that
source's rows alone, whatever the switches say, and nothing is filled from an import; "imported" shows the chosen
import in each span.

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
