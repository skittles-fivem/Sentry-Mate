//! The rows of a car's time-indexed tables (drives, charges, state spans, updates) and how they are asked for.

use model::{CarId, Charge, Drive, Source, StateSpan, Update};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// A car's time-indexed tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Table {
    Drives,
    Charges,
    States,
    Updates,
}

impl Table {
    pub const ALL: [Table; 4] = [Table::Drives, Table::Charges, Table::States, Table::Updates];

    pub fn name(self) -> &'static str {
        match self {
            Table::Drives => "drives",
            Table::Charges => "charges",
            Table::States => "states",
            Table::Updates => "updates",
        }
    }

    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

/// Counts and totals of a car's drives and charges, kept up to date as rows come and go. Kept as integers (metres,
/// watt-hours) so adding a row and taking it away again gives back exactly what was there.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    pub drives: i64,
    pub charges: i64,
    pub distance_m: i64,
    /// Energy the drives used, Wh.
    pub drive_energy_wh: i64,
    /// Energy charges put into the pack, Wh.
    pub charge_added_wh: i64,
    /// Energy charges took from the grid, Wh (where known).
    pub charge_used_wh: i64,
}

fn milli(v: Option<f64>) -> i64 {
    v.filter(|v| v.is_finite()).map_or(0, |v| (v * 1000.0).round() as i64)
}

impl Totals {
    pub fn distance_km(&self) -> f64 {
        self.distance_m as f64 / 1000.0
    }
    pub fn drive_energy_kwh(&self) -> f64 {
        self.drive_energy_wh as f64 / 1000.0
    }
    pub fn charge_added_kwh(&self) -> f64 {
        self.charge_added_wh as f64 / 1000.0
    }
    pub fn charge_used_kwh(&self) -> f64 {
        self.charge_used_wh as f64 / 1000.0
    }

    fn add(&mut self, other: &Totals, sign: i64) {
        add(&mut self.drives, sign, other.drives);
        add(&mut self.charges, sign, other.charges);
        add(&mut self.distance_m, sign, other.distance_m);
        add(&mut self.drive_energy_wh, sign, other.drive_energy_wh);
        add(&mut self.charge_added_wh, sign, other.charge_added_wh);
        add(&mut self.charge_used_wh, sign, other.charge_used_wh);
    }
}

/// `to += sign × v`, wrapping: a row with an absurd value (a bad odometer jump) can't overflow and panic, and
/// adding it and taking it away again still gives back exactly what was there.
fn add(to: &mut i64, sign: i64, v: i64) {
    *to = to.wrapping_add(sign.wrapping_mul(v));
}

/// What a row adds to its car's summary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    pub can: Totals,
    pub teslamate: Totals,
    pub states: i64,
    pub updates: i64,
}

impl Tally {
    pub fn totals(&self, source: Source) -> &Totals {
        match source {
            Source::Can => &self.can,
            Source::Teslamate => &self.teslamate,
        }
    }

    pub(crate) fn totals_mut(&mut self, source: Source) -> &mut Totals {
        match source {
            Source::Can => &mut self.can,
            Source::Teslamate => &mut self.teslamate,
        }
    }

    pub(crate) fn add(&mut self, other: &Tally, sign: i64) {
        self.can.add(&other.can, sign);
        self.teslamate.add(&other.teslamate, sign);
        add(&mut self.states, sign, other.states);
        add(&mut self.updates, sign, other.updates);
    }

    /// How many rows of `table` it counts.
    pub(crate) fn rows(&self, table: Table) -> i64 {
        match table {
            Table::Drives => self.can.drives.wrapping_add(self.teslamate.drives),
            Table::Charges => self.can.charges.wrapping_add(self.teslamate.charges),
            Table::States => self.states,
            Table::Updates => self.updates,
        }
    }

    pub(crate) fn is_zero(&self) -> bool {
        *self == Tally::default()
    }

    /// Only what rows of `table` count for.
    pub fn part(&self, table: Table) -> Tally {
        let pick = |t: &Totals| match table {
            Table::Drives => Totals {
                drives: t.drives,
                distance_m: t.distance_m,
                drive_energy_wh: t.drive_energy_wh,
                ..Totals::default()
            },
            Table::Charges => Totals {
                charges: t.charges,
                charge_added_wh: t.charge_added_wh,
                charge_used_wh: t.charge_used_wh,
                ..Totals::default()
            },
            Table::States | Table::Updates => Totals::default(),
        };
        Tally {
            can: pick(&self.can),
            teslamate: pick(&self.teslamate),
            states: if table == Table::States { self.states } else { 0 },
            updates: if table == Table::Updates { self.updates } else { 0 },
        }
    }
}

pub(crate) mod sealed {
    /// Only the db's own row and point types: each has its file, fixed.
    pub trait Sealed {}
    impl Sealed for model::Drive {}
    impl Sealed for model::Charge {}
    impl Sealed for super::StateRow {}
    impl Sealed for super::UpdateRow {}
    impl Sealed for model::Sample {}
    impl Sealed for model::ChargePoint {}
}

