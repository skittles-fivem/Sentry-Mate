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
- **step2:** step 1 and the page changes (below), on origin/main `a14f0cf` (with that day's page-speed work by another session), the same
  data folder the same day.

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

Step 2, the pages (`ui/`):
- **Map and chart style sheets:** they are no longer in `index.html`'s head (console, horizon, instrument,
  manager), where they held up the first paint. The map and chart loaders fetch the library and its style sheet
  together and draw nothing until both are in.
- **Relative addresses:** fonts in CSS, page style sheets, uPlot, MapLibre and its worker, and the brand image are
  named relative to the file. They load under `/_v/<version>/` too, so a warm load revalidates nothing.
- **Fonts:** every `@font-face` already had `font-display: swap`. Each pack now preloads the 2 or 3 font files
  its first paint uses (measured), and no others.
- **API reads** (`ui/shared/api.js`):
  - **Identical reads in flight** share one request, keyed by URL, accept and headers.
  - **Kept answers:** the session's `/me` at start is read with `keep: true`, so the map's tile settings
    (`maxAgeMs`) take that answer. That removed the second `/api/v1/me` on every map page.
  - **Revalidation:** reads go with `cache: 'no-cache'`, so the browser revalidates with the server's `ETag`
    and an unchanged answer comes back as `304` without a body.
- **Pages fixed:** logbook battery and console settings each asked for the same data twice; they now reuse it.

### Results

Means over the 6 packs x 5 pages:

| mean over 6 packs × 5 pages | baseline | step1 | step2 |
|---|---|---|---|
| cold TTU s | 11.1 | 5.4 | 5.3 |
| cold FCP s | 3.6 | 1.7 | 1.6 |
| cold KiB | 1279 | 512 | 472 |
| warm TTU s | 7.2 | 2.8 | 2.6 |
| warm FCP s | 1.7 | 0.5 | 0.5 |
| warm KiB | 476 | 120 | 60 |

- **Protocol:** every request of step1 and step2 went over HTTP/2; every request of the baseline over HTTP/1.1.
- **Duplicate API GETs** (the same URL twice in one load), summed over all 60 loads: 28 in the baseline, none in step2.
- **API bytes** per page load: 56 KiB → 10 KiB.
- **Warm loads in step2:** no pack file is asked for again (no `304` either).
- **Long tasks** per cold load are a little higher in step2: 1.4 s → 1.6 s (warm: 1.1 s in both). With the files
  arriving sooner and at once, more script runs in the same stretch. They are the pages' own work (drawing the
  map, the lists), which this part doesn't change.

<details><summary>Every pack and page (baseline → step1 → step2)</summary>

