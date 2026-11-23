//! A **full rebuild**: everything the server worked out for a car (or every car) thrown away and made again from the
//! raw inputs it keeps, beside the history in use, then switched to at once. For when the derived history is wrong
//! and nothing else would mend it: a decoder fixed (every stored CAN log decoded again with the DBC and decoder of
//! now), rules changed, or a history that went wrong some other way.
//!
//! **What is made again**: every stored CAN log decoded again (its samples, positions, charge points, firmware
//! fingerprints, the values no CAN message carries are the APIs' and stay as they came); the Tesla API's, TeslaFi's
//! and local TeslaMate's stored data fused in again; the timeline (states, drives, charges, visits, updates found
//! from fingerprints); the pack's day readings (battery and efficiency figures); and every kept working value of the
//! car is dropped (the long periods' day parts, the charges' import fills, the vehicle's build days), so nothing
//! stale is answered (the pages work them out again, and the background pass does it ahead). Places are matched
//! when read and statistics are worked out from the rows, so they follow.
//!
//! **What stays as it is**: the raw inputs (the CAN logs, their sidecars, signatures and trust records, the APIs'
//! stored samples and readings, imported rows: an import's source file is not kept, so its rows are what it left)
//! and everything an owner entered: charge costs (typed or kept from the price then), places and their prices,
//! typed versions and firmware names, the drives' purposes and rules, the car's settings, record and Now, overlap
//! choices, source order, sold status, accounts.
//!
//! **How** ([`Engine::full_rebuild`]): one car at a time, one full rebuild at a time. For each car a shadow db is
//! started ([`db::Db::shadow`]: the car's folder copied without its SMO series, its kept keys copied in), a second
//! engine over it ([`Engine::shadow_of`]) clears what was worked out from CAN logs and decodes every log again as a
//! rebuild does, then works out the pack's day readings, then the car is switched to it under its lock
//! ([`db::Db::adopt_car`], then its records: the derived keys replaced, the kept ones left). Until the switch the
//! pages read the old history (and new logs keep going into it: they are found not recorded after the switch and
//! decoded again then); a failure, a cancel or a crash before the switch leaves it as it was. The job's record is
//! kept (`engine` / `full-rebuild`), so a restart carries on where it was: decoding goes on with the logs the shadow
//! has not recorded, a switch cut off is finished.
//!
//! **Versions** ([`derived_version`]): the decoder's version, the timeline's rules and a digest of the DBC sets the
//! history was made with are kept per car (in its record's meta). A server whose decoding differs finds the cars made
//! with another at its start ([`Engine::version_stale`]); their logs are then not decoded again in place for that
//! alone, and a full rebuild is offered (or run at once, the server's choice: the web side's).

use crate::jobs::JobState;
use crate::records::{self, CarMeta, NS};
use crate::{Engine, Event, Result, Work, lock, now_ms};
use db::Db;
use model::{CarId, Source};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// The shadow's folder in the data folder (a dot first: a move or an export never carries it).
pub const SHADOW: &str = ".full-rebuild";
/// The job's record.
const KEY: &str = "full-rebuild";
/// A job's errors kept at most.
const MAX_ERRORS: usize = 50;
/// No progress for this long while running: the status says it is stalled.
pub const STALL_MS: u64 = 120_000;
/// A test's stop of the worker (see [`Engine::full_rebuild_crash_at`]).
const CRASHED: &str = "stopped as by a crash, for a test";
/// The job's record is written at most this often while it moves on (and at every phase).
const NOTE_EVERY_MS: u64 = 2_000;

/// The decoding and rules a history is made with: the decoder's version, the timeline's rules and a digest of the
/// DBC sets as parsed. A server that would decode stored logs otherwise has another.
pub fn derived_version() -> String {
    static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        let mut h = Sha256::new();
        for p in [dbc::Profile::Tesla3Y, dbc::Profile::TeslaSxLegacy] {
            h.update(p.name().as_bytes());
            let d = p.dbc();
            let mut ids: Vec<&u32> = d.messages.keys().collect();
            ids.sort();
            for id in ids {
                h.update(format!("{:?}", d.messages[id]).as_bytes());
            }
        }
        let dbc: String = h.finalize()[..4].iter().map(|b| format!("{b:02x}")).collect();
        format!("decode-{}.rules-{}.dbc-{dbc}", records::DECODE_VERSION, timeline::RULES_VERSION)
    })
    .clone()
}

