# Storage: as little disk as possible, nothing lost

Status (2026-11-23): measured, and two rounds of savings are in (the second: "Every input's raw, as small as it
goes", below). Every value, log, signature and imported row
reads exactly as before: nothing here is lossy, and the signed logs are kept byte for byte.

## How it is measured

All of it is made up (no one's car). Two data folders:

- **Long history** (the one `docs/plans/performance.md` times the pages on): `dev seed --days 75 --light` (5,695 small
  SMO logs) and `dev history`: a TeslaMate import of 2,693,072 positions, a TeslaFi import, and TeslaFi's API and
  local TeslaMate as live inputs (1,665,520 samples).
- **Three weeks of SMO uploads at their real size**: `dev seed --days 21` without `--light` (1,598 logs, 34 KB
  each at the median, and their sidecars), decoded by the engine.

```bash
cargo build --release -p sentry-mate --features dev-seed
B=target/release/sentry-mate
$B --data D/data --keys D/keys dev storage              # the table, by category
$B --data D/data --keys D/keys dev storage --compact    # a whole compaction pass first (on a copy)
```

"On disk" is what the file system set aside (whole 4 KiB blocks), which is what fills a disk: 15,142 files of
59.1 MB took 90.1 MB. The same walk feeds the Storage panel (`GET /api/v1/system/storage/detail`).

**The made-up history was not the size of a real one.** `dev history` made floats no backup holds (a speed of
61.38291…, a temperature with 15 digits), which the columns can only store as raw bits: its series took 137 MB,
the folder 217 MB. It now gives each value the precision a backup's columns have (`12.301000`, a whole speed, power
and battery level, `399.00` km, `12.5` degrees; the odometer stays a float with any digits). The same history is
then 90 MB, of which the import's 2.7 million positions are 10 MB. Row counts are unchanged; the pages' timings in
`performance.md` were taken on the larger folder.

## Before and after

### Long history

| category | files before | on disk before | files after | on disk after | change |
|---|---:|---:|---:|---:|---:|
| logs: uploaded xz logs | 5,695 | 28,028,928 | 0 | 0 | into the day packs |
| logs: sidecars | 5,695 | 23,326,720 | 0 | 0 | into the day packs |
| logs: day packs (logs and sidecars) | 0 | 0 | 68 | 23,035,904 | -55 % of the two above |
| logs: check results (ledger) | 5 | 3,588,096 | 5 | 3,584,000 | 0 % |
| time series: TeslaMate import | 330 | 10,399,744 | 31 | 7,602,176 | -27 % |
| time series: local TeslaMate (live) | 330 | 2,879,488 | 31 | 2,273,280 | -21 % |
| time series: TeslaFi (live) | 330 | 1,351,680 | 31 | 581,632 | -57 % |
| time series: CAN logs | 70 | 3,104,768 | 21 | 561,152 | -82 % |
| time series: TeslaFi import | 60 | 245,760 | 3 | 98,304 | -60 % |
| charge curves (all sources) | 737 | 3,444,736 | 65 | 417,792 | -88 % |
| tables (drives, charges, states, updates) | 7 | 3,338,240 | 5 | 512,000 | -85 % |
| records: the engine's | 732 | 5,570,560 | 17 | 172,032 | -97 % |
| records: kept day parts | 990 | 4,182,016 | 2 | 1,642,496 | -61 % |
| records: battery, vehicle | 151 | 618,496 | 4 | 16,384 | -97 % |
| server (car list, places, settings, tokens, vault file) | 10 | 69,632 | 10 | 65,536 | -6 % |
| **total** | **15,142** | **90,148,864** | **293** | **40,562,688** | **-55 %** |

No quarantine, imports' uploaded backups, audit log, accounts, overview tracker, exports or map tiles in these
folders (a `dev` folder has no web side); the panel counts them when they are there.

### Three weeks of SMO uploads at their real size

| category | files before | on disk before | files after | on disk after | change |
|---|---:|---:|---:|---:|---:|
| logs: uploaded xz logs | 1,598 | 58,593,280 | 0 | 0 | into the day packs |
| logs: sidecars | 1,598 | 6,545,408 | 0 | 0 | into the day packs |
| logs: day packs (logs and sidecars) | 0 | 0 | 18 | 55,500,800 | -15 % of the two above |
| logs: check results (ledger) | 5 | 1,146,880 | 5 | 1,142,784 | 0 % |
| time series and charge curves: CAN logs | 37 | 1,302,528 | 28 | 258,048 | -80 % |
| tables | 7 | 106,496 | 5 | 24,576 | -77 % |
| records (engine, day parts, battery, vehicle) | 130 | 1,245,184 | 8 | 126,976 | -90 % |
| everything else | 12 | 98,304 | 12 | 94,208 | |
| **total** | **3,387** | **69,038,080** | **76** | **57,147,392** | **-17 %** |

**What this says about a real server.** With SMO uploading, the signed xz logs are 94 % of the disk and stay that
size: they are never re-encoded, and xz bytes don't pack further. What could be saved around them is saved (the
blocks' slack, the sidecars' repetition); the logs' own 55.0 MB are now 97 % of that folder. An imported history is
small beside them: 2.7 million positions in about 7 MB.

