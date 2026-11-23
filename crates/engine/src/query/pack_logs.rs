//! The pack's own readings of the days ([`super::battery`]'s day readings), worked out ahead in the background so a
//! battery answer finds them done and decodes no log while it answers.
//!
//! - **A fill** ([`start`]) works out every record day of a car whose reading is missing or stale (its newest logs
//!   changed), newest day first, on all cores, as an answer would (and kept as an answer keeps it). It runs once the
//!   engine is idle after a start, again every [`PASS_EVERY`] when idle, and whenever an answer needed a day not done.
//! - **The logs' own readings** are kept too ([`NS`], per record day, the newest few logs of the day): a day whose
//!   newest log changed, but whose reading comes from a log read before, is worked out without decoding.
//! - **An answer** first runs as one that may not decode ([`light`]): when every day it needs is done, it answers so,
//!   without waiting for a turn; else it asks for a fill, waits for it (a while), and tries once more; only past that
//!   does it decode what it needs itself, in its turn (the web side's `heavy`).

use super::battery::PackReading;
use crate::{Engine, Result, lock, records};
use model::CarId;
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// The key/value namespace of the logs' pack readings, per record day.
pub const NS: &str = "battery-logs";
/// Logs of a day whose readings are kept (the newest; the battery pages look at the newest 3).
const KEEP: usize = 6;
/// How often an idle engine looks for days to work out (the logs of a day change as they arrive).
pub const PASS_EVERY: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DayLogs {
    logs: BTreeMap<String, LogPack>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LogPack {
    /// The log's record when it was decoded (`LogEntry::done_ms`).
    done_ms: u64,
    /// The DBC set it was decoded with.
    profile: String,
    /// Where its data ends (to keep the newest).
    end_ms: u64,
    reading: PackReading,
}

fn key(car: CarId, day: u32) -> String {
    format!("car/{car}/day/{day}")
}

/// A day's kept log readings.
pub(crate) struct Kept(DayLogs);

impl Kept {
    pub(crate) fn read(e: &Engine, car: CarId, day: u32) -> Result<Kept> {
        Ok(Kept(records::kept::<DayLogs>(e.db(), NS, &key(car, day))?.unwrap_or_default()))
    }

    /// The reading of log `name` as recorded at `done_ms`, decoded with the DBC set `profile`.
    pub(crate) fn get(&self, name: &str, done_ms: u64, profile: &str) -> Option<PackReading> {
        self.0.logs.get(name).filter(|l| l.done_ms == done_ms && l.profile == profile).map(|l| l.reading.clone())
    }
}

/// Keeps log `name`'s reading on record day `day` (the newest [`KEEP`] of the day are kept).
#[allow(clippy::too_many_arguments)]
pub(crate) fn keep(
    e: &Engine,
    car: CarId,
    day: u32,
    name: &str,
    done_ms: u64,
    profile: &str,
    end_ms: u64,
    reading: PackReading,
) -> Result<()> {
    let k = key(car, day);
    let mut d = records::kept::<DayLogs>(e.db(), NS, &k)?.unwrap_or_default();
    d.logs.insert(name.to_string(), LogPack { done_ms, profile: profile.to_string(), end_ms, reading });
    if d.logs.len() > KEEP {
        let mut by: Vec<(u64, String)> = d.logs.iter().map(|(n, l)| (l.end_ms, n.clone())).collect();
        by.sort_unstable_by(|a, b| b.cmp(a));
        let gone: Vec<String> = by.into_iter().skip(KEEP).map(|(_, n)| n).collect();
        if gone.iter().any(|n| n == name) {
            // older than the day's newest: never looked at
            return Ok(());
        }
        for n in gone {
            d.logs.remove(&n);
        }
    }
    records::put(e.db(), NS, &k, &d)
}

// ---------------------------------------------------------------- answers that may not decode

thread_local! {
    /// `None`: decoding allowed; `Some(needed)`: not, and whether something needed it.
    static LIGHT: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Runs `f` (a battery answer, on this thread) so that it decodes no log: a day it would decode ends it with an
/// error instead. Also answers whether that happened (then `f`'s answer is not one to give).
pub fn light<R>(f: impl FnOnce() -> R) -> (R, bool) {
    struct Restore(Option<bool>);
    impl Drop for Restore {
        fn drop(&mut self) {
            LIGHT.with(|l| l.set(self.0));
        }
    }
    let restore = Restore(LIGHT.with(|l| l.replace(Some(false))));
    let r = f();
    let needed = LIGHT.with(|l| l.get()).unwrap_or(false);
    drop(restore);
    (r, needed)
}

/// Whether the query on this thread may decode a log now (else it is noted that it needed to).
pub(crate) fn may_decode() -> bool {
    LIGHT.with(|l| match l.get() {
        None => true,
        Some(_) => {
            l.set(Some(true));
            false
        }
    })
}

/// The error a query that may not decode ends with when it needed to.
pub(crate) fn not_done() -> super::QueryError {
    super::QueryError::Engine(crate::Error::Store("the pack's readings of a day are not worked out yet".into()))
}

// ---------------------------------------------------------------- the fill

/// The cars whose fill runs (and whether another was asked for while it ran).
#[derive(Default)]
pub(crate) struct Fills {
    running: Mutex<HashMap<CarId, bool>>,
    done: Condvar,
}

/// Starts working out `car`'s days in the background; one already running for it runs once more after.
pub fn start(e: &Engine, car: CarId) {
    let Ok(handle) = e.handle() else { return };
    {
        let mut r = lock(&e.inner.packs.running);
        if let Some(again) = r.get_mut(&car) {
            *again = true;
            return;
        }
        r.insert(car, false);
    }
    let e = e.clone();
    // (runs on its own: nothing waits for its handle)
    drop(handle.spawn_blocking(move || {
        loop {
            if let Err(err) = fill(&e, car) {
                tracing::warn!("car {car}: the pack's readings: {err}");
            }
            let mut r = lock(&e.inner.packs.running);
            if r.get(&car) == Some(&true) {
                r.insert(car, false);
                continue;
            }
            r.remove(&car);
            e.inner.packs.done.notify_all();
            return;
        }
    }));
}

/// Waits up to `wait` for `car`'s fill to end (at once when none runs): whether it ended.
pub fn wait(e: &Engine, car: CarId, wait: Duration) -> bool {
    let until = Instant::now() + wait;
    let mut r = lock(&e.inner.packs.running);
    while r.contains_key(&car) {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        r = e.inner.packs.done.wait_timeout(r, left).unwrap_or_else(std::sync::PoisonError::into_inner).0;
    }
    true
}

/// Fills every car once the engine is idle after a start, then again every [`PASS_EVERY`] when idle.
pub(crate) async fn passes(e: Engine) {
    loop {
        e.idle().await;
        if let Ok(cars) = super::all_cars(&e) {
            for car in cars {
                start(&e, car);
            }
        }
        tokio::time::sleep(PASS_EVERY).await;
    }
}

/// Every record day's pack reading of `car`, newest day first, on all cores: each worked out (and kept) as an answer
/// would; a day done already costs reading its record.
fn fill(e: &Engine, car: CarId) -> Result<()> {
    let Some(c) = e.db().car(car)? else { return Ok(()) };
    let days: Vec<u32> = records::meta(e.db(), car)?.days.into_iter().rev().collect();
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get).min(days.len().max(1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failed: Mutex<Option<String>> = Mutex::new(None);
    std::thread::scope(|sc| {
        for _ in 0..threads {
            sc.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(&d) = days.get(i) else { return };
                    if let Err(err) = super::battery::day_pack(e, &c, d) {
                        *lock(&failed) = Some(err.to_string());
                        return;
                    }
                }
            });
        }
    });
    match lock(&failed).take() {
        Some(err) => Err(crate::Error::Store(err)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_light_query_notes_the_decoding_it_needed_and_decodes_nothing() {
        assert!(may_decode(), "outside, a query decodes");
        let ((), needed) = light(|| ());
        assert!(!needed);
        let (may, needed) = light(may_decode);
        assert!(!may && needed);
        // nested: the outer one sees the inner one's need only through its own calls
        let ((inner, may), outer) = light(|| (light(may_decode).1, may_decode()));
        assert!(inner && !may && outer);
        assert!(may_decode(), "restored after");
    }
}
