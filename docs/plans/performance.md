# Performance

## Delivery

How fast the pages reach the browser and paint: what the server sends and how (compression, caching, HTTP/2), and
what a page asks for before it is usable. The server's own work on each answer (queries over long histories) is
a separate part.

### How it is measured

`ui/tools/perf/` (how to run it: `ui/tools/perf/README.txt`):
- **Server:** the release binary (`--features dev-seed`) on a seeded data folder: 2 made-up cars, 30 light days,
  64 drives per car.
- **Browser:** Chromium through Playwright, an emulated phone (390x844 at 3x, touch), with:
  - CPU 4x slower (`Emulation.setCPUThrottlingRate`);
  - a Fast 3G-like network: 150 ms latency, 1.44 Mbit/s down, 675 kbit/s up.
- **Pages:** each pack's car (`/cars/c1`), drives, charges and battery pages, and Settings: Display and units.
- **Loads:** each page cold (a new browser profile, empty cache) and then warm (the same URL again in the same
  page).
- **What is counted:**
  - **bytes:** on the wire, headers included;
  - **requests:** pack files plus API calls;
  - **FCP:** first contentful paint;
  - **TTU, time to usable:** the main view has its content, nothing in it is busy or a placeholder, and the last
    file or API answer the page asked for has arrived;
  - **long tasks:** over 50 ms;
  - **duplicate API GETs:** the same URL asked for twice in one load.
- **Noise:** one run per row, so allow a few hundred ms on TTU.

Runs:
- **baseline:** origin/main `5fdaf34`, before any of the changes below.
- **step1:** the server changes only (below), on the same data folder the same day. Its base is a little older
  (`880fa26`); the commits in between change no page measured here.

### What changed

Step 1, the server (`crates/web`):
- **Compressed pack files:** built-in pack files go in brotli (quality 11) or gzip (level 9), made at build time
  (`build.rs`, kept in `OUT_DIR` by content hash, one copy per distinct file). An uploaded pack's files are
  compressed the first time they are asked for. `Content-Encoding`, `Vary: Accept-Encoding`, and a tag per
  encoding.
  - MapLibre GL: 1.1 MB → 243 KB.
  - A pack's `app.css`: about 135 KB → 14 to 22 KB.
- **Versioned addresses:**
  - `index.html` loads the pack's files from `/_v/<version>/...`, where the version is a hash of every file the
    pack serves. Those are `Cache-Control: public, max-age=31536000, immutable`; a new build or pack changes
    every address.
  - Files load each other by relative addresses, so they stay under the same version.
  - `index.html` itself is `no-cache` with an `ETag`. Any other address of a file is `no-cache` with an `ETag`
    and `304`: an unversioned path, or a version that is no longer the pack's (a tab left open across an update).
  - The CSP and the rest of the per-pack rules are unchanged.
- **Module preloading:** `index.html` names every module the first page imports statically (following imports all
  the way down) as `<link rel="modulepreload">`, so the browser asks for them at once instead of level by level.
- **API answers** (`crates/web/src/delivery.rs`):
  - JSON and CSV answers of 1 KB or more go in brotli (quality 4) or gzip.
  - A successful `GET` with a JSON body gets a weak `ETag` (a hash of the body) and `Cache-Control: private,
    no-cache`, and `304` with no body when `If-None-Match` names it.
  - Paths that may carry a secret (tokens, keys, setup, `/me`, sessions, two-step, exports, the move) stay
    `no-store`, untagged and uncompressed.
  - Streams (`/api/v1/events`) go as they are.
  - Compression gives another site nothing to measure: the session cookie is `SameSite=Strict`.
- **HTTP/2:** the web port offers `h2` by ALPN, still TLS 1.3 only. One connection carries every request of a tab;
  HTTP/1.1 with keep-alive stays for clients that don't offer `h2`.

### Results

Means over the 6 packs x 5 pages:

