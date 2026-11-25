//! Sentry Mate's storage for everything derived from the CAN Bridge's logs (the logs themselves are kept by
//! `ingest`): drives, charges, state spans, updates, 1 Hz samples and charge curves per car; the car list with a
//! summary of each car; places; settings; a key/value store for the other parts; backups.
//!
//! # Any number of cars
//!
//! There is no limit on cars, and thousands must run on a modest server, so:
//!
//! - **A folder per car** (`db/cars/<id>/`), opened when used and kept in a bounded cache of recently used cars
//!   (64 by default, and at most 256 MiB of open tables): nothing loads all cars, and a request about one car reads
//!   only that car's files, only the ones it needs.
//! - **A lock per car**: different cars are read and written at the same time; one car's writes are one at a time.
//! - **Nothing is rewritten whole as it grows**: every table is a sealed snapshot plus an append-only journal of
//!   changes, folded into a new snapshot once the journal is about as big as the state, so a write costs a small
//!   constant factor of its own size however long the history.
//! - **The fleet without opening cars**: the car list and a small summary per car (name, CAN keys, latest Now, last
//!   activity, counts and totals of distance, energy and charge added) are kept up to date as rows change, in one
//!   global table, so listing, searching (name or key, any part) and paging (newest activity first, by cursor)
//!   through thousands of cars touches no car's folder. This summary is the one thing held for every car: about
//!   560 bytes each as stored.
//!
//! Measured with 2,000 synthetic cars (`tests/scale.rs`, release build): reopening the db 8 ms, the whole fleet
//! paged 50 at a time 0.8 ms, a search 0.12 ms, one car's states and samples 0.08 ms, never more than 64 cars open,
//! and listing the fleet opened no car.
//!
//! # Layout (all under the data folder, every file sealed)
//!
//! ```text
//! db/fleet.snap, db/fleet.log                  cars and their summaries
//! db/places.*, db/settings.*                   shared small tables
//! db/kv/                                       the key/value store (names hashed, see kv)
//! db/cars/<id>/drives.* charges.* states.* updates.*   a car's tables (snapshot + journal)
//! db/cars/<id>/seg/<series>.<can|tesla-api|teslamate>/<YYYY-MM-DD>.*     its time series per UTC day (see segment)
//! db/cars/<id>/seg/<series>.<can|tesla-api|teslamate>/<YYYY-MM>.pack     a finished month's days in one file
//! db/cars/<id>/meta                            how far its days are settled
//! ```
//!
//! # Security
//!
//! Everything is sealed with the vault before it touches the disk, temporary files and journals included (labels
//! are the file's path under the data folder, so a file moved elsewhere doesn't open). Nothing is ever written in
//! the clear. Every file carries the format version; a file of another version, or one damaged, is reported with
//! its name, never read as empty. So is a file that is gone where it can be told: a table whose files are missing
//! while the car's summary counts rows in it, a fleet missing while cars' folders are there. Values JSON can't hold
//! (NaN, infinities) are refused when written, not lost when read back. Writes are crash-safe: whole files go to a
//! sealed temporary file (owner-only, like every db file), are synced and renamed; journals drop only a
//! half-written last entry.
//!
//! # Compression
//!
//! Segment days are columns (each field its own column: times as delta-of-delta varints, floats as exact scaled
//! integers where the values allow it, else XOR with the previous value; see [`segment`]), then compressed, then
//! sealed. Measured on realistic synthetic days (1 Hz, values quantized the way the car's DBC factors make them,
//! parked noise taken from a real parked minute), release build, one core:
//!
//! | day | points | columns | brotli q4 | q6 | q9 | **q10** | q11 | deflate 9 | lz4 | xz (lzma-rs) |
//! |---|---|---|---|---|---|---|---|---|---|---|
//! | driving (3 drives, an AC charge) | 19,800 | 614 KB | 85 KB | 82 KB | 81 KB | **73 KB** | 72 KB | 91 KB | 133 KB | 614 KB |
//! | parked, mostly asleep | 3,600 | 94 KB | 4.7 KB | 4.8 KB | 4.7 KB | **4.1 KB** | 4.1 KB | 5.1 KB | 8.5 KB | 94 KB |
//! | parked, awake 24 h | 86,400 | 2.2 MB | 91 KB | 96 KB | 95 KB | **84 KB** | 83 KB | 104 KB | 179 KB | 2.2 MB |
//! | its charge curve | 10,800 | 76 KB | 11.7 KB | 11.7 KB | 11.6 KB | **10.8 KB** | 10.5 KB | 12.3 KB | 22 KB | 76 KB |
//!
//! Encoding, MB of columns a second: brotli q4 63-218, q6 28-87, q9 6-24, q10 2-6, q11 0.3-3; deflate 9 15-70; lz4
//! 950-4,000. Decoding: brotli 160-730, deflate 580-2,300, lz4 1,700-3,900. lzma-rs's encoder stores without
//! compressing. A window of 22 or 24 bits makes no difference (a day fits in 4 MiB). The driving day as JSON is
//! 14 MB (419 KB with brotli 9): the columns matter most; before the two encodings that are exact only where the
//! data allows (a factor found in the data, power worked out from V × I), the same day was 190 KB at q11.
//!
//! Picked: **brotli**, the smallest by a clear margin and fast enough to read. Journal entries (a window written
//! every minute) at q4; snapshots folded while writing at q6; finished days packed by [`Db::settle`] at q10 (10%
//! smaller than q9; a quarter to half a second per day; q11 gains 1% more at 2-4 times the time). A whole driving
//! day reads (unpack and decode, every field) in about 7 ms; the largest possible day (86,400 points, awake all
//! day) in about 25 ms, some 19 ms of it the system handing over the 43 MB its points take in memory; the 4 fields
//! a chart shows of that day read in about 8 ms ([`Db::read_fields`]).
//!
//! Finished months are then packed into one file a series ([`Db::compact_series`], see [`segment`]): the days'
//! compressed bytes as they are, so nothing is unpacked or compressed again and every value reads as before. What
//! it saves is what files cost: each takes whole blocks on disk and carries the vault's overhead, and many days are
//! tiny. Measured on a made-up year of history of one car (8 series, 4.9 million points, every day settled): 1,843
//! files of 11.6 MB taking 16.9 MB on disk become 187 files taking 12.1 MB; the pass took 0.2 s in 34 steps, none
//! holding the car's lock longer than 18 ms; reading every point takes the same 3 s before and after.
//!
//! Finished days are then made as small as they go (2026-11-23, docs/plans/storage.md, "The stored series"): the
//! small columns ([`segment::encode_small`]: scales for values converted from miles and °F, a scaled column that
//! keeps its few odd values beside it, floats with any digits as integer deltas of their bits; brotli 11), and a
//! month's days packed together in runs of about 2 MiB of columns, each run one stream. Every day written again is
//! read back from the disk and compared with its points bit for bit before it replaces anything. Measured on a
//! made-up history of one car (9 series, 5.1 million points): 11.7 MB of series become 1.8 MB; a first pass takes
//! about 2 minutes of one core in steps that hold the car's lock for up to 3 s; reading every point takes 3.5 s
//! (4.0 s before).

