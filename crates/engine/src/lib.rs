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

mod ahead;
pub mod compact;
pub mod costs;
pub mod firmware;
pub mod full;
pub mod gps_fit;
pub mod gps_fit_runs;
pub mod history;
pub mod importer;
pub mod invoice_prices;
pub mod invoices;
pub mod jobs;
mod live_gps;
mod pipeline;
pub use pipeline::NEW_CAR_NAME;
mod hot;
mod memo;
pub mod placing;
pub mod query;
pub mod records;
pub mod rehome;
mod speed;
pub mod tesla_in;
pub mod tmlocal_catchup;
pub mod tz;
pub mod versions;
mod warm;

pub use tesla_in::{TeslaSink, TeslafiSink, TeslamateLocalSink};

pub use jobs::{Job, JobStatus, Scope};
pub use records::DECODE_VERSION;
pub use speed::RebuildSpeed;

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
    #[error("{0}")]
    Other(String),
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
    /// While a car's stored logs are worked through (at the start, after a rebuild was asked for), the windows of
    /// the timeline they change are rebuilt once per this many logs, joined where they meet, instead of after every
    /// log, where that gives the same timeline (nothing stored yet at or after the logs, no API input: a first
    /// start over a full store, a car rebuilt whole). `1`: always after every log, as a log that arrives alone gets.
    pub window_batch: usize,
    /// A rebuild a saved setting asks for waits this long (ms) before it starts, so a few saves in a row (or a
    /// setting saved again while one runs) are one rebuild ([`Engine::rebuild_because`]).
    pub rebuild_settle_ms: u64,
    /// Threads that read and decode a car's stored logs ahead while its logs are worked through one after the
    /// other (the logs found at the start, a car rebuilt whole, a full rebuild: [`ahead`]). `0`: each log is read
    /// in place. By default the machine's cores but one, eight at most, none on one or two cores.
    pub read_ahead: usize,
    /// How far either side of a log a window's bounds are first looked for (ms): the state spans and idle cut points
    /// of that stretch are read, and four times as far each time no bound is found in it (the same bounds however
    /// far it starts: crates/sentry-mate/src/window_batch.rs). Two hours: a car in sentry mode has a cut point every
    /// five minutes, and the stretch read is then about 50 state spans, not two days' of them.
    pub window_look_ms: u64,
    /// The newest values of a car are sent to the live stream at most this often (ms) while a rebuild of the car
    /// runs, and once more when it ends if one was held back ([`Engine::send_now`]). Live ingest outside a rebuild
    /// is never held back. `0`: after every window.
    pub rebuild_now_gap_ms: u64,
    /// The clock the throttle above reads (ms since the epoch); a test sets its own.
    pub clock: fn() -> u64,
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
            window_batch: 64,
            rebuild_settle_ms: 3_000,
            read_ahead: ahead::Ahead::threads(),
            window_look_ms: 2 * 3_600_000,
            rebuild_now_gap_ms: 1_000,
            clock: now_ms,
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
    /// Of the time the logs took (`decode_ms_total`): reading each log from the store and decoding it into samples,
    /// writing its samples, and working out the car's newest values from them (µs; measured, docs/plans/performance.md).
    pub read_us_total: u64,
    pub samples_us_total: u64,
    pub now_us_total: u64,
    /// Logs whose reading made ahead on another core was used ([`ahead`]).
    pub logs_read_ahead: u64,
    /// Of the windows' time (`rebuild_ms_total`, µs): finding the window, reading the inputs' samples, fusing them,
    /// building the timeline, writing its rows, the rest of the writing (the car's state now, the events), and of
    /// the first: the window's logs, the nearest logs either side; of the rows: the charges' costs, the rows
    /// replaced, the cut points; of the rest: the car's state now.
    pub window_parts_us: [u64; WINDOW_PARTS],
    /// Windows whose bounds were both found near their log ([`Options::window_look_ms`]).
    pub windows_bounded_near: u64,
    /// GPS fixes the timeline rebuilds left out since the server started (a placeholder, half a fix, or a jump
    /// faster than a car drives from the fixes around it): rebuilding a window counts its fixes again.
    pub gps_fixes_dropped: u64,
    pub last_error: Option<String>,
}

