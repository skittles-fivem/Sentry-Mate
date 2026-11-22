//! What an import did, for the owner and the audit log: rows read, taken and left out (with why) per table and per
//! car, the time span, and what in the backup this importer doesn't know. It holds no VIN serials, coordinates or
//! values from the rows, only counts, names and column names, so it can be shown and kept as it is.
//!
//! The [`Report`] is the whole import's and stays the same size however many cars there are; each car's own
//! [`CarReport`] is handed to the sink with its state when the import ends ([`Sink::car_done`](crate::Sink)), one car
//! at a time. While the import runs a car's counts take a fixed few hundred bytes ([`CarTally`]).

use crate::input::{Compression, Encoding};
use crate::schema::Table;
use model::CarId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Why a row wasn't taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Skip {
    /// An earlier import took it (or left it out for good).
    AlreadyImported,
    /// No end yet (still going when the backup was made): taken from a later backup.
    Open,
    /// Its car isn't in the backup's `cars` table.
    UnknownCar,
    /// The caller left its car out.
    CarLeftOut,
    /// A charge point whose charge isn't in the backup, or wasn't kept ([`Options::max_charges`](crate::Options)).
    UnknownCharge,
    /// A value it needs is missing or unreadable (`bad_values` says which).
    BadValue,
    /// Not as many fields as the table has columns.
    WrongFieldCount,
    /// Longer than [`Options::max_line`](crate::Options).
    TooLong,
}

impl Skip {
    pub const ALL: [Skip; 8] = [
        Skip::AlreadyImported,
        Skip::Open,
        Skip::UnknownCar,
        Skip::CarLeftOut,
        Skip::UnknownCharge,
        Skip::BadValue,
        Skip::WrongFieldCount,
        Skip::TooLong,
    ];

    fn ix(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TableReport {
    pub read: u64,
    pub imported: u64,
    pub skipped: BTreeMap<Skip, u64>,
    /// Columns whose value was missing or unreadable, how often: the row was left out when it needed the value,
    /// else the value was read as empty.
    pub bad_values: BTreeMap<String, u64>,
    /// Rows taken with something changed or estimated, how often.
    pub notes: BTreeMap<String, u64>,
}

impl TableReport {
    pub fn skipped(&self, why: Skip) -> u64 {
        self.skipped.get(&why).copied().unwrap_or(0)
    }

    pub(crate) fn skip(&mut self, why: Skip) {
        *self.skipped.entry(why).or_default() += 1;
    }

    pub(crate) fn bad(&mut self, column: &str) {
        *self.bad_values.entry(column.to_string()).or_default() += 1;
    }

    pub(crate) fn note(&mut self, what: &str) {
        *self.notes.entry(what.to_string()).or_default() += 1;
    }
}

/// One car of the backup. Its tables' `bad_values` and `notes` are counted in the import's [`Report`] only.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CarReport {
    /// TeslaMate's id for it.
    pub tm_car_id: i64,
    /// The car the caller said it is; `None` when left out.
    pub car: Option<CarId>,
    pub vin_prefix: Option<String>,
    pub name: Option<String>,
    pub tables: BTreeMap<Table, TableReport>,
    /// The time span of the rows taken.
    pub first_ms: Option<u64>,
    pub last_ms: Option<u64>,
    /// Rows still without an end, waited for in a later backup.
    pub open_waiting: u64,
    pub warnings: Vec<String>,
}

impl CarReport {
    pub fn table(&self, t: Table) -> TableReport {
        self.tables.get(&t).cloned().unwrap_or_default()
    }
}

/// A car's counts for one table while the import runs: fixed size, no allocation.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Tally {
    pub read: u64,
    pub imported: u64,
    skipped: [u64; 8],
}

impl Tally {
    pub fn skip(&mut self, why: Skip) {
        self.skipped[why.ix()] += 1;
    }

    pub(crate) fn report(&self) -> TableReport {
        TableReport {
            read: self.read,
            imported: self.imported,
            skipped: Skip::ALL
                .iter()
                .filter(|w| self.skipped[w.ix()] > 0)
                .map(|w| (*w, self.skipped[w.ix()]))
                .collect(),
            ..TableReport::default()
        }
    }

