//! The second read: every row of the tables taken, mapped, and handed to the sink per car in batches as it is read.
//!
//! Rows of different cars are mixed in the backup (TeslaMate numbers them in the order they happened, every car on
//! one counter), so each car gathers its own batch. To keep memory flat with any number of cars, all batches are
//! handed over together whenever the rows waiting reach [`Options::max_buffered_rows`]: with thousands of cars each
//! batch is then smaller, never the memory larger. A car's rows still come in the backup's order.

use crate::copy::{CopyHeader, Handler, RawRow, Take};
use crate::links::{AddressBook, Ends, Procs, Route, Routes};
use crate::map::{self, ChargeCtx, DriveCtx, Mapped};
use crate::report::{CarTally, Links, Report, Skip, TableReport, Tally, widen};
use crate::rows::{
    AddressRow, Batch, ChargePointRow, ChargeRow, DriveRow, PlaceRow, PositionRow, Sink, StateRow, UpdateRow,
};
use crate::schema::{Addr, Car, CarSetting, Chg, Class, ColumnMap, Cp, Drv, Geo, Pos, Row, St, Table, Upd, classify};
use crate::state::ImportState;
use crate::{Error, Options};
use std::collections::HashSet;

/// The most entries of each list in the report (warnings, tables passed over, unknown columns per table): a TeslaMate
/// backup has a handful, and a damaged or made-up file can't make the report grow past this.
const MAX_LISTED: usize = 1000;

#[derive(Default)]
struct Bufs {
    positions: Vec<PositionRow>,
    drives: Vec<DriveRow>,
    charges: Vec<ChargeRow>,
    points: Vec<ChargePointRow>,
    states: Vec<StateRow>,
    updates: Vec<UpdateRow>,
    dirty: bool,
}

/// A row type that waits in a car's batch.
trait Buffered: Sized {
    const TABLE: Table;
    fn waiting(b: &mut Bufs) -> &mut Vec<Self>;
}

macro_rules! buffered {
    ($($t:ty => $table:ident . $field:ident),* $(,)?) => {
        $(impl Buffered for $t {
            const TABLE: Table = Table::$table;
            fn waiting(b: &mut Bufs) -> &mut Vec<Self> {
                &mut b.$field
            }
        })*
    };
}

buffered!(
    PositionRow => Positions.positions,
    DriveRow => Drives.drives,
    ChargeRow => ChargingProcesses.charges,
    ChargePointRow => Charges.points,
    StateRow => States.states,
    UpdateRow => Updates.updates,
);

impl Bufs {
    fn take(&mut self, t: Table) -> Option<Batch> {
        fn some<T>(v: &mut Vec<T>, f: fn(Vec<T>) -> Batch) -> Option<Batch> {
            (!v.is_empty()).then(|| f(std::mem::take(v)))
        }
        match t {
            Table::Positions => some(&mut self.positions, Batch::Positions),
            Table::Drives => some(&mut self.drives, Batch::Drives),
            Table::ChargingProcesses => some(&mut self.charges, Batch::Charges),
            Table::Charges => some(&mut self.points, Batch::ChargePoints),
            Table::States => some(&mut self.states, Batch::States),
            Table::Updates => some(&mut self.updates, Batch::Updates),
            Table::Cars | Table::CarSettings | Table::Addresses | Table::Geofences => None,
        }
    }
}

pub(crate) struct Emit<'s> {
    sink: &'s mut dyn Sink,
    batch_rows: usize,
    max_buffered: usize,
    max_open: usize,
    max_open_total: usize,
    /// Open rows remembered so far, all cars.
    open_total: usize,
    routes: Routes,
    labels: AddressBook,
    procs: Procs,
    ends: Ends,
    report: Report,
    bufs: Vec<Bufs>,
    dirty: Vec<usize>,
    buffered: usize,
    places: Vec<PlaceRow>,
    addresses: Vec<AddressRow>,
    current: Option<ColumnMap>,
    notes: Vec<&'static str>,
}

/// Where a row of a car goes, once it is let in.
struct Admitted {
    slot: usize,
    report: usize,
    efficiency: Option<f64>,
}

