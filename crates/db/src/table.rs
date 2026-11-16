//! A car's time-indexed table in memory while the car is open: rows by id, and an index by start time. Changes are
//! journaled as batches (`{"del": [ids], "put": [rows]}`), one batch per write, so a write is all or nothing.

use crate::error::{Error, Result};
use crate::file::{Ctx, Kind};
use crate::logged::Logged;
use crate::rows::{Cursor, Item, MAX_PAGE, Order, Page, Query};
use model::Source;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::ops::Bound;

pub(crate) struct TimeTable<T> {
    rows: HashMap<u64, T>,
    by_start: BTreeSet<(u64, u64)>,
    /// The longest row ever held: rows that start this long before a window can still reach into it.
    max_span: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "T: Item")]
pub(crate) struct Batch<T> {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub del: Vec<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub put: Vec<T>,
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "T: Item")]
struct Snapshot<T> {
    rows: Vec<T>,
}

impl<T: Item> TimeTable<T> {
    pub(crate) fn get(&self, id: u64) -> Option<&T> {
        self.rows.get(&id)
    }

    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    pub(crate) fn all(&self) -> impl Iterator<Item = &T> {
        self.by_start.iter().filter_map(|(_, id)| self.rows.get(id))
    }

    fn overlaps(row: &T, from: u64, to: u64) -> bool {
        let (s, e) = (row.start_ms(), row.end_ms());
        s < to && (e > from || s >= from)
    }

    /// The ids of rows from `source` that start in `[from, to)`.
    pub(crate) fn starting_in(&self, from: u64, to: u64, source: Source) -> Vec<u64> {
        if from >= to {
            // BTreeSet::range panics on a backwards range
            return Vec::new();
        }
        self.by_start
            .range((Bound::Included((from, 0)), Bound::Excluded((to, 0))))
            .filter(|(_, id)| self.rows.get(id).is_some_and(|r| r.source() == source))
            .map(|(_, id)| *id)
            .collect()
    }

    pub(crate) fn query(&self, q: &Query) -> Page<T> {
        let limit = q.limit.min(MAX_PAGE);
        let lo = (q.from_ms.saturating_sub(self.max_span), 0);
        let hi = (q.to_ms, 0);
        let keep = |r: &&T| Self::overlaps(r, q.from_ms, q.to_ms) && q.source.is_none_or(|s| r.source() == s);
        let mut items = Vec::new();
        let mut more = false;
        let mut push = |key: &(u64, u64)| -> bool {
            if let Some(r) = self.rows.get(&key.1).filter(keep) {
                if items.len() == limit {
                    more = true;
                    return false;
                }
                items.push(r.clone());
            }
            true
        };
        match q.order {
            Order::OldestFirst => {
                let start = match q.after {
                    Some(c) if (c.start_ms, c.id) >= lo => Bound::Excluded((c.start_ms, c.id)),
                    _ => Bound::Included(lo),
                };
                if start_before_end(start, hi) {
                    for k in self.by_start.range((start, Bound::Excluded(hi))) {
                        if !push(k) {
                            break;
                        }
                    }
                }
            }
            Order::NewestFirst => {
                let end = match q.after {
                    Some(c) if (c.start_ms, c.id) < hi => Bound::Excluded((c.start_ms, c.id)),
                    _ => Bound::Excluded(hi),
                };
                if start_before_end(Bound::Included(lo), end_key(end)) {
                    for k in self.by_start.range((Bound::Included(lo), end)).rev() {
                        if !push(k) {
                            break;
                        }
                    }
                }
            }
        }
        let next = if more { items.last().map(|r| Cursor { start_ms: r.start_ms(), id: r.id() }) } else { None };
        Page { items, next }
    }
}

fn end_key(b: Bound<(u64, u64)>) -> (u64, u64) {
    match b {
        Bound::Included(k) | Bound::Excluded(k) => k,
        Bound::Unbounded => (u64::MAX, u64::MAX),
    }
}

/// Whether a range from `start` to `end` (exclusive) is well formed (BTreeSet::range panics otherwise).
fn start_before_end(start: Bound<(u64, u64)>, end: (u64, u64)) -> bool {
    match start {
        Bound::Included(s) => s < end,
        Bound::Excluded(s) => s < end,
        Bound::Unbounded => true,
    }
}

impl<T: Item> Logged for TimeTable<T> {
    type Op = Batch<T>;
    const KIND: Kind = Kind::Snapshot;

    fn empty() -> Self {
        TimeTable { rows: HashMap::new(), by_start: BTreeSet::new(), max_span: 0 }
    }

    fn apply(&mut self, op: Batch<T>) {
        for id in op.del {
            if let Some(old) = self.rows.remove(&id) {
                self.by_start.remove(&(old.start_ms(), id));
            }
        }
        for row in op.put {
            let id = row.id();
            if let Some(old) = self.rows.remove(&id) {
                self.by_start.remove(&(old.start_ms(), id));
            }
            self.max_span = self.max_span.max(row.end_ms() - row.start_ms());
            self.by_start.insert((row.start_ms(), id));
            self.rows.insert(id, row);
        }
    }

    fn encode_op(_ctx: &Ctx, op: &Batch<T>) -> Result<Vec<u8>> {
        serde_json::to_vec(op).map_err(|e| Error::Corrupt(e.to_string()))
    }

    fn decode_op(b: &[u8]) -> Result<Batch<T>> {
        serde_json::from_slice(b).map_err(|e| Error::Corrupt(format!("a journal entry: {e}")))
    }

    fn encode_state(&self) -> Result<Vec<u8>> {
        let rows: Vec<&T> = self.all().collect();
        serde_json::to_vec(&serde_json::json!({ "rows": rows })).map_err(|e| Error::Corrupt(e.to_string()))
    }

    fn decode_state(b: &[u8]) -> Result<Self> {
        let s: Snapshot<T> = serde_json::from_slice(b).map_err(|e| Error::Corrupt(format!("a snapshot: {e}")))?;
        let mut t = TimeTable::empty();
        t.apply(Batch { del: Vec::new(), put: s.rows });
        Ok(t)
    }
}
