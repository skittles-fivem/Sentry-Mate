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