impl<'s> Emit<'s> {
    pub fn new(
        sink: &'s mut dyn Sink,
        o: &Options,
        routes: Routes,
        labels: AddressBook,
        procs: Procs,
        ends: Ends,
    ) -> Self {
        let bufs = (0..routes.slots.len()).map(|_| Bufs::default()).collect();
        Emit {
            sink,
            batch_rows: o.batch_rows.max(1),
            max_buffered: o.max_buffered_rows.max(1),
            max_open: o.max_open,
            max_open_total: o.max_open_total,
            open_total: 0,
            routes,
            labels,
            procs,
            ends,
            report: Report::default(),
            bufs,
            dirty: Vec::new(),
            buffered: 0,
            places: Vec::new(),
            addresses: Vec::new(),
            current: None,
            notes: Vec::new(),
        }
    }

    /// A warning for the whole import, once however often it happens.
    fn warn(&mut self, w: String) {
        let ws = &mut self.report.warnings;
        if ws.len() < MAX_LISTED && !ws.contains(&w) {
            ws.push(w);
        }
    }

    fn passed_over(&mut self, header: &CopyHeader, why: &str) {
        let tables = &mut self.report.skipped_tables;
        if tables.len() < MAX_LISTED {
            tables.insert(header.full_name(), why.into());
        }
    }

    fn table_report(&mut self, t: Table) -> &mut TableReport {
        self.report.tables.entry(t).or_default()
    }

    fn car_table(&mut self, report: usize, t: Table) -> Option<&mut Tally> {
        self.routes.reports.get_mut(report).and_then(|r| r.table(t))
    }

    /// Counts a row left out, for the table and (when known) its car.
    fn skip(&mut self, t: Table, report: Option<usize>, why: Skip, column: Option<&str>) {
        let tr = self.table_report(t);
        tr.skip(why);
        if let Some(c) = column {
            tr.bad(c);
        }
        if let Some(cr) = report.and_then(|r| self.car_table(r, t)) {
            cr.skip(why);
        }
    }

    /// Lets a row of a car in, or counts why not. Every row of a car goes through the car's marks here.
    fn admit(&mut self, t: Table, id: i64, tm_car: Option<i64>, time: Option<u64>) -> Option<Admitted> {
        let Some(tm_car) = tm_car else {
            let why = if t == Table::Charges { Skip::UnknownCharge } else { Skip::UnknownCar };
            self.skip(t, None, why, None);
            return None;
        };
        match self.routes.by_tm.get(&tm_car).copied() {
            None => {
                self.skip(t, None, Skip::UnknownCar, None);
                None
            }
            Some(Route::LeftOut { report }) => {
                if let Some(cr) = self.car_table(report, t) {
                    cr.read += 1;
                }
                self.skip(t, Some(report), Skip::CarLeftOut, None);
                None
            }
            Some(Route::To { slot, report, efficiency }) => {
                if let Some(cr) = self.car_table(report, t) {
                    cr.read += 1;
                }
                let s = &mut self.routes.slots[slot];
                let old = s.state.tables.get(&t);
                if let Some(i) = t.car_ix() {
                    s.work[i].see(old, id, time);
                }
                if s.state.seen(t, id) {
                    self.skip(t, Some(report), Skip::AlreadyImported, None);
                    return None;
                }
                Some(Admitted { slot, report, efficiency })
            }
        }
    }

    /// What became of a let-in row (with the first and last time it covers, when taken).
    fn settle<T: Buffered>(
        &mut self,
        t: Table,
        id: i64,
        at: &Admitted,
        row: &Row<'_, '_>,
        out: Mapped<(T, (u64, u64))>,
    ) -> Result<(), Error> {
        let notes = std::mem::take(&mut self.notes);
        match out {
            Mapped::Take((o, (first, last))) => {
                let bad: Vec<&'static str> = row.bad_columns().collect();
                let count = |tr: &mut TableReport| {
                    tr.imported += 1;
                    bad.iter().for_each(|c| tr.bad(c));
                    notes.iter().for_each(|n| tr.note(n));
                };
                count(self.report.tables.entry(t).or_default());
                if let Some(cr) = self.routes.reports.get_mut(at.report) {
                    if let Some(tally) = cr.table(t) {
                        tally.imported += 1;
                    }
                    for tm in [first, last] {
                        widen(&mut cr.first_ms, &mut cr.last_ms, tm);
                        widen(&mut self.report.first_ms, &mut self.report.last_ms, tm);
                    }
                }
                self.push(at.slot, o)?;
            }
            Mapped::Open => {
                if let Some(w) = self.routes.slots[at.slot].work(t)
                    && w.still_open(id, self.max_open, self.open_total < self.max_open_total)
                {
                    self.open_total += 1;
                }
                self.skip(t, Some(at.report), Skip::Open, None);
            }
            Mapped::Bad(col) => self.skip(t, Some(at.report), Skip::BadValue, Some(col)),
        }
        Ok(())
    }