## What changed

All of it is done by **compaction** (`crates/engine/src/compact.rs`): three steps, each on data that is no longer
written. Old data folders read as they are; nothing is converted except by a pass.

1. **Logs: one sealed pack per car and UTC day** (`crates/ingest/src/pack.rs`, `can/<key>/packs/<day>.pack`). The
   logs' bytes go in as they were uploaded (never recompressed); the day's sidecars are compressed together (brotli
   11), so what they share costs once: 1.76 KB each becomes about 0.3 KB. A file in the car's folder always wins over
   the pack's copy, so a log sent again (one that failed its check) works as before. Reads, hashes, upload answers
   and the per-car count are the same before and after.
2. **Time series: journals folded, days packed by month** (`crates/db/src/segment/pack.rs`,
   `<series>/<YYYY-MM>.pack`). `Db::settle` was never called: imported and live days sat in journals at the fast
   compression level. A pass folds every finished day into a cold snapshot (brotli 10), and puts the finished
   months' days into one file a month, each day's compressed columns copied as they are. A write into a packed day
   goes to its journal as before; the day's own file then wins until the next pass. The tables' journals are folded
   too.
3. **Records: compressed, and bundled** (`crates/db/src/kv.rs`). A record was one sealed file of plain JSON: at
   least 4 KiB on disk each. New records are compressed (brotli 4); a pass packs a namespace's records into
   bundles compressed together (brotli 10), split by hash so a bundle unpacks to about 2 MiB however many keys
   there are. A record's own file wins over the bundle; a delete of a bundled key leaves a small file saying so
   until the next pass.

**Safety, the same rule in all three:** the new file is written under a temporary name and synced; it is read
back from the disk, opened, and compared with what it was made from (byte for byte); only then is it renamed into
place, and only then are the files it took in removed. A crash or a restart at any point leaves every value
readable, from the old files or the new one, and the next pass carries on. A pack or bundle that doesn't open is
reported with its name, never read as empty, and nothing is removed for it.

