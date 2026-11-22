//! What earlier imports took from a car, so a later backup adds only what's new.
//!
//! TeslaMate numbers every row from one counter per table, so a newer backup's new rows have higher ids. A car's
//! [`ImportState`] keeps, per table, the highest id an import has seen and the ids of rows that were still open then
//! (a drive, charge, state or update with no end yet): those are taken when a later backup has their end. It stays
//! small however long the history is (a few numbers per table, plus the open rows, at most
//! [`Options::max_open`](crate::Options::max_open) each), so the caller keeps one per car and rewrites it after each
//! import.
//!
//! What it doesn't catch: a row TeslaMate changes after it was imported (a cost typed in later) isn't taken again,
//! and a row whose insert was still being committed while the earlier backup was made (TeslaMate writes a position
//! every few seconds while driving) can be passed over. Backups made while the car is parked avoid the second.

use crate::schema::Table;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The layout of [`ImportState`]; a different one is refused rather than misread.
pub const STATE_VERSION: u32 = 1;

/// One car's import marks (keep it with the car; give it back to the next import).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportState {
    pub version: u32,
    /// Per table of the car's history.
    pub tables: BTreeMap<Table, Mark>,
    /// What TeslaFi imports took (kept apart: TeslaFi has no row ids).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teslafi: Option<crate::teslafi::State>,
}

impl Default for ImportState {
    fn default() -> Self {
        ImportState { version: STATE_VERSION, tables: BTreeMap::new(), teslafi: None }
    }
}

/// What earlier imports took from one table of one car.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Mark {
    /// Every row of this car with an id at or below this was seen: taken, or left out for good.
    pub high_id: i64,
    /// The time of that row (its date or start), to notice a backup of another TeslaMate database.
    pub high_ms: Option<u64>,
    /// Rows that had no end yet: taken once a backup has their end.
    pub open: BTreeSet<i64>,
}

impl ImportState {
    /// Whether a row was seen by an earlier import and needn't be looked at again.
    pub(crate) fn seen(&self, table: Table, id: i64) -> bool {
        self.tables.get(&table).is_some_and(|m| id <= m.high_id && !m.open.contains(&id))
    }
}

/// What this import sees of one table of one car, to make the next [`Mark`].
#[derive(Debug, Default)]
pub(crate) struct TableWork {
    /// The highest id in this backup, and its time.
    high: Option<(i64, Option<u64>)>,
    /// Rows without an end in this backup.
    open: BTreeSet<i64>,
    /// Earlier open rows that this backup has (open or not).
    old_open_seen: BTreeSet<i64>,
    /// The row with the earlier high id has another time: probably another database.
    mismatch: bool,
    /// Open rows not remembered (past the ceilings).
    forgotten: u64,
}

/// How much later than the row that set a mark another database's newest row must be to be taken for one (rows are
/// numbered as they are written, which can be a little out of time order).
const OTHER_DATABASE_SLACK_MS: u64 = 10 * 60_000;

/// How the open rows changed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpenChange {
    /// Earlier open rows this backup no longer has (TeslaMate deleted them).
    pub gone: u64,
    /// Open rows past [`Options::max_open`](crate::Options::max_open), the oldest, no longer waited for.
    pub forgotten: u64,
}

impl TableWork {
    /// Every row of this car and table goes through here, whatever becomes of it.
    pub fn see(&mut self, old: Option<&Mark>, id: i64, time: Option<u64>) {
        if self.high.is_none_or(|(h, _)| id > h) {
            self.high = Some((id, time));
        }
        if let Some(m) = old {
            if id == m.high_id && m.high_ms.is_some() && time.is_some() && time != m.high_ms {
                self.mismatch = true;
            }
            if m.open.contains(&id) {
                self.old_open_seen.insert(id);
            }
        }
    }

    /// Whether this backup looks like another TeslaMate database than the one the mark came from (TeslaMate set up
    /// afresh numbers its rows from 1 again, so its rows look imported already): the row that set the mark has another
    /// time here, or this backup's newest row has a lower id than that row yet is later (one database numbers its
    /// rows in the order they come, so its older backups end with older rows).
    pub fn other_database(&self, old: Option<&Mark>) -> bool {
        if self.mismatch {
            return true;
        }
        match (old, self.high) {
            (Some(m), Some((h, Some(t)))) => {
                h < m.high_id && m.high_ms.is_some_and(|ms| t > ms.saturating_add(OTHER_DATABASE_SLACK_MS))
            }
            _ => false,
        }
    }

    /// A row without an end: remembered, the oldest forgotten past `max` (or this one, when `room` is false: the
    /// import's ceiling for all cars is reached).
    pub fn still_open(&mut self, id: i64, max: usize, room: bool) -> bool {
        if !room {
            self.forgotten += 1;
            return false;
        }
        self.open.insert(id);
        if self.open.len() > max {
            self.open.pop_first();
            self.forgotten += 1;
        }
        true
    }