    fn push<T: Buffered>(&mut self, slot: usize, row: T) -> Result<(), Error> {
        let b = &mut self.bufs[slot];
        let waiting = T::waiting(b);
        waiting.push(row);
        let n = waiting.len();
        if !b.dirty {
            b.dirty = true;
            self.dirty.push(slot);
        }
        self.buffered += 1;
        self.report.peak_buffered_rows = self.report.peak_buffered_rows.max(self.buffered + self.globals());
        if n >= self.batch_rows {
            self.flush(slot, T::TABLE)?;
        }
        if self.buffered + self.globals() > self.max_buffered {
            self.flush_all()?;
        }
        Ok(())
    }

    fn globals(&self) -> usize {
        self.places.len() + self.addresses.len()
    }

    fn flush(&mut self, slot: usize, t: Table) -> Result<(), Error> {
        if let Some(batch) = self.bufs[slot].take(t) {
            self.buffered -= batch.len();
            let car = self.routes.slots[slot].car;
            self.sink.batch(car, batch).map_err(Error::Sink)?;
        }
        Ok(())
    }

    fn flush_all(&mut self) -> Result<(), Error> {
        for slot in std::mem::take(&mut self.dirty) {
            for t in Table::PER_CAR {
                self.flush(slot, t)?;
            }
            self.bufs[slot].dirty = false;
        }
        self.flush_globals()
    }

    fn flush_globals(&mut self) -> Result<(), Error> {
        if !self.places.is_empty() {
            self.sink.places(std::mem::take(&mut self.places)).map_err(Error::Sink)?;
        }
        if !self.addresses.is_empty() {
            self.sink.addresses(std::mem::take(&mut self.addresses)).map_err(Error::Sink)?;
        }
        Ok(())
    }

    fn push_global(&mut self, row: Result<PlaceRow, AddressRow>) -> Result<(), Error> {
        let n = match row {
            Ok(p) => {
                self.places.push(p);
                self.places.len()
            }
            Err(a) => {
                self.addresses.push(a);
                self.addresses.len()
            }
        };
        self.report.peak_buffered_rows = self.report.peak_buffered_rows.max(self.buffered + self.globals());
        if n >= self.batch_rows || self.buffered + self.globals() > self.max_buffered {
            self.flush_globals()?;
        }
        Ok(())
    }

    /// A row's id (or another id it needs), or the row is counted as unreadable.
    fn id(&mut self, t: Table, v: Result<i64, &'static str>) -> Option<i64> {
        match v {
            Ok(id) => Some(id),
            Err(c) => {
                self.skip(t, None, Skip::BadValue, Some(c));
                None
            }
        }
    }

