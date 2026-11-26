//! TeslaMate and TeslaFi imports for `/imports`: a check of a whole file that stores nothing (which cars it holds,
//! what each brings, what a car here would take), the car suggested for each by its VIN prefix, and the import itself
//! into the engine's db ([`crate::importer::DbSink`]) with every car matched.
//!
//! A TeslaMate backup is read through `open` (called twice per run, as [`import::import`] needs); TeslaFi's export
//! once per pass ([`import::teslafi::import`], [`teslafi_check`], [`teslafi_run`]). `stop` ends a run at the next read
//! (the reader answers an error), so a discarded job stops reading at once.

use super::{QResult, QueryError};
use crate::Engine;
use crate::importer::{DbSink, Match, import_state};
use import::{Batch, CarReport, Report, SinkError, Table, Target, TmCar};
use model::CarId;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, HashMap};
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
    /// The car here with its whole VIN, else the one with the same VIN prefix whose whole VIN is not known to be
    /// another (and suggested for no other car of the backup); for rows that need a car, the one car of their name
    /// their odometer and time fit; else `None`.
    pub suggested: Option<CarId>,
    /// TeslaFi rows without a VIN the file ties to no car: a car is to be chosen for them (they are held till then).
    #[serde(default)]
    pub needs_car: bool,
    /// The car of the file its rows go with (a car with a VIN whose odometer they carry on, or a car of rows that
    /// need a car it follows): it takes that car's match.
    #[serde(default)]
    pub joins: Option<i64>,
    /// Rows of the file it has (TeslaFi), with those of the cars that follow it.
    #[serde(default)]
    pub rows: u64,
    #[serde(default)]
    pub first_odo_km: Option<f64>,
    #[serde(default)]
    pub last_odo_km: Option<f64>,
}

/// A check's result: the cars, and the report as JSON (`imported` 0 everywhere: nothing was stored).
#[derive(Debug, Clone)]
pub struct Checked {
    pub cars: Vec<BackupCar>,
    pub report: Value,
    /// TeslaFi's cars of the files as the rows told them apart (empty for a TeslaMate backup).
    pub groups: Vec<import::teslafi::Group>,
}

/// Where each car of the backup goes, by TeslaMate id: `Some(car)`, `None` = a new car; left-out cars are absent
/// from the map and listed in `leave_out`.
#[derive(Debug, Clone, Default)]
pub struct Matches {
    pub to: HashMap<i64, Option<CarId>>,
    pub leave_out: Vec<i64>,
    /// Cars of the file that go with another of the file.
    pub joins: HashMap<i64, i64>,
    /// Cars of the file whose rows are held until a car is chosen.
    pub hold: Vec<i64>,
    /// The `/imports` id held rows are kept under.
    pub hold_import: Option<String>,
}

impl Matches {
    fn apply(&self, sink: &mut DbSink<'_>) {
        for (tm, to) in &self.to {
            sink.matches.insert(*tm, to.map_or(Match::New, Match::Car));
        }
        for tm in &self.leave_out {
            sink.matches.insert(*tm, Match::LeaveOut);
        }
        for tm in &self.hold {
            sink.matches.insert(*tm, Match::Hold);
        }
        sink.joins = self.joins.clone();
        sink.hold_import = self.hold_import.clone();
    }
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

/// The one car here an imported car with this VIN prefix (and whole VIN's keyed hash, `tag`) may be
/// ([`crate::rehome::cars_for_vin`]: never a car whose whole VIN is known to be another).
pub fn suggest(e: &Engine, vin_prefix: Option<&str>, tag: Option<&str>) -> QResult<Option<CarId>> {
    let Some(p) = vin_prefix.filter(|p| !p.is_empty()) else { return Ok(None) };
    let same = crate::rehome::cars_for_vin(e.db(), p, tag)?;
    Ok(if same.len() == 1 { same.first().copied() } else { None })
}

/// The sink of a check: every car taken (under a stand-in id), its rows counted and dropped.
struct CountSink<'a> {
    engine: &'a Engine,
    stand_in: HashMap<CarId, i64>,
    cars: Vec<BackupCar>,
    reports: Vec<CarReport>,
    /// The time zone of each stand-in's suggested car (TeslaFi's times are read in it).
    zones: HashMap<CarId, String>,
}

/// Stand-in ids of a check's cars: from the top, far from any real car's.
const STAND_IN: CarId = 0xFF00_0000;