mod backup;
mod cars;
mod codec;
mod error;
mod file;
mod fleet;
mod global;
mod journal;
mod kv;
mod logged;
mod plain;
mod readonly;
mod rows;
mod salvage;
pub mod segment;
mod shadow;
mod table;

pub use backup::{BackupReport, RestoreReport};
pub use codec::Level;
pub use error::{Error, Result};
pub use file::{FORMAT_VERSION, allocated};
pub use fleet::{FleetCursor, FleetEntry, FleetPage, FleetQuery, Summary};
pub use kv::{BUNDLE_TARGET, Kv, KvCompacted, KvUsage, MAX_VALUE};
pub use readonly::{DayRead, Reader, TRIES as READ_TRIES};
pub use rows::{Cursor, Item, MAX_PAGE, Order, Page, Query, StateRow, Table, Tally, Totals, UpdateRow};
pub use salvage::{JournalPart, Salvage, SnapshotPart};
pub use segment::{DAY_MS, DayInfo, END_MS, Fields, Point, keeping_days};

use cars::{CarData, OpenCars};
use file::{Ctx, Which};
use fleet::{Fleet, FleetOp};
use global::{PlaceOp, Places, SettingOp, Settings};
use journal::Files;
use logged::LoggedFile;
use model::{Car, CarId, ChargePoint, Now, Place, PlaceId, Sample, Source, Stream};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use table::{Batch, TimeTable};
use vault::Vault;

/// Compresses bytes as the db does (for measurements).
#[doc(hidden)]
pub fn compress(data: &[u8], level: Level) -> Result<Vec<u8>> {
    codec::compress(data, level)
}

#[doc(hidden)]
pub fn decompress(data: &[u8]) -> Result<Vec<u8>> {
    codec::decompress(data)
}

/// Where a simulated crash stops the work (for tests: see [`Db::crash_at`]).
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Crash {
    /// A whole file's sealed temporary copy is written and synced, not yet renamed into place.
    BeforeRename,
    /// A new snapshot is in place; the journal it replaces is not yet removed.
    BeforeJournalRemoved,
    /// A car's table is written; its summary in the fleet is not yet.
    BeforeSummary,
    /// A pack (a month of days, a bundle of key/value records) is in place; the loose files it took in are not yet
    /// removed.
    BeforeLooseRemoved,
    /// A switch to a shadow's car ([`Db::adopt_car`]): the old folder is moved out, the new one not yet in.
    BetweenSwapRenames,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Cars kept open at most (a car in use is never dropped, so a few more can be open for a moment).
    pub max_open_cars: usize,
    /// Bytes of open tables at most, across the open cars.
    pub max_open_bytes: u64,
    /// Bytes of day snapshots kept opened and unpacked at most (days read again soon, by the same page or the next):
    /// a quarter of the memory free for them, up to this (`db/src/plain.rs`); 0 keeps none.
    pub max_cached_bytes: u64,
    /// A journal is folded into its snapshot once it has at least this many bytes (and about as many as the
    /// snapshot).
    pub fold_min_bytes: u64,
    /// Compression: journal entries, snapshots folded while writing, finished days.
    pub hot: Level,
    pub warm: Level,
    pub cold: Level,
    /// Finished days are kept in their smallest form by [`Db::settle`] and [`Db::compact_series`]: the columns of
    /// [`segment::encode_small`], compressed at `small`. `false`: as servers before 2026-11-23 wrote them (the
    /// quick columns at `cold`), for tests of folders they left.
    pub small_columns: bool,
    pub small: Level,
    /// The days of a finished month are packed together, in runs of about this many bytes of columns compressed as
    /// one ([`segment`], "pack"): a day is then read by unpacking its run. 0: each day by itself (and with
    /// `small_columns` off).
    pub group_bytes: usize,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            max_open_cars: 64,
            max_open_bytes: 256 << 20,
            max_cached_bytes: 4 << 30,
            fold_min_bytes: 64 << 10,
            hot: Level::HOT,
            warm: Level::WARM,
            cold: Level::COLD,
            small_columns: true,
            small: Level::SMALL,
            group_bytes: 2 << 20,
        }
    }
}

/// How the db is doing (for health pages and tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stats {
    pub cars: usize,
    /// About how much memory the fleet (every car's record and summary) takes: its size as stored, uncompressed.
    pub fleet_bytes: u64,
    pub open_cars: usize,
    pub open_bytes: u64,
    /// Cars opened since the db was opened.
    pub cars_opened: u64,
    /// Tables read from disk since the db was opened.
    pub tables_read: u64,
}

/// Rows removed by [`Db::delete_window`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Deleted {
    pub drives: usize,
    pub charges: usize,
    pub states: usize,
    pub updates: usize,
}

/// What [`Db::settle`] packed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settled {
    pub tables: usize,
    pub days: usize,
}

/// What a step of [`Db::compact_series`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeriesCompacted {
    /// Days whose journal was folded in, or whose snapshot was packed again at the cold level.
    pub days_folded: usize,
    /// Days taken into (or dropped from) their month's pack.
    pub days_packed: usize,
    /// Days of a month's pack written again in the small form, or together with their neighbours.
    pub days_recoded: usize,
    /// There is more to do: call again.
    pub more: bool,
}

/// What a car's data is at, in this process: each number moves on (to a value never used before) when that part
/// changes. In memory only, starting again at a restart: for caches kept in memory, keyed by what they were made
/// from ([`Db::version`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Version {
    /// The car's live rows and points (SMO, the APIs, local TeslaMate) and its record.
    pub live: u64,
    /// Its imported rows and points (TeslaMate's, TeslaFi's) and its record.
    pub imported: u64,
    /// The places and the server's settings (every car's answers name places).
    pub shared: u64,
}

/// The counters behind [`Version`].
#[derive(Default)]
struct Versions {
    next: AtomicU64,
    cars: Mutex<HashMap<CarId, (u64, u64)>>,
    shared: AtomicU64,
}

