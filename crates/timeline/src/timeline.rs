//! The timeline of one car over a window: a pure, deterministic function of the window's samples and log facts.
//!
//! First the states ([`crate::states`]); the settled spans among them ([`is_settled`]) cut the window into pieces that
//! are worked out each on its own (drives, charges, positions). So a window that starts and ends in a settled span
//! gives exactly what the same stretch gives inside a longer window, and rebuilding one window after a late log
//! changes nothing outside it ([`crate::window`]).
//!
//! A car that stays awake for days (Sentry mode, camping, a long slow charge done) has no settled span, so the pieces
//! are cut again at **idle cut points** ([`Timeline::cuts`]): moments on a fixed grid where the car has been online,
//! parked and not charging for a while on both sides, so no drive or charge can go on across them. They are found
//! from the samples alone, and only where the samples around them are all in the window, so a window from one cut
//! point to another gives the same cut points (and everything else) as a longer one.

use crate::charges;
use crate::drives;
use crate::facts::LogFacts;
use crate::states::{is_settled, states, with_api};
use model::{CarId, Charge, ChargeId, ChargePoint, Drive, DriveId, Position, Sample, StateSpan};
use serde::{Deserialize, Serialize};

/// The version of these rules: drives and charges made by an older one are made again.
/// 2: a fix is one input's latitude and longitude, and fixes that cannot be real (a placeholder, or a jump faster
/// than [`crate::MAX_FIX_KMH`] from the fixes around it) are left out, so no drive starts or ends where the car
/// never was.
/// 3: a charge's energy added and grid energy are measured over one span, or say they are not
/// ([`model::Charge::energy_spans`]).
pub const RULES_VERSION: u32 = 3;

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
    /// Where the samples come from the Tesla API alone (one every few seconds to a minute): a drive ends once the car
    /// has been parked this long (ms), and a sample counts across a gap this long at most in sums (ms).
    #[serde(default = "api_park_ms")]
    pub api_park_ms: u64,
    #[serde(default = "api_max_hold_ms")]
    pub api_max_hold_ms: u64,
}

fn api_park_ms() -> u64 {
    150_000
}

fn api_max_hold_ms() -> u64 {
    90_000
}