| pack | page | phase | TTU s (baseline → step1 → step2) | FCP s | KiB | requests (static+api) | long tasks ms |
|---|---|---|---|---|---|---|---|
| instrument | car | cold | 17.9 → 7.3 → 7.0 | 3.9 → 1.7 → 1.7 | 2184 → 838 → 841 | 57+17 → 57+17 → 57+16 | 1851 → 1916 → 2365 |
| instrument | car | warm | 14.0 → 3.1 → 3.4 | 1.5 → 0.4 → 0.5 | 1231 → 149 → 130 | 57+17 → 57+17 → 57+16 | 1811 → 1473 → 2016 |
| instrument | drives | cold | 7.4 → 4.4 → 4.4 | 3.9 → 1.8 → 1.7 | 961 → 442 → 436 | 46+12 → 46+12 → 45+12 | 1210 → 1328 → 1459 |
| instrument | drives | warm | 3.5 → 2.0 → 1.9 | 1.5 → 0.4 → 0.4 | 87 → 10 → 9 | 46+12 → 46+12 → 45+12 | 945 → 758 → 792 |
| instrument | charges | cold | 18.6 → 8.0 → 8.1 | 3.9 → 1.6 → 1.7 | 2123 → 823 → 825 | 50+21 → 50+21 → 50+20 | 2719 → 2531 → 2670 |
| instrument | charges | warm | 16.1 → 4.1 → 4.0 | 2.4 → 0.4 → 0.4 | 1634 → 154 → 131 | 50+21 → 50+21 → 50+20 | 2190 → 2066 → 2252 |
| instrument | battery | cold | 9.6 → 5.2 → 5.4 | 4.0 → 1.7 → 1.7 | 1103 → 464 → 459 | 47+22 → 47+22 → 46+22 | 1675 → 1530 → 2009 |
| instrument | battery | warm | 4.8 → 3.0 → 3.3 | 1.5 → 0.4 → 0.7 | 191 → 23 → 23 | 47+22 → 47+22 → 46+22 | 1385 → 1260 → 1608 |
| instrument | settings | cold | 6.2 → 3.4 → 3.4 | 3.9 → 1.7 → 1.7 | 869 → 418 → 411 | 44+14 → 44+14 → 42+14 | 523 → 475 → 554 |
| instrument | settings | warm | 2.7 → 1.1 → 1.2 | 1.4 → 0.4 → 0.5 | 62 → 9 → 9 | 44+14 → 44+14 → 42+14 | 274 → 183 → 602 |
| logbook | car | cold | 16.6 → 6.3 → 6.6 | 2.5 → 0.8 → 1.0 | 2004 → 762 → 710 | 45+22 → 47+22 → 45+21 | 2622 → 2955 → 3367 |
| logbook | car | warm | 15.6 → 4.3 → 4.4 | 2.0 → 0.4 → 0.4 | 1843 → 456 → 132 | 45+22 → 47+22 → 45+21 | 2137 → 2145 → 2963 |
| logbook | drives | cold | 6.6 → 4.0 → 4.0 | 2.5 → 0.9 → 1.0 | 724 → 361 → 308 | 37+13 → 39+13 → 37+13 | 730 → 640 → 845 |
| logbook | drives | warm | 3.4 → 2.0 → 1.9 | 0.4 → 0.3 → 0.3 | 83 → 67 → 13 | 37+13 → 39+13 → 37+13 | 475 → 496 → 477 |
| logbook | charges | cold | 17.4 → 7.3 → 7.8 | 2.5 → 0.9 → 1.0 | 1925 → 737 → 685 | 42+21 → 44+21 → 42+20 | 2931 → 2667 → 3865 |
| logbook | charges | warm | 16.9 → 5.2 → 4.1 | 2.0 → 0.4 → 0.5 | 1764 → 433 → 129 | 42+21 → 44+21 → 42+20 | 2625 → 2131 → 2355 |
| logbook | battery | cold | 10.1 → 7.5 → 7.9 | 2.5 → 0.9 → 1.0 | 940 → 409 → 356 | 41+27 → 43+27 → 41+26 | 4376 → 4577 → 5322 |
| logbook | battery | warm | 8.8 → 5.8 → 5.0 | 1.9 → 0.5 → 0.3 | 768 → 104 → 26 | 41+27 → 43+27 → 41+26 | 3829 → 4283 → 3913 |
| logbook | settings | cold | 5.5 → 3.3 → 3.2 | 2.6 → 0.9 → 1.0 | 637 → 341 → 288 | 34+13 → 36+13 → 34+13 | 560 → 493 → 484 |
| logbook | settings | warm | 2.7 → 1.5 → 1.2 | 0.4 → 0.3 → 0.3 | 43 → 61 → 7 | 34+13 → 36+13 → 34+13 | 219 → 297 → 234 |
| horizon | car | cold | 17.8 → 7.2 → 6.3 | 4.0 → 1.7 → 1.6 | 2028 → 682 → 659 | 60+17 → 63+17 → 60+16 | 2644 → 2188 → 2956 |
| horizon | car | warm | 14.1 → 3.5 → 4.1 | 1.6 → 0.5 → 0.6 | 1184 → 156 → 129 | 60+17 → 63+17 → 60+16 | 2388 → 1857 → 2586 |
| horizon | drives | cold | 6.5 → 4.3 → 4.2 | 3.9 → 1.7 → 1.6 | 804 → 285 → 253 | 49+12 → 52+12 → 48+12 | 1476 → 1667 → 1712 |
| horizon | drives | warm | 3.8 → 2.3 → 2.0 | 1.6 → 0.5 → 0.5 | 90 → 36 → 10 | 49+12 → 52+12 → 48+12 | 1299 → 1006 → 1024 |
| horizon | charges | cold | 18.2 → 8.0 → 8.2 | 4.0 → 1.7 → 1.7 | 1966 → 667 → 643 | 53+21 → 56+21 → 53+20 | 3445 → 3245 → 3669 |
| horizon | charges | warm | 17.6 → 4.3 → 4.2 | 3.6 → 0.5 → 0.5 | 1875 → 180 → 131 | 53+21 → 56+21 → 53+20 | 2728 → 2294 → 2560 |
| horizon | battery | cold | 8.5 → 5.0 → 5.5 | 4.0 → 1.8 → 1.6 | 946 → 308 → 277 | 50+22 → 53+22 → 49+22 | 2115 → 2030 → 2397 |
| horizon | battery | warm | 5.3 → 4.1 → 3.1 | 1.8 → 0.8 → 0.7 | 194 → 49 → 23 | 50+22 → 53+22 → 49+22 | 1608 → 2132 → 1593 |
| horizon | settings | cold | 5.8 → 3.1 → 2.9 | 4.0 → 1.7 → 1.6 | 713 → 262 → 229 | 47+14 → 50+14 → 45+14 | 918 → 712 → 536 |
| horizon | settings | warm | 2.7 → 1.3 → 1.1 | 1.5 → 0.4 → 0.4 | 65 → 36 → 9 | 47+14 → 50+14 → 45+14 | 290 → 363 → 506 |
| console | car | cold | 15.8 → 6.4 → 5.6 | 3.4 → 1.7 → 1.4 | 1771 → 689 → 603 | 49+17 → 53+17 → 49+16 | 845 → 831 → 800 |
| console | car | warm | 6.8 → 2.7 → 2.3 | 1.4 → 0.3 → 0.3 | 81 → 217 → 128 | 49+17 → 53+17 → 49+16 | 393 → 399 → 333 |
| console | drives | cold | 5.7 → 3.8 → 3.1 | 3.3 → 1.7 → 1.3 | 625 → 302 → 208 | 44+11 → 48+11 → 42+11 | 305 → 346 → 213 |
| console | drives | warm | 3.2 → 1.6 → 1.4 | 1.4 → 0.3 → 0.4 | 100 → 97 → 9 | 44+11 → 48+11 → 42+11 | 131 → 107 → 79 |
| console | charges | cold | 16.4 → 6.8 → 6.6 | 3.4 → 1.7 → 1.3 | 1820 → 704 → 619 | 52+26 → 56+26 → 52+25 | 571 → 529 → 619 |
| console | charges | warm | 7.2 → 3.1 → 2.8 | 1.4 → 0.4 → 0.3 | 86 → 219 → 130 | 52+26 → 56+26 → 52+25 | 223 → 129 → 166 |
| console | battery | cold | 6.8 → 4.0 → 3.7 | 3.4 → 1.6 → 1.4 | 749 → 329 → 237 | 43+16 → 47+16 → 42+16 | 538 → 486 → 506 |
| console | battery | warm | 4.3 → 2.0 → 2.2 | 1.4 → 0.4 → 0.4 | 173 → 106 → 18 | 43+16 → 47+16 → 42+16 | 251 → 123 → 347 |
| console | settings | cold | 4.9 → 2.8 → 2.5 | 3.4 → 1.7 → 1.3 | 559 → 294 → 199 | 41+13 → 45+13 → 39+12 | 202 → 198 → 197 |
| console | settings | warm | 2.5 → 1.1 → 0.9 | 1.4 → 0.4 → 0.3 | 50 → 95 → 6 | 41+13 → 45+12 → 39+12 | 71 → 93 → 50 |
| pebble | car | cold | 16.3 → 6.4 → 6.2 | 3.6 → 2.2 → 2.1 | 1861 → 675 → 609 | 48+18 → 52+18 → 48+17 | 765 → 785 → 948 |
| pebble | car | warm | 6.7 → 2.8 → 2.4 | 1.8 → 0.9 → 0.9 | 89 → 198 → 129 | 48+18 → 52+18 → 48+17 | 244 → 390 → 355 |
| pebble | drives | cold | 5.1 → 3.4 → 3.2 | 3.7 → 2.3 → 2.2 | 509 → 225 → 158 | 36+12 → 39+12 → 35+12 | 353 → 376 → 350 |
| pebble | drives | warm | 2.8 → 1.6 → 1.4 | 1.8 → 0.9 → 0.9 | 54 → 75 → 6 | 36+12 → 39+12 → 35+12 | 0 → 0 → 0 |
| pebble | charges | cold | 15.9 → 6.5 → 6.2 | 3.7 → 2.2 → 2.1 | 1726 → 608 → 541 | 41+18 → 44+18 → 40+17 | 666 → 602 → 633 |
| pebble | charges | warm | 6.8 → 3.3 → 2.9 | 1.9 → 1.1 → 1.1 | 65 → 197 → 127 | 41+18 → 44+18 → 40+17 | 244 → 219 → 134 |
| pebble | battery | cold | 6.0 → 4.0 → 3.7 | 3.7 → 2.3 → 2.2 | 613 → 256 → 189 | 38+17 → 41+17 → 37+17 | 741 → 683 → 522 |
| pebble | battery | warm | 3.3 → 2.1 → 1.9 | 1.8 → 1.0 → 0.9 | 95 → 84 → 14 | 38+17 → 41+17 → 37+17 | 72 → 82 → 79 |
| pebble | settings | cold | 5.0 → 3.4 → 3.1 | 3.6 → 2.3 → 2.1 | 536 → 237 → 170 | 38+13 → 41+13 → 37+13 | 142 → 325 → 199 |
| pebble | settings | warm | 2.7 → 1.3 → 1.3 | 1.8 → 0.9 → 0.8 | 46 → 76 → 6 | 38+13 → 41+13 → 37+13 | 0 → 0 → 0 |
| manager | car | cold | 18.9 → 8.0 → 7.1 | 4.1 → 1.8 → 1.8 | 2282 → 894 → 896 | 58+18 → 58+18 → 58+17 | 1406 → 2162 → 1808 |
| manager | car | warm | 7.5 → 3.4 → 2.8 | 1.4 → 0.5 → 0.4 | 103 → 131 → 131 | 58+18 → 58+18 → 58+17 | 902 → 1596 → 1338 |
| manager | drives | cold | 8.4 → 4.7 → 4.8 | 4.2 → 1.8 → 1.7 | 1046 → 494 → 488 | 47+12 → 47+12 → 46+12 | 1463 → 1545 → 1606 |
| manager | drives | warm | 3.6 → 2.1 → 2.2 | 1.4 → 0.4 → 0.6 | 88 → 10 → 9 | 47+12 → 47+12 → 46+12 | 898 → 1198 → 1031 |
| manager | charges | cold | 19.3 → 8.6 → 9.1 | 4.1 → 1.8 → 1.8 | 2208 → 875 → 878 | 51+21 → 51+21 → 51+20 | 2944 → 2970 → 3702 |
| manager | charges | warm | 17.5 → 4.4 → 4.4 | 3.6 → 0.5 → 0.4 | 1880 → 154 → 131 | 51+21 → 51+21 → 51+20 | 2436 → 2256 → 2699 |
| manager | battery | cold | 9.8 → 5.9 → 5.8 | 4.2 → 1.7 → 1.7 | 1188 → 517 → 512 | 48+22 → 48+22 → 47+22 | 1914 → 1983 → 1995 |
| manager | battery | warm | 5.2 → 3.4 → 3.5 | 1.6 → 0.6 → 0.6 | 192 → 23 → 23 | 48+22 → 48+22 → 47+22 | 1608 → 1646 → 1853 |
| manager | settings | cold | 7.0 → 3.8 → 3.7 | 4.1 → 1.8 → 1.8 | 954 → 471 → 464 | 45+14 → 45+14 → 43+14 | 591 → 678 → 675 |
| manager | settings | warm | 2.9 → 1.3 → 1.0 | 1.5 → 0.5 → 0.4 | 63 → 9 → 9 | 45+14 → 45+14 → 43+14 | 368 → 379 → 481 |

