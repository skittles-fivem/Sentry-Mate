//! TeslaMate imports for `/imports`: a check of a whole backup that stores nothing (which cars it holds, what each
//! brings, what a car here would take), the car suggested for each by its VIN prefix, and the import itself into the
//! engine's db ([`crate::importer::DbSink`]) with every car matched.
//!
//! The backup is read through `open` (called twice per run, as [`import::import`] needs); `stop` ends a run at the
//! next read (the reader answers an error), so a discarded job stops reading at once.

use super::{QResult, QueryError};
use crate::Engine;
use crate::importer::{DbSink, Match, import_state};
use import::{Batch, CarReport, Report, SinkError, Table, Target, TmCar};
use model::CarId;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// What the backup holds for one of its cars.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct BackupCar {
    pub tm_car: i64,
    pub name: Option<String>,
    pub vin_prefix: Option<String>,
    pub drives: u64,
    pub charges: u64,
    pub first_ms: Option<u64>,
    pub last_ms: Option<u64>,
    /// The car here with the same VIN prefix (the only one, and suggested for no other car of the backup), else
    /// `None`.
    pub suggested: Option<CarId>,
}

/// A check's result: the cars, and the report as JSON (`imported` 0 everywhere: nothing was stored).
#[derive(Debug, Clone)]
pub struct Checked {
    pub cars: Vec<BackupCar>,
    pub report: Value,
}

/// Where each car of the backup goes, by TeslaMate id: `Some(car)`, `None` = a new car; left-out cars are absent
/// from the map and listed in `leave_out`.
#[derive(Debug, Clone, Default)]
pub struct Matches {
    pub to: HashMap<i64, Option<CarId>>,
    pub leave_out: Vec<i64>,
}

/// Reads through `inner`, counting the bytes into `read` and failing once `stop` is set.
pub struct Counted<R> {
    inner: R,
    read: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.stop.load(Ordering::Relaxed) {
            return Err(io::Error::other("the import was discarded"));
        }
        let n = self.inner.read(buf)?;
        self.read.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// `open` with its reads counted (both reads of a run add up: a run reads the file twice) and `stop` obeyed.
pub fn counted<R: Read>(
    mut open: impl FnMut() -> io::Result<R>,
    read: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
) -> impl FnMut() -> io::Result<Counted<R>> {
    move || Ok(Counted { inner: open()?, read: read.clone(), stop: stop.clone() })
}

/// The one car here with this VIN prefix.
pub fn suggest(e: &Engine, vin_prefix: Option<&str>) -> QResult<Option<CarId>> {
    let Some(p) = vin_prefix.filter(|p| !p.is_empty()) else { return Ok(None) };
    // (the fleet search reads names and keys, not VINs: every car is looked at, from the fleet list alone)
    let mut q = db::FleetQuery { search: None, limit: 500, after: None };
    let mut same = Vec::new();
    for _ in 0..100_000 {
        let page = e.db().fleet(&q)?;
        same.extend(
            page.cars.iter().filter(|x| x.car.vin.as_ref().is_some_and(|v| v.vin_prefix == p)).map(|x| x.car.id),
        );
        match page.next {
            Some(n) => q.after = Some(n),
            None => break,
        }
    }
    Ok(if same.len() == 1 { same.first().copied() } else { None })
}

/// The sink of a check: every car taken (under a stand-in id), its rows counted and dropped.
struct CountSink<'a> {
    engine: &'a Engine,
    stand_in: HashMap<CarId, i64>,
    cars: Vec<BackupCar>,
    reports: Vec<CarReport>,
}

/// Stand-in ids of a check's cars: from the top, far from any real car's.
const STAND_IN: CarId = 0xFF00_0000;

fn sink_err(e: impl std::fmt::Display) -> SinkError {
    e.to_string().into()
}

