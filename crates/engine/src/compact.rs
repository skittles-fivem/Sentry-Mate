//! Compaction: packing what is no longer written as small as it goes, without changing a value (docs/plans/storage.md).
//! A pass has three steps:
//!
//! - **logs**: the uploaded logs and sidecars of finished days go into one sealed pack per car and day, byte for byte
//!   ([`ingest::Store::pack_finished`]);
//! - **series**: journals of finished days are folded into their snapshots at the cold level, the finished months'
//!   days packed into one file a month, the tables' journals folded ([`db::Db::compact_series`]);
//! - **records**: the key/value store's small records go into bundles ([`db::Kv::compact`]).
//!
//! Each step writes the new file, reads it back and compares it with what it took in before anything is removed, so
//! a pass stopped anywhere (a crash, a restart) leaves every value readable, and the next pass carries on.
//!
//! It runs by itself when the server is idle ([`Engine::compact_automatically`]): on a thread of its own at the
//! lowest priority, one small unit at a time with a pause after each, giving way as soon as logs wait to be decoded.
//! "Compact now" ([`Engine::compact_start`]) runs the same pass without the pauses. Every unit holds a pass of the
//! data folder's write gate, so an export's snapshot or a move sees one moment.

use crate::{Engine, Result, lock, now_ms, records};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Days that ended this long ago are packed (a day still receiving uploads keeps its journal).
pub const FINISHED_AFTER_MS: u64 = 36 * 3_600_000;
/// The first automatic pass comes this long after the start, the next ones this long after the last.
pub const AUTO_FIRST: Duration = Duration::from_secs(10 * 60);
pub const AUTO_EVERY: Duration = Duration::from_secs(6 * 3600);
/// A pass that gave way to other work is tried again after this long.
pub const AUTO_RETRY: Duration = Duration::from_secs(10 * 60);
/// Days of a car's series worked on per hold of its lock.
const DAYS_A_TURN: usize = 16;
/// How long a unit waits for the write gate (an export's snapshot holds it for a moment).
const GATE_WAIT: Duration = Duration::from_secs(30);
const KEY: &str = "compaction";

/// What a pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Compacted {
    /// Files of uploaded logs and sidecars taken into packs.
    pub log_files: u64,
    /// Days of time series folded or packed.
    pub days: u64,
    /// Kept records and answers taken into bundles.
    pub records: u64,
    /// Bytes on disk before and after, of what the pass looked at.
    pub before_bytes: u64,
    pub after_bytes: u64,
    /// Stopped early (asked to, or the server got busy): the next pass carries on.
    pub stopped: bool,
}

impl Compacted {
    pub fn saved_bytes(&self) -> u64 {
        self.before_bytes.saturating_sub(self.after_bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Idle,
    Running,
    Failed,
}

/// Compaction as it is now, for the Storage panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Compaction {
    pub status: Status,
    /// It runs by itself when the server is idle.
    pub automatic: bool,
    /// The pass under way, else the last one.
    pub started_ms: Option<u64>,
    pub finished_ms: Option<u64>,
    /// While running: `logs`, `series` or `records`, and how far it is.
    pub step: Option<&'static str>,
    pub done: u64,
    pub total: u64,
    /// What the last finished pass freed, and all passes together.
    pub last_saved_bytes: Option<u64>,
    pub saved_bytes: u64,
    pub error: Option<String>,
}

/// What is kept of the passes so far (`engine compaction` in the key/value store).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Kept {
    started_ms: Option<u64>,
    finished_ms: Option<u64>,
    last_saved_bytes: Option<u64>,
    saved_bytes: u64,
    error: Option<String>,
}

/// The pass under way, in memory.
#[derive(Default)]
pub(crate) struct Running {
    running: bool,
    automatic: bool,
    started_ms: u64,
    step: Option<&'static str>,
    done: u64,
    total: u64,
    thread: Option<std::thread::JoinHandle<()>>,
    /// When the last pass of this run ended, and whether it gave way before its end (or failed).
    finished: Option<Instant>,
    gave_way: bool,
}

/// Bytes on disk of everything under `dir`.
fn tree_disk(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let mut n = 0;
    for f in rd.flatten() {
        let Ok(t) = f.file_type() else { continue };
        if t.is_dir() {
            n += tree_disk(&f.path());
        } else if t.is_file() {
            n += f.metadata().map_or(0, |m| db::allocated(&m));
        }
    }
    n
}