</details>

Caveats:
- **Warm loads of the map pages, baseline:** in instrument, logbook, horizon and manager, the warm car and
  charges pages fetched some `no-cache` files again in full under network emulation instead of revalidating them.
  MapLibre GL was one of them. Their warm bytes are high for that reason. In step1 a warm load asks for no file under
  `/_v/`. Its few remaining `304`s (0 to 10 per page) were files the pages still named by a root address, which
  step 2 made relative.
- **Duplicate API GETs in the baseline** (all fixed in step 2):
  - `/api/v1/me` twice on the car and charges pages of every pack;
  - logbook battery: `/api/v1/cars/c1/battery` twice;
  - console settings: `/api/v1/cars?limit=200&sort=name` twice.

# Plan: page speed with a long history

Status (2026-11-23): done. On the owner-sized made-up history every page's calls answer within the target warm and
after a restart; on a data folder never worked on before, the first calls of a few pages are slower while the
background pass (below) is still working (statistics 2.5 s, temperatures 1.5 s, once).

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
cores (other builds sometimes ran at the same time: take them as about, ±20 %). "now" is the server after every change
below; the first start ran with the background pass that starts with the server.

## The times (ms)

| Call | origin/main: first start / restart / warm | now: first start / restart / warm |
|---|---:|---:|
| `cars/c1` | 6 / 7 / 6 | 8 / 6 / 6 |
| `cars/c1/now` | 511 / 463 / 386 | 8 / 279 / 6 |
| `cars/c1/activity?period=1d` | 1,260 / 1,205 / 1,189 | 9 / 6 / 4 |
| `cars/c1/series?5 fields&period=1d&max_points=600` | 1,282 / 1,317 / 1,310 | 99 / 78 / 31 |
| `cars/c1/timeline?limit=20` | 1,275 / 1,152 / 1,230 | 9 / 5 / 6 |
| `cars/c1/health` | 96 / 91 / 100 | 72 / 75 / 44 |
| `cars/c1/totals` | 484 / 411 / 440 | 11 / 43 / 5 |
| `cars/c1/statistics?period=month&limit=12` | 12,353 / 11,686 / 10,823 | 2,512 / 561 / 44 |
| `cars/c1/timeline?period=30d&limit=50` | 1,326 / 1,256 / 1,316 | 15 / 7 / 7 |
| `cars/c1/states?period=30d` | 4 / 5 / 4 | 5 / 4 / 5 |
| `cars/c1/activity?period=30d` | 1,340 / 1,215 / 1,221 | 7 / 6 / 7 |
| `cars/c1/drives?limit=50` | 2,045 / 1,896 / 1,663 | 56 / 67 / 47 |
| `cars/c1/drives?period=365d&limit=50` | 1,752 / 1,853 / 1,864 | 54 / 56 / 47 |
| `cars/c1/drive-stats?period=365d` | 1,861 / 1,933 / 1,838 | 75 / 48 / 58 |
| `cars/c1/gaps?limit=50` | 62 / 67 / 49 | 20 / 16 / 15 |
| `cars/c1/charges?limit=50` | 3,573 / 3,169 / 3,325 | 16 / 17 / 17 |
| `cars/c1/charging-stats?period=365d` | 4,394 / 3,416 / 3,491 | 14 / 11 / 11 |
| `charging-stats/places?cars=c1` | 3,586 / 3,255 / 3,354 | 14 / 10 / 11 |
| `cars/c1/charging-stats/dc-curve?period=365d` | 423 / 365 / 434 | 28 / 27 / 9 |
| `cars/c1/battery` | 760 / 458 / 457 | 141 / 47 / 36 |
| `cars/c1/battery/capacity?group=day&x=odometer&limit=500` | 521 / 396 / 436 | 114 / 74 / 70 |
| `cars/c1/battery/capacity?group=month&x=odometer&limit=500` | 520 / 448 / 481 | 60 / 66 / 58 |
| `cars/c1/battery-level?period=365d&bucket=day` | 2,718 / 2,560 / 2,449 | 785 / 32 / 31 |
| `cars/c1/projected-range?period=365d` | 3,210 / 3,061 / 2,882 | 67 / 25 / 23 |
| `cars/c1/parked?limit=12` | 1,692 / 1,469 / 1,577 | 44 / 29 / 25 |
| `cars/c1/settings` | 5 / 5 / 5 | 26 / 16 / 13 |
| `cars/c1/efficiency` | 2,482 / 2,302 / 2,064 | 40 / 33 / 37 |
| `cars/c1/efficiency?period=365d` | 2,425 / 2,344 / 2,310 | 49 / 37 / 38 |
| `cars/c1/temperatures?period=365d` | 2,823 / 2,768 / 2,803 | 1,494 / 18 / 14 |
| `cars/c1/statistics?period=month` | 12,411 / 11,836 / 11,828 | 47 / 47 / 44 |
| `cars/c1/statistics?period=year` | 12,807 / 11,551 / 11,644 | 43 / 45 / 43 |
| `cars/c1/statistics?period=all` | 12,534 / 11,218 / 11,710 | 42 / 45 / 41 |
| `cars/c1/mileage?group=month` | 429 / 468 / 396 | 6 / 6 / 6 |
| `visits?cars=c1&limit=50` | 455 / 443 / 432 | 4 / 5 / 4 |
| `visits/days?cars=c1` | 414 / 396 / 428 | 6 / 6 / 5 |
| `tracks?cars=c1&period=30d&max_points=2000` | 83,333 / 76,860 / 77,282 | 228 / 304 / 193 |
| `cars/c1/updates` | 424 / 419 / 420 | 4 / 5 / 3 |
| `cars/c1/firmware` | 3 / 3 / 3 | 5 / 3 / 4 |
| `cars/c1/firmware/history` | 428 / 414 / 417 | 4 / 4 / 3 |
| `places` | 492 / 399 / 4 | 7 / 11 / 4 |
| `places/suggestions` | 442 / 440 / 397 | 7 / 8 / 8 |
| `cars/c1/storage` | 60 / 32 / 29 | 33 / 33 / 28 |
| `cars/c1/gaps` | 54 / 60 / 63 | 16 / 16 / 15 |
| `cars/c1/series?5 fields&period=1d&max_points=1000` | 1,284 / 1,329 / 1,306 | 45 / 48 / 47 |
| `cars/c1/series?5 fields&period=30d&max_points=1000` | 2,312 / 2,126 / 2,213 | 9 / 11 / 8 |
| `cars/c1/series?5 fields&period=365d&max_points=1000` | 9,831 / 9,206 / 9,458 | 15 / 12 / 11 |
| `cars/c1/series?5 fields&period=all&max_points=1000` | 9,290 / 9,054 / 8,620 | 16 / 15 / 12 |

