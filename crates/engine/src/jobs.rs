//! Rebuilding a car (or every car) from its raw logs: everything worked out from CAN logs in the period is taken out,
//! then each log is worked on again in time order, as if it had just arrived. Imported TeslaMate rows are never taken
//! out. A job runs in the background, counts its progress, and can be cancelled; a restart carries on with it (the car
//! keeps a mark until its logs are all done again, and [`Engine::reconcile`] picks the rest up).
//!
//! **One rebuild per car at a time, asks joined.** A job not started yet takes in every later ask for its car (or,
//! one of every car, for any car): its period widened, its work and causes added. A rebuild a saved setting asks for
//! waits [`crate::Options::rebuild_settle_ms`] first (each later ask moves that on), so a few saves in a row are one
//! rebuild; one asked for while the car's job runs waits for it to end, and the asks meanwhile are one job after it.
//!
//! **Only what changed.** A job does the least that gives the same history ([`What`]): a car's GPS layout changed,
//! only its logs whose positions came from the Screenmate's own GPS frames are decoded again (the rest read the same
//! whatever the layout); a car's source order changed, its windows are built again from what is stored (the order
//! only says which input wins when they are joined), nothing decoded again. Each is noted on the car before its job
//! starts and cleared once done, so a restart in between does it then ([`records::Redo`]).

use crate::history::Run;
use crate::records::{self, Outcome, RebuildMark, Redo};
use crate::versions::Cause;
use crate::{Engine, Event, Result, lock, now_ms};
use model::CarId;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// What to rebuild: a car or every car, over a period (both ends open by default: the whole history).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Scope {
    pub car: Option<CarId>,
    pub from_ms: Option<u64>,
    pub to_ms: Option<u64>,
}

impl Scope {
    pub fn car(car: CarId) -> Scope {
        Scope { car: Some(car), ..Scope::default() }
    }
    pub fn all() -> Scope {
        Scope::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Running,
    Done,
    Cancelled,
    Failed,
}

/// A rebuild job as it is now.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Job {
    pub id: u64,
    pub car: Option<CarId>,
    pub status: JobStatus,
    /// Logs worked on, and to work on (grows as each car's logs are counted).
    pub done: u64,
    pub total: u64,
    /// Of the logs worked on, those that could not be (their stored file could not be read, mostly).
    pub failed: u64,
    pub from_ms: u64,
    pub to_ms: u64,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    pub took_ms: Option<u64>,
    pub error: Option<String>,
    /// What it does: `car` (the history made again from the logs), else `positions` and/or `inputs` ([`What`]).
    pub kind: String,
    /// Why it runs (every ask it took in).
    pub causes: Vec<Cause>,
    /// Not started yet: it waits for asks to settle, or for the car's job under way to end.
    pub waiting: bool,
}

/// What a job does for its cars.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct What {
    /// Everything worked out from the logs in the period, made again (covers the rest).
    pub whole: bool,
    /// Only the logs whose positions came from the Screenmate's own GPS frames, decoded again.
    pub positions: bool,
    /// The windows built again from what is stored (the inputs joined again), nothing decoded again.
    pub inputs: bool,
}

impl What {
    pub const WHOLE: What = What { whole: true, positions: false, inputs: false };
    pub const POSITIONS: What = What { whole: false, positions: true, inputs: false };

    fn positions_or_whole(self) -> bool {
        self.whole || self.positions
    }
    pub const INPUTS: What = What { whole: false, positions: false, inputs: true };

    fn join(self, o: What) -> What {
        What { whole: self.whole || o.whole, positions: self.positions || o.positions, inputs: self.inputs || o.inputs }
    }

    pub fn word(self) -> String {
        if self.whole {
            return "car".into();
        }
        let mut v = Vec::new();
        if self.positions {
            v.push("positions");
        }
        if self.inputs {
            v.push("inputs");
        }
        v.join("+")
    }
}

