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

### Pages in milliseconds (bundle, service worker, lazy panels, in-app cache)

**Target:** on the slowed phone, a return visit usable under 500 ms with its first paint under 200 ms; on an
unthrottled desktop, usable under 150 ms warm and under 400 ms cold; in-app navigation under 100 ms.

Measured with the same harness, extended (`measure.js`): the pages are now car, drives, drive, charges, battery,
statistics, Settings: Display and units, and Settings: Integrations (8 per pack, 48 loads per phase);
`--desktop` runs a 1440x900 window with no throttling; `--nav` clicks the pack's own link from the car page to each
other page (a hover and touchstart first) and times the click to the new page's `data-ready`. The same seeded data
folder (2 made-up cars, 30 light days) and day for every run; one run per row (allow a few hundred ms on TTU).

What changed, each its own commit:
1. **Bundled and minified** (`ui/tools/bundle`, esbuild pinned by its lock file, run by `build-binary.sh` in the
   Image workflow, about 5 s): each page an entry at `pages/<id>.js`, the shared code in `core/c-<hash>.js` chunks,
   style sheets and `boot.js` minified, `vendor/` as it was. The server serves them in place of the sources
   (`SENTRY_MATE_UI_BUNDLE` in `build.rs`); a download, an uploaded pack laid over a built-in one, the tests and the
   dev server keep the sources. Instrument's scripts: 1.68 MB of sources, 1.08 MB bundled.
2. **A service worker** (`GET /sw.js`, embedded, `no-cache`): the kept `index.html` at once and refreshed behind,
   `/_v/` files cache first, the last API answers in the worker's memory only (never on disk, never a `no-store`
   one) for a page that can take them; all of it cleared on sign-out and on any change of who is signed in or which
   pack they see.
3. **Below the fold later** (`ui/shared/lazy.js`): charts (and the maps of the car and charges pages) built when
   they come within a screen, in the room they already have; `check.js` scrolls each page through before checking.
4. **In-app pages from memory** (`ui/shared/api.js`, `ui/shared/prefetch.js`, every router): answers kept per tab
   with their `ETag` (a `304` gives the kept one), a link's page module, style sheet and first reads loaded under
   the pointer (hover, touchstart, focus), the main sections' modules on idle 2 s after the first page; a first
   read may show the kept (or the worker's) answer when the network's is not in within 100 ms, the view dimmed
   (`data-stale`) until the fresh one is in and the page built again in place.
5. **The server:** hyper's server sends no `103 Early Hints` (only its client reads them) and browsers dropped
   HTTP/2 push, so the first modules stay named with `modulepreload`; `index.html` is about 3 KB compressed with
   no inline script (a test keeps it under 8 KB).

Means over 6 packs x 8 pages (ms):

| run | phone cold TTU | cold FCP | warm TTU | warm FCP | nav | desktop cold | warm | nav |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| origin/main `4889351` | 5001 | 1593 | 2206 | 440 | 1606 | 439 | 291 | 242 |
| 1 bundled | 4462 | 1274 | 2093 | 405 | 1521 | 415 | 282 | 233 |
| 2 + service worker (on `cf81f68`) | 3125 | 1227 | 1123 | 229 | 844 | 479 | 372 | 264 |
| 3 + below the fold later | 3201 | 1242 | 1182 | 271 | 786 | 492 | 386 | 260 |
| 4 + in-app cache and prefetch | 3186 | 1328 | 1214 | 298 | 503 | 647 | 513 | 140 |
| 4b + fresh answer first within 100 ms, `/me` once | 3204 | 1315 | 1078 | 289 | 507 | 652 | 495 | 145 |
| 4c + idle warming 2 s after the first page | **2842** | 1286 | **783** | 252 | **437** | **415** | **293** | **140** |
| all of it, rebased on origin/main `46e9d0f` | 3191 | 1351 | 1057 | 352 | 573 | 447 | 310 | 158 |
| 6 + one whole-pixel snap per page drawn | 2857 | 1296 | 806 | 265 | 451 | 403 | 288 | 146 |
| 7 + the first page warmed while the session is checked | 2877 | 1292 | **743** | 261 | 452 | **395** | **265** | **136** |

- From the start to the end: phone cold 5.0 → 2.8 s, warm 2.2 → 0.78 s (first paint 0.44 → 0.25 s), in-app
  navigation 1.6 → 0.44 s; desktop cold 439 → 415 ms, warm 291 → 293 ms, navigation 242 → 140 ms. Warm loads now
  ask the network for no pack file and no `index.html` (0 KiB of files on the phone).
- Step 2's run is on a newer origin/main (`cf81f68`, with other sessions' page and map changes): part of its cold
  change is theirs. The last row is the same changes on a still newer origin/main (`46e9d0f`: 84 more commits of
  other sessions, among them the build check on every answer, windowed long lists and tiered prices); warm loads
  there are slower again (783 → 1057 ms on the phone), which was not looked into yet.
- Step 3 made little difference here (the dev server showed the long tasks of a cold car or charges page about
  halved; on the real server's pages the measured TTU did not move beyond noise). Long tasks per cold load overall:
  1142 → 638 ms.
- Tried and dropped, each measured: a static route of the service worker for `/_v/` files (warm loads fetched
  137 KiB again: a route's cache miss skips the worker's keeping), and leaving `/_v/` files to the browser's cache
  while online (warm first paint 1.0 s and 270 KiB: with a worker in place Chrome did not serve them from its cache).
- Step 6: the routers held the whole-pixel snap (`core/snap.js`) while a page was built, but let it go when the
  page's `mount()` returned, before its data was in; every part drawn after, and every resize and font load (which
  did not look at the hold), ran a snap of the whole page: 5 or 6 per load, 30 to 150 ms each on the slowed phone.
  Now one, at `ready()`; maps and charts are never walked into. Step 7: the first page's module, car and first
  reads start with the router (when a note in this browser says it was signed in), not after the session check.
- A static route for `/_v/` files was tried once more, with the page telling the worker every file it loaded:
  slower again (phone warm 1000 ms, cold 3935 ms), dropped.
- **Not met yet:** phone warm 743 ms (target 500) and first paint 261 ms (200); desktop warm 265 ms (150);
  navigation 136 ms on the desktop (100) and 452 ms on the phone. Desktop cold (395 ms) is within its 400. What is left is
  mostly the pages' own drawing (long tasks 408 ms per warm phone load) and the API round trips a warm load still
  waits for (TTU counts the last answer): the next steps would be drawing less on the first frame (`core/snap.js`'s
  whole-document passes, long lists), and not waiting on answers that are only below the fold.

#### Instrument alone: reads sent twice

Since the other packs were archived the runs are Instrument's 8 pages only (8 loads per phase, 7 navigations), on
the same data folder; the baseline is origin/main `768cec29`, which carries other sessions' work since the last row
above (the off-screen first draw, the bug hunt's fixes), so its numbers are not those of the table above.

The harness counts a GET of the same address twice in one load as a duplicate: there were 4 to 10 per cold load
(`/cars/c1` on every page, ten on Battery) and up to 4 per warm load. Each costs a round trip (150 ms) and its bytes
on the phone. Four causes, each its own commit:
1. **The service worker** answered a page's stale-ok read (the second read a first page sends when the network's
   answer is not in within 100 ms) from the network when it had nothing kept: it joins the read of the same address
   under way now (one sent without a tag).
2. **No worker at all** (a first visit) still got that second read: the router asks for it only when a worker
   controls the page.
3. **A page built again** because a fresh answer differed from the worker's read everything once more: the fresh
   answers are warm now (taken at once, like a prefetched one).
4. **Settings: Integrations**' invoices block read each input's overview again right after its section had
   (`follow()` takes `initial`).

| run | phone cold TTU | cold FCP | warm TTU | warm FCP | nav | desktop cold | warm | nav |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| origin/main `768cec29` | 3992 | 1704 | 1037 | 353 | 802 | 607 | 396 | 180 |
| 1 the worker joins the read under way | 4035 | 1650 | 898 | 343 | 678 | 533 | 350 | 177 |
| 2, 4 + no second read without a worker, Integrations once | 3926 | 1653 | 986 | 316 | 703 | 540 | 371 | 175 |
| 3 + a page built again takes the fresh answers | **3774** | 1608 | **937** | 325 | **668** | 579 | **349** | 177 |