    fn row_of(&mut self, map: &ColumnMap, raw: &RawRow<'_>) -> Result<(), Error> {
        let t = map.table;
        self.table_report(t).read += 1;
        if raw.fields() != map.width {
            self.skip(t, None, Skip::WrongFieldCount, None);
            return Ok(());
        }
        let row = map.row(raw);
        match t {
            Table::Cars => {
                let Some(id) = self.id(t, row.req_i64(Car::id)) else { return Ok(()) };
                match self.routes.by_tm.get(&id) {
                    Some(Route::To { .. }) => self.table_report(t).imported += 1,
                    Some(Route::LeftOut { .. }) => self.table_report(t).skip(Skip::CarLeftOut),
                    // A second cars table: its cars weren't asked about.
                    None => self.table_report(t).skip(Skip::UnknownCar),
                }
            }
            Table::CarSettings => match row.req_i64(CarSetting::id) {
                Ok(_) => self.table_report(t).imported += 1,
                Err(c) => self.skip(t, None, Skip::BadValue, Some(c)),
            },
            Table::Addresses => match row.req_i64(Addr::id) {
                Ok(id) => {
                    let a = map::address(&row, id);
                    let bad: Vec<_> = row.bad_columns().collect();
                    let tr = self.table_report(t);
                    tr.imported += 1;
                    bad.iter().for_each(|c| tr.bad(c));
                    self.push_global(Err(a))?;
                }
                Err(c) => self.skip(t, None, Skip::BadValue, Some(c)),
            },
            Table::Geofences => {
                let Some(id) = self.id(t, row.req_i64(Geo::id)) else { return Ok(()) };
                let mut notes = Vec::new();
                match map::place(&row, id, &mut notes) {
                    Ok(p) => {
                        let tr = self.table_report(t);
                        tr.imported += 1;
                        notes.iter().for_each(|n| tr.note(n));
                        self.push_global(Ok(p))?;
                    }
                    Err(c) => self.skip(t, None, Skip::BadValue, Some(c)),
                }
            }
            Table::Positions => {
                let Some(id) = self.id(t, row.req_i64(Pos::id)) else { return Ok(()) };
                if self.ends.wants(id)
                    && let Some(p) = map::endpoint(&row)
                {
                    self.ends.fill(id, &p);
                }
                let Some(at) = self.admit(t, id, row.i64(Pos::car_id), row.ms(Pos::date)) else { return Ok(()) };
                let out = map::position(&row, id).map(|p| {
                    let t = p.sample.t_ms;
                    (p, (t, t))
                });
                self.settle(t, id, &at, &row, out)?;
            }
            Table::Drives => {
                let Some(id) = self.id(t, row.req_i64(Drv::id)) else { return Ok(()) };
                let Some(at) = self.admit(t, id, row.i64(Drv::car_id), row.ms(Drv::start_date)) else { return Ok(()) };
                let car = self.routes.slots[at.slot].car;
                let cx = DriveCtx { labels: &self.labels, ends: &self.ends, efficiency: at.efficiency };
                let out = map::drive(&row, id, car, &cx, &mut self.notes).map(|d| {
                    let span = (d.drive.start_ms, d.drive.end_ms);
                    (d, span)
                });
                self.settle(t, id, &at, &row, out)?;
            }
            Table::ChargingProcesses => {
                let Some(id) = self.id(t, row.req_i64(Cp::id)) else { return Ok(()) };
                let Some(at) = self.admit(t, id, row.i64(Cp::car_id), row.ms(Cp::start_date)) else { return Ok(()) };
                let car = self.routes.slots[at.slot].car;
                let cx = ChargeCtx {
                    labels: &self.labels,
                    ends: &self.ends,
                    proc: self.procs.get(id),
                    newest_ms: self.procs.newest_of(row.i64(Cp::car_id)),
                };
                let out = map::charge(&row, id, car, &cx, &mut self.notes).map(|c| {
                    let span = (c.charge.start_ms, c.charge.end_ms);
                    (c, span)
                });
                self.settle(t, id, &at, &row, out)?;
            }
            Table::Charges => {
                let Some(id) = self.id(t, row.req_i64(Chg::id)) else { return Ok(()) };
                let Some(cp) = self.id(t, row.req_i64(Chg::charging_process_id)) else { return Ok(()) };
                let tm_car = self.procs.get(cp).and_then(|p| p.car());
                let Some(at) = self.admit(t, id, tm_car, row.ms(Chg::date)) else { return Ok(()) };
                let out = map::charge_point(&row, id, cp).map(|p| {
                    let t = p.point.t_ms;
                    (p, (t, t))
                });
                self.settle(t, id, &at, &row, out)?;
            }
            Table::States => {
                let Some(id) = self.id(t, row.req_i64(St::id)) else { return Ok(()) };
                let Some(at) = self.admit(t, id, row.i64(St::car_id), row.ms(St::start_date)) else { return Ok(()) };
                let car = self.routes.slots[at.slot].car;
                let out = map::state(
                    &row,
                    id,
                    car,
                    row.i64(St::car_id).and_then(|c| self.procs.last_state.get(&c).copied()).unwrap_or(0),
                    &mut self.notes,
                )
                .map(|s| {
                    let span = (s.span.start_ms, s.span.end_ms);
                    (s, span)
                });
                self.settle(t, id, &at, &row, out)?;
            }
            Table::Updates => {
                let Some(id) = self.id(t, row.req_i64(Upd::id)) else { return Ok(()) };
                let Some(at) = self.admit(t, id, row.i64(Upd::car_id), row.ms(Upd::start_date)) else { return Ok(()) };
                let car = self.routes.slots[at.slot].car;
                let out = map::update(&row, id, car).map(|u| {
                    let span = (u.update.start_ms, u.update.end_ms.unwrap_or(u.update.start_ms));
                    (u, span)
                });
                self.settle(t, id, &at, &row, out)?;
            }
        }
        Ok(())
    }

