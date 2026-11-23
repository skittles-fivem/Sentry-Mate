//! Drives, charges and state spans worked out from the polls, as aggregates that can be joined: a file ends (a
//! month) while the car drives, charges or sleeps, so what runs over a file's edge is kept as an edge of that file's
//! piece and joined with the next file's when both are known, in this import or a later one.

use super::poll::{CarState, Poll};
use crate::rows::MAPPING_VERSION;
use model::{Charge, ChargeKind, Drive, Position, Source, State, StateSpan};
use serde::{Deserialize, Serialize};

/// A drive ends after this long without a poll.
pub(crate) const DRIVE_GAP_MS: u64 = 15 * 60_000;
/// A drive ends after this long standing still in gear.
pub(crate) const STANDING_MS: u64 = 10 * 60_000;
/// A charge ends after this long without a poll (unless its energy carries on: then it is the same session).
pub(crate) const CHARGE_GAP_MS: u64 = 6 * 3_600_000;
/// An online span ends after this long without a poll (TeslaFi polls an idle car less often while it lets it fall
/// asleep); an asleep one lasts until the car is seen again.
pub(crate) const ONLINE_GAP_MS: u64 = 60 * 60_000;
/// Power is integrated only over gaps up to this.
const INTEGRATE_GAP_MS: u64 = 5 * 60_000;
/// Polls standing still in gear held while it is not known yet whether the drive goes on.
const MAX_STANDING_POLLS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Drive,
    Charge,
    State,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Drive, Kind::Charge, Kind::State];
}

/// A poll reduced to what a segment's ends keep.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snap {
    pub t: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elev: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub odo: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soc: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpower: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out_t: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_t: Option<f64>,
}

impl Snap {
    pub(crate) fn of(p: &Poll) -> Snap {
        Snap {
            t: p.t_ms,
            lat: p.lat,
            lon: p.lon,
            elev: p.elevation_m,
            speed: p.speed_kmh,
            power: p.power_kw,
            odo: p.odometer_km,
            soc: p.soc,
            range: p.range_km,
            added: p.energy_added_kwh,
            cpower: p.charger_power_kw,
            out_t: p.outside_temp_c,
            in_t: p.inside_temp_c,
        }
    }

    fn position(&self) -> Option<Position> {
        Some(Position {
            t_ms: self.t,
            lat: self.lat?,
            lon: self.lon?,
            elevation_m: self.elev,
            speed_kmh: self.speed,
            power_kw: self.power,
            soc: self.soc,
            odometer_km: self.odo,
            range_km: self.range,
            outside_temp_c: self.out_t,
            inside_temp_c: self.in_t,
        })
    }
}

fn max(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, y) => x.or(y),
    }
}

fn min(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    }
}

/// A drive, charge or state span so far. Joining two (the later one after) gives what one over both would be.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agg {
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<CarState>,
    pub start_ms: u64,
    pub end_ms: u64,
    pub first: Snap,
    pub last: Snap,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_pos: Option<Snap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pos: Option<Snap>,
    #[serde(default)]
    pub start_odo: Option<f64>,
    #[serde(default)]
    pub end_odo: Option<f64>,
    #[serde(default)]
    pub start_soc: Option<f64>,
    #[serde(default)]
    pub end_soc: Option<f64>,
    #[serde(default)]
    pub start_range: Option<f64>,
    #[serde(default)]
    pub end_range: Option<f64>,
    #[serde(default)]
    pub speed_max: Option<f64>,
    #[serde(default)]
    pub power_max: Option<f64>,
    #[serde(default)]
    pub power_min: Option<f64>,
    #[serde(default)]
    pub cpower_max: Option<f64>,
    #[serde(default)]
    pub added_max: Option<f64>,
    #[serde(default)]
    pub charge_kind: Option<ChargeKind>,
    #[serde(default)]
    pub out_sum: f64,
    #[serde(default)]
    pub out_n: u32,
    #[serde(default)]
    pub in_sum: f64,
    #[serde(default)]
    pub in_n: u32,
    #[serde(default)]
    pub ascent: f64,
    #[serde(default)]
    pub descent: f64,
    #[serde(default)]
    pub elev_seen: bool,
    /// Energy integrated (a drive: battery power; a charge: charger power), kWh, and whether any was.
    #[serde(default)]
    pub energy: f64,
    #[serde(default)]
    pub energy_seen: bool,
    pub polls: u64,
    /// The ids this segment (or its parts) has in the store from earlier imports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stored: Vec<i64>,
    /// New or changed in this import (to be handed over).
    #[serde(skip)]
    pub fresh: bool,
}