The budget test (`perf_pages_stay_within_their_budget`, the 4-month history, release, these 4 cores) passes: every
call within 300 ms warm and 2 s cold, and each page's calls sent at once (as the packs send them) all answered within
300 ms warm: car 26, timeline 10, drives 19, charges 7, battery 44, efficiency 10, statistics 11, travel 108,
software 2, places 2, health 18, series 27 ms.

## What was slow, and what changed

1. **Which import shows where, worked out on every request.** Where a TeslaMate and a TeslaFi import cover the same
   time, each car answer works out which one shows (`query/system.rs`, `Sites`), and that counted every position both
   imports have there: on every request of every page (0.1 s on 4 months, about 0.4 s here, and once per drive in
   `/tracks`: 77 s). The db now keeps a **version** of each car's data (live, imported, and the places and settings
   every car shares; in memory, moved on after each write, `db::Db::version`), and the engine keeps such parts while
   the versions they were made at stay (`engine/src/memo.rs`): the shown sites, the live coverage of imported
   history, the import sites' days. Choosing between two imports that tie counts their positions by time only.
2. **The same days read again and again, one at a time.** A day of the time series is a sealed, compressed file (with
   a journal of the minutes written since, for a day written by the minute: no day is ever folded, `Db::settle` is
   not called). Days are now kept opened and unpacked in memory (`db/src/plain.rs`: a quarter of the memory free for
   them, the machine's or a container's, at most 4 GB, the least recently used let go first, looked at again every 3
   s), each with its files' inode, size and change time: a day written again is a new file, so it is read again. A
   day of a snapshot alone is unpacked without the car's lock, a day with a journal under it, and every day is decoded
   outside it, so a page's calls (and the per-day work below) read one car side by side.
