//! How fast the logs are being worked on now (Data health's "Rebuild speed"): the logs a second over the last
//! half minute or so, and the stage of a log's work that took the most of that time, with its share. From the
//! engine's counters ([`crate::EngineStats`], with a full rebuild's own engine's added: [`Engine::work_stats`]),
//! read each time the speed is asked for and kept a minute.

use crate::{Engine, EngineStats, lock, now_ms};
use serde::Serialize;
use std::collections::VecDeque;

/// The speed now.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RebuildSpeed {
    /// Logs worked on a second, over [`RebuildSpeed::over_ms`].
    pub logs_per_s: f64,
    /// The stage that took the most of the logs' time: `read` (reading and decoding the logs), `samples` (working
    /// out their samples: merged with those around them, held and written at the day's end), `now` (the car's newest values), `windows` (rebuilding the timeline around them) or `other`
    /// (the rest: the logs' records).
    pub stage: &'static str,
    /// Its share of the logs' time, 0 to 100.
    pub stage_share_pct: u32,
    /// The time measured over (ms).
    pub over_ms: u64,
}

/// The counters at one moment.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Snap {
    pub at_ms: u64,
    pub logs: u64,
    /// µs of each stage: read, samples, now, windows, all of a log but its window.
    pub read_us: u64,
    pub samples_us: u64,
    pub now_us: u64,
    pub windows_us: u64,
    pub log_us: u64,
}

impl Snap {
    pub(crate) fn of(s: &EngineStats, at_ms: u64) -> Snap {
        Snap {
            at_ms,
            logs: s.logs_decoded + s.logs_left_out,
            read_us: s.read_us_total,
            samples_us: s.samples_us_total,
            now_us: s.now_us_total,
            windows_us: s.rebuild_ms_total.saturating_mul(1000),
            log_us: s.decode_ms_total.saturating_mul(1000),
        }
    }
}

/// Kept this long.
const KEEP_MS: u64 = 60_000;
/// Measured over at least this long.
const MIN_OVER_MS: u64 = 2_000;
/// And at most about this long (the newest snapshot this old or older is the base).
const OVER_MS: u64 = 30_000;

/// The speed between two readings of the counters (`None`: no log worked on between them, or too short a time).
pub(crate) fn between(a: &Snap, b: &Snap) -> Option<RebuildSpeed> {
    let over_ms = b.at_ms.checked_sub(a.at_ms)?;
    let logs = b.logs.checked_sub(a.logs)?;
    if over_ms < MIN_OVER_MS || logs == 0 {
        return None;
    }
    let d = |x: u64, y: u64| y.saturating_sub(x);
    let (read, samples, now) = (d(a.read_us, b.read_us), d(a.samples_us, b.samples_us), d(a.now_us, b.now_us));
    let windows = d(a.windows_us, b.windows_us);
    let other = d(a.log_us, b.log_us).saturating_sub(read + samples + now);
    let stages = [("read", read), ("samples", samples), ("now", now), ("windows", windows), ("other", other)];
    let total: u64 = stages.iter().map(|s| s.1).sum();
    // (the first of the largest)
    let (stage, most) = stages.iter().fold(("read", 0), |best, s| if s.1 > best.1 { *s } else { best });
    let share = if total > 0 { ((most as f64 * 100.0) / total as f64).round() as u32 } else { 0 };
    Some(RebuildSpeed {
        logs_per_s: logs as f64 * 1000.0 / over_ms as f64,
        stage,
        stage_share_pct: share.min(100),
        over_ms,
    })
}

/// Readings of the counters, the newest last.
#[derive(Debug, Default)]
pub(crate) struct Speeds {
    snaps: VecDeque<Snap>,
}

impl Speeds {
    /// Notes the counters now and answers the speed since about [`OVER_MS`] ago.
    pub(crate) fn note(&mut self, now: Snap) -> Option<RebuildSpeed> {
        // counters that went back (a full rebuild's engine gone): measured from here
        if self.snaps.back().is_some_and(|l| now.logs < l.logs || now.log_us < l.log_us || now.at_ms < l.at_ms) {
            self.snaps.clear();
        }
        while self.snaps.front().is_some_and(|f| now.at_ms.saturating_sub(f.at_ms) > KEEP_MS) {
            self.snaps.pop_front();
        }
        if self.snaps.back().is_none_or(|l| now.at_ms.saturating_sub(l.at_ms) >= 1000) {
            self.snaps.push_back(now);
        }
        let base =
            self.snaps.iter().rev().find(|s| now.at_ms.saturating_sub(s.at_ms) >= OVER_MS).or(self.snaps.front())?;
        between(base, &now)
    }
}

impl Engine {
    /// The speed the logs are worked on now (a rebuild's, a full rebuild's, or new logs'), `None` while none were
    /// in the last [`OVER_MS`] or before two readings a few seconds apart (each ask is a reading).
    pub fn rebuild_speed(&self) -> Option<RebuildSpeed> {
        let now = Snap::of(&self.work_stats(), now_ms());
        lock(&self.inner.speeds).note(now)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn snap(at_s: u64, logs: u64, read_ms: u64, windows_ms: u64, log_ms: u64) -> Snap {
        Snap {
            at_ms: at_s * 1000,
            logs,
            read_us: read_ms * 1000,
            samples_us: 0,
            now_us: 0,
            windows_us: windows_ms * 1000,
            log_us: log_ms * 1000,
        }
    }

    #[test]
    fn the_rate_and_the_slowest_stage_of_the_time_between() {
        let s = between(&snap(0, 10, 100, 1000, 300), &snap(10, 21, 200, 9000, 500)).unwrap();
        assert!((s.logs_per_s - 1.1).abs() < 1e-9, "{s:?}");
        // windows 8,000 ms of 8,000 + 100 read + 100 other
        assert_eq!((s.stage, s.stage_share_pct, s.over_ms), ("windows", 98, 10_000));
        assert_eq!(between(&snap(0, 10, 0, 0, 0), &snap(10, 10, 0, 0, 0)), None, "no logs");
        assert_eq!(between(&snap(0, 10, 0, 0, 0), &snap(1, 12, 0, 0, 0)), None, "too short");
    }

    #[test]
    fn measured_over_the_last_half_minute_and_from_again_when_the_counters_go_back() {
        let mut sp = Speeds::default();
        assert_eq!(sp.note(snap(0, 0, 0, 0, 0)), None);
        // 2 logs a second for 40 s, then 1 a second
        for t in 1..=40 {
            sp.note(snap(t, 2 * t, 0, 0, 10 * t));
        }
        let s = (41..=70).map(|t| sp.note(snap(t, 80 + (t - 40), 0, 0, 10 * t))).last().flatten().unwrap();
        assert!((s.logs_per_s - 1.0).abs() < 1e-9, "{s:?}");
        assert_eq!((s.stage, s.over_ms), ("other", 30_000));
        // a full rebuild's engine gone: its counts no longer added
        assert_eq!(sp.note(snap(71, 5, 0, 0, 1)), None);
        let s = sp.note(snap(75, 9, 40, 0, 50)).unwrap();
        assert!((s.logs_per_s - 1.0).abs() < 1e-9, "{s:?}");
        assert_eq!((s.stage, s.stage_share_pct), ("read", 82));
    }
}