/// The parts of a window's time counted ([`EngineStats::window_parts_us`]).
pub const WINDOW_PARTS: usize = 12;

#[derive(Default)]
struct Counters {
    decoded: AtomicU64,
    left_out: AtomicU64,
    windows: AtomicU64,
    queued: AtomicU64,
    failures: AtomicU64,
    decode_ms: AtomicU64,
    rebuild_ms: AtomicU64,
    read_us: AtomicU64,
    samples_us: AtomicU64,
    now_us: AtomicU64,
    read_ahead: AtomicU64,
    gps_fixes_dropped: AtomicU64,
    window_parts: [AtomicU64; WINDOW_PARTS],
    bounded_near: AtomicU64,
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
    /// An API input's values beside its samples ([`tesla_in`]).
    ApiMeta {
        input: model::Input,
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
    /// The places' prices from Supercharger invoices worked out again ([`invoice_prices::run`]).
    InvoicePrices,
}

/// A car's newest values as the live stream gets them while a rebuild of the car runs.
#[derive(Debug, Default)]
struct NowSent {
    /// Rebuilds of the car under way.
    rebuilding: u32,
    /// When the last one was sent during them.
    last_ms: Option<u64>,
    /// One was held back: the rebuild's end sends it.
    held: bool,
}

/// A rebuild of a car under way ([`Engine::rebuilding`]): ends with the newest values the throttle held back sent.
pub(crate) struct Rebuilding {
    e: Engine,
    car: CarId,
}

impl Drop for Rebuilding {
    fn drop(&mut self) {
        let held = {
            let mut m = lock(&self.e.inner.now_sent);
            let n = m.entry(self.car).or_default();
            n.rebuilding = n.rebuilding.saturating_sub(1);
            let held = n.rebuilding == 0 && std::mem::take(&mut n.held);
            if n.rebuilding == 0 {
                m.remove(&self.car);
            }
            held
        };
        if held {
            self.e.emit_now(self.car);
        }
    }
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
    /// The logs told of and not yet taken, per CAN key, in the order the judge told them: each [`Work::Log`] works
    /// on the first of its key's once it holds the car's lock, so a logger's logs are worked on in the order they
    /// came whichever worker gets the lock first ([`Engine::work_log`]).
    pub(crate) told: Arc<Mutex<HashMap<String, std::collections::VecDeque<String>>>>,
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
    /// Per car, the newest rebuilt stretch whose inputs' fixes disagreed ([`timeline::fuse::gps_cross_check`]).
    pub(crate) gps_cross: Mutex<HashMap<CarId, GpsDisagreement>>,
    /// Per car, how the newest values reach the live stream while a rebuild of it runs ([`Engine::send_now`]).
    now_sent: Mutex<HashMap<CarId, NowSent>>,
    /// Answers' parts kept while their data stays the same.
    pub(crate) memo: memo::Memo,
    /// The cars whose long periods wait to be worked out in the background ([`warm`]).
    pub(crate) warm: Mutex<warm::Warm>,
    /// The cars whose days' pack readings are being worked out ([`query::pack_logs`]).
    pub(crate) packs: query::pack_logs::Fills,
    /// The SMO samples of the days being worked on ([`hot`]).
    pub(crate) hot: Mutex<hot::Hot>,
    /// The compaction pass under way ([`compact`]), whether it was asked to stop, and whether passes start by
    /// themselves.
    pub(crate) compaction: Mutex<compact::Running>,
    pub(crate) compact_stop: std::sync::atomic::AtomicBool,
    pub(crate) compact_steps: Mutex<Vec<Arc<dyn compact::Step>>>,
    pub(crate) compact_auto: std::sync::atomic::AtomicBool,
    /// The full rebuild under way ([`full`]); its record's changes, one at a time; a test's failure point.
    pub(crate) full: Mutex<full::Full>,
    /// The counters as last read for [`Engine::rebuild_speed`].
    pub(crate) speeds: Mutex<speed::Speeds>,
    pub(crate) full_start: Mutex<()>,
    /// The server is stopping ([`Engine::stopping`]): the full rebuild leaves its run as it stands.
    pub(crate) stopping: std::sync::atomic::AtomicBool,
    pub(crate) full_fail_at: Mutex<Option<full::Phase>>,
    pub(crate) full_crash_at: Mutex<Option<full::Phase>>,
    pub(crate) full_hold_at: Mutex<Option<full::Phase>>,
    /// The windows the full rebuilds' decoding rebuilt beside the history (their own engine's count, added up).
    pub(crate) full_windows: AtomicU64,
    /// Tests: the full rebuild's worker stops as by a crash once it decoded this many logs of a car.
    pub(crate) full_crash_logs: Mutex<Option<u64>>,
    /// Tests: called by the full rebuild's worker when its run ended, before it goes.
    pub(crate) full_exit_hook: Mutex<Option<full::ExitHook>>,
    /// Tests: called by a rebuild job just before it clears the narrow rebuilds' notes it did.
    pub(crate) job_clear_hook: Mutex<Option<full::ExitHook>>,
    pub(crate) full_auto: std::sync::atomic::AtomicBool,
    /// How long a new version's full rebuild waits before it starts ([`Engine::full_rebuild_auto_plan`]'s grace).
    pub(crate) full_grace_ms: AtomicU64,
    /// The cars whose history was made with another decoding ([`Engine::version_stale`]).
    pub(crate) version_stale: Mutex<std::collections::HashSet<CarId>>,
    /// The invoice sessions as kept ([`invoices`]), while none changes; one writer at a time; bumped by each write.
    pub(crate) invoice_all: Mutex<Option<Arc<std::collections::BTreeMap<String, invoices::Session>>>>,
    pub(crate) invoice_write: Mutex<()>,
    pub(crate) invoice_gen: AtomicU64,
    /// A run of the places' invoice prices is queued ([`invoice_prices::run`]); one runs at a time.
    pub(crate) invoice_prices_queued: std::sync::atomic::AtomicBool,
    pub(crate) invoice_prices_run: Mutex<()>,
    /// The cars' GPS layout fits ([`gps_fit`]), and the one that runs (one at a time).
    pub(crate) gps_fits: Mutex<gps_fit::Fits>,
    pub(crate) gps_fit_one: Mutex<()>,
    /// How often a fit waiting for a rebuild looks again ([`gps_fit`]).
    pub(crate) gps_fit_wait: Mutex<std::time::Duration>,
    /// The rebuilds' history ([`history`]): one writer at a time.
    pub(crate) history: Mutex<()>,
    /// Rebuild jobs not started yet, per car (`None`: every car), that later asks join ([`jobs`]).
    pub(crate) pending_jobs: Mutex<HashMap<Option<CarId>, Arc<jobs::JobState>>>,
}

/// A stretch of a car's history where one input's GPS fixes disagreed with another's of the same moments by far
/// more than lag or noise (a decode mistake, a wrong car): `disagree` names the inputs out of step, `not_used`
/// those whose fixes were left out of positions there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct GpsDisagreement {
    #[serde(serialize_with = "input_words")]
    pub disagree: model::Inputs,
    #[serde(serialize_with = "input_words")]
    pub not_used: model::Inputs,
    pub from_ms: u64,
    pub to_ms: u64,
}