    /// Hands over what is left, then each car's report and new marks, one car at a time.
    pub fn finish(mut self) -> Result<Report, Error> {
        self.flush_all()?;
        let mut reports: Vec<Option<CarTally>> =
            std::mem::take(&mut self.routes.reports).into_iter().map(Some).collect();
        let mut report = std::mem::take(&mut self.report);
        report.cars = reports.len() as u64;
        report.cars_left_out = reports.iter().flatten().filter(|r| r.car.is_none()).count() as u64;
        for slot in std::mem::take(&mut self.routes.slots) {
            let Some(mut r) = reports.get_mut(slot.report).and_then(Option::take) else { continue };
            let mut state = ImportState {
                version: slot.state.version,
                tables: Default::default(),
                teslafi: slot.state.teslafi.clone(),
            };
            for (t, w) in Table::PER_CAR.into_iter().zip(slot.work) {
                let old = slot.state.tables.get(&t);
                let mismatch = w.other_database(old);
                let (mark, change) = w.finish(old, self.max_open);
                r.open_waiting += mark.open.len() as u64;
                if mismatch {
                    r.warnings.push(format!(
                        "{}: rows have other times than rows with the same ids imported before: is this another \
                         TeslaMate database? Rows with ids imported before were left out.",
                        t.name()
                    ));
                }
                if change.gone > 0 {
                    r.warnings.push(format!(
                        "{}: {} rows that had no end yet are no longer in the backup (deleted in TeslaMate)",
                        t.name(),
                        change.gone
                    ));
                }
                if change.forgotten > 0 {
                    r.warnings.push(format!(
                        "{}: {} of the oldest rows without an end are no longer waited for",
                        t.name(),
                        change.forgotten
                    ));
                }
                if old.is_some() || mark.high_id != 0 || !mark.open.is_empty() {
                    state.tables.insert(t, mark);
                }
            }
            self.sink.car_done(r.into_report(), Some(state)).map_err(Error::Sink)?;
        }
        // Cars left out, and further TeslaMate cars that were the same car as an earlier one.
        for r in reports.into_iter().flatten() {
            self.sink.car_done(r.into_report(), None).map_err(Error::Sink)?;
        }
        report.links = Links {
            address_labels: self.labels.len() as u64,
            address_labels_over: self.labels.over,
            charges: self.procs.len() as u64,
            charges_over: self.procs.over,
            endpoints_wanted: self.ends.wanted() as u64,
            endpoints_found: self.ends.found() as u64,
            endpoints_over: self.ends.over,
        };
        Ok(report)
    }
}

impl Handler for Emit<'_> {
    type Error = Error;

    fn begin(&mut self, header: &CopyHeader) -> Result<Take, Error> {
        self.current = None;
        let t = match classify(header) {
            Class::Ours(t) => t,
            Class::Secret => {
                self.passed_over(header, "Tesla's tokens: never read");
                return Ok(Take::Skip);
            }
            Class::Other => {
                self.passed_over(header, "not used");
                return Ok(Take::Skip);
            }
        };
        let map = ColumnMap::new(t, header);
        for (list, columns) in [
            (&mut self.report.unknown_columns, map.unknown.iter().map(String::as_str).collect::<Vec<_>>()),
            (&mut self.report.missing_columns, map.missing.clone()),
        ] {
            if columns.is_empty() {
                continue;
            }
            let l = list.entry(t).or_default();
            let mut listed: HashSet<String> = l.iter().cloned().collect();
            for c in columns {
                if l.len() >= MAX_LISTED {
                    break;
                }
                if listed.insert(c.to_string()) {
                    l.push(c.to_string());
                }
            }
        }
        if !map.missing_required.is_empty() {
            self.warn(format!(
                "{} can't be read: it has no {} column",
                header.full_name(),
                map.missing_required.join(", ")
            ));
            return Ok(Take::Skip);
        }
        self.table_report(t);
        self.current = Some(map);
        Ok(Take::Rows)
    }

    fn row(&mut self, raw: &RawRow<'_>) -> Result<(), Error> {
        let Some(map) = self.current.take() else { return Ok(()) };
        let r = self.row_of(&map, raw);
        self.current = Some(map);
        r
    }

    fn too_long(&mut self) {
        if let Some(t) = self.current.as_ref().map(|m| m.table) {
            let tr = self.table_report(t);
            tr.read += 1;
            tr.skip(Skip::TooLong);
        }
    }

    fn end(&mut self) -> Result<(), Error> {
        // A table's rows are all handed over before the next table's.
        if self.current.take().is_some() {
            self.flush_all()?;
        }
        Ok(())
    }

    fn unreadable(&mut self) {
        self.warn("a COPY line couldn't be read: its rows were passed over".into());
    }
}
