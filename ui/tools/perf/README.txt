Sentry Mate browser-delivery measurement (made-up data only)
============================================================

Files
  compare.js   node compare.js before.json after.json [...]: a markdown table of runs side by side, and means
  measure.js   the harness: per pack x page, a cold and a warm load on an emulated phone; writes JSON + a markdown table
  setup.js     first-run setup of a freshly seeded server (reads SETUP-CODE.txt, POST /api/v1/setup over HTTPS)
  settled.js   waits until the engine has decoded the seeded logs (drive/charge counts stop changing)
  run.sh       stop/start a given binary on the seeded data folder and run the harness

Needs: Node with Playwright resolvable via require('playwright') and its Chromium; a sentry-mate binary built
with --features dev-seed only for seeding (serving works with any build).

Below, P is a scratch folder holding a copy of these files (data/ and keys/ go next to them; keep it out of the
repository), D=$P/data, K=$P/keys.

1. Seed once (already done for the baseline: 30 light days, 2 cars, start 31 days before 2026-11-23)
     cargo build --release -p sentry-mate --features dev-seed --locked
     B=target/release/sentry-mate
     mkdir -p $D $K
     $B --data $D --keys $K init                       # prints a recovery key (made-up server; not needed)
     $B --data $D --keys $K dev seed --days 30 --cars 2 --light --start $(date -u -d '31 days ago' +%F)   # ~3 min
     $B --data $D --keys $K serve --bind 127.0.0.1 > $P/server.log 2>&1 &
     until curl -sk -o /dev/null https://127.0.0.1:10101/health; do sleep 1; done
     node $P/setup.js --base https://127.0.0.1:10101 --keys $K      # admin "owner" / "correct horse battery staple"
     node $P/settled.js --base https://127.0.0.1:10101              # ~4 min until 64 drives per car
   Note: the seed is relative to "today", so a period preset (24h, 30d) shows less history as days pass. For a
   fair A/B compare both binaries on the same day, or reseed into a new folder (--start fixed) for both.

2. Rerun with another binary on the same data folder (the account and the seeded history are kept; the engine
   may rebuild for a while after a newer binary starts, so settled.js runs first):
     $P/run.sh /path/to/sentry-mate improved          # -> $P/improved.json, $P/improved.md, $P/improved.log
   or by hand:
     pkill -f -- "--data $D --keys $K serve" ; $BIN --data $D --keys $K serve --bind 127.0.0.1 > $P/server.log 2>&1 &
     until curl -sk -o /dev/null https://127.0.0.1:10101/health; do sleep 1; done
     node $P/settled.js
     TMPDIR=$P node $P/measure.js --label improved --out $P/improved.json --md $P/improved.md
     pkill -f -- "--data $D --keys $K serve"

measure.js options
  --base URL (https://127.0.0.1:10101)  --user owner  --pass '...'  --car c1 (default: first car)
  --packs instrument,logbook,horizon,console,pebble,manager   --pages car,drives,charges,battery,settings
  --out FILE.json  --md FILE.md  --label NAME  --timeout 90000  --quiet 500
  --cpu 4  --latency 150  --no-throttle  --warm-pause MS  --verbose (every request in the JSON)
  --device laptop (1440x900 at 1x, CPU 2x, no network throttling unless --latency)  --scroll (scroll the list: fps)
  --pages logbook (also: the Logbook). rows ms / list ms: first list row painted, from navigation / from the list's
  request; dom rows and nodes at the end. The dev server works too: --base http://127.0.0.1:PORT --user admin
  --pass sentry-mate-dev (serve.py --drives 3000 for a long Logbook)

What is measured (see the header of measure.js for detail)
  device   390x844 CSS px, deviceScaleFactor 3, isMobile, hasTouch (Android Chrome UA)
  CPU      Emulation.setCPUThrottlingRate {rate: 4}
  network  Network.emulateNetworkConditions {latency 150 ms, download 180000 B/s (1.6 Mbit/s x 0.9),
           upload 84375 B/s (750 kbit/s x 0.9)}
  cold     a new persistent Chromium profile (empty disk cache) per pack/page, session cookie copied in
  warm     page.goto of the same URL again in that same page (HTTP cache warm; packs answer
           Cache-Control: no-cache + ETag, so static files come back as 304s)
  bytes    sum of encodedDataLength from Network.loadingFinished (headers + body on the wire); split static vs /api/
           (the module worker's own script is sized through Playwright, its response never reaches the page session)
  requests count split static vs /api/ (the SSE stream /api/v1/events counts as one api request, never its bytes)
  protocol Network.responseReceived response.protocol
  FCP      performance 'first-contentful-paint'
  TTU      "time to usable", in the page's clock from navigation start: max( the moment the main view (#view in
           instrument/horizon/console/manager, #main in logbook/pebble) last became ready = it has an element child
           and text, no aria-busy="true" on it or inside it, and no loading placeholder inside it (.loading,
           .page--loading, .lb-loading, .loading-grid, .loading-head, .skeleton*) ; the last responseEnd of any
           resource except /api/v1/events ). The run waits for that plus 500 ms with nothing in flight
           (Playwright's view of requests, workers included; SSE excluded). Counts are taken up to that point.
  long tasks  PerformanceObserver 'longtask' (buffered, injected by addInitScript): count, total ms, max
  dup GETs    identical API GET URLs (path+query) requested more than once during the load (SSE excluded): sum of n-1

Page paths (all packs share ui/packs/*/web/shared/state.js ROUTES):
  car /cars/{car}, drives /cars/{car}/drives, charges /cars/{car}/charges, battery /cars/{car}/battery,
  settings /settings/display ("Settings: Display and units"; no pack has a plain /settings route of its own,
  manager's 'settings' hub page id aside)
The pack is chosen per account with PATCH /api/v1/me/preferences {ui_pack: "<id>"} (set back to null at the end).

Caveats seen on the baseline (origin/main 5fdaf34, 2026-11-23)
  - Warm loads of the pages that load the map (car, charges) in instrument, logbook, horizon and manager, plus
    logbook battery, fetched some files again in full (200, not 304): maplibre-gl.mjs (1.1 MB), sometimes app.css
    or a page module. With --no-throttle the same instrument car warm load is all 304s (98 KiB), and pebble and
    console warm loads are 304s under throttling too, so it is tied to Chrome's cache under DevTools network
    emulation and/or how those packs load the module; cause not established. Compare runs made under the same
    conditions; check the "biggest" list in the JSON (status 200 vs 304).
  - The worker script (maplibre-gl-worker.mjs) is sized via Playwright (its CDP events go to the worker target).
  - The server speaks HTTP/1.1 only here, so every request is h1 in the baseline.
  - One run per row: expect some noise on TTU; run twice for close calls.