3. **Long periods read every sample.** A series of 30 days or more, the battery level, the projected range and the
   temperatures are summed from **parts kept per UTC day** (`query/dayparts.rs`: per hour for the series, per quarter
   hour for the others; in memory and in the key/value store, namespace `day-parts`), each stamped with its day's
   files (and the days beside it), the import sites, the flips, the gap filling switch: a day written again is worked
   out again, the others never. Days not kept are worked out on every core.
4. **Lists that read every row's samples.** A live charge's import-filled values are worked out once and kept (in
   memory and in `charge-fill`; `FILL_VERSION` in `query/charges.rs` takes a new number when the rules change); the
   statistics no longer run the whole `/parked` answer; parked spells' sample reads are kept while their days stay
   the same; `/tracks` reads its drives on every core with one gap filling per car; health keeps its chain gaps while
   the keys' records stay the same.
5. **The battery page decoded logs and waited its turn.** The days' pack readings are worked out ahead
   (`query/pack_logs.rs`: after the start once the engine is idle, every 10 minutes, and when an answer finds a day not
   done), so `/battery`, `/battery/capacity`, `/battery-level`, `/projected-range` and `/efficiency` answer
   without a turn of the answers that decode logs; only `/signals` and `/signals/latest` still take turns
   (`ui/shared/api.js` sends at most two of those at once).
