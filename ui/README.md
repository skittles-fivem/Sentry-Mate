# UI packs

Every interface of Sentry Mate is a UI pack: a folder (or a zip of it) with `ui.json` and `web/`. The program
serves the pack's `web/` folder and the JSON API at `/api/v1`; a pack only talks to that API. Custom packs are
installed from a zip in Settings (planned; see [docs/plans/rust-can-logger.md](../docs/plans/rust-can-logger.md)).

| Pack | Look |
|---|---|
| [`packs/instrument`](packs/instrument) | Dark first, a side bar grouped by task, a battery gauge, the map as the main view. Runs on `/api/v1`; how its pages are built: [`web/core/README.md`](packs/instrument/web/core/README.md) |

Instrument is the one built-in pack: the server embeds every folder of `ui/packs`, and the tools and tests run on
those. The other designs (Logbook, Horizon, Console, Pebble and Manager) are archived in
[`archived/`](archived/README.md): kept for later, not built, served or tested.

**Status: being built.** Desktop first: a 1280 px laptop up to a native 4K screen, browser zoom 50 to 200 %, and an
Interface size setting: the account's (follows the person to every screen) and, under it, one for this device only
(Automatic or 80 to 125 %, kept in this browser; Settings: Display and units and the account menu). The packs only talk to `/api/v1`; until the Sentry Mate server serves it, the dev API server
stands in for it, with made-up data (cars, drives, charges and places invented for the purpose, not anyone's real car).

Preview a pack with the dev API server. It replaces `python3 -m http.server`, which cannot answer `/api/v1`:

```bash
python3 ui/devserver/serve.py --pack ui/packs/instrument --port 8000
```

Then open `http://127.0.0.1:8000/`. Add `--cars 200000` to try a large fleet, `--latency-ms 150` for a slow network,
`--signed-out` or `--needs-setup` for the sign-in and set-up screens (all flags: [`devserver/README.md`](devserver/README.md)).
Check every page of a pack with `node ui/tools/check.js --pack instrument` ([`tools/README.md`](tools/README.md)).

Rules every pack follows: a strict Content Security Policy (scripts, styles and fonts only from the pack itself; map
tiles from the configured tile server), no outside requests, metric units in the API with the person's units applied
in the page, and pages that stay still: a page you are on must not jump around, and nothing may show up, go away
and come back while live values and list changes arrive. Keep the old content until the new data is there (no
loading text or empty state in between), give blocks that fill in later their room from the start, and change text
in place. `node ui/tools/check.js --pack NAME --stable` checks it ([`tools/README.md`](tools/README.md#stability-checks---stable)).

Bundled third-party files keep their licences next to them: MapLibre GL JS (BSD-3-Clause), uPlot (MIT), Inter, Orbitron,
Chakra Petch, IBM Plex Sans, Manrope and JetBrains Mono (SIL Open Font License 1.1), Bitstream Charter (Bitstream's
free licence).

All six are built into the server; Instrument is the default until an admin picks another for everyone (Settings,
Interface), and each person can pick their own.