impl Versions {
    fn bump(&self, car: CarId, live: bool, imported: bool) {
        let n = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let mut m = self.cars.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let e = m.entry(car).or_default();
        if live {
            e.0 = n;
        }
        if imported {
            e.1 = n;
        }
    }

    fn bump_shared(&self) {
        let n = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.shared.store(n, Ordering::Relaxed);
    }

    fn of(&self, car: CarId) -> Version {
        let m = self.cars.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (live, imported) = m.get(&car).copied().unwrap_or_default();
        Version { live, imported, shared: self.shared.load(Ordering::Relaxed) }
    }
}

struct Inner {
    versions: Versions,
    ctx: Arc<Ctx>,
    dir: PathBuf,
    fleet: Mutex<LoggedFile<Fleet>>,
    places: Mutex<Option<LoggedFile<Places>>>,
    settings: Mutex<Option<LoggedFile<Settings>>>,
    kv: Kv,
    cars: OpenCars,
    tables_read: AtomicU64,
    /// The cars' files found damaged since the db was opened ([`Db::damaged`]).
    damaged: Mutex<BTreeMap<String, DamagedFile>>,
}

/// A file of a car that was found damaged when something read it ([`Db::damaged`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DamagedFile {
    pub car: CarId,
    /// Its path under the data folder (`db/cars/7/drives.snap`).
    pub file: String,
    pub why: String,
    /// When it was first found so (ms since 1970).
    pub since_ms: u64,
    /// Whether everything in it is made again from the stored CAN logs (a series of decoded samples or charge
    /// points: a rebuild of the car replaces it), as opposed to a table or an import's or an API's series, which
    /// also hold what no log gives.
    pub from_logs: bool,
}

/// The db. Cheap to clone; share it between threads.
#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

/// A file (its path under the data folder) of a car's series that is made from the stored CAN logs
/// (`seg/<series>.can`): its series folder, its name and the time it covers.
fn can_series_file(car: CarId, file: &str) -> Option<(&str, &str, (u64, u64))> {
    let rest = file.strip_prefix("db/cars/")?.strip_prefix(&car.to_string())?.strip_prefix("/seg/")?;
    let (series, name) = rest.split_once('/')?;
    let can = series.rsplit_once('.').is_some_and(|(_, stream)| stream == Stream::Smo.dir_name());
    segment::file_span(name).filter(|_| can).map(|span| (series, name, span))
}

fn lock<T>(m: &Mutex<T>) -> Result<MutexGuard<'_, T>> {
    m.lock().map_err(|_| Error::invalid("a lock was left by a crashed thread; restart Sentry Mate"))
}

/// The last row with each id (a batch naming an id twice keeps the last).
fn dedup<T: Item>(rows: &[T]) -> Vec<T> {
    let mut last: HashMap<u64, usize> = HashMap::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        last.insert(r.id(), i);
    }
    rows.iter().enumerate().filter(|(i, r)| last.get(&r.id()) == Some(i)).map(|(_, r)| r.clone()).collect()
}

fn check_rows<T: Item>(car: CarId, rows: &[T]) -> Result<()> {
    if let Some(r) = rows.iter().find(|r| r.car() != car) {
        return Err(Error::invalid(format!("row {} of {} is for car {}, not {car}", r.id(), T::TABLE.name(), r.car())));
    }
    if let Some(r) = rows.iter().find(|r| !storable(*r)) {
        return Err(Error::invalid(format!(
            "row {} of {} has a value that can't be kept (a number that isn't finite)",
            r.id(),
            T::TABLE.name()
        )));
    }
    Ok(())
}

/// Whether a value comes back from its stored form as it is. JSON has no NaN or infinity: serde writes them as
/// `null`, which reads back as `None` (a value silently lost) or, for a plain `f64` such as a position's
/// latitude, doesn't read at all, and the whole file would then be reported damaged.
fn storable<T: serde::Serialize + serde::de::DeserializeOwned + PartialEq>(v: &T) -> bool {
    serde_json::to_vec(v).ok().and_then(|b| serde_json::from_slice::<T>(&b).ok()).is_some_and(|back| back == *v)
}

fn check_storable<T: serde::Serialize + serde::de::DeserializeOwned + PartialEq>(v: &T, what: &str) -> Result<()> {
    match storable(v) {
        true => Ok(()),
        false => Err(Error::invalid(format!("{what} has a value that can't be kept (a number that isn't finite)"))),
    }
}

impl Db {
    /// Opens (or starts) the db in `<data>/db`.
    pub fn open(data: &Path, vault: Arc<Vault>, opts: Options) -> Result<Db> {
        let dir = data.join("db");
        std::fs::create_dir_all(dir.join("cars"))?;
        let cars = OpenCars::new(opts.max_open_cars, opts.max_open_bytes);
        let ctx = Arc::new(Ctx::new(vault, data.to_path_buf(), opts));
        // a switch to a shadow's car a crash cut off (before the trash goes: the old folder waits there)
        shadow::finish_adopt(&ctx, &dir)?;
        // cars removed before a crash finished deleting them
        let trash = dir.join("trash");
        if trash.exists() {
            std::fs::remove_dir_all(&trash)?;
        }
        let files = Files::named(&dir, "fleet");
        if !files.snap.exists() && !files.log.exists() && std::fs::read_dir(dir.join("cars"))?.next().is_some() {
            // opened as empty, the next car made would get the first car's id, and its folder
            return Err(Error::Damaged {
                file: ctx.label(&files.snap),
                why: "is missing, but there are cars' folders: put it back (or restore a backup)".into(),
            });
        }
        let fleet = LoggedFile::<Fleet>::load(&ctx, files)?;
        let kv = Kv::new(ctx.clone(), dir.join("kv"));
        Ok(Db {
            inner: Arc::new(Inner {
                versions: Versions::default(),
                ctx,
                dir,
                fleet: Mutex::new(fleet),
                places: Mutex::new(None),
                settings: Mutex::new(None),
                kv,
                cars,
                tables_read: AtomicU64::new(0),
                damaged: Mutex::new(BTreeMap::new()),
            }),
        })
    }

    /// What a car's data is at now (see [`Version`]).
    pub fn version(&self, car: CarId) -> Version {
        self.inner.versions.of(car)
    }

    /// Makes the next matching step fail as a crash would (tests only).
    #[doc(hidden)]
    pub fn crash_at(&self, at: Option<Crash>) {
        self.inner.ctx.set_crash(at);
    }

    pub fn stats(&self) -> Stats {
        let (open_cars, open_bytes, cars_opened) = self.inner.cars.stats();
        let (cars, fleet_bytes) = self.inner.fleet.lock().map_or((0, 0), |f| (f.state.len(), f.weight()));
        Stats {
            cars,
            fleet_bytes,
            open_cars,
            open_bytes,
            cars_opened,
            tables_read: self.inner.tables_read.load(Ordering::Relaxed),
        }
    }

