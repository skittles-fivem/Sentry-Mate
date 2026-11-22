//! Odometer readings an import keeps only when they can be true: at most [`MAX_KM`], and, beside the car's last
//! kept reading, no further than the car could have driven in between ([`MAX_KMH`] and [`SLACK_KM`]). A reading
//! that fails is dropped (the row keeps its other values) and counted, by why, in the report's `skipped_values`.
//! A run of readings that agree with each other but not with the last kept one (the last kept one was the bad one,
//! or the odometer was really changed) is taken from its [`RUN`]th reading on.

/// No car's odometer reads more (a 32-bit "not available" reads 4,294,967 km).
pub const MAX_KM: f64 = 2_000_000.0;
/// Faster than any car keeps up between two readings.
pub const MAX_KMH: f64 = 400.0;
/// Rounding, and readings a few seconds apart.
pub const SLACK_KM: f64 = 2.0;
/// Readings that agree with each other, after which they are taken over the last kept one.
const RUN: u32 = 3;

/// Why a reading was dropped, as the report names it.
pub const PAST_MAX: &str = "odometer: past 2,000,000 km";
pub const JUMP: &str = "odometer: jump no car could drive";
pub const BACKWARDS: &str = "odometer: end before start";

/// A reading on its own: a finite, non-negative distance below [`MAX_KM`].
pub fn plausible(km: f64) -> Result<f64, &'static str> {
    if km.is_finite() && (0.0..=MAX_KM).contains(&km) { Ok(km) } else { Err(PAST_MAX) }
}

fn agree(a: (u64, f64), b: (u64, f64)) -> bool {
    let hours = a.0.abs_diff(b.0) as f64 / 3_600_000.0;
    (a.1 - b.1).abs() <= SLACK_KM + MAX_KMH * hours
}

/// One car's readings, in time order (roughly: a reading is checked against the last kept one either side).
#[derive(Debug, Clone, Default)]
pub struct OdoCheck {
    last: Option<(u64, f64)>,
    /// The last dropped jump, and how many readings agree with it.
    alt: Option<(u64, f64, u32)>,
}

impl OdoCheck {
    /// The reading at `t_ms` if it is kept, else why not.
    pub fn check(&mut self, t_ms: u64, km: f64) -> Result<f64, &'static str> {
        let km = plausible(km)?;
        let now = (t_ms, km);
        match self.last {
            Some(last) if !agree(last, now) => {
                let n = match self.alt {
                    Some((t, k, n)) if agree((t, k), now) => n + 1,
                    _ => 1,
                };
                if n >= RUN {
                    self.last = Some(now);
                    self.alt = None;
                    return Ok(km);
                }
                self.alt = Some((t_ms, km, n));
                Err(JUMP)
            }
            _ => {
                self.last = Some(now);
                self.alt = None;
                Ok(km)
            }
        }
    }

    /// The same for an optional reading: `None` when dropped, with why in `why`.
    pub fn keep(&mut self, t_ms: u64, km: &mut Option<f64>) -> Option<&'static str> {
        let v = (*km)?;
        match self.check(t_ms, v) {
            Ok(_) => None,
            Err(w) => {
                *km = None;
                Some(w)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const MIN: u64 = 60_000;

    #[test]
    fn a_not_available_reading_and_a_jump_are_dropped() {
        let mut c = OdoCheck::default();
        assert_eq!(c.check(0, 41_000.0), Ok(41_000.0));
        assert_eq!(c.check(MIN, 4_294_967.295), Err(PAST_MAX));
        assert_eq!(c.check(2 * MIN, 41_001.0), Ok(41_001.0));
        assert_eq!(c.check(3 * MIN, 91_001.0), Err(JUMP));
        assert_eq!(c.check(4 * MIN, 41_002.0), Ok(41_002.0));
        // a day later, 900 km on: possible
        assert_eq!(c.check(24 * 60 * MIN, 41_902.0), Ok(41_902.0));
    }

    #[test]
    fn a_bad_first_reading_gives_way_to_a_run_that_agrees() {
        let mut c = OdoCheck::default();
        assert_eq!(c.check(0, 900_000.0), Ok(900_000.0));
        assert_eq!(c.check(MIN, 41_000.0), Err(JUMP));
        assert_eq!(c.check(2 * MIN, 41_001.0), Err(JUMP));
        assert_eq!(c.check(3 * MIN, 41_002.0), Ok(41_002.0));
        assert_eq!(c.check(4 * MIN, 41_003.0), Ok(41_003.0));
        assert_eq!(c.check(5 * MIN, 900_000.0), Err(JUMP));
    }
}
