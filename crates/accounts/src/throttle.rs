//! Slowing down guessing: failed sign-ins (wrong passwords, codes, setup codes and password confirmations) are counted
//! per address and per account, and after a few the next try has to wait: 30 s, then twice as long each time, up to
//! 15 minutes. A right answer clears the count.
//!
//! - **Per address** stops one machine trying many accounts; **per account** stops many machines (a botnet) trying
//!   one account. The account count is kept apart for the home network and outside it, so guessing from the internet
//!   can't lock the owner out at home. Two-step codes have a count of their own per account, cleared only by a right
//!   code: knowing the password doesn't buy faster guessing of codes.
//! - **Unknown accounts are counted the same way as real ones** (by the name typed), so the answers say nothing about
//!   which accounts exist.
//! - **A try is counted when it starts**, before the slow password check, and cleared if it was right: a burst of
//!   guesses sent in parallel can't all slip past before the first failure is noted.
//! - **Bounded memory**: entries are forgotten a day after their last failure, and at most [`MAX_TRACKED`] are kept
//!   (those not holding anyone back go first when it is full).

use crate::error::{Error, Result};
use std::collections::HashMap;
use std::sync::Mutex;

/// Failures allowed before the waits start (typos happen).
pub const FREE_FAILURES: u32 = 5;
pub const FIRST_WAIT_MS: u64 = 30_000;
pub const MAX_WAIT_MS: u64 = 15 * 60_000;
/// A key with no failure for this long starts again from zero.
pub const FORGET_AFTER_MS: u64 = 24 * 3600 * 1000;
/// At most this many keys are remembered (about 100 bytes each).
pub const MAX_TRACKED: usize = 20_000;

#[derive(Debug, Clone, Copy)]
struct Strikes {
    failures: u32,
    until_ms: u64,
    last_ms: u64,
}

/// The wait after `failures` failures.
pub fn wait_after(failures: u32) -> u64 {
    if failures < FREE_FAILURES {
        return 0;
    }
    let doublings = (failures - FREE_FAILURES).min(16);
    (FIRST_WAIT_MS << doublings).min(MAX_WAIT_MS)
}

pub(crate) struct Throttle {
    map: Mutex<HashMap<String, Strikes>>,
    max: usize,
}