pub(crate) struct JobState {
    id: u64,
    car: Option<CarId>,
    from_ms: AtomicU64,
    to_ms: AtomicU64,
    started_ms: AtomicU64,
    what: Mutex<What>,
    causes: Mutex<Vec<Cause>>,
    /// Started (no longer takes in later asks).
    started: AtomicBool,
    /// It does not start before this (ms).
    settle_until: AtomicU64,
    done: AtomicU64,
    total: AtomicU64,
    cancel: AtomicBool,
    status: Mutex<JobStatus>,
    finished_ms: Mutex<Option<u64>>,
    error: Mutex<Option<String>>,
    /// Bytes of the logs worked on, and to work on (their stored length).
    pub(crate) bytes_done: AtomicU64,
    pub(crate) bytes_total: AtomicU64,
    /// When the last log was done (ms).
    pub(crate) last_step_ms: AtomicU64,
    /// The logs that could not be worked on, and what went wrong (the first few).
    pub(crate) log_errors: Mutex<Vec<(String, String)>>,
    /// Every log that could not be worked on (their names): they count as worked on, never as made again.
    pub(crate) failed_logs: Mutex<Vec<String>>,
    /// A full rebuild's: its progress is noted as it goes ([`crate::full`]).
    pub(crate) full: bool,
    /// A full rebuild's: the engine whose rebuild it is. The decoding itself runs on a second engine over the
    /// shadow, which has no record of the rebuild: the progress is noted on this one.
    owner: Option<Engine>,
    /// Tests: stop as by a crash once this many logs are done (0: never).
    crash_after: AtomicU64,
}

/// A job's log errors kept at most (as many as a full rebuild's record keeps).
pub(crate) const MAX_LOG_ERRORS: usize = 50;

impl JobState {
    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    pub(crate) fn add_total(&self, n: u64, bytes: u64) {
        self.total.fetch_add(n, Ordering::SeqCst);
        self.bytes_total.fetch_add(bytes, Ordering::SeqCst);
    }

    pub(crate) fn step(&self, e: &Engine, car: Option<CarId>, bytes: u64) {
        self.bytes_done.fetch_add(bytes, Ordering::SeqCst);
        self.last_step_ms.store(now_ms(), Ordering::SeqCst);
        let done = self.done.fetch_add(1, Ordering::SeqCst) + 1;
        let total = self.total.load(Ordering::SeqCst);
        if self.full {
            if let Some(o) = &self.owner {
                o.full_noted();
            }
        } else if done.is_multiple_of(50) || done == total {
            e.emit(Event::Job { job: self.id, car, status: JobStatus::Running, done, total });
        }
    }

    /// Tests: whether the worker stops here as by a crash (once).
    /// Whether the server of a full rebuild's job is stopping ([`Engine::stopping`]): its worker goes no further.
    pub(crate) fn stopping(&self) -> bool {
        self.owner.as_ref().is_some_and(|o| o.full_not_stopping().is_err())
    }

    pub(crate) fn crash_now(&self) -> bool {
        let n = self.crash_after.load(Ordering::SeqCst);
        n > 0 && self.done.load(Ordering::SeqCst) >= n && self.crash_after.swap(0, Ordering::SeqCst) > 0
    }

    /// A log that could not be worked on: counted, and what went wrong kept for the job's status (the first
    /// [`MAX_LOG_ERRORS`]). A log the store can't give is said so (the store's own words are for an uploader).
    pub(crate) fn log_failed(&self, name: &str, why: &crate::Error) {
        let said = match why {
            crate::Error::Store(_) => "could not be read: its stored file (or the day's pack it is in) is damaged or \
                                       cut short; the server's log names the file"
                .to_string(),
            e => format!("could not be decoded: {e}"),
        };
        lock(&self.failed_logs).push(name.to_string());
        let mut v = lock(&self.log_errors);
        if v.len() < MAX_LOG_ERRORS {
            v.push((name.to_string(), said));
        }
    }

    /// The logs that could not be worked on.
    pub(crate) fn failed(&self) -> Vec<String> {
        lock(&self.failed_logs).clone()
    }

    /// A full rebuild's: the logs of the car that need no work (decoded by this rebuild before a restart), told
    /// to the rebuild's own engine once they are counted.
    pub(crate) fn counted(&self, skipped: u64, skipped_bytes: u64) {
        if let Some(o) = &self.owner {
            o.full_counted(skipped, skipped_bytes);
        }
    }

