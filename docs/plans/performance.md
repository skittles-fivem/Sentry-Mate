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
