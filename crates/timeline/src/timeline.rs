//! The timeline of one car over a window: a pure, deterministic function of the window's samples and log facts.
//!
//! First the states ([`crate::states`]); the settled spans among them ([`is_settled`]) cut the window into pieces that
//! are worked out each on its own (drives, charges, positions). So a window that starts and ends in a settled span
//! gives exactly what the same stretch gives inside a longer window, and rebuilding one window after a late log
//! changes nothing outside it ([`crate::window`]).

use crate::charges;
use crate::drives;
use crate::facts::LogFacts;
use crate::states::{is_settled, states};
use model::{CarId, Charge, ChargeId, ChargePoint, Drive, DriveId, Position, Sample, StateSpan};
use serde::{Deserialize, Serialize};

/// The version of these rules: drives and charges made by an older one are made again.
pub const RULES_VERSION: u32 = 1;

/// A drive's id: the car in the high 32 bits, its start (s since 1970) in the low ones. The same drive rebuilt keeps
/// its id, and no two cars share one. (As a JSON number it is exact while car ids stay under 2^21; past that the API
/// should send ids as strings.)
pub fn drive_id(car: CarId, start_ms: u64) -> DriveId {
    (u64::from(car) << 32) | ((start_ms / 1000) & 0xFFFF_FFFF)
}

/// A charge's id, made as [`drive_id`].
pub fn charge_id(car: CarId, start_ms: u64) -> ChargeId {
    drive_id(car, start_ms)
}

/// The thresholds, with their defaults.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Params {
    /// A drive ends once the gear has been back in P this long (ms).
    pub park_ms: u64,
    /// Drives shorter than this, or that moved less than `min_drive_km`, are left out.
    pub min_drive_ms: u64,
    pub min_drive_km: f64,
    /// Charging that stops for less than this and starts again is one charge (ms).
    pub charge_gap_ms: u64,
    /// One position kept per this long while parked (ms).
    pub parked_position_every_ms: u64,
    /// An unknown span this long (or any asleep span) is a settled point: nothing goes on across it (ms).
    pub settle_ms: u64,
    /// Logs whose frames are this close are one online span (ms).
    pub join_ms: u64,
    /// In energy sums, a sample counts across a gap this long at most (ms).
    pub max_hold_ms: u64,
    /// GPS steps faster than this are a bad fix (km/h).
    pub max_gps_kmh: f64,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            park_ms: 60_000,
            min_drive_ms: 30_000,
            min_drive_km: 0.05,
            charge_gap_ms: 180_000,
            parked_position_every_ms: 300_000,
            settle_ms: 600_000,
            join_ms: 5_000,
            max_hold_ms: 10_000,
            max_gps_kmh: 300.0,
        }
    }
}

/// What a window's timeline is made from.
#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    pub car: CarId,
    /// The window, `[from_ms, to_ms)`: from a settled point to a settled point (or to now).
    pub from_ms: u64,
    pub to_ms: u64,
    /// The car's samples in the window, any order (one per second: of two with the same second, the later is kept).
    pub samples: &'a [Sample],
    /// Every log that overlaps the window, and the last one before it, any order: of the one source the car's
    /// timeline uses there (its preferred CAN key, when two record at once).
    pub logs: &'a [LogFacts],
    /// Spans known to have no usable data (a log that failed its check, a gap in the chain): unknown, never asleep.
    pub holes: &'a [(u64, u64)],
}

/// Frames lost in a log (`# dropped` lines), for marking what overlaps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loss {
    pub start_ms: u64,
    pub end_ms: u64,
    pub dropped: u32,
}

/// A window's history, each list sorted by time.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Timeline {
    pub states: Vec<StateSpan>,
    pub drives: Vec<Drive>,
    pub charges: Vec<Charge>,
    /// Every second while driving, one per [`Params::parked_position_every_ms`] while parked.
    pub positions: Vec<Position>,
    /// Every second of each charge.
    pub charge_points: Vec<(ChargeId, ChargePoint)>,
    pub losses: Vec<Loss>,
}

impl Timeline {
    pub fn drive(&self, id: DriveId) -> Option<&Drive> {
        self.drives.iter().find(|d| d.id == id)
    }
}

