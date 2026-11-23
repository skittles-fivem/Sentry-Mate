//! A car's odometer as one clean series, for every figure read from it (`/statistics`, `/mileage`, `/totals`,
//! `/compare?metric=odometer`).
//!
//! The readings are the shown drives' start and end odometers, oldest first. A reading is kept only where it can be
//! true beside the car's other readings: at most [`MAX_KM`] (a 32-bit "not available" reads 4,294,967 km), never
//! below an earlier kept one, and no further on than the car could have driven in between. Readings that agree with
//! each other form runs; the chain of runs that agree with each other and hold the most readings is kept, the rest
//! dropped. So a stretch of another car's rows, a distance read in the wrong unit or a single bad value is left out
//! however early it comes (the first reading of a period no longer decides the period's distance). The series is
//! kept per car while its data stays the same ([`crate::memo`]).

use super::drives::each;
use super::system::Shown;
use super::{QResult, SourceFilter};
use crate::Engine;
use model::{CarId, Drive};
use std::sync::Arc;

/// No car's odometer reads more.
pub(crate) const MAX_KM: f64 = 2_000_000.0;
/// Faster than any car keeps up between two readings.
const MAX_KMH: f64 = 400.0;
/// Rounding, and readings a few seconds apart.
const SLACK_KM: f64 = 2.0;
/// How many runs back a run looks for the one it follows (the chain of a sane history breaks rarely).
const LOOK_BACK: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Reading {
    t: u64,
    km: f64,
    /// The drive it is of (its index in reading order): readings of one drive are interpolated between.
    drive: usize,
}

/// The kept readings, oldest first and never going down, and when each dropped one was.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Odometer {
    kept: Vec<Reading>,
    dropped: Vec<u64>,
}

fn agree(a: &Reading, b: &Reading) -> bool {
    let hours = b.t.saturating_sub(a.t) as f64 / 3_600_000.0;
    b.km >= a.km - SLACK_KM && b.km - a.km <= SLACK_KM + MAX_KMH * hours
}

impl Odometer {
    /// The series of `readings` (any order).
    fn clean(mut all: Vec<Reading>) -> Odometer {
        all.sort_by(|a, b| a.t.cmp(&b.t).then(a.drive.cmp(&b.drive)));
        let mut dropped = Vec::new();
        let mut ok = Vec::with_capacity(all.len());
        for x in all {
            if x.km.is_finite() && (0.0..=MAX_KM).contains(&x.km) {
                ok.push(x);
            } else {
                dropped.push(x.t);
            }
        }
        // runs of readings that agree with the one before
        let mut runs: Vec<(usize, usize)> = Vec::new();
        for i in 0..ok.len() {
            match runs.last_mut() {
                Some(r) if agree(&ok[r.1 - 1], &ok[i]) => r.1 = i + 1,
                _ => runs.push((i, i + 1)),
            }
        }
        // the chain of runs holding the most readings, each run agreeing with the one it follows
        let mut best: Vec<(usize, Option<usize>)> = Vec::with_capacity(runs.len());
        for (i, r) in runs.iter().enumerate() {
            let mut b = (r.1 - r.0, None);
            for j in (i.saturating_sub(LOOK_BACK)..i).rev() {
                let n = best[j].0 + (r.1 - r.0);
                if n > b.0 && agree(&ok[runs[j].1 - 1], &ok[r.0]) {
                    b = (n, Some(j));
                }
            }
            best.push(b);
        }
        let mut chosen = vec![false; runs.len()];
        // the first of the best (an earlier chain wins a tie: it came first)
        let mut at = best.iter().enumerate().fold(None, |m: Option<usize>, (i, b)| match m {
            Some(k) if best[k].0 >= b.0 => Some(k),
            _ => Some(i),
        });
        while let Some(i) = at {
            chosen[i] = true;
            at = best[i].1;
        }
        let mut kept: Vec<Reading> = Vec::with_capacity(ok.len());
        for (r, on) in runs.iter().zip(chosen) {
            for x in &ok[r.0..r.1] {
                if on {
                    // within the slack a reading may dip a little: the series never goes down
                    let km = kept.last().map_or(x.km, |p| p.km.max(x.km));
                    kept.push(Reading { km, ..*x });
                } else {
                    dropped.push(x.t);
                }
            }
        }
        dropped.sort_unstable();
        Odometer { kept, dropped }
    }

    /// The odometer at `t`: the newest kept reading by then (in step with the drive's distance across a drive going
    /// on at `t`); before the first, the first.
    pub(crate) fn at(&self, t: u64) -> Option<f64> {
        let i = self.kept.partition_point(|x| x.t <= t);
        if i == 0 {
            return self.kept.first().map(|x| x.km);
        }
        let a = &self.kept[i - 1];
        match self.kept.get(i) {
            Some(b) if b.drive == a.drive && b.t > a.t => {
                let part = (t - a.t) as f64 / (b.t - a.t) as f64;
                Some(a.km + (b.km - a.km) * part.clamp(0.0, 1.0))
            }
            _ => Some(a.km),
        }
    }

    /// The first and the last kept reading, with when the last was.
    pub(crate) fn first(&self) -> Option<f64> {
        self.kept.first().map(|x| x.km)
    }