impl Agg {
    pub(crate) fn new(kind: Kind, p: &Poll) -> Agg {
        let s = Snap::of(p);
        let pos = s.lat.is_some().then(|| s.clone());
        let (out_n, in_n) = (u32::from(p.outside_temp_c.is_some()), u32::from(p.inside_temp_c.is_some()));
        Agg {
            kind,
            state: if kind == Kind::State { p.state } else { None },
            start_ms: p.t_ms,
            end_ms: p.t_ms,
            first_pos: pos.clone(),
            last_pos: pos,
            start_odo: p.odometer_km,
            end_odo: p.odometer_km,
            start_soc: p.soc,
            end_soc: p.soc,
            start_range: p.range_km,
            end_range: p.range_km,
            speed_max: p.speed_kmh,
            power_max: p.power_kw,
            power_min: p.power_kw,
            cpower_max: p.charger_power_kw.filter(|_| p.charging),
            added_max: p.energy_added_kwh.filter(|_| kind == Kind::Charge),
            charge_kind: if kind == Kind::Charge { p.kind } else { None },
            out_sum: p.outside_temp_c.unwrap_or(0.0),
            out_n,
            in_sum: p.inside_temp_c.unwrap_or(0.0),
            in_n,
            ascent: 0.0,
            descent: 0.0,
            elev_seen: p.elevation_m.is_some(),
            energy: 0.0,
            energy_seen: false,
            polls: 1,
            first: s.clone(),
            last: s,
            stored: Vec::new(),
            fresh: true,
        }
    }

    pub(crate) fn add(&mut self, p: &Poll) {
        let next = Agg::new(self.kind, p);
        self.join(next);
    }

    /// `later` (which starts after this one ends) joined on.
    pub(crate) fn join(&mut self, later: Agg) {
        let dt = later.first.t.saturating_sub(self.last.t);
        if dt <= INTEGRATE_GAP_MS {
            let pick = |s: &Snap| match self.kind {
                Kind::Drive => s.power,
                Kind::Charge => s.cpower,
                Kind::State => None,
            };
            if let (Some(a), Some(b)) = (pick(&self.last), pick(&later.first)) {
                self.energy += (a + b) / 2.0 * dt as f64 / 3_600_000.0;
                self.energy_seen = true;
            }
        }
        if let (Some(a), Some(b)) = (self.last.elev, later.first.elev) {
            if b > a {
                self.ascent += b - a;
            } else {
                self.descent += a - b;
            }
        }
        self.energy += later.energy;
        self.energy_seen |= later.energy_seen;
        self.ascent += later.ascent;
        self.descent += later.descent;
        self.elev_seen |= later.elev_seen;
        self.end_ms = self.end_ms.max(later.end_ms);
        self.first_pos = self.first_pos.take().or(later.first_pos);
        if later.last_pos.is_some() {
            self.last_pos = later.last_pos;
        }
        self.start_odo = self.start_odo.or(later.start_odo);
        self.end_odo = later.end_odo.or(self.end_odo);
        self.start_soc = self.start_soc.or(later.start_soc);
        self.end_soc = later.end_soc.or(self.end_soc);
        self.start_range = self.start_range.or(later.start_range);
        self.end_range = later.end_range.or(self.end_range);
        self.speed_max = max(self.speed_max, later.speed_max);
        self.power_max = max(self.power_max, later.power_max);
        self.power_min = min(self.power_min, later.power_min);
        self.cpower_max = max(self.cpower_max, later.cpower_max);
        self.added_max = max(self.added_max, later.added_max);
        self.charge_kind = match (self.charge_kind, later.charge_kind) {
            (Some(ChargeKind::Dc), _) | (_, Some(ChargeKind::Dc)) => Some(ChargeKind::Dc),
            (a, b) => a.or(b),
        };
        self.out_sum += later.out_sum;
        self.out_n += later.out_n;
        self.in_sum += later.in_sum;
        self.in_n += later.in_n;
        self.polls += later.polls;
        self.last = later.last;
        self.stored.extend(later.stored);
        self.fresh |= later.fresh;
    }