/// The version a car's history was made with (none kept: made before versions were kept, or no log decoded yet).
pub fn car_version(db: &Db, car: CarId) -> Result<Option<String>> {
    Ok(records::meta(db, car)?.version)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Waiting for its turn (the first car not started yet).
    Queued,
    /// The car's raw inputs copied beside its history.
    Copying,
    /// Every stored CAN log decoded again.
    Decoding,
    /// The pack's day readings worked out.
    Days,
    /// The car switched to the new history.
    Switching,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FullStatus {
    Running,
    Done,
    Failed,
    Cancelled,
}

/// Why a full rebuild runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// An admin asked for it.
    Owner,
    /// The server's decoding changed since the history was made.
    Version,
}

/// Something that went wrong, said so a person can read it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FullError {
    pub car: Option<CarId>,
    /// The log it is about, when one.
    pub log: Option<String>,
    pub message: String,
    pub at_ms: u64,
}

/// What a car's history looked like when its shadow was started, to tell what changed meanwhile.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Seed {
    /// The newest data of each API input then.
    api_last: Vec<Option<u64>>,
}

/// A full rebuild as it is kept (and answered).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FullRecord {
    pub id: u64,
    /// Every car, or the one asked for.
    pub all: bool,
    pub cars: Vec<CarId>,
    /// The car under way (an index into `cars`; `cars.len()` once all are done).
    pub index: usize,
    pub phase: Phase,
    pub status: FullStatus,
    pub reason: Reason,
    /// Who asked (an account's name), for the audit log.
    pub by: Option<String>,
    pub version: String,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    /// When the car under way, and its phase, started.
    pub car_started_ms: u64,
    pub phase_started_ms: u64,
    /// The logs and their bytes, done and in all (adding up over the cars, each counted as it starts); the days
    /// of the pack readings, the same.
    pub logs_done: u64,
    pub logs_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub days_done: u64,
    pub days_total: u64,
    /// Bytes copied beside the history for the car under way.
    pub bytes_copied: u64,
    /// The logs done since this run (or this restart) of the phase began: the rate is of those.
    #[serde(default)]
    pub base_logs: u64,
    #[serde(default)]
    pub base_bytes: u64,
    #[serde(default)]
    pub base_days: u64,
    pub last_progress_ms: u64,
    pub errors: Vec<FullError>,
    /// Why it failed.
    pub error: Option<String>,
    /// An admin asked it to stop.
    pub cancel: bool,
    /// The audit log has its end.
    pub audited_end: bool,
    #[serde(default)]
    seed: Seed,
    /// Test only: fail at this phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fail_at: Option<Phase>,
}

impl FullRecord {
    /// Whether it is still under way.
    pub fn running(&self) -> bool {
        self.status == FullStatus::Running
    }

    /// The car under way.
    pub fn car(&self) -> Option<CarId> {
        self.cars.get(self.index).copied()
    }

    /// Bytes a second of the logs decoded in this run of the phase, and the time left (ms) at that rate.
    pub fn rate(&self, now: u64) -> (Option<f64>, Option<u64>) {
        let r = self.rates(now);
        (r.bytes_per_s, r.eta_ms)
    }

    /// The speeds of this run of the phase (logs and their bytes while decoding, days in the days step) and the time
    /// the phase has left: by the logs left at the logs' speed while decoding (the same measure as the percent
    /// shown), by the days left in the days step.
    pub fn rates(&self, now: u64) -> Rates {
        let secs = now.saturating_sub(self.phase_started_ms) as f64 / 1000.0;
        let per_s = |done: u64| (secs >= 1.0 && done > 0).then(|| done as f64 / secs);
        let left_ms = |left: u64, rate: Option<f64>| rate.map(|r| ((left as f64 / r) * 1000.0) as u64);
        match self.phase {
            Phase::Decoding => {
                let logs = per_s(self.logs_done.saturating_sub(self.base_logs));
                Rates {
                    bytes_per_s: logs.and(per_s(self.bytes_done.saturating_sub(self.base_bytes))),
                    logs_per_s: logs,
                    days_per_s: None,
                    eta_ms: left_ms(self.logs_total.saturating_sub(self.logs_done), logs),
                }
            }
            Phase::Days => {
                let days = per_s(self.days_done.saturating_sub(self.base_days));
                Rates {
                    bytes_per_s: None,
                    logs_per_s: None,
                    days_per_s: days,
                    eta_ms: left_ms(self.days_total.saturating_sub(self.days_done), days),
                }
            }
            Phase::Queued | Phase::Copying | Phase::Switching => Rates::default(),
        }
    }

    /// No progress for [`STALL_MS`] while running.
    pub fn stalled(&self, now: u64) -> bool {
        self.running() && now.saturating_sub(self.last_progress_ms) > STALL_MS
    }

    fn error(&mut self, car: Option<CarId>, log: Option<String>, message: String) {
        if self.errors.len() < MAX_ERRORS {
            self.errors.push(FullError { car, log, message, at_ms: now_ms() });
        }
    }
}