impl import::Sink for CountSink<'_> {
    fn car(&mut self, tm: &TmCar) -> Result<Option<Target>, SinkError> {
        let prefix = tm.vin.as_ref().map(|v| v.vin_prefix.clone()).filter(|p| !p.is_empty());
        let suggested = suggest(self.engine, prefix.as_deref()).map_err(sink_err)?;
        let id = STAND_IN + self.cars.len() as CarId;
        self.stand_in.insert(id, tm.tm_id);
        self.cars.push(BackupCar {
            tm_car: tm.tm_id,
            name: tm.name.clone().filter(|n| !n.trim().is_empty()),
            vin_prefix: prefix,
            drives: 0,
            charges: 0,
            first_ms: None,
            last_ms: None,
            suggested,
        });
        // what an earlier import of the suggested car took is not counted again
        let state = match suggested {
            Some(c) => import_state(self.engine.db(), c).map_err(sink_err)?.unwrap_or_default(),
            None => Default::default(),
        };
        Ok(Some(Target { car: id, state }))
    }

    fn batch(&mut self, car: CarId, batch: Batch) -> Result<(), SinkError> {
        let Some(tm) = self.stand_in.get(&car).copied() else { return Ok(()) };
        let Some(c) = self.cars.iter_mut().find(|c| c.tm_car == tm) else { return Ok(()) };
        match &batch {
            Batch::Drives(v) => c.drives += v.len() as u64,
            Batch::Charges(v) => c.charges += v.len() as u64,
            _ => {}
        }
        Ok(())
    }

    fn places(&mut self, _rows: Vec<import::PlaceRow>) -> Result<(), SinkError> {
        Ok(())
    }

    fn addresses(&mut self, _rows: Vec<import::AddressRow>) -> Result<(), SinkError> {
        Ok(())
    }

    fn car_done(&mut self, report: CarReport, _state: Option<import::ImportState>) -> Result<(), SinkError> {
        if let Some(c) = self.cars.iter_mut().find(|c| c.tm_car == report.tm_car_id) {
            c.first_ms = report.first_ms;
            c.last_ms = report.last_ms;
        }
        self.reports.push(report);
        Ok(())
    }
}

/// A failure of a run: `(code, message)` for the job's `error`.
pub fn error_of(e: &import::Error) -> (&'static str, String) {
    let code = match e {
        import::Error::Io(_) => "read_failed",
        import::Error::Unpack(_) | import::Error::Unsupported(_) | import::Error::Utf16Be => "unreadable",
        import::Error::CustomFormat => "custom_format",
        import::Error::NotTeslamate => "not_a_backup",
        import::Error::Incomplete(_) => "incomplete",
        import::Error::Changed => "changed",
        import::Error::StateVersion(_) => "state_version",
        import::Error::Sink(s) if s.to_string().starts_with("cars_unmatched") => "cars_unmatched",
        import::Error::Sink(_) => "store_failed",
    };
    (code, e.to_string())
}

fn skipped(t: &import::TableReport) -> Map<String, Value> {
    t.skipped
        .iter()
        .map(|(k, n)| {
            let word = serde_json::to_value(k).ok().and_then(|v| v.as_str().map(|s| s.replace('-', "_")));
            (word.unwrap_or_else(|| format!("{k:?}")), json!(n))
        })
        .collect()
}

/// A car's own figures over every table.
fn car_figures(r: &CarReport, imported: bool) -> (u64, u64, Map<String, Value>) {
    let (mut read, mut took) = (0u64, 0u64);
    let mut sk: HashMap<String, u64> = HashMap::new();
    for t in r.tables.values() {
        read += t.read;
        took += t.imported;
        for (k, v) in skipped(t) {
            *sk.entry(k).or_default() += v.as_u64().unwrap_or(0);
        }
    }
    (read, if imported { took } else { 0 }, sk.into_iter().map(|(k, v)| (k, json!(v))).collect())
}

