# UI packs

Every interface of Sentry Mate is a UI pack: a folder (or a zip of it) with `ui.json` and `web/`. The program
serves the pack's `web/` folder and the JSON API at `/api/v1`; a pack only talks to that API. Custom packs are
installed from a zip in Settings (planned; see [docs/plans/rust-can-logger.md](../docs/plans/rust-can-logger.md)).

| Pack | Look |
|---|---|
| [`packs/instrument`](packs/instrument) | Dark first, a side bar grouped by task, a battery gauge, the map as the main view. Runs on `/api/v1`; how its pages are built: [`web/core/README.md`](packs/instrument/web/core/README.md) |
| [`packs/logbook`](packs/logbook) | Paper and ink with a serif type, a night mode, the day told as a log. Runs on `/api/v1` (preview: `python3 ui/devserver/serve.py --pack ui/packs/logbook`); how its pages are built: [`web/core/README.md`](packs/logbook/web/core/README.md) |

**Status: being built.** Desktop first: a 1280 px laptop up to a native 4K screen, browser zoom 50 to 200 %, and an
Interface size setting. The packs only talk to `/api/v1`; until the Sentry Mate server serves it, the dev API server
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
in the page.

Bundled third-party files keep their licences next to them: MapLibre GL JS (BSD-3-Clause), uPlot (MIT), Inter and
JetBrains Mono (SIL Open Font License 1.1), Bitstream Charter (Bitstream's free licence).