**When it runs:** by itself, first 10 minutes after the server started and then every 6 hours, only while no log
waits to be decoded and no rebuild runs: on a thread of its own at the lowest priority (`nice 19`, which the
system's disk scheduler follows), a small unit at a time with a pause twice as long as the unit took, giving way as
soon as logs arrive. Each unit holds a pass of the data folder's write gate, so an export or a move sees one
moment. It stops before the server stops or switches its data folder. **Compact now** (Settings: Server and
access, admins) runs the same pass without the pauses. On the long history a first pass took 62 s at full speed;
a pass with nothing to do takes well under a second.

**The API:** `GET /api/v1/system/storage/detail` (categories, totals, free space, compaction as it is) and
`POST /api/v1/system/storage/compact` (`docs/ui-api.md`).

## Every input's raw, as small as it goes (second round, 2026-11-23)

The first round packed files together. This one asks of each input what it stores and how small that can be made
with nothing lost. The tables in "Before and after" above are the first round's; the series' numbers there are
this section's "before".

### What each input stores

| input | what is kept | lossless means | done here |
|---|---|---|---|
| SMO CAN logs | the signed xz logs, their sidecars, the decoded series | the uploaded bytes | the logs: nothing, at the floor (see "Looked at and left"); the decoded series: small columns |
| Tesla API (`vehicle_data`, the stream) | parsed values only, in day columns; the answers' text is never written | every value's 64 bits, which fields are there, every time | small columns, days packed together |
| TeslaFi polls | parsed values only (a poll with a `data_id` already seen is skipped) | the same | the same |
| local TeslaMate, MQTT | parsed values only (a sample when a value changed, at most one a second) | the same | the same |
| local TeslaMate, catch-up rows | read like an import into the `teslamate` series; the rows' text is a temporary file, removed after each catch-up | the same | the same |
| imports (TeslaMate dump, TeslaFi CSVs) | parsed values in series and tables, and **the uploaded file, kept until the import is discarded** | values: the same; the file: its bytes | series: the same; the file: packed by columns |

So the only raw text the server keeps is the signed logs and the imports' uploaded files.

### Per input, a car-day

| input | before | after | ratio | CPU to pack a day | a day read cold, before → after |
|---|---:|---:|---:|---|---|
| Tesla API (7,656 samples a day) | 17,040 | 8,056 | 2.1 | 107 → 198 ms | 3.1 → 7.0 ms |
| TeslaFi, live (1 a minute) | 1,512 | 609 | 2.5 | 7 → 17 ms | 0.2 → 0.7 ms |
| local TeslaMate, live (1 every 5 s) | 73,900 | 13,400 | 5.5 | not timed by itself | not timed by itself |
| TeslaMate import or catch-up: its series | 22,808 | 1,447 | 15.8 (see the odometer, below) | 70 → 129 ms | 3.8 → 7.3 ms |
| TeslaFi import: its series | 1,515 | 620 | 2.4 | 7 → 16 ms | 0.2 → 0.6 ms |
| TeslaMate import: the uploaded dump, plain (4,823 positions, 629 charge rows) | 855,762 | 40,944 | 20.9 | 0.4 s | the whole 100 MB dump: 0.14 → 1.4 s |
| TeslaFi import: the uploaded CSVs, plain (233 rows) | 72,040 | 1,262 | 57 | 0.02 s | a month: under 0.1 s |
| an uploaded gzip, xz or zip | as uploaded | as uploaded | 1 | | |
| SMO CAN logs: the decoded series | 7,628 | about 4,450 | 1.7 | 63 → 198 ms | 3.4 → 3.0 ms, a day by itself; in a run not timed |
| SMO CAN logs: the signed logs | not touched | | | | |

Bytes, made-up data. Series: "before" is the first round's form (quick columns, brotli 10, each day by itself);
the "after" of local TeslaMate and of the CAN series is their bytes on disk over their days, the others come from
the measuring example. Times are of one core, taken with other work running: good to about a fifth.

### The stored series

Measured on the long history, now with a made-up Tesla API input (`dev history`, 229,694 samples in 30 days: the
stream's records while driving, `vehicle_data` answers while awake, values with the API's own digits times
1.609344).

**What was wrong.** A finished day used the same quick columns as a day still being written. One value that didn't
fit a scale sent its whole column to raw bits: every Tesla API distance (miles times 1.609344), a power column with
a -0.0 in it, an odometer with any digits. And each day was compressed by itself, starting from nothing.

**What is built** (`crates/db/src/segment/columns.rs`, `pack.rs`):

- **Small columns** for finished days. Per column every encoding is tried and the smallest kept: scales for miles
  (`n / 10^k × 1.609344`) and °F, a scaled column that keeps its few odd values beside it, floats with any digits as
  integer deltas of their bits, runs. What is written is decoded and compared with the points bit for bit; if it ever
  differed, the quick columns are written instead. Days still being written keep the quick columns, byte for byte.
- **brotli 11** instead of 10 for these days.
- **A month's days packed together**, in runs of about 2 MiB of columns, each run one stream (pack version 2).
  Reading one day unpacks its run; the run's other days are kept unpacked with it.

Encryption is as it was: the vault seals the same files.

| series, bytes in the long history | before | after |
|---|---:|---:|
| TeslaMate import | 7,537,475 | 497,756 |
| local TeslaMate (live) | 2,218,375 | 402,934 |
| CAN logs | 521,117 | 302,882 |
| Tesla API (live) | 512,600 | 257,256 |
| TeslaFi (live) | 508,445 | 215,199 |
| TeslaFi import | 92,523 | 38,855 |
| charge curves (all) | 292,113 | 130,608 |
| **all series** | **11,682,648** | **1,845,490 (-84 %)** |

The whole folder on disk: 42,147,840 → 32,317,440 (the signed logs are most of the rest). The current month's days
are small but not yet packed together (the month is not over): 20 of the Tesla API's 30 days here.

**What was tried**, on the Tesla API's days (bytes a car-day; the others follow the same order):

| candidate | bytes | against today |
|---|---:|---:|
| today: quick columns, brotli 10 | 17,040 | |
| brotli 11 (a 16 MiB window gives the same as 4 MiB) | 16,670 | -2.2 % |
| LZMA 9 | 16,736 | -1.8 % |
| the miles and °F scales alone | 9,136 | -46.4 % |
| odd values kept beside a scaled column, alone | 15,707 | -7.8 % |
| bits as integer deltas, alone | 11,573 | -32.1 % |
| byte planes of the bits, alone | 15,663 | -8.1 % |
| change-only runs / deltas of deltas / byte runs, each alone | 16,987 / 17,080 / 17,018 | -0.3 / +0.2 / -0.1 % |
| small columns (all of these), brotli 11 | 8,728 | -48.8 % |
| small columns, LZMA 9 | 8,539 | -49.9 % |
| small columns, the first or the previous day as a brotli dictionary | 8,613 | -49.5 % |
| **built: small columns, runs of at most 2 MiB as one stream, brotli 11** | **8,056** | **-52.7 %** |
| a month's points as one body of columns | 7,574 | -55.6 % |

Left: LZMA (no gain once days are packed together: 8,153 against 8,056; slower to read; one more dependency at run
time). A dictionary: about 1 %. Change-only encoding and deltas of deltas: brotli already packs a column of delta
zeros to almost nothing, so they gain under 0.5 %; they stay as candidates a column may pick. A month as one body:
the smallest, but one day then costs the whole month to decode (77 to 157 ms). Runs of 1 MiB, 512 KiB or none give
1.91, 1.99 and 2.35 MB for all series against 1.85.

**The import's number depends on its odometer.** The made-up odometer climbs evenly with any digits, which the bit
deltas pack well. A car-day of the import's series, today → built:

| the odometer's shape | today | built |
|---|---:|---:|
| as made up (any digits, an even climb) | 22,807 | 1,446 |
| miles with 6 decimals × 1.609344 | 24,817 | 1,427 |
| miles with 1 decimal × 1.609344 | 4,361 | 1,380 |
| km with 2 decimals | 2,241 | 1,409 |
| any digits, the last 20 bits with no pattern | 28,124 | 22,777 |

What a real backup holds is still not known.

**What it costs.** Packing a day: 107 → 198 ms (Tesla API), 70 → 129 ms (import), once, in the background. A first
pass over the long history: 114 s of one core in 220 steps, the car's lock held 3.8 s at most; a pass with nothing
to do: under a millisecond. Reading one day cold: 3.1 → 7.0 ms (Tesla API), 3.8 → 7.3 ms (import), the fields of a
chart 1.2 → 4.1 ms; read again, as before. Reading every point of the folder: 4.0 → 3.5 s (a run is unpacked once
for all its days).

**Old data.** Nothing is converted at once. The compaction pass folds days into the small form, writes the packs'
days again a few at a time, then packs them together. Each step: written under a temporary name, synced, read back
from the disk, opened, compared (points bit for bit, or bytes), renamed. A crash at any step leaves the old file or
the new. A day's level (2 small, 3 and 4 packed together) says it is done. A damaged pack is reported by name and
kept. A late write into a run takes the run apart; the next pass packs it again. On the long history all 5,110,063
stored points were read before and after and compared equal, bit for bit. Day stamps change once, so kept day parts
of those days are worked out again on first use. An older server can't read a folder a newer one has packed (it
says so: an unknown encoding or version); the newer reads everything old.