/// How fast a full rebuild's phase goes ([`FullRecord::rates`]).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rates {
    pub bytes_per_s: Option<f64>,
    pub logs_per_s: Option<f64>,
    pub days_per_s: Option<f64>,
    pub eta_ms: Option<u64>,
}

/// Asking for one while another runs.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("a full rebuild is under way already")]
    Running(Box<FullRecord>),
    #[error("no such car")]
    NoSuchCar,
    #[error(transparent)]
    Engine(#[from] crate::Error),
}

/// What runs now, in memory.
#[derive(Default)]
pub(crate) struct Full {
    /// The car under way's log counting, while it decodes.
    job: Option<Arc<JobState>>,
    /// When the record was last written.
    noted_ms: u64,
    /// One worker at a time.
    worker: bool,
}

/// The keys of the engine's record of a car that are worked out from its logs (replaced by a full rebuild); the rest
/// of the car's keys are its raw inputs' and its owner's (kept).
fn derived_engine_key(car: CarId, key: &str) -> bool {
    let p = format!("car/{car}");
    let Some(rest) = key.strip_prefix(&p) else { return false };
    rest.is_empty()
        || ["/dirty", "/rebuild", "/fw/history", "/api_dirty"].contains(&rest)
        || rest.starts_with("/day/")
        || rest.starts_with("/fw/day/")
        || rest.starts_with("/fw/sum/")
}

/// The namespaces that only keep working values (each made again when missing).
const CACHES: [&str; 4] =
    [crate::query::dayparts::NS, crate::query::battery::PACK_NS, crate::query::pack_logs::NS, "charge-fill"];
/// The namespaces with a car's owner's data or its imports' (kept), besides the engine's own.
const KEPT: [&str; 4] = [crate::query::logbook::NS, "fleet", "update-version", crate::query::vehicle::NS];

fn car_prefix(car: CarId) -> String {
    format!("car/{car}/")
}

/// Whether a key of a kept namespace is a working value after all (the vehicle's build days).
fn cache_in_kept(ns: &str, car: CarId, key: &str) -> bool {
    ns == crate::query::vehicle::NS && key.starts_with(&format!("car/{car}/day/"))
}

fn copy_key(from: &Db, to: &Db, ns: &str, key: &str) -> Result<()> {
    if let Some(v) = from.kv().get(ns, key)? {
        to.kv().put(ns, key, &v)?;
    }
    Ok(())
}

/// A car's meta record with only what its API inputs and arrivals gave (the logs' part starts again).
fn inputs_meta(m: &CarMeta) -> CarMeta {
    CarMeta {
        api_days: m.api_days.clone(),
        api_last_ms: m.api_last_ms,
        api_received_ms: m.api_received_ms,
        teslafi_days: m.teslafi_days.clone(),
        teslafi_last_ms: m.teslafi_last_ms,
        teslafi_received_ms: m.teslafi_received_ms,
        tmlocal_days: m.tmlocal_days.clone(),
        tmlocal_last_ms: m.tmlocal_last_ms,
        tmlocal_received_ms: m.tmlocal_received_ms,
        last_arrived_ms: m.last_arrived_ms,
        ..CarMeta::default()
    }
}

fn api_lasts(m: &CarMeta) -> Vec<Option<u64>> {
    model::Input::apis().map(|i| m.api_times(i).0).collect()
}

impl Engine {
    /// A second engine over a shadow db (for a full rebuild): the same stored logs, options and runtime; never
    /// started (its work is run by the caller, one car).
    pub(crate) fn shadow_of(&self, db: Db) -> Result<Engine> {
        let e = Engine::new(db, self.inner.store.clone(), self.inner.opts.clone());
        *lock(&e.inner.handle) = Some(self.handle()?);
        e.set_default_time_zone(&self.inner.default_zone());
        e.set_import_gap_fill(self.import_gap_fill());
        Ok(e)
    }

    fn full_get(&self) -> Result<Option<FullRecord>> {
        records::get(&self.inner.db, NS, KEY)
    }

    fn full_put(&self, r: &FullRecord) -> Result<()> {
        records::put(&self.inner.db, NS, KEY, r)
    }

    /// The full rebuild under way or the last one (with the progress of the car under way), if any.
    pub fn full_rebuild_status(&self) -> Result<Option<FullRecord>> {
        let Some(mut r) = self.full_get()? else { return Ok(None) };
        if r.running()
            && let Some(j) = lock(&self.inner.full).job.clone()
        {
            let (done, total, bd, bt) = j.counts();
            r.logs_done = r.base_logs + done;
            r.logs_total = r.base_logs + total;
            r.bytes_done = r.base_bytes + bd;
            r.bytes_total = r.base_bytes + bt;
            r.last_progress_ms = r.last_progress_ms.max(j.last_step_ms.load(Ordering::SeqCst));
            for (log, why) in lock(&j.log_errors).iter() {
                if !r.errors.iter().any(|e| e.log.as_deref() == Some(log.as_str())) {
                    r.error(r.car(), Some(log.clone()), format!("The log {log} could not be decoded: {why}"));
                }
            }
        }
        Ok(Some(r))
    }