    /// Whether `later` (starting after this one) carries this one on.
    pub(crate) fn continues(&self, later: &Agg) -> bool {
        if self.kind != later.kind {
            return false;
        }
        let gap = later.first.t.saturating_sub(self.last.t);
        match self.kind {
            Kind::Drive => gap <= DRIVE_GAP_MS,
            Kind::Charge => {
                gap <= CHARGE_GAP_MS
                    && match (self.last.added, later.first.added) {
                        (Some(a), Some(b)) => b + 0.05 >= a,
                        _ => gap <= INTEGRATE_GAP_MS,
                    }
            }
            Kind::State => self.state == later.state && (self.state == Some(CarState::Asleep) || gap <= ONLINE_GAP_MS),
        }
    }

    /// The id it has in the store: its start, in seconds.
    pub fn id(&self) -> i64 {
        i64::try_from(self.start_ms / 1000).unwrap_or(0)
    }

    fn avg(sum: f64, n: u32) -> Option<f64> {
        (n > 0).then(|| (sum / f64::from(n) * 10.0).round() / 10.0)
    }

    pub fn drive(&self) -> Drive {
        let distance = match (self.start_odo, self.end_odo) {
            (Some(a), Some(b)) if b >= a && b - a < 3000.0 => Some(((b - a) * 1000.0).round() / 1000.0),
            _ => None,
        };
        let r3 = |x: f64| (x * 1000.0).round() / 1000.0;
        Drive {
            id: 0,
            car: 0,
            start_ms: self.start_ms,
            end_ms: self.end_ms,
            start: self.first_pos.as_ref().and_then(Snap::position),
            end: self.last_pos.as_ref().and_then(Snap::position),
            start_place: None,
            end_place: None,
            start_address: None,
            end_address: None,
            start_odometer_km: self.start_odo,
            end_odometer_km: self.end_odo,
            distance_km: distance,
            speed_max_kmh: self.speed_max,
            power_max_kw: self.power_max,
            power_min_kw: self.power_min,
            energy_used_kwh: (self.energy_seen && self.energy > 0.0).then(|| r3(self.energy)),
            start_soc: self.start_soc,
            end_soc: self.end_soc,
            start_range_km: self.start_range,
            end_range_km: self.end_range,
            outside_temp_avg_c: Agg::avg(self.out_sum, self.out_n),
            inside_temp_avg_c: Agg::avg(self.in_sum, self.in_n),
            ascent_m: self.elev_seen.then(|| self.ascent.round()),
            descent_m: self.elev_seen.then(|| self.descent.round()),
            source: Source::Live,
            inputs: Default::default(),
            rules_version: MAPPING_VERSION,
        }
    }

