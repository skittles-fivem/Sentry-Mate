//! Sentry Mate's engine: the background pipeline that turns stored, checked CAN Bridge logs into each car's history,
//! and the read side the web API answers from.
//!
//! # The pipeline
//!
//! 1. **Told.** `ingest`'s [`ingest::Judge`] tells the engine of every log whose trust record it writes
//!    ([`Engine::listener`], given to [`ingest::Judge::on_judged`]), and the engine works on it in the background: a
//!    bounded number of worker threads ([`Options::workers`]), one log of a car at a time, so uploads never wait on it.
//! 2. **Its car.** The log's CAN key names the car ([`db::Db::car_for_key`]). A key no car claims goes to the car its
//!    token was made for, to the only car (unless its logs' frames name another VIN, or its token was made for
//!    another car's name: then it makes a car of its own), makes the first, or waits for an admin
//!    ([`Options::new_keys`], [`Engine::assign_key`]).
//! 3. **Decoded**, only if its check allows its data ([`ingest::judge::Record::usable`]): `logfmt` → `dbc` with the DBC
//!    set the car's VIN calls for (looked for in the log's own frames, then its sidecar, while the car has none) →
//!    one sample a second ([`timeline::read_log`]), written to the car's time series. A log whose check fails is never
//!    used: it is a hole (unknown, never asleep), and one that stops being usable (a key distrusted, judged again, even
//!    while the engine was down) has its data taken out. Where two keys of a car record at once, each second has one
//!    source: the car's preferred key, else the verified log, else the one with fewer drops. A log whose frames name
//!    another VIN than the car's (from frames) is left out.
//! 4. **The window it changes** ([`timeline::rebuild_window`]): back and forward to the nearest settled span or idle
//!    cut point, so a late or out-of-order log rebuilds only what it touches. The window's states, drives, charges and
//!    charge points are built ([`timeline::build`]) and replace the CAN rows of the window
//!    ([`db::Db::replace_window`]). Imported TeslaMate rows are never touched by it.
//! 5. **Places and costs**: a charge that ended is priced at its place's price then, and keeps that cost across
//!    rebuilds ([`costs`]). Places are matched live when read ([`placing`]), so a renamed or moved place shows at once.
//! 6. **Live**: the car's newest values (its Now), and an [`Event`] for every change, on a broadcast the web server
//!    turns into server-sent events ([`Engine::subscribe`]).
//!
//! **A second live input**, the Tesla API ([`tesla_in`]): its samples and its view of the car's state are kept apart
//! from SMO's and fused with them when a window is built, so a drive both saw is one row naming both inputs.
//!
//! **Restart-safe.** What was decoded is recorded per log ([`records`]), and a window written to is noted dirty until it
//! is rebuilt. On start ([`Engine::start`]) each car's logs are compared with what was recorded
//! ([`Engine::reconcile`]): logs told of while the engine was down (or cut off by a crash) are worked on, dirty windows
//! are rebuilt, and logs already recorded are not decoded again.
//!
//! **Rebuilds** ([`Engine::rebuild`]): a car, or every car, from its raw logs, cancellable, with progress ([`Job`]).
//!
//! # The read side
//!
//! [`query`]: plain Rust functions over the db (no HTTP), one module per resource of `docs/ui-api.md`, answering in
//! the contract's field names and units (metric, named in the field; ids as strings; times in ms UTC; local days in
//! the car's time zone), with its paging, filters and sorting.

pub mod costs;
pub mod firmware;
pub mod importer;
pub mod jobs;
mod pipeline;
pub use pipeline::NEW_CAR_NAME;
pub mod placing;
pub mod query;
pub mod records;
pub mod tesla_in;
pub mod tmlocal_catchup;
pub mod tz;

pub use tesla_in::{TeslaSink, TeslafiSink, TeslamateLocalSink};

pub use jobs::{Job, JobStatus, Scope};
pub use records::DECODE_VERSION;

