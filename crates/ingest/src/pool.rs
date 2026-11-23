//! The judges' own threads: one per core ([`std::thread::available_parallelism`]), for the work of checking logs
//! (reading a log and its sidecar, the SHA-256, the signature). Each log's check stands on its own, so as many run at
//! once as there are cores, and none of it waits for a car key's lock (only writing the verdicts does).
//!
//! The threads are the judges' alone: a full rebuild or the decode queue (the engine's threads) never takes one, so
//! a log that just arrived is checked at once whatever else the server is doing. Two queues: a log that just arrived
//! goes before the logs of a car judged again whole ([`crate::Judge::rejudge_all`] and the like), which only take a
//! thread no arrival is waiting for.

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::{Condvar, Mutex, OnceLock, PoisonError};

type Job = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct Queues {
    urgent: VecDeque<Job>,
    bulk: VecDeque<Job>,
}

struct Pool {
    queues: Mutex<Queues>,
    ready: Condvar,
    threads: usize,
}

static POOL: OnceLock<Pool> = OnceLock::new();

fn pool() -> &'static Pool {
    POOL.get_or_init(|| {
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get());
        for i in 0..threads {
            let spawned = std::thread::Builder::new().name(format!("judge-{i}")).spawn(work);
            if let Err(e) = spawned {
                tracing::error!("could not start a thread to check CAN logs: {e}");
            }
        }
        Pool { queues: Mutex::new(Queues::default()), ready: Condvar::new(), threads }
    })
}

fn work() {
    let p = pool();
    loop {
        let job = {
            let mut q = p.queues.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if let Some(j) = q.urgent.pop_front().or_else(|| q.bulk.pop_front()) {
                    break j;
                }
                q = p.ready.wait(q).unwrap_or_else(PoisonError::into_inner);
            }
        };
        // a job that panics drops its answer (its caller gets an error) and the thread goes on
        let _ = std::panic::catch_unwind(AssertUnwindSafe(job));
    }
}

/// The judges' threads.
pub fn threads() -> usize {
    pool().threads
}

fn push(urgent: bool, job: Job) {
    let p = pool();
    let mut q = p.queues.lock().unwrap_or_else(PoisonError::into_inner);
    if urgent {
        q.urgent.push_back(job)
    } else {
        q.bulk.push_back(job)
    }
    drop(q);
    p.ready.notify_one();
}

fn stopped() -> std::io::Error {
    std::io::Error::other("a thread checking CAN logs stopped")
}

/// Runs `f` on one of the judges' threads. `urgent`: before the bulk work queued.
pub(crate) async fn run<T: Send + 'static>(urgent: bool, f: impl FnOnce() -> T + Send + 'static) -> std::io::Result<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    push(
        urgent,
        Box::new(move || {
            let _ = tx.send(f());
        }),
    );
    rx.await.map_err(|_| stopped())
}

/// Runs every job on the judges' threads, as many at once as there are threads, from a blocking thread: the
/// answers in the jobs' order.
pub(crate) fn run_all<T: Send + 'static>(
    urgent: bool,
    jobs: Vec<Box<dyn FnOnce() -> T + Send>>,
) -> std::io::Result<Vec<T>> {
    let mut answers = Vec::with_capacity(jobs.len());
    for f in jobs {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        push(
            urgent,
            Box::new(move || {
                let _ = tx.send(f());
            }),
        );
        answers.push(rx);
    }
    answers.into_iter().map(|rx| rx.recv().map_err(|_| stopped())).collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    #[test]
    fn every_answer_comes_back_in_order() {
        let jobs: Vec<Box<dyn FnOnce() -> u64 + Send>> =
            (0..500u64).map(|i| Box::new(move || i * i) as Box<dyn FnOnce() -> u64 + Send>).collect();
        let out = super::run_all(false, jobs).unwrap();
        assert_eq!(out, (0..500u64).map(|i| i * i).collect::<Vec<_>>());
        assert!(super::threads() >= 1);
    }

    #[test]
    fn a_job_that_panics_is_an_error_and_the_threads_go_on() {
        let jobs: Vec<Box<dyn FnOnce() -> u64 + Send>> = vec![Box::new(|| panic!("made-up failure")), Box::new(|| 7)];
        assert!(super::run_all(true, jobs).is_err());
        let jobs: Vec<Box<dyn FnOnce() -> u64 + Send>> =
            (0..super::threads() * 2 + 1).map(|_| Box::new(|| 1u64) as Box<dyn FnOnce() -> u64 + Send>).collect();
        assert_eq!(super::run_all(true, jobs).unwrap().len(), super::threads() * 2 + 1);
    }
}
