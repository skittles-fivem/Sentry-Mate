//! A car's stored logs read and decoded ahead, on other cores, while the car's own work goes on one log after the
//! other: [`Engine::reconcile_car`] (the logs found at the start, a car rebuilt whole, a full rebuild) hands the
//! names it is about to work on to [`Ahead::run`], and [`Engine::log_locked`] takes each log's reading from it in
//! the same order.
//!
//! Only the part of a log that depends on the log alone is done ahead: its stored file and sidecar read and its
//! frames decoded into samples ([`timeline::read_log_with`], a pure function of the bytes, the car's DBC set, the
//! sample parameters and the car's GPS layout). Everything that reads or writes the car's history (its samples, the
//! day records, the windows of its timeline) stays where it was, in order, under the car's lock. A reading made with
//! another DBC set or GPS layout than the car has when its log's turn comes (the VIN seen in a log meanwhile, a
//! layout applied) is not used: that log is decoded again in place, as without this. So the history is the very one
//! a log-by-log decoding gives (`crates/sentry-mate/src/window_batch.rs` compares everything stored, ids included).
//!
//! At most [`AHEAD_PER_THREAD`] logs a thread are read ahead of the one being worked on (a minute's log is about a
//! megabyte packed), so memory stays small however long the car's history.

use crate::{Engine, Error, Result, lock};
use model::CarId;
use std::collections::HashMap;
use std::sync::{Condvar, Mutex};

/// Logs read ahead at most, per thread.
pub(crate) const AHEAD_PER_THREAD: usize = 2;

/// A stored log as read: its bytes and its sidecar, `None` when it is not stored.
pub(crate) type Stored = Option<(zeroize::Zeroizing<Vec<u8>>, serde_json::Value)>;

/// A log read and decoded ahead.
pub(crate) struct Read {
    pub(crate) stored: Result<Stored>,
    /// The DBC set and GPS layout it was decoded with, and what that gave (`None`: not decoded, the car's record or
    /// layout could not be read; it is decoded in place).
    pub(crate) profile: String,
    pub(crate) gps: Option<model::gps::GpsLayout>,
    pub(crate) decoded: Option<std::result::Result<timeline::LogRead, logfmt::Error>>,
}

impl Read {
    /// The decoding, when it was made with this DBC set and GPS layout.
    pub(crate) fn decoded_with(
        &mut self,
        profile: &str,
        gps: Option<&model::gps::GpsLayout>,
    ) -> Option<std::result::Result<timeline::LogRead, logfmt::Error>> {
        if self.profile == profile && self.gps.as_ref() == gps { self.decoded.take() } else { None }
    }
}

struct State {
    /// The next log a thread takes, and the one the car's work waits for.
    next: usize,
    wanted: usize,
    done: HashMap<usize, Read>,
    stop: bool,
}

/// The readings of one run of logs, handed out in order.
pub(crate) struct Ahead {
    state: Mutex<State>,
    moved: Condvar,
    names: Vec<String>,
    ahead: usize,
}

impl Ahead {
    /// Threads that read logs ahead, from the machine's cores: one left for the car's own work and the server's
    /// other work (the pages, the uploads), eight at most. `0` on one or two cores: logs are read in place.
    pub(crate) fn threads() -> usize {
        let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        if cores <= 2 { 0 } else { (cores - 1).min(8) }
    }

    /// Runs `work` with `names` (logs of `car`, in the order `work` asks for them) read ahead on `threads` threads
    /// ([`Ahead::take`]); with fewer than two logs or no threads, `work` gets `None` and reads in place. The threads
    /// stop when `work` returns, early or not (each finishes the log it is on).
    pub(crate) fn run<T>(
        e: &Engine,
        car: CarId,
        names: Vec<String>,
        threads: usize,
        work: impl FnOnce(Option<&Ahead>) -> T,
    ) -> T {
        if threads == 0 || names.len() < 2 {
            return work(None);
        }
        let threads = threads.min(names.len());
        let a = Ahead {
            state: Mutex::new(State { next: 0, wanted: 0, done: HashMap::new(), stop: false }),
            moved: Condvar::new(),
            names,
            ahead: threads * AHEAD_PER_THREAD,
        };
        std::thread::scope(|s| {
            for i in 0..threads {
                let (a, e) = (&a, e.clone());
                // (a thread that can't start leaves its logs to the others; with none, each is read in place)
                let _ = std::thread::Builder::new()
                    .name(format!("log-ahead-{i}"))
                    .spawn_scoped(s, move || a.worker(&e, car));
            }
            struct Stop<'a>(&'a Ahead);
            impl Drop for Stop<'_> {
                fn drop(&mut self) {
                    lock(&self.0.state).stop = true;
                    self.0.moved.notify_all();
                }
            }
            let _stop = Stop(&a);
            work(Some(&a))
        })
    }

    fn worker(&self, e: &Engine, car: CarId) {
        loop {
            let i = {
                let mut st = lock(&self.state);
                loop {
                    // (the server stopping, or its data moved away: nothing more is read ahead)
                    if st.stop || st.next >= self.names.len() || e.is_stopping() {
                        return;
                    }
                    if st.next < st.wanted + self.ahead {
                        break;
                    }
                    st = self.moved.wait(st).unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                st.next += 1;
                st.next - 1
            };
            let r = e.read_ahead(car, &self.names[i]);
            lock(&self.state).done.insert(i, r);
            self.moved.notify_all();
        }
    }

    /// The reading of `name` when it is the next log of the run (`None`: it is read in place: a log not read ahead,
    /// or one no thread took).
    pub(crate) fn take(&self, name: &str) -> Option<Read> {
        let mut st = lock(&self.state);
        let i = st.wanted;
        if self.names.get(i).map(String::as_str) != Some(name) {
            return None;
        }
        st.wanted += 1;
        // (the threads go on to the logs after it)
        self.moved.notify_all();
        loop {
            if let Some(r) = st.done.remove(&i) {
                return Some(r);
            }
            // taken by no thread yet, and none will take it: read in place
            if st.next <= i {
                st.next = st.next.max(i + 1);
                return None;
            }
            st = self.moved.wait(st).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

impl Engine {
    /// A stored log and its sidecar, as the store has them now (`None`: not stored).
    pub(crate) fn read_stored(&self, name: &str) -> Result<Stored> {
        let rt = self.handle()?;
        let read = |n: &str| rt.block_on(self.inner.store.read(n)).map_err(|e| Error::Store(e.to_string()));
        let Some(bytes) = read(name)? else { return Ok(None) };
        let side: serde_json::Value =
            read(&format!("{name}.json"))?.and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Ok(Some((bytes, side)))
    }

    /// A log of `car` read and decoded with the car's DBC set and GPS layout as they are now.
    fn read_ahead(&self, car: CarId, name: &str) -> Read {
        let stored = self.read_stored(name);
        let car_now = self.inner.db.car(car).ok().flatten();
        let gps = self.gps_layout(car).ok();
        let profile = car_now.as_ref().map(|c| c.profile().dbc_name().to_string()).unwrap_or_default();
        let decoded = match (&stored, car_now, gps) {
            (Ok(Some((bytes, side))), Some(_), Some(gps)) => Some(timeline::read_log_with(
                bytes,
                side,
                dbc::Profile::from_name(&profile),
                self.inner.opts.sample,
                gps,
            )),
            _ => None,
        };
        Read { stored, profile, gps: gps.flatten(), decoded }
    }
}
