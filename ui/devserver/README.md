# Dev API server

A stand-in for the Sentry Mate server while the UI packs are built: it serves a pack's `web/` folder and
the `/api/v1` of [`docs/ui-api.md`](../../docs/ui-api.md) on made-up data. Python 3 standard library only, no
outside network. All data is invented: no real places of anyone's home or car, no real VINs, names or keys.

## Run it

```bash
python3 ui/devserver/serve.py --pack ui/packs/instrument --port 0
```

It prints exactly one line when it is ready, `listening on http://127.0.0.1:PORT/`. Open that address.
Any path that is not under `/api/` and is not a file of the pack gets the pack's `index.html` (client-side
routes such as `/cars/c1/drives`). `GET /health` answers 200 with no body.

| Flag | Default | What it does |
|---|---|---|
| `--pack DIR` | (required) | The UI pack; its `web/` folder is served at `/`. The built-in one: `ui/packs/instrument` (the archived packs of `ui/archived` can be given as a folder, for a look). |
| `--host` | `127.0.0.1` | Address to listen on. |
| `--port` | `8000` | `0` picks a free port (read it from the printed line). |
| `--cars N` | `3` | Fleet size. 1 to 3 are the featured cars; above 3 adds light synthetic cars `c4`..`cN`; `0` is an empty fleet. A large fleet's sort, search and attention indexes are built before the server listens (about 1.5 s for 200000 cars). |
| `--drives N` | `0` | Gives `c1` about `N` drives in all (short errands from home added on most evenings; about 2,200 of them this year with `--drives 3000`), to try long lists (Logbook, Drives, Charges). `0`: the usual history, about 1,150. |
| `--latency-ms N` | `0` | Waits this long before every `/api` answer (not the event stream). |
| `--seed N` | `1` | Same seed and flags give the same data. |
| `--signed-out` | off | Starts with no session, to test the sign-in page. |
| `--role admin\|watch` | `admin` | Who the automatic session is: `watch` sees only car `c1` and may not change car data. |
| `--map-tiles online\|none\|offline` | `online` | How maps start: `online` (OpenStreetMap; the page checker answers the tiles), `none` (a new server: offline tiles with no map file, so every map shows its "No map tiles yet" note) or `offline` (a made-up raster offline map file, `bay-area.pmtiles`, served at `/api/v1/maps/tiles/{z}/{x}/{y}` as plain tiles with a grid line). |
| `--now-ms MS` | `1794850200000` | The virtual "now" at start (2026-11-16 17:30 UTC). It then moves with real time. |
| `--fresh` (or `--needs-setup`) | off | Starts as a fresh server: no accounts and nobody signed in, the first-run set-up page. The set-up code is `SETUP-DEV-0000`. See "First-run set-up and moving" below. |
| `--move-state S` | `none` | Starts Settings: Server's "Move to another server" in state `S`: `offered`, `expired`, `copying`, `paused`, `moved` or `done` (screenshots; add `--freeze-clock` to hold it there). Not with `--fresh`. |
| `--export P` | `quick` | How a new export file is made: `quick` (3 s), `slow` (18 GB in 2 min), `stall` (stops at 40 %, `stalled` a minute later) or `fail` (problem `no_space` at 55 %). |
| `--export-state S` | `none` | An export file there at start: `preparing` (slow, a third collected), `stalled`, `failed` or `ready` (its SHA-256 worked out). Screenshots: add `--freeze-clock`. Not with `--fresh`. |
| `--own-certificate` | off | Runs with the owner's own certificate (`certificate.own` in `/network`), expiring in 12 days: the `certificate_expiring` notice is up and `POST /network/certificate` is `409 own_certificate`. Without it the certificate is one the server made, which never expires soon. |
| `--release-check off\|newer\|up_to_date\|error\|never\|checking` | `off` | The release check at start: off, or on with that result of the last check (`GET /api/v1/release`): a newer build (`9c1e7d3`, 4 changes, with the admins' `release_available` notice), up to date, GitHub's rate limit, not checked yet, or a check in flight. Turned on later in a session it finds the newer build. `/about` carries a made-up `build` (`0.1.0 · build 2f44a52`). |
| `--teslamate-trouble none\|connection_refused\|auth_failed\|pin_mismatch\|quiet\|catchup_failed` | `none` | The local TeslaMate starts in that trouble with its admins' notice (`teslamate_connection_refused`, `teslamate_auth_failed`, `teslamate_pin_mismatch`, `teslamate_quiet`, `teslamate_catchup_failed`; the last turns the database catch-up on), linking to Settings: Integrations (#teslamate). In tests, `resources.teslamate.fail(store, part, code, now)` starts or ends one (`quiet` for a broker that stays connected and sends nothing). |
| `--full-rebuild none\|running\|failed\|offered\|planned\|paused` | `none` | The full rebuild the server starts with (`/cars/{car}/full-rebuild`, `/system/full-rebuild`): one of every car under way (a long one, a few minutes in, with a log it could not decode), one that failed on the second car (no space left; the old history kept), the first car's history made with an older decoding and `full_rebuild.auto` off (the `full_rebuild_offered` notice), a run paused part way after an earlier full rebuild was cancelled (`paused`), or a new version's full rebuild of every car planned and waiting its ten-minute grace (`hold.kind: "grace"`, the `full_rebuild_planned` notice; PATCH `{ when }` starts it now, puts it off until tonight or pauses it). Every status says why it runs (`causes`) and has the last rebuilds (`history`: three made-up ones from before the start). One started from a page runs about three minutes a car on the virtual clock and ends done. |
| `--gps-fit none\|disagree\|running\|done\|history` | `none` | Every featured car's Screenmate positions are out of step with its Tesla API's (`/cars/{car}/health` says `gps_inputs_disagree`, with `gps_disagreement`), so Data health shows admins **Find GPS layout** next to the notice; and the car's GPS layout fit (`/cars/{car}/gps-layout/fit`): not run (`disagree`), under way in the logs and never ending (`running`), or done with a made-up layout found (`done`), or four finished runs (a far-off, a weak and two good ones, one applied by `admin`) with the layouts the car was read with over time (`history`). One started from a page runs 8 s on the virtual clock and ends done; the result holds layouts, counts and metres only. |
| `--move-peer-after-s N` | `15` | A made-up new server (`192.0.2.30`) claims a waiting move code `N` seconds after it is made, then copies, pauses, switches and is done in about 17 s. `0`: never. |
| `--freeze-clock` | off | Keeps "now" fixed (stable screenshots and tests). |
| `--sse-interval-ms N` | `2000` | How often the cars' `now` values are pushed on `/api/v1/events` (the live figures `job`, `health`, `connections` and `storage` are looked at every 0.25 s; see "Live figures on the event stream"). |
| `--rows-every-s N` | `0` | Also sends every featured car a `rows` event every `N` seconds (all row kinds), as if its rows had changed, so the pages load their lists again (`ui/tools/check.js --stable` uses it). `0`: `rows` only when a drive, charge or parked spell really starts or ends. |
| `--rebuild-running` | off | Every featured car's history is being rebuilt the whole time: `/cars/{car}/health` says `rebuild_pending` (so Data health reads itself again every few seconds) and the car gets a `rows` event every second (every `--rows-every-s` when that is given), as while a long rebuild rewrites its rows. `ui/tools/scroll.js` scrolls the pages in this state. |
| `--health-list-max N` | `500` | How many (the newest) of a car's chain gaps, dropped frames and unknown spans `/cars/{car}/health` lists; the `*_total` beside each list counts them all. The real server lists 500; a small `N` shows a page what a car with more than that looks like (`--health-list-max 3`: car `c1` has 3 of its dropped frames listed and all of them counted). |
| `--verbose` | off | Logs every request to stderr. |

### UI packs

`GET /api/v1/ui-packs` lists the built-in packs of `ui/packs` (Instrument alone: the others are archived in
`ui/archived`; plus the `--pack` folder when it is another one), as the real server does. The `--pack` one is
the pack chosen for everyone. A person's `ui_pack` preference picks the pack their browser is served from the next
page load on, as on the real server; `null` goes back to the `--pack` one, and an unknown id is `422`.

Every answer names the build of the interface in `X-Sentry-Mate-Build`, as the real server does. `POST
/__dev/new-build` (the dev server only) stands for a server update: from then on every answer names a new build, so a
tab open across it loads its next page in full (`ui/tools/server-update.test.js`). A file under a build's address
(`/_v/<build>/core/app.js`) is the file of that path now, as the real server answers a tab loaded before an update.

### Signing in

Unless `--signed-out` is given, the first request without a session cookie signs in the default account
(admin, or watch with `--role watch`) and sets the cookie, so a page load or a plain `curl` just works. After a
sign-out the browser stays signed out. Accounts, all with the password `sentry-mate-dev`:

| Name | Role | Cars | Notes |
|---|---|---|---|
| `admin` | admin | all | the default session |
| `watch` | watch | `c1` | prefers miles, °F and psi |
| `alex` | admin | all | two-step sign-in; the code is always `000000` |

The cookie is `sentry-mate=<token>; Path=/; HttpOnly; SameSite=Strict`. It is **not** `Secure`, because the dev
server is plain HTTP on 127.0.0.1. The real server sends `__Host-sentry-mate` with `Secure` over HTTPS only.

Every change (POST, PUT, PATCH, DELETE) needs the header `X-Sentry-Mate: 1`, else `403 csrf`.

### Headers

Every answer carries a Content Security Policy like the real server's (pages may load tiles only from the online
tile server, named only while online tiles are on), `X-Content-Type-Options: nosniff` and
`Referrer-Policy: no-referrer` (pages: `strict-origin` while online tiles are on, as the real server, so
OpenStreetMap's tile server sees a Referer). `/api/` answers carry `Cache-Control: no-store`, but for a
successful `GET` with a JSON body, as the real server does (`crates/web/src/delivery.rs`): a weak `ETag` (a hash
of the body), `Cache-Control: private, no-cache`, and `304` with no body when `If-None-Match` names the tag. Paths
that may carry a secret (tokens, keys, set-up, `/me`, sessions, two-step, exports, the move) stay `no-store`.

## The made-up dataset

Built once at start (about a second) from the seed and "now". History runs to two days past "now": pieces that
start after the virtual clock are not shown yet, and the piece the clock is in is shown in progress.

| Car | What it is | Story |
|---|---|---|
| `c1` Comet | Model Y 2025, Long Range AWD, NCA pack, `America/Los_Angeles` | Home in Noe Valley, work in Mission Bay (San Francisco). Weekday commutes, weekend outings, a road trip to Los Angeles in July with six Supercharger stops, a nine-day holiday parked at home (vampire drain), Sentry at work, scheduled home charging at 23:30 (1 phase, 32 A). **Driving at "now"** (home to Palo Alto). A drive and a charge that stopped mid-way, and a gap of every kind. Three software updates found from fingerprints, one of each kind of name: `2026.2.6` typed by the owner (`named_by: "owner"`); one nobody has named that changed every module (on a real car its fingerprints flap for minutes after such an install, and the server keeps one update with the final set: so does the mock, never several same-version updates minutes apart); and the newest, `2026.32.5`, named from TeslaFi (`version_source: "teslafi_api"`, `named_by: "auto"`), its current version. Its gaps have each of the common reasons (`no_log`, `not_recorded`, `failed_check`, `dropped_frames`). |
| `c2` Maple | Model 3 2022, RWD, LFP pack, `America/Chicago` | Home in Logan Square, work garage in River North (Chicago). Cold winters with reduced range days, home charging on 3 phases, public AC at work, a road trip to Toronto in August (DC in Michigan and in Ontario, costs in CAD). A slow leak in the rear left tyre for the last three weeks. **Charging at "now"** at work. Its newest software update (found from fingerprints, 17 days ago) is named `2026.44.1` from the owner's local TeslaMate (`version_source: "teslamate_local"`, `named_by: "auto"`), so that is its current version; once local TeslaMate's car 1 is matched to Maple, `/now` also reads it live there. One of its gaps is a stored log that could not be decoded (`unreadable`). |
| `c3` Atlas | Model S 2017, 90D, old (legacy) CAN profile, `America/Los_Angeles` | East Bay. 16 months imported from TeslaMate (`source: "teslamate"`, estimated energy) that overlap 5 days of CAN rows, and a week of that history imported from TeslaFi as well (TeslaFi's copies of the same drives and charges, `source: "teslafi"`; with no live data there, one import is shown: TeslaMate's at first, flipped in `/overlaps`), free Supercharging, fewer decoded signals (no tyres, no set points). Its CAN Bridge went quiet about 55 hours before "now": the last log failed its check. Its current version `2025.45.8` is read live from TeslaFi (`current_version_source: "teslafi_api"`); TeslaFi first said it eight days after the update, too late to name it, so that update stays `build_match`. One of its gaps is a log that names another car's VIN (`other_vin`). |
| `c4`..`cN` | synthetic | Names, models, states and battery from the car's index only; lists elsewhere are empty but valid. |

Where each row came from (`sources`, `source_label`, see `mockapi/sources.py`): `c1` has SMO and the Tesla API
(most rows SMO, a fifth seen by both, a few by the API alone, and an afternoon two days before "now" when SMO was
offline and only the API saw the car); `c2` has both on every live row; `c3` has SMO and its TeslaMate import; the
synthetic cars have SMO. Only the labels differ: the made-up data is the same.

Also present: places that overlap (Work and the Mission Bay Supercharger, Home and Noe Valley), charges with
and without cost, two software changes per car (one known only as "software changed"), awake, asleep and
vehicle-bus-only spells, attention items, upload tokens, a CAN Bridge key waiting to be assigned, an audit log
(about 118 made-up entries over 400 days by five people and the server, every code of `ui/shared/audit-codes.json`
and one code no list knows, at 192.0.2.x addresses; `mockapi/audit.py`),
saved trips and purpose rules, 40 places (some with prices by time of day), server and MQTT settings, notices (an
unsaved recovery key; with `--own-certificate` also a certificate that expires in 12 days), a Tesla account (`ta1`, reading `c1` and `c2`, a third
car not matched; every change asks for the password `sentry-mate-dev` again), the finished import that brought `c3`'s TeslaMate
history in (its report counts exactly what `c3` holds), and storage figures that add up from the cars to the server.
Seeds that are not car history are in `mockapi/dataset/extras.py`.

Everything agrees across pages because it is all worked out from the same pieces and rows: a month in Statistics
has the same drives, energy and cost as the Drives and Charges lists for that month; battery capacity readings come
from the charges the Charges page lists; a renamed place shows at once in drives, charges, the timeline, stops,
Travel, `now` and the MQTT preview. A charge keeps the cost it had when it ended until the owner reprices it.

Series (1 Hz while driving or charging, every 30 s parked and awake, nothing asleep) are computed on demand from
each piece's own seed and never stored, so a year of data answers in a few hundred milliseconds.

## What is built

| docs/ui-api.md section | State |
|---|---|
| Conventions (paging, period, `cars=`, errors, access, CSRF, audit, CSV, source and basis, null) | done |
| Sign-in, accounts and preferences | done |
| Fleet and cars | done |
| Live values, including `/api/v1/events` (server-sent events) | done |
| Time series | done |
| Timeline and states | done |
| Drives (track as GeoJSON and GPX, logbook as JSON, CSV and PDF) | done |
| Charges | done |
| Battery and efficiency | done |
| Reports (statistics, mileage, totals, gaps, compare, stops and Travel, tracks, software, car events) | done |
| Places (preview of a change, reprice, suggestions, the place at a point) | done |
| Integrations (MQTT settings, status, preview, test; outside connections) | done |
| Integrations: Overview (every connection with made-up hourly counters for 7 days and the week's problems; see below) | done |
| Integrations: Tesla (accounts, sign-in at Tesla with a pasted address, refresh token, cars, pause, poll setting, status; nothing leaves the dev server) | done |
| Integrations: Supercharger invoices (made-up sessions on the Supercharger charges, list and CSV, zip, PDFs, the `invoices` blocks and their PATCH; see below) | done |
| Server, settings and health (notices, maps, data health, rebuild, overlaps, storage) | done |
| Import (resumable upload, check, car matching, import, report) | done |
| Moving and first-run setup (claim, start fresh, move here, restore, move code, undo, export file) | done |
| Deliberately absent (no car commands, no wake, no Tesla token, no SQL, no VIN serial) | absent: `404` |

Decisions on points the API document left open are at the end of `docs/ui-api.md` ("Decisions").

## Integrations: Overview

`GET /api/v1/integrations/overview` (admin; `watch` gets `403`, like every `/integrations` route) lists the CAN
Bridge of each car (the sold car `c0` has none), the Tesla and TeslaFi accounts, local TeslaMate (MQTT, database),
the MQTT broker, online map tiles, address lookup and the release check, in that order, from the mock's own state
(`mockapi/resources/overview.py`). With the default flags: `c1` and `c2` live (driving, charging), `c3` in
`problem` (`quiet`: its CAN Bridge has sent nothing since the log that failed its check, about 55 h before "now";
the one open problem), TeslaFi `idle` (`parked`: it reads `c3`), the TeslaMate database, address lookup (offline)
and the release check `off`. Synthetic cars are live when driving, charging or parked, else `idle` (`car_asleep`).
`--teslamate-trouble`, `--release-check error`, a Tesla sign-in refused or TeslaFi token refused put that
connection in `problem` with an open problem since its error.

The hourly counters are worked out, never stored: a featured car's CAN Bridge sends a log a minute while the car
is awake (nothing while it sleeps), the Tesla and TeslaFi counts follow their paces and their cars, the rest
follow a day-time shape (Pacific time); the odd failed try is marked, and a try inside a problem counts as an
error. The five resolved problems of the week (`c1`'s CAN Bridge offline for the afternoon two days before, as in
its sources; Tesla, TeslaMate's broker, the MQTT broker, the tiles: 12 minutes to 6 hours) are fixed relative to
`--now-ms`; `uptime` is the share of the period outside the connection's problems. On a running clock the current
hour's count grows, `last_ok_ms` and the open problem's `duration_ms` move on, and the list keeps its order.

A large fleet lists the CAN Bridge of the first 50 cars only (`MAX_CAR_ROWS`); `totals` still counts every car
(the cars not listed from the fleet's cached state counts), so `--cars 200000` answers in about 0.15 s.

## Supercharger invoices

`mockapi/resources/invoices.py`. Every Supercharger charge of `c1` and `c2` that ended an hour or more before
`--now-ms` has a made-up Tesla session (`tesla-1000101` on, oldest first) matched to it, at a made-up site
("Made-up Supercharger, Hilltown", "Sample Supercharger, Rivermouth"; one name per place): the charging fee (billed
kWh at a made-up rate, CAD on Maple's Ontario stops), an idle fee on every fifth session, a tiny made-up PDF (every
ninth one's is still waiting: `pdf: false`, `404 no_pdf`). Two more sessions of the account's third car are not
matched. A matched charge's cost is the session's total (`cost.source: "tesla_invoice"`, `cost_breakdown.mode` the
same, one part per fee) unless an admin entered a cost (`applied: false`). Tesla invoices start on (the made-up
admin turned them on; the real default is off), TeslaFi's off and with no sessions (`gives_pdfs: false`, a
`note`). `PATCH /integrations/tesla` or `/teslafi` with `invoices` asks for the password again like `poll`; turning
them off stops the checks (`next_check_ms` null) and keeps what was fetched. The zip writes `invoices-downloaded`.

## Live figures on the event stream

Besides `now`, `rows`, `fleet`, `prefs` and `notice`, `/api/v1/events` carries the live figures of the real server
(`crates/web/src/live.rs`), worked out with the same handlers as their GET routes (`mockapi/events.py`,
`EventHub.live_round`, every 0.25 s while a stream is open):

| Event | Who gets it | `data` |
|---|---|---|
| `job` | admins | `{ kind, id, status }`: `full_rebuild` (id `""`, status = `GET /system/full-rebuild`), `compaction` (`""`, the `compaction` of `GET /system/storage/detail`), `export` (the export's id, `GET /system/exports/{id}`), `missing_scan` (`""`, `{ state, keys_done, keys_total, found, started_ms, finished_ms }`; always `idle` here), `import` (the import's id, `GET /imports/{id}`), `invoices` (`tesla` or `teslafi`, the `invoices` block of `/integrations/tesla` or `/teslafi`) |
| `health` | streams that see the car | `{ car, health }`, health = `GET /cars/{car}/health` (featured cars) |
| `connections` | admins | `GET /integrations/overview` |
| `storage` | admins | `GET /system/storage` plus `compaction` |

Each topic (a job's kind and id, a car's health, the connections, the storage) is sent when its value changed
(a top-level `read_ms` left out of the comparison), at most every 0.5 s for a job, 1 s for health and connections,
2 s for storage; a change that comes too soon goes at the next look. The first look after a stream opens (with no
other stream open) sends every topic once; a stream that opens while others are open gets only later changes, as on
the real server. With `--freeze-clock` nothing moves, so each topic is sent once.

What moves on the virtual clock:

- A full rebuild (`--full-rebuild running`, or one started from a page): `logs.done` and `bytes.done` rise together
  while `decoding` (`days.total` known from the start, `days.done` 0), then `days`, `switching`, done. `percent`
  is logs done of all while decoding, days done of all in `days`, 0 while copying, 100 while switching or done,
  null otherwise; `rate_logs_per_s` and `rate_bytes_per_s` while decoding, `rate_days_per_s` in `days`; `eta_ms`
  is logs left / logs per second while decoding, days left / days per second in `days`. `speed` while decoding:
  `rate_logs_per_s` and the windows as the slowest stage (72 %); a car's health has the same `rebuild_speed`
  (1.1 logs/s) while its `rebuild_pending`.
- Uploads (`mockapi/uploads.py`): the preferred CAN Bridge key of each featured car that is driving or charging
  uploads a made-up log every 30 s of the virtual clock (its key's and token's `last_upload_ms`), and the server's
  `free_bytes` shrinks by 34.4 kB a log: `health` and `storage` change.
- The overview's `last_ok_ms`, `next_ms` and today's counts move with the clock: `connections`.
- A compaction started with `POST /system/storage/compact` moves one notch every 2 s while a stream is open (and at
  each read of the detail, as before), five notches to done: `job` compaction and `storage`.
- An export being prepared (`--export slow`, `--export-state preparing`): `job` export.

## First-run set-up and moving

Everything runs on the virtual clock in a few seconds (frozen with `--freeze-clock`: tests move it on). Nothing is
copied: the progress is worked out from the time.

**A fresh server** (`--fresh`): `POST /api/v1/setup/claim { "setup_code": "SETUP-DEV-0000" }` first (the browser
keeps the claim in the cookie `sentry-mate-setup`), then one of:

- **Start fresh**: `POST /api/v1/setup/fresh { name, password }` makes the admin, signs it in and answers a made-up
  recovery key (a new one each time).
- **Move here**: `POST /api/v1/setup/move-here { from, code }`. The code must read (`SMMV-` and six groups of five
  with a right check: take one from `POST /api/v1/system/move` on another dev server, or
  `python3 -c "import sys; sys.path.insert(0, 'ui/devserver'); from mockapi.resources.moving import make_move_code; print(make_move_code()[0])"`).
  The first label of the address picks what the made-up old server does:

  | `from` | What happens |
  |---|---|
  | anything else, e.g. `192.0.2.20` | works: `connecting`, `copying`, `catching_up`, `finishing`, `switching`, `done` in about 12 s |
  | `127.0.0.1` or `localhost` | works, and `result.same_machine` is true |
  | `unreachable.example`, or any name ending in `.invalid` | `502 old_server_unreachable`, nothing starts |
  | `wrong.example` | `422 move_code_wrong` |
  | `expired.example` | `422 move_code_expired` |
  | `used.example` | `409 move_code_used` |
  | `newer.example` | `422 move_too_old` |
  | `away.example` | `422 move_not_home_network` |
  | `nomove.example` | `422 move_not_offered` |
  | `flaky.example` | the connection drops at 40 % for 5 s (`waiting`, problem `unreachable`), then it carries on |
  | `gone.example` | drops at 50 % and never comes back (`waiting`): try `POST /api/v1/setup/move-here/finish` |
  | `cancel.example` | the old server cancels at 60 % (`stopped`, problem `cancelled`); `POST /api/v1/setup/move-here` again with the same address and a new code carries it on from there |

- **Restore**: `POST /api/v1/setup/restore { name, size, recovery_key, header }`, then PATCH the chunks. The old
  server's recovery key is `SMRK-DEVK-EY00-0000-0000-0000-0000-0000-0000-0000-0000-0000-0000-0000`. The file must be
  one a dev server made (Settings: Server, "Download export file", or `GET /api/v1/system/exports/{export}/file`):
  any other gives `422 not_an_export`. A file whose name has `newer` in it gives `422 export_too_new`; one with
  `broken` in it fails at 60 % of the upload (`failed`, problem `export_damaged`). After the last chunk:
  `checking` (3 s), `switching` (1.5 s), `done`.

After a move or a restore the server has the made-up accounts (`admin`, `watch`, `alex`, password
`sentry-mate-dev`) and its dataset, and nobody is signed in.

**A set-up server**: `POST /api/v1/system/move { password }` (the account's password: `sentry-mate-dev`; `alex` also
sends `two_step: "000000"`) answers the move code once. After `--move-peer-after-s` the made-up new server claims it:
`copying` (10 s, round 1, then round 2), `paused` (2 s), `moved` (2 s: the server takes no changes, `409 moved`),
`done`. `POST /api/v1/system/move/undo { password, confirm: true }` takes it back. `POST /api/v1/system/exports
{ password }` prepares a 4 MB made-up export file in 3 s (counting 0.5 s, waiting 0.3 s, collecting 2.2 s, then its
SHA-256 in 2 s more); its `url` downloads it (with `Range`), and it restores on a `--fresh` dev server. `--export
slow` makes it collect 18 GB in 2 min, `--export stall` stops it at 40 % (`stalled` a minute later, for ever; cancel
it with `DELETE`), `--export fail` fails it at 55 % with problem `no_space`.

**A new certificate**: `POST /api/v1/network/certificate { password }` answers `{ names, sha256, upload_address }`
with `localhost` and the saved host names and a made-up SHA-256 (a new one each time), which `/network` shows from
then on. `409 move_under_way` while a move code waits or a copy runs, `409 moved` after a move, and
`409 own_certificate` (before the password is checked) when the server runs with the owner's own certificate:
`--own-certificate`, or the store's `cert_own` (tests). `/network` then says `certificate.own: true`, and only
then is the `certificate_expiring` notice up (a certificate the server makes is valid until 4096). `certificate.names`
starts with `localhost`, as the real server's do. A missing `password` on any request that asks for it again is
`400 bad_input`, a wrong one `403 wrong_password`.

**Storage and compaction**: `GET /api/v1/system/storage/detail` (admin) lists the made-up space on disk by
category, largest first (the cars' figures of `/system/storage` split into logs as uploaded, their sidecars, day
packs, series, rows and records, plus the server's own files), with a compaction that is `idle` at start (its last
pass ended about three hours before `--now-ms`), so a first load is always the same. `POST
/api/v1/system/storage/compact` starts a pass that "runs" for four reads of the detail (`logs` counted and half
done, `logs` done, `series`, `records`) and ends on the fifth, whatever the clock does (also with `--freeze-clock`):
most logs and sidecars as uploaded have then gone into the day packs, the series and records are smaller, and
`last_saved_bytes` is what the disk total lost. A later pass finds nothing left (`last_saved_bytes` 0). A `failed`
pass is not played; tests set it in the store (`store.compaction`).

## Layout

```
serve.py                 the entry point (flags in mockapi/config.py)
mockapi/server.py        HTTP: static files, SPA fallback, headers, cookie, dispatch, event stream
mockapi/router.py        path patterns -> handlers per method, admin and public methods
mockapi/conventions.py   the request object, query parsing with field errors, cursor paging, CSV
mockapi/errors.py        the one error shape
mockapi/store.py         everything that can change (accounts, sessions, preferences, audit, overrides)
mockapi/audit.py         the audit codes and groups (read from ui/shared/audit-codes.json) and the seeded log
mockapi/rows.py          history pieces -> Drive, Charge and timeline rows (in progress, costs, purposes)
mockapi/placing.py       places as they are now, the nearest-centre match, costs that stay put
mockapi/derive.py        figures several sections share: odometer, stops (visits), parked spells, caching
mockapi/live.py          the "now" object, the car list item, attention items
mockapi/series.py        bucketed series that keep minima and maxima
mockapi/events.py        the event hub behind /api/v1/events (now deltas, rows, and the live figures)
mockapi/uploads.py       the made-up upload cadence of the live CAN Bridges (last uploads, free space)
mockapi/dataset/         places, cars, routes, the day-by-day simulator, pieces, synthetic cars, extras
mockapi/resources/       one module per section of docs/ui-api.md (Reports is reports, travel and software;
                         moving.py is first-run set-up, the move to another server and export files;
                         tesla.py is the Tesla API input: a made-up account `ta1` with c1, c2 and one car not
                         matched; a refresh token starting `refused` or `unreachable` plays Tesla saying no or
                         being down)
tests/                   unittest; test_contract.py checks the server against docs/ui-api.md
```

### Adding a section

1. Write `mockapi/resources/<section>.py` with a `register(router)` function. `drives.py` shows the pattern:
   read filters with `req.period()`, `req.enum()`, `req.list_param()` (they answer 400 with `fields`), build
   rows from `req.history()` (the car's pieces: `h.segs`, `h.drives`, `h.charges`, `h.updates`, `h.shadow` for
   TeslaMate copies of CAN rows and TeslaFi copies of TeslaMate rows (`site = "teslafi"`; `sources.site_of(row)`),
   `h.overlaps`, `h.import_overlaps`), page with `conventions.page(req, rows, key=..., reverse=...)` (CSV then
   comes for free), and for changes validate (400 for shape, 422 for rules), change the store under
   `st.lock`, then `st.record(account, action, target, summary)` for the audit log.
2. Add the module to `MODULES` in `mockapi/resources/__init__.py`, and the resource to `docs/ui-api.md`:
   `tests/test_contract.py` fails for a route the document lacks, and for a documented write without a recipe.
3. Car-scoped routes (`{car}` in the pattern) are checked before the handler runs; `admin=("PATCH",)` makes a
   method admin-only; `public=("GET",)` needs no sign-in. Return a dict (JSON), a `conventions.Response`, or
   raise `errors.ApiError`. A `POST` that sets `"_created": True` in its dict answers 201. Keys starting with
   `_` are never sent.
4. Pieces have `values(t)` (every series field at a moment) and `activity()` (strip segments); parked spells
   have `subs` (awake, vehicle-bus-only and asleep sub-spans with reasons and power) and `soc_at(t)`; charges
   have `curve(t)` and `figures()`. The owner's choice of side in a CAN and TeslaMate overlap is
   `h.overlaps[i]["shown"]`, of the import shown where both imports cover the same days
   `h.import_overlaps[i]["shown"]` (`teslamate` or `teslafi`; `h.shown(row, source)` and `h.site_shown` apply them),
   and `h.overlap_changes` counts the changes for their `version` (the dataset fields meant to change: change them
   under `st.lock`).

## Tests

```bash
python3 -m unittest discover -s ui/devserver/tests -t ui/devserver
```

They start servers in-process on free ports (about 45 s for all). They cover:

- the conventions: paging, errors, hidden cars with the watch role, CSRF, CSV, headers, the SPA fallback;
- every section's documented fields and rules, and that figures agree across sections (statistics and the lists,
  capacity and the charges, a renamed place everywhere, storage from the cars to the server, the import report and
  `c3`'s history);
- the contract (`test_contract.py`): every resource, method and field in the tables of `docs/ui-api.md` is read from
  the document and called with ids from the API; it fails on a missing resource, a 5xx or a missing field, on a route
  the document lacks, on anything under "Deliberately absent" that answers, and on a VIN serial in any answer;
- determinism, a 200,000-car fleet paged under 200 ms a page, and the event stream (with `--rows-every-s` at its pace).
- storage in detail and compaction (`test_system.py`): categories largest first that add up, idle at start, a pass that
  runs for a few reads and moves the logs into the day packs, a second pass with nothing left, a failed pass, admin only.
- Supercharger invoices (`test_invoices.py`): the sessions, the cost they give a charge (an entered one wins), the
  PDFs, the zip, the list and its CSV, the blocks and their PATCH with the password asked again, admin only.
- the integrations overview (`test_overview.py`): its shape, admin only, problems newest first with one open, 24
  hourly entries ending at the current hour, values that move in place, the capped car rows of a large fleet.
- the live figures on the event stream (`test_live_events.py`): a running full rebuild's `job` events with rising
  `logs.done` and `bytes.done`, `days.total` known while decoding and `percent` matching the logs, one started from a
  page through `decoding`, `days` and done, at most one `job` per topic every 0.5 s, `health` and `connections`
  changing as the clock moves, a compaction sent as it moves on, each topic once with a frozen clock, and no
  admin-only event (`job`, `connections`, `storage`) on a `watch` stream.
