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
| `--pack DIR` | (required) | The UI pack; its `web/` folder is served at `/`. |
| `--host` | `127.0.0.1` | Address to listen on. |
| `--port` | `8000` | `0` picks a free port (read it from the printed line). |
| `--cars N` | `3` | Fleet size. 1 to 3 are the featured cars; above 3 adds light synthetic cars `c4`..`cN`; `0` is an empty fleet. |
| `--latency-ms N` | `0` | Waits this long before every `/api` answer (not the event stream). |
| `--seed N` | `1` | Same seed and flags give the same data. |
| `--signed-out` | off | Starts with no session, to test the sign-in page. |
| `--role admin\|watch` | `admin` | Who the automatic session is: `watch` sees only car `c1` and may not change car data. |
| `--now-ms MS` | `1794850200000` | The virtual "now" at start (2026-11-16 17:30 UTC). It then moves with real time. |
| `--needs-setup` | off | Starts with no accounts, to test the set-up page (code `SETUP-DEV-0000`). |
| `--freeze-clock` | off | Keeps "now" fixed (stable screenshots and tests). |
| `--sse-interval-ms N` | `2000` | How often live values are pushed on `/api/v1/events`. |
| `--verbose` | off | Logs every request to stderr. |

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

Every answer carries the pack's Content Security Policy, `X-Content-Type-Options: nosniff` and
`Referrer-Policy: no-referrer`; `/api/` answers also carry `Cache-Control: no-store`.

## The made-up dataset

Built once at start (about a second) from the seed and "now". History runs to two days past "now": pieces that
start after the virtual clock are not shown yet, and the piece the clock is in is shown in progress.

| Car | What it is | Story |
|---|---|---|
| `c1` Comet | Model Y 2025, Long Range AWD, NCA pack, `America/Los_Angeles` | Home in Noe Valley, work in Mission Bay (San Francisco). Weekday commutes, weekend outings, a road trip to Los Angeles in July with six Supercharger stops, a nine-day holiday parked at home (vampire drain), Sentry at work, scheduled home charging at 23:30 (1 phase, 32 A). **Driving at "now"** (home to Palo Alto). A drive and a charge that stopped mid-way, and a gap of every kind. |
| `c2` Maple | Model 3 2022, RWD, LFP pack, `America/Chicago` | Home in Logan Square, work garage in River North (Chicago). Cold winters with reduced range days, home charging on 3 phases, public AC at work, a road trip to Toronto in August (DC in Michigan and in Ontario, costs in CAD). A slow leak in the rear left tyre for the last three weeks. **Charging at "now"** at work. |
| `c3` Atlas | Model S 2017, 90D, old (legacy) CAN profile, `America/Los_Angeles` | East Bay. 16 months imported from TeslaMate (`source: "teslamate"`, estimated energy) that overlap 5 days of CAN rows, free Supercharging, fewer decoded signals (no tyres, no set points). Its CAN Bridge went quiet about 55 hours before "now": the last log failed its check. |
| `c4`..`cN` | synthetic | Names, models, states and battery from the car's index only; lists elsewhere are empty but valid. |

Also present: places that overlap (Work and the Mission Bay Supercharger, Home and Noe Valley), charges with
and without cost, two software changes per car (one known only as "software changed"), awake, asleep and
vehicle-bus-only spells, attention items, upload tokens, a CAN Bridge key waiting to be assigned, an audit log,
saved trips and purpose rules. Seeds for the sections still to build are in `mockapi/dataset/extras.py`
(a certificate that expires in 12 days, server and MQTT settings, an import job, storage figures).

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
| Battery and efficiency, Reports, Places, Integrations, Server settings and health, Import | `501 { error: { code: "not_built" } }` |

Decisions on points the API document left open are at the end of `docs/ui-api.md` ("Decisions").

## Layout

```
serve.py                 the entry point (flags in mockapi/config.py)
mockapi/server.py        HTTP: static files, SPA fallback, headers, cookie, dispatch, event stream
mockapi/router.py        path patterns -> handlers per method, admin and public methods
mockapi/conventions.py   the request object, query parsing with field errors, cursor paging, CSV
mockapi/errors.py        the one error shape
mockapi/store.py         everything that can change (accounts, sessions, preferences, audit, overrides)
mockapi/rows.py          history pieces -> Drive, Charge and timeline rows (in progress, costs, purposes)
mockapi/live.py          the "now" object, the car list item, attention items
mockapi/series.py        bucketed series that keep minima and maxima
mockapi/events.py        the event hub behind /api/v1/events
mockapi/dataset/         places, cars, routes, the day-by-day simulator, pieces, synthetic cars, extras
mockapi/resources/       one module per section of docs/ui-api.md, plus stubs.py
tests/                   unittest
```

### Adding a section

1. Write `mockapi/resources/<section>.py` with a `register(router)` function. `drives.py` shows the pattern:
   read filters with `req.period()`, `req.enum()`, `req.list_param()` (they answer 400 with `fields`), build
   rows from `req.history()` (the car's pieces: `h.segs`, `h.drives`, `h.charges`, `h.updates`, `h.shadow` for
   TeslaMate copies, `h.overlaps`), page with `conventions.page(req, rows, key=..., reverse=...)` (CSV then
   comes for free), and for changes validate (400 for shape, 422 for rules), change the store under
   `st.lock`, then `st.record(account, action, target, summary)` for the audit log.
2. Add the module to `MODULES` in `mockapi/resources/__init__.py` and remove its lines from `stubs.py` (a real
   route replaces a stub with the same pattern even if the line stays).
3. Car-scoped routes (`{car}` in the pattern) are checked before the handler runs; `admin=("PATCH",)` makes a
   method admin-only; `public=("GET",)` needs no sign-in. Return a dict (JSON), a `conventions.Response`, or
   raise `errors.ApiError`. A `POST` that sets `"_created": True` in its dict answers 201. Keys starting with
   `_` are never sent.
4. Pieces have `values(t)` (every series field at a moment) and `activity()` (strip segments); parked spells
   have `subs` (awake, vehicle-bus-only and asleep sub-spans with reasons and power) and `soc_at(t)`; charges
   have `curve(t)` and `figures()`. The owner's choice of side in a CAN and TeslaMate overlap is
   `h.overlaps[i]["shown"]` (the one dataset field meant to change: change it under `st.lock`).

## Tests

```bash
python3 -m unittest discover -s ui/devserver/tests -t ui/devserver
```

They start servers in-process on free ports and cover the conventions (paging, errors, hidden cars with the
watch role, CSRF, CSV, headers, the SPA fallback), every built resource's documented fields, determinism, paging
a 200,000-car fleet under 200 ms a page, and the event stream.
