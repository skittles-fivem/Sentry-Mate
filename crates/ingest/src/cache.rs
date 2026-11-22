//! The two pieces that let any number of cars share one process: a bounded cache of recently used per-car state,
//! and a lock per car that exists only while someone holds it.
//!
//! Neither grows with the number of cars: the cache keeps at most its capacity (least recently used out first), and
//! the lock table forgets a car's lock as soon as nobody holds it.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, Weak};

/// A least-recently-used cache keyed by car key (or token hash). Every operation is O(log n).
pub(crate) struct Lru<V> {
    cap: usize,
    tick: u64,
    map: HashMap<String, (u64, V)>,
    /// Ticks in use order: the first is the least recently used.
    order: BTreeMap<u64, String>,
}

impl<V> Lru<V> {
    pub(crate) fn new(cap: usize) -> Lru<V> {
        Lru { cap: cap.max(1), tick: 0, map: HashMap::new(), order: BTreeMap::new() }
    }

    fn touch(&mut self, key: &str) -> Option<&mut V> {
        self.tick += 1;
        let tick = self.tick;
        let (t, v) = self.map.get_mut(key)?;
        self.order.remove(t);
        *t = tick;
        self.order.insert(tick, key.to_string());
        Some(v)
    }

    /// The value, now the most recently used.
    pub(crate) fn get(&mut self, key: &str) -> Option<&mut V> {
        self.touch(key)
    }

    /// Takes the value out (the caller holds the car's lock and puts it back when done).
    pub(crate) fn take(&mut self, key: &str) -> Option<V> {
        let (t, v) = self.map.remove(key)?;
        self.order.remove(&t);
        Some(v)
    }

    /// Puts a value in as the most recently used; the least recently used ones beyond the capacity are dropped.
    pub(crate) fn put(&mut self, key: String, value: V) {
        self.take(&key);
        self.tick += 1;
        self.order.insert(self.tick, key.clone());
        self.map.insert(key, (self.tick, value));
        while self.map.len() > self.cap {
            let Some((_, oldest)) = self.order.pop_first() else { break };
            self.map.remove(&oldest);
        }
    }

    pub(crate) fn remove(&mut self, key: &str) {
        self.take(key);
    }

    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    pub(crate) fn set_cap(&mut self, cap: usize) {
        self.cap = cap.max(1);
        while self.map.len() > self.cap {
            let Some((_, oldest)) = self.order.pop_first() else { break };
            self.map.remove(&oldest);
        }
    }

    /// The first value that matches (any order; for rare lookups by something other than the key).
    pub(crate) fn find(&self, mut matches: impl FnMut(&V) -> bool) -> Option<&V> {
        self.map.values().map(|(_, v)| v).find(|v| matches(v))
    }
}

/// One lock per car key, so cars work in parallel and one car's requests take turns. A key's lock lives as long as
/// someone holds it; the table drops forgotten ones as it grows, so it stays about the size of the cars in use now.
pub(crate) struct KeyLocks<T> {
    inner: Mutex<LockTable<T>>,
}

struct LockTable<T> {
    map: HashMap<String, Weak<T>>,
    prune_at: usize,
}

const MIN_PRUNE: usize = 64;

impl<T: Default> KeyLocks<T> {
    pub(crate) fn new() -> KeyLocks<T> {
        KeyLocks { inner: Mutex::new(LockTable { map: HashMap::new(), prune_at: MIN_PRUNE }) }
    }

    /// The key's lock (the same one for everyone asking while it is held).
    pub(crate) fn get(&self, key: &str) -> Arc<T> {
        // the table holds no invariant a panic could break halfway, so a poisoned one is still good to use
        let mut t = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(lock) = t.map.get(key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(T::default());
        t.map.insert(key.to_string(), Arc::downgrade(&lock));
        if t.map.len() > t.prune_at {
            t.map.retain(|_, w| w.strong_count() > 0);
            t.prune_at = (t.map.len() * 2).max(MIN_PRUNE);
        }
        lock
    }

    /// Entries in the table now (live and not yet pruned).
    pub(crate) fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner).map.len()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_least_recently_used_goes_first() {
        let mut c = Lru::new(3);
        for k in ["a", "b", "c"] {
            c.put(k.to_string(), k.len());
        }
        assert!(c.get("a").is_some()); // a is now the most recent
        c.put("d".into(), 1);
        assert_eq!(c.len(), 3);
        assert!(c.get("b").is_none(), "b was the least recently used");
        assert!(c.get("a").is_some() && c.get("c").is_some() && c.get("d").is_some());
        assert_eq!(c.take("c"), Some(1));
        assert_eq!(c.len(), 2);
        c.put("a".into(), 9);
        assert_eq!(c.get("a").copied(), Some(9));
        c.set_cap(1);
        assert_eq!(c.len(), 1);
        assert!(c.get("a").is_some(), "the most recent stays");
    }

    #[test]
    fn a_hundred_thousand_keys_stay_within_the_capacity() {
        let mut c = Lru::new(1000);
        for i in 0..100_000u32 {
            c.put(format!("car{i}"), i);
            if i % 7 == 0 {
                c.get("car0");
            }
        }
        assert_eq!(c.len(), 1000);
        assert_eq!(c.order.len(), 1000);
        assert!(c.get("car0").is_some(), "kept by being used");
        assert!(c.get("car99999").is_some());
        assert!(c.get("car5").is_none());
    }

    #[test]
    fn a_lock_is_shared_while_held_and_forgotten_after() {
        let locks: KeyLocks<Mutex<()>> = KeyLocks::new();
        let a = locks.get("car-a");
        let a2 = locks.get("car-a");
        assert!(Arc::ptr_eq(&a, &a2));
        drop((a, a2));
        for i in 0..10_000 {
            let _held = locks.get(&format!("car{i}"));
        }
        assert!(locks.len() <= MIN_PRUNE + 1, "{} entries for 10,000 cars used one at a time", locks.len());
        let held: Vec<_> = (0..500).map(|i| locks.get(&format!("busy{i}"))).collect();
        assert!(locks.len() >= 500 && locks.len() <= 1100, "{}", locks.len());
        drop(held);
        for i in 0..2000 {
            let _held = locks.get(&format!("later{i}"));
        }
        assert!(locks.len() <= MIN_PRUNE + 1, "{} entries once the busy ones are let go", locks.len());
    }
}