fn input_words<S: serde::Serializer>(i: &model::Inputs, s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(i.iter().map(model::Input::word))
}

fn gps_layout_key(car: CarId) -> String {
    format!("car/{car}/gps-layout")
}

fn gps_layout_set_key(car: CarId) -> String {
    format!("car/{car}/gps-layout-set")
}

impl Engine {
    /// The car's own layout of the Screenmate's GPS frame (0x04F on its internal bus), when one is set: its logs
    /// are decoded with it instead of the DBC's.
    pub fn gps_layout(&self, car: CarId) -> Result<Option<model::gps::GpsLayout>> {
        Ok(records::get::<Option<model::gps::GpsLayout>>(&self.inner.db, records::NS, &gps_layout_key(car))?.flatten())
    }

    /// Sets (or with `None` drops) the car's own layout of the Screenmate's GPS frame. When it changes, the car's
    /// positions are read again: only its stored logs whose positions came from that frame are decoded again with it
    /// (the others read the same whatever the layout), after a short settle so a few saves in a row are one rebuild
    /// ([`jobs`]). The job is returned.
    pub fn set_gps_layout(&self, car: CarId, layout: Option<model::gps::GpsLayout>) -> Result<Option<jobs::Job>> {
        self.set_gps_layout_by(car, layout, None, None)
    }