**How to measure again.**

    cargo run --release -p db --example series_sizes -- D/data D/keys [columns] [stream]
    cargo run --release -p db --example series_sizes -- D/data D/keys odometer teslamate
    cargo run --release -p db --example compact -- D/data D/keys 1 BEFORE_MS 16     # on a copy

Tests: `crates/db/tests/small.rs` (odd values and random days bit for bit, bodies an older server wrote, a folder
an older server left, a crash at every step, a damaged pack).

### The kept uploads of imports

An import keeps the file that was uploaded (`web/imports/<id>/backup`, `backup.1` …) until the import is
discarded. It was stored as it arrived: a plain TeslaMate dump at its full size. Now compaction packs it once the
import is over (`done` or `failed`), and every reader gets the same bytes back.

**Measured** on made-up files (`cargo run --release -p kept --example measure`): a dump of 60 days and 2 cars
(578,727 positions about a second apart, 75,534 charge rows, 102.6 MB of text) and 36 TeslaFi months (255,959
rows, 78.9 MB).

| the upload, packed (lossless) | TeslaMate dump | TeslaFi CSVs | time for the dump |
|---|---:|---:|---|
| as stored before (plain) | 102,691,402 | 78,956,107 | |
| gzip -6 | 21.3 % | 7.7 % | 5 s |
| xz -6 / xz -9e | 12.0 % / 12.0 % | 4.9 % / 4.3 % | 71 s / 262 s |
| brotli 11 (16 MiB window; a 1 GiB window is the same) | 12.3 % | 4.4 % | 318 s |
| rows turned into columns, 4 MiB blocks, xz 6 | 5.0 % | 1.9 % | 7 s |
| **rows turned into columns, 4 MiB blocks, brotli 11 (built)** | **4.8 %** | **1.75 %** | 50 s, compare included |