    pub(crate) fn cancel_now(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub(crate) fn counts(&self) -> (u64, u64, u64, u64) {
        (
            self.done.load(Ordering::SeqCst),
            self.total.load(Ordering::SeqCst),
            self.bytes_done.load(Ordering::SeqCst),
            self.bytes_total.load(Ordering::SeqCst),
        )
    }

    /// A job state of a full rebuild's car (never listed with [`Engine::jobs`]).
    pub(crate) fn for_full(car: CarId, owner: &Engine, crash_after: Option<u64>) -> JobState {
        let mut s = JobState::new(0, Some(car), 0, u64::MAX);
        s.full = true;
        s.owner = Some(owner.clone());
        s.crash_after.store(crash_after.unwrap_or(0), Ordering::SeqCst);
        s.started.store(true, Ordering::SeqCst);
        s
    }

    fn new(id: u64, car: Option<CarId>, from_ms: u64, to_ms: u64) -> JobState {
        JobState {
            id,
            car,
            from_ms: AtomicU64::new(from_ms),
            to_ms: AtomicU64::new(to_ms),
            started_ms: AtomicU64::new(now_ms()),
            what: Mutex::new(What::WHOLE),
            causes: Mutex::new(Vec::new()),
            started: AtomicBool::new(false),
            settle_until: AtomicU64::new(0),
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
            status: Mutex::new(JobStatus::Running),
            finished_ms: Mutex::new(None),
            error: Mutex::new(None),
            bytes_done: AtomicU64::new(0),
            bytes_total: AtomicU64::new(0),
            last_step_ms: AtomicU64::new(now_ms()),
            log_errors: Mutex::new(Vec::new()),
            failed_logs: Mutex::new(Vec::new()),
            full: false,
            owner: None,
            crash_after: AtomicU64::new(0),
        }
    }

    fn period(&self) -> (u64, u64) {
        (self.from_ms.load(Ordering::SeqCst), self.to_ms.load(Ordering::SeqCst))
    }

    fn view(&self) -> Job {
        let finished_ms = *lock(&self.finished_ms);
        let (from_ms, to_ms) = self.period();
        let started_ms = self.started_ms.load(Ordering::SeqCst);
        Job {
            id: self.id,
            car: self.car,
            status: *lock(&self.status),
            done: self.done.load(Ordering::SeqCst),
            total: self.total.load(Ordering::SeqCst),
            failed: lock(&self.failed_logs).len() as u64,
            from_ms,
            to_ms,
            started_ms,
            finished_ms,
            took_ms: finished_ms.map(|f| f.saturating_sub(started_ms)),
            error: lock(&self.error).clone(),
            kind: lock(&self.what).word(),
            causes: lock(&self.causes).clone(),
            waiting: !self.started.load(Ordering::SeqCst),
        }
    }

    /// A later ask taken in (not started yet): its period, work and cause added; `settle`: the start moved on.
    fn take_in(&self, from_ms: u64, to_ms: u64, what: What, cause: Cause, settle_until: u64) {
        self.from_ms.fetch_min(from_ms, Ordering::SeqCst);
        self.to_ms.fetch_max(to_ms, Ordering::SeqCst);
        {
            let mut w = lock(&self.what);
            *w = w.join(what);
        }
        Cause::add(&mut lock(&self.causes), cause);
        self.settle_until.fetch_max(settle_until, Ordering::SeqCst);
    }
}

/// Counts the rebuild jobs running at once (tests: the most there were at once).
struct RunningGuard<'a>(&'a crate::Inner);