    /// Starts a full rebuild of `car` (`None`: every car) in the background. `by`: the admin who asked.
    pub fn full_rebuild(
        &self,
        car: Option<CarId>,
        reason: Reason,
        by: Option<String>,
    ) -> Result<FullRecord, StartError> {
        let cars = match car {
            Some(c) => {
                if self.inner.db.car(c).map_err(crate::Error::from)?.is_none() {
                    return Err(StartError::NoSuchCar);
                }
                vec![c]
            }
            None => crate::query::all_cars(self).map_err(|e| crate::Error::Other(e.to_string()))?,
        };
        self.full_rebuild_of(cars, car.is_none(), reason, by)
    }

    /// Starts a full rebuild of these cars (`all`: they are every car) in the background.
    pub fn full_rebuild_of(
        &self,
        cars: Vec<CarId>,
        all: bool,
        reason: Reason,
        by: Option<String>,
    ) -> Result<FullRecord, StartError> {
        self.handle()?;
        let _one = lock(&self.inner.full_start);
        if let Some(r) = self.full_get()?
            && r.running()
        {
            return Err(StartError::Running(Box::new(r)));
        }
        let now = now_ms();
        let id = self.full_get()?.map_or(1, |r| r.id + 1);
        let r = FullRecord {
            id,
            all,
            cars,
            index: 0,
            phase: Phase::Queued,
            status: FullStatus::Running,
            reason,
            by,
            version: derived_version(),
            started_ms: now,
            finished_ms: None,
            car_started_ms: now,
            phase_started_ms: now,
            logs_done: 0,
            logs_total: 0,
            bytes_done: 0,
            bytes_total: 0,
            days_done: 0,
            days_total: 0,
            bytes_copied: 0,
            base_logs: 0,
            base_bytes: 0,
            base_days: 0,
            last_progress_ms: now,
            errors: Vec::new(),
            error: None,
            cancel: false,
            audited_end: false,
            seed: Seed::default(),
            fail_at: *lock(&self.inner.full_fail_at),
        };
        self.full_put(&r)?;
        self.full_spawn();
        Ok(r)
    }

    /// Asks the full rebuild under way to stop: the cars not switched yet keep their history as it is (the shadow
    /// goes). Whether one was running.
    pub fn cancel_full_rebuild(&self) -> Result<bool> {
        let _one = lock(&self.inner.full_start);
        let Some(mut r) = self.full_get()? else { return Ok(false) };
        if !r.running() {
            return Ok(false);
        }
        r.cancel = true;
        let (job, worker) = {
            let f = lock(&self.inner.full);
            (f.job.clone(), f.worker)
        };
        if !worker {
            // no worker on it (stopped, or the engine not started): it ends here; the history in use was never
            // touched (a switch under way has a worker until it is done)
            r.status = FullStatus::Cancelled;
            r.finished_ms = Some(now_ms());
            self.inner.db.remove_shadow(SHADOW)?;
        }
        self.full_put(&r)?;
        if let Some(j) = job {
            j.cancel_now();
        }
        Ok(true)
    }

    /// The audit log has a finished full rebuild's end.
    pub fn full_rebuild_audited(&self, id: u64) -> Result<()> {
        let _one = lock(&self.inner.full_start);
        if let Some(mut r) = self.full_get()?
            && r.id == id
            && !r.running()
        {
            r.audited_end = true;
            self.full_put(&r)?;
        }
        Ok(())
    }

    /// Makes the next full rebuild fail at `phase` (tests).
    #[doc(hidden)]
    pub fn full_rebuild_fail_at(&self, phase: Option<Phase>) {
        *lock(&self.inner.full_fail_at) = phase;
    }

    /// Makes the full rebuild's worker stop at `phase` as a stop of the server would (tests: a restart carries on).
    #[doc(hidden)]
    pub fn full_rebuild_crash_at(&self, phase: Option<Phase>) {
        *lock(&self.inner.full_crash_at) = phase;
    }

    /// Carries on with a full rebuild a restart cut off (from [`Engine::start`]).
    pub(crate) fn full_resume(&self) {
        if self.full_get().ok().flatten().is_some_and(|r| r.running()) {
            self.full_spawn();
        }
    }