fn sink_err(e: impl std::fmt::Display) -> SinkError {
    e.to_string().into()
}

impl import::Sink for CountSink<'_> {
    fn car(&mut self, tm: &TmCar) -> Result<Option<Target>, SinkError> {
        let prefix = tm.vin.as_ref().map(|v| v.vin_prefix.clone()).filter(|p| !p.is_empty());
        let tag = tm.full_vin.as_ref().map(|v| crate::rehome::vin_tag(self.engine.db(), v));
        let suggested = suggest(self.engine, prefix.as_deref(), tag.as_deref()).map_err(sink_err)?;
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
            needs_car: tm.needs_match,
            joins: None,
            rows: 0,
            first_odo_km: None,
            last_odo_km: None,
        });
        // what an earlier import of the suggested car took is not counted again
        let state = match suggested {
            Some(c) => import_state(self.engine.db(), c).map_err(sink_err)?.unwrap_or_default(),
            None => Default::default(),
        };
        let zone = match suggested {
            Some(c) => self.engine.db().car(c).map_err(sink_err)?.map(|c| self.engine.car_zone(&c).0),
            None => None,
        };
        // rows that need a car are read in the zone of the one car suggested so far (they are compared with it)
        let zone = match zone {
            None if tm.needs_match => {
                let s: BTreeSet<CarId> = self.cars.iter().filter_map(|c| c.suggested).collect();
                match s.iter().collect::<Vec<_>>().as_slice() {
                    [one] => self.engine.db().car(**one).map_err(sink_err)?.map(|c| self.engine.car_zone(&c).0),
                    _ => None,
                }
            }
            z => z,
        };
        self.zones.insert(id, zone.unwrap_or_else(|| self.engine.inner.default_zone()));
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

    fn time_zone(&mut self, car: CarId) -> Option<String> {
        self.zones.get(&car).cloned()
    }
}

/// A failure of a run: `(code, message)` for the job's `error`.
pub fn error_of(e: &import::Error) -> (&'static str, String) {
    let code = match e {
        import::Error::Io(_) => "read_failed",
        import::Error::Unpack(_) | import::Error::Unsupported(_) | import::Error::Utf16Be => "unreadable",
        import::Error::CustomFormat => "custom_format",
        import::Error::NotTeslamate => "not_a_backup",
        import::Error::NotTeslafi => "not_a_teslafi_export",
        import::Error::TeslafiSummary => "teslafi_summary",
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

/// A car's own figures per table (`read`, `imported`, `skipped` by reason), so a car left out can be taken out of
/// the report's totals again ([`without_left_out`]).
fn car_tables(r: &CarReport, imported: bool) -> Value {
    r.tables
        .iter()
        .map(|(t, x)| {
            let took = if imported { x.imported } else { 0 };
            (
                t.name().to_string(),
                json!({ "read": x.read, "imported": took, "found": x.imported, "skipped": skipped(x) }),
            )
        })
        .collect::<Map<_, _>>()
        .into()
}

/// Takes the cars in `left_out` (TeslaMate car numbers as text) out of a report of [`report_json`] or
/// [`teslafi_report_json`]: out of its cars and out of its counts, as if the file never held them.
pub fn without_left_out(report: &mut Value, left_out: &[String]) {
    if left_out.is_empty() {
        return;
    }
    let Some(cars) = report.get_mut("cars").and_then(Value::as_array_mut) else { return };
    let (gone, kept): (Vec<Value>, Vec<Value>) = std::mem::take(cars)
        .into_iter()
        .partition(|c| c["tm_car"].as_str().is_some_and(|t| left_out.iter().any(|x| x == t)));
    *cars = kept;
    let less = |v: &mut Value, by: &Value| {
        if let (Some(a), Some(b)) = (v.as_u64(), by.as_u64()) {
            *v = json!(a.saturating_sub(b));
        }
    };
    for c in &gone {
        if let Some(ts) = c["tables"].as_object() {
            for (t, f) in ts {
                let Some(tot) = report.get_mut("tables").and_then(|x| x.get_mut(t)) else { continue };
                for k in ["read", "imported", "found"] {
                    if let Some(v) = tot.get_mut(k) {
                        less(v, &f[k]);
                    }
                }
                if let (Some(sk), Some(fs)) =
                    (tot.get_mut("skipped").and_then(Value::as_object_mut), f["skipped"].as_object())
                {
                    for (why, n) in fs {
                        if let Some(v) = sk.get_mut(why) {
                            less(v, n);
                        }
                    }
                }
            }
        }
        if report.get("kind").and_then(Value::as_str) == Some(import::teslafi::KIND) {
            for k in ["read", "used"] {
                if let Some(v) = report.pointer_mut(&format!("/rows/{k}")) {
                    less(v, &c["rows"]);
                }
            }
            if let Some(v) = report.pointer_mut("/found/drives") {
                less(v, &c["drives"]);
            }
            if let Some(v) = report.pointer_mut("/found/charges") {
                less(v, &c["charges"]);
            }
            if let Some(v) = report.get_mut("cars_count") {
                less(v, &json!(1));
            }
        }
    }
    if let Some(m) = report.as_object_mut() {
        m.remove("cars_left_out");
    }
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
                // open rows TeslaMate never ended, taken ending at their last reading (not skipped)
                "closed_at_last_reading": tr.notes.get(import::CLOSED_AT_LAST).copied().unwrap_or(0),
            }),
        );
    }
    // a table the backup doesn't have is a gap (null), not zero rows
    for t in [Table::Cars, Table::Drives, Table::ChargingProcesses, Table::Positions, Table::Updates, Table::Geofences]
    {
        tables
            .entry(t.name().to_string())
            .or_insert_with(|| json!({ "read": null, "imported": null, "skipped": {}, "bad_values": {} }));
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
                // left out, or no rows of its own: a gap, not zero
                _ => (Value::Null, Value::Null, Value::Null),
            };
            json!({ "tm_car": c.tm_car.to_string(), "name": c.name, "match": m, "drives": c.drives,
                    "charges": c.charges, "read": read, "imported": took, "skipped": sk,
                    "tables": own.map_or(Value::Null, |o| car_tables(o, imported)) })
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