    /// [`Engine::set_gps_layout`], noting who set it (`by`, an account's name) and the fit run it came from (`fit`)
    /// in the car's list of layouts over time ([`Engine::gps_layout_changes`]).
    pub fn set_gps_layout_by(
        &self,
        car: CarId,
        layout: Option<model::gps::GpsLayout>,
        by: Option<String>,
        fit: Option<u64>,
    ) -> Result<Option<jobs::Job>> {
        if layout.is_some_and(|l| !l.lat.valid() || !l.lon.valid() || l.accuracy.is_some_and(|a| !a.valid())) {
            return Err(Error::Store("a GPS field does not fit in 8 bytes".into()));
        }
        let before = self.gps_layout(car)?;
        if before == layout {
            return Ok(None);
        }
        records::put(&self.inner.db, records::NS, &gps_layout_key(car), &layout)?;
        records::put(&self.inner.db, records::NS, &gps_layout_set_key(car), &now_ms())?;
        self.gps_layout_note(
            car,
            gps_fit_runs::LayoutChange {
                at_ms: now_ms(),
                by,
                layout: layout.map(|l| l.to_string()),
                before: before.map(|l| l.to_string()),
                fit,
            },
        )?;
        // noted first: a restart before the job did it does it then
        self.note_redo(car, records::Redo::Positions)?;
        Ok(Some(self.rebuild_because(Scope::car(car), jobs::What::POSITIONS, versions::Cause::gps_layout(), true)?))
    }

    /// When the car's GPS layout was last changed (set, or dropped back to the DBC's) through
    /// [`Engine::set_gps_layout`], in ms; `None` when it never was (or before this was kept).
    pub fn gps_layout_set_ms(&self, car: CarId) -> Result<Option<u64>> {
        records::get::<u64>(&self.inner.db, records::NS, &gps_layout_set_key(car))
    }

    /// The newest stretch of the car's history whose inputs' GPS fixes disagreed (since the server started).
    pub fn gps_disagreement(&self, car: CarId) -> Option<GpsDisagreement> {
        lock(&self.inner.gps_cross).get(&car).copied()
    }
}

