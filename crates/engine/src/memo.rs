//! Answers' parts kept in memory while the data they were worked out from stays the same: each is kept with the
//! car's [`db::Version`] it was made at (and a key of what else it rests on), and made again once that moved on. Lost
//! at a restart (the versions start again too).

use crate::Engine;
use db::Version;
use model::CarId;
use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Entries kept at most; past it the oldest half goes.
const MAX: usize = 512;

/// Which parts of a car's version a kept value rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum On {
    /// Imported rows and points (and the car's record).
    Imported,
    /// Everything of the car, and the places and settings.
    All,
}

fn key_of(v: Version, on: On) -> (u64, u64, u64) {
    match on {
        On::Imported => (0, v.imported, 0),
        On::All => (v.live, v.imported, v.shared),
    }
}

type Slot = ((u64, u64, u64), u64, Arc<dyn Any + Send + Sync>);

#[derive(Default)]
pub(crate) struct Memo {
    map: Mutex<HashMap<(CarId, &'static str, u64), Slot>>,
    tick: std::sync::atomic::AtomicU64,
}

impl Memo {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(CarId, &'static str, u64), Slot>> {
        self.map.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Engine {
    /// `make()`'s value, kept while the car's data that `on` names (and `extra`, a key of anything else it rests on)
    /// stays the same. The version is read before `make` runs, so a change during it makes it again next time.
    pub(crate) fn memo<T, E>(
        &self,
        car: CarId,
        name: &'static str,
        on: On,
        extra: u64,
        make: impl FnOnce() -> Result<T, E>,
    ) -> Result<Arc<T>, E>
    where
        T: Send + Sync + 'static,
    {
        let at = key_of(self.inner.db.version(car), on);
        let memo = &self.inner.memo;
        let k = (car, name, extra);
        if let Some((v, _, val)) = memo.lock().get(&k)
            && *v == at
            && let Ok(t) = val.clone().downcast::<T>()
        {
            return Ok(t);
        }
        let t = Arc::new(make()?);
        let tick = memo.tick.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut m = memo.lock();
        if m.len() >= MAX {
            let mut ticks: Vec<u64> = m.values().map(|s| s.1).collect();
            ticks.sort_unstable();
            let cut = ticks[ticks.len() / 2];
            m.retain(|_, s| s.1 > cut);
        }
        m.insert(k, (at, tick, t.clone()));
        Ok(t)
    }
}

/// A key of a value that can be serialized (what else a kept value rests on).
pub(crate) fn key<T: serde::Serialize>(v: &T) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_vec(v).unwrap_or_default().hash(&mut h);
    h.finish()
}