6. **A first page after a start or an import made what it read.** A background pass (`engine/src/warm.rs`) runs the
   long periods' answers for every car (most recently active first) once the logs waiting are decoded, and after an
   import for its cars, so their parts are kept before a page asks.
7. **The pages waited for their slowest call.** Battery and car in every pack (and efficiency, timeline, travel,
   charges, health where they waited on several calls) lay out at once with each block's room held, and fill each
   block when its own answers come. Requests asked twice on one load are asked once.

Left open: `Db::settle` is never called, so a live car's days keep their journals (the first read of such a day
replays it); folding them would make those reads cheaper still, at the cost of packing every finished day again.

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

## CAN logs through the engine, and a car rebuilt whole

**How it is measured.** `PERF_DAYS=30 cargo test --release -p sentry-mate measure_engine -- --ignored --nocapture`
(`crates/sentry-mate/src/perf_engine.rs`): 30 made-up days of SMO logs (2,341 one-minute logs, a frame a second per
message, with the bridge's usual trouble) are put in the store as uploads are; then the engine is started on them
(every log read, recorded and its window of the timeline rebuilt, one after the other as for one car), then the car
is rebuilt whole (`Engine::rebuild`, what a change of the source order does), then the engine is started again with
nothing to do. Memory is the process's highest (`VmHWM`), with one malloc arena (`MALLOC_ARENA_MAX=1`; glibc here,
the image's musl allocator differs).

| 30 days, 2,341 logs | before | now |
|---|---:|---:|
| the logs found at the start | 68.6 s (29.3 ms a log), 53.7 s of CPU | 38.7 s (16.5 ms a log), 25.4 s of CPU |
| the car rebuilt whole | 75.8 s (32.4 ms a log), 63.7 s of CPU | 46.2 s (19.7 ms a log), 34.8 s of CPU |
| started again, nothing to do | 0.02 s | 0.03 s |
| memory at most: the logs / the rebuild | 71 MB / 71 MB | 101 MB / 138 MB |

At the made-up car's 78 logs a day, a year is about 28,500 logs: a rebuild of the car took about 15 minutes and
takes about 9.

**Where a log's 29 ms went.** Reading the log itself (unpack, decode, samples) was under 1 ms of it. The rest was
the same things read again for every log:

| Per log (ms) | before | now |
|---|---:|---:|
| the log's trust record looked up | 3.9 | 1.3 |
| the day's samples read for the window (and once more to merge the log's into them) | 5.6 + part of 1.9 | about 1 |
| the day records read (the log's entry, the window's entries and cuts, the nearest logs) | about 3 | about 1 |
| the car's values now, worked out for the live event after every window | 3.9 | 2.4 |

