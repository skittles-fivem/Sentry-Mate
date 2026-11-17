# UI packs

Every interface of Sentry Mate is a UI pack: a folder (or a zip of it) with `ui.json` and `web/`. The program
serves the pack's `web/` folder and the JSON API at `/api/v1`; a pack only talks to that API. Custom packs are
installed from a zip in Settings (planned; see [docs/plans/rust-can-logger.md](../docs/plans/rust-can-logger.md)).

| Pack | Look |
|---|---|
| [`packs/instrument`](packs/instrument) | Dark, a side bar, a battery gauge, the map as the main view |
| [`packs/logbook`](packs/logbook) | Paper and ink with a serif type, a night mode, the day told as a log |

**Status: design prototypes.** Both run on made-up data from `web/mock-api.js` (cars, drives, charges and places
invented for the purpose, not anyone's real car). `app.js` imports only that module, so moving to the real server
means replacing that one file with one that calls `/api/v1`. Desktop first: a 1280 px laptop up to a native 4K
screen, browser zoom 50 to 200 %, and an Interface size setting.

Preview one with any static file server, for example:

```bash
python3 -m http.server -d ui/packs/instrument/web 8000
```

Then open `http://127.0.0.1:8000/`. Add `?cars=200000` to try a large fleet, `?latency=0` to drop the simulated
network delay.

Rules every pack follows: a strict Content Security Policy (scripts, styles and fonts only from the pack itself; map
tiles from the configured tile server), no outside requests, metric units in the API with the person's units applied
in the page.

Bundled third-party files keep their licences next to them: MapLibre GL JS (BSD-3-Clause), uPlot (MIT), Inter and
JetBrains Mono (SIL Open Font License 1.1), Bitstream Charter (Bitstream's free licence).