/// Puts what TeslaFi's rows told of each car of the files on the check's cars: which go with another (their rows and
/// counts folded into it), which need a car (with the car of their name their odometer and time fit, suggested).
fn with_groups(e: &Engine, cars: &mut [BackupCar], groups: &[import::teslafi::Group]) -> crate::Result<()> {
    let by_tm: HashMap<i64, &import::teslafi::Group> = groups.iter().map(|g| (g.tm_car, g)).collect();
    for c in cars.iter_mut() {
        if let Some(g) = by_tm.get(&c.tm_car) {
            c.needs_car = g.needs_match;
            c.joins = g.joins;
            c.rows = g.rows;
            c.first_odo_km = g.first_odo_km;
            c.last_odo_km = g.last_odo_km;
        }
    }
    // a car's followers add to it
    let adds: Vec<(i64, BackupCar)> = cars.iter().filter_map(|c| c.joins.map(|j| (j, c.clone()))).collect();
    for (j, f) in adds {
        if let Some(lead) = cars.iter_mut().find(|c| c.tm_car == j) {
            lead.rows += f.rows;
            lead.drives += f.drives;
            lead.charges += f.charges;
            lead.first_ms = match (lead.first_ms, f.first_ms) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            lead.last_ms = lead.last_ms.max(f.last_ms);
            if lead.needs_car && f.first_ms <= lead.first_ms {
                lead.first_odo_km = f.first_odo_km.or(lead.first_odo_km);
            }
            if lead.needs_car && f.last_ms >= lead.last_ms {
                lead.last_odo_km = f.last_odo_km.or(lead.last_odo_km);
            }
        }
    }
    for c in cars.iter_mut().filter(|c| c.needs_car && c.joins.is_none()) {
        let mut merged: Vec<import::teslafi::Spell> = groups
            .iter()
            .filter(|g| g.tm_car == c.tm_car || (g.needs_match && g.joins == Some(c.tm_car)))
            .flat_map(|g| g.spells.iter().copied())
            .collect();
        merged.sort_by_key(|s| s.first_ms);
        let g = import::teslafi::Group { spells: merged, ..Default::default() };
        c.suggested = crate::rehome::by_name(e, c.name.as_deref(), &g)?;
    }
    Ok(())
}