| mean over 6 packs × 5 pages | baseline | step1 |
|---|---|---|
| cold TTU s | 11.1 | 5.4 |
| cold FCP s | 3.6 | 1.7 |
| cold KiB | 1279 | 512 |
| warm TTU s | 7.2 | 2.8 |
| warm FCP s | 1.7 | 0.5 |
| warm KiB | 476 | 120 |

Every request of the step1 run went over HTTP/2; every request of the baseline over HTTP/1.1.

<details><summary>Every pack and page (baseline → step1)</summary>

| pack | page | phase | TTU s (baseline → step1) | FCP s | KiB | requests (static+api) | long tasks ms |
|---|---|---|---|---|---|---|---|
| instrument | car | cold | 17.9 → 7.3 | 3.9 → 1.7 | 2184 → 838 | 57+17 → 57+17 | 1851 → 1916 |
| instrument | car | warm | 14.0 → 3.1 | 1.5 → 0.4 | 1231 → 149 | 57+17 → 57+17 | 1811 → 1473 |
| instrument | drives | cold | 7.4 → 4.4 | 3.9 → 1.8 | 961 → 442 | 46+12 → 46+12 | 1210 → 1328 |
| instrument | drives | warm | 3.5 → 2.0 | 1.5 → 0.4 | 87 → 10 | 46+12 → 46+12 | 945 → 758 |
| instrument | charges | cold | 18.6 → 8.0 | 3.9 → 1.6 | 2123 → 823 | 50+21 → 50+21 | 2719 → 2531 |
| instrument | charges | warm | 16.1 → 4.1 | 2.4 → 0.4 | 1634 → 154 | 50+21 → 50+21 | 2190 → 2066 |
| instrument | battery | cold | 9.6 → 5.2 | 4.0 → 1.7 | 1103 → 464 | 47+22 → 47+22 | 1675 → 1530 |
| instrument | battery | warm | 4.8 → 3.0 | 1.5 → 0.4 | 191 → 23 | 47+22 → 47+22 | 1385 → 1260 |
| instrument | settings | cold | 6.2 → 3.4 | 3.9 → 1.7 | 869 → 418 | 44+14 → 44+14 | 523 → 475 |
| instrument | settings | warm | 2.7 → 1.1 | 1.4 → 0.4 | 62 → 9 | 44+14 → 44+14 | 274 → 183 |
| logbook | car | cold | 16.6 → 6.3 | 2.5 → 0.8 | 2004 → 762 | 45+22 → 47+22 | 2622 → 2955 |
| logbook | car | warm | 15.6 → 4.3 | 2.0 → 0.4 | 1843 → 456 | 45+22 → 47+22 | 2137 → 2145 |
| logbook | drives | cold | 6.6 → 4.0 | 2.5 → 0.9 | 724 → 361 | 37+13 → 39+13 | 730 → 640 |
| logbook | drives | warm | 3.4 → 2.0 | 0.4 → 0.3 | 83 → 67 | 37+13 → 39+13 | 475 → 496 |
| logbook | charges | cold | 17.4 → 7.3 | 2.5 → 0.9 | 1925 → 737 | 42+21 → 44+21 | 2931 → 2667 |
| logbook | charges | warm | 16.9 → 5.2 | 2.0 → 0.4 | 1764 → 433 | 42+21 → 44+21 | 2625 → 2131 |
| logbook | battery | cold | 10.1 → 7.5 | 2.5 → 0.9 | 940 → 409 | 41+27 → 43+27 | 4376 → 4577 |
| logbook | battery | warm | 8.8 → 5.8 | 1.9 → 0.5 | 768 → 104 | 41+27 → 43+27 | 3829 → 4283 |
| logbook | settings | cold | 5.5 → 3.3 | 2.6 → 0.9 | 637 → 341 | 34+13 → 36+13 | 560 → 493 |
| logbook | settings | warm | 2.7 → 1.5 | 0.4 → 0.3 | 43 → 61 | 34+13 → 36+13 | 219 → 297 |
| horizon | car | cold | 17.8 → 7.2 | 4.0 → 1.7 | 2028 → 682 | 60+17 → 63+17 | 2644 → 2188 |
| horizon | car | warm | 14.1 → 3.5 | 1.6 → 0.5 | 1184 → 156 | 60+17 → 63+17 | 2388 → 1857 |
| horizon | drives | cold | 6.5 → 4.3 | 3.9 → 1.7 | 804 → 285 | 49+12 → 52+12 | 1476 → 1667 |
| horizon | drives | warm | 3.8 → 2.3 | 1.6 → 0.5 | 90 → 36 | 49+12 → 52+12 | 1299 → 1006 |
| horizon | charges | cold | 18.2 → 8.0 | 4.0 → 1.7 | 1966 → 667 | 53+21 → 56+21 | 3445 → 3245 |
| horizon | charges | warm | 17.6 → 4.3 | 3.6 → 0.5 | 1875 → 180 | 53+21 → 56+21 | 2728 → 2294 |
| horizon | battery | cold | 8.5 → 5.0 | 4.0 → 1.8 | 946 → 308 | 50+22 → 53+22 | 2115 → 2030 |
| horizon | battery | warm | 5.3 → 4.1 | 1.8 → 0.8 | 194 → 49 | 50+22 → 53+22 | 1608 → 2132 |
| horizon | settings | cold | 5.8 → 3.1 | 4.0 → 1.7 | 713 → 262 | 47+14 → 50+14 | 918 → 712 |
| horizon | settings | warm | 2.7 → 1.3 | 1.5 → 0.4 | 65 → 36 | 47+14 → 50+14 | 290 → 363 |
| console | car | cold | 15.8 → 6.4 | 3.4 → 1.7 | 1771 → 689 | 49+17 → 53+17 | 845 → 831 |
| console | car | warm | 6.8 → 2.7 | 1.4 → 0.3 | 81 → 217 | 49+17 → 53+17 | 393 → 399 |
| console | drives | cold | 5.7 → 3.8 | 3.3 → 1.7 | 625 → 302 | 44+11 → 48+11 | 305 → 346 |
| console | drives | warm | 3.2 → 1.6 | 1.4 → 0.3 | 100 → 97 | 44+11 → 48+11 | 131 → 107 |
| console | charges | cold | 16.4 → 6.8 | 3.4 → 1.7 | 1820 → 704 | 52+26 → 56+26 | 571 → 529 |
| console | charges | warm | 7.2 → 3.1 | 1.4 → 0.4 | 86 → 219 | 52+26 → 56+26 | 223 → 129 |
| console | battery | cold | 6.8 → 4.0 | 3.4 → 1.6 | 749 → 329 | 43+16 → 47+16 | 538 → 486 |
| console | battery | warm | 4.3 → 2.0 | 1.4 → 0.4 | 173 → 106 | 43+16 → 47+16 | 251 → 123 |
| console | settings | cold | 4.9 → 2.8 | 3.4 → 1.7 | 559 → 294 | 41+13 → 45+13 | 202 → 198 |
| console | settings | warm | 2.5 → 1.1 | 1.4 → 0.4 | 50 → 95 | 41+13 → 45+12 | 71 → 93 |
| pebble | car | cold | 16.3 → 6.4 | 3.6 → 2.2 | 1861 → 675 | 48+18 → 52+18 | 765 → 785 |
| pebble | car | warm | 6.7 → 2.8 | 1.8 → 0.9 | 89 → 198 | 48+18 → 52+18 | 244 → 390 |
| pebble | drives | cold | 5.1 → 3.4 | 3.7 → 2.3 | 509 → 225 | 36+12 → 39+12 | 353 → 376 |
| pebble | drives | warm | 2.8 → 1.6 | 1.8 → 0.9 | 54 → 75 | 36+12 → 39+12 | 0 → 0 |
| pebble | charges | cold | 15.9 → 6.5 | 3.7 → 2.2 | 1726 → 608 | 41+18 → 44+18 | 666 → 602 |
| pebble | charges | warm | 6.8 → 3.3 | 1.9 → 1.1 | 65 → 197 | 41+18 → 44+18 | 244 → 219 |
| pebble | battery | cold | 6.0 → 4.0 | 3.7 → 2.3 | 613 → 256 | 38+17 → 41+17 | 741 → 683 |
| pebble | battery | warm | 3.3 → 2.1 | 1.8 → 1.0 | 95 → 84 | 38+17 → 41+17 | 72 → 82 |
| pebble | settings | cold | 5.0 → 3.4 | 3.6 → 2.3 | 536 → 237 | 38+13 → 41+13 | 142 → 325 |
| pebble | settings | warm | 2.7 → 1.3 | 1.8 → 0.9 | 46 → 76 | 38+13 → 41+13 | 0 → 0 |
| manager | car | cold | 18.9 → 8.0 | 4.1 → 1.8 | 2282 → 894 | 58+18 → 58+18 | 1406 → 2162 |
| manager | car | warm | 7.5 → 3.4 | 1.4 → 0.5 | 103 → 131 | 58+18 → 58+18 | 902 → 1596 |
| manager | drives | cold | 8.4 → 4.7 | 4.2 → 1.8 | 1046 → 494 | 47+12 → 47+12 | 1463 → 1545 |
| manager | drives | warm | 3.6 → 2.1 | 1.4 → 0.4 | 88 → 10 | 47+12 → 47+12 | 898 → 1198 |
| manager | charges | cold | 19.3 → 8.6 | 4.1 → 1.8 | 2208 → 875 | 51+21 → 51+21 | 2944 → 2970 |
| manager | charges | warm | 17.5 → 4.4 | 3.6 → 0.5 | 1880 → 154 | 51+21 → 51+21 | 2436 → 2256 |
| manager | battery | cold | 9.8 → 5.9 | 4.2 → 1.7 | 1188 → 517 | 48+22 → 48+22 | 1914 → 1983 |
| manager | battery | warm | 5.2 → 3.4 | 1.6 → 0.6 | 192 → 23 | 48+22 → 48+22 | 1608 → 1646 |
| manager | settings | cold | 7.0 → 3.8 | 4.1 → 1.8 | 954 → 471 | 45+14 → 45+14 | 591 → 678 |
| manager | settings | warm | 2.9 → 1.3 | 1.5 → 0.5 | 63 → 9 | 45+14 → 45+14 | 368 → 379 |

