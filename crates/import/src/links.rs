//! What the first read of the backup keeps so the second can link tables: TeslaMate's tables point at each other by
//! id (a drive at its first and last positions and its addresses, a charge point at its charge, which names the car),
//! and pg_dump writes them in an order where the one pointed at often comes later.
//!
//! Everything here has a fixed ceiling ([`Options`](crate::Options)): a backup of any size, with any number of cars,
//! can't make it grow past that. Past a ceiling the link is left out (the ids are still handed over, and the report
//! counts what didn't fit); nothing is guessed.

use crate::report::CarTally;
use crate::schema::Table;
use crate::state::{ImportState, TableWork};
use model::{CarId, Position};
use std::collections::HashMap;

/// Short names of addresses, to name drive and charge ends.
pub(crate) struct AddressBook {
    labels: HashMap<i64, Box<str>>,
    bytes: usize,
    max_bytes: usize,
    pub over: u64,
}

/// Per entry, besides the text: the key, the box and the table's own share.
const LABEL_OVERHEAD: usize = 40;

impl AddressBook {
    pub fn new(max_bytes: usize) -> Self {
        AddressBook { labels: HashMap::new(), bytes: 0, max_bytes, over: 0 }
    }

    pub fn add(&mut self, id: i64, label: &str) {
        if self.labels.contains_key(&id) {
            return;
        }
        let cost = label.len() + LABEL_OVERHEAD;
        if self.bytes + cost > self.max_bytes {
            self.over += 1;
            return;
        }
        self.bytes += cost;
        self.labels.insert(id, label.into());
    }

    pub fn get(&self, id: Option<i64>) -> Option<String> {
        id.and_then(|i| self.labels.get(&i)).map(|l| l.to_string())
    }

    pub fn len(&self) -> usize {
        self.labels.len()
    }
}

/// A charge (TeslaMate's `charging_processes` row) as its points and its own row tell it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Proc {
    /// TeslaMate's car id, [`Proc::NO_CAR`] until the charge's row is read.
    pub car: i64,
    /// 0 unknown, 1 AC, 2 DC (a point with a fast charger makes it DC).
    pub kind: u8,
    /// The highest charger power of its points, kW; NaN when none had one.
    pub power_max: f32,
}

impl Proc {
    pub const NO_CAR: i64 = i64::MIN;

    pub fn car(&self) -> Option<i64> {
        (self.car != Proc::NO_CAR).then_some(self.car)
    }

    pub fn power_max_kw(&self) -> Option<f64> {
        (!self.power_max.is_nan()).then_some(f64::from(self.power_max))
    }
}

pub(crate) struct Procs {
    map: HashMap<i64, Proc>,
    max: usize,
    pub over: u64,
}

impl Procs {
    pub fn new(max: usize) -> Self {
        Procs { map: HashMap::new(), max, over: 0 }
    }

    /// The charge's entry, made if there is room.
    pub fn entry(&mut self, id: i64) -> Option<&mut Proc> {
        if !self.map.contains_key(&id) {
            if self.map.len() >= self.max {
                self.over += 1;
                return None;
            }
            self.map.insert(id, Proc { car: Proc::NO_CAR, kind: 0, power_max: f32::NAN });
        }
        self.map.get_mut(&id)
    }

    pub fn get(&self, id: i64) -> Option<Proc> {
        self.map.get(&id).copied()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
}

/// A position kept to be a drive's end or a charge's place: [`Position`] without its `Option`s' room (NaN is none).
#[derive(Debug, Clone, Copy)]
struct CompactPos {
    t_ms: u64,
    v: [f64; 10],
}

impl CompactPos {
    fn of(p: &Position) -> Self {
        let n = |o: Option<f64>| o.unwrap_or(f64::NAN);
        CompactPos {
            t_ms: p.t_ms,
            v: [
                p.lat,
                p.lon,
                n(p.elevation_m),
                n(p.speed_kmh),
                n(p.power_kw),
                n(p.soc),
                n(p.odometer_km),
                n(p.range_km),
                n(p.outside_temp_c),
                n(p.inside_temp_c),
            ],
        }
    }