impl Throttle {
    pub(crate) fn new(max: usize) -> Throttle {
        Throttle { map: Mutex::new(HashMap::new()), max: max.max(16) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Strikes>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn wait_ms(map: &HashMap<String, Strikes>, keys: &[String], now: u64) -> u64 {
        keys.iter()
            .filter_map(|k| map.get(k))
            .filter(|s| s.last_ms + FORGET_AFTER_MS > now)
            .map(|s| s.until_ms.saturating_sub(now))
            .max()
            .unwrap_or(0)
    }

    fn refuse(ms: u64) -> Error {
        Error::Throttled { retry_after_s: ms.div_ceil(1000).max(1) }
    }

    /// Whether these keys may try now (nothing is counted).
    pub(crate) fn check(&self, keys: &[String], now: u64) -> Result<()> {
        match Self::wait_ms(&self.lock(), keys, now) {
            0 => Ok(()),
            ms => Err(Self::refuse(ms)),
        }
    }

    /// Starts a try: refused while any key must wait, else counted as a failure on every key until
    /// [`Throttle::success`] clears it.
    pub(crate) fn begin(&self, keys: &[String], now: u64) -> Result<()> {
        let mut map = self.lock();
        match Self::wait_ms(&map, keys, now) {
            0 => {}
            ms => return Err(Self::refuse(ms)),
        }
        for k in keys {
            if !map.contains_key(k) && map.len() >= self.max {
                Self::make_room(&mut map, self.max, now);
            }
            let s = map.entry(k.clone()).or_insert(Strikes { failures: 0, until_ms: 0, last_ms: now });
            if s.last_ms + FORGET_AFTER_MS <= now {
                s.failures = 0;
            }
            s.failures = s.failures.saturating_add(1);
            s.last_ms = now;
            s.until_ms = now + wait_after(s.failures);
        }
        Ok(())
    }

    /// The try was right: the keys start again from zero.
    pub(crate) fn success(&self, keys: &[String]) {
        let mut map = self.lock();
        for k in keys {
            map.remove(k);
        }
    }

    /// Drops what is no longer needed, down to 90 % of the limit: forgotten keys first, then those not holding
    /// anyone back, then those whose wait ends soonest.
    fn make_room(map: &mut HashMap<String, Strikes>, max: usize, now: u64) {
        map.retain(|_, s| s.last_ms + FORGET_AFTER_MS > now);
        let target = max - max / 10;
        if map.len() > target {
            map.retain(|_, s| s.until_ms > now);
        }
        if map.len() > target {
            let mut by_end: Vec<(u64, String)> = map.iter().map(|(k, s)| (s.until_ms, k.clone())).collect();
            by_end.sort_unstable();
            for (_, k) in by_end.into_iter().take(map.len() - target) {
                map.remove(&k);
            }
        }
    }

    /// Forgets keys whose last failure was a day ago.
    pub(crate) fn sweep(&self, now: u64) {
        self.lock().retain(|_, s| s.last_ms + FORGET_AFTER_MS > now);
    }

    /// How many keys are remembered now.
    pub(crate) fn tracked(&self) -> usize {
        self.lock().len()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    fn keys(k: &str) -> Vec<String> {
        vec![k.to_string()]
    }

    #[test]
    fn waits_grow_from_30_s_to_15_min() {
        assert_eq!((0..5).map(wait_after).collect::<Vec<_>>(), [0, 0, 0, 0, 0]);
        let waits: Vec<u64> = (5..12).map(|n| wait_after(n) / 1000).collect();
        assert_eq!(waits, [30, 60, 120, 240, 480, 900, 900]);
        assert_eq!(wait_after(u32::MAX), MAX_WAIT_MS);
    }

    #[test]
    fn tries_are_counted_up_front_and_cleared_by_success() {
        let t = Throttle::new(100);
        let k = keys("ip:192.0.2.1");
        let now = 1_000_000;
        for _ in 0..FREE_FAILURES {
            t.begin(&k, now).unwrap();
        }
        assert!(matches!(t.begin(&k, now), Err(Error::Throttled { retry_after_s: 30 })));
        assert!(matches!(t.check(&k, now + 29_000), Err(Error::Throttled { retry_after_s: 1 })));
        t.begin(&k, now + 30_000).unwrap();
        assert!(matches!(t.check(&k, now + 30_000), Err(Error::Throttled { retry_after_s: 60 })));
        t.success(&k);
        t.check(&k, now + 30_000).unwrap();
        // any key holding back is enough
        let both = vec!["a".to_string(), "b".to_string()];
        for _ in 0..FREE_FAILURES {
            t.begin(&keys("b"), now).unwrap();
        }
        assert!(t.check(&both, now).is_err());
        assert!(t.check(&keys("a"), now).is_ok());
    }

    #[test]
    fn counts_are_forgotten_after_a_day() {
        let t = Throttle::new(100);
        let k = keys("x");
        for _ in 0..FREE_FAILURES + 3 {
            let _ = t.begin(&k, 0);
        }
        assert!(t.check(&k, 0).is_err());
        let later = FORGET_AFTER_MS + 1;
        t.check(&k, later).unwrap();
        t.begin(&k, later).unwrap();
        t.check(&k, later).unwrap();
        t.sweep(later + FORGET_AFTER_MS + 1);
        assert_eq!(t.tracked(), 0);
    }

    #[test]
    fn memory_stays_bounded_and_a_flood_does_not_free_who_is_held_back() {
        let t = Throttle::new(1000);
        let held = keys("ip:203.0.113.9");
        for _ in 0..FREE_FAILURES + 4 {
            let _ = t.begin(&held, 0);
        }
        assert!(t.check(&held, 1).is_err());
        // 50,000 names tried once each (none held back)
        for i in 0..50_000u32 {
            t.begin(&keys(&format!("account:{i}")), 1).unwrap();
            assert!(t.tracked() <= 1000);
        }
        assert!(t.check(&held, 2).is_err(), "still held back");
    }
}