</details>

Caveats:
- **Warm loads of the map pages, baseline:** in instrument, logbook, horizon and manager, the warm car and
  charges pages fetched some `no-cache` files again in full under network emulation instead of revalidating them.
  MapLibre GL was one of them. Their warm bytes are high for that reason. In step1, a warm load asks for no file under
  `/_v/`. Its few remaining `304`s (0 to 10 per page) are files the pages still name by a root address (fonts in
  CSS, page style sheets).
- **Duplicate API GETs in the baseline:**
  - `/api/v1/me` twice on the car and charges pages of every pack;
  - logbook battery: `/api/v1/cars/c1/battery` twice;
  - console settings: `/api/v1/cars?limit=200&sort=name` twice.

# Plan: page speed with a long history

Status (2026-11-23): measured; the first fixes are in (below), the rest are being worked on.

Pages were slow on a server with a long history (the owner's: one Model Y, about 2.7 million imported TeslaMate rows,
mostly positions, plus TeslaFi imports, SMO uploads and TeslaMate and TeslaFi as live inputs): the Battery page sat on
its loading skeleton, other pages were slow too. This is how the pages' API calls are timed on a history that size,
what they took, and what was changed.

**Target:** every page's API calls under 300 ms warm and under 2 s cold (the first call after the server started), in
a release build on 4 cores, on that history.