    fn full_spawn(&self) {
        {
            let mut f = lock(&self.inner.full);
            if f.worker {
                return;
            }
            f.worker = true;
        }
        let Ok(rt) = self.handle() else {
            lock(&self.inner.full).worker = false;
            return;
        };
        let e = self.clone();
        self.inner.outstanding_add();
        rt.spawn_blocking(move || {
            match e.full_run() {
                // (a test's crash: left as a stop of the server would leave it)
                Err(err) if err.to_string().contains(CRASHED) => {}
                Err(err) => e.full_failed(&err.to_string()),
                Ok(()) => {}
            }
            {
                let mut f = lock(&e.inner.full);
                f.worker = false;
                f.job = None;
            }
            e.inner.outstanding_sub();
        });
    }

    fn full_failed(&self, why: &str) {
        tracing::warn!("full rebuild: {why}");
        let _one = lock(&self.inner.full_start);
        if let Ok(Some(mut r)) = self.full_get()
            && r.running()
        {
            let car = r.car();
            r.error(car, None, why.to_string());
            r.error = Some(why.to_string());
            r.status = FullStatus::Failed;
            r.finished_ms = Some(now_ms());
            let _ = self.full_put(&r);
        }
        // the history in use was never touched: only the shadow goes
        let _ = self.inner.db.remove_shadow(SHADOW);
    }

    /// The record moved on (a log done): written now and then, so a restart carries on near where it was.
    pub(crate) fn full_noted(&self) {
        let now = now_ms();
        {
            let mut f = lock(&self.inner.full);
            if now.saturating_sub(f.noted_ms) < NOTE_EVERY_MS {
                return;
            }
            f.noted_ms = now;
        }
        let _one = lock(&self.inner.full_start);
        if let Ok(Some(r)) = self.full_rebuild_status()
            && r.running()
        {
            let _ = self.full_put(&r);
        }
    }

    /// Changes the record (read, changed, written), under the start lock.
    fn full_edit(&self, f: impl FnOnce(&mut FullRecord)) -> Result<FullRecord> {
        let _one = lock(&self.inner.full_start);
        let mut r = self
            .full_rebuild_status()?
            .ok_or_else(|| crate::Error::Other("the full rebuild's record is gone".into()))?;
        f(&mut r);
        self.full_put(&r)?;
        Ok(r)
    }

    fn full_phase(&self, phase: Phase) -> Result<FullRecord> {
        let r = self.full_edit(|r| {
            r.phase_started_ms = now_ms();
            r.last_progress_ms = now_ms();
            r.base_logs = r.logs_done;
            r.base_bytes = r.bytes_done;
            // (a restart in the days works out all the car's days again: counted from where the car's began)
            if r.phase != phase || phase != Phase::Days {
                r.base_days = r.days_done;
            }
            r.phase = phase;
        })?;
        if r.fail_at == Some(phase) {
            return Err(crate::Error::Other(format!("stopped at {phase:?} for a test")));
        }
        {
            let mut c = lock(&self.inner.full_crash_at);
            if *c == Some(phase) {
                *c = None;
                return Err(crate::Error::Other(CRASHED.into()));
            }
        }
        Ok(r)
    }

    fn full_cancelled(&self) -> bool {
        self.full_get().ok().flatten().is_none_or(|r| r.cancel || !r.running())
    }

    fn full_run(&self) -> Result<()> {
        loop {
            let Some(r) = self.full_get()? else { return Ok(()) };
            if !r.running() {
                return Ok(());
            }
            if r.cancel {
                self.inner.db.remove_shadow(SHADOW)?;
                self.full_edit(|r| {
                    r.status = FullStatus::Cancelled;
                    r.finished_ms = Some(now_ms());
                })?;
                return Ok(());
            }
            let Some(car) = r.car() else {
                self.full_edit(|r| {
                    r.status = FullStatus::Done;
                    r.finished_ms = Some(now_ms());
                })?;
                return Ok(());
            };
            if self.inner.db.car(car)?.is_none() {
                // removed meanwhile: nothing to rebuild
                self.inner.db.remove_shadow(SHADOW)?;
                self.full_next()?;
                continue;
            }
            self.full_car(car, &r)?;
            if self.full_cancelled() {
                continue;
            }
            self.full_next()?;
        }
    }

    fn full_next(&self) -> Result<()> {
        // (the counts go on adding up over the cars)
        self.full_edit(|r| {
            r.index += 1;
            r.phase = Phase::Queued;
            r.car_started_ms = now_ms();
            r.phase_started_ms = now_ms();
            r.bytes_copied = 0;
        })?;
        Ok(())
    }