    fn position(&self) -> Position {
        let o = |i: usize| Some(self.v[i]).filter(|x| !x.is_nan());
        Position {
            t_ms: self.t_ms,
            lat: self.v[0],
            lon: self.v[1],
            elevation_m: o(2),
            speed_kmh: o(3),
            power_kw: o(4),
            soc: o(5),
            odometer_km: o(6),
            range_km: o(7),
            outside_temp_c: o(8),
            inside_temp_c: o(9),
        }
    }
}

/// Positions wanted as drive ends and charge places, by TeslaMate id.
pub(crate) struct Ends {
    map: HashMap<i64, Option<CompactPos>>,
    max: usize,
    pub over: u64,
    unresolved: usize,
}

impl Ends {
    pub fn new(max: usize) -> Self {
        Ends { map: HashMap::new(), max, over: 0, unresolved: 0 }
    }

    pub fn want(&mut self, id: i64) {
        if self.map.contains_key(&id) {
            return;
        }
        if self.map.len() >= self.max {
            self.over += 1;
            return;
        }
        self.map.insert(id, None);
        self.unresolved += 1;
    }

    /// Whether a position is wanted and not found yet.
    pub fn wants(&self, id: i64) -> bool {
        self.unresolved > 0 && matches!(self.map.get(&id), Some(None))
    }

    pub fn any_wanted(&self) -> bool {
        self.unresolved > 0
    }

    pub fn fill(&mut self, id: i64, p: &Position) {
        if let Some(slot @ None) = self.map.get_mut(&id) {
            *slot = Some(CompactPos::of(p));
            self.unresolved -= 1;
        }
    }

    pub fn get(&self, id: Option<i64>) -> Option<Position> {
        id.and_then(|i| self.map.get(&i)).and_then(|e| e.as_ref()).map(CompactPos::position)
    }

    pub fn wanted(&self) -> usize {
        self.map.len()
    }

    pub fn found(&self) -> usize {
        self.map.len() - self.unresolved
    }
}

/// Where a TeslaMate car's rows go.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Route {
    To {
        slot: usize,
        report: usize,
        /// kWh per km, as TeslaMate keeps it.
        efficiency: Option<f64>,
    },
    LeftOut {
        report: usize,
    },
}

/// One Sentry Mate car being imported into.
pub(crate) struct Slot {
    pub car: CarId,
    pub state: ImportState,
    /// Its first TeslaMate car's report (where its state's news goes, and which carries the state at the end).
    pub report: usize,
    /// By [`Table::PER_CAR`] position.
    pub work: [TableWork; 6],
}

impl Slot {
    pub fn work(&mut self, t: Table) -> Option<&mut TableWork> {
        t.car_ix().map(|i| &mut self.work[i])
    }
}

/// The cars of the backup and where their rows go: one small fixed-size entry per car (about 2 KB with its counts and
/// batch, whatever its history holds).
#[derive(Default)]
pub(crate) struct Routes {
    pub by_tm: HashMap<i64, Route>,
    pub slots: Vec<Slot>,
    pub reports: Vec<CarTally>,
    /// The `cars` table was read and every car asked about.
    pub known: bool,
}

impl Routes {
    /// Whether this import will take a row of a car (when the cars aren't known yet: maybe).
    pub fn may_take(&self, table: Table, tm_car: i64, id: i64) -> bool {
        if !self.known {
            return true;
        }
        match self.by_tm.get(&tm_car) {
            Some(Route::To { slot, .. }) => self.slots.get(*slot).is_some_and(|s| !s.state.seen(table, id)),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn ceilings_hold() {
        let mut b = AddressBook::new(2 * (LABEL_OVERHEAD + 3));
        b.add(1, "one");
        b.add(2, "two");
        b.add(3, "six");
        b.add(1, "one");
        assert_eq!((b.len(), b.over), (2, 1));
        assert_eq!(b.get(Some(2)).as_deref(), Some("two"));
        assert_eq!(b.get(Some(3)), None);

        let mut p = Procs::new(1);
        p.entry(5).unwrap().power_max = 11.0;
        assert!(p.entry(6).is_none());
        assert_eq!((p.len(), p.over), (1, 1));
        assert_eq!(p.get(5).unwrap().power_max_kw(), Some(11.0));
        assert_eq!(p.get(5).unwrap().car(), None);

        let mut e = Ends::new(2);
        e.want(10);
        e.want(11);
        e.want(12);
        assert_eq!((e.wanted(), e.over), (2, 1));
        assert!(e.wants(10) && !e.wants(12));
        let pos = Position { t_ms: 5, lat: 1.5, lon: -2.5, soc: Some(80.0), ..Position::default() };
        e.fill(10, &pos);
        e.fill(12, &pos);
        assert!(!e.wants(10));
        assert_eq!(e.found(), 1);
        assert_eq!(e.get(Some(10)), Some(pos));
        assert_eq!(e.get(Some(11)), None);
    }
}