## The made-up history

All of it is made up (no one's car):

```bash
cargo build --release -p sentry-mate --features dev-seed
B=target/release/sentry-mate
$B --data D/data --keys D/keys init
$B --data D/data --keys D/keys dev seed --days 75 --light --start 2026-09-07 --seed 7   # SMO logs, the last 75 days
$B --data D/data --keys D/keys dev history --end 2026-11-21                            # the rest (about 15 min)
```

`dev history` (`crates/sentry-mate/src/history.rs`, in a `dev-seed` build only) gives the car `dev seed` made:

- a **TeslaMate import** of 330 days through the importer's own sink, as a backup's rows arrive: 2,693,072 positions (a
  position a second while driving, every 15 s while charging), 1,300 drives, 500 charges with 372,188 charge points,
  4,377 states, 10 software updates, two geofences;
- a **TeslaFi import** over its first 60 days (a point a minute: 23,296), with its own drives and charges, overlapping
  the TeslaMate one;
- **TeslaFi's API** (a sample a minute) and **local TeslaMate** (one every 5 s) as live inputs over the last 30 days:
  1,665,520 samples, built into the timeline by the engine as a poll's are;
- gap filling on (the server's default).

204 MB on disk.

## How it is timed

`crates/sentry-mate/src/perf.rs` lists every page's calls (as the packs make them; periods end at the history's end):
car, timeline, drives, charges, battery, efficiency, statistics, travel, software, places, health, and the series at a
day, 30 days, a year and all. Each call is timed **first start** (the first call after the server started on the data
folder for the first time: nothing worked out before), **after a restart** (what the server kept on disk stays, what it
kept in memory is gone) and **warm** (the faster of the next two). The tests:

- `cargo test --release -p sentry-mate perf_pages_stay_within_their_budget -- --ignored --nocapture`: a 4-month
  history (524,056 positions), the budget asserted in a release build; run weekly by the Full tests workflow.
- `PERF_DATA=DIR cargo test --release -p sentry-mate measure_pages -- --ignored --nocapture`: the owner's size, prints
  the table (keeps the data folder in `DIR` for the next run).

The numbers below were taken with a server process (`serve`) and `curl`, one call at a time, on this container's 4
cores (other builds sometimes ran at the same time: take them as about, ±20 %).

## The times (ms)

| Call | origin/main: first start / restart / warm | now: first start / restart / warm |
|---|---:|---:|
| `cars/c1` | 6 / 7 / 6 | 7 / 7 / 6 |
| `cars/c1/now` | 511 / 463 / 386 | 538 / 566 / 5 |
| `cars/c1/activity?period=1d` | 1,260 / 1,205 / 1,189 | 4 / 4 / 3 |
| `cars/c1/series?5 fields&period=1d&max_points=600` | 1,282 / 1,317 / 1,310 | 74 / 61 / 54 |
| `cars/c1/timeline?limit=20` | 1,275 / 1,152 / 1,230 | 10 / 9 / 9 |
| `cars/c1/health` | 96 / 91 / 100 | 88 / 89 / 75 |
| `cars/c1/totals` | 484 / 411 / 440 | 5 / 6 / 4 |
| `cars/c1/statistics?period=month&limit=12` | 12,353 / 11,686 / 10,823 | 5,981 / 6,353 / 4,663 |
| `cars/c1/timeline?period=30d&limit=50` | 1,326 / 1,256 / 1,316 | 22 / 22 / 21 |
| `cars/c1/states?period=30d` | 4 / 5 / 4 | 4 / 4 / 4 |
| `cars/c1/activity?period=30d` | 1,340 / 1,215 / 1,221 | 5 / 5 / 5 |
| `cars/c1/drives?limit=50` | 2,045 / 1,896 / 1,663 | 1,016 / 1,121 / 1,049 |
| `cars/c1/drives?period=365d&limit=50` | 1,752 / 1,853 / 1,864 | 1,107 / 1,005 / 1,008 |
| `cars/c1/drive-stats?period=365d` | 1,861 / 1,933 / 1,838 | 1,626 / 1,743 / 1,538 |
| `cars/c1/gaps?limit=50` | 62 / 67 / 49 | 60 / 59 / 56 |
| `cars/c1/charges?limit=50` | 3,573 / 3,169 / 3,325 | 1,356 / 1,305 / 1,035 |
| `cars/c1/charging-stats?period=365d` | 4,394 / 3,416 / 3,491 | 1,271 / 1,244 / 1,075 |
| `charging-stats/places?cars=c1` | 3,586 / 3,255 / 3,354 | 1,205 / 1,326 / 978 |
| `cars/c1/charging-stats/dc-curve?period=365d` | 423 / 365 / 434 | 20 / 20 / 15 |
| `cars/c1/battery` | 760 / 458 / 457 | 305 / 38 / 41 |
| `cars/c1/battery/capacity?group=day&x=odometer&limit=500` | 521 / 396 / 436 | 65 / 68 / 84 |
| `cars/c1/battery/capacity?group=month&x=odometer&limit=500` | 520 / 448 / 481 | 68 / 45 / 57 |
| `cars/c1/battery-level?period=365d&bucket=day` | 2,718 / 2,560 / 2,449 | 1,372 / 1,391 / 688 |
| `cars/c1/projected-range?period=365d` | 3,210 / 3,061 / 2,882 | 1,310 / 1,158 / 1,116 |
| `cars/c1/parked?limit=12` | 1,692 / 1,469 / 1,577 | 335 / 329 / 344 |
| `cars/c1/settings` | 5 / 5 / 5 | 6 / 5 / 5 |
| `cars/c1/efficiency` | 2,482 / 2,302 / 2,064 | 297 / 294 / 295 |
| `cars/c1/efficiency?period=365d` | 2,425 / 2,344 / 2,310 | 315 / 273 / 312 |
| `cars/c1/temperatures?period=365d` | 2,823 / 2,768 / 2,803 | 943 / 801 / 950 |
| `cars/c1/statistics?period=month` | 12,411 / 11,836 / 11,828 | 4,415 / 4,759 / 4,841 |
| `cars/c1/statistics?period=year` | 12,807 / 11,551 / 11,644 | 4,633 / 4,646 / 4,896 |
| `cars/c1/statistics?period=all` | 12,534 / 11,218 / 11,710 | 4,753 / 4,819 / 4,574 |
| `cars/c1/mileage?group=month` | 429 / 468 / 396 | 6 / 7 / 5 |
| `visits?cars=c1&limit=50` | 455 / 443 / 432 | 6 / 6 / 4 |
| `visits/days?cars=c1` | 414 / 396 / 428 | 5 / 7 / 5 |
| `tracks?cars=c1&period=30d&max_points=2000` | 83,333 / 76,860 / 77,282 | 1,760 / 1,883 / 1,863 |
| `cars/c1/updates` | 424 / 419 / 420 | 4 / 5 / 4 |
| `cars/c1/firmware` | 3 / 3 / 3 | 3 / 4 / 3 |
| `cars/c1/firmware/history` | 428 / 414 / 417 | 4 / 4 / 3 |
| `places` | 492 / 399 / 4 | 6 / 6 / 3 |
| `places/suggestions` | 442 / 440 / 397 | 7 / 6 / 6 |
| `cars/c1/storage` | 60 / 32 / 29 | 31 / 26 / 29 |
| `cars/c1/gaps` | 54 / 60 / 63 | 60 / 58 / 57 |
| `cars/c1/series?5 fields&period=1d&max_points=1000` | 1,284 / 1,329 / 1,306 | 46 / 39 / 34 |
| `cars/c1/series?5 fields&period=30d&max_points=1000` | 2,312 / 2,126 / 2,213 | 838 / 722 / 712 |
| `cars/c1/series?5 fields&period=365d&max_points=1000` | 9,831 / 9,206 / 9,458 | 4,613 / 5,105 / 4,783 |
| `cars/c1/series?5 fields&period=all&max_points=1000` | 9,290 / 9,054 / 8,620 | 4,454 / 4,955 / 4,946 |

## What was slow, and what changed

1. **Which import shows where, worked out on every request.** Where a TeslaMate and a TeslaFi import cover the same
   time, each car answer works out which one shows (`query/system.rs`, `Sites`), and that counted every position both
   imports have there: on every request of every page (0.1 s on 4 months, about 0.4 s here, and once per drive in
   `/tracks`: 77 s). The db now keeps a **version** of each car's data (live, imported, and the places and settings
   every car shares; in memory, moved on after each write, `db::Db::version`), and the engine keeps such parts while
   the versions they were made at stay (`engine/src/memo.rs`): the shown sites and the live coverage of imported
   history.