    pub(crate) fn last(&self) -> Option<(f64, u64)> {
        self.kept.last().map(|x| (x.km, x.t))
    }

    /// The newest kept reading in `[a, b)`.
    pub(crate) fn last_in(&self, a: u64, b: u64) -> Option<f64> {
        let i = self.kept.partition_point(|x| x.t < b);
        self.kept[..i].last().filter(|x| x.t >= a).map(|x| x.km)
    }

    /// How many readings in `[a, b)` were left out.
    pub(crate) fn dropped_in(&self, a: u64, b: u64) -> u64 {
        (self.dropped.partition_point(|&t| t < b) - self.dropped.partition_point(|&t| t < a)) as u64
    }
}

/// The car's odometer series as `source` shows its drives (null: as the lists show them by default).
pub(crate) fn of(e: &Engine, car: CarId, source: Option<SourceFilter>) -> QResult<Arc<Odometer>> {
    let key = crate::memo::key(&format!("{source:?}"));
    e.memo(car, "odometer", crate::memo::On::All, key, || {
        let shown = Shown::new(e, car, source)?;
        let mut all = Vec::new();
        let mut n = 0usize;
        each::<Drive>(e, car, 0, u64::MAX, source, &shown, false, |d| {
            for (t, km) in [(d.start_ms, d.start_odometer_km), (d.end_ms.max(d.start_ms), d.end_odometer_km)] {
                if let Some(km) = km {
                    all.push(Reading { t, km, drive: n });
                }
            }
            n += 1;
            Ok(true)
        })?;
        Ok(Odometer::clean(all))
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const H: u64 = 3_600_000;
    const DAY: u64 = 24 * H;
    const MI: f64 = 1.609_344;

    /// One drive a day of `km` from `from` km, starting at day `d0`, as (start, end) readings.
    fn days(d0: u64, n: u64, from: f64, km: f64, drive0: usize) -> Vec<Reading> {
        (0..n)
            .flat_map(|i| {
                let t = (d0 + i) * DAY + 8 * H;
                let o = from + i as f64 * km;
                let drive = drive0 + i as usize;
                [Reading { t, km: o, drive }, Reading { t: t + H, km: o + km, drive }]
            })
            .collect()
    }

    #[test]
    fn a_month_of_another_cars_readings_is_left_out() {
        // January: a sold car's rows at 145,600 km (90,470 mi); from February the car itself from 53,100 km
        let mut all = days(0, 31, 90_470.0 * MI, 50.0, 0);
        all.extend(days(31, 300, 53_100.0, 50.0, 31));
        let o = Odometer::clean(all);
        assert_eq!(o.first(), Some(53_100.0));
        assert_eq!(o.dropped_in(0, 31 * DAY), 62, "every reading of the other car");
        // the year's distance by odometer: last minus first kept reading, not zero
        let (a, b) = (o.at(0).unwrap(), o.at(331 * DAY).unwrap());
        assert!((b - a - 300.0 * 50.0).abs() < 1e-6, "{a} {b}");
        assert_eq!(o.last_in(0, 31 * DAY), None, "January has no reading of this car");
    }

    #[test]
    fn a_not_available_value_and_a_value_in_the_wrong_unit_are_dropped() {
        let mut all = days(0, 10, 41_000.0, 30.0, 0);
        all.push(Reading { t: 3 * DAY + 12 * H, km: 4_294_967.295, drive: 100 });
        // a reading in miles stored as km, in the middle
        all.push(Reading { t: 5 * DAY + 12 * H, km: 41_150.0 / MI, drive: 101 });
        let o = Odometer::clean(all);
        assert_eq!(o.dropped_in(0, 10 * DAY), 2);
        assert_eq!(o.first(), Some(41_000.0));
        assert_eq!(o.last().map(|l| l.0), Some(41_300.0));
        let kept: Vec<f64> = o.kept.iter().map(|x| x.km).collect();
        assert!(kept.windows(2).all(|w| w[1] >= w[0]), "monotonic");
    }

    #[test]
    fn small_dips_are_kept_flat_and_a_drive_is_interpolated() {
        let all = vec![
            Reading { t: 0, km: 100.0, drive: 0 },
            Reading { t: H, km: 150.0, drive: 0 },
            Reading { t: 2 * H, km: 149.9, drive: 1 },
            Reading { t: 3 * H, km: 160.0, drive: 1 },
        ];
        let o = Odometer::clean(all);
        assert_eq!(o.dropped_in(0, u64::MAX), 0);
        assert_eq!(o.at(H / 2), Some(125.0));
        assert_eq!(o.at(2 * H), Some(150.0), "never down");
        assert_eq!(o.at(10 * H), Some(160.0));
        assert_eq!(Odometer::clean(Vec::new()).at(5), None);
    }

    #[test]
    fn a_long_gap_with_a_believable_distance_is_kept() {
        let mut all = days(0, 5, 10_000.0, 20.0, 0);
        // thirty days later, 3,000 km on: possible
        all.extend(days(35, 5, 13_100.0, 20.0, 5));
        let o = Odometer::clean(all);
        assert_eq!(o.dropped_in(0, u64::MAX), 0);
    }
}