impl Params {
    /// The thresholds for rows with seconds from the Tesla API alone.
    pub fn for_api(&self) -> Params {
        Params {
            park_ms: self.park_ms.max(self.api_park_ms),
            max_hold_ms: self.max_hold_ms.max(self.api_max_hold_ms),
            ..*self
        }
    }
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
            api_park_ms: api_park_ms(),
            api_max_hold_ms: api_max_hold_ms(),
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
    /// Every log that overlaps the window, the last one before it and the first one after it, any order: of the one
    /// source the car's timeline uses there (its preferred CAN key, when two record at once). (The one before says
    /// whether a sleep began before the window; the one after whether the car stays online across the window's end,
    /// when that end is an idle cut point.)
    pub logs: &'a [LogFacts],
    /// Spans known to have no usable data (a log that failed its check, a gap in the chain): unknown, never asleep.
    pub holes: &'a [(u64, u64)],
    /// What the Tesla API saw of the car's state ([`crate::ApiSpan`]), any order: fills what SMO does not know.
    pub api: &'a [crate::ApiSpan],
    /// The seconds of `samples` that are not SMO's alone ([`crate::Fused::marks`], sorted): the rows made from them
    /// name the Tesla API. Empty: every second is SMO's.
    pub marks: &'a [(u64, model::Inputs)],
    /// The order the car's live inputs win in (its `source_order`): of two APIs that saw the car's state the same,
    /// the higher one is named.
    pub order: model::SourceOrder,
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
    /// Also split at the idle cut points: adjacent spans with the same state and buses are one stretch (a page
    /// showing them joins them).
    pub states: Vec<StateSpan>,
    pub drives: Vec<Drive>,
    pub charges: Vec<Charge>,
    /// Every second while driving, one per [`Params::parked_position_every_ms`] while parked.
    pub positions: Vec<Position>,
    /// Every second of each charge.
    pub charge_points: Vec<(ChargeId, ChargePoint)>,
    pub losses: Vec<Loss>,
    /// The idle cut points of the window (ms, sorted): the states are split at them, and nothing is carried across
    /// one. The caller keeps them with the states and gives them back to [`crate::rebuild_window`], so a car that
    /// never sleeps still gets small windows.
    #[serde(default)]
    pub cuts: Vec<u64>,
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

/// Read just before an idle cut point: a sample this close before it shows the car was there and parked (ms).
const CUT_LEAD_MS: u64 = 10_000;

/// How long the car must be parked and not charging on each side of an idle cut point (ms): long enough that a drive
/// before it has been back in P for [`Params::park_ms`] (seen by a sample within [`CUT_LEAD_MS`] before the point),
/// that a charge before and one after are more than [`Params::charge_gap_ms`] apart, and that no value is held across
/// it ([`Params::max_hold_ms`]). 91 s with the defaults.
pub(crate) fn cut_margin_ms(params: &Params) -> u64 {
    params
        .park_ms
        .saturating_add(1000 + CUT_LEAD_MS)
        .max((params.charge_gap_ms / 2).saturating_add(1000))
        .max(params.max_hold_ms)
}

/// The grid of idle cut points: a multiple of [`Params::parked_position_every_ms`] (so a cut never changes which parked
/// positions are kept), and at least two margins wide (so the samples one cut point looks at never reach another).
/// Five minutes with the defaults.
pub(crate) fn cut_step_ms(params: &Params) -> u64 {
    let every = params.parked_position_every_ms.max(1);
    let need = cut_margin_ms(params).saturating_mul(2).saturating_add(1);
    every.saturating_mul(need.div_ceil(every).max(1))
}

/// The idle cut points of one piece `[a, b)` (between settled spans) with its sorted samples: grid points `c` with
/// `[c - margin, c + margin)` inside the piece, online at `c`, no sample in there out of P or charging, and a sample
/// within [`CUT_LEAD_MS`] before `c`. Only samples inside the piece are looked at, so the same piece (or the part of it
/// between two cut points) always gives the same cut points.
fn idle_cuts(seg: &[&Sample], a: u64, b: u64, states: &[StateSpan], params: &Params) -> Vec<u64> {
    let (margin, step) = (cut_margin_ms(params), cut_step_ms(params));
    let mut out = Vec::new();
    let Some(first) = a.checked_add(margin) else { return out };
    let mut c = first.div_ceil(step).saturating_mul(step);
    while c.saturating_add(margin) <= b && c < u64::MAX {
        let lo = seg.partition_point(|s| s.t_ms < c - margin);
        let hi = seg.partition_point(|s| s.t_ms < c.saturating_add(margin));
        let around = &seg[lo..hi];
        let idle = around.iter().all(|s| s.charging != Some(true) && !crate::drives::moving(s));
        let seen = around.iter().any(|s| s.t_ms >= c.saturating_sub(CUT_LEAD_MS) && s.t_ms < c);
        let online = {
            let i = states.partition_point(|s| s.end_ms <= c);
            states.get(i).is_some_and(|s| s.start_ms <= c && s.state == model::State::Online)
        };
        if idle && seen && online {
            out.push(c);
        }
        // (no samples ahead: jump to the grid point before the next one)
        let next = seg.get(hi).map_or(b, |s| s.t_ms);
        let skip = next.saturating_sub(margin.saturating_add(CUT_LEAD_MS)) / step * step;
        c = c.saturating_add(step).max(skip);
    }
    out
}

/// The spans split at the cut points inside them.
fn split_at(spans: Vec<StateSpan>, cuts: &[u64]) -> Vec<StateSpan> {
    if cuts.is_empty() {
        return spans;
    }
    let mut out = Vec::with_capacity(spans.len() + cuts.len());
    for s in spans {
        let lo = cuts.partition_point(|&c| c <= s.start_ms);
        let hi = cuts.partition_point(|&c| c < s.end_ms);
        let mut start = s.start_ms;
        for &c in &cuts[lo..hi] {
            out.push(StateSpan { start_ms: start, end_ms: c, ..s.clone() });
            start = c;
        }
        out.push(StateSpan { start_ms: start, ..s });
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
    let smo = states(car, from_ms, to_ms, &logs, input.holes, params);
    // a window with only the API's data names the API on the unknown spans it leaves
    let api_only =
        input.logs.is_empty() && input.holes.is_empty() && (!input.api.is_empty() || !input.marks.is_empty());
    let unknown_inputs = if api_only {
        // the APIs that gave the window's data (the Tesla API when none is named)
        let by = input
            .api
            .iter()
            .map(crate::ApiSpan::input)
            .chain(input.marks.iter().map(|m| m.1.without_smo()))
            .fold(model::Inputs::NONE, model::Inputs::union);
        if by.is_empty() { model::Inputs::TESLA_API } else { by }
    } else {
        model::Inputs::SMO
    };
    let mut t = Timeline { states: with_api(smo, input.api, unknown_inputs, &input.order), ..Timeline::default() };
    let samples = sorted(input.samples, from_ms, to_ms);

    // the pieces between settled spans, cut again at their idle cut points
    let mut parts = Vec::new();
    for (a, b) in pieces(&t.states, from_ms, to_ms, params) {
        let lo = samples.partition_point(|s| s.t_ms < a);
        let hi = samples.partition_point(|s| s.t_ms < b);
        let cuts = idle_cuts(&samples[lo..hi], a, b, &t.states, params);
        let mut start = a;
        for &c in &cuts {
            parts.push((start, c));
            start = c;
        }
        parts.push((start, b));
        t.cuts.extend(cuts);
    }
    t.states = split_at(std::mem::take(&mut t.states), &t.cuts);

    for (a, b) in parts {
        let lo = samples.partition_point(|s| s.t_ms < a);
        let hi = samples.partition_point(|s| s.t_ms < b);
        let seg = &samples[lo..hi];
        if seg.is_empty() {
            continue;
        }
        let marks = input.marks;
        let api: Vec<bool> = if marks.is_empty() {
            Vec::new()
        } else {
            seg.iter().map(|s| !crate::fuse::inputs_at(marks, s.t_ms).contains(model::Input::Smo)).collect()
        };
        // the thresholds of rows with seconds from the API alone, and the inputs of a row's seconds
        let params_of = |i: usize, j: usize| if api[i..=j].contains(&true) { params.for_api() } else { *params };
        let params_of = |i: usize, j: usize| if api.is_empty() { *params } else { params_of(i, j) };
        let inputs_of = |i: usize, j: usize| -> model::Inputs {
            if marks.is_empty() {
                return model::Inputs::SMO;
            }
            seg[i..=j].iter().fold(model::Inputs::NONE, |acc, s| acc.union(crate::fuse::inputs_at(marks, s.t_ms)))
        };
        let found = drives::find(seg, &api, params);
        let kept: Vec<(usize, usize)> = found
            .iter()
            .copied()
            .filter(|&(i, j)| {
                match drives::make(car, &seg[i..=j], i.checked_sub(1).map(|k| seg[k]), &params_of(i, j)) {
                    Some(mut d) => {
                        d.inputs = inputs_of(i, j);
                        t.drives.push(d);
                        true
                    }
                    None => false,
                }
            })
            .collect();
        for (i, j) in charges::find(seg, &found, params) {
            // where the car is: samples since the last drive that ended before the charge
            let since = found.iter().filter(|&&(_, e)| e < i).map(|&(_, e)| e + 1).max().unwrap_or(0);
            if let Some(mut c) = charges::make(car, &seg[i..=j], &seg[since..i], &params_of(i, j)) {
                c.inputs = inputs_of(i, j);
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
