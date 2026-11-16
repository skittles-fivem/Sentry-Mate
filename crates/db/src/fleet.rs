//! The car list and a small summary of each car, kept up to date as the car's data changes, so the fleet page and
//! the car search page through any number of cars without opening one. This is the one thing held for every car:
//! a few hundred bytes each (the car's record, its latest Now, counts and totals).

use crate::error::{Error, Result};
use crate::file::{Ctx, Kind};
use crate::logged::Logged;
use crate::rows::Tally;
use model::{Car, CarId, Now};
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Bound;

/// What the fleet page shows of a car without opening it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// The newest time anything was recorded for the car (a row's end, or its latest Now).
    pub last_activity_ms: Option<u64>,
    /// The car's newest values.
    pub now: Option<Now>,
    /// Counts and totals of its rows, by source.
    pub tally: Tally,
}

/// A car and its summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetEntry {
    pub car: Car,
    pub summary: Summary,
}

/// Where a fleet page ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FleetCursor {
    pub last_activity_ms: u64,
    pub car: CarId,
}

impl std::fmt::Display for FleetCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.last_activity_ms, self.car)
    }
}

impl std::str::FromStr for FleetCursor {
    type Err = Error;
    fn from_str(s: &str) -> Result<FleetCursor> {
        let bad = || Error::invalid("a fleet cursor is <last_activity_ms>.<car>");
        let (a, b) = s.split_once('.').ok_or_else(bad)?;
        Ok(FleetCursor { last_activity_ms: a.parse().map_err(|_| bad())?, car: b.parse().map_err(|_| bad())? })
    }
}

/// Cars, most recently active first, a page at a time; `search` keeps those whose name or a CAN key contains it
/// (any case), or whose id it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetQuery {
    pub search: Option<String>,
    pub limit: usize,
    pub after: Option<FleetCursor>,
}

impl Default for FleetQuery {
    fn default() -> FleetQuery {
        FleetQuery { search: None, limit: 50, after: None }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FleetPage {
    pub cars: Vec<FleetEntry>,
    pub next: Option<FleetCursor>,
}

#[derive(Serialize, Deserialize)]
pub(crate) enum FleetOp {
    Put(Box<FleetEntry>),
    Remove(CarId),
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    next_id: CarId,
    cars: Vec<FleetEntry>,
}

pub(crate) struct Fleet {
    cars: BTreeMap<CarId, FleetEntry>,
    by_activity: BTreeSet<(Reverse<u64>, CarId)>,
    keys: HashMap<String, CarId>,
    /// Lower-case name and keys, for search.
    text: HashMap<CarId, String>,
    pub next_id: CarId,
}

fn activity_key(e: &FleetEntry) -> (Reverse<u64>, CarId) {
    (Reverse(e.summary.last_activity_ms.unwrap_or(0)), e.car.id)
}

impl Fleet {
    pub(crate) fn get(&self, id: CarId) -> Option<&FleetEntry> {
        self.cars.get(&id)
    }

    pub(crate) fn contains(&self, id: CarId) -> bool {
        self.cars.contains_key(&id)
    }

    pub(crate) fn len(&self) -> usize {
        self.cars.len()
    }

    pub(crate) fn car_for_key(&self, key: &str) -> Option<CarId> {
        self.keys.get(key).copied()
    }

    pub(crate) fn query(&self, q: &FleetQuery) -> FleetPage {
        let limit = q.limit.min(crate::rows::MAX_PAGE);
        let needle = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_lowercase);
        let start = match q.after {
            Some(c) => Bound::Excluded((Reverse(c.last_activity_ms), c.car)),
            None => Bound::Unbounded,
        };
        let mut cars = Vec::new();
        let mut more = false;
        for (_, id) in self.by_activity.range((start, Bound::Unbounded)) {
            let matches = match &needle {
                None => true,
                Some(n) => id.to_string() == *n || self.text.get(id).is_some_and(|t| t.contains(n.as_str())),
            };
            if !matches {
                continue;
            }
            if cars.len() == limit {
                more = true;
                break;
            }
            if let Some(e) = self.cars.get(id) {
                cars.push(e.clone());
            }
        }
        let next = if more {
            cars.last().map(|e: &FleetEntry| FleetCursor {
                last_activity_ms: e.summary.last_activity_ms.unwrap_or(0),
                car: e.car.id,
            })
        } else {
            None
        };
        FleetPage { cars, next }
    }

    fn unindex(&mut self, id: CarId) -> Option<FleetEntry> {
        let old = self.cars.remove(&id)?;
        self.by_activity.remove(&activity_key(&old));
        for k in &old.car.vehicle_keys {
            if self.keys.get(k) == Some(&id) {
                self.keys.remove(k);
            }
        }
        self.text.remove(&id);
        Some(old)
    }
}

impl Logged for Fleet {
    type Op = FleetOp;
    const KIND: Kind = Kind::Snapshot;

    fn empty() -> Self {
        Fleet {
            cars: BTreeMap::new(),
            by_activity: BTreeSet::new(),
            keys: HashMap::new(),
            text: HashMap::new(),
            next_id: 1,
        }
    }

    fn apply(&mut self, op: FleetOp) {
        match op {
            FleetOp::Put(e) => {
                let id = e.car.id;
                self.unindex(id);
                self.next_id = self.next_id.max(id.saturating_add(1));
                self.by_activity.insert(activity_key(&e));
                for k in &e.car.vehicle_keys {
                    self.keys.insert(k.clone(), id);
                }
                let mut text = e.car.name.to_lowercase();
                for k in &e.car.vehicle_keys {
                    text.push('\n');
                    text.push_str(&k.to_lowercase());
                }
                self.text.insert(id, text);
                self.cars.insert(id, *e);
            }
            FleetOp::Remove(id) => {
                self.unindex(id);
            }
        }
    }

    fn encode_op(_ctx: &Ctx, op: &FleetOp) -> Result<Vec<u8>> {
        serde_json::to_vec(op).map_err(|e| Error::Corrupt(e.to_string()))
    }

    fn decode_op(b: &[u8]) -> Result<FleetOp> {
        serde_json::from_slice(b).map_err(|e| Error::Corrupt(format!("a fleet entry: {e}")))
    }

    fn encode_state(&self) -> Result<Vec<u8>> {
        let s = serde_json::json!({ "next_id": self.next_id, "cars": self.cars.values().collect::<Vec<_>>() });
        serde_json::to_vec(&s).map_err(|e| Error::Corrupt(e.to_string()))
    }

    fn decode_state(b: &[u8]) -> Result<Self> {
        let s: Snapshot = serde_json::from_slice(b).map_err(|e| Error::Corrupt(format!("the fleet: {e}")))?;
        let mut f = Fleet::empty();
        for e in s.cars {
            f.apply(FleetOp::Put(Box::new(e)));
        }
        f.next_id = f.next_id.max(s.next_id);
        Ok(f)
    }
}