    /// The mark to keep for the next import.
    pub fn finish(self, old: Option<&Mark>, max_open: usize) -> (Mark, OpenChange) {
        let mut change = OpenChange { gone: 0, forgotten: self.forgotten };
        let backup_high = self.high.map(|(h, _)| h);
        let (high_id, high_ms) = match (old, self.high) {
            (Some(m), Some((h, t))) if h > m.high_id => (h, t),
            (Some(m), _) => (m.high_id, m.high_ms),
            (None, Some((h, t))) => (h, t),
            (None, None) => (0, None),
        };
        let mut open = self.open;
        if let Some(m) = old {
            for &id in &m.open {
                if self.old_open_seen.contains(&id) {
                    continue;
                }
                // Not in this backup: deleted if the backup is newer than that row, else an older backup.
                if backup_high.is_some_and(|h| id <= h) {
                    change.gone += 1;
                } else {
                    open.insert(id);
                }
            }
        }
        while open.len() > max_open {
            open.pop_first();
            change.forgotten += 1;
        }
        (Mark { high_id, high_ms, open }, change)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn marks_move_forward_and_keep_open_rows() {
        let mut w = TableWork::default();
        for (id, t) in [(3, 30), (1, 10), (2, 20)] {
            w.see(None, id, Some(t));
        }
        w.still_open(3, 10, true);
        let (m, c) = w.finish(None, 10);
        assert_eq!(m, Mark { high_id: 3, high_ms: Some(30), open: [3].into() });
        assert_eq!(c, OpenChange::default());

        let mut st = ImportState::default();
        st.tables.insert(Table::Drives, m.clone());
        assert!(st.seen(Table::Drives, 2));
        assert!(!st.seen(Table::Drives, 3));
        assert!(!st.seen(Table::Drives, 4));
        assert!(!st.seen(Table::States, 1));

        // A newer backup: 3 is closed now, 5 is new and open, 4 is new.
        let mut w = TableWork::default();
        for (id, t) in [(1, 10), (2, 20), (3, 30), (4, 40), (5, 50)] {
            w.see(Some(&m), id, Some(t));
        }
        w.still_open(5, 10, true);
        let (m2, _) = w.finish(Some(&m), 10);
        assert_eq!(m2, Mark { high_id: 5, high_ms: Some(50), open: [5].into() });
        assert!(!w_mismatch(&m, 3, 30));
        assert!(w_mismatch(&m, 3, 99));
        // Another database with fewer rows: its newest is below the mark but later; an older backup's is earlier.
        let newest = |id, t| {
            let mut w = TableWork::default();
            w.see(Some(&m), id, Some(t));
            w.other_database(Some(&m))
        };
        assert!(newest(2, 30 + OTHER_DATABASE_SLACK_MS + 1));
        assert!(!newest(2, 20) && !newest(2, 30 + OTHER_DATABASE_SLACK_MS) && !newest(4, 1_000_000_000));

        // An older backup doesn't lose what is still open, nor move the mark back.
        let mut w = TableWork::default();
        w.see(Some(&m2), 1, Some(10));
        let (m3, c) = w.finish(Some(&m2), 10);
        assert_eq!(m3, m2);
        assert_eq!(c.gone, 0);

        // A newer backup without row 5: TeslaMate deleted it.
        let mut w = TableWork::default();
        w.see(Some(&m2), 6, Some(60));
        let (m4, c) = w.finish(Some(&m2), 10);
        assert!(m4.open.is_empty());
        assert_eq!(c.gone, 1);
    }

    fn w_mismatch(m: &Mark, id: i64, t: u64) -> bool {
        let mut w = TableWork::default();
        w.see(Some(m), id, Some(t));
        w.mismatch
    }

    #[test]
    fn open_rows_are_capped_oldest_first() {
        let mut w = TableWork::default();
        for id in 1..=5 {
            w.see(None, id, None);
            w.still_open(id, 3, true);
        }
        w.still_open(6, 3, false);
        let (m, c) = w.finish(None, 2);
        assert_eq!(m.open, [4, 5].into());
        assert_eq!(c.forgotten, 4);
    }

    #[test]
    fn state_round_trips_as_json() {
        let mut st = ImportState::default();
        st.tables.insert(Table::ChargingProcesses, Mark { high_id: 7, high_ms: Some(1), open: [7].into() });
        let j = serde_json::to_string(&st).unwrap();
        assert!(j.contains("\"charging_processes\""), "{j}");
        assert_eq!(serde_json::from_str::<ImportState>(&j).unwrap(), st);
    }
}
