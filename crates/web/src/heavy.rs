//! Answers that decode stored CAN logs while they answer (`/signals`, `/signals/latest`, the battery pages, the
//! efficiency page): a few at a time for the whole server, a few at a time per account, and stopped once their request
//! is gone.
//!
//! At most [`RUNNING`] such queries run at once; more wait their turn (in order) for at most [`WAIT`], then get
//! `503 busy` with `Retry-After`. One account has at most [`PER_ACCOUNT`] of them running or waiting; one more is
//! `429 throttled`. A query's place is held until its blocking work really ends, not only until its request is
//! dropped, and a dropped request (the client went away, or the request timed out) sets the query's cancel flag
//! ([`engine::query::decoding::with_cancel`]), which the decoders look at between logs and every few thousand
//! lines.

use crate::{ApiError, ApiResult};
use accounts::AccountId;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Log-decoding queries running at once, at most, for the whole server.
pub const RUNNING: usize = 3;
/// Log-decoding queries of one account running or waiting, at most.
pub const PER_ACCOUNT: usize = 4;
/// How long a query waits for its turn before `503 busy`.
pub const WAIT: Duration = Duration::from_secs(20);

pub struct Heavy {
    size: usize,
    running: Arc<Semaphore>,
    per: Arc<Mutex<HashMap<AccountId, usize>>>,
    wait: Duration,
}

impl Default for Heavy {
    fn default() -> Heavy {
        Heavy::new(RUNNING, WAIT)
    }
}

/// An account's place among the queries running or waiting, given back when dropped.
struct Place {
    per: Arc<Mutex<HashMap<AccountId, usize>>>,
    account: AccountId,
}

impl Drop for Place {
    fn drop(&mut self) {
        let mut per = self.per.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = per.get_mut(&self.account) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                per.remove(&self.account);
            }
        }
    }
}

/// A turn to run: held by the blocking work until it ends.
pub struct Turn {
    _permit: OwnedSemaphorePermit,
    _place: Place,
}

/// Sets the flag when dropped: the request's future went away (or finished: then nobody is looking).
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

impl Heavy {
    pub fn new(running: usize, wait: Duration) -> Heavy {
        let size = running.max(1);
        Heavy { size, running: Arc::new(Semaphore::new(size)), per: Arc::default(), wait }
    }

    /// Waits for a turn for `account`: `429` when it has [`PER_ACCOUNT`] running or waiting, `503` after the wait.
    pub async fn turn(&self, account: AccountId) -> ApiResult<Turn> {
        let place = {
            let mut per = self.per.lock().unwrap_or_else(|e| e.into_inner());
            let n = per.entry(account).or_default();
            if *n >= PER_ACCOUNT {
                return Err(ApiError::throttled(2));
            }
            *n += 1;
            Place { per: self.per.clone(), account }
        };
        match tokio::time::timeout(self.wait, self.running.clone().acquire_owned()).await {
            Ok(Ok(permit)) => Ok(Turn { _permit: permit, _place: place }),
            _ => Err(ApiError::new(503, "busy", "The server is busy reading logs. Try again in a moment.")
                .header(http::header::RETRY_AFTER, "5")),
        }
    }

    /// Queries running now (for tests and the status page).
    pub fn running(&self) -> usize {
        self.size.saturating_sub(self.running.available_permits())
    }
}

/// Runs `f` (a log-decoding engine query) off the async threads in `turn`, with a cancel flag that is set once this
/// future is dropped.
pub(crate) async fn blocking<R: Send + 'static>(
    turn: Turn,
    f: impl FnOnce() -> ApiResult<R> + Send + 'static,
) -> ApiResult<R> {
    let flag = Arc::new(AtomicBool::new(false));
    let _cancel = CancelOnDrop(flag.clone());
    tokio::task::spawn_blocking(move || {
        let _turn = turn;
        engine::query::decoding::with_cancel(flag, f)
    })
    .await
    .map_err(|e| ApiError::server(format!("a task failed: {e}")))?
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;
    use engine::query::decoding::cancelled;
    use std::time::Instant;

    fn status(r: &ApiResult<Turn>) -> Option<u16> {
        r.as_ref().err().map(|e| e.status)
    }

    #[tokio::test]
    async fn one_account_waits_its_turn_and_cannot_queue_without_end() {
        let h = Heavy::new(1, Duration::from_millis(100));
        let first = h.turn(1).await.unwrap();
        assert_eq!(h.running(), 1);
        // the server's one turn is taken: another waits, then is told the server is busy
        let t = Instant::now();
        let busy = h.turn(2).await;
        assert_eq!(status(&busy), Some(503));
        assert!(t.elapsed() >= Duration::from_millis(100));
        drop(first);
        assert!(h.turn(2).await.is_ok(), "free again");

        // one account: at most PER_ACCOUNT running or waiting, the next one refused at once
        let h = Arc::new(Heavy::new(1, Duration::from_secs(30)));
        let held = h.turn(7).await.unwrap();
        let mut waiting = Vec::new();
        for _ in 1..PER_ACCOUNT {
            let h = h.clone();
            waiting.push(tokio::spawn(async move { h.turn(7).await.map(|_| ()) }));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let t = Instant::now();
        assert_eq!(status(&h.turn(7).await), Some(429));
        assert!(t.elapsed() < Duration::from_secs(1), "refused at once");
        // another account still queues
        let other = {
            let h = h.clone();
            tokio::spawn(async move { h.turn(8).await.map(|_| ()) })
        };
        drop(held);
        for w in waiting {
            assert!(w.await.unwrap().is_ok());
        }
        assert!(other.await.unwrap().is_ok());
        assert!(h.per.lock().unwrap().is_empty(), "every place given back");
    }

    #[test]
    fn a_cancelled_query_is_not_logged_as_server_trouble() {
        let gone = || engine::query::QueryError::Engine(engine::Error::Store("the request was dropped".into()));
        assert_eq!(crate::resources::now::failed(gone()).status, 500);
        let flag = Arc::new(AtomicBool::new(true));
        let e = engine::query::decoding::with_cancel(flag, || crate::resources::now::failed(gone()));
        assert_eq!(e.status, 408);
    }

    #[tokio::test]
    async fn a_dropped_request_stops_its_work_and_holds_its_turn_until_the_work_ends() {
        let h = Heavy::new(1, Duration::from_millis(50));
        let turn = h.turn(1).await.unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let req = tokio::spawn(blocking(turn, move || {
            // a long decode: it looks at the flag as it goes
            let t = Instant::now();
            while !cancelled() && t.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(100));
            tx.send(cancelled()).unwrap();
            Ok(())
        }));
        tokio::time::sleep(Duration::from_millis(50)).await;
        // the client goes away (or the request times out): its future is dropped
        req.abort();
        let _ = req.await;
        // the work is still ending: its turn is not free yet
        assert_eq!(status(&h.turn(2).await), Some(503));
        let seen = tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(5))).await.unwrap();
        assert_eq!(seen, Ok(true), "the work saw it was cancelled");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(h.turn(2).await.is_ok(), "free once the work ended");
    }
}