2. **The same days read again and again.** A day of the time series is a sealed, compressed file; most answers read
   whole days, several answers of a page read the same days, and a list reads a drive's or a charge's day once per
   row. Day snapshots are now kept opened and unpacked in memory (`db/src/plain.rs`, 256 MB at most, the least
   recently used let go first), each with its file's inode, size and change time: a day written again is a new file
   (written whole and renamed into place), so it is read again.
3. **The pages waited for their slowest call.** Battery and car in every pack (and efficiency, timeline, travel,
   charges, health where they waited on several calls) lay out at once with each block's room held, and fill each
   block when its own answers come. Requests asked twice on one load are asked once.

# More performance: logs, rebuilds, start-up, the always-on parts

Status (2026-11-23): measured area by area; each section says what was measured, what changed and what was left.
All on made-up data, in a release build on this container's 4 cores (take the times as about, ±15 %).

## CAN logs: unpacking and decoding

**How it is measured.** `cargo test --release -p timeline --test decode_speed -- --ignored --nocapture`: the busiest
60 one-minute logs in a row of a made-up day at the car's real rates (a drive: 1,161,659 frames, 323 a second, 53 MB
as candump text), read one after the other on one core, the best of three runs. Three times per format: unpacked only
(xz, and for columns the columns put back into lines), unpacked and every line parsed, and read whole into samples
(`timeline::read_log`: what the engine does with a log).