/// The report of a run as the API writes it. `cars` with their match (`"c1"`, `"new"`, `"leave_out"` or null);
/// a car left out of an import that ran has its counts null.
pub fn report_json(r: &Report, reports: &[CarReport], cars: &[(BackupCar, Value)], imported: bool) -> Value {
    let mut tables = Map::new();
    for (t, tr) in &r.tables {
        tables.insert(
            t.name().to_string(),
            json!({
                "read": tr.read,
                "imported": if imported { tr.imported } else { 0 },
                "skipped": skipped(tr),
                "bad_values": tr.bad_values,
            }),
        );
    }
    for t in [Table::Cars, Table::Drives, Table::ChargingProcesses, Table::Positions, Table::Updates, Table::Geofences]
    {
        tables
            .entry(t.name().to_string())
            .or_insert_with(|| json!({ "read": 0, "imported": 0, "skipped": {}, "bad_values": {} }));
    }
    let per_car: Vec<Value> = cars
        .iter()
        .map(|(c, m)| {
            let own = reports.iter().find(|x| x.tm_car_id == c.tm_car);
            let left_out = imported && m.as_str() == Some("leave_out");
            let (read, took, sk) = match own {
                Some(o) if !left_out => {
                    let (a, b, s) = car_figures(o, imported);
                    (json!(a), json!(b), Value::Object(s))
                }
                _ if left_out => (Value::Null, Value::Null, Value::Null),
                _ => (json!(0), json!(0), json!({})),
            };
            json!({ "tm_car": c.tm_car.to_string(), "name": c.name, "match": m, "drives": c.drives,
                    "charges": c.charges, "read": read, "imported": took, "skipped": sk })
        })
        .collect();
    json!({
        "tables": tables,
        "span": { "from_ms": r.first_ms, "to_ms": r.last_ms },
        "cars": per_car,
        "imported": imported,
        "compression": serde_json::to_value(r.compression).unwrap_or(Value::Null),
        "unpacked_bytes": r.unpacked_bytes,
        "unknown_columns": r.unknown_columns.iter().map(|(t, c)| (t.name().to_string(), json!(c))).collect::<Map<_, _>>(),
        "missing_columns": r.missing_columns.iter().map(|(t, c)| (t.name().to_string(), json!(c))).collect::<Map<_, _>>(),
        "warnings": r.warnings,
    })
}

/// A car here is suggested for one car of the backup at most: two cars of the same model, year and plant share a VIN
/// prefix, and both going into one car would mix two histories. When several share a suggestion, none keeps it.
pub fn one_each(cars: &mut [BackupCar]) {
    let mut n: HashMap<CarId, usize> = HashMap::new();
    for c in cars.iter() {
        if let Some(s) = c.suggested {
            *n.entry(s).or_default() += 1;
        }
    }
    for c in cars.iter_mut() {
        if c.suggested.is_some_and(|s| n.get(&s).copied().unwrap_or(0) > 1) {
            c.suggested = None;
        }
    }
}

/// Checks a whole backup: reads it as the import would and stores nothing.
pub fn check<R: Read>(e: &Engine, open: impl FnMut() -> io::Result<R>) -> Result<Checked, import::Error> {
    let mut sink = CountSink { engine: e, stand_in: HashMap::new(), cars: Vec::new(), reports: Vec::new() };
    let report = import::import(open, &mut sink, &import::Options::default())?;
    one_each(&mut sink.cars);
    let pairs: Vec<(BackupCar, Value)> = sink.cars.iter().map(|c| (c.clone(), Value::Null)).collect();
    let json = report_json(&report, &sink.reports, &pairs, false);
    Ok(Checked { cars: sink.cars, report: json })
}

/// What an import run did.
#[derive(Debug, Clone)]
pub struct Imported {
    pub report: Report,
    pub cars: Vec<CarReport>,
    /// Cars the import made.
    pub made: Vec<CarId>,
}

/// Imports a backup with every car matched (`currency`: what imported charge costs are in, when known).
pub fn run<R: Read>(
    e: &Engine,
    open: impl FnMut() -> io::Result<R>,
    m: &Matches,
    currency: Option<String>,
) -> Result<Imported, import::Error> {
    let mut sink = DbSink::new(e);
    for (tm, to) in &m.to {
        sink.matches.insert(*tm, to.map_or(Match::New, Match::Car));
    }
    for tm in &m.leave_out {
        sink.matches.insert(*tm, Match::LeaveOut);
    }
    sink.currency = currency;
    let r = import::import(open, &mut sink, &import::Options::default());
    sink.finish();
    let report = r?;
    Ok(Imported { report, cars: std::mem::take(&mut sink.reports), made: std::mem::take(&mut sink.made) })
}

/// A car of the backup may be matched to this car (it exists).
pub fn car_exists(e: &Engine, car: CarId) -> QResult<bool> {
    Ok(e.db().car(car)?.is_some())
}

/// For the error answer of a query that is not about a car.
pub fn no_such_import() -> QueryError {
    QueryError::NotFound("no_such_import")
}