/// Between two units of a pass: lets go of the write gate, pauses (an automatic pass), and takes the gate again.
struct Turns<'a> {
    e: &'a Engine,
    gate: Option<gate::Gate>,
    pass: RefCell<Option<gate::Pass>>,
    worked_since: RefCell<Instant>,
    /// An automatic pass: pauses, and gives way to logs waiting.
    polite: bool,
    stop: &'a dyn Fn() -> bool,
}

impl Turns<'_> {
    /// Whether to stop. Otherwise the gate is held again when this returns.
    fn next(&self) -> bool {
        *self.pass.borrow_mut() = None;
        if (self.stop)() || self.e.inner.compact_stop.load(Ordering::SeqCst) {
            return true;
        }
        if self.polite {
            if self.e.queued() > 0 {
                return true;
            }
            // twice the time worked, so a pass takes a third of a core and of the disk at most
            let worked = self.worked_since.borrow().elapsed();
            std::thread::sleep((worked * 2).min(Duration::from_secs(2)));
        }
        *self.worked_since.borrow_mut() = Instant::now();
        match &self.gate {
            Some(g) => match g.enter(GATE_WAIT) {
                Ok(p) => {
                    *self.pass.borrow_mut() = Some(p);
                    false
                }
                // a pause that lasts, or the folder is closed to writes: another time
                Err(gate::Refused::Busy | gate::Refused::Closed(_) | gate::Refused::Io(_)) => true,
            },
            None => false,
        }
    }
}

impl Engine {
    fn kept_compaction(&self) -> Kept {
        records::kept(self.db(), records::NS, KEY).ok().flatten().unwrap_or_default()
    }

    /// Compaction as it is now.
    pub fn compaction(&self) -> Compaction {
        let kept = self.kept_compaction();
        let r = lock(&self.inner.compaction);
        Compaction {
            status: match (r.running, &kept.error) {
                (true, _) => Status::Running,
                (false, Some(_)) => Status::Failed,
                (false, None) => Status::Idle,
            },
            automatic: self.inner.compact_auto.load(Ordering::SeqCst),
            started_ms: if r.running { Some(r.started_ms) } else { kept.started_ms },
            finished_ms: if r.running { None } else { kept.finished_ms },
            step: r.step.filter(|_| r.running),
            done: if r.running { r.done } else { 0 },
            total: if r.running { r.total } else { 0 },
            last_saved_bytes: kept.last_saved_bytes,
            saved_bytes: kept.saved_bytes,
            error: if r.running { None } else { kept.error },
        }
    }

    fn progress(&self, step: &'static str, done: u64, total: u64) {
        let mut r = lock(&self.inner.compaction);
        r.step = Some(step);
        r.done = done;
        r.total = total.max(done);
    }