/// A row of one of a car's tables. Ids come from the caller and are deterministic (the timeline makes them from
/// what the row is, an import from the source's own ids), so writing the same row again replaces it.
pub trait Item:
    sealed::Sealed + Clone + std::fmt::Debug + PartialEq + Serialize + DeserializeOwned + Send + Sync + 'static
{
    const TABLE: Table;
    fn id(&self) -> u64;
    fn car(&self) -> CarId;
    fn start_ms(&self) -> u64;
    /// Never before the start (an open-ended row ends where it starts).
    fn end_ms(&self) -> u64;
    fn source(&self) -> Source;
    /// Adds (sign 1) or takes away (sign -1) what this row counts for in its car's summary.
    fn tally(&self, t: &mut Tally, sign: i64);
}

impl Item for Drive {
    const TABLE: Table = Table::Drives;
    fn id(&self) -> u64 {
        self.id
    }
    fn car(&self) -> CarId {
        self.car
    }
    fn start_ms(&self) -> u64 {
        self.start_ms
    }
    fn end_ms(&self) -> u64 {
        self.end_ms.max(self.start_ms)
    }
    fn source(&self) -> Source {
        self.source
    }
    fn tally(&self, t: &mut Tally, sign: i64) {
        let x = t.totals_mut(self.source);
        add(&mut x.drives, sign, 1);
        add(&mut x.distance_m, sign, milli(self.distance_km));
        add(&mut x.drive_energy_wh, sign, milli(self.energy_used_kwh));
    }
}

impl Item for Charge {
    const TABLE: Table = Table::Charges;
    fn id(&self) -> u64 {
        self.id
    }
    fn car(&self) -> CarId {
        self.car
    }
    fn start_ms(&self) -> u64 {
        self.start_ms
    }
    fn end_ms(&self) -> u64 {
        self.end_ms.max(self.start_ms)
    }
    fn source(&self) -> Source {
        self.source
    }
    fn tally(&self, t: &mut Tally, sign: i64) {
        let x = t.totals_mut(self.source);
        add(&mut x.charges, sign, 1);
        add(&mut x.charge_added_wh, sign, milli(self.energy_added_kwh));
        add(&mut x.charge_used_wh, sign, milli(self.energy_used_kwh));
    }
}

/// A state span with its id (the model's span has none).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateRow {
    pub id: u64,
    pub span: StateSpan,
}

impl Item for StateRow {
    const TABLE: Table = Table::States;
    fn id(&self) -> u64 {
        self.id
    }
    fn car(&self) -> CarId {
        self.span.car
    }
    fn start_ms(&self) -> u64 {
        self.span.start_ms
    }
    fn end_ms(&self) -> u64 {
        self.span.end_ms.max(self.span.start_ms)
    }
    fn source(&self) -> Source {
        self.span.source
    }
    fn tally(&self, t: &mut Tally, sign: i64) {
        add(&mut t.states, sign, 1);
    }
}

/// A software update with its id (the model's update has none).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateRow {
    pub id: u64,
    pub update: Update,
}

impl Item for UpdateRow {
    const TABLE: Table = Table::Updates;
    fn id(&self) -> u64 {
        self.id
    }
    fn car(&self) -> CarId {
        self.update.car
    }
    fn start_ms(&self) -> u64 {
        self.update.start_ms
    }
    fn end_ms(&self) -> u64 {
        self.update.end_ms.unwrap_or(self.update.start_ms).max(self.update.start_ms)
    }
    fn source(&self) -> Source {
        self.update.source
    }
    fn tally(&self, t: &mut Tally, sign: i64) {
        add(&mut t.updates, sign, 1);
    }
}

/// Newest first (the lists in the UI) or oldest first (the timeline).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Order {
    #[default]
    NewestFirst,
    OldestFirst,
}

/// Where a page ended: the start and id of its last row. Written as `<start_ms>.<id>` for URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cursor {
    pub start_ms: u64,
    pub id: u64,
}

impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.start_ms, self.id)
    }
}

impl std::str::FromStr for Cursor {
    type Err = crate::Error;
    fn from_str(s: &str) -> Result<Cursor, crate::Error> {
        let bad = || crate::Error::invalid("a cursor is <start_ms>.<id>");
        let (a, b) = s.split_once('.').ok_or_else(bad)?;
        Ok(Cursor { start_ms: a.parse().map_err(|_| bad())?, id: b.parse().map_err(|_| bad())? })
    }
}

/// At most this many rows in a page.
pub const MAX_PAGE: usize = 10_000;

/// Rows that overlap `[from_ms, to_ms)` (an open-ended row counts where it starts), a page at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub from_ms: u64,
    pub to_ms: u64,
    pub order: Order,
    /// At most [`MAX_PAGE`].
    pub limit: usize,
    /// Continue after this row (the `next` of the page before).
    pub after: Option<Cursor>,
    /// Only rows from this source.
    pub source: Option<Source>,
}

impl Default for Query {
    fn default() -> Query {
        Query { from_ms: 0, to_ms: u64::MAX, order: Order::NewestFirst, limit: 100, after: None, source: None }
    }
}

impl Query {
    pub fn between(from_ms: u64, to_ms: u64) -> Query {
        Query { from_ms, to_ms, ..Query::default() }
    }
    pub fn oldest_first(mut self) -> Query {
        self.order = Order::OldestFirst;
        self
    }
    pub fn limit(mut self, n: usize) -> Query {
        self.limit = n;
        self
    }
    pub fn after(mut self, c: Option<Cursor>) -> Query {
        self.after = c;
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Pass as [`Query::after`] for the next page; `None` when this was the last.
    pub next: Option<Cursor>,
}