    fn is_empty(&self) -> bool {
        self.read == 0 && self.imported == 0 && self.skipped.iter().all(|n| *n == 0)
    }
}

/// A car's counts while the import runs, made a [`CarReport`] at its end.
#[derive(Debug, Clone, Default)]
pub(crate) struct CarTally {
    pub tm_car_id: i64,
    pub car: Option<CarId>,
    pub vin_prefix: Option<String>,
    pub name: Option<String>,
    /// By [`Table::PER_CAR`] position.
    pub tables: [Tally; 6],
    pub first_ms: Option<u64>,
    pub last_ms: Option<u64>,
    pub open_waiting: u64,
    pub warnings: Vec<String>,
}

impl CarTally {
    pub fn table(&mut self, t: Table) -> Option<&mut Tally> {
        t.car_ix().map(|i| &mut self.tables[i])
    }

    pub fn into_report(self) -> CarReport {
        CarReport {
            tm_car_id: self.tm_car_id,
            car: self.car,
            vin_prefix: self.vin_prefix,
            name: self.name,
            tables: Table::PER_CAR
                .iter()
                .zip(&self.tables)
                .filter(|(_, t)| !t.is_empty())
                .map(|(n, t)| (*n, t.report()))
                .collect(),
            first_ms: self.first_ms,
            last_ms: self.last_ms,
            open_waiting: self.open_waiting,
            warnings: self.warnings,
        }
    }
}

/// What the importer kept in memory to link tables, and what didn't fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Links {
    /// Addresses whose short name was kept, to name drive and charge ends.
    pub address_labels: u64,
    /// Addresses not kept ([`Options::max_address_bytes`](crate::Options)): their drives and charges have no
    /// address text (the address rows themselves are still handed over).
    pub address_labels_over: u64,
    /// Charges known (their car, AC/DC and highest power).
    pub charges: u64,
    /// Rows (charges and charge points) that found no room ([`Options::max_charges`](crate::Options)): those
    /// charges' points are left out, and the charges have no AC/DC or top power.
    pub charges_over: u64,
    /// Drive ends and charge places looked up among the positions.
    pub endpoints_wanted: u64,
    pub endpoints_found: u64,
    /// Wanted but not kept ([`Options::max_endpoints`](crate::Options)): the caller can find them by time.
    pub endpoints_over: u64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Report {
    pub compression: Compression,
    pub encoding: Encoding,
    pub packed_bytes: u64,
    pub unpacked_bytes: u64,
    pub tables: BTreeMap<Table, TableReport>,
    /// Tables of the backup passed over unread, with why (Tesla's tokens among them). Like the other lists here, at
    /// most 1000 entries, whatever the file holds.
    pub skipped_tables: BTreeMap<String, String>,
    /// Columns this importer doesn't know (a newer TeslaMate), per table: passed over.
    pub unknown_columns: BTreeMap<Table, Vec<String>>,
    /// Columns used here that the backup doesn't have (an older TeslaMate), per table: read as empty.
    pub missing_columns: BTreeMap<Table, Vec<String>>,
    /// Cars in the backup, and of them those the caller left out (each car's own report goes to the sink).
    pub cars: u64,
    pub cars_left_out: u64,
    pub first_ms: Option<u64>,
    pub last_ms: Option<u64>,
    pub links: Links,
    /// The most rows waiting at once to be handed over: bounded by
    /// [`Options::max_buffered_rows`](crate::Options) however many cars there are.
    pub peak_buffered_rows: usize,
    /// Each warning once, however often it happened.
    pub warnings: Vec<String>,
}

impl Report {
    pub fn table(&self, t: Table) -> TableReport {
        self.tables.get(&t).cloned().unwrap_or_default()
    }
}

pub(crate) fn widen(first: &mut Option<u64>, last: &mut Option<u64>, t: u64) {
    *first = Some(first.map_or(t, |f| f.min(t)));
    *last = Some(last.map_or(t, |l| l.max(t)));
}