impl<'a> RunningGuard<'a> {
    fn new(inner: &'a crate::Inner) -> Self {
        let now = inner.rebuilds_running.fetch_add(1, Ordering::SeqCst) + 1;
        inner.rebuilds_overlap.fetch_max(now, Ordering::SeqCst);
        RunningGuard(inner)
    }
}

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        self.0.rebuilds_running.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Engine {
    /// Tests: makes the rebuild jobs that wait to start meet at `barrier` once, each, when they are free to start.
    #[doc(hidden)]
    pub fn job_start_hook_set(&self, barrier: Option<Arc<std::sync::Barrier>>) {
        *lock(&self.inner.job_start_hook) = barrier;
    }

    /// Tests: the most rebuild jobs that ran at once since the engine started.
    #[doc(hidden)]
    pub fn rebuilds_overlap_max(&self) -> u64 {
        self.inner.rebuilds_overlap.load(Ordering::SeqCst)
    }

    /// Makes the next rebuild job stop as by a crash once it worked on `logs` logs (tests: the mark it kept is
    /// what a restart carries on from).
    #[doc(hidden)]
    pub fn rebuild_crash_after_logs(&self, logs: Option<u64>) {
        *lock(&self.inner.job_crash_logs) = logs;
    }

    /// Starts a rebuild in the background and returns at once (an admin's: the history of the period made again).
    pub fn rebuild(&self, scope: Scope) -> Result<Job> {
        self.rebuild_because(scope, What::WHOLE, Cause::owner(None), false)
    }

    /// Asks for a rebuild of `what` over `scope`, for `cause`: a job of the car (or of every car) that has not
    /// started yet takes it in; else a new one starts once the car's job under way ended (and, with `settle`, once
    /// [`crate::Options::rebuild_settle_ms`] passed with no later ask). Returns the job that will do it.
    pub fn rebuild_because(&self, scope: Scope, what: What, cause: Cause, settle: bool) -> Result<Job> {
        let handle = self.handle()?;
        let (from_ms, to_ms) =
            (scope.from_ms.unwrap_or(0), scope.to_ms.unwrap_or(u64::MAX).max(scope.from_ms.unwrap_or(0)));
        let settle_until = if settle { now_ms().saturating_add(self.inner.opts.rebuild_settle_ms) } else { 0 };
        let st = {
            let mut pending = lock(&self.inner.pending_jobs);
            let waiting = pending.get(&scope.car).or_else(|| scope.car.and(pending.get(&None))).cloned();
            if let Some(st) = waiting {
                st.take_in(from_ms, to_ms, what, cause, settle_until);
                return Ok(st.view());
            }
            let id = self.inner.next_job.fetch_add(1, Ordering::SeqCst);
            let st = Arc::new(JobState::new(id, scope.car, from_ms, to_ms));
            st.crash_after.store(lock(&self.inner.job_crash_logs).take().unwrap_or(0), Ordering::SeqCst);
            *lock(&st.what) = what;
            lock(&st.causes).push(cause);
            st.settle_until.store(settle_until, Ordering::SeqCst);
            pending.insert(scope.car, st.clone());
            lock(&self.inner.jobs).insert(id, st.clone());
            st
        };
        let e = self.clone();
        self.inner.outstanding_add();
        let job = st.clone();
        Engine::background(&handle, "engine-rebuild", move || {
            let r = if e.job_wait(&st) { e.run_rebuild(&st) } else { Ok(()) };
            let status = match &r {
                Err(err) => {
                    *lock(&st.error) = Some(err.to_string());
                    e.failed(err);
                    JobStatus::Failed
                }
                Ok(()) if st.cancelled() => JobStatus::Cancelled,
                Ok(()) => JobStatus::Done,
            };
            let finished = now_ms();
            // (the rebuilds' history first: a job seen as ended is in it)
            let v = st.view();
            e.note_rebuild(Run {
                kind: v.kind.clone(),
                cars: v.car.into_iter().collect(),
                all: v.car.is_none(),
                causes: v.causes.clone(),
                state: match status {
                    JobStatus::Failed => "failed",
                    JobStatus::Cancelled => "cancelled",
                    _ => "done",
                }
                .into(),
                started_ms: v.started_ms,
                finished_ms: finished,
                logs: v.done,
            });
            *lock(&st.status) = status;
            *lock(&st.finished_ms) = Some(finished);
            e.emit(Event::Job { job: v.id, car: v.car, status, done: v.done, total: v.total });
            e.inner.outstanding_sub();
        });
        Ok(job.view())
    }

    /// Waits until the job may start (its settle time passed, no job of its cars under way), then takes it out of
    /// the jobs that take in later asks. `false`: cancelled before it started.
    fn job_wait(&self, st: &Arc<JobState>) -> bool {
        let mut hooked = false;
        loop {
            if st.cancelled() {
                let mut pending = lock(&self.inner.pending_jobs);
                if pending.get(&st.car).is_some_and(|p| Arc::ptr_eq(p, st)) {
                    pending.remove(&st.car);
                }
                return false;
            }
            let now = now_ms();
            let until = st.settle_until.load(Ordering::SeqCst);
            if now < until {
                std::thread::sleep(std::time::Duration::from_millis((until - now).min(100)));
                continue;
            }
            // tests: both jobs of a pair are here, free to start, before either takes the lock
            if !std::mem::replace(&mut hooked, true) {
                let barrier = lock(&self.inner.job_start_hook).clone();
                if let Some(b) = barrier {
                    b.wait();
                }
            }
            // (the check and the start are one step under the lock the waiting jobs are kept in: two jobs of the
            // same car never both find it free. Lock order: `pending_jobs`, then `jobs`)
            let mut pending = lock(&self.inner.pending_jobs);
            if self.job_busy(st) {
                drop(pending);
                std::thread::sleep(std::time::Duration::from_millis(20));
                continue;
            }
            // (an ask taken in just now moved the start on)
            if st.settle_until.load(Ordering::SeqCst) > now_ms() {
                continue;
            }
            if pending.get(&st.car).is_some_and(|p| Arc::ptr_eq(p, st)) {
                pending.remove(&st.car);
            }
            st.started_ms.store(now_ms(), Ordering::SeqCst);
            st.started.store(true, Ordering::SeqCst);
            return true;
        }
    }

    /// Tests: the generation of the car's positions' rebuild noted (`None`: none).
    #[doc(hidden)]
    pub fn positions_redo_for_tests(&self, car: CarId) -> Option<u64> {
        records::redo(&self.inner.db, car, Redo::Positions).ok().flatten()
    }

    /// Tests: what a positions' job that began with generation `generation` does when it ends.
    #[doc(hidden)]
    pub fn positions_redo_done_for_tests(&self, car: CarId, generation: u64) -> Result<()> {
        self.clear_redo_if(car, Redo::Positions, generation)
    }

    /// Whether a job that redoes the car's positions (a GPS layout's, or the whole history's) is pending or runs.
    pub(crate) fn positions_job_live(&self, car: CarId) -> bool {
        lock(&self.inner.jobs).values().any(|j| {
            (j.car.is_none() || j.car == Some(car))
                && *lock(&j.status) == JobStatus::Running
                && lock(&j.what).positions_or_whole()
        })
    }

    /// Whether another job of the same car (or of every car) has started and runs.
    fn job_busy(&self, st: &JobState) -> bool {
        lock(&self.inner.jobs).values().any(|j| {
            j.id != st.id
                && j.started.load(Ordering::SeqCst)
                && *lock(&j.status) == JobStatus::Running
                && (j.car.is_none() || st.car.is_none() || j.car == st.car)
        })
    }

    fn run_rebuild(&self, st: &JobState) -> Result<()> {
        let _running = RunningGuard::new(&self.inner);
        let db = &self.inner.db;
        let cars: Vec<CarId> = match st.car {
            Some(c) => vec![c],
            None => crate::query::all_cars(self).map_err(|e| crate::Error::Other(e.to_string()))?,
        };
        let what = *lock(&st.what);
        let (from_ms, to_ms) = st.period();
        let started_ms = st.started_ms.load(Ordering::SeqCst);
        for car in cars {
            if st.cancelled() {
                break;
            }
            // (the live stream gets the car's newest values at most once a second until this car is done)
            let _rebuilding = self.rebuilding(car);
            if !what.whole {
                if what.positions {
                    self.redo_positions(car, Some(st))?;
                }
                if what.inputs && !st.cancelled() {
                    self.redo_inputs(car, Some(st))?;
                }
                continue;
            }
            // (the narrow rebuilds asked for up to now are done by this one; one asked for meanwhile is not)
            let (gen_p, gen_i) = (records::redo(db, car, Redo::Positions)?, records::redo(db, car, Redo::Inputs)?);
            let mut window = (from_ms, to_ms);
            {
                let l = self.car_lock(car);
                let _held = lock(&l);
                // (before the clearing: it takes the row away that tells)
                let cut_from_ms = if to_ms == u64::MAX { None } else { self.cut_from(car, to_ms)? };
                let end_clear = to_ms == u64::MAX || cut_from_ms.is_none();
                let mut mark = RebuildMark { from_ms, to_ms, started_ms, cleared: false, end_clear, cut_from_ms };
                records::put_rebuild_mark(db, car, Some(&mark))?;
                self.clear_window(car, from_ms, to_ms)?;
                window = records::rebuild_mark(db, car)?.map_or(window, |m| (m.from_ms, m.to_ms));
                // (a window grown over a damaged file: its end is another)
                if records::rebuild_mark(db, car)?.is_some_and(|m| (m.from_ms, m.to_ms) != (from_ms, to_ms)) {
                    mark.end_clear = false;
                    mark.cut_from_ms = None;
                }
                mark.cleared = true;
                records::put_rebuild_mark(db, car, Some(&mark))?;
            }
            // (what was cleared is cleared by now: a repair that came before this is covered by it)
            let cleared_ms = now_ms();
            self.reconcile_car(car, Some(st))?;
            if st.cancelled() {
                // the car's window is cleared: the logs this job did not reach are worked on now, in the background
                // (only those: what it redid is kept)
                self.send(crate::Work::Reconcile { car: Some(car) });
                break;
            }
            // (a drives table repaired meanwhile: the owner's purposes go back on the drives made again)
            self.repair_after_rebuild(car, cleared_ms, window.0, window.1)?;
            // (the whole history made again: the narrow rebuilds noted are done with it)
            if from_ms == 0 && to_ms == u64::MAX {
                self.job_clear_hook();
                for (what, generation) in [(Redo::Positions, gen_p), (Redo::Inputs, gen_i)] {
                    if let Some(generation) = generation {
                        self.clear_redo_if(car, what, generation)?;
                    }
                }
            }
        }
        // logs that could not be worked on: what they held was cleared and is not made again, so the job did not
        // do what it was asked for, and says so
        let failed = st.failed();
        if let Some(first) = failed.first() {
            let why = lock(&st.log_errors).first().map(|(_, w)| w.clone()).unwrap_or_default();
            let total = st.total.load(Ordering::SeqCst);
            let more = match failed.len() - 1 {
                0 => String::new(),
                n => format!(", and {n} more"),
            };
            let so = if what.whole { ", so the history was made again without what they held" } else { "" };
            return Err(crate::Error::Other(format!(
                "{} of {total} stored logs could not be worked on{so}: {first} {why}{more}",
                failed.len()
            )));
        }
        Ok(())
    }

    /// Where the earliest drive or charge worked out from logs that goes on across `t` (begun before it, ended
    /// after it) began; `None` where none does.
    fn cut_from(&self, car: CarId, t: u64) -> Result<Option<u64>> {
        fn cut<T: db::Item>(db: &db::Db, car: CarId, t: u64) -> Result<Option<u64>> {
            let q = db::Query {
                limit: 10,
                source: Some(model::Source::Live),
                ..db::Query::between(t, t.saturating_add(1))
            };
            Ok(db.list::<T>(car, &q)?.items.iter().map(|r| r.start_ms()).filter(|&s| s < t).min())
        }
        let db = &self.inner.db;
        Ok(match (cut::<model::Drive>(db, car, t)?, cut::<model::Charge>(db, car, t)?) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        })
    }

    /// The narrow rebuild of a GPS layout's change, when one is noted on the car: its logs whose positions came from
    /// the Screenmate's own GPS frames decoded again (each rebuilding the window it changes, as a log that arrives
    /// does); the rest read the same whatever the layout and are left. A log decoded before that was noted is read
    /// once (nothing written) to tell.
    pub(crate) fn redo_positions(&self, car: CarId, job: Option<&JobState>) -> Result<()> {
        let db = &self.inner.db;
        let Some(generation) = records::redo(db, car, Redo::Positions)? else { return Ok(()) };
        let Some(c) = db.car(car)? else { return Ok(()) };
        let mut wanted = Vec::new();
        let mut bytes = 0;
        for d in records::meta(db, car)?.days {
            let rec = records::day(db, car, d)?;
            let mut learned = Vec::new();
            for (name, e) in &rec.logs {
                if !matches!(e.outcome, Outcome::Decoded { .. }) {
                    continue;
                }
                let uses = match e.internal_gps {
                    Some(u) => u,
                    None => {
                        let u = self.log_uses_internal_gps(&c, name)?;
                        learned.push((name.clone(), u));
                        u
                    }
                };
                if uses {
                    wanted.push((e.key.clone(), name.clone()));
                }
            }
            if !learned.is_empty() {
                let l = self.car_lock(car);
                let _held = lock(&l);
                let mut rec = records::day(db, car, d)?;
                for (name, u) in learned {
                    if let Some(e) = rec.logs.get_mut(&name) {
                        e.internal_gps = Some(u);
                    }
                }
                records::put_day(db, car, d, &rec)?;
            }
        }
        if let Some(j) = job {
            let rt = self.handle()?;
            for (key, name) in &wanted {
                bytes += rt.block_on(self.inner.judge.record(key, name)).and_then(|r| r.length).unwrap_or(0);
            }
            j.add_total(wanted.len() as u64, bytes);
        }
        // A day of logs at a time ([`crate::Options::positions_by_day`]): the car's lock held over the day's logs,
        // their records, samples, Now and firmware notes held in memory (the car's only) and written before the lock
        // is let go, as the day's last log would leave them, then the windows the day's logs change rebuilt once.
        // Nothing else writes the car's records without its lock, so nothing is held but the day's logs' own. A
        // kill in the middle of a day leaves the day as it was and the change noted: it is done again from the start.
        let by_day = self.inner.opts.positions_by_day;
        let mut days: Vec<Vec<(String, String)>> = Vec::new();
        let mut last_day = None;
        for (key, name) in wanted {
            let day = records::name_start_ms(&name).map(records::day_of);
            if !by_day || days.is_empty() || last_day != day {
                days.push(Vec::new());
            }
            last_day = day;
            if let Some(d) = days.last_mut() {
                d.push((key, name));
            }
        }
        for logs in days {
            if job.is_some_and(JobState::cancelled) {
                return Ok(());
            }
            {
                let l = self.car_lock(car);
                let _held = lock(&l);
                let holds = if by_day { DayHolds::new(self, car) } else { None };
                for (key, name) in &logs {
                    let r = self.log_locked(car, key, name, true, !by_day);
                    if let Err(e) = r {
                        if let Some(j) = job {
                            j.log_failed(name, &e);
                        }
                        self.failed(&e);
                    }
                    if let Some(j) = job {
                        j.step(self, Some(car), 0);
                    }
                }
                if let Some(h) = &holds {
                    h.write()?;
                }
            }
            if by_day {
                self.rebuild_dirty(car)?;
            }
        }
        self.job_clear_hook();
        self.clear_redo_if(car, Redo::Positions, generation)
    }

    /// Whether a stored log's positions come from the Screenmate's own GPS frames (read, nothing written).
    fn log_uses_internal_gps(&self, c: &model::Car, name: &str) -> Result<bool> {
        let rt = self.handle()?;
        let read = |n: &str| rt.block_on(self.inner.store.read(n)).map_err(|e| crate::Error::Store(e.to_string()));
        let Some(bytes) = read(name)? else { return Ok(false) };
        let side: serde_json::Value =
            read(&format!("{name}.json"))?.and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let profile = dbc::Profile::from_name(c.profile().dbc_name());
        Ok(timeline::read_log(&bytes, &side, profile, self.inner.opts.sample).is_ok_and(|r| r.internal_gps))
    }

    /// The narrow rebuild of a source order's change, when one is noted on the car: every window of its history
    /// built again from what is stored, in time order (the order says which input wins where they are joined; the
    /// samples each input stored stay as they are), nothing decoded again.
    pub(crate) fn redo_inputs(&self, car: CarId, job: Option<&JobState>) -> Result<()> {
        let db = &self.inner.db;
        let Some(generation) = records::redo(db, car, Redo::Inputs)? else { return Ok(()) };
        let meta = records::meta(db, car)?;
        let api = self.api_span(car)?;
        let first = [meta.first_ms, api.map(|a| a.0)].into_iter().flatten().min();
        // the newest data of any input (an API's days reach to their end: its newest reading)
        let newest = model::Input::apis().map(|i| meta.api_times(i).0).fold(meta.newest_ms, Option::max);
        if let (Some(a), Some(b)) = (first, newest) {
            let mut at = a;
            while at <= b {
                if job.is_some_and(JobState::cancelled) {
                    return Ok(());
                }
                // (the car's lock per window: logs that arrive meanwhile wait one window at most)
                let l = self.car_lock(car);
                let _held = lock(&l);
                let mut meta = records::meta(db, car)?;
                // a UTC day at a time (its window reaches on to the settled points around it, as a log's does)
                let end = (at / records::DAY_MS + 1) * records::DAY_MS;
                let (_, to) = self.rebuild_span(car, &mut meta, at, end.min(b.saturating_add(1)))?;
                at = to.max(end);
            }
        }
        self.job_clear_hook();
        self.clear_redo_if(car, Redo::Inputs, generation)
    }

    /// Notes a narrow rebuild asked for (a later generation of its note), under the car's lock.
    pub(crate) fn note_redo(&self, car: CarId, what: Redo) -> Result<()> {
        let l = self.car_lock(car);
        let _held = lock(&l);
        records::bump_redo(&self.inner.db, car, what).map(|_| ())
    }

    /// Takes a narrow rebuild's note away once done, unless it was asked for again since the generation `generation` the
    /// job began with: that ask has its own job, which finds the note.
    pub(crate) fn clear_redo_if(&self, car: CarId, what: Redo, generation: u64) -> Result<()> {
        let l = self.car_lock(car);
        let _held = lock(&l);
        records::clear_redo_if(&self.inner.db, car, what, generation)
    }

    /// Tests: called by a job just before it takes its narrow rebuilds' notes away.
    fn job_clear_hook(&self) {
        let hook = lock(&self.inner.job_clear_hook).clone();
        if let Some(h) = hook {
            h();
        }
    }

    /// Tests: calls `hook` in a job just before it clears the notes of the narrow rebuilds it did.
    #[doc(hidden)]
    pub fn job_clear_hook_set(&self, hook: Option<crate::full::ExitHook>) {
        *lock(&self.inner.job_clear_hook) = hook;
    }

    /// A job as it is now (kept until the engine stops).
    pub fn job(&self, id: u64) -> Option<Job> {
        lock(&self.inner.jobs).get(&id).map(|s| s.view())
    }

    /// Every job since the engine started, newest first.
    pub fn jobs(&self) -> Vec<Job> {
        let mut v: Vec<Job> = lock(&self.inner.jobs).values().map(|s| s.view()).collect();
        v.sort_by_key(|j| std::cmp::Reverse(j.id));
        v
    }

    /// Asks a running job to stop after the log it is on. The car it was on gets the logs it did not reach worked on
    /// at once, as a comparison does (never cleared again); cars it did not start on are left as they were. Whether there was such a job running.
    pub fn cancel(&self, id: u64) -> bool {
        match lock(&self.inner.jobs).get(&id) {
            Some(s) if *lock(&s.status) == JobStatus::Running => {
                s.cancel.store(true, Ordering::SeqCst);
                true
            }
            _ => false,
        }
    }

    /// Rebuilds a car's history after a change to the order its live inputs win in (its `source_order`): noted
    /// on the car at once (a restart before the job carries it out does it then), then its windows built again in
    /// the background ([`Engine::redo_inputs`]); saves in a row are one rebuild.
    pub(crate) fn rebuild_for_order(&self, car: CarId) -> Result<()> {
        self.note_redo(car, Redo::Inputs)?;
        if self.handle().is_ok() {
            self.rebuild_because(Scope::car(car), What::INPUTS, Cause::source_order(), true)?;
        }
        Ok(())
    }

    /// Whether a car has a rebuild under way (or one asked for, or one a restart cut off).
    pub fn rebuild_pending(&self, car: CarId) -> bool {
        let db = &self.inner.db;
        records::rebuild_mark(db, car).ok().flatten().is_some()
            || !records::dirty(db, car).unwrap_or_default().is_empty()
            || records::redo(db, car, Redo::Positions).unwrap_or(None).is_some()
            || records::redo(db, car, Redo::Inputs).unwrap_or(None).is_some()
            || lock(&self.inner.pending_jobs).keys().any(|k| k.is_none() || *k == Some(car))
    }
}