Blocks of 16 or 64 MiB gain 2 to 3 % more for 4 to 16 times the memory. Reading a packed dump whole takes 1.4 s
for 100 MB (0.14 s before); reading its start 1 ms.

**How** (`crates/kept`). The text is cut into blocks of about 4 MiB at a line's end (the first is 64 KiB, so a
look at the file's start reads little). In a block, runs of rows with the same number of fields (a `COPY` block's
tab-separated rows, a CSV's rows) are turned on their side. A column is stored as numbers when every field's exact
text comes back: whole numbers, decimals and timestamps as differences, with the fraction digits each was written
with; a dictionary for few different texts; else the text. A field that is not canonically written (`\N`, `007`,
`-0`, `1e5`) keeps its text. Everything else (SQL, comments, headers) is kept as it is. Then brotli 11. Each block
is turned back and compared before it is used, and the pack ends with the upload's length and SHA-256. Rows are
never sorted: their order is part of the file. The blocks are sealed by the vault, each by itself, the file's name
bound in.

The 4.8 % rests on one guess: that a dump's odometer is miles with six decimals times 1.609344 (stored then as
the miles; each value is worked out again and compared, else kept as text). Without it the same dump is 6.2 %.
This needs a look at a real backup's numbers.

**Safety**, the rule of the other steps: the pack is written under a temporary name and synced, read back from
the disk, unpacked whole and compared with the upload byte for byte (and by SHA-256), renamed into place, and only
then is the upload removed. A pass stopped half-way through a large dump is carried on from its last whole block.
If both the upload and its pack are there after a crash, the upload wins: it is what is read, and the next pass
compares the pack again before removing it. A pack that doesn't open is reported with its name and its block,
never read as a shorter file, and nothing is removed; the pass then shows as failed with that name, and what it
packed still counts. A pass looks at each pack's start and end; a damaged block in the middle is found when the
pack is next read.

**Who does it.** The web side knows which imports are over, the engine when to pack: the web side registers a
step (`engine::compact::Step`), run last in every pass and by Compact now, a block at a time under the write gate.
Nothing reads an upload once its import is over, and imports not yet over are never packed. A discard asks a pack
under way to stop and waits for it (one block), then removes the folder. Packing costs about half a second a MB:
some 8 minutes of one core at the lowest priority for a dump of 1 GB.