What changed (nothing stored differently, nothing answered differently):

1. **The trust journal was read whole for every log.** The engine looks up each log's record (`Judge::record`), and
   that read the key's journal from the disk every time (up to 2,048 changes before it is folded). The journal as it
   was last read is kept while its file is the same file of the same size and time (`ingest/src/ledger.rs`,
   `journal_as_read`; a journal only grows or is replaced by the next generation's). The journals of four keys at
   most.
2. **A day record was parsed again for every question about it.** A day's record (every log of the day with its
   facts, as JSON in the sealed key/value store) was read about ten times per log. Its bytes are still read from the
   db every time, but a record whose bytes are the ones last read is not parsed again
   (`engine/src/records.rs`, `day_shared`: 24 days at most; it cannot be out of date, the bytes are compared).
3. **The day's samples were read from the db twice per log.** A day with a journal is opened, unpacked and replayed
   whole for every read. While a log or a car's logs are worked on, the days read last are kept
   (`engine/src/hot.rs`: three days and 32 MB at most), what the engine writes into them is put into the kept days
   as a read would give it back, and a window is answered from them while a key of the days' files
   (`Db::days_stamp`) is still the one taken after the last write. In a debug build (every test) each answer from
   memory is compared with the db's. Let go when the work is done: this is the extra memory in the table.