/// The holds of a GPS layout change's day of logs in place ([`Engine::redo_positions`]): one car's records
/// ([`records::Hold::of`]), samples, Now and firmware notes held while its lock is held; turned off when dropped,
/// what was not written dropped with them (as a kill would). One car at a time: `None` when another holds.
struct DayHolds<'e> {
    e: &'e Engine,
    records: records::Hold,
    _one: std::sync::MutexGuard<'e, ()>,
}

impl<'e> DayHolds<'e> {
    fn new(e: &'e Engine, car: CarId) -> Option<DayHolds<'e>> {
        let one = e.inner.day_holding.try_lock().ok()?;
        // (a full rebuild's shadow holds on its own engine; this engine holds nothing else meanwhile)
        e.hold_car(Some(car));
        let records = records::Hold::of(&e.inner.db, Some(car));
        e.smo_hold(true);
        e.now_hold(true);
        e.fw_hold(true);
        Some(DayHolds { e, records, _one: one })
    }

    /// Writes what is held, in the order a log writes it (its samples, its Now, its firmware notes, its records).
    fn write(&self) -> Result<()> {
        self.e.smo_write_held()?;
        self.e.now_write_held()?;
        self.e.fw_write_held()?;
        self.records.write()
    }
}

impl Drop for DayHolds<'_> {
    fn drop(&mut self) {
        self.e.smo_hold(false);
        self.e.now_hold(false);
        self.e.fw_hold(false);
        self.e.hold_car(None);
    }
}