**Left as uploaded: gzip, xz and zip.** Unpacking and packing by columns would save 77 % of a gzip or zip and
60 % of an xz, but the uploaded bytes themselves could not be made again: the server's deflate gives other bytes
than `gzip` from the second byte on, at every level, and its xz other bytes than `xz` after the headers, at every
preset. A recipe that did match would depend on a packer never changing. So these stay byte for byte. (If the
owner would rather keep the unpacked text exactly and let go of the container's bytes, that is a decision, not a
lossless step.)

**The upload can't be dropped.** The import keeps the values of the tables it reads; the upload also holds the
tables and columns it doesn't, the SQL around them, and each value's own text. It can't be made again from what
is stored.

Tests: `crates/kept/tests/packs.rs` (round trips, nasty and random text, every stop and a restart, damage),
`crates/web/tests/imports_kept.rs` (an import checked and imported again from its pack reports the same as from
the upload; a discard during a pack; TeslaFi's several files).

## Tests

- Round trips, byte for byte, on made-up data: `crates/ingest/tests/packs.rs`, `crates/db/tests/packs.rs`,
  `crates/db/tests/kv.rs`.
- Old formats: a folder with no packs, records written before values were compressed
  (`a_folder_written_before_there_were_packs…`, `records_written_before_values_were_compressed…`).
- Crash safety: each step stopped before the rename and before the removal, then a restart; and the whole pass
  stopped after every unit in turn (`crates/engine/src/compact.rs`,
  `a_pass_stopped_anywhere_and_a_restart_leave_every_value_as_it_was`).
- Damage: a flipped byte in a pack or bundle is reported; nothing is removed.
- On the long history, all 4,872,125 stored points were read before and after packing and compared equal.

## Looked at and left

- **The xz logs themselves.** Signed originals: kept as uploaded, and the app keeps writing
  `canbridge-columns-xz/2`. Re-packing them was measured (2026-11-23; nothing stored was changed) on the three
  weeks of made-up logs at their real size: 1,598 logs, 54,905,160 bytes of xz, 361,324,326 bytes unpacked.

  | the unpacked body, packed again (lossless) | against the xz as uploaded | time |
  |---|---:|---|
  | xz -9e, each log (80 logs) | -0.07 % | |
  | brotli 9, each log (232 logs of 3 days) | +7.8 % (larger) | 3.8 s for 52 MB |
  | brotli 11, each log | -5.6 % | 35 s for 52 MB (0.15 s a log) |
  | brotli 11, a day's logs as one stream | -8.2 % | 57 s for 52 MB |
  | xz -9e, a day's logs as one stream (7 days) | 4.8 % under xz -9e of each log | 8 s a day |

  At best about 78 MB a car a year of about 955 MB, for some two hours of one core, and only by packing a day as
  one stream (which the app, signing a log a minute, can't do; and a single log is then read by unpacking its
  day). The day packs save about 167 MB a year on the same logs and keep every signed byte. A real log measured
  before says the same (`rust-can-logger.md`, "Compression, measured on this log": the body at xz -9e is 0.04 %
  smaller; the frame times are 58 % of it and each carries about 9 bits of real jitter; per-message time deltas
  and XOR'd payloads came out larger). **Decided: no re-encoding of stored logs and no new upload format.** A
  stored log could only be re-encoded without losing its signature if the server made the app's xz bytes again
  bit for bit, which needs the app's exact encoder and a real log to try (liblzma at no preset gave back any of
  80 made-up logs, but those are the generator's, not the app's). What would make logs smaller is what the app
  records, which is the app's to decide and not lossless.
- **zstd with trained dictionaries.** The db is pure Rust by choice (brotli was picked by measuring, see
  `crates/db`'s docs); the pure-Rust zstd encoders compress worse than brotli. What a dictionary would give small
  records (shared structure paid once) the bundles and day packs give by compressing them together.
- **The odometer of imported positions** was most of an import's series. The small columns now pack it whatever
  shape it has but one (digits with no pattern at all); see "The import's number depends on its odometer" above.
  What a real backup holds is still not known.
- **The catch-up's temporary dump** (`teslamate-local/catchup.dump`) is plain text on the disk while a catch-up
  runs, then removed. Writing it through the vault would take a few lines; not done here.
- **The ledger of check results** (`trust-*.records`, about 700 bytes a log: 3.6 MB here) is the judge's
  append-only record; not touched.
- **Kept day parts** (1.6 MB after bundling) can be worked out again, but the pages' first calls on a long history
  depend on them (`performance.md`), so they stay on disk.
- **Gateway copies of a signal on two buses, identical consecutive samples:** the stored columns already cost
  these almost nothing (a repeated value is one zero byte before compression, and a day's column of them
  compresses to a few bytes); the copies on two buses exist only inside the xz logs.

## The tables in full (long history)

Before:

| category | files | bytes | on disk |
|---|---:|---:|---:|
| `logs.raw` | 5695 | 21191890 | 28028928 |
| `logs.sidecars` | 5695 | 10053862 | 23326720 |
| `series.teslamate.journals` | 330 | 9721478 | 10399744 |
| `kv.engine` | 732 | 3039455 | 5570560 |
| `kv.day-parts` | 990 | 1803145 | 4182016 |
| `logs.ledger` | 5 | 3571824 | 3588096 |
| `series.can.journals` | 67 | 2691439 | 3067904 |
| `rows.journals` | 4 | 2836468 | 2854912 |
| `series.teslamate-local.snapshots` | 299 | 2012470 | 2555904 |
| `curves.teslamate.journals` | 330 | 118325 | 1351680 |
| `curves.can.snapshots` | 322 | 133868 | 1331200 |
| `series.teslafi-api.snapshots` | 299 | 466896 | 1224704 |
| `curves.can.journals` | 25 | 404126 | 516096 |
| `rows.snapshots` | 3 | 478463 | 483328 |
| `kv.battery` | 82 | 27969 | 335872 |
| `series.teslamate-local.journals` | 31 | 261833 | 323584 |
| `kv.vehicle` | 69 | 34913 | 282624 |
| `curves.teslafi.journals` | 60 | 19014 | 245760 |
| `series.teslafi.journals` | 60 | 104269 | 245760 |
| `series.teslafi-api.journals` | 31 | 54952 | 126976 |
| `server.db` | 4 | 41548 | 57344 |
| `series.can.snapshots` | 3 | 31062 | 36864 |
| `server.tokens` | 2 | 275 | 8192 |
| `server.vault` | 1 | 445 | 4096 |
| `server.other` | 3 | 0 | 0 |
| **total** | 15142 | 59099989 | 90148864 |

After:

| category | files | bytes | on disk |
|---|---:|---:|---:|
| `logs.packs` | 68 | 22901584 | 23035904 |
| `series.teslamate.packs` | 11 | 7061427 | 7081984 |
| `logs.ledger` | 5 | 3571824 | 3584000 |
| `series.teslamate-local.packs` | 11 | 2079469 | 2101248 |
| `kv.day-parts` | 2 | 1634800 | 1642496 |
| `series.teslamate.snapshots` | 20 | 476048 | 520192 |
| `rows.snapshots` | 5 | 495262 | 512000 |
| `series.teslafi-api.packs` | 11 | 476159 | 499712 |
| `series.can.packs` | 2 | 372335 | 376832 |
| `series.can.snapshots` | 19 | 148782 | 184320 |
| `kv.engine` | 17 | 137857 | 172032 |
| `series.teslamate-local.snapshots` | 20 | 138906 | 172032 |
| `curves.can.packs` | 11 | 116105 | 151552 |
| `series.teslafi.packs` | 3 | 92523 | 98304 |
| `curves.teslamate.packs` | 11 | 62322 | 86016 |
| `curves.can.snapshots` | 20 | 19094 | 81920 |
| `curves.teslamate.snapshots` | 20 | 4450 | 81920 |
| `series.teslafi-api.snapshots` | 20 | 32286 | 81920 |
| `server.db` | 4 | 41548 | 53248 |
| `curves.teslafi.packs` | 3 | 10806 | 16384 |
| `kv.battery` | 2 | 3351 | 8192 |
| `kv.vehicle` | 2 | 1598 | 8192 |
| `server.tokens` | 2 | 275 | 8192 |
| `server.vault` | 1 | 445 | 4096 |
| `server.other` | 3 | 0 | 0 |
| **total** | 293 | 39879256 | 40562688 |
