//! What was last sent for recently published cars on this connection, so a car's unchanged values (tire pressures,
//! odometer while parked) aren't sent again with every update. Bounded however many cars there are: two generations
//! of at most half the size each; when the newer fills, the older is dropped whole and the newer becomes the older,
//! so lookups and inserts stay O(1) and the cars used least recently fall out. A car that fell out costs only one
//! full send of its values (retained topics, so sending again is harmless).

use crate::render::Field;
use model::CarId;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

/// Hashes of the payloads last sent to each topic of a car (0: nothing sent yet on this connection).
#[derive(Debug, Clone, Default)]
pub(crate) struct Sent {
    /// Of the car's name and model, when its discovery configs were sent.
    pub discovery: u64,
    pub fields: [u64; Field::COUNT],
}

/// A stable 64-bit hash, never 0 (0 means "not sent").
pub(crate) fn fingerprint(v: &impl Hash) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish().max(1)
}

#[derive(Debug)]
pub(crate) struct SentCache {
    hot: HashMap<CarId, Sent>,
    cold: HashMap<CarId, Sent>,
    half: usize,
}

impl SentCache {
    /// Holds at most `cap` cars (at least 2).
    pub fn new(cap: usize) -> SentCache {
        let half = (cap / 2).max(1);
        SentCache { hot: HashMap::new(), cold: HashMap::new(), half }
    }

    /// The car's record, made empty if it isn't remembered.
    pub fn entry(&mut self, car: CarId) -> &mut Sent {
        if !self.hot.contains_key(&car) {
            let sent = self.cold.remove(&car).unwrap_or_default();
            if self.hot.len() >= self.half {
                self.cold = std::mem::take(&mut self.hot);
            }
            self.hot.insert(car, sent);
        }
        self.hot.entry(car).or_default()
    }

    pub fn forget(&mut self, car: CarId) {
        self.hot.remove(&car);
        self.cold.remove(&car);
    }

    /// A new connection: nothing is known to be on the broker until sent again.
    pub fn clear(&mut self) {
        self.hot = HashMap::new();
        self.cold = HashMap::new();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.hot.len() + self.cold.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_recent_cars() {
        let mut c = SentCache::new(4);
        c.entry(1).fields[0] = 11;
        c.entry(2).fields[0] = 22;
        // hot is full (2): car 3 moves 1 and 2 to cold
        c.entry(3).fields[0] = 33;
        assert_eq!(c.entry(1).fields[0], 11, "found in cold and brought back");
        assert_eq!(c.len(), 3);
        c.entry(4); // hot full again: the cold generation (car 2) is dropped
        c.entry(5);
        assert_eq!(c.entry(2).fields[0], 0, "fell out: sent in full again");
        c.forget(5);
        c.clear();
        assert_eq!(c.len(), 0);
        assert_eq!(c.entry(1).fields[0], 0);
    }

    #[test]
    fn bounded_with_2000_cars() {
        let mut c = SentCache::new(256);
        for round in 0..3u64 {
            for car in 0..2_000 {
                c.entry(car).fields[1] = round + 1;
                assert!(c.len() <= 256, "{}", c.len());
            }
        }
        // the cars used last are still there
        assert_eq!(c.entry(1_999).fields[1], 3);
    }

    #[test]
    fn fingerprints() {
        assert_eq!(fingerprint(&"80.5"), fingerprint(&"80.5"));
        assert_ne!(fingerprint(&"80.5"), fingerprint(&"80.6"));
        assert_ne!(fingerprint(&""), 0);
    }
}