    fn car_dir(&self, car: CarId) -> PathBuf {
        self.inner.dir.join("cars").join(car.to_string())
    }

    // ---- cars and the fleet ----

    fn check_keys(f: &Fleet, car: &Car) -> Result<()> {
        for k in &car.vehicle_keys {
            if let Some(other) = f.car_for_key(k).filter(|&o| o != car.id) {
                return Err(Error::KeyTaken { key: k.clone(), car: other });
            }
        }
        Ok(())
    }

    /// Adds a car; its id is given here (never reused). Returns the car as kept.
    pub fn create_car(&self, mut car: Car) -> Result<Car> {
        check_storable(&car, "the car")?;
        let mut f = lock(&self.inner.fleet)?;
        car.id = f.state.next_id;
        Db::check_keys(&f.state, &car)?;
        // a new car's folder can't be there already: if it is, the fleet lost cars and the new one would get
        // another car's history
        let dir = self.car_dir(car.id);
        if dir.exists() {
            return Err(Error::Damaged {
                file: self.inner.ctx.label(&dir),
                why: "is there already for a new car (the fleet's files lost cars?)".into(),
            });
        }
        let e = FleetEntry { car: car.clone(), summary: Summary::default() };
        f.commit(&self.inner.ctx, FleetOp::Put(Box::new(e)))?;
        Ok(car)
    }

    /// Changes a car's record (name, keys, settings); its summary stays.
    pub fn update_car(&self, car: Car) -> Result<()> {
        check_storable(&car, "the car")?;
        let mut f = lock(&self.inner.fleet)?;
        let summary = f.state.get(car.id).ok_or(Error::NoSuchCar(car.id))?.summary.clone();
        Db::check_keys(&f.state, &car)?;
        let id = car.id;
        f.commit(&self.inner.ctx, FleetOp::Put(Box::new(FleetEntry { car, summary })))?;
        self.inner.versions.bump(id, true, true);
        Ok(())
    }