    /// One car, from where its record is (a restart carries on).
    fn full_car(&self, car: CarId, r: &FullRecord) -> Result<()> {
        let db = &self.inner.db;
        let shadow = match (r.phase, db.open_shadow(SHADOW)?) {
            (Phase::Decoding | Phase::Days | Phase::Switching, Some(s)) if s.car(car)?.is_some() => s,
            _ => {
                self.full_phase(Phase::Copying)?;
                self.full_seed(car)?
            }
        };
        let se = self.shadow_of(shadow)?;
        let r = self.full_get()?.unwrap_or_else(|| r.clone());
        if r.phase == Phase::Switching {
            return self.full_switch(car, &se);
        }
        if r.phase != Phase::Days {
            self.full_phase(Phase::Decoding)?;
            // the days the days step will go over, as the car's history has them now (counted again exactly when the
            // step starts): the page shows them from the start
            let days = records::meta(db, car)?.days.len() as u64;
            self.full_edit(|r| {
                r.days_done = r.base_days;
                r.days_total = r.base_days + days;
            })?;
            let job = Arc::new(JobState::for_full(car));
            lock(&self.inner.full).job = Some(job.clone());
            let res = se.reconcile_car(car, Some(&job));
            // the counts kept in the record before the job goes
            self.full_edit(|_| {})?;
            lock(&self.inner.full).job = None;
            res?;
            if self.full_cancelled() {
                return Ok(());
            }
        }
        self.full_phase(Phase::Days)?;
        self.full_days(car, &se)?;
        if self.full_cancelled() {
            return Ok(());
        }
        self.full_phase(Phase::Switching)?;
        self.full_switch(car, &se)
    }

    /// Starts the car's shadow: its folder copied (without SMO's series), its kept keys copied in, then what was
    /// worked out from CAN logs cleared there.
    fn full_seed(&self, car: CarId) -> Result<Db> {
        let db = &self.inner.db;
        let (shadow, bytes) = db.shadow(SHADOW, car)?;
        let real_meta = records::meta(db, car)?;
        // the engine's keys of the car that are not worked out from its logs, and the places' extras
        for key in db.kv().keys(NS)? {
            let mine = key.starts_with(&car_prefix(car)) && !derived_engine_key(car, &key);
            if mine || key.starts_with("place/") {
                copy_key(db, &shadow, NS, &key)?;
            }
        }
        records::put_meta(&shadow, car, &inputs_meta(&real_meta))?;
        for ns in KEPT {
            for key in db.kv().keys(ns)? {
                if key.starts_with(&car_prefix(car)) && !cache_in_kept(ns, car, &key) {
                    copy_key(db, &shadow, ns, &key)?;
                }
            }
        }
        let se = self.shadow_of(shadow.clone())?;
        {
            let l = se.car_lock(car);
            let _held = lock(&l);
            se.clear_window(car, 0, u64::MAX)?;
        }
        self.full_edit(|r| {
            r.bytes_copied = bytes;
            r.seed = Seed { api_last: api_lasts(&real_meta) };
        })?;
        Ok(shadow)
    }

    /// The pack's readings of every record day, worked out in the shadow (kept with it, switched in with it).
    fn full_days(&self, car: CarId, se: &Engine) -> Result<()> {
        let Some(c) = se.inner.db.car(car)? else { return Ok(()) };
        let days: Vec<u32> = records::meta(&se.inner.db, car)?.days.into_iter().rev().collect();
        // (a restart in this phase counts the car's days again)
        let base = self.full_edit(|r| {
            r.days_done = r.base_days;
            r.days_total = r.base_days + days.len() as u64;
        })?;
        let base = base.base_days;
        let mut last = now_ms();
        for (i, d) in days.iter().enumerate() {
            if let Err(err) = crate::query::battery::day_pack(se, &c, *d) {
                let day = chrono::DateTime::from_timestamp_millis(i64::from(*d) * 86_400_000)
                    .map_or_else(|| d.to_string(), |t| t.format("%Y-%m-%d").to_string());
                self.full_edit(|r| r.error(Some(car), None, format!("The pack's readings of {day} (UTC): {err}")))?;
            }
            let now = now_ms();
            if now.saturating_sub(last) >= NOTE_EVERY_MS || i + 1 == days.len() {
                last = now;
                if self.full_cancelled() {
                    return Ok(());
                }
                self.full_edit(|r| {
                    r.days_done = base + i as u64 + 1;
                    r.last_progress_ms = now;
                })?;
            }
        }
        Ok(())
    }

