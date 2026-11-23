//! Rebuilding a car (or every car) from its raw logs: everything worked out from CAN logs in the period is taken out,
//! then each log is worked on again in time order, as if it had just arrived. Imported TeslaMate rows are never taken
//! out. A job runs in the background, counts its progress, and can be cancelled; a restart carries on with it (the car
//! keeps a mark until its logs are all done again, and [`Engine::reconcile`] picks the rest up).

use crate::records::{self, RebuildMark};
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
    pub from_ms: u64,
    pub to_ms: u64,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    pub took_ms: Option<u64>,
    pub error: Option<String>,
}

pub(crate) struct JobState {
    id: u64,
    car: Option<CarId>,
    from_ms: u64,
    to_ms: u64,
    started_ms: u64,
    done: AtomicU64,
    total: AtomicU64,
    cancel: AtomicBool,
    status: Mutex<JobStatus>,
    finished_ms: Mutex<Option<u64>>,
    error: Mutex<Option<String>>,
}

impl JobState {
    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    pub(crate) fn add_total(&self, n: u64) {
        self.total.fetch_add(n, Ordering::SeqCst);
    }

    pub(crate) fn step(&self, e: &Engine, car: Option<CarId>) {
        let done = self.done.fetch_add(1, Ordering::SeqCst) + 1;
        let total = self.total.load(Ordering::SeqCst);
        if done.is_multiple_of(50) || done == total {
            e.emit(Event::Job { job: self.id, car, status: JobStatus::Running, done, total });
        }
    }

    fn view(&self) -> Job {
        let finished_ms = *lock(&self.finished_ms);
        Job {
            id: self.id,
            car: self.car,
            status: *lock(&self.status),
            done: self.done.load(Ordering::SeqCst),
            total: self.total.load(Ordering::SeqCst),
            from_ms: self.from_ms,
            to_ms: self.to_ms,
            started_ms: self.started_ms,
            finished_ms,
            took_ms: finished_ms.map(|f| f.saturating_sub(self.started_ms)),
            error: lock(&self.error).clone(),
        }
    }
}

impl Engine {
    /// Starts a rebuild in the background and returns at once.
    pub fn rebuild(&self, scope: Scope) -> Result<Job> {
        let handle = self.handle()?;
        let id = self.inner.next_job.fetch_add(1, Ordering::SeqCst);
        let (from_ms, to_ms) =
            (scope.from_ms.unwrap_or(0), scope.to_ms.unwrap_or(u64::MAX).max(scope.from_ms.unwrap_or(0)));
        let st = Arc::new(JobState {
            id,
            car: scope.car,
            from_ms,
            to_ms,
            started_ms: now_ms(),
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
            status: Mutex::new(JobStatus::Running),
            finished_ms: Mutex::new(None),
            error: Mutex::new(None),
        });
        lock(&self.inner.jobs).insert(id, st.clone());
        let e = self.clone();
        self.inner.outstanding_add();
        handle.spawn_blocking(move || {
            let r = e.run_rebuild(&st);
            let status = match &r {
                Err(err) => {
                    *lock(&st.error) = Some(err.to_string());
                    e.failed(err);
                    JobStatus::Failed
                }
                Ok(()) if st.cancelled() => JobStatus::Cancelled,
                Ok(()) => JobStatus::Done,
            };
            *lock(&st.status) = status;
            *lock(&st.finished_ms) = Some(now_ms());
            let v = st.view();
            e.emit(Event::Job { job: v.id, car: v.car, status, done: v.done, total: v.total });
            e.inner.outstanding_sub();
        });
        Ok(self.job(id).unwrap_or_else(|| unreachable_job(id)))
    }

    fn run_rebuild(&self, st: &JobState) -> Result<()> {
        let db = &self.inner.db;
        let cars: Vec<CarId> = match st.car {
            Some(c) => vec![c],
            None => {
                let mut out = Vec::new();
                let mut q = db::FleetQuery { search: None, limit: 500, after: None };
                loop {
                    let page = db.fleet(&q)?;
                    out.extend(page.cars.iter().map(|e| e.car.id));
                    match page.next {
                        Some(n) => q.after = Some(n),
                        None => break,
                    }
                }
                out
            }
        };
        for car in cars {
            if st.cancelled() {
                break;
            }
            {
                let l = self.car_lock(car);
                let _held = lock(&l);
                let mut mark =
                    RebuildMark { from_ms: st.from_ms, to_ms: st.to_ms, started_ms: st.started_ms, cleared: false };
                records::put_rebuild_mark(db, car, Some(&mark))?;
                self.clear_window(car, st.from_ms, st.to_ms)?;
                mark.cleared = true;
                records::put_rebuild_mark(db, car, Some(&mark))?;
            }
            self.reconcile_car(car, Some(st))?;
            if st.cancelled() {
                // the car's window is cleared: the logs this job did not reach are worked on now, in the background
                // (only those: what it redid is kept)
                self.send(crate::Work::Reconcile { car: Some(car) });
                break;
            }
        }
        Ok(())
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

    /// Rebuilds a car's whole history after a change to the order its live inputs win in (its `source_order`): noted
    /// pending at once (a restart before the job carries it out does it then), then rebuilt in the background as a
    /// change of preferred key is.
    pub(crate) fn rebuild_for_order(&self, car: CarId) -> Result<()> {
        {
            let l = self.car_lock(car);
            let _held = lock(&l);
            let mark = RebuildMark { from_ms: 0, to_ms: u64::MAX, started_ms: now_ms(), cleared: false };
            records::put_rebuild_mark(&self.inner.db, car, Some(&mark))?;
        }
        if self.handle().is_ok() {
            self.rebuild(Scope::car(car))?;
        }
        Ok(())
    }

    /// Whether a car has a rebuild under way (or one a restart cut off).
    pub fn rebuild_pending(&self, car: CarId) -> bool {
        records::rebuild_mark(&self.inner.db, car).ok().flatten().is_some()
            || !records::dirty(&self.inner.db, car).unwrap_or_default().is_empty()
    }
}

fn unreachable_job(id: u64) -> Job {
    Job {
        id,
        car: None,
        status: JobStatus::Running,
        done: 0,
        total: 0,
        from_ms: 0,
        to_ms: 0,
        started_ms: now_ms(),
        finished_ms: None,
        took_ms: None,
        error: None,
    }
}