/// Re-runs TeslaFi's matching over an import's kept files (nothing is read into a car) and moves the rows an
/// earlier run put on `cars` that the matching now puts elsewhere: to the car of their name their odometer and time
/// fit ([`crate::rehome::by_name`]), else held for a car to be chosen under `import`. Returns the new check (the
/// cars as the rows tell them now) and the moves.
pub fn teslafi_rehome<R: Read, I: IntoIterator<Item = io::Result<(String, R)>>>(
    e: &Engine,
    files: TeslafiFiles<R, I>,
    s: &TeslafiSettings,
    cars: &[CarId],
    import: &str,
) -> Result<(Checked, Vec<crate::rehome::Move>), import::Error> {
    let sink_err = |x: crate::Error| import::Error::Sink(x.to_string().into());
    let checked = teslafi_check_files(e, files, s)?;
    let plan = crate::rehome::rehome_plan(e, &checked.groups).map_err(sink_err)?;
    let mut moves = Vec::new();
    for r in plan {
        if r.spans.is_empty() {
            continue;
        }
        let dest = match r.to {
            Some(c) => crate::rehome::Dest::Car(c),
            None => crate::rehome::Dest::Hold(crate::rehome::HoldAs {
                import: Some(import.to_string()),
                tm_car: Some(r.tm_car),
                name: r.name.clone(),
                first_odo_km: r.first_odo_km,
                last_odo_km: r.last_odo_km,
                why: "no_vin".into(),
                ..Default::default()
            }),
        };
        for &car in cars.iter().filter(|&&c| Some(c) != r.to) {
            if let Some(m) =
                crate::rehome::move_rows(e, car, &dest, model::Source::Teslafi, &r.spans).map_err(sink_err)?
            {
                moves.push(m);
            }
        }
    }
    Ok((checked, moves))
}

/// Checks a whole backup: reads it as the import would and stores nothing.
pub fn check<R: Read>(e: &Engine, open: impl FnMut() -> io::Result<R>) -> Result<Checked, import::Error> {
    let mut sink =
        CountSink { engine: e, stand_in: HashMap::new(), cars: Vec::new(), reports: Vec::new(), zones: HashMap::new() };
    let report = import::import(open, &mut sink, &import::Options::default())?;
    one_each(&mut sink.cars);
    let pairs: Vec<(BackupCar, Value)> = sink.cars.iter().map(|c| (c.clone(), Value::Null)).collect();
    let json = report_json(&report, &sink.reports, &pairs, false);
    Ok(Checked { cars: sink.cars, report: json, groups: Vec::new() })
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
    m.apply(&mut sink);
    sink.currency = currency;
    let r = import::import(open, &mut sink, &import::Options::default());
    sink.finish();
    let report = r?;
    Ok(Imported { report, cars: std::mem::take(&mut sink.reports), made: std::mem::take(&mut sink.made) })
}

/// What the owner said about a TeslaFi file (each `None`: worked out from the file, or the car's time zone).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct TeslafiSettings {
    pub distance_unit: Option<import::teslafi::Distance>,
    pub temperature_unit: Option<import::teslafi::Temperature>,
    pub time_zone: Option<String>,
}

fn teslafi_options(e: &Engine, s: &TeslafiSettings) -> import::teslafi::Options {
    import::teslafi::Options {
        distance: s.distance_unit,
        temperature: s.temperature_unit,
        time_zone: s.time_zone.clone(),
        default_time_zone: e.inner.default_zone(),
        ..import::teslafi::Options::default()
    }
}