    /// Switches the car to its shadow's history, under the car's lock.
    fn full_switch(&self, car: CarId, se: &Engine) -> Result<()> {
        let db = &self.inner.db;
        let sdb = &se.inner.db;
        let seed = self.full_get()?.map(|r| r.seed).unwrap_or_default();
        let redo = {
            let l = self.car_lock(car);
            let _held = lock(&l);
            if sdb.car_folder_exists(car) {
                // what came meanwhile and is not made again: imported rows (an import ran) and the updates the
                // Tesla API named
                for src in [Source::Teslamate, Source::Teslafi] {
                    carry::<model::Drive>(db, sdb, car, src)?;
                    carry::<model::Charge>(db, sdb, car, src)?;
                    carry::<db::StateRow>(db, sdb, car, src)?;
                    carry::<db::UpdateRow>(db, sdb, car, src)?;
                }
                let api_updates: Vec<db::UpdateRow> =
                    crate::pipeline::all_rows::<db::UpdateRow>(db, car, 0, u64::MAX, Some(Source::Live))?
                        .into_iter()
                        .filter(|u| u.update.inputs.contains(model::Input::TeslaApi))
                        .collect();
                sdb.upsert(car, &api_updates)?;
                db.adopt_car(car, sdb)?;
            }
            self.full_switch_records(car, sdb, &seed)?;
            // a change of the car's order or keys meanwhile was built into the old history only
            let (a, b) = (db.car(car)?, sdb.car(car)?);
            a.as_ref().map(|c| (&c.preferred_key, &c.vehicle_keys))
                != b.as_ref().map(|c| (&c.preferred_key, &c.vehicle_keys))
                || self.source_order(car) != se.source_order(car)
        };
        lock(&self.inner.version_stale).remove(&car);
        db.remove_shadow(SHADOW)?;
        self.full_edit(|r| r.last_progress_ms = now_ms())?;
        self.emit(Event::Rows {
            car,
            kinds: vec!["drives", "charges", "updates", "visits", "states"],
            from_ms: 0,
            to_ms: u64::MAX,
        });
        if let Ok(now) = crate::query::now::now_view(self, car) {
            self.emit(Event::Now { car, now: Box::new(now) });
        }
        // the logs that came during the rebuild (not recorded in the new history), the API data that came, the
        // days' readings and long periods worked out again ahead
        if redo {
            self.rebuild(crate::Scope::car(car))?;
        } else {
            self.send(Work::Reconcile { car: Some(car) });
        }
        self.send(Work::ApiBuild { car });
        crate::query::pack_logs::start(self, car);
        self.warm(Some(car));
        Ok(())
    }

    /// The car's records switched to the shadow's (its lock held, its folder switched): the keys worked out from
    /// its logs replaced, its kept working values dropped, the rest left as they are.
    fn full_switch_records(&self, car: CarId, sdb: &Db, seed: &Seed) -> Result<()> {
        let db = &self.inner.db;
        let real_meta = records::meta(db, car)?;
        let mut dirty = records::api_dirty(db, car)?;
        dirty.extend(records::api_dirty(sdb, car)?);
        let lasts = api_lasts(&real_meta);
        if lasts != seed.api_last {
            // API data came during the rebuild: built in again from the oldest newest-data then
            let from = seed.api_last.iter().flatten().min().copied().unwrap_or(0);
            dirty.push((from.saturating_sub(records::DAY_MS), u64::MAX));
        }
        for key in db.kv().keys(NS)? {
            if derived_engine_key(car, &key) {
                db.kv().delete(NS, &key)?;
            }
        }
        for ns in CACHES.iter().chain(&[crate::query::vehicle::NS]) {
            for key in db.kv().keys(ns)? {
                if key.starts_with(&car_prefix(car)) && (CACHES.contains(ns) || cache_in_kept(ns, car, &key)) {
                    db.kv().delete(ns, &key)?;
                }
            }
        }
        for key in sdb.kv().keys(NS)? {
            if derived_engine_key(car, &key) && key != format!("car/{car}") && key != format!("car/{car}/api_dirty") {
                copy_key(sdb, db, NS, &key)?;
            }
        }
        for ns in [crate::query::battery::PACK_NS, crate::query::pack_logs::NS] {
            for key in sdb.kv().keys(ns)? {
                if key.starts_with(&car_prefix(car)) {
                    copy_key(sdb, db, ns, &key)?;
                }
            }
        }
        let mut meta = records::meta(sdb, car)?;
        let inputs = inputs_meta(&real_meta);
        meta.api_days = inputs.api_days;
        meta.api_last_ms = inputs.api_last_ms;
        meta.api_received_ms = inputs.api_received_ms;
        meta.teslafi_days = inputs.teslafi_days;
        meta.teslafi_last_ms = inputs.teslafi_last_ms;
        meta.teslafi_received_ms = inputs.teslafi_received_ms;
        meta.tmlocal_days = inputs.tmlocal_days;
        meta.tmlocal_last_ms = inputs.tmlocal_last_ms;
        meta.tmlocal_received_ms = inputs.tmlocal_received_ms;
        meta.last_arrived_ms = meta.last_arrived_ms.max(real_meta.last_arrived_ms);
        meta.version = Some(derived_version());
        records::put_meta(db, car, &meta)?;
        records::put_api_dirty(db, car, &crate::pipeline::join_spans(dirty))?;
        records::put_rebuild_mark(db, car, None)?;
        Ok(())
    }