    pub fn charge(&self) -> Charge {
        let r3 = |x: f64| (x * 1000.0).round() / 1000.0;
        let used = (self.charge_kind != Some(ChargeKind::Dc) && self.energy_seen && self.energy > 0.0)
            .then(|| r3(self.energy));
        Charge {
            id: 0,
            car: 0,
            start_ms: self.start_ms,
            end_ms: self.end_ms,
            position: self.first_pos.as_ref().and_then(Snap::position),
            place: None,
            address: None,
            kind: self.charge_kind,
            energy_added_kwh: self.added_max.map(r3),
            energy_used_kwh: used,
            energy_spans: None,
            start_soc: self.start_soc,
            end_soc: self.end_soc,
            start_range_km: self.start_range,
            end_range_km: self.end_range,
            power_max_kw: self.cpower_max,
            outside_temp_avg_c: Agg::avg(self.out_sum, self.out_n),
            battery_temp_avg_c: None,
            cost: None,
            source: Source::Live,
            inputs: Default::default(),
            rules_version: MAPPING_VERSION,
        }
    }

    pub fn span(&self) -> (StateSpan, bool) {
        let (state, offline) = match self.state {
            Some(CarState::Online) => (State::Online, false),
            Some(CarState::Asleep) => (State::Asleep, false),
            _ => (State::Unknown, true),
        };
        let span = StateSpan {
            car: 0,
            state,
            start_ms: self.start_ms,
            end_ms: self.end_ms,
            buses: Vec::new(),
            source: Source::Live,
            inputs: Default::default(),
        };
        (span, offline)
    }
}

/// What a poll led to.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Out {
    /// A segment that began and ended inside the piece.
    Closed(Box<Agg>),
    /// A new software version: `(last poll with the old one, first with the new, version)`.
    Update(u64, u64, String),
}

/// The segments of one piece (a car's run of polls in one file) as its polls come.
#[derive(Debug, Default)]
pub(crate) struct Run {
    pub first_ms: Option<u64>,
    pub last_ms: u64,
    pub rows: u64,
    pub first_version: Option<(u64, String)>,
    pub last_version: Option<(u64, String)>,
    drive: Option<Agg>,
    standing: Vec<Poll>,
    /// The last poll was parked (or nothing came before): a drive may start in gear without moving.
    was_parked: bool,
    charge: Option<Agg>,
    state: Option<Agg>,
    /// Segments that began with the piece's first poll and ended inside it.
    pub heads: Vec<Agg>,
}

impl Run {
    pub fn new() -> Run {
        Run { was_parked: true, ..Run::default() }
    }

    fn close(&mut self, a: Agg, out: &mut Vec<Out>) {
        let opened_at_start = self.first_ms == Some(a.start_ms);
        if opened_at_start {
            self.heads.push(a);
        } else {
            out.push(Out::Closed(Box::new(a)));
        }
    }

    fn close_drive(&mut self, out: &mut Vec<Out>, with_standing: bool) {
        if let Some(mut d) = self.drive.take() {
            if with_standing {
                for p in std::mem::take(&mut self.standing) {
                    d.add(&p);
                }
            }
            self.standing.clear();
            self.close(d, out);
        }
    }

    /// The drive under way (its start), for the points it takes.
    pub fn drive_start(&self) -> Option<u64> {
        self.drive.as_ref().map(|d| d.start_ms)
    }

    pub fn charge_start(&self) -> Option<u64> {
        self.charge.as_ref().map(|c| c.start_ms)
    }