/// The report of a TeslaFi pass as the API writes it (`kind: "teslafi"`): files, rows read, used and skipped with why,
/// the drives, charges, states and updates found (stored, once `imported`), the cars with their match, the date range,
/// the units and time zone the times and values were read in (the file can't tell its time zone: `asked` says the
/// owner should confirm it).
pub fn teslafi_report_json(
    r: &import::teslafi::Report,
    reports: &[CarReport],
    cars: &[(BackupCar, Value)],
    imported: bool,
) -> Value {
    let n = |x: u64| if imported { x } else { 0 };
    let table = |t: Table| -> Value {
        let (read, took, skipped) = reports.iter().fold((0u64, 0u64, Map::new()), |(a, b, mut m), c| {
            let x = c.table(t);
            for (k, v) in skipped(&x) {
                let old = m.get(&k).and_then(Value::as_u64).unwrap_or(0);
                m.insert(k, json!(old + v.as_u64().unwrap_or(0)));
            }
            (a + x.read, b + x.imported, m)
        });
        // the file has no rows to build it from: a gap, not zero
        if r.rows_read == 0 {
            return json!({ "read": null, "imported": null, "found": null, "skipped": {} });
        }
        json!({ "read": read, "imported": n(took), "found": took, "skipped": skipped })
    };
    let per_car: Vec<Value> = cars
        .iter()
        .map(|(c, m)| {
            let own = reports.iter().find(|x| x.tm_car_id == c.tm_car);
            let left_out = m.as_str() == Some("leave_out") || own.is_some_and(|o| o.car.is_none() && imported);
            let rows = own.map_or(0, |o| o.table(Table::Positions).read);
            // this pass's own figures (a run may read the times in another zone than the check did), else the check's
            let mine = own.filter(|o| o.car.is_some() && o.first_ms.is_some());
            let (drives, charges, first_ms, last_ms) = match mine {
                Some(o) => {
                    (o.table(Table::Drives).imported, o.table(Table::ChargingProcesses).imported, o.first_ms, o.last_ms)
                }
                None => (c.drives, c.charges, c.first_ms, c.last_ms),
            };
            json!({
                "tm_car": c.tm_car.to_string(), "name": c.name, "vin_prefix": c.vin_prefix, "match": m,
                "drives": drives, "charges": charges, "first_ms": first_ms, "last_ms": last_ms,
                "rows": if (left_out && imported) || own.is_none() { Value::Null } else { json!(rows) },
                "tables": own.map_or(Value::Null, |o| car_tables(o, imported)),
            })
        })
        .collect();
    json!({
        "kind": import::teslafi::KIND,
        "imported": imported,
        "files": r.files,
        "files_skipped": r.files_skipped,
        "rows": {
            "read": r.rows_read,
            "used": r.rows_used,
            "skipped": r.rows_skipped,
            "bad_values": r.bad_values,
            "windows_1252": r.lines_windows_1252,
        },
        "found": {
            "drives": r.drives, "charges": r.charges, "states": r.states, "updates": r.updates,
            "positions": r.positions, "charge_points": r.charge_points, "replaced": r.removed,
            "read_again": r.read_again,
        },
        "tables": {
            "positions": table(Table::Positions),
            "drives": table(Table::Drives),
            "charging_processes": table(Table::ChargingProcesses),
            "states": table(Table::States),
            "updates": table(Table::Updates),
        },
        "span": { "from_ms": r.first_ms, "to_ms": r.last_ms },
        "cars": per_car,
        "cars_count": r.cars,
        "cars_left_out": r.cars_left_out,
        "units": r.units,
        "time_zone": { "name": r.time_zone, "from": r.time_zone_from, "asked": true, "dst_repeated_rows": r.dst_repeated },
        "compression": r.compression,
        "packed_bytes": r.packed_bytes,
        "unpacked_bytes": r.unpacked_bytes,
        "missing_columns": r.missing_columns,
        "unknown_columns": r.unknown_columns,
        "warnings": r.warnings,
        "uploads": r.uploads,
    })
}

/// TeslaFi files to read: one upload, or several read as one ([`import::teslafi::import_files`]).
pub enum TeslafiFiles<R, I> {
    One(R),
    Several(I),
}

impl<R: Read, I: IntoIterator<Item = io::Result<(String, R)>>> TeslafiFiles<R, I> {
    fn import(
        self,
        sink: &mut dyn import::Sink,
        o: &import::teslafi::Options,
    ) -> Result<import::teslafi::Report, import::Error> {
        match self {
            TeslafiFiles::One(r) => import::teslafi::import(r, sink, o),
            TeslafiFiles::Several(f) => import::teslafi::import_files(f, sink, o),
        }
    }
}

/// Checks TeslaFi's export: reads it as the import would and stores nothing.
pub fn teslafi_check<R: Read>(e: &Engine, r: R, s: &TeslafiSettings) -> Result<Checked, import::Error> {
    teslafi_check_files(e, TeslafiFiles::<R, Vec<io::Result<(String, R)>>>::One(r), s)
}

