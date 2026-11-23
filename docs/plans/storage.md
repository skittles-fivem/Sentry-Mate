# Storage: as little disk as possible, nothing lost

Status (2026-11-23): measured, and the first round of savings is in. Every value, log, signature and imported row
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

- **The xz logs themselves.** Signed originals: kept as uploaded.
- **zstd with trained dictionaries.** The db is pure Rust by choice (brotli was picked by measuring, see
  `crates/db`'s docs); the pure-Rust zstd encoders compress worse than brotli. What a dictionary would give small
  records (shared structure paid once) the bundles and day packs give by compressing them together.
- **The odometer of imported positions** is now most of an import's series (6.8 of 7.5 MB): a float with any digits
  (kilometres worked out from miles), stored as XOR of bits, 2.6 bytes a value. If a real backup's values turn out
  to be `miles with 6 decimals × 1.609344`, an exact encoding for that would shrink it several times; this needs a
  look at a real backup's numbers (not at TeslaMate's code) and is not done.
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