/// Where a car's zone was last looked up (latitude, longitude) and the zone found there.
type ZoneAt = (f64, f64, Option<&'static str>);

/// A car's zone is looked up again once its position moved this far (degrees, about a kilometre).
const ZONE_MOVE_DEG: f64 = 0.01;

/// How much lower the engine's background work runs than the server's other threads ([`Engine::background`]): the
/// uploads' judging, the web pages. Compaction runs lower still (19).
pub const BACKGROUND_NICE: i32 = 10;

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
                told: Arc::default(),
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
                gps_cross: Mutex::new(HashMap::new()),
                now_sent: Mutex::new(HashMap::new()),
                memo: memo::Memo::default(),
                warm: Mutex::default(),
                packs: Default::default(),
                hot: Mutex::new(hot::Hot::default()),
                compaction: Mutex::default(),
                compact_stop: std::sync::atomic::AtomicBool::new(false),
                compact_steps: Mutex::default(),
                compact_auto: std::sync::atomic::AtomicBool::new(false),
                full: Mutex::default(),
                speeds: Mutex::default(),
                full_start: Mutex::new(()),
                stopping: std::sync::atomic::AtomicBool::new(false),
                full_fail_at: Mutex::new(None),
                full_crash_at: Mutex::new(None),
                full_hold_at: Mutex::new(None),
                full_windows: AtomicU64::new(0),
                full_crash_logs: Mutex::new(None),
                full_exit_hook: Mutex::new(None),
                job_clear_hook: Mutex::new(None),
                full_auto: std::sync::atomic::AtomicBool::new(true),
                full_grace_ms: AtomicU64::new(full::AUTO_GRACE_MS),
                version_stale: Mutex::default(),
                invoice_all: Mutex::new(None),
                invoice_write: Mutex::new(()),
                invoice_gen: AtomicU64::new(0),
                invoice_prices_queued: std::sync::atomic::AtomicBool::new(false),
                invoice_prices_run: Mutex::new(()),
                gps_fits: Mutex::default(),
                gps_fit_one: Mutex::new(()),
                gps_fit_wait: Mutex::new(std::time::Duration::from_secs(1)),
                history: Mutex::new(()),
                pending_jobs: Mutex::new(HashMap::new()),
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

    /// The order a car's live inputs win in where two cover the same time: its `source_order` setting, else the
    /// default (SMO, the Tesla API, TeslaFi, local TeslaMate).
    pub fn source_order(&self, car: model::CarId) -> model::SourceOrder {
        query::fleet::source_order_of(self, car)
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

    /// The last contact of each car key's CAN Bridge (its last request the upload server accepted), when tokens
    /// were given: the upload server's live values when they share its contacts ([`ingest::Tokens::with_contacts`]),
    /// else what it last saved. `None`: no tokens given (pages then go by when the newest log arrived).
    pub fn contacts(&self) -> Option<&Arc<ingest::Contacts>> {
        self.tokens().map(|t| t.contacts())
    }

    /// What to give [`ingest::Judge::on_judged`]: it notes the log and returns at once.
    pub fn listener(&self) -> ingest::OnJudged {
        let (tx, outstanding, told) = (self.inner.tx.clone(), self.inner.outstanding.clone(), self.inner.told.clone());
        Arc::new(move |j: &ingest::Judged| {
            // (a send fails only once the engine is gone; the next start's reconcile finds the log)
            outstanding.fetch_add(1, Ordering::SeqCst);
            // noted in the order told, before the work goes out (see `told`)
            let mut t = lock(&told);
            t.entry(j.key.clone()).or_default().push_back(j.name.clone());
            if tx.send(Work::Log { key: j.key.clone(), name: j.name.clone() }).is_err() {
                outstanding.fetch_sub(1, Ordering::SeqCst);
                if let Some(q) = t.get_mut(&j.key) {
                    q.pop_back();
                }
            }
        })
    }

    /// Every change from now on. A receiver that falls [`Options::event_capacity`] behind misses events (it is told
    /// so by `RecvError::Lagged`); the page then loads what it shows again.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    /// Marks `car` as being rebuilt until the answer is dropped.
    pub(crate) fn rebuilding(&self, car: CarId) -> Rebuilding {
        lock(&self.inner.now_sent).entry(car).or_default().rebuilding += 1;
        Rebuilding { e: self.clone(), car }
    }

    /// Sends the car's newest values to the live stream (worked out only while someone listens: they read the car's
    /// last 30 days of drives, imported ones included). While a rebuild of the car runs, at most once per
    /// [`Options::rebuild_now_gap_ms`]: a window finishing sooner holds it back, and the rebuild's end sends it, so
    /// the last state always arrives. Outside a rebuild every call sends.
    pub(crate) fn send_now(&self, car: CarId) {
        if self.inner.events.receiver_count() == 0 {
            return;
        }
        {
            let mut m = lock(&self.inner.now_sent);
            if let Some(n) = m.get_mut(&car).filter(|n| n.rebuilding > 0) {
                let now = (self.inner.opts.clock)();
                let gap = self.inner.opts.rebuild_now_gap_ms;
                if n.last_ms.is_some_and(|t| now.saturating_sub(t) < gap) {
                    n.held = true;
                    return;
                }
                n.last_ms = Some(now);
                n.held = false;
            }
        }
        self.emit_now(car);
    }

    fn emit_now(&self, car: CarId) {
        if let Ok(now) = query::now::now_view(self, car) {
            self.emit(Event::Now { car, now: Box::new(now) });
        }
    }

    pub(crate) fn emit(&self, e: Event) {
        // charges rebuilt where invoiced sessions are: their matches, and so the places' invoice prices, may move
        if let Event::Rows { car, kinds, from_ms, to_ms } = &e
            && kinds.contains(&"charges")
        {
            self.invoice_rows_changed(*car, *from_ms, *to_ms);
        }
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
        // the cars whose history was made with another decoding: held for a full rebuild, not redone in place
        self.check_versions();
        self.send(Work::Reconcile { car: None });
        // the places' prices from invoices, as the sessions and charges kept now give them
        self.invoice_prices_soon();
        // a full rebuild a restart cut off carries on
        self.full_resume();
        // the days' pack readings, worked out once idle (and again now and then)
        handle.spawn(query::pack_logs::passes(self.clone()));
        Ok(())
    }

    pub(crate) fn send(&self, w: Work) {
        self.inner.outstanding.fetch_add(1, Ordering::SeqCst);
        if self.inner.tx.send(w).is_err() {
            self.inner.outstanding.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Runs `f` on a thread of its own below the server's other threads (nice [`BACKGROUND_NICE`]): the decoding,
    /// rebuilds and full rebuilds still take every core nothing else wants, but checking an upload or answering a
    /// page goes first. On the blocking threads of `rt` should a thread not start.
    pub(crate) fn background(rt: &tokio::runtime::Handle, name: &str, f: impl FnOnce() + Send + 'static) {
        let job = Arc::new(Mutex::new(Some(f)));
        let mine = job.clone();
        let spawned = std::thread::Builder::new().name(name.into()).spawn(move || {
            #[cfg(unix)]
            let _ = rustix::process::nice(BACKGROUND_NICE);
            if let Some(f) = lock(&mine).take() {
                f();
            }
        });
        if spawned.is_err()
            && let Some(f) = lock(&job).take()
        {
            drop(rt.spawn_blocking(f));
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
            let rt = tokio::runtime::Handle::current();
            Engine::background(&rt, "engine-work", move || {
                let r = match w {
                    Work::Log { key, name } => e.work_log(&key, &name),
                    Work::Reconcile { car } => e.work_reconcile(car),
                    Work::ApiData { input, car, samples, facts } => e.work_api_data(input, car, samples, facts),
                    Work::ApiMeta { input, car, meta } => e.work_api_meta(input, car, *meta),
                    Work::ApiBuild { car } => e.work_api_build(car),
                    Work::OffCan { input, car, at_ms, values } => e.work_off_can(input, car, at_ms, *values),
                    Work::InvoicePrices => invoice_prices::run(&e),
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
            read_us_total: c.read_us.load(Ordering::Relaxed),
            samples_us_total: c.samples_us.load(Ordering::Relaxed),
            now_us_total: c.now_us.load(Ordering::Relaxed),
            logs_read_ahead: c.read_ahead.load(Ordering::Relaxed),
            window_parts_us: std::array::from_fn(|i| c.window_parts[i].load(Ordering::Relaxed)),
            windows_bounded_near: c.bounded_near.load(Ordering::Relaxed),
            gps_fixes_dropped: c.gps_fixes_dropped.load(Ordering::Relaxed),
            last_error: lock(&self.inner.last_error).clone(),
        }
    }

    /// [`Self::stats`] with the counters of the full rebuild's decoding under way added (it runs on an engine of its
    /// own, over the shadow): what the logs being worked on took, stage by stage (the rebuild speed).
    pub fn work_stats(&self) -> EngineStats {
        let mut s = self.stats();
        let shadow = lock(&self.inner.full).shadow.clone();
        if let Some(se) = shadow {
            let t = se.stats();
            s.logs_decoded += t.logs_decoded;
            s.logs_left_out += t.logs_left_out;
            s.windows_rebuilt += t.windows_rebuilt;
            s.decode_ms_total += t.decode_ms_total;
            s.rebuild_ms_total += t.rebuild_ms_total;
            s.read_us_total += t.read_us_total;
            s.samples_us_total += t.samples_us_total;
            s.now_us_total += t.now_us_total;
            s.logs_read_ahead += t.logs_read_ahead;
            s.windows_bounded_near += t.windows_bounded_near;
            for (a, b) in s.window_parts_us.iter_mut().zip(t.window_parts_us) {
                *a += b;
            }
        }
        s
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