| An hour of driving (60 logs) | before | now |
|---|---:|---:|
| candump (`/1`, 3.4 MB packed): unpack | 565 ms | 526 ms |
| candump: read whole into 3,566 samples | 1,173 ms (1,010 ns a frame) | 721 ms (621 ns a frame) |
| columns (`/2`, 2.1 MB packed): unpack | 354 ms | 385 ms |
| columns: read whole into 3,566 samples | 922 ms (794 ns a frame) | 600 ms (517 ns a frame) |

**What was slow, and what changed.**

1. **Every signal of every frame was decoded, bit by bit in floating point.** A sample is made of about 110 signals;
   the DBC set has about 2,800, and each frame's signals were all taken out (each bit a `powi`), put in a list and
   looked up by name. Now `SampleBuilder::feed` takes only the wanted signals out of a frame (a table per message,
   made once per DBC set: `timeline/src/signals.rs`, `Table::by_message`), most frames are dropped after one lookup,
   and a signal of up to 53 bits is taken out with integers (`dbc::raw_bits`; longer ones keep the bit-by-bit sum,
   which is what Sentry Six Manager's JavaScript gives). The interface name (`can0`) is no longer formatted and
   looked up per frame (`dbc::Decoder::message`).
2. Checked the same: `dbc`'s `the_quick_bits_are_the_summed_bits` (every signal of both sets and every start and
   length, on made-up payloads, whole and cut short) and `decode_speed.rs`'s
   `feeding_frames_gives_what_every_decoded_signal_gives` (a made-up day with trouble, both formats: the samples are
   those of every signal decoded and given by name).

**Left as it is.**

- **xz itself is now most of the time** (about 470 of 721 ms for candump, 230 of 600 ms for columns). The pure-Rust
  `lzma-rs` unpacks candump text at about 115 MB/s. `lzma-rust2` (already used by the tests to pack) was tried:
  the same on candump (442 against 467 ms), twice as fast on columns (119 against 226 ms, about 100 ms an hour of
  driving). Not changed here: the unpacker is where the upload limits are enforced (`logfmt` stops at the unpack
  limit through its own sink), and that wants its own change with its own tests.
- **Columns logs are put back into text lines and parsed again** (about 150 ms an hour of driving). A way straight
  from the columns to frames would save most of it; the line is what every reader of a log is written against.
- A log is a minute long, so memory per log is small (a columns log's body, at most 64 MB by the limit, in practice
  about 1 MB) and logs of different cars are already read at once (one car's are read one after the other: each
  changes the car's timeline).
