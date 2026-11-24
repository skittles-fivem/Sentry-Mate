# Archived UI packs

Horizon, Manager, Console, Logbook and Pebble were built-in designs beside Instrument. They are archived here,
kept with their history (`git log --follow ui/archived/<pack>`) for later, and are:

- **not built**: the server embeds only the folders of `ui/packs` (`crates/web/build.rs`), the release bundle
  (`ui/tools/bundle`) only bundles those, and the Docker build leaves this folder out (`.dockerignore`);
- **not served**: a server offers Instrument (and any pack an admin uploads). A choice of an archived pack saved
  before (a person's own `ui_pack`, the pack chosen for everyone) names no pack on the server, so Instrument is
  served instead, with no error; choosing one now is refused like any unknown pack (`422` on `ui_pack`);
- **not tested**: `ui/tools/sync_shared.py` does not copy `ui/shared` into them, and `ui/tools/check.js`, the
  browser tests of `ui/tools`, the dev server and its tests run on the built-in packs only. A change pushed to
  `ui/archived/` alone starts no image build.

Their copies of `ui/shared` (in `web/shared/`) and their pages are as they were on the day they were archived, so
they fall behind the API and the shared modules from here on.

| Pack | Look |
|---|---|
| [`horizon`](horizon) | Simple's structure and pages (six sections: Car, Trips, Charging, Battery, Stats, Settings, their pages as tabs, the car page with the key numbers big and "Show all details") in a synthwave look: a dashboard strip along the top with the sections as buttons, neon HUD panels of dense glass with corner brackets over a dusk sky with a striped setting sun, wireframe mountains and a grid floor that scrolls toward the horizon (Background: Auto, Full, Calm or Off; at most 30 frames a second, still with reduced motion), text always on glass, a daybreak light mode, Orbitron and Chakra Petch type. On a phone Simple's bar of icons and menu in the same look. Runs on `/api/v1`; how its pages are built: [`web/core/README.md`](horizon/web/core/README.md) |
| [`manager`](manager) | Simple: Sentry Manager's frame and look (a section card at the left with six sections: Car, Trips, Charging, Battery, Stats, Settings; the section's pages as pill tabs; flat graphite cards and one lime accent), in dark and a matching light theme, the car page with only the key things big and clear. On a phone the sections are a bar of icons along the top, tables turn into cards and every tap target is at least 44 px. Runs on `/api/v1`; how its pages are built: [`web/core/README.md`](manager/web/core/README.md) |
| [`console`](console) | Keyboard first, like an operator console: flat, monospace and dense, a command bar that shows the route as a path, a command list (Ctrl+K), dark and light. Runs on `/api/v1`; how its pages are built: [`web/core/README.md`](console/web/core/README.md) |
| [`logbook`](logbook) | Paper and ink with a serif type, a night mode, the day told as a log. Runs on `/api/v1`; how its pages are built: [`web/core/README.md`](logbook/web/core/README.md) |
| [`pebble`](pebble) | Soft and touch first: rounded tiles in a grid, a floating dock at the bottom on every screen, sheets that slide up, light with a dark mode, Manrope type. Runs on `/api/v1`; how its pages are built: [`web/core/README.md`](pebble/web/core/README.md) |

## Bringing one back

1. Move it back with history: `git mv ui/archived/<pack> ui/packs/<pack>`. The server embeds it on the next
   build, the dev server lists it and `GET /api/v1/ui-packs` offers it; a saved choice of it serves again.
2. Copy the shared modules in: `python3 ui/tools/sync_shared.py`, then `python3 ui/tools/sync_shared.py --check`.
3. Bring it up to the current API and shared modules, and check every page:
   `node ui/tools/check.js --pack ui/packs/<pack> --pixel` (then `--stable` and `--jumps`), and the browser tests
   of `ui/tools` (they loop over every pack of `ui/packs`: `ui/tools/builtin-packs.js`).
4. Update the lists that name the built-in packs: `builtin_packs_are_embedded_without_dev_files` in
   `crates/web/src/packs/mod.rs`, the dev server's `UiPacks` test (`ui/devserver/tests/test_system.py`), the
   fallback list of Instrument's picker (`PACKS` in `ui/packs/instrument/web/pages/settings-display.js`), and the
   docs (`ui/README.md`, `README.md`, `docs/development.md`).

To look at one without bringing it back, the dev server serves any pack folder:
`python3 ui/devserver/serve.py --pack ui/archived/<pack> --port 8000` (its pages may fail where the API moved on).