    /// Compares every car's history's version with this server's (at the start): one made with another, or before
    /// versions were kept, is held (see [`Engine::version_stale`]).
    pub(crate) fn check_versions(&self) {
        let now = derived_version();
        let Ok(cars) = crate::query::all_cars(self) else { return };
        let mut stale = lock(&self.inner.version_stale);
        for car in cars {
            match records::meta(&self.inner.db, car) {
                Ok(m) if m.version.as_ref().is_some_and(|v| *v != now) => {
                    stale.insert(car);
                }
                // made before versions were kept: with decoded logs, by an older decoding (the same as this one or
                // not, it can't be told), so it is made again
                Ok(m) if m.version.is_none() && m.decoded > 0 => {
                    stale.insert(car);
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("car {car}: its history's version could not be read: {e}"),
            }
        }
    }

    /// Whether a new server version's full rebuild runs by itself (the server setting `full_rebuild.auto`; the web
    /// side starts it).
    pub fn full_rebuild_auto(&self) -> bool {
        self.inner.full_auto.load(Ordering::SeqCst)
    }

    pub fn set_full_rebuild_auto(&self, on: bool) {
        self.inner.full_auto.store(on, Ordering::SeqCst);
    }

    /// The cars whose history was made with another decoding than this server's: a full rebuild makes them again
    /// (until then their logs are not decoded again in place for that alone).
    pub fn version_stale(&self) -> Vec<CarId> {
        let mut v: Vec<CarId> = lock(&self.inner.version_stale).iter().copied().collect();
        v.sort_unstable();
        v
    }

    pub(crate) fn version_held(&self, car: CarId) -> bool {
        lock(&self.inner.version_stale).contains(&car)
    }

    /// Marks a car's history as made with another decoding (tests: as a newer server would find it).
    #[doc(hidden)]
    pub fn set_car_version(&self, car: CarId, version: &str) -> Result<()> {
        {
            let l = self.car_lock(car);
            let _held = lock(&l);
            let mut m = records::meta(&self.inner.db, car)?;
            m.version = Some(version.to_string());
            records::put_meta(&self.inner.db, car, &m)?;
        }
        if version == derived_version() {
            lock(&self.inner.version_stale).remove(&car);
        } else {
            lock(&self.inner.version_stale).insert(car);
        }
        Ok(())
    }
}

/// Puts in the shadow the rows `real` has from an import site (an import that ran during the rebuild).
fn carry<T: db::Item>(real: &Db, shadow: &Db, car: CarId, src: Source) -> Result<()> {
    let rows = crate::pipeline::all_rows::<T>(real, car, 0, u64::MAX, Some(src))?;
    let had = crate::pipeline::all_rows::<T>(shadow, car, 0, u64::MAX, Some(src))?;
    if rows.len() == had.len() && rows.iter().zip(&had).all(|(a, b)| a.id() == b.id()) {
        shadow.upsert(car, &rows)?;
        return Ok(());
    }
    let gone: Vec<u64> = had.iter().map(db::Item::id).filter(|id| !rows.iter().any(|r| r.id() == *id)).collect();
    shadow.delete::<T>(car, &gone)?;
    shadow.upsert(car, &rows)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_logs_part_of_a_cars_record_is_told_from_the_rest() {
        let c: CarId = 7;
        for k in [
            "car/7",
            "car/7/day/20000",
            "car/7/dirty",
            "car/7/rebuild",
            "car/7/fw/day/3",
            "car/7/fw/sum/3",
            "car/7/fw/history",
            "car/7/api_dirty",
        ] {
            assert!(derived_engine_key(c, k), "{k}");
        }
        for k in [
            "car/7/costs/2026-01",
            "car/7/fw/names",
            "car/7/api/20000",
            "car/7/api_meta",
            "car/7/now_at",
            "car/7/teslafi_off_can",
            "car/7/input_versions",
            "car/7/overlaps",
            "car/70/day/1",
            "car/70",
            "keys/waiting",
            "car/7/cost-before-typed/9",
        ] {
            assert!(!derived_engine_key(c, k), "{k}");
        }
    }

    #[test]
    fn the_version_names_the_decoding_and_is_steady() {
        let v = derived_version();
        assert!(v.starts_with(&format!("decode-{}.rules-{}.dbc-", records::DECODE_VERSION, timeline::RULES_VERSION)));
        assert_eq!(v, derived_version());
    }
}