    /// One pass, here and now (the caller's thread). `stop` is asked between units; `polite` pauses after each and
    /// gives way to logs waiting to be decoded.
    pub fn compact(&self, stop: &dyn Fn() -> bool, polite: bool) -> Result<Compacted> {
        let data = self.store().dir().parent().map(Path::to_path_buf);
        let gate = match &data {
            Some(d) => Some(gate::Gate::open(d).map_err(|e| crate::Error::Store(format!("the write gate: {e}")))?),
            None => None,
        };
        let turns =
            Turns { e: self, gate, pass: RefCell::new(None), worked_since: RefCell::new(Instant::now()), polite, stop };
        let mut out = Compacted::default();
        let before_ms = now_ms().saturating_sub(FINISHED_AFTER_MS);
        if turns.next() {
            out.stopped = true;
            return Ok(out);
        }
        // the uploaded logs and sidecars of finished days
        self.progress("logs", 0, 0);
        let can = self.store().dir().to_path_buf();
        out.before_bytes += tree_disk(&can);
        let packed = self.store().pack_finished(before_ms, &|| turns.next());
        out.after_bytes += tree_disk(&can);
        if packed.failed > 0 {
            tracing::warn!(
                "compaction: {} day(s) of logs could not be packed and were left exactly as they were",
                packed.failed
            );
        }
        out.log_files = packed.files as u64;
        out.stopped = packed.stopped;
        // the time series and tables, car by car
        let cars = crate::query::all_cars(self).map_err(|e| crate::Error::Store(e.to_string()))?;
        let cars_dir = data.as_deref().map(|d| d.join("db").join("cars"));
        if !out.stopped {
            let mut total = 0;
            for &car in &cars {
                total += self.db().series_pending(car, before_ms)? as u64;
            }
            self.progress("series", 0, total);
            'cars: for &car in &cars {
                let dir = cars_dir.as_ref().map(|d| d.join(car.to_string()));
                out.before_bytes += dir.as_deref().map_or(0, tree_disk);
                loop {
                    let done = self.db().compact_series(car, before_ms, DAYS_A_TURN);
                    let done = match done {
                        Ok(d) => d,
                        // a car removed meanwhile
                        Err(db::Error::NoSuchCar(_)) => break,
                        Err(e) => return Err(e.into()),
                    };
                    out.days += (done.days_folded + done.days_packed) as u64;
                    self.progress("series", out.days, total);
                    if turns.next() {
                        out.stopped = true;
                        out.after_bytes += dir.as_deref().map_or(0, tree_disk);
                        break 'cars;
                    }
                    if !done.more {
                        // its days are done: the tables' journals folded into their snapshots
                        match self.db().settle(car, before_ms) {
                            Ok(_) | Err(db::Error::NoSuchCar(_)) => {}
                            Err(e) => return Err(e.into()),
                        }
                        break;
                    }
                }
                out.after_bytes += dir.as_deref().map_or(0, tree_disk);
            }
        }
        // the key/value store's records
        if !out.stopped {
            let kv_dir = data.as_deref().map(|d| d.join("db").join("kv"));
            let pending = self.db().kv().pending()?;
            self.progress("records", 0, pending);
            out.before_bytes += kv_dir.as_deref().map_or(0, tree_disk);
            let kv = self.db().kv().compact(&|| turns.next())?;
            out.after_bytes += kv_dir.as_deref().map_or(0, tree_disk);
            out.records = kv.records;
            out.stopped = kv.stopped;
            self.progress("records", kv.records, pending);
        }
        Ok(out)
    }

    /// Runs a pass and writes down how it went.
    fn compact_and_keep(&self, polite: bool) -> Option<Compacted> {
        let started = lock(&self.inner.compaction).started_ms;
        let r = self.compact(&|| false, polite);
        let mut kept = self.kept_compaction();
        kept.started_ms = Some(started);
        kept.finished_ms = Some(now_ms());
        match &r {
            Ok(c) => {
                kept.error = None;
                kept.last_saved_bytes = Some(c.saved_bytes());
                kept.saved_bytes = kept.saved_bytes.saturating_add(c.saved_bytes());
                tracing::info!(
                    "compaction: {} log files, {} days, {} records packed; {} bytes freed{}",
                    c.log_files,
                    c.days,
                    c.records,
                    c.saved_bytes(),
                    if c.stopped { " (gave way: the rest another time)" } else { "" }
                );
            }
            Err(e) => {
                tracing::warn!("compaction stopped: {e} (nothing was lost: what it had not packed stays as it was)");
                kept.error = Some(e.to_string());
            }
        }
        if let Err(e) = records::put(self.db(), records::NS, KEY, &kept) {
            tracing::warn!("compaction: its result was not written down: {e}");
        }
        r.ok()
    }

    /// Starts a pass in the background unless one runs (`polite`: an automatic one). The answer is the compaction as
    /// it is right after.
    pub fn compact_start(&self, polite: bool) -> Result<Compaction> {
        // a server whose engine doesn't run (its data moved away) doesn't change its data
        self.handle()?;
        {
            let mut r = lock(&self.inner.compaction);
            if !r.running {
                if let Some(t) = r.thread.take() {
                    let _ = t.join();
                }
                r.running = true;
                r.automatic = polite;
                r.started_ms = now_ms();
                (r.step, r.done, r.total) = (None, 0, 0);
                self.inner.compact_stop.store(false, Ordering::SeqCst);
                let e = self.clone();
                let spawned = std::thread::Builder::new().name("compaction".into()).spawn(move || {
                    // the lowest priority (this thread's only): the system gives its disk work the lowest too
                    #[cfg(unix)]
                    let _ = rustix::process::nice(19);
                    let done = e.compact_and_keep(polite);
                    let mut r = lock(&e.inner.compaction);
                    r.running = false;
                    r.gave_way = done.is_none_or(|c| c.stopped);
                    r.finished = Some(Instant::now());
                });
                match spawned {
                    Ok(t) => r.thread = Some(t),
                    Err(e) => {
                        r.running = false;
                        return Err(crate::Error::Store(format!("starting compaction: {e}")));
                    }
                }
            }
        }
        Ok(self.compaction())
    }

    /// Starts the automatic passes (call once, inside the engine's runtime): the first [`AUTO_FIRST`] after the
    /// start, then one every [`AUTO_EVERY`], each only while no log waits to be decoded and no rebuild runs.
    pub fn compact_automatically(&self) {
        let Ok(rt) = self.handle() else { return };
        self.inner.compact_auto.store(true, Ordering::SeqCst);
        let e = self.clone();
        rt.spawn(async move {
            tokio::time::sleep(AUTO_FIRST).await;
            loop {
                let due = {
                    let r = lock(&e.inner.compaction);
                    !r.running
                        && r.finished.is_none_or(|t| t.elapsed() >= if r.gave_way { AUTO_RETRY } else { AUTO_EVERY })
                };
                let busy = e.queued() > 0 || e.jobs().iter().any(|j| j.status == crate::jobs::JobStatus::Running);
                if due
                    && !busy
                    && let Err(err) = e.compact_start(true)
                {
                    tracing::debug!("compaction not started: {err}");
                }
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }

    /// Stops a pass under way (at its next unit) and waits for it: before the data folder is switched or the server
    /// stops.
    pub fn compact_stop(&self) {
        self.inner.compact_stop.store(true, Ordering::SeqCst);
        let t = lock(&self.inner.compaction).thread.take();
        if let Some(t) = t {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use model::{CarId, ChargePoint, Sample, Source};
    use std::cell::Cell;
    use std::sync::Arc;

    const DAY: u64 = 86_400_000;

    fn open(data: &Path, v: &Arc<vault::Vault>) -> Engine {
        let store = ingest::Store::new(data.join("can"), v.clone());
        let db = db::Db::open(data, v.clone(), db::Options::default()).unwrap();
        Engine::new(db, store, crate::Options::default())
    }

    /// Everything read back, to compare before and after: every point of both series, and every record.
    #[derive(Debug, PartialEq)]
    struct Read {
        samples: Vec<Sample>,
        live: Vec<Sample>,
        curve: Vec<ChargePoint>,
        records: Vec<(String, Option<Vec<u8>>)>,
    }

    fn read(e: &Engine, car: CarId) -> Read {
        let db = e.db();
        let mut records = Vec::new();
        for ns in ["engine", "day-parts"] {
            for k in db.kv().keys(ns).unwrap() {
                // the pass's own note of how it went is not what it packs
                if k != KEY {
                    records.push((format!("{ns} {k}"), db.kv().get(ns, &k).unwrap()));
                }
            }
        }
        Read {
            samples: db.read_points(car, Source::Teslamate, 0, u64::MAX).unwrap(),
            live: db.read_points(car, Source::Live, 0, u64::MAX).unwrap(),
            curve: db.read_points(car, Source::Teslamate, 0, u64::MAX).unwrap(),
            records,
        }
    }

    /// A made-up car with 70 days of an import (whole days), live windows that left journals, charge curves, and
    /// small records; all of it long over.
    fn made_up(e: &Engine) -> CarId {
        let car = e
            .db()
            .create_car(model::Car {
                id: 0,
                name: "Made-up Car".into(),
                vehicle_keys: Vec::new(),
                preferred_key: None,
                vin: None,
                profile_setting: None,
                time_zone: "UTC".into(),
                time_zone_typed: Some(true),
                efficiency_wh_km: None,
                created_ms: 0,
                inputs: Default::default(),
            })
            .unwrap()
            .id;
        let day0 = (now_ms() / DAY - 100) * DAY;
        for d in 0..70u64 {
            let start = day0 + d * DAY;
            let pts: Vec<Sample> = (0..300u64)
                .map(|i| Sample {
                    t_ms: start + i * 60_000,
                    soc: Some(40.0 + ((i + d) % 50) as f64),
                    speed_kmh: Some(((i * 7 + d) % 130) as f64),
                    odometer_km: Some(12_000.0 + (d * 300 + i) as f64 * 0.137),
                    outside_temp_c: Some(((i + d) % 300) as f64 / 10.0),
                    ..Sample::default()
                })
                .collect();
            e.db().write_points(car, Source::Teslamate, start, start + DAY, &pts).unwrap();
            // live windows of a few minutes each: journals
            for w in 0..3u64 {
                let a = start + w * 3_600_000;
                let pts: Vec<Sample> = (0..120u64)
                    .map(|i| Sample {
                        t_ms: a + i * 1000,
                        soc: Some(55.5),
                        speed_kmh: Some(i as f64),
                        ..Sample::default()
                    })
                    .collect();
                e.db().write_points(car, Source::Live, a, a + 120_000, &pts).unwrap();
            }
            if d % 3 == 0 {
                let pts: Vec<ChargePoint> = (0..40u64)
                    .map(|i| ChargePoint {
                        t_ms: start + i * 15_000,
                        power_kw: Some(7.0),
                        soc: Some(50.0 + i as f64),
                        ..ChargePoint::default()
                    })
                    .collect();
                e.db().write_points(car, Source::Teslamate, start, start + DAY, &pts).unwrap();
            }
            e.db()
                .kv()
                .put("engine", &format!("car/{car}/day/{d}"), format!("{{\"logs\":{d}}}").repeat(20).as_bytes())
                .unwrap();
            e.db().kv().put("day-parts", &format!("car/{car}/series-day/{d}"), &[d as u8; 300]).unwrap();
        }
        car
    }

    #[test]
    fn a_pass_stopped_anywhere_and_a_restart_leave_every_value_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
        let v = Arc::new(v);
        let mut e = open(&data, &v);
        let car = made_up(&e);
        let want = read(&e, car);
        assert_eq!(want.samples.len(), 70 * 300);
        let before = crate::query::storage::categories(&e).unwrap().iter().map(|c| c.disk_bytes).sum::<u64>();
        // stopped after 1, 2, 3 ... units, each time followed by a restart: nothing reads differently
        let mut passes = 0;
        loop {
            let asked = Cell::new(0u32);
            let stop_after = passes + 1;
            let done = e.compact(&|| asked.replace(asked.get() + 1) >= stop_after, false).unwrap();
            drop(e);
            e = open(&data, &v);
            assert_eq!(read(&e, car), want, "after a pass stopped at its unit {stop_after}");
            passes += 1;
            if !done.stopped {
                break;
            }
            assert!(passes < 200, "the pass never ends");
        }
        assert!(passes > 3, "the pass was stopped at several points: {passes}");
        // a whole pass now has nothing left to do, and the data folder is smaller
        let again = e.compact(&|| false, false).unwrap();
        assert_eq!((again.days, again.records, again.log_files, again.stopped), (0, 0, 0, false), "{again:?}");
        assert_eq!(read(&e, car), want);
        let after = crate::query::storage::categories(&e).unwrap().iter().map(|c| c.disk_bytes).sum::<u64>();
        assert!(after * 2 < before, "{before} -> {after} bytes on disk");
        // writes after packing: read back with the rest, and packed by the next pass
        let day0 = (now_ms() / DAY - 100) * DAY;
        let late: Vec<Sample> = (0..60u64)
            .map(|i| Sample { t_ms: day0 + 50_000_000 + i * 1000, soc: Some(1.5), ..Sample::default() })
            .collect();
        e.db().write_points(car, Source::Teslamate, day0 + 50_000_000, day0 + 50_060_000, &late).unwrap();
        e.db().kv().put("engine", &format!("car/{car}/day/3"), b"changed").unwrap();
        let want2 = read(&e, car);
        assert_eq!(want2.samples.len(), want.samples.len() + 60);
        let third = e.compact(&|| false, false).unwrap();
        assert!(third.days >= 1 && third.records == 1, "{third:?}");
        drop(e);
        let e = open(&data, &v);
        assert_eq!(read(&e, car), want2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn compact_now_runs_in_the_background_and_keeps_what_it_saved() {
        let dir = tempfile::tempdir().unwrap();
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
        let v = Arc::new(v);
        let e = open(&data, &v);
        assert!(e.compact_start(false).is_err(), "not on an engine that doesn't run");
        let car = made_up(&e);
        let want = read(&e, car);
        e.start().unwrap();
        let c = e.compaction();
        assert_eq!((c.status, c.started_ms, c.saved_bytes, c.automatic), (Status::Idle, None, 0, false));
        let c = e.compact_start(false).unwrap();
        assert_eq!(c.status, Status::Running);
        assert_eq!(e.compact_start(false).unwrap().started_ms, c.started_ms, "no second pass");
        for _ in 0..1200 {
            if e.compaction().status != Status::Running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let c = e.compaction();
        assert_eq!((c.status, c.step, c.error.clone()), (Status::Idle, None, None), "{c:?}");
        assert!(c.finished_ms.is_some() && c.saved_bytes > 0 && c.last_saved_bytes == Some(c.saved_bytes), "{c:?}");
        assert_eq!(read(&e, car), want);
        // a stop asked for ends a pass at once and waits for it; what was saved so far is kept across a restart
        e.compact_start(true).unwrap();
        e.compact_stop();
        assert_ne!(e.compaction().status, Status::Running);
        drop(e);
        let e = open(&data, &v);
        assert_eq!(e.compaction().saved_bytes, c.saved_bytes);
        assert_eq!(read(&e, car), want);
    }
}