use db::Db;
use ingest::{Judge, Store};
use model::CarId;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{broadcast, mpsc};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Db(#[from] db::Error),
    #[error("reading a stored log: {0}")]
    Store(String),
    #[error("{0} is damaged")]
    Damaged(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("the engine is not running (Engine::start was not called)")]
    NotStarted,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What to do with a CAN key no car claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NewKeys {
    /// Make a new car for it (named after its model and VIN prefix until someone names it), whatever cars there are.
    NewCar,
    /// Give it to the car when there is exactly one, make the first car when there is none, else wait for an admin
    /// (Settings → Cars: "New CAN Bridge seen: which car?"). A new Screenmate, or the app reinstalled, on the only
    /// car keeps its history in one car.
    #[default]
    OnlyCarElseWait,
    /// Always wait for an admin ([`Engine::assign_key`]).
    Wait,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Logs worked on at once (each on its own car). Uploads never wait on them.
    pub workers: usize,
    pub params: timeline::Params,
    pub sample: timeline::SampleParams,
    pub new_keys: NewKeys,
    /// Events a slow reader may fall behind before it misses some.
    pub event_capacity: usize,
    /// The time zone of a car made here, at start (the server setting `default_time_zone` then follows it:
    /// [`Engine::set_default_time_zone`]).
    pub default_time_zone: String,
    /// A car's Tesla API data is built into its timeline at most this often (ms): a car polled every half minute, or
    /// streaming while driving, is not rebuilt for every reading.
    pub api_rebuild_ms: u64,
    /// How SMO's and the Tesla API's samples are made one ([`timeline::fuse`]).
    pub fuse: timeline::FuseParams,
}

impl Default for Options {
    fn default() -> Options {
        let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
        Options {
            workers: (cores / 2).clamp(1, 4),
            params: timeline::Params::default(),
            sample: timeline::SampleParams::default(),
            new_keys: NewKeys::default(),
            event_capacity: 1024,
            default_time_zone: "UTC".into(),
            api_rebuild_ms: 30_000,
            fuse: timeline::FuseParams::default(),
        }
    }
}

/// A change the web server passes on (as server-sent events, filtered by the account's cars).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// A car's newest values changed: its whole Now (the web server sends only what changed).
    Now { car: CarId, now: Box<query::now::NowView> },
    /// Rows of these kinds changed in `[from_ms, to_ms)` (a window rebuilt, an import, a rebuild).
    Rows { car: CarId, kinds: Vec<&'static str>, from_ms: u64, to_ms: u64 },
    /// A car was added, removed or renamed.
    Fleet { car: CarId, change: &'static str, name: Option<String> },
    /// A rebuild job moved on.
    Job { job: u64, car: Option<CarId>, status: JobStatus, done: u64, total: u64 },
}

/// How the engine is doing (for `/system/health`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct EngineStats {
    pub logs_decoded: u64,
    pub logs_left_out: u64,
    pub windows_rebuilt: u64,
    pub queued: u64,
    pub failures: u64,
    pub decode_ms_total: u64,
    pub rebuild_ms_total: u64,
    pub last_error: Option<String>,
}

#[derive(Default)]
struct Counters {
    decoded: AtomicU64,
    left_out: AtomicU64,
    windows: AtomicU64,
    queued: AtomicU64,
    failures: AtomicU64,
    decode_ms: AtomicU64,
    rebuild_ms: AtomicU64,
}

pub(crate) enum Work {
    Log {
        key: String,
        name: String,
    },
    Reconcile {
        car: Option<CarId>,
    },
    /// Tesla API or TeslaFi API data to store ([`tesla_in`]).
    ApiData {
        input: model::Input,
        car: CarId,
        samples: Vec<model::Sample>,
        facts: Vec<timeline::ApiSpan>,
    },
    ApiMeta {
        car: CarId,
        meta: Box<tesla::ApiMeta>,
    },
    /// A car's stored Tesla API data to build into its timeline.
    ApiBuild {
        car: CarId,
    },
    /// An input's values no CAN message carries (TeslaFi's polls, local TeslaMate's MQTT) ([`tesla_in`]).
    OffCan {
        input: model::Input,
        car: CarId,
        at_ms: u64,
        values: Box<model::OffCan>,
    },
}

pub(crate) struct Inner {
    /// Work sent and not finished.
    outstanding: Arc<AtomicU64>,
    pub(crate) db: Db,
    pub(crate) store: Store,
    pub(crate) judge: Judge,
    pub(crate) opts: Options,
    events: broadcast::Sender<Event>,
    pub(crate) tx: mpsc::UnboundedSender<Work>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<Work>>>,
    car_locks: Mutex<HashMap<CarId, Arc<Mutex<()>>>>,
    /// Making a car for a new key, one at a time.
    pub(crate) claim: Mutex<()>,
    /// A change to a car's record (read, changed, written back), one at a time: held only around that, never while
    /// taking another lock, so the pipeline (which holds a car's lock) and an admin's change can't undo each other.
    pub(crate) car_record: Mutex<()>,
    pub(crate) places: RwLock<Option<Arc<placing::Places>>>,
    pub(crate) jobs: Mutex<HashMap<u64, Arc<jobs::JobState>>>,
    pub(crate) next_job: AtomicU64,
    counters: Counters,
    last_error: Mutex<Option<String>>,
    handle: Mutex<Option<tokio::runtime::Handle>>,
    /// The upload tokens (their car names tell a new car's logger from the only car's), when given.
    tokens: std::sync::OnceLock<Arc<ingest::Tokens>>,
    /// Cars whose Tesla API data waits for a build, and when each was last built.
    pub(crate) api_pending: Mutex<std::collections::HashSet<CarId>>,
    pub(crate) api_built: Mutex<HashMap<CarId, u64>>,
    /// The time zone a car made from now on starts in.
    default_zone: RwLock<String>,
    /// Whether an import fills the values live history lacks (the server setting; a car's own setting wins).
    pub(crate) gap_fill: std::sync::atomic::AtomicBool,
    /// Per car, the position its zone was last looked up at and the zone found there ([`Inner::car_zone`]).
    zone_at: Mutex<HashMap<CarId, ZoneAt>>,
}

/// Where a car's zone was last looked up (latitude, longitude) and the zone found there.
type ZoneAt = (f64, f64, Option<&'static str>);

/// A car's zone is looked up again once its position moved this far (degrees, about a kilometre).
const ZONE_MOVE_DEG: f64 = 0.01;

impl Inner {
    pub(crate) fn default_zone(&self) -> String {
        self.default_zone.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The zone a car's days and clocks follow, and where it comes from: the owner's, else the zone of its newest
    /// position (its Now's, else its newest drive's end; looked up in the boundaries built in, never sent anywhere),
    /// else the server's default.
    pub(crate) fn car_zone(&self, c: &model::Car) -> (String, tz::ZoneSource) {
        if let Some(z) = c.typed_zone() {
            return tz::pick(Some(z), None, "");
        }
        let at = self.position_of(c.id);
        let found = at.and_then(|(lat, lon)| {
            let mut cache = lock(&self.zone_at);
            match cache.get(&c.id) {
                Some(&(a, b, z)) if (a - lat).abs() < ZONE_MOVE_DEG && (b - lon).abs() < ZONE_MOVE_DEG => z,
                Some(_) | None => {
                    let z = tz::zone_at(lat, lon);
                    cache.insert(c.id, (lat, lon, z));
                    z
                }
            }
        });
        tz::pick(None, found, &self.default_zone())
    }

    /// A car's newest known position.
    fn position_of(&self, car: CarId) -> Option<(f64, f64)> {
        let now = self.db.entry(car).ok().flatten().and_then(|x| x.summary.now);
        if let Some(p) = now.and_then(|n| n.sample.lat.zip(n.sample.lon)) {
            return Some(p);
        }
        let newest = self.db.list::<model::Drive>(car, &db::Query::default().limit(1)).ok()?.items.into_iter().next();
        newest.and_then(|d| d.end.or(d.start)).map(|p| (p.lat, p.lon))
    }

    /// A car with `time_zone` the zone it follows ([`Inner::car_zone`]); `time_zone_typed` is written out, so
    /// storing the car back keeps what the owner set (and an automatic zone automatic).
    pub(crate) fn zoned(&self, mut c: model::Car) -> model::Car {
        let (zone, source) = self.car_zone(&c);
        c.time_zone_typed = Some(match source {
            tz::ZoneSource::Owner => true,
            tz::ZoneSource::Position | tz::ZoneSource::Default => false,
        });
        c.time_zone = zone;
        c
    }

    pub(crate) fn outstanding_add(&self) {
        self.outstanding.fetch_add(1, Ordering::SeqCst);
    }
    pub(crate) fn outstanding_sub(&self) {
        self.outstanding.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The engine. Cheap to clone; share it.
#[derive(Clone)]
pub struct Engine {
    pub(crate) inner: Arc<Inner>,
}

impl Engine {
    /// An engine over the db and the logs `store` keeps. It reads trust records with its own [`Judge`] over the same
    /// store (the judge's records are safe to share between judges and processes); give [`Engine::listener`] to the
    /// judge the uploads use, then call [`Engine::start`].
    pub fn new(db: Db, store: Store, opts: Options) -> Engine {
        let (events, _) = broadcast::channel(opts.event_capacity.max(16));
        let (tx, rx) = mpsc::unbounded_channel();
        Engine {
            inner: Arc::new(Inner {
                outstanding: Arc::new(AtomicU64::new(0)),
                judge: Judge::new(store.clone()),
                db,
                store,
                events,
                tx,
                rx: Mutex::new(Some(rx)),
                car_locks: Mutex::new(HashMap::new()),
                claim: Mutex::new(()),
                car_record: Mutex::new(()),
                places: RwLock::new(None),
                jobs: Mutex::new(HashMap::new()),
                next_job: AtomicU64::new(1),
                counters: Counters::default(),
                last_error: Mutex::new(None),
                handle: Mutex::new(None),
                tokens: std::sync::OnceLock::new(),
                api_pending: Mutex::new(std::collections::HashSet::new()),
                api_built: Mutex::new(HashMap::new()),
                default_zone: RwLock::new(opts.default_time_zone.clone()),
                gap_fill: std::sync::atomic::AtomicBool::new(true),
                zone_at: Mutex::new(HashMap::new()),
                opts,
            }),
        }
    }

    pub fn db(&self) -> &Db {
        &self.inner.db
    }

    /// The time zone a car made from now on starts in (a new logger's, a Tesla account's or an imported car).
    pub fn default_time_zone(&self) -> String {
        self.inner.default_zone()
    }

    /// The zone a car's days and clocks follow (`America/Chicago`), and where it comes from: the owner's, else the
    /// zone its newest position is in, else the server's default.
    pub fn car_zone(&self, c: &model::Car) -> (String, tz::ZoneSource) {
        self.inner.car_zone(c)
    }

    /// Sets the server's default time zone (a car with no zone set and no position known follows it); `false`
    /// (and nothing changed) for a name that is not an IANA zone.
    pub fn set_default_time_zone(&self, zone: &str) -> bool {
        if !tz::is_zone(zone) {
            return false;
        }
        *self.inner.default_zone.write().unwrap_or_else(|e| e.into_inner()) = zone.to_string();
        true
    }

    /// Whether imports fill the values live history lacks, server-wide (on until the server setting turns it off;
    /// a car's own `import_gap_fill` setting wins over it).
    pub fn import_gap_fill(&self) -> bool {
        self.inner.gap_fill.load(Ordering::SeqCst)
    }

    /// Turns import gap filling on or off server-wide (the server setting `import_gap_fill.enabled`).
    pub fn set_import_gap_fill(&self, on: bool) {
        self.inner.gap_fill.store(on, Ordering::SeqCst);
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    /// The engine's own judge (for reading trust records: logs and their check results).
    pub fn judge(&self) -> &Judge {
        &self.inner.judge
    }

    pub fn options(&self) -> &Options {
        &self.inner.opts
    }

    /// Gives the engine the upload tokens (before [`Engine::start`]): a new CAN key whose token was made for another
    /// car's name, or on another car's page, gets that car, or one of its own, instead of the only car. The first
    /// given stays.
    pub fn set_tokens(&self, tokens: Arc<ingest::Tokens>) {
        let _ = self.inner.tokens.set(tokens);
    }

    /// The upload tokens, when given ([`Engine::set_tokens`]).
    pub fn tokens(&self) -> Option<&Arc<ingest::Tokens>> {
        self.inner.tokens.get()
    }

    /// What to give [`ingest::Judge::on_judged`]: it notes the log and returns at once.
    pub fn listener(&self) -> ingest::OnJudged {
        let (tx, outstanding) = (self.inner.tx.clone(), self.inner.outstanding.clone());
        Arc::new(move |j: &ingest::Judged| {
            // (a send fails only once the engine is gone; the next start's reconcile finds the log)
            outstanding.fetch_add(1, Ordering::SeqCst);
            if tx.send(Work::Log { key: j.key.clone(), name: j.name.clone() }).is_err() {
                outstanding.fetch_sub(1, Ordering::SeqCst);
            }
        })
    }

    /// Every change from now on. A receiver that falls [`Options::event_capacity`] behind misses events (it is told
    /// so by `RecvError::Lagged`); the page then loads what it shows again.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    pub(crate) fn emit(&self, e: Event) {
        let _ = self.inner.events.send(e);
    }

    /// Starts the background work on the current tokio runtime: carries on with what a restart left (pending logs,
    /// dirty windows, rebuilds under way), then compares every car's logs with what was recorded, then works on logs
    /// as they are told. Calling it again does nothing.
    pub fn start(&self) -> Result<()> {
        // the runtime first: taking the receiver and then failing would leave every later start a no-op
        let handle = tokio::runtime::Handle::try_current().map_err(|_| Error::NotStarted)?;
        let Some(rx) = lock(&self.inner.rx).take() else { return Ok(()) };
        *lock(&self.inner.handle) = Some(handle.clone());
        let engine = self.clone();
        handle.spawn(async move { engine.run(rx).await });
        self.send(Work::Reconcile { car: None });
        Ok(())
    }

    pub(crate) fn send(&self, w: Work) {
        self.inner.outstanding.fetch_add(1, Ordering::SeqCst);
        if self.inner.tx.send(w).is_err() {
            self.inner.outstanding.fetch_sub(1, Ordering::SeqCst);
        }
    }

    pub(crate) fn handle(&self) -> Result<tokio::runtime::Handle> {
        lock(&self.inner.handle).clone().ok_or(Error::NotStarted)
    }

    /// Queues a comparison of a car's (or every car's) logs with what was recorded: logs not worked on, or worked on
    /// with older rules or another DBC set, are worked on again.
    pub fn reconcile(&self, car: Option<CarId>) {
        self.send(Work::Reconcile { car });
    }

    async fn run(self, mut rx: mpsc::UnboundedReceiver<Work>) {
        let permits = Arc::new(tokio::sync::Semaphore::new(self.inner.opts.workers.max(1)));
        while let Some(w) = rx.recv().await {
            let Ok(permit) = permits.clone().acquire_owned().await else { break };
            let e = self.clone();
            self.inner.counters.queued.fetch_add(1, Ordering::Relaxed);
            tokio::task::spawn_blocking(move || {
                let r = match w {
                    Work::Log { key, name } => e.work_log(&key, &name),
                    Work::Reconcile { car } => e.work_reconcile(car),
                    Work::ApiData { input, car, samples, facts } => e.work_api_data(input, car, samples, facts),
                    Work::ApiMeta { car, meta } => e.work_api_meta(car, *meta),
                    Work::ApiBuild { car } => e.work_api_build(car),
                    Work::OffCan { input, car, at_ms, values } => e.work_off_can(input, car, at_ms, *values),
                };
                if let Err(err) = r {
                    e.failed(&err);
                }
                e.inner.counters.queued.fetch_sub(1, Ordering::Relaxed);
                e.inner.outstanding.fetch_sub(1, Ordering::SeqCst);
                drop(permit);
            });
        }
    }

    pub(crate) fn failed(&self, err: &Error) {
        self.inner.counters.failures.fetch_add(1, Ordering::Relaxed);
        tracing::warn!("background decoding: {err}");
        *lock(&self.inner.last_error) = Some(err.to_string());
    }

    /// The lock that keeps one car's work one at a time.
    pub(crate) fn car_lock(&self, car: CarId) -> Arc<Mutex<()>> {
        lock(&self.inner.car_locks).entry(car).or_default().clone()
    }

    /// Work sent and not finished (logs and comparisons).
    pub fn queued(&self) -> u64 {
        self.inner.outstanding.load(Ordering::SeqCst)
    }

    pub fn stats(&self) -> EngineStats {
        let c = &self.inner.counters;
        EngineStats {
            logs_decoded: c.decoded.load(Ordering::Relaxed),
            logs_left_out: c.left_out.load(Ordering::Relaxed),
            windows_rebuilt: c.windows.load(Ordering::Relaxed),
            queued: c.queued.load(Ordering::Relaxed),
            failures: c.failures.load(Ordering::Relaxed),
            decode_ms_total: c.decode_ms.load(Ordering::Relaxed),
            rebuild_ms_total: c.rebuild_ms.load(Ordering::Relaxed),
            last_error: lock(&self.inner.last_error).clone(),
        }
    }

    /// Waits until no work is queued or under way (tests, and a command that wants the history up to date).
    pub async fn idle(&self) {
        while self.inner.outstanding.load(Ordering::SeqCst) > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