/// The samples of the window by time, one per second.
fn sorted(samples: &[Sample], from_ms: u64, to_ms: u64) -> Vec<&Sample> {
    let mut v: Vec<&Sample> = samples.iter().filter(|s| s.t_ms >= from_ms && s.t_ms < to_ms).collect();
    v.sort_by_key(|s| s.t_ms); // stable: equal seconds keep their input order
    let mut out: Vec<&Sample> = Vec::with_capacity(v.len());
    for s in v {
        match out.last_mut() {
            Some(last) if last.t_ms == s.t_ms => *last = s,
            _ => out.push(s),
        }
    }
    out
}

/// The pieces between settled spans, `[start, end)`.
fn pieces(spans: &[StateSpan], from_ms: u64, to_ms: u64, params: &Params) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut cur = from_ms;
    for s in spans.iter().filter(|s| is_settled(s, params)) {
        if s.start_ms > cur {
            out.push((cur, s.start_ms));
        }
        cur = cur.max(s.end_ms);
    }
    if to_ms > cur {
        out.push((cur, to_ms));
    }
    out
}

/// The timeline of one car over one window. Pure and deterministic: the same input gives the same output, ids
/// included; its work and memory are bounded by the window. Samples inside a settled span (which only input that
/// contradicts itself has: frames in a recording pause, data in a hole) are left out.
pub fn build(input: &Input<'_>, params: &Params) -> Timeline {
    let (car, from_ms, to_ms) = (input.car, input.from_ms, input.to_ms);
    let logs: Vec<&LogFacts> = input.logs.iter().collect();
    let mut t = Timeline { states: states(car, from_ms, to_ms, &logs, input.holes, params), ..Timeline::default() };
    let samples = sorted(input.samples, from_ms, to_ms);

    for (a, b) in pieces(&t.states, from_ms, to_ms, params) {
        let lo = samples.partition_point(|s| s.t_ms < a);
        let hi = samples.partition_point(|s| s.t_ms < b);
        let seg = &samples[lo..hi];
        if seg.is_empty() {
            continue;
        }
        let found = drives::find(seg, params);
        let kept: Vec<(usize, usize)> = found
            .iter()
            .copied()
            .filter(|&(i, j)| match drives::make(car, &seg[i..=j], i.checked_sub(1).map(|k| seg[k]), params) {
                Some(d) => {
                    t.drives.push(d);
                    true
                }
                None => false,
            })
            .collect();
        for (i, j) in charges::find(seg, &found, params) {
            // where the car is: samples since the last drive that ended before the charge
            let since = found.iter().filter(|&&(_, e)| e < i).map(|&(_, e)| e + 1).max().unwrap_or(0);
            if let Some(c) = charges::make(car, &seg[i..=j], &seg[since..i], params) {
                t.charge_points.extend(charges::points(&seg[i..=j]).into_iter().map(|p| (c.id, p)));
                t.charges.push(c);
            }
        }
        positions(seg, &kept, params, &mut t.positions);
    }
    t.losses = input
        .logs
        .iter()
        .filter(|l| l.dropped > 0)
        .map(|l| {
            let (start_ms, end_ms) = l.span();
            Loss { start_ms, end_ms, dropped: l.dropped }
        })
        .filter(|l| l.end_ms > from_ms && l.start_ms < to_ms)
        .collect();
    t.losses.sort_by_key(|l| (l.start_ms, l.end_ms));
    t
}

/// Every second of a drive, and while parked the first fix of each [`Params::parked_position_every_ms`] (counted from
/// 1970, so the same in any window; a drive starts the count again where it ends).
fn positions(seg: &[&Sample], drives: &[(usize, usize)], params: &Params, out: &mut Vec<Position>) {
    let every = params.parked_position_every_ms.max(1);
    let mut bucket: Option<u64> = None;
    let mut d = drives.iter().peekable();
    for (i, s) in seg.iter().enumerate() {
        while d.peek().is_some_and(|&&(_, e)| e < i) {
            d.next();
            bucket = None;
        }
        let driving = d.peek().is_some_and(|&&(a, _)| a <= i);
        let Some(p) = crate::calc::position(s) else { continue };
        if driving {
            out.push(p);
        } else if bucket != Some(s.t_ms / every) {
            bucket = Some(s.t_ms / every);
            out.push(p);
        }
    }
}