    /// The next poll (later than the last).
    pub fn push(&mut self, p: &Poll, out: &mut Vec<Out>) {
        if self.first_ms.is_none() {
            self.first_ms = Some(p.t_ms);
        }
        self.rows += 1;
        let gap = p.t_ms.saturating_sub(self.last_ms);
        self.last_ms = p.t_ms;

        // the version
        if let Some(v) = &p.version {
            match &mut self.last_version {
                Some((_, old)) if old != v => {
                    let since = self.last_version.as_ref().map_or(p.t_ms, |x| x.0);
                    out.push(Out::Update(since, p.t_ms, v.clone()));
                    self.last_version = Some((p.t_ms, v.clone()));
                }
                Some((t, _)) => *t = p.t_ms,
                None => {
                    self.first_version = Some((p.t_ms, v.clone()));
                    self.last_version = Some((p.t_ms, v.clone()));
                }
            }
        }

        // the drive
        if self.drive.is_some() && gap > DRIVE_GAP_MS {
            self.close_drive(out, true);
            self.was_parked = true;
        }
        let gear_driving = matches!(p.gear, Some(model::Gear::D | model::Gear::R))
            || (p.moving() && matches!(p.gear, None | Some(model::Gear::N)));
        let in_gear = p.driving() || p.moving();
        match &mut self.drive {
            Some(_) if !in_gear || (p.charging && !p.moving()) => self.close_drive(out, true),
            Some(d) if p.moving() => {
                for q in std::mem::take(&mut self.standing) {
                    d.add(&q);
                }
                d.add(p);
            }
            Some(_) => {
                // in gear standing still: held until it is known whether the drive goes on
                let since = self.standing.first().map_or(p.t_ms, |q| q.t_ms);
                let last_moving = self.drive.as_ref().map_or(p.t_ms, |d| d.end_ms);
                if p.t_ms.saturating_sub(last_moving.min(since)) >= STANDING_MS
                    || self.standing.len() >= MAX_STANDING_POLLS
                {
                    self.close_drive(out, false);
                } else {
                    self.standing.push(p.clone());
                }
            }
            None if gear_driving && (p.moving() || self.was_parked) && !p.charging => {
                self.drive = Some(Agg::new(Kind::Drive, p));
            }
            None => {}
        }
        self.was_parked = !in_gear;

        // the charge
        if let Some(c) = &self.charge {
            let reset = matches!((c.last.added, p.energy_added_kwh), (Some(a), Some(b)) if b + 0.5 < a);
            if (!p.charging || p.moving() || gap > CHARGE_GAP_MS || reset)
                && let Some(c) = self.charge.take()
            {
                self.close(c, out);
            }
        }
        if p.charging && !p.moving() {
            match &mut self.charge {
                Some(c) => c.add(p),
                None => self.charge = Some(Agg::new(Kind::Charge, p)),
            }
        }

        // the state
        let st = p.state.or((p.driving() || p.charging).then_some(CarState::Online));
        if let Some(st) = st {
            let carry_on = self.state.as_ref().map(|s| {
                let same = s.state == Some(st);
                (same && (st == CarState::Asleep || gap <= ONLINE_GAP_MS), s.state == Some(CarState::Asleep))
            });
            match carry_on {
                Some((true, _)) => {
                    if let Some(s) = &mut self.state {
                        s.add(p);
                    }
                }
                Some((false, asleep)) => {
                    if let Some(mut s) = self.state.take() {
                        // asleep until seen again; online up to the next poll when it came soon
                        if asleep || gap <= ONLINE_GAP_MS {
                            s.end_ms = p.t_ms;
                        }
                        self.close(s, out);
                    }
                    let mut n = Agg::new(Kind::State, p);
                    n.state = Some(st);
                    self.state = Some(n);
                }
                None => {
                    let mut n = Agg::new(Kind::State, p);
                    n.state = Some(st);
                    self.state = Some(n);
                }
            }
        }
    }

    /// The piece is over: what is still under way.
    pub fn finish(mut self, out: &mut Vec<Out>) -> (Run, Vec<Agg>) {
        let mut tails = Vec::new();
        if let Some(mut d) = self.drive.take() {
            // standing still at the end: the drive ended where it stopped
            let standing = std::mem::take(&mut self.standing);
            let still = standing.last().is_some_and(|q| q.t_ms.saturating_sub(d.end_ms) >= STANDING_MS);
            if still {
                self.close(d, out);
            } else {
                for q in standing {
                    d.add(&q);
                }
                tails.push(d);
            }
        }
        tails.extend(self.charge.take());
        tails.extend(self.state.take());
        (self, tails)
    }
}
