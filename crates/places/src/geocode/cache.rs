//! The online answers kept, bounded: least recently used out first. Shared by every car (an address is the same
//! whichever car stops there), so its size is set by how many places are visited, never by how many cars there are.
//!
//! The caller persists it ([`super::Nominatim::cache_snapshot`] / [`super::Nominatim::restore_cache`]). It holds
//! positions the cars have been to, so it must be kept sealed like everything else in the data folder.

use super::{Address, RoundedPos};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// One remembered answer. `address: None` means the server found nothing there (at sea, say), which is worth
/// remembering too.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedAddress {
    pub at: RoundedPos,
    pub address: Option<Address>,
    pub fetched_ms: u64,
}

pub(crate) struct Lru {
    cap: usize,
    tick: u64,
    map: HashMap<RoundedPos, (Option<Address>, u64, u64)>,
    order: BTreeMap<u64, RoundedPos>,
}

impl Lru {
    pub(crate) fn new(cap: usize) -> Lru {
        Lru { cap: cap.max(1), tick: 0, map: HashMap::new(), order: BTreeMap::new() }
    }

    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    fn next_tick(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    /// The answer for `k`, marking it as just used.
    pub(crate) fn get(&mut self, k: &RoundedPos) -> Option<Option<Address>> {
        let t = self.next_tick();
        let (a, _, used) = self.map.get_mut(k)?;
        self.order.remove(used);
        *used = t;
        self.order.insert(t, *k);
        Some(a.clone())
    }

    pub(crate) fn contains(&self, k: &RoundedPos) -> bool {
        self.map.contains_key(k)
    }

    pub(crate) fn insert(&mut self, k: RoundedPos, a: Option<Address>, fetched_ms: u64) {
        let t = self.next_tick();
        if let Some((_, _, used)) = self.map.insert(k, (a, fetched_ms, t)) {
            self.order.remove(&used);
        }
        self.order.insert(t, k);
        while self.map.len() > self.cap {
            let Some((_, old)) = self.order.pop_first() else { break };
            self.map.remove(&old);
        }
    }

    /// Everything, least recently used first (so restoring it in order keeps the order).
    pub(crate) fn snapshot(&self) -> Vec<CachedAddress> {
        self.order
            .values()
            .filter_map(|k| {
                self.map.get(k).map(|(a, f, _)| CachedAddress { at: *k, address: a.clone(), fetched_ms: *f })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    fn k(i: i32) -> RoundedPos {
        RoundedPos { lat_e4: i, lon_e4: -i }
    }

    #[test]
    fn keeps_the_most_recently_used() {
        let mut c = Lru::new(3);
        for i in 0..3 {
            c.insert(k(i), None, i as u64);
        }
        assert!(c.get(&k(0)).is_some()); // 0 is now the newest
        c.insert(k(3), None, 3); // pushes out 1
        assert_eq!(c.len(), 3);
        assert!(!c.contains(&k(1)));
        assert!(c.contains(&k(0)) && c.contains(&k(2)) && c.contains(&k(3)));
        let order: Vec<i32> = c.snapshot().iter().map(|e| e.at.lat_e4).collect();
        assert_eq!(order, vec![2, 0, 3]);
        // Replacing an entry doesn't grow it.
        c.insert(k(2), None, 9);
        assert_eq!(c.len(), 3);
        assert_eq!(c.snapshot().last().unwrap().fetched_ms, 9);
    }
}