4. **The build readings of two logs were decoded again for every live event.** The car's values now include the
   recommended tyre pressures, which read the car's build from the newest day's two newest logs; with every log
   that arrived both were unpacked again. A log's own readings are now kept in memory as it was worked on
   (`query/vehicle.rs`), so only the new log is read.

**Measured, and left as it is.**

- **A rebuild's windows are long.** While the logs arrive in order, the window a log changes reaches from the last
  settled point to the log: 10 hours on average here. In a rebuild the car's newest data is still known, so each
  window reaches from there to the end of the history: 17 days on average here (408 hours), and a year's rebuild
  would do more per log than these 30 days did. What costs then is what is written over the whole window for every
  log (the charge curves: 4.6 ms a log, whole days packed again). Rebuilding the windows once per run of logs
  instead of after each log would take most of a rebuild's time away; it is not done here because the timeline it
  gives has to be shown to be the same one (the ids drives and charges keep, the costs carried to them).
- **Skipping a write of charge points that are already stored** was tried (read, compare, skip): reading the day
  to compare cost more than the write (5.7 against 2.0 ms a log). Not kept.
- **A smaller Brotli window for small bodies** (a log's minute of points is packed with a 4 MB window) was tried:
  no gain at the hot level, slower at the warm one. Not kept.
- **Syncs.** A log makes about 18 `fsync`/`fdatasync` calls (12,782 + 6,448 for 1,080 logs: the samples' journal,
  the tables' journals, each record written whole and renamed). On this container's disk a sync takes 40 µs, and
  the 13 s between the wall time and the CPU time above are mostly those. On a disk where a sync takes 5 ms (a
  spinning disk, a pool with synchronous writes) they would be 90 ms a log and all that counts. They are what keeps
  a crash from losing a log's work; fewer of them is a change to that, not a speed-up.
- **Start-up** with every log already worked on is the comparison of the store's logs with the day records: 0.03 s
  for these 30 days.