/// [`teslafi_check`] of one file or several.
pub fn teslafi_check_files<R: Read, I: IntoIterator<Item = io::Result<(String, R)>>>(
    e: &Engine,
    files: TeslafiFiles<R, I>,
    s: &TeslafiSettings,
) -> Result<Checked, import::Error> {
    let mut sink =
        CountSink { engine: e, stand_in: HashMap::new(), cars: Vec::new(), reports: Vec::new(), zones: HashMap::new() };
    let report = files.import(&mut sink, &teslafi_options(e, s))?;
    one_each(&mut sink.cars);
    with_groups(e, &mut sink.cars, &report.groups).map_err(|x| import::Error::Sink(x.to_string().into()))?;
    let pairs: Vec<(BackupCar, Value)> =
        sink.cars.iter().filter(|c| c.joins.is_none()).map(|c| (c.clone(), Value::Null)).collect();
    let json = teslafi_report_json(&report, &sink.reports, &pairs, false);
    Ok(Checked { cars: sink.cars, report: json, groups: report.groups })
}

/// Reads a TeslaMate backup again for the drives it holds for one car (nothing is stored: see
/// [`crate::importer::ReplaySink`]).
pub fn replay_drives<R: Read>(
    e: &Engine,
    open: impl FnMut() -> io::Result<R>,
    plan: crate::importer::ReplayPlan,
) -> Result<Vec<model::Drive>, import::Error> {
    let mut sink = crate::importer::ReplaySink::new(e, plan, model::Source::Teslamate);
    import::import(open, &mut sink, &import::Options::default())?;
    Ok(sink.into_drives())
}

/// [`replay_drives`] of TeslaFi's files.
pub fn replay_teslafi_drives<R: Read, I: IntoIterator<Item = io::Result<(String, R)>>>(
    e: &Engine,
    files: TeslafiFiles<R, I>,
    s: &TeslafiSettings,
    plan: crate::importer::ReplayPlan,
) -> Result<Vec<model::Drive>, import::Error> {
    let mut sink = crate::importer::ReplaySink::new(e, plan, model::Source::Teslafi);
    files.import(&mut sink, &teslafi_options(e, s))?;
    Ok(sink.into_drives())
}

/// What a TeslaFi run did.
#[derive(Debug, Clone)]
pub struct TeslafiImported {
    pub report: import::teslafi::Report,
    pub cars: Vec<CarReport>,
    pub made: Vec<CarId>,
    /// The lots of rows held for a car to be chosen: (the car of the file, the lot's id).
    pub held: Vec<(i64, String)>,
}

/// Imports TeslaFi's export with every car matched.
pub fn teslafi_run<R: Read>(
    e: &Engine,
    r: R,
    m: &Matches,
    s: &TeslafiSettings,
) -> Result<TeslafiImported, import::Error> {
    teslafi_run_files(e, TeslafiFiles::<R, Vec<io::Result<(String, R)>>>::One(r), m, s)
}

/// [`teslafi_run`] of one file or several.
pub fn teslafi_run_files<R: Read, I: IntoIterator<Item = io::Result<(String, R)>>>(
    e: &Engine,
    files: TeslafiFiles<R, I>,
    m: &Matches,
    s: &TeslafiSettings,
) -> Result<TeslafiImported, import::Error> {
    let mut sink = DbSink::teslafi(e);
    m.apply(&mut sink);
    let r = files.import(&mut sink, &teslafi_options(e, s));
    sink.finish();
    let report = r?;
    // each lot named as its rows are, with their odometer
    for (tm, lot) in &sink.held {
        let members: Vec<&import::teslafi::Group> =
            report.groups.iter().filter(|g| g.tm_car == *tm || m.joins.get(&g.tm_car) == Some(tm)).collect();
        let name = members.iter().find_map(|g| g.name.clone());
        let first =
            members.iter().filter(|g| g.first_odo_km.is_some()).min_by_key(|g| g.first_ms).and_then(|g| g.first_odo_km);
        let last =
            members.iter().filter(|g| g.last_odo_km.is_some()).max_by_key(|g| g.last_ms).and_then(|g| g.last_odo_km);
        crate::rehome::describe_lot(e.db(), lot, name, (first, last))
            .map_err(|x| import::Error::Sink(x.to_string().into()))?;
    }
    Ok(TeslafiImported {
        report,
        cars: std::mem::take(&mut sink.reports),
        made: std::mem::take(&mut sink.made),
        held: std::mem::take(&mut sink.held),
    })
}

/// A car of the backup may be matched to this car (it exists).
pub fn car_exists(e: &Engine, car: CarId) -> QResult<bool> {
    Ok(e.db().car(car)?.is_some())
}

/// For the error answer of a query that is not about a car.
pub fn no_such_import() -> QueryError {
    QueryError::NotFound("no_such_import")
}