- Duplicates per run: cold loads none left (from 4 to 10 each); warm loads 3 to 4 left on Car, Battery and
  Integrations (reads made after the page is drawn, looked at next).
- Long tasks per cold phone load 970 → 741 ms (fewer answers to parse and draw twice).

#### The phone's network for the service worker too: a corrected baseline

The harness slowed the page's network with Chrome's `Network.emulateNetworkConditions`, which does not reach the
service worker's own requests: a warm or in-app read the worker passed to the server cost about 6 ms instead of a
round trip (150 ms) and its bytes at 1.44 Mbit/s. **Every phone warm and nav figure above, and the cold ones, were
measured without the service worker slowed**, so they are too low. The harness now attaches to each service worker as
it starts (`Target.setAutoAttach`, through a debugging port of its own) and sets the same conditions there; the
results say `service_worker_throttled`, and `--no-sw-throttle` gives the old way. It also no longer counts a read the
worker answers from its memory (`X-Sentry-Mate-Stale`) as a duplicate request: those are `memory_api_gets`.

With it, the same pages (Instrument's 8; phone means of 3 runs, desktop one run):

| run | phone cold TTU | cold view ready | cold FCP | warm TTU | warm FCP | nav | desktop cold | warm | nav |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| origin/main `ea7ad752`, worker not slowed (as before) | 3789 | – | 1607 | 905 | 310 | 597 | 571 | 359 | 172 |
| origin/main `ea7ad752`, worker slowed | 8780 | 4618 | 1613 | 2108 | 327 | 1107 | 552 | 341 | 176 |
| + one snap, the session read with set-up | 8669 | 4489 | 1619 | 2122 | 334 | 1041 | 523 | 342 | 162 |

- With the worker slowed a phone page waits for its reads again: warm 0.9 → 2.1 s, nav 0.6 → 1.1 s. The desktop is
  not throttled, so its numbers stand.
- A cold phone TTU (8.8 s) is about 4 s after the view is ready: the other main sections' modules load on idle after
  the first page (about 250 ms each, one after another) and TTU counts the last response. The view itself is ready
  at about 4.6 s.
- Two changes, each its own commit, measured on it: the loading skeleton's copy laid over the page while it is built
  (`data-snap-over`) going away no longer runs a second whole-page snap (long tasks per warm phone load 615 → 532
  ms); and the session check asks for the account and its preferences with `/setup`, not after it. Neither moves the
  phone TTU beyond noise: what is left is the reads a page waits for, next.


#### The request chains, and the files a first visit left out of the service worker

Measured with the worker slowed too (above), on origin/main `d3b8371a` against the same with this step on it: the
phone means of 3 runs, the desktop one run.

The chain of a warm car page (`PERF_TRACE`-style timing of each request, the page's and the worker's own) was not
the API at all: the worker fetched `pages/car.js` (318 → 966 ms), then that module's 10 parts (to 1466 ms) and the
display fonts (to 1.7 s) **from the network**, about 300 KB at 1.44 Mbit/s, and the page's reads (about 340 ms each)
could only start after. On a first visit the page loads those files before the worker answers it, and the worker
kept only the files `index.html` names, so every second visit fetched them again. Each its own commit:
1. **The frame's first reads and Settings' own go out with the session check**: a browser signed in last time asks
   for about, notices (the first page waits for the notices bar) and the fleet count at once, not after `/setup`, me
   and preferences; Settings: Display warms its reads like the car pages do (not the map settings: a page with a map
   takes a warmed answer as fresh, and they change from that page; `map-tiles.test.js` caught it).
2. **A page takes the period its warm-up read with**: the router's warm-up (the first page, a link under the pointer)
   and the mount after it each resolved the period with their own "now", so every read of a period ending now went
   out twice (statistics, series, activity and charges on the car page; the harness did not count them as
   duplicates, their addresses differ by 40 ms).
3. **The worker keeps the files a first visit loaded before it answered**: the page tells it the versioned files it
   loaded, and each one that finishes loading past it later (`PerformanceObserver`), and the worker keeps this
   version's, a few at a time and none twice.
4. **The worker is registered once the first page is ready** (at most 10 s), not at the load event: its first
   fetches had taken the first page's bandwidth (with 1–3 alone, on `ea7ad752`, cold view ready went 4.6 → 6.1 s).

| run | phone cold TTU | cold view ready | cold FCP | warm TTU | warm FCP | nav | desktop cold | warm | nav |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| origin/main `d3b8371a` | 8803 | 4691 | 1708 | 1992 | 276 | 1056 | 534 | 331 | 163 |
| + 1–4 | 8726 | **4373** | 1688 | **1205** | 338 | 1216 | **399** | **309** | **158** |

- Warm phone loads 2.0 → 1.2 s (every page: car 1.9 → 1.1 s, statistics 1.65 → 0.85 s, Settings: Display 1.37 → 0.59 s, Integrations 3.0 → 1.8 s); a warm load fetches
  no file any more. Long tasks per warm phone load 548 → 507 ms. Warm FCP is within noise (one run 400 ms).
- **Nav is slower on one page**: the drive page needs the map library (maplibre, 365 KB with its worker) and is
  ready only once the map is. On main it is fast (about 0.95 s) only when the idle preloads of the main sections
  (2 s after the first page) loaded it before the harness clicked, slow (2.5 s) otherwise (2 of 6 main runs). With
  the warm load a second shorter the harness clicks before that every time: 2.5 s in every run. Without the drive
  page nav is 1070 → 1001 ms.
- The worker's fetches of the files it keeps were not answered by the browser's cache in the harness (its fetches
  showed no disk cache hit, though the page's own second load came all from it): on a first visit those files come
  down twice. Not looked into further.
- The final run is of the commits before the map settings were left out of Settings' warm-up (a read less there).
- **Not met yet:** phone warm 1.2 s (target 500 ms) and FCP 338 ms (200); in-app navigation 1.2 s on the phone and
  158 ms on the desktop (100); desktop warm 309 ms (150). Desktop cold (399 ms) is within its 400.

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

## Scrolling

A report from a 4K desktop (Instrument, Data health, while drives and charges were being rebuilt): the scroll bar
moved for a few seconds, then the page jumped to where it should be. The browser had no time to draw while it
scrolled: the page was busy, or every frame cost too much to paint.

### How it is measured

`node ui/tools/scroll.js` ([ui/tools/README.md](../../ui/tools/README.md#the-scroll-check-scrolljs)):
- **Server:** the dev server with `--rebuild-running --sse-interval-ms 500 --rows-every-s 1`. The rebuild never
  ends, so Data health reads itself again, every featured car gets a `rows` event every second, and live values
  come every 500 ms.
- **Browser:** Chromium through Playwright, headless, drawing in software with no GPU. That is harder on painting
  than most desktops.
- **Screen:** a 4K screen, at 100 % (3840x2160 at scale 1) and at 200 % (1920x1080 at scale 2).
- **Pages:** each pack's Data health, car, drives, charges, statistics, Settings: Server and logbook.
- **Scroll:** 30 mouse-wheel steps of 100 px down, 50 ms apart, then the same back up.
- **What is counted:** the time between animation frames (a page fails with one over 50 ms), long tasks, and a
  DevTools trace of the scroll.
- **Noise:** one run per row. A single frame of 66 to 166 ms on a page that is otherwise at 16 ms is often noise.

Runs, on the same machine the same day:
- **before:** origin/main `48ecb90`;
- **after:** the changes below.

### What was slow

- **Instrument painted the whole window on every frame.**
  - Every panel had a backdrop blur, and the top bar too.
  - The page's glow was a body background fixed to the window, which the browser paints again on each frame of a
    scroll.
  - On Data health at 3840x2160 that was 17 s of raster in a 4-second scroll.
- **Pages were busy on every update while a rebuild ran.**
  - On each change of content `core/snap.js` snapped the whole page to whole pixels (Instrument, Horizon,
    Manager, Logbook). That is 50 to 100 ms on a page of 2,000 elements, and again for each card that loaded.
  - Its ResizeObserver started yet another whole run for each grid that had just been taken off the page.
- **The logbook page (Instrument, Horizon, Manager) was rebuilt in one go on each `rows` event.**
  - Every drive of the period went into one table, about 41,000 elements, in tasks of 1.2 to 1.5 s.
- **Car pages drew the map again on every live value.**
  - The marker and the map's position were redone each time, even when the car had not moved.
  - The Logbook pack's car page laid its whole live column out again, the map's section included.
- **Horizon's sky kept moving under the page.**
  - The floor animates at 30 frames a second and the glow breathes, so the window is drawn again under a scrolling
    page.

### What changed

- **`core/snap.js` (Instrument, Horizon, Manager, Logbook):**
  - After a change of content, only the boxes that hold it are snapped again; rows added at the end of a table are
    snapped on their own.
  - The whole page is snapped again only when such a box changed size by something other than whole pixels.
  - Grids taken off the page are left alone, and a box that is shown or hidden is watched too.
- **Tables (Instrument, Horizon, Manager):**
  - Rows the page already has are drawn 200 at a time, the next 200 as the foot scrolls into view.
  - With `reuseRows` (the logbook, the gaps on Data health), a row that loads again with the same data keeps its
    element, so a reload that changed nothing touches nothing.
- **Data health:** draws its status card and its totals again only when they changed.
- **Car pages:** move the map only when the car moved, turned or changed state.
- **Logbook pack car page and Data health:** swap only the parts that changed (`replaceChanged`,
  `ui/shared/dom.js`).
- **Paint:**
  - No backdrop blur on panels or the top bar; the top bar and Horizon's panels are a little more solid instead.
  - The glow is a fixed layer of its own, painted once.
  - While the page scrolls (`data-scrolling`, a passive listener) the pulsing dot, the car marker's halo and
    Horizon's glow, haze and floor hold still.

### Results

Pages that pass (no frame over 50 ms): **40 of 84 before, 59 of 84 after.** The longest frame, and the longest
long task (`-`: none), in ms:

| Pack | Page | 3840x2160@1 before | after | 1920x1080@2 before | after |
|---|---|---|---|---|---|
| instrument | Data health | 750 (65) | 67 (-) | 183 (53) | 17 (-) |
| instrument | car | 667 (255) | 200 (177) | 383 (269) | 217 (187) |
| instrument | drives | 750 (54) | 83 (-) | 183 (-) | 33 (-) |
| instrument | charges | 900 (-) | 83 (-) | 167 (-) | 17 (-) |
| instrument | statistics | 850 (-) | 67 (-) | 150 (-) | 17 (-) |
| instrument | Settings: Server | 733 (-) | 50 (-) | 233 (-) | 17 (-) |
| instrument | logbook | 2100 (1279) | 50 (-) | 1967 (1309) | 50 (-) |
| horizon | Data health | 250 (88) | 133 (-) | 167 (66) | 50 (-) |
| horizon | logbook | 2017 (1238) | 133 (-) | 2017 (1250) | 50 (-) |
| manager | logbook | 1950 (1305) | 33 (-) | 1900 (1160) | 33 (-) |
| logbook | Data health | 117 (108) | 33 (-) | 117 (54) | 33 (-) |
| logbook | car | 233 (184) | 100 (100) | 250 (222) | 167 (86) |

Every page of every pack at 1920x1080@2 is at most 50 ms after, except these:

| Pack | Page | Longest frame (ms) | Why |
|---|---|---|---|
| horizon, instrument, logbook, manager | car | 100 to 217 | The mock car is driving, so the map moves with it, and the map's WebGL is drawn in software here. |
| console | car | 100 | Console's pages were not changed. |
| console | logbook | 100 | Console's pages were not changed. |
| manager | drives | 67 | One frame. |

Still over 50 ms at 3840x2160@1:
- **Horizon:** every page measured, 100 to 233 ms. The whole window is its sky: several layers that the software
  renderer blends again on every frame of a scroll. With no sky, the same page keeps under 50 ms.
- **Instrument:** its frames now sit at about 50 ms (three 60 Hz frames). The fixed glow layer is about one frame
  of that.
- **Logbook pack drives:** 92 long tasks, the same before and after; not looked into yet.

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


### After that: the two things left

Both of the things left above are now done, for columns logs (what the bridge sends by default):

| An hour of driving (60 logs) | first | after the signals | now |
|---|---:|---:|---:|
| columns: unpack | 354 ms | 385 ms | 249 ms |
| columns: read whole into 3,566 samples | 922 ms (794 ns a frame) | 600 ms | 341 ms (294 ns a frame) |
| candump: read whole | 1,173 ms | 721 ms | 646 ms |

1. **A columns log's frames are made straight from its columns** (`logfmt::read_lines`), without the text line in
   between and its parsing. A frame is made that way only when its key reads as `can<n> <id>` (asked of the
   line parser itself, once per key); any other key, and every line that is not a frame, goes through the text
   as before. `decode_speed.rs`'s `lines_read_straight_from_columns_are_the_lines_parsed_from_text` reads a
   made-up day in both formats and a body of odd keys and lines both ways (at six line limits, stopped early at
   five places): the same lines, the same summary.
2. **The unpacker is `lzma-rust2`**, the one a backup is imported with (no unsafe code without its `optimization`
   feature, which is off), with the same limit on the xz window (64 MiB, xz's level 9; the bridge uses level 6).
   Twice as fast on columns bodies, the same on candump text. `logfmt/tests/unpackers.rs` reads the same logs with
   both unpackers: whole logs at four levels give the same bytes; of 4,222 damaged ones (cut short anywhere, a
   byte changed, bytes after the stream) both refuse the same ones, but for 13 where a byte was changed that the
   unpacker before never looked at (the range coder's first byte, always 0, and its last bytes): it gave the whole
   text, this one refuses the log. No packer writes such a stream, and a stored log cannot come to hold one (it is
   sealed, and was checked against its hash when it came). A log that cannot be unpacked is left out with the
   reason as its text, and that text now is this unpacker's.

The engine's workers (logs of different cars worked on at once) were already sized from the machine: half its
cores, four at most. One car's logs are worked on one after the other by design, so more workers help a server
with several cars catching up at once and nothing else; left as it is.

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

### After that: a rebuild's windows once per run of logs

The long windows above are now rebuilt once per 64 logs (`engine::Options::window_batch`), joined where they meet,
instead of after every log, where that gives the very same timeline: nothing of the timeline is stored yet at or
after the logs (a first start over a full store, a car rebuilt whole) and the car has no API input. A log that
arrives alone, logs that fill a gap before rows already there, and a car with an API input get a window per log as
before. It uses what a crash already left behind: a log's window is noted dirty before its samples are written, and
windows left dirty are rebuilt at the next start.

| 30 days, 2,341 logs | before this | now |
|---|---:|---:|
| the car rebuilt whole | 46.2 s (19.7 ms a log), 34.8 s of CPU | 25.4 s (10.9 ms a log), 17.0 s of CPU |
| the logs found at the start | 38.7 s (16.5 ms a log) | 35.3 s (15.1 ms a log) |
| memory at most: the logs / the rebuild | 101 MB / 138 MB | 100 MB / 114 MB |

A rebuild of this car now takes a third of what it took at the start of this work (75.8 s).

**Why not everywhere.** `crates/sentry-mate/src/window_batch.rs` puts the same made-up logs through two engines,
one rebuilding after every log and one per run, and compares everything stored for the car (drives, charges, state
spans, updates, charge curves, samples, the summary and newest values, the engine's record), after every step and
after a rebuild: all logs at the start; every seventh log and a stretch of two hours filled in later; the second
half before the first. A day runs with the tests; a week at runs of 7, 64 and all logs is run by hand
(`cargo test --release -p sentry-mate windows_rebuilt -- --ignored`). Without the condition above, the week with
its gaps filled later gave one drive with the same times and values and another id: a drive keeps the id of the
piece of it that was stored first, and which piece that is depends on the windows rebuilt on the way. What points
at a drive (its purpose, notes, costs) goes by that id, so there the windows stay one per log.

**Beside an API input it is not the same either, so such a car stays as it was.** Tried by hand with the second
condition taken out: the same logs beside a made-up TeslaMate import, TeslaFi's API and local TeslaMate over the
same days. Two days came out the same; five days, rebuilt whole per run of 7 and of 64 logs, each gave one drive
with the same times and values and another id (a rebuild builds rows from the API's samples ahead of the logs, and
which piece of a drive is stored first depends on the windows on the way). With both conditions out, two days gave
a drive without its start and end positions (which input's position is used is judged over the window). Rebuilding
after every log is deterministic there: two such engines agreed on everything. So a car with TeslaFi or TeslaMate
beside SMO gains nothing from this step; for it to, a drive's id would have to come from the data alone and not
from what was stored before, which is a change to what a rebuild keeps and not a speed-up.

**A rebuild of a period** clears the period first, so the rule above looks only at what starts between its first
log and its end: nothing, and it goes per run, where no drive or charge goes on across the period's end. The same
two engines compare periods in the middle, at the start and at the end of a history, cut through a drive or a
charge, with gaps inside and before them, and over two cars (`a_rebuild_of_a_period_gives_the_same_timeline_*`).
About the middle third of a week: 374 windows after every log (6.7 s) against 12 per run of 64 (2.5 s). A period
whose end is inside a drive or a charge is not the same per run: the part after the end is kept, made a drive or
charge of its own on the way, and the whole one keeps the id of whichever piece was stored first (a week's middle
third that ends 18 minutes into a charge gave the charge its later part's id per run of 64). Such a period still
gets a window per log.

## Start-up and the idle server, on the long history

**How it is measured.** The long made-up history above (211 MB: 2.7 million imported positions, 1.7 million API
samples, 5,695 SMO logs), `serve` started on it with a release build, the time until the web port answers, the
CPU time and memory (`/proc/<pid>/stat`, `status`) until it goes quiet, then over an idle stretch, and the system
calls of a minute idle (`strace -f -c`).

| | |
|---|---:|
| the web port answers | 0.20 to 0.25 s after the process starts (0.24 to 0.28 s of CPU, 58 MB) |
| a start after the history was made (nothing kept on disk yet of what the pages work out ahead) | quiet after about 20 s, 15.5 s of CPU, 968 MB of memory (1,029 MB at most) |
| the next start | quiet after about 5 s, 1.5 s of CPU, 159 MB |
| idle | 0.02 s of CPU in 30 s, 0.10 s in 120 s (under 0.1 % of a core); 7 threads |
| a minute idle, in system calls | 316 `futex`, 382 `epoll_wait`, 140 `statx` (the look for a move coming in, once a second), 129 `read` |

Nothing here needed changing: the vault opens and both ports listen in a quarter of a second whatever the history's
size (the car's tables are read when first asked for), and the loops that run all the time (the look for a move once
a second, the 1-minute sweep of logs waiting for their sidecar, the hourly ones) cost nothing that shows.

One thing to know, from the page-speed work rather than this one: the first start on a long history works out the
pages' long periods ahead and keeps the days it read in memory, up to a share of the machine's free memory (here,
on a 15 GB machine, 968 MB stayed; the limit follows the machine's free memory, `db/src/plain.rs`).

## MQTT

**How it is measured.** `cargo test --release -p mqtt measure_a_sample -- --ignored --nocapture`: a car's newest
values turned into the messages to send, as the publisher does for every `now` the engine gives it.

| | |
|---|---:|
| the first time on a connection | 84 Home Assistant discovery configs and 23 values, 41.8 kB |
| a sample with nothing new | 2.4 µs, no message |
| a sample with the charge level and the time new | 2.8 µs, 2 messages |
| a renamed car (every discovery config again) | 301 µs |

Nothing needed changing: values are sent only when their text changed, the discovery configs only when the car's
name or model did (or on a new connection), and a car's values waiting to be sent replace each other, so a burst of
logs sends the last ones. What costs more than the publishing is working out the car's values for each event (2.4 ms
after every log, above), which the live pages need too.

## The pages while they stay open

**How it is measured.** `node ui/tools/perf/idle.js --pack horizon` (new): a pack's car page in Chromium on the dev
API server, live values every 2 s, left alone. Read from Chromium's own counters: how busy the main thread is
(script, layout, style), the heap after a collection, the DOM nodes and listeners; and with `ps`, the CPU time of
all of the browser's processes, so drawing counts too. The browser here draws without a graphics card (headless,
software): on a machine with one the drawing costs far less, but it is what a small always-on display does.

| The car page, 1440×900, a minute | main thread busy | of it script | all browser processes |
|---|---:|---:|---:|
| Horizon | 9.0 % | 0.7 % | 110 % of a core |
| Instrument | 5.0 % | 1.4 % | 108 % |
| Logbook | 6.7 % | 2.3 % | 8 % |
| Manager | 1.9 % | 0.4 % | 8 % |
| Console | 1.4 % | 0.2 % | 2 % |
| Pebble | 0.6 % | 0.1 % | 2 % |

**The scripts are not the cost.** Clocks, "… ago" texts and live values together are under 2.5 % of the main
thread as script in every pack.

**What costs is two endless animations under glass.** Instrument and Horizon blur what is behind their panels, bars
and map cards (`backdrop-filter`). Two small things on the car page never stop moving: the ring around the "live"
dot in the top bar, and the halo of the car's marker on the map. Each frame they draw, every blurred surface is
blurred again. Measured on Instrument by switching one thing off at a time:

| Instrument's car page | all browser processes |
|---|---:|
| as it is | 110 % of a core |
| without the marker's halo | 40 % |
| without any endless animation | 10 % |
| with the animations, without the blur | 15 % |

Manager has the same two animations and no blur in its default look: 8 %. Horizon without its CSS animations is at
30 % (its floor canvas, drawn at most 30 times a second, is the rest).

Left as it is, for the owner to decide: any fix changes what is seen. The choices are a halo and a ring that pulse
a few times after each new value and then rest, or panels on the car page that do not blur, or nothing (with a
graphics card the cost is small). Taking the blur off the map's own panel alone (the map covers it) was tried and
changed nothing: the cards over the map blur too.

### A page left open: the charts of every live refresh stayed in memory

**How it is measured.** The same tool with the dev server sending list changes too
(`--server-args "--sse-interval-ms 500 --rows-every-s 3"`): Horizon's car page left open for 30 minutes.

| Horizon's car page | at the start | before, after 30 minutes | now, after 10 minutes |
|---|---:|---:|---:|
| heap after a collection | 6.7 MB | 88.2 MB (21.6 MB after the first 5) | 8.7 MB |
| DOM nodes | 2,941 | 142,906 | 2,912 |
| listeners | 193 | 22,917 | 190 |

(Now the nodes stand at 2,912 at every look. The heap still rises slowly, about 0.3 MB in 200 s at a list change
every 3 s: a few kB per refresh, probably the entries of the page's clean-up list, let go when the page is left.)

It grew in a straight line, with every `rows` event (a drive, charge or parked spell started or ended: on a real
server, about every log while the car is awake) and not with the live values. The car page draws its period again
on such an event, charts included, into new boxes; the charts it replaced were never taken down. Each kept its
size observer, its display and theme subscriptions, uPlot's listeners on the window and, through the page's
clean-up list, its whole drawing, until the page was left. In 30 seconds with a list change every second:

| The car page, 30 s, a `rows` event a second | nodes before | nodes now | listeners before | listeners now |
|---|---:|---:|---:|---:|
| Horizon | 3,189 → 6,697 | 2,914 → 2,912 | 237 → 819 | 178 → 178 |
| Instrument | 3,288 → 6,796 | 3,940 → 3,025 | 235 → 805 | 187 → 182 |
| Manager | 3,158 → 6,666 | 2,883 → 2,881 | 234 → 816 | 175 → 175 |
| Logbook | 3,253 → 5,188 | 2,885 → 2,885 | 342 → 972 | 222 → 222 |
| Console | 1,694 → 2,969 | 1,580 → 1,580 | 189 → 609 | 140 → 140 |
| Pebble | 1,747 → 1,747 | (was fine) | 130 → 130 | |

What changed: where a part of a page is swapped for its newly drawn copy (`core/stage.js`, `offstage`), the charts
in the part that goes are taken down first; a chart that is taken down lets go of everything it held
(`core/chart.js` in Horizon, Instrument, Manager and Console); and Logbook's stacked charts, which the page already
took down before drawing them again, let go of their boxes and data then. What is drawn is the same.

**The pages that are built again whole kept every page before them.** Timeline, Charges, Statistics and Trip (in
Console: Charges and Statistics) answer a `rows` event by building the whole page again off screen and swapping it
in (`core/router.js`, `stageView`). The new page kept, for as long as it lived, the function that cleans the old
page up, and with it the old page; that one kept the page before it, and so on: a whole page of nodes more with
every event. Every page of every pack was left open with a `rows` event every second for 24 seconds to find them
(Pebble and Logbook: none), then with one every 8 seconds:

| 90 s, a `rows` event every 8 s | nodes before | nodes now | listeners before | listeners now |
|---|---:|---:|---:|---:|
| Horizon, Timeline | 2,722 → 25,547 | 2,722 → 2,723 | 50 → 171 | 50 → 50 |
| Horizon, Charges | 3,653 → 39,569 | 3,653 → 6,649 | 221 → 1,705 | 221 → 348 |

Now the clean-up of the old page is let go of once it has run, in the four packs' routers. Where the count still
rises once (Charges, Statistics, Trip: by one page) it stays there: three minutes of Horizon's Charges read 6,650
nodes at every look after the first event. Checked the same way after the change: Timeline, Charges, Statistics,
Trip and the car page of Horizon, Instrument and Manager, Charges, Statistics and the car page of Console, and
Logbook's car page.

## The image and the container's start

The image is one static binary on an empty base, so its size is the binary's and its start is the process's.

| | |
|---|---:|
| the binary (a release build here, glibc; the image's is musl) | 62.7 MB, 27.7 MB gzipped |
| of it code | 28.9 MB |
| of it built-in data | 24.3 MB: the six UI packs (11.5 MB once each; the 12.4 MB that are the same file in several packs are in the binary once), their brotli and gzip copies (6.4 MB), the DBC files |
| start: the web port answers | 0.20 to 0.25 s (above) |

Left as it is. The same file in several packs was thought to be in the binary several times (233 of the packs' 694
files are copies: MapLibre, uPlot, the fonts, `ui/shared`); a build that names each file's bytes once came out the
same size, the compiler already keeps one copy. The code is the larger half; link-time optimisation would shrink
it and was taken out on purpose (`Cargo.toml`: 9 minutes per architecture in the image build).
## The Logbook: a long list in a box

The owner's Logbook (`/cars/c1/logbook`, This year) held 2,035 drives and stuttered on a 4K screen: every row of the
period came in one answer and every row was drawn at once. Now the table sits in a box ten rows high whose header
stays while the rows scroll; only the rows in view and a few on either side are in the page (`ui/shared/vlist.js`),
and they load from the API 50 at a time as the box comes to them (by the `next` cursor of the chunk before, else by
`offset`; Jump to date asks with `at_ms`). The purpose filter, the search, the order and the period go to the server;
the totals, the counts per purpose, the continuity check and the rows per rule of the whole period come with every
page; the CSV is the server's, every row (written as it is sent). The server works a period's rows out once (addresses,
places, purposes, the continuity check) and keeps them while the car's data, places, settings, the shown sides of
overlaps, purposes and rules stay the same (`docs/ui-api.md`, `/logbook`). Drives and Charges were checked the same
way and left as they are: they already draw 25 to 50 rows a page (20 to 56 rows in the page with 3,000 drives).

### How it is measured

`ui/tools/perf/measure.js --device laptop --scroll --pages logbook,drives,charges` on the dev server
(`serve.py --drives 3000 --freeze-clock`: car c1 with 3,104 made-up drives, 2,222 of them this year):
- **Laptop:** 1440x900 at 1x, CPU 2x slower, no network throttling (a server on the home network).
- **rows ms:** from navigation start until the first list row was painted; **list ms:** the same from the start of the
  page's first `/logbook` request (the part this change is about: a page's own start-up comes before it).
- **dom rows / nodes:** list rows and elements in the document after the load.
- **fps:** the list (its box, else the page) scrolled 60 px a frame for 180 frames.
- **Before:** origin/main `5424832` (the pages and the dev server's full answer), with only `--drives` added to its dev
  server. **After:** this change. One run each, cold (a new profile) and warm; allow some noise.

### Results (This year, 2,222 drives)

| pack | dom rows before → after | nodes before → after | list ms cold before → after | list ms warm before → after | rows ms cold before → after | fps before → after |
|---|---:|---:|---:|---:|---:|---:|
| Instrument | 2,222 → 16 | 112,150 → 1,493 | 6,452 → 305 | 6,631 → 130 | 6,724 → 982 | 2 → 59 |
| Horizon | 2,222 → 17 | 112,064 → 1,457 | 5,439 → 348 | 6,946 → 146 | 6,415 → 1,184 | 35 → 30 |
| Simple | 2,222 → 17 | 112,040 → 1,433 | 5,207 → 328 | 4,682 → 119 | 6,686 → 1,035 | 12 → 58 |
| Logbook | 102 → 18 | 5,088 → 1,416 | 747 → 419 | 509 → 206 | 1,282 → 1,167 | 60 → 48 |
| Console | 200 → 18 | 8,883 → 1,018 | 372 → 215 | 433 → 83 | 983 → 815 | 60 → 60 |
| Pebble | 60 → 16 | 2,333 → 804 | 255 → 331 | 112 → 69 | 800 → 903 | 60 → 60 |

- The answer of the first rows: 1.14 MB (every row) → 27 KB (50 rows and the period's figures). On the dev server a page
  of a period it keeps answers in about 3 ms; the first one of a period, about 20 ms (200 ms while c1 is driving: the
  dev server works a car's year out again every 5 s while a drive goes on, so a cold load there pays it).
- First rows under 300 ms from the list's request: every pack warm (69 to 206 ms); cold, Console only, the others 305 to
  419 ms, of which about 200 ms is that dev server's working out of the year. Logbook, Console and Pebble drew a part
  of the rows before (102, 200, 60, with Show more), so their cold times move little; the three that drew all 2,222
  went from 5 to 7 s to about 0.3 s.
- Scrolling: Instrument and Simple went from 2 and 12 fps to 58 to 59. Two things made it slow: every row drawn, and
  the pack's whole-pixel snap (`core/snap.js`), which ran over the whole page on every row added; rows moving in and
  out of a window now snap once the scrolling rests (150 ms). Horizon stays at 30 fps: under 8 ms a frame goes to
  script, style and layout, and the rest is drawing its glass panel over the moving background, which it already caps at
  30 frames a second. The Logbook pack scrolls at 48 to 55 fps: about 7 ms a frame goes to style, as its ledger rule for
  every fifth row's line is matched again when a row comes in.
- The pixel checks (`check.js --pixel`) pass on Logbook, Drives and Charges in all six packs at 1440x900, 3840x2160 and
  390x844@3 with `--drives 3000`.

## CAN log checks on a backlog, and "every ounce of hardware"

What the owner saw: the CAN Bridge app said "Verified by Sentry Mate: 7,215 logs, 13,441 waiting for its check"
for hours while a full rebuild ran and a backlog was being sent again (about 23,000 uploads that day).

### How it is measured

`JUDGE_LOGS=20000 cargo test --release -p ingest --test judge_speed measure_backlog -- --ignored --nocapture`
(`crates/ingest/tests/judge_speed.rs`; 600 logs of it run with the tests): one made-up car key, 20,000 signed logs of
16 KB in one chain, sent again out of order through the upload API, 16 at a time as the app does (the log, then its
sidecar); the newest first, so the live chain after it can verify. Beside it, as on the server:

- **the live chain:** a new log every 2 s after the backlog's newest, timed from its upload to `verified`;
- **the app's status calls:** every 0.7 s (within the token's 1,200 calls in 10 minutes), up to 2,000 names: the
  live logs not answered yet, then the backlog's, oldest first;
- **the engine:** a `Judge` of its own over the same store (as `serve` has: the uploads' and the engine's), its
  decode queue looking every judged log's record up, and a full rebuild reading the car's whole book, then every
  log's record in turn, again and again.

4 cores (Xeon 2.1 GHz), a disk where a sync takes about 40 µs. Times are wall time from the first upload.

| 20,000 logs, 4 cores | before | now |
|---|---:|---:|
| every log of the backlog answered `verified` | 849 s (24 logs/s) | 84.5 to 94.7 s (211 to 237 logs/s) |
| a live log's upload (log, then sidecar: judged when the sidecar is in) | | 78 ms median, 190 ms at most |
| a live log, from its upload to `verified` in a status call (calls 0.7 s apart) | 979 ms median, 2.4 s at most | 359 to 431 ms median, 0.8 s at most |
| a status call of up to 2,000 names | 438 ms median, 1.2 s at most | 34 ms median, 95 ms at most |
| the engine's record lookups beside it | 57 a second | 7,430 to 7,970 a second |

With the engine's reads made through the uploads' own judge (a first version of the measurement, lighter: a book read
every 200 ms) the backlog took 124 s before; the engine's reads through the key's lock file are what made it 849 s,
and on the owner's server, with 41,000 logs of that key and a full rebuild, longer still: every status call that
waited more than 2 s for the key answered `pending` for all its names.

### What was slow

1. **One car's uploads took turns for everything.** An arrival held the car key (its lock in memory and its lock
   file) while it read the sidecar, read and hashed the log, checked the signature and wrote its record with a sync:
   one core, one log at a time per car.
2. **Every read held the key and read the whole history.** A status call read every record of the key from disk
   (unsealed and parsed: 41,000 on the owner's server), as did the check of a log sent again (`replaceable`), Data
   health's counts and its "last upload". The engine's lookup of one record read the records file up to that log
   (half of it on average) for every log decoded, and a rebuild read the whole book; with the key held all the
   while, so uploads and status calls waited for them, and they for the uploads.
3. **The uploads' judge and the engine's were strangers.** Two `Judge`s of the same folder in one process took
   turns only through the key's lock file (a blocking `flock` on a blocking thread), each with its own heads.

### What changed

- **Checked on every core, written together** (`ingest/src/judge.rs`, `ingest/src/pool.rs`). An arrival's log and
  sidecar are read (each once: the SHA-256 as the log is unsealed, nothing decoded) and checked against each other,
  signature included, on the judges' own threads (one per core: `available_parallelism`), without holding the key.
  Only the verdict against the pairing (pin, keys trusted before, the owner's overrides) and writing it hold the key,
  and whoever holds it next writes every arrival checked meanwhile: one journal write and one sync for all of them,
  each log's changes still whole in one sealed record (`vault::Records::append_each`, `Ledger::append_groups`). The
  arrivals of one batch are judged in the order they came, each against the head the ones before left, so a batch
  ends exactly as one arrival after the other would. A log or sidecar sent again or packed between its check and its
  writing is checked again: the verdict is of the files as they are.
- **The records in memory, read without the key.** Each key's records are kept in memory once read (all keys
  together at most 250,000 records, about 1 KB each; a key with more is read from its files as before), changed with
  the files when this process writes them, and read without holding the key while the files are the ones they were
  made from (checked on every read: the head file's first bytes and the journal's size and time). A status call,
  the engine's lookups, a whole book, Data health's counts and the missing-log scan no longer wait for an upload,
  nor an upload for them. The owner's command (another process) changing the files is seen at the next read, which
  then reads the files again.
- **One memory per data folder.** Every `Judge` of a folder in the process shares its keys' locks, heads and
  records; the uploads' and the engine's no longer meet at the lock file. (Tests use `Judge::alone` for "another
  process".)
- **The chain fitted from what is stored.** The chain check of a log is still worked out at every status call from
  the stored SHA-256 and index of the log before it (nothing about the chain is stored with a log), so a log whose
  log before it lands, or is sent again and verifies, is answered `verified` at the next call; with the records in
  memory that pass costs a few milliseconds instead of a read of the whole history.
- **A car judged again whole** (a key trusted, newer rules, the sweep of logs whose sidecar never came): the logs are
  read and checked on every core, then judged in name order and written as before (all at once into the next
  generation for a new pin).
- **Judging first.** The judges' threads are theirs alone (a log that just arrived goes before a car judged again
  whole). The engine's decoding, its rebuild jobs and the full rebuild run on threads of their own at nice 10
  (`Engine::background`; compaction stays at 19): they still take every core nothing else wants, but checking an
  upload and answering a page go first.

Nothing is judged differently: the same rules, a changed, cut, unsigned or wrongly signed log still fails. Tests:
arrivals at once (shuffled, sidecar sometimes first, a changed log, another key's signature, an unsigned sidecar) end
with the very records one after the other gives, and the app is told the same; every answer from memory (book, chain,
signers, last arrival, newest usable, counts, status checks, single records, waiting) equals a judge reading only the
files, after arrivals, a sweep, a key trusted, all judged again, and a change by another process.

### The rest of the server's work, measured and left as it is

- **A car's logs through the engine** (`measure_engine`, 30 days, 2,341 logs, the section above): 24.9 s at the start
  and 20.0 s for the car rebuilt whole before, 26.2 s and 21.1 s now (with a test run beside it): the same within the
  noise. That work is one car's timeline, built window after window from what the windows before stored, and almost
  all of it is the day records and samples read and written (decoding a log is under a millisecond of it), so it is
  not spread over cores; several cars are worked on side by side as before (`Options::workers`). What changed for it
  is that it no longer holds back the uploads (its own threads at nice 10, its record lookups from memory).
  (Those were light logs, a frame a second. At the car's real rates decoding is about half a log's time, and it is
  now done on the other cores: the next section.)
- **Compaction** packs a car's finished days one at a time while it holds that car's upload lock, so days of one car
  packed side by side would only wait for each other, and the automatic pass is meant to take a third of a core at
  most. Left as it is.
- **Page answers** that read every trust record of a key on each call now read them from memory: Data health's
  counts, the Connections overview's token rows and Settings' key and token rows (`Judge::arrivals`: the first and
  newest arrival and the logs waiting, instead of a copy of the whole book).

## A full rebuild on every core

Status (2026-11-25). The owner's report: a full rebuild of about 22,500 logs at about 1.1 logs/s ("about 5 h 58 min
left"), the sentry-mate process at about 100 % of one core while the machine (a Ryzen 5 8500G, 6 cores and 12
threads) was 75 % idle, no iowait, 384 MB of memory; and a GPS layout fit reading the same logs at the same time.

**How it is measured.** `PERF_DAYS=15 cargo test --release -p sentry-mate measure_full_rebuild -- --ignored
--nocapture` (`crates/sentry-mate/src/perf_engine.rs`): made-up SMO logs at the car's real rates (synth's
`Realistic`, the bridge's usual trouble, columns format: 15 days, 1,283 one-minute logs, 44.5 MB packed, about 35 KB
a log) seeded once and copied, then on each copy the engine is started (every log decoded), the car rebuilt whole,
and a full rebuild run, with the logs read ahead on 0 threads (as before) and on 3 (this container's 4 cores but
one). `PERF_API=1` adds TeslaFi's API and local TeslaMate over the same days; `PERF_READ_AHEAD=0,3,0,3` the threads
tried. Where a log's time goes is counted by the engine (`EngineStats`: `read_us_total`, `samples_us_total`,
`now_us_total`, besides the windows' `rebuild_ms_total`). Release build, this container's 4 cores (other builds
ran on the machine at times: take the times as ±20 %; the table has the two runs of each).

| 15 days, 1,283 logs | before (read in place) | now (3 threads read ahead) |
|---|---:|---:|
| a full rebuild | 16.9 s, 15.8 s (76, 81 logs/s), 13.4 s of CPU | 8.2 s, 8.2 s (156 logs/s), 13.4 s of CPU |
| the car rebuilt whole | 16.1 s, 15.7 s (79, 82 logs/s) | 9.4 s, 7.9 s (136, 162 logs/s) |
| the logs found at the start | 18.8 s, 17.1 s (68, 75 logs/s) | 12.2 s, 10.2 s (105, 125 logs/s) |
| beside TeslaFi's API and local TeslaMate: a full rebuild | 71.9 s (18 logs/s), 37 s of CPU | 111 s (12 logs/s), 38 s of CPU, one run, disk-bound (see below) |
| beside them: the car rebuilt whole | 162.5 s (7.9 logs/s), 40.8 s of CPU | 84.0 s (15.3 logs/s), 37.9 s of CPU |

**Where a log's time went** (ms, read in place, the car rebuilt whole): reading the log from the store and decoding
it 6.3, its samples written 1.2, the day records and the rest 2.3, its window 1.0 (once per run of 64 logs); 12.5 in
all. So at the car's real rates decoding is half of a log's time, not "under a millisecond" (that was a frame a
second). Beside an API input every log gets its window (see "a rebuild's windows once per run of logs"): 65 ms of
windows and 22 ms of the car's values now per log, on top of the 6.7 of reading. A full rebuild makes about 7.5
syncs a log (21 beside the APIs: `strace -f -e trace=fsync,fdatasync`, counted between the run's start and end the
test prints). The rows beside the APIs, where wall time is twice the CPU time, are bound by those syncs on this
container's shared disk and vary from run to run more than they differ.

**What changed: a car's logs are read and decoded ahead on the other cores** (`crates/engine/src/ahead.rs`,
`engine::Options::read_ahead`). A car's logs are still worked on one after the other (each changes the car's
timeline, and the windows, the samples, the day records and the ids come out of that order), but while one is
worked on, threads read the next ones from the store (unsealed, unpacked) and decode them into samples
(`timeline::read_log_with`: a pure function of the log's bytes, the car's DBC set, the sample parameters and the
car's GPS layout). When a log's turn comes its reading is taken if it was made with the DBC set and GPS layout the
car has then; one made before the car's VIN was seen (another DBC set) or before a layout was applied is not used,
and that log is decoded in place as before. Used by the logs found at the start, a car rebuilt whole and a full
rebuild (`reconcile_car`); a log that arrives alone, and the positions-only rebuild after a GPS layout change, read
in place as before.

- **Cores:** the machine's cores but one (`available_parallelism() - 1`, eight at most; none on one or two cores).
  The threads are the rebuild's own, at its nice 10, so the web side and the uploads go first; on the owner's 12
  threads that is 8.
- **Memory:** at most two logs a thread read ahead of the one worked on (`AHEAD_PER_THREAD`), so at most 3 x 8 = 24
  logs in flight on the owner's machine: each a packed log (about 35 KB here; the store takes 64 MB at most) and a
  minute's samples, and while one is unpacked its text, bounded by the unpack limit (`logfmt::Limits`, 64 MB). In
  practice a few MB in all.
- **The same history:** `crates/sentry-mate/src/window_batch.rs`, `logs_read_ahead_on_other_cores_give_the_same_history`:
  the same made-up logs through an engine that reads each log in place and one with 3 threads reading ahead, at
  runs of 16 logs, and everything stored compared, ids included (drives, charges, states, updates, charge curves,
  samples, the summary and newest values, the engine's record), after the logs at the start (the first logs give
  the car its VIN, so the readings made before were made with another DBC set and are not used), after gaps filled
  later, after the car rebuilt whole and after a full rebuild; 199 of the logs were taken from the threads. A week
  at runs of 64 and of 1, and two and three days beside TeslaFi and local TeslaMate, are run by hand
  (`cargo test --release -p sentry-mate logs_read_ahead -- --ignored`).

**Is 1.1 logs/s what a 4-core box gives?** No. Here one core works a log of the car's real rates in about 12 ms
(80 logs/s) as a full rebuild, and about 30 ms beside an API input over 15 days; 1.1 logs/s is about 900 ms of CPU a
log. Reading ahead takes the decoding off the one core (about twice as fast here), but the rest of a log's work
stays on it, so a server where that rest is what costs gains less. The owner's history differs from these made-up
ones in its length (a year of logs, and a TeslaMate import of about 3 million positions under the same days, with
TeslaFi's API: every log gets its window, and a window reads every input's points over its span) and in its logs'
size. What of a log's work grows with such a history was not measured here (the made-up history at the owner's size
is a release build and a data folder too big for this machine's disk at the time): it is the next thing to measure,
on a copy of a real data folder or `history::Options::big` under a year of logs (`PERF_HISTORY=big`, with
`EngineStats`' counts: reading, samples, values now, windows). Extrapolated from 15 days: 22,500 logs as these would
take a full rebuild of about 2.5 minutes now (5 before), and with an API input, at 12 to 18 logs/s, 20 to 30
minutes, if nothing of a log's work grew with the history.

**A GPS layout fit and a rebuild don't run at once** (`crates/engine/src/gps_fit.rs`, `Engine::rebuild_running`;
docs/rebuilds.md). Both read and unpack the same logs. The fit waits while a full rebuild or a rebuild job runs,
between two of its days or logs, its progress kept and its status saying why, and goes on once none runs; the
rebuild never waits for the fit. Not chosen: the fit reading the frames the rebuild decoded (a rebuild keeps samples
read with one layout, not the raw 8 bytes of the GPS frame every layout is tried on, so that would be a new store of
frames), or one shared pass (a fit's 30 days and a rebuild's whole history are different logs, and the rebuild
would wait for the fit's own work).

# Memory: one car, 23,000 logs, in the image

Status (2026-11-25): measured state by state on made-up data the size of the owner's server; one change made (the
fusing of a long window, below), the rest listed.

The owner asked how much memory the server takes. Their server: one container on TrueNAS, one car, about 22,500 CAN
logs (about 7 GB of data). During a full rebuild they saw 316 MB for the process and 384 MB for the container, one
core busy.

## How it is measured

- **The data:** one made-up car, `dev seed --days 33 --awake-min 270 --start 2026-10-21` (a car that stays awake
  4.5 hours after each use, as in Sentry Mode: 23,141 one-minute logs in columns format, 804 MB, about 700 a day),
  then `dev history --end 2026-11-23 --days 33 --drives 80 --charges 25 --teslafi-days 0 --api-days 0
  --tesla-days 30` (a TeslaMate import over the same days and 30 days of Tesla API samples, 129,072 of them). 930 MB
  in all. The made-up logs are smaller than the owner's (36 KB packed for the busiest minute, the owner's average
  about 311 KB), so what grows with a log's size is estimated below, not measured.
- **The server:** the image as `docker build .` makes it (the static musl binary on an empty image), run as a
  container with the two folders mounted, on this container's 4 cores and 15 GB. The steps through it: the API as the
  pages use it (setup and sign-in, `full-rebuild`, `gps-layout/fit`, `imports`), three pages open in Chromium (the
  car, its Logbook, its Data health), `docker exec ... export`, and the move between two containers
  (`move code`, then a new one started with `SENTRY_MATE_MOVE_FROM` and `SENTRY_MATE_MOVE_CODE`).
- **Uploads:** `crates/moving/examples/test_bridge.rs` (signed candump logs over TLS, chunks, sidecars, as the
  bridge sends them) with a token of its own: one log a minute for 10 minutes (the bridge's pace while the car is
  awake), then a backlog sent 16 at a time as the app does after a time away.
- **The log checks of a long backlog** run in the process of `crates/ingest/tests/judge_speed.rs`
  (`JUDGE_LOGS=22000 JUDGE_LOG_KB=40`, glibc): the upload API stops one token at 1,200 requests in 10 minutes, so a
  backlog of 22,000 can't be sent to a running server in one go.
- **Sampling:** every second, the process's `VmRSS` and `VmHWM` (`/proc/<pid>/status`), its CPU time
  (`/proc/<pid>/stat`), and the container's cgroup. This machine has cgroup v1, so the container's figures are
  `memory.usage_in_bytes` and `memory.max_usage_in_bytes` (cgroup v2, as on TrueNAS SCALE 24.10 and later, calls
  them `memory.current` and `memory.peak`). Both count the page cache: the data folder's files read through the
  kernel's cache, which the kernel gives back when the container needs memory. "Steady" is the median of the last
  two thirds of a state; "peak" the highest sample (and `VmHWM` where a peak was shorter than a second).

## The results

| State | steady RSS | peak RSS | container (cgroup) peak | time, cores |
|---|---:|---:|---:|---|
| first start after the history was made (pages' long periods worked out ahead, once) | 146 MB | 662 MB (first 5 s) | 298 MB | quiet after 7 s |
| just started (a restart) | 155 MB | 228 MB | 193 MB | quiet after 5 s |
| idle, 3 pages open, 30 minutes | 164 MB | 253 MB (an idle compaction at minute 8) | 961 MB, of which 784 MB page cache | 0.14 cores on average (the compaction) |
| uploads at the bridge's pace (a log a minute, 10 minutes) | 164 MB | 164 MB | 125 MB | under 0.01 cores |
| a backlog sent 16 at a time (581 logs before the token's rate limit) | 176 MB | 177 MB | 194 MB | 1.1 cores |
| log checks of a 22,000-log backlog (in the test's process, glibc) | 87 MB | 95 MB | | 106 s (207 logs/s), 1.3 cores |
| the same with glibc's default malloc arenas | 356 MB | 412 MB | | 94 s (233 logs/s), 1.2 cores |
| a full rebuild of the 23,141 logs, before the change below | 316 MB | 3,714 MB (5 s at the end) | 4,515 MB | 35.4 min: 10.9 logs/s, 0.97 cores |
| the same, with the change | 308 MB | 2,637 MB (5 s at the end) | 2,600 MB | 38.2 min with the GPS fit beside it: 10.1 logs/s, 1.06 cores |
| the GPS layout fit over 30 days | 261 MB | 272 MB | 1,067 MB, mostly page cache | 265 s, 0.99 cores |
| the full rebuild and the GPS fit at the same time (the 6.5 minutes they overlap) | 275 MB | 332 MB (393 MB `VmHWM`) | 357 MB | the fit 390 s, 1.64 cores together |
| an export file made (`export > FILE`, 879 MB) | server 241 MB, export process 10 MB | 241 MB | 1,064 MB, mostly page cache | about 4 s |
| a move to a second instance (2,881 files, 1.5 GB), old side | 282 MB | 298 MB | 901 MB, mostly page cache | about 40 s, 0.16 cores |
| the same, new side | 135 MB | 1,035 MB (3 s: its own vault made at start, Argon2id at 1 GiB), 458 MB at the switch | 1,908 MB, mostly page cache | 0.42 cores |
| a TeslaMate import (a made-up year of two cars, 604 MB, 1.89 million rows of the one imported) | 265 MB | 578 MB | 1,189 MB, mostly page cache | about 45 s with the upload, 1.25 cores |
| `dev history`'s first decoding of the 23,141 logs (glibc, for comparison) | 230 to 270 MB | | | 450 s, 0.7 cores |

**Idle doesn't grow.** With the three pages open the process stayed at 153 MB for its first 8 minutes, went to 164 MB
with the idle compaction (44,663 log files packed into their days), and stayed at 164 MB from minute 14 to minute 30.
Ten minutes of uploads after it stayed at 164 MB too.

**The full rebuild's decoding is steady at about 300 MB**, as on the owner's server, and on one core (one car's logs
are worked on one after the other). The one exception is its end, below.

**The image's allocator matters.** The image is a static musl binary: its allocator gives freed memory back and has
no arena per thread. glibc (a build run outside the image, every test) keeps up to eight arenas per core: the same
backlog of log checks took 412 MB with glibc's default and 95 MB with `MALLOC_ARENA_MAX=2`. The live memory there is
under 100 MB. Measure the image, not a glibc build.

**The page cache is not the process.** The container's figure rose to about 1 GB in most states, with the process at
150 to 300 MB: the files the server read (a compaction, a rebuild, an export, a move) stay in the kernel's cache,
charged to the container. The kernel takes that back first when the container nears its limit; it is not a reason
to raise the limit.

## The end of a full rebuild: the car's Tesla API days built as one window

For about 5 seconds at the end of the decoding the process went from 320 MB to 3.7 GB. That step builds the car's
API data into its new history (`Engine::api_build_locked`): every day the car has Tesla API (or TeslaFi, or local
TeslaMate) data for is one span, and it is rebuilt as one window. A window reads every input's samples of its days
(here 33 days of SMO samples, 1.3 million of them at about 850 bytes, about 1.1 GB) and fuses them; the fusing held
them four times over:

1. SMO's fixes are not used when they disagree with the other inputs' (the made-up logs and the made-up API are
   different drives, so they disagree here): its samples were copied whole without their fixes first;
2. the top input's samples were copied again, with their inputs, as they were laid over the input below;
3. and copied once more out of those pairs into the samples given back.

**Changed** (`crates/timeline/src/fuse.rs`, `layered` and `lay`): a distrusted input's fixes are left out as each
sample is copied; the laid samples and their inputs are kept side by side and the two inputs merged by time (both
are sorted, so the order is the stable sort's of before: an input's second before the lower input's at the same
time); the samples given back are the ones laid. The same output, checked by
`the_samples_fused_with_one_copy_are_the_samples_fused_before`, which keeps the code from before and compares the
two on 420 made-up sets of inputs (seconds missing, twice, out of order; SMO's fixes or an API's distrusted, and
not; two and three inputs in several orders, an input with no samples). With the change: 2.6 GB instead of 3.7 GB, the
rest of the rebuild the same.

**What is left there, and why it matters:** the window still holds its samples twice (as read, and as laid), so the
step takes about **100 KB per CAN log inside the car's API days** (two copies of a log's 60 samples), on top of the
300 MB. With 30 days of Tesla API data and 700 logs a day that is the 2.6 GB above; with a year of Tesla API data
beside a year of logs it would be ten times that. A container limit below it gets the server killed at the end of
every full rebuild (and a restart carries the rebuild on into the same step). Two ways on, in order:

- **Build a long API span in pieces** (each window from where the one before it ended, as the windows of logs
  worked on one by one are). Tried here: with pieces of 3 hours, 2 made-up days whose inputs' fixes disagree gave a
  drive without its start and end positions (whether an input's fixes are used is judged over the window, as the
  window-per-run work found, above). Not kept: it changes the history where the inputs disagree. Doing it needs
  that judgement made over a fixed stretch (a day, say) instead of over the window.
- **Fuse in place:** the window's own samples moved instead of copied (`fuse_ordered` taking the samples it may
  keep), which leaves one copy: about 50 KB per log.

## Measured and left as it is

- **The days' plaintext cache** (`db/src/plain.rs`) holds what it reads up to a quarter of the memory free, at most
  4 GB, looked at every 3 s. It is what kept 968 MB after a first start on the long history (above, "Start-up and the
  idle server"). Under a container limit it reads the cgroup v2 limit (`/sys/fs/cgroup/memory.max`) and fits a
  quarter of what is left under it, so it shrinks with the limit. On a cgroup v1 host it does not see a container's
  limit (only the machine's free memory); TrueNAS SCALE 24.10 and later is cgroup v2.
- **The first start after the history was made** (662 MB for 5 s here, on the engine's threads): what the pages
  show worked out ahead once. A restart after it peaked at 228 MB.
- **The judge's book of a car's records** is copied whole for a status call or a rebuild's reconcile (about 15 MB
  for 22,000 logs) and its mirror keeps up to 250,000 records (about 170 MB) of the keys used last: bounded, and
  small beside the above.
- **The bounded caches** checked: the trust journals as last read (4 keys), the day records as last parsed (24 days),
  the hot days of a car being worked on (3 days, 32 MB, though its read takes the 3 days before it checks the size),
  the memo of query answers (512 entries, counted not weighed), the opened packs (2 of up to 32 MB). An upload's
  chunk is buffered whole (16 MiB at most, 16 at once), but a log of 40 KB is one chunk of 40 KB.
- **A decode worker's buffers** (for the logs read ahead on the other cores, "A full rebuild on every core" above,
  which came after these measurements: they are of one log at a time): a log's xz window grows
  with what it unpacks (it is not allocated at the 64 MiB limit), a columns body is held whole about twice over while
  its frames are made, and a log's minute of samples is small. The busiest made-up log is 227 KB unpacked; the
  owner's logs, at about 311 KB packed, are about 2 MB unpacked: **about 10 to 20 MB per worker** at the owner's
  sizes (an estimate from those sizes, not measured). The read-ahead decodes on 8 threads at most (up to 24 logs in
  flight, but a waiting log is only its packed bytes and a minute's samples): about 100 to 200 MB on top of a
  rebuild's 300 MB.