    /// A keyed hash of `text` for `purpose` (hex), with a key made from the vault's (the same after a restart or a
    /// move): to tell values apart without keeping them, such as two cars' whole VINs.
    pub fn keyed_tag(&self, purpose: &[u8], text: &str) -> String {
        use hmac::{Hmac, Mac};
        let key = self.inner.ctx.vault.derive_key(purpose);
        let Ok(mut m) = <Hmac<sha2::Sha256> as Mac>::new_from_slice(&*key) else { return String::new() };
        m.update(text.as_bytes());
        m.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn car(&self, id: CarId) -> Result<Option<Car>> {
        Ok(lock(&self.inner.fleet)?.state.get(id).map(|e| e.car.clone()))
    }

    /// A car and its summary.
    pub fn entry(&self, id: CarId) -> Result<Option<FleetEntry>> {
        Ok(lock(&self.inner.fleet)?.state.get(id).cloned())
    }

    /// The car a CAN Bridge key belongs to.
    pub fn car_for_key(&self, key: &str) -> Result<Option<CarId>> {
        Ok(lock(&self.inner.fleet)?.state.car_for_key(key))
    }

    /// Cars, most recently active first, searched and paged (see [`FleetQuery`]). Opens no car.
    pub fn fleet(&self, q: &FleetQuery) -> Result<FleetPage> {
        Ok(lock(&self.inner.fleet)?.state.query(q))
    }

    /// Removes a car and everything kept for it.
    pub fn remove_car(&self, id: CarId) -> Result<bool> {
        let slot = self.inner.cars.get(id);
        let mut data = slot.lock();
        {
            let mut f = lock(&self.inner.fleet)?;
            if !f.state.contains(id) {
                return Ok(false);
            }
            f.commit(&self.inner.ctx, FleetOp::Remove(id))?;
        }
        data.unload();
        data.removed = true;
        let dir = self.car_dir(id);
        if dir.exists() {
            let mut rnd = [0u8; 8];
            getrandom::fill(&mut rnd).map_err(|e| Error::invalid(e.to_string()))?;
            let trash = self.inner.dir.join("trash");
            std::fs::create_dir_all(&trash)?;
            let to = trash.join(format!("{id}-{}", hex::encode(rnd)));
            std::fs::rename(&dir, &to)?;
            std::fs::remove_dir_all(&to)?;
        }
        drop(data);
        self.inner.cars.forget(id);
        self.inner.versions.bump(id, true, true);
        Ok(true)
    }

    /// Keeps a car's newest values (older ones than those kept are ignored).
    pub fn set_now(&self, now: Now) -> Result<()> {
        check_storable(&now, "the car's Now")?;
        let mut f = lock(&self.inner.fleet)?;
        let mut e = f.state.get(now.car).ok_or(Error::NoSuchCar(now.car))?.clone();
        if e.summary.now.as_ref().is_some_and(|n| n.at_ms > now.at_ms) {
            return Ok(());
        }
        e.summary.last_activity_ms = e.summary.last_activity_ms.max(Some(now.at_ms));
        e.summary.now = Some(now);
        f.commit(&self.inner.ctx, FleetOp::Put(Box::new(e)))
    }

    /// Adds `delta` to a car's summary and moves its last activity forward to `activity`.
    fn bump(&self, car: CarId, delta: &Tally, activity: Option<u64>) -> Result<()> {
        self.inner.ctx.crash_point(Crash::BeforeSummary)?;
        let mut f = lock(&self.inner.fleet)?;
        let Some(old) = f.state.get(car) else { return Err(Error::NoSuchCar(car)) };
        let newer = activity.is_some_and(|a| old.summary.last_activity_ms.is_none_or(|l| a > l));
        if delta.is_zero() && !newer {
            return Ok(());
        }
        let mut e = old.clone();
        e.summary.tally.add(delta, 1);
        if newer {
            e.summary.last_activity_ms = activity;
        }
        f.commit(&self.inner.ctx, FleetOp::Put(Box::new(e)))
    }

    /// Runs `f` on a car under its lock. A failed write forgets the car's memory, so the next use reads the disk.
    fn with_car<R>(&self, car: CarId, f: impl FnOnce(&Db, &mut CarData, &Path) -> Result<R>) -> Result<R> {
        let slot = self.inner.cars.get(car);
        let mut data = slot.lock();
        if data.removed || !lock(&self.inner.fleet)?.state.contains(car) {
            return Err(Error::NoSuchCar(car));
        }
        let dir = self.car_dir(car);
        let r = f(self, &mut data, &dir);
        if let Err(e) = &r {
            data.unload();
            self.note_damage(car, e);
        }
        slot.set_weight(data.weight());
        drop(data);
        drop(slot);
        self.inner.cars.trim();
        r
    }

    /// A table of a car, read the first time; its summary is checked against it then (and mended if a crash came
    /// between a write and its summary).
    fn table<'a, T: Item>(
        &self,
        data: &'a mut CarData,
        car: CarId,
        dir: &Path,
    ) -> Result<&'a mut LoggedFile<TimeTable<T>>> {
        let was = data.loaded(T::TABLE);
        let (t, _) = data.table::<T>(&self.inner.ctx, dir)?;
        if !was {
            self.inner.tables_read.fetch_add(1, Ordering::Relaxed);
            let mut part = Tally::default();
            let mut last = None;
            for r in t.state.all() {
                r.tally(&mut part, 1);
                last = last.max(Some(r.end_ms()));
            }
            // no file at all, though the summary counts rows: the files were lost, not emptied (emptying a table
            // leaves its files). Reported, never mended to zero: that would hide the loss for good. (A car whose
            // whole folder is gone, as after a backup that caught it being removed, is left to be mended.)
            let lost = t.state.len() == 0 && !t.files.snap.exists() && !t.files.log.exists() && dir.exists();
            let mut f = lock(&self.inner.fleet)?;
            if let Some(e) = f.state.get(car) {
                let have = e.summary.tally.part(T::TABLE);
                let counted = have.rows(T::TABLE);
                if lost && counted > 0 {
                    return Err(Error::Damaged {
                        file: self.inner.ctx.label(&t.files.snap),
                        why: format!("is missing, but the car's summary counts {counted} rows in it"),
                    });
                }
                let late = last.is_some_and(|l| e.summary.last_activity_ms.is_none_or(|a| l > a));
                if have != part || late {
                    let mut e = e.clone();
                    e.summary.tally.add(&have, -1);
                    e.summary.tally.add(&part, 1);
                    e.summary.last_activity_ms = e.summary.last_activity_ms.max(last);
                    f.commit(&self.inner.ctx, FleetOp::Put(Box::new(e)))?;
                }
            }
        }
        Ok(t)
    }

    // ---- a car's tables ----

    /// Adds or replaces rows (by id), all or none.
    pub fn upsert<T: Item>(&self, car: CarId, rows: &[T]) -> Result<()> {
        check_rows(car, rows)?;
        let rows = dedup(rows);
        if rows.is_empty() {
            return Ok(());
        }
        let imported = rows.iter().any(|r| r.source().is_imported());
        let live = rows.iter().any(|r| !r.source().is_imported());
        let r = self.with_car(car, |db, data, dir| {
            let t = db.table::<T>(data, car, dir)?;
            let mut delta = Tally::default();
            let mut last = None;
            for r in &rows {
                if let Some(old) = t.state.get(r.id()) {
                    old.tally(&mut delta, -1);
                }
                r.tally(&mut delta, 1);
                last = last.max(Some(r.end_ms()));
            }
            t.commit(&db.inner.ctx, Batch { del: Vec::new(), put: rows })?;
            let w = t.weight();
            data.set_weight(T::TABLE, w);
            db.bump(car, &delta, last)
        });
        self.inner.versions.bump(car, live, imported);
        r
    }

    /// Replaces the CAN rows that start in `[from_ms, to_ms)` with `rows` (which must start there too), all or
    /// none: a rebuilt window. Rows from TeslaMate stay. Returns how many rows were removed.
    pub fn replace_window<T: Item>(&self, car: CarId, from_ms: u64, to_ms: u64, rows: &[T]) -> Result<usize> {
        self.replace_window_of(car, Source::Live, from_ms, to_ms, rows)
    }

    /// [`Db::replace_window`] for the rows of `source`.
    fn replace_window_of<T: Item>(
        &self,
        car: CarId,
        source: Source,
        from_ms: u64,
        to_ms: u64,
        rows: &[T],
    ) -> Result<usize> {
        if from_ms > to_ms {
            return Err(Error::invalid("a window ends before it starts"));
        }
        check_rows(car, rows)?;
        if let Some(r) = rows.iter().find(|r| r.start_ms() < from_ms || r.start_ms() >= to_ms) {
            return Err(Error::invalid(format!("row {} starts outside the window", r.id())));
        }
        let rows = dedup(rows);
        let new_ids: HashSet<u64> = rows.iter().map(Item::id).collect();
        let imported = source.is_imported() || rows.iter().any(|r| r.source().is_imported());
        let live = !source.is_imported() || rows.iter().any(|r| !r.source().is_imported());
        let r = self.with_car(car, |db, data, dir| {
            let t = db.table::<T>(data, car, dir)?;
            let del = t.state.starting_in(from_ms, to_ms, source);
            let del_ids: HashSet<u64> = del.iter().copied().collect();
            let mut delta = Tally::default();
            let mut gone = 0;
            for id in &del {
                if let Some(old) = t.state.get(*id) {
                    old.tally(&mut delta, -1);
                    gone += usize::from(!new_ids.contains(id));
                }
            }
            let mut last = None;
            for r in &rows {
                if !del_ids.contains(&r.id())
                    && let Some(old) = t.state.get(r.id())
                {
                    old.tally(&mut delta, -1);
                }
                r.tally(&mut delta, 1);
                last = last.max(Some(r.end_ms()));
            }
            if del.is_empty() && rows.is_empty() {
                return Ok(0);
            }
            t.commit(&db.inner.ctx, Batch { del, put: rows })?;
            let w = t.weight();
            data.set_weight(T::TABLE, w);
            db.bump(car, &delta, last)?;
            Ok(gone)
        });
        self.inner.versions.bump(car, live, imported);
        r
    }

    /// Removes rows by id. Returns how many there were.
    pub fn delete<T: Item>(&self, car: CarId, ids: &[u64]) -> Result<usize> {
        let r = self.with_car(car, |db, data, dir| {
            let t = db.table::<T>(data, car, dir)?;
            let mut delta = Tally::default();
            let mut del = Vec::new();
            let mut seen = HashSet::with_capacity(ids.len());
            for id in ids {
                if let Some(old) = t.state.get(*id)
                    && seen.insert(*id)
                {
                    old.tally(&mut delta, -1);
                    del.push(*id);
                }
            }
            if del.is_empty() {
                return Ok(0);
            }
            let n = del.len();
            t.commit(&db.inner.ctx, Batch { del, put: Vec::new() })?;
            let w = t.weight();
            data.set_weight(T::TABLE, w);
            db.bump(car, &delta, None)?;
            Ok(n)
        });
        self.inner.versions.bump(car, true, true);
        r
    }

    pub fn get<T: Item>(&self, car: CarId, id: u64) -> Result<Option<T>> {
        self.with_car(car, |db, data, dir| Ok(db.table::<T>(data, car, dir)?.state.get(id).cloned()))
    }

    /// Rows overlapping a time range, a page at a time.
    pub fn list<T: Item>(&self, car: CarId, q: &Query) -> Result<Page<T>> {
        self.with_car(car, |db, data, dir| Ok(db.table::<T>(data, car, dir)?.state.query(q)))
    }

    /// How many rows a table of a car has.
    pub fn count<T: Item>(&self, car: CarId) -> Result<usize> {
        self.with_car(car, |db, data, dir| Ok(db.table::<T>(data, car, dir)?.state.len()))
    }

    /// Removes everything worked out from CAN logs for `[from_ms, to_ms)` (to rebuild that window): rows of every
    /// table that start in it and the CAN samples and charge points in it. What came from TeslaMate stays.
    pub fn delete_window(&self, car: CarId, from_ms: u64, to_ms: u64) -> Result<Deleted> {
        if from_ms > to_ms {
            return Err(Error::invalid("a window ends before it starts"));
        }
        let deleted = Deleted {
            drives: self.replace_window::<model::Drive>(car, from_ms, to_ms, &[])?,
            charges: self.replace_window::<model::Charge>(car, from_ms, to_ms, &[])?,
            states: self.replace_window::<StateRow>(car, from_ms, to_ms, &[])?,
            updates: self.replace_window::<UpdateRow>(car, from_ms, to_ms, &[])?,
        };
        self.write_points::<Sample>(car, Source::Live, from_ms, to_ms, &[])?;
        self.write_points::<ChargePoint>(car, Source::Live, from_ms, to_ms, &[])?;
        Ok(deleted)
    }

    /// Keeps a damaged file of a car that a read or a write met, for [`Db::damaged`].
    fn note_damage(&self, car: CarId, err: &Error) {
        let Error::Damaged { file, why } = err else { return };
        if !file.starts_with(&format!("db/cars/{car}/")) {
            return;
        }
        let Ok(mut d) = self.inner.damaged.lock() else { return };
        if d.len() >= 200 && !d.contains_key(file) {
            return;
        }
        let since_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |t| u64::try_from(t.as_millis()).unwrap_or(u64::MAX));
        let from_logs = can_series_file(car, file).is_some();
        d.entry(file.clone()).or_insert_with(|| DamagedFile {
            car,
            file: file.clone(),
            why: why.clone(),
            since_ms,
            from_logs,
        });
    }

    /// Forgets that these files of a car were found damaged (they were put aside or written again).
    fn forget_damage(&self, car: CarId, files: &[&Path]) {
        let Ok(mut d) = self.inner.damaged.lock() else { return };
        for f in files {
            d.remove(&self.inner.ctx.label(f));
        }
        let _ = car;
    }

    /// Takes a car's summary to count `part` as what `table` holds now (and its last activity to include `last`).
    fn mend_summary(&self, car: CarId, table: Table, part: &Tally, last: Option<u64>) -> Result<()> {
        let mut f = lock(&self.inner.fleet)?;
        if let Some(e) = f.state.get(car) {
            let have = e.summary.tally.part(table);
            let late = last.is_some_and(|l| e.summary.last_activity_ms.is_none_or(|a| l > a));
            if have != *part || late {
                let mut e = e.clone();
                e.summary.tally.add(&have, -1);
                e.summary.tally.add(part, 1);
                e.summary.last_activity_ms = e.summary.last_activity_ms.max(last);
                f.commit(&self.inner.ctx, FleetOp::Put(Box::new(e)))?;
            }
        }
        Ok(())
    }

    /// The cars' files that something found damaged since the db was opened and that are still there (one set
    /// aside or replaced since is no longer listed; one put back while the server ran is, until the next start),
    /// by path. Nothing damaged is ever read as empty, so what needs such a file fails each time: this is what to
    /// tell the owner, once.
    pub fn damaged(&self) -> Vec<DamagedFile> {
        let Ok(mut d) = self.inner.damaged.lock() else { return Vec::new() };
        let data = &self.inner.ctx.data;
        d.retain(|file, _| data.join(file).exists());
        d.values().cloned().collect()
    }

    /// A rebuild met `err`: when it is damage to a file of a series that is made from the stored CAN logs (a day's
    /// snapshot or journal, or a month's pack, of `seg/<series>.can`), sets the file aside (`<name>.damaged` beside
    /// it, in place of an earlier one) and returns the time it covered: everything it held is made again by decoding
    /// the logs of that time, which the caller then does. `None`: not such damage, nothing touched (a table, an
    /// import's or an API's series are not made again from logs, and a file of another format is not damage).
    pub fn set_aside_damaged_can_series(&self, car: CarId, err: &Error) -> Result<Option<(u64, u64)>> {
        let Error::Damaged { file, .. } = err else { return Ok(None) };
        let Some((series, name, span)) = can_series_file(car, file) else { return Ok(None) };
        self.with_car(car, |_, _, dir| {
            let folder = dir.join("seg").join(series);
            let (f, aside) = (folder.join(name), folder.join(format!("{name}.damaged")));
            if !f.exists() {
                return Ok(None);
            }
            file::remove_if_exists(&aside)?;
            std::fs::rename(&f, &aside)?;
            file::sync_dir(&folder);
            Ok(Some(span))
        })
    }

    /// Removes everything an import from `source` (an import site, never [`Source::Live`]) stored for `[from_ms,
    /// to_ms)`: rows of every table that start in it and that site's samples and charge points in it (a stretch read
    /// again).
    pub fn delete_imported_window(&self, car: CarId, source: Source, from_ms: u64, to_ms: u64) -> Result<Deleted> {
        if from_ms > to_ms {
            return Err(Error::invalid("a window ends before it starts"));
        }
        if source.site().is_none() {
            return Err(Error::invalid("only an import's rows are removed this way"));
        }
        let deleted = Deleted {
            drives: self.replace_window_of::<model::Drive>(car, source, from_ms, to_ms, &[])?,
            charges: self.replace_window_of::<model::Charge>(car, source, from_ms, to_ms, &[])?,
            states: self.replace_window_of::<StateRow>(car, source, from_ms, to_ms, &[])?,
            updates: self.replace_window_of::<UpdateRow>(car, source, from_ms, to_ms, &[])?,
        };
        self.write_points::<Sample>(car, source, from_ms, to_ms, &[])?;
        self.write_points::<ChargePoint>(car, source, from_ms, to_ms, &[])?;
        Ok(deleted)
    }

    // ---- a car's time series ----

    /// Replaces every point of a series in `[from_ms, to_ms)` with `points` (in time order, one per time, all in
    /// the window). An empty `points` deletes the window.
    pub fn write_points<P: Point>(
        &self,
        car: CarId,
        stream: impl Into<Stream>,
        from_ms: u64,
        to_ms: u64,
        points: &[P],
    ) -> Result<()> {
        let stream = stream.into();
        let r = self.with_car(car, |db, data, dir| {
            let first = segment::write(&db.inner.ctx, dir, stream, from_ms, to_ms, points)?;
            if let Some(day) = first {
                let m = data.meta(&db.inner.ctx, dir)?;
                if day * DAY_MS < m.settled_before_ms.max(m.small_before_ms) {
                    m.settled_before_ms = m.settled_before_ms.min(day * DAY_MS);
                    m.small_before_ms = m.small_before_ms.min(day * DAY_MS);
                    data.save_meta(&db.inner.ctx, dir)?;
                }
            }
            match points.last() {
                Some(p) => db.bump(car, &Tally::default(), Some(p.t_ms())),
                None => Ok(()),
            }
        });
        self.inner.versions.bump(car, !stream.is_imported(), stream.is_imported());
        r
    }

    /// The points of a series in `[from_ms, to_ms)`, in time order.
    pub fn read_points<P: Point>(
        &self,
        car: CarId,
        stream: impl Into<Stream>,
        from_ms: u64,
        to_ms: u64,
    ) -> Result<Vec<P>> {
        let stream = stream.into();
        // days of a snapshot alone are unpacked without the car's lock, days with a journal under it; all are
        // decoded after it, so several answers read one car at once. Which days there are is looked up under it: a
        // month being packed has its days as files or in the pack, and a listing in between could miss both
        let ctx = &self.inner.ctx;
        let (dir, days) = self.with_car(car, |_, _, dir| {
            Ok((dir.to_path_buf(), segment::days_in::<P>(ctx, dir, stream, from_ms, to_ms)?))
        })?;
        let days = segment::plains::<P>(ctx, &dir, stream, &days, &mut |day| {
            self.with_car(car, |_, _, dir| segment::journaled_plain::<P>(ctx, dir, stream, day))
        })
        .inspect_err(|e| self.note_damage(car, e))?;
        segment::points_of(days, from_ms, to_ms)
    }

    /// Some fields of a series in `[from_ms, to_ms)`, as columns (what a chart draws; faster than whole points).
    pub fn read_fields<P: Point>(
        &self,
        car: CarId,
        stream: impl Into<Stream>,
        from_ms: u64,
        to_ms: u64,
        fields: &[&str],
    ) -> Result<Fields> {
        let stream = stream.into();
        // as [`Db::read_points`]: a day with a journal under the car's lock, the others without it
        let ctx = &self.inner.ctx;
        let (dir, days) = self.with_car(car, |_, _, dir| {
            Ok((dir.to_path_buf(), segment::days_in::<P>(ctx, dir, stream, from_ms, to_ms)?))
        })?;
        let days = segment::plains::<P>(ctx, &dir, stream, &days, &mut |day| {
            self.with_car(car, |_, _, dir| segment::journaled_plain::<P>(ctx, dir, stream, day))
        })
        .inspect_err(|e| self.note_damage(car, e))?;
        segment::fields_of::<P>(days, from_ms, to_ms, fields)
    }

    /// Per day of a series (its UTC start), a key of what its files are now: it changes whenever the day is written
    /// (what was worked out from a day can be kept with it).
    pub fn day_stamps<P: Point>(&self, car: CarId, stream: impl Into<Stream>) -> Result<Vec<(u64, u64)>> {
        let stream = stream.into();
        self.with_car(car, |db, _, dir| segment::day_stamps::<P>(&db.inner.ctx, dir, stream))
    }

    /// The days a series has.
    pub fn days<P: Point>(&self, car: CarId, stream: impl Into<Stream>) -> Result<Vec<DayInfo>> {
        let stream = stream.into();
        self.with_car(car, |db, _, dir| segment::days::<P>(&db.inner.ctx, dir, stream))
    }

    /// One key of what the files of a series' days in `[from_ms, to_ms)` are now: it changes whenever a point in
    /// those days is written or removed (what was read from them can be kept with it).
    pub fn days_stamp<P: Point>(&self, car: CarId, stream: impl Into<Stream>, from_ms: u64, to_ms: u64) -> Result<u64> {
        let stream = stream.into();
        self.with_car(car, |db, _, dir| segment::days_stamp::<P>(&db.inner.ctx, dir, stream, from_ms, to_ms))
    }

    /// Bytes a car's time series take on disk.
    pub fn segment_bytes(&self, car: CarId) -> Result<u64> {
        self.with_car(car, |_, _, dir| segment::bytes(dir))
    }

    /// Housekeeping for a car (run when idle): packs its finished days (before `before_ms`) at the cold level and
    /// folds its tables' journals. Every day before `before_ms` with a journal is folded, however old (a late
    /// upload, an import into old days). The days stay files of their own: [`Db::compact_series`] also packs the
    /// months.
    pub fn settle(&self, car: CarId, before_ms: u64) -> Result<Settled> {
        self.with_car(car, |db, data, dir| {
            let mut out =
                Settled { days: db.compact_days(data, dir, before_ms, usize::MAX, false)?.days_folded, tables: 0 };
            out.tables += db.fold_table::<model::Drive>(data, car, dir)?;
            out.tables += db.fold_table::<model::Charge>(data, car, dir)?;
            out.tables += db.fold_table::<StateRow>(data, car, dir)?;
            out.tables += db.fold_table::<UpdateRow>(data, car, dir)?;
            Ok(out)
        })
    }

    /// A pass over a car's time series, under its lock (see [`segment::compact`]); notes how far its days are
    /// known to be settled, which later passes need not look at again.
    fn compact_days(
        &self,
        data: &mut CarData,
        dir: &Path,
        before_ms: u64,
        max_days: usize,
        packing: bool,
    ) -> Result<SeriesCompacted> {
        let ctx = &self.inner.ctx;
        let before = before_ms / DAY_MS * DAY_MS;
        // (a folder that got the small form later: its days are looked at once more)
        let small = ctx.opts.small_columns;
        let meta = data.meta(ctx, dir)?;
        let known = if small { meta.small_before_ms } else { meta.settled_before_ms };
        let c = segment::compact(ctx, dir, known, before, max_days, packing)?;
        if c.known_cold_before != known {
            let meta = data.meta(ctx, dir)?;
            meta.settled_before_ms = meta.settled_before_ms.max(c.known_cold_before);
            if small {
                meta.small_before_ms = c.known_cold_before;
            }
            data.save_meta(ctx, dir)?;
        }
        Ok(SeriesCompacted { days_folded: c.folded, days_packed: c.packed, days_recoded: c.recoded, more: c.more })
    }

    /// A step of compacting a car's time series, losslessly (run it again, with pauses, while it says `more`): at
    /// most about `max_days` days of work under one hold of the car's lock, oldest first. First the days that ended
    /// before `before_ms` are settled as [`Db::settle`] does (journals folded in, snapshots at the cold level); then
    /// each month that ended before `before_ms` is packed into one file, its days' snapshots as they are (see
    /// [`segment`]; a month is packed whole, so a step may go a month past `max_days`). Days of a month not over
    /// stay files of their own. A crash at any point leaves every day readable; the next step carries on.
    pub fn compact_series(&self, car: CarId, before_ms: u64, max_days: usize) -> Result<SeriesCompacted> {
        self.with_car(car, |db, data, dir| db.compact_days(data, dir, before_ms, max_days, true))
    }

    /// How many days [`Db::compact_series`] would still fold or pack for a car (0: nothing to do). Cheap: listings
    /// of its series' folders only. A snapshot not yet known to be at the cold level counts, though the pass may
    /// find it is.
    pub fn series_pending(&self, car: CarId, before_ms: u64) -> Result<usize> {
        self.with_car(car, |db, data, dir| {
            let ctx = &db.inner.ctx;
            let meta = data.meta(ctx, dir)?;
            let known = if ctx.opts.small_columns { meta.small_before_ms } else { meta.settled_before_ms };
            segment::pending(ctx, dir, known, before_ms / DAY_MS * DAY_MS)
        })
    }

    fn fold_table<T: Item>(&self, data: &mut CarData, car: CarId, dir: &Path) -> Result<usize> {
        let has = dir.join(format!("{}.log", T::TABLE.name())).exists();
        if !has {
            return Ok(0);
        }
        let t = self.table::<T>(data, car, dir)?;
        t.fold(&self.inner.ctx, Which::Warm)?;
        let w = t.weight();
        data.set_weight(T::TABLE, w);
        Ok(1)
    }

    // ---- places, settings, key/value ----

    fn places_mut<R>(&self, f: impl FnOnce(&Ctx, &mut LoggedFile<Places>) -> Result<R>) -> Result<R> {
        let mut p = lock(&self.inner.places)?;
        if p.is_none() {
            *p = Some(LoggedFile::load(&self.inner.ctx, Files::named(&self.inner.dir, "places"))?);
        }
        let lf = p.as_mut().ok_or_else(|| Error::invalid("places"))?;
        let r = f(&self.inner.ctx, lf);
        if r.is_err() {
            *p = None;
        }
        r
    }

    pub fn places(&self) -> Result<Vec<Place>> {
        self.places_mut(|_, p| Ok(p.state.places.values().cloned().collect()))
    }

    pub fn place(&self, id: PlaceId) -> Result<Option<Place>> {
        self.places_mut(|_, p| Ok(p.state.places.get(&id).cloned()))
    }

    fn check_place(p: &Place) -> Result<()> {
        if !(model::MIN_RADIUS_M..=model::MAX_RADIUS_M).contains(&p.radius_m) {
            return Err(Error::invalid(format!(
                "a place's radius is {} to {} m",
                model::MIN_RADIUS_M,
                model::MAX_RADIUS_M
            )));
        }
        if !(-90.0..=90.0).contains(&p.lat) || !(-180.0..=180.0).contains(&p.lon) {
            return Err(Error::invalid("a place's position is out of range"));
        }
        check_storable(p, "the place")
    }

    /// Adds a place; its id is given here. Returns the place as kept.
    pub fn add_place(&self, mut place: Place) -> Result<Place> {
        Db::check_place(&place)?;
        let r = self.places_mut(|ctx, p| {
            place.id = p.state.next_id;
            p.commit(ctx, PlaceOp::Put(place.clone()))?;
            Ok(place)
        });
        self.inner.versions.bump_shared();
        r
    }

    pub fn update_place(&self, place: Place) -> Result<()> {
        Db::check_place(&place)?;
        let r = self.places_mut(|ctx, p| {
            if !p.state.places.contains_key(&place.id) {
                return Err(Error::invalid(format!("no place {}", place.id)));
            }
            p.commit(ctx, PlaceOp::Put(place))
        });
        self.inner.versions.bump_shared();
        r
    }

    pub fn remove_place(&self, id: PlaceId) -> Result<bool> {
        let r = self.places_mut(|ctx, p| {
            if !p.state.places.contains_key(&id) {
                return Ok(false);
            }
            p.commit(ctx, PlaceOp::Remove(id))?;
            Ok(true)
        });
        self.inner.versions.bump_shared();
        r
    }

    fn settings_mut<R>(&self, f: impl FnOnce(&Ctx, &mut LoggedFile<Settings>) -> Result<R>) -> Result<R> {
        let mut s = lock(&self.inner.settings)?;
        if s.is_none() {
            *s = Some(LoggedFile::load(&self.inner.ctx, Files::named(&self.inner.dir, "settings"))?);
        }
        let lf = s.as_mut().ok_or_else(|| Error::invalid("settings"))?;
        let r = f(&self.inner.ctx, lf);
        if r.is_err() {
            *s = None;
        }
        r
    }

    pub fn setting(&self, key: &str) -> Result<Option<serde_json::Value>> {
        self.settings_mut(|_, s| Ok(s.state.values.get(key).cloned()))
    }

    pub fn settings(&self) -> Result<BTreeMap<String, serde_json::Value>> {
        self.settings_mut(|_, s| Ok(s.state.values.clone()))
    }

    pub fn set_setting(&self, key: &str, value: serde_json::Value) -> Result<()> {
        let r = self.settings_mut(|ctx, s| s.commit(ctx, SettingOp::Set(key.to_string(), value)));
        self.inner.versions.bump_shared();
        r
    }

    pub fn remove_setting(&self, key: &str) -> Result<bool> {
        let r = self.settings_mut(|ctx, s| {
            if !s.state.values.contains_key(key) {
                return Ok(false);
            }
            s.commit(ctx, SettingOp::Remove(key.to_string()))?;
            Ok(true)
        });
        self.inner.versions.bump_shared();
        r
    }

    /// The key/value store for other parts.
    pub fn kv(&self) -> &Kv {
        &self.inner.kv
    }
}
