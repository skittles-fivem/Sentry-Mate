//! Imported history (TeslaMate's backup, TeslaFi's data download), stored beside the CAN-derived history and marked
//! by source: [`DbSink`] is the [`import::Sink`] that puts an import's rows into the db as [`model::Source::Teslamate`]
//! or [`model::Source::Teslafi`] (its points in that site's own stream). A CAN rebuild only ever replaces CAN rows
//! ([`db::Db::replace_window`]), so imported rows stay whatever is rebuilt.
//!
//! Imported rows get ids of their own (the top bit set, the next one for TeslaFi's, then the car, then the import's
//! id: TeslaMate's, or a TeslaFi row's start in seconds), so they never meet a CAN row's id nor each other's, and
//! importing the same file again replaces them in place. Each car's [`import::ImportState`] is kept so a newer backup
//! or month adds only what is new.

use crate::pipeline::merge_points;
use crate::records::{self, NS};
use crate::{Engine, Event, Result, now_ms};
use db::{StateRow, UpdateRow};
use import::{AddressRow, Batch, CarReport, ImportState, PlaceRow, SinkError, Target, TmCar};
use model::{Car, CarId, ChargePoint, Sample, Source};
use std::collections::{BTreeSet, HashMap};

const TM_BIT: u64 = 1 << 63;

/// Set (with [`TM_BIT`]) on TeslaFi rows' ids. TeslaMate rows' ids hold the car in the bits below it (a car id past
/// 2^22 would reach it; ids are given from 1 up).
const TF_BIT: u64 = 1 << 62;

/// The id an imported TeslaMate row gets.
pub fn imported_id(car: CarId, tm_id: i64) -> u64 {
    TM_BIT | ((u64::from(car) & 0x7F_FFFF) << 40) | (u64::try_from(tm_id).unwrap_or(0) & 0xFF_FFFF_FFFF)
}

/// The id a row imported from `source` gets.
pub fn imported_id_of(source: Source, car: CarId, id: i64) -> u64 {
    match source {
        Source::Teslafi => {
            TM_BIT | TF_BIT | ((u64::from(car) & 0x3F_FFFF) << 40) | (u64::try_from(id).unwrap_or(0) & 0xFF_FFFF_FFFF)
        }
        _ => imported_id(car, id),
    }
}

/// Whether an id is an imported row's.
pub fn is_imported(id: u64) -> bool {
    id & TM_BIT != 0
}

/// Where a car of the backup goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    Car(CarId),
    /// A new car, made from the backup's record.
    New,
    LeaveOut,
    /// Held until an admin chooses its car ([`crate::rehome`]): rows of the files the import can't put on a car for
    /// sure.
    Hold,
}

/// Stand-in car ids of the lots an import holds (far above any real car's).
const HOLD_BASE: CarId = 0xFE00_0000;

/// Stores an import's rows in the engine's db.
pub struct DbSink<'a> {
    engine: &'a Engine,
    /// TeslaMate car id → where it goes. A car not named here goes to the one car with the same VIN prefix (when no
    /// other car of the backup has had it), a new car when no car has it, and is refused as unmatched when that is not
    /// clear (two cars of the same model, year, plant and make share a prefix: only the serial tells them apart, and
    /// a car keeps only the prefix). Name every car here to be sure.
    pub matches: HashMap<i64, Match>,
    /// VIN prefixes matched by the prefix alone in this import, and the TeslaMate car each went with.
    by_prefix: HashMap<String, i64>,
    /// The currency imported charge costs are in (TeslaMate keeps none).
    pub currency: Option<String>,
    pub reports: Vec<CarReport>,
    /// Cars the import made, and the cars that got rows.
    pub made: Vec<CarId>,
    touched: BTreeSet<CarId>,
    spans: HashMap<CarId, (u64, u64)>,
    pub places_added: usize,
    pub addresses_seen: usize,
    /// The source the rows are stored as: [`Source::Teslamate`] (the default) or [`Source::Teslafi`].
    pub source: Source,
    /// Cars of the file that go with another of the file (TeslaMate car id → the one it goes with): their rows are
    /// that car's.
    pub joins: HashMap<i64, i64>,
    /// The `/imports` id the rows held for a car are kept under ([`Match::Hold`]); without one they are left out.
    pub hold_import: Option<String>,
    /// Where each car of the file went (after [`DbSink::joins`]).
    targets: HashMap<i64, Option<CarId>>,
    /// Stand-in id → the lot it fills.
    lots: HashMap<CarId, String>,
    /// The lots this import filled, by the car of the file each is for.
    pub held: Vec<(i64, String)>,
    /// Rows of earlier imports taken away (a TeslaFi drive over a month's end joined into one).
    pub removed: usize,
}

impl<'a> DbSink<'a> {
    pub fn new(engine: &'a Engine) -> DbSink<'a> {
        DbSink {
            engine,
            matches: HashMap::new(),
            by_prefix: HashMap::new(),
            currency: None,
            reports: Vec::new(),
            made: Vec::new(),
            touched: BTreeSet::new(),
            spans: HashMap::new(),
            places_added: 0,
            addresses_seen: 0,
            source: Source::Teslamate,
            removed: 0,
            joins: HashMap::new(),
            hold_import: None,
            targets: HashMap::new(),
            lots: HashMap::new(),
            held: Vec::new(),
        }
    }

    /// A sink storing rows imported from TeslaFi.
    pub fn teslafi(engine: &'a Engine) -> DbSink<'a> {
        DbSink { source: Source::Teslafi, ..DbSink::new(engine) }
    }

    fn id(&self, car: CarId, id: i64) -> u64 {
        imported_id_of(self.source, car, id)
    }

    fn widen(&mut self, car: CarId, a: u64, b: u64) {
        let s = self.spans.entry(car).or_insert((a, b));
        s.0 = s.0.min(a);
        s.1 = s.1.max(b);
        self.touched.insert(car);
    }

    /// Tells the pages what changed (call once the import is done).
    pub fn finish(&self) {
        for &car in &self.touched {
            // the days the import brought, worked out ahead for the pages' long periods
            self.engine.warm(Some(car));
            let (a, b) = self.spans.get(&car).copied().unwrap_or((0, u64::MAX));
            self.engine.emit(Event::Rows {
                car,
                kinds: vec!["drives", "charges", "updates", "visits", "states"],
                from_ms: a,
                to_ms: b.saturating_add(1),
            });
        }
    }
}

fn err(e: impl std::fmt::Display) -> SinkError {
    e.to_string().into()
}

/// The import state kept for a car.
pub fn import_state(db: &db::Db, car: CarId) -> Result<Option<ImportState>> {
    records::get(db, NS, &format!("car/{car}/import"))
}

impl DbSink<'_> {
    /// The car of the file `tm` goes with (itself, or the one [`DbSink::joins`] names, followed to its end).
    fn root(&self, tm: i64) -> i64 {
        let mut t = tm;
        for _ in 0..64 {
            match self.joins.get(&t) {
                Some(&j) if j != t => t = j,
                _ => break,
            }
        }
        t
    }

    /// A lot for the car of the file `root` (its stand-in id).
    fn lot(&mut self, root: i64, tm: &TmCar) -> Result<Option<CarId>, SinkError> {
        let Some(import) = self.hold_import.clone() else { return Ok(None) };
        let how = crate::rehome::HoldAs {
            import: Some(import),
            tm_car: Some(root),
            name: tm.name.clone().filter(|n| !n.trim().is_empty()),
            why: "no_vin".into(),
            ..Default::default()
        };
        let id = crate::rehome::open_lot(&self.engine.inner.db, self.source, &how, true).map_err(err)?;
        let stand_in = HOLD_BASE + self.lots.len() as CarId;
        self.lots.insert(stand_in, id.clone());
        self.held.push((root, id));
        Ok(Some(stand_in))
    }

    /// Where the car of the file `root` goes (asked with `tm`, a car of the file that goes with it): as matched, else
    /// the car here with its whole VIN, else the one car with its VIN prefix whose whole VIN is not known to be
    /// another, else a new car. Rows without a VIN the file ties to no car are held when not matched.
    fn resolve(&mut self, root: i64, tm: &TmCar) -> Result<Option<Target>, SinkError> {
        let db = &self.engine.inner.db;
        let tag = tm.full_vin.as_ref().map(|v| crate::rehome::vin_tag(db, v));
        let m = match self.matches.get(&root) {
            Some(m) => *m,
            None if tm.needs_match => Match::Hold,
            None => {
                let prefix = tm.vin.as_ref().map(|v| v.vin_prefix.clone()).filter(|p| !p.is_empty());
                match prefix {
                    None => Match::New,
                    Some(p) => {
                        let unmatched = || {
                            err(format!(
                                "cars_unmatched: TeslaMate car {} shares its VIN prefix with another car; choose which \
                                 car it is",
                                tm.tm_id
                            ))
                        };
                        let key = format!("{p}/{}", tag.as_deref().unwrap_or(""));
                        if self.by_prefix.get(&key).is_some_and(|&other| other != root) {
                            return Err(unmatched());
                        }
                        let same = crate::rehome::cars_for_vin(db, &p, tag.as_deref()).map_err(err)?;
                        match same.as_slice() {
                            [] => Match::New,
                            [one] => {
                                self.by_prefix.insert(key, root);
                                Match::Car(*one)
                            }
                            _ => return Err(unmatched()),
                        }
                    }
                }
            }
        };
        let car = match m {
            Match::LeaveOut => return Ok(None),
            Match::Hold => return Ok(self.lot(root, tm)?.map(|car| Target { car, state: Default::default() })),
            Match::Car(c) => {
                if db.car(c).map_err(err)?.is_none() {
                    return Err(err(format!("no car {c}")));
                }
                c
            }
            Match::New => {
                let name = tm
                    .name
                    .clone()
                    .filter(|n| !n.trim().is_empty())
                    .or_else(|| tm.vin.as_ref().map(crate::pipeline::name_from_vin))
                    .unwrap_or_else(|| "Imported car".into());
                let c = db
                    .create_car(Car {
                        id: 0,
                        name: name.clone(),
                        vehicle_keys: Vec::new(),
                        preferred_key: None,
                        vin: tm.vin.clone(),
                        profile_setting: None,
                        time_zone: self.engine.inner.default_zone(),
                        time_zone_typed: Some(false),
                        efficiency_wh_km: tm.efficiency_wh_km,
                        created_ms: now_ms(),
                        inputs: Default::default(),
                    })
                    .map_err(err)?;
                self.made.push(c.id);
                if let Some(p) = tm.vin.as_ref().map(|v| v.vin_prefix.clone()).filter(|p| !p.is_empty()) {
                    self.by_prefix.entry(format!("{p}/{}", tag.as_deref().unwrap_or(""))).or_insert(root);
                }
                self.engine.emit(Event::Fleet { car: c.id, change: "added", name: Some(name) });
                c.id
            }
        };
        // what TeslaMate said of the car's build, for its `vehicle` facts (labelled `teslamate`)
        let build = model::vehicle::TextBuild {
            car_type: tm.model.clone(),
            trim_badging: tm.trim_badging.clone(),
            exterior_color: tm.exterior_color.clone(),
            wheel_type: tm.wheel_type.clone(),
            lfp_battery: tm.lfp_battery,
            ..Default::default()
        };
        if build != model::vehicle::TextBuild::default() {
            crate::query::vehicle::put_imported(db, car, &build).map_err(err)?;
        }
        // the car's whole VIN, as a keyed hash: a later import tells it from another car of its prefix
        if let Some(t) = &tag {
            crate::rehome::add_vin_tag(db, car, t).map_err(err)?;
        }
        let state = import_state(db, car).map_err(err)?.unwrap_or_default();
        Ok(Some(Target { car, state }))
    }

    /// A batch for a lot: its rows kept as they would be stored, to go on a car once one is chosen.
    fn hold_batch(&mut self, lot: &str, batch: Batch) -> Result<(), SinkError> {
        let db = &self.engine.inner.db;
        let src = self.source;
        let id = |tm: i64| imported_id_of(src, 0, tm);
        let mut b = crate::rehome::Bundle::new(src);
        match batch {
            Batch::Drives(rows) => {
                for r in rows {
                    let mut d = r.drive;
                    d.id = id(r.tm_id);
                    d.source = src;
                    b.spans.push((d.start_ms, d.end_ms));
                    b.drives.push(d);
                }
            }
            Batch::Charges(rows) => {
                for r in rows {
                    let mut c = r.charge;
                    c.id = id(r.tm_id);
                    c.source = src;
                    b.spans.push((c.start_ms, c.end_ms));
                    if let (Some(amount), Some(cur)) = (c.cost, &self.currency) {
                        let entry = records::CostEntry {
                            amount,
                            currency: cur.clone(),
                            source: src.site().map_or("teslamate", model::ImportSite::word).into(),
                            mode: None,
                            entered: None,
                            energy_basis: None,
                            place: None,
                            parts: Vec::new(),
                            session_fee: None,
                            gap_ms: 0,
                            cycle: None,
                            of: None,
                            at_ms: now_ms(),
                        };
                        b.costs.push((c.id, c.start_ms, entry));
                    }
                    b.charges.push(c);
                }
            }
            Batch::ChargePoints(rows) => b.points = rows.into_iter().map(|r| r.point).collect(),
            Batch::Positions(rows) => b.samples = rows.into_iter().map(|r| r.sample).collect(),
            Batch::States(rows) => {
                for r in rows {
                    let mut sp = r.span;
                    sp.source = src;
                    b.spans.push((sp.start_ms, sp.end_ms));
                    b.states.push(StateRow { id: id(r.tm_id), span: sp });
                }
            }
            Batch::Updates(rows) => {
                for r in rows {
                    let mut u = r.update;
                    u.source = src;
                    b.spans.push((u.start_ms, u.end_ms.unwrap_or(u.start_ms)));
                    b.updates.push(UpdateRow { id: id(r.tm_id), update: u });
                }
            }
            // a joined row's halves, a stretch read again: what a lot holds is replaced whole when the import runs
            // again
            Batch::Removed(_) | Batch::Cleared(_) => return Ok(()),
        }
        let times = b.samples.iter().map(|p| p.t_ms).chain(b.points.iter().map(|p| p.t_ms));
        if let (Some(a), Some(z)) = (times.clone().min(), times.max()) {
            b.spans.push((a, z));
        }
        crate::rehome::hold(db, lot, b).map_err(err)
    }
}

impl import::Sink for DbSink<'_> {
    fn car(&mut self, tm: &TmCar) -> Result<Option<Target>, SinkError> {
        let root = self.root(tm.tm_id);
        if let Some(t) = self.targets.get(&root).copied() {
            let Some(car) = t else { return Ok(None) };
            let state = if self.lots.contains_key(&car) {
                Default::default()
            } else {
                import_state(&self.engine.inner.db, car).map_err(err)?.unwrap_or_default()
            };
            return Ok(Some(Target { car, state }));
        }
        let t = self.resolve(root, tm)?;
        self.targets.insert(root, t.as_ref().map(|t| t.car));
        Ok(t)
    }

    fn batch(&mut self, car: CarId, batch: Batch) -> Result<(), SinkError> {
        if let Some(lot) = self.lots.get(&car).cloned() {
            return self.hold_batch(&lot, batch);
        }
        let db = &self.engine.inner.db;
        match batch {
            Batch::Drives(rows) => {
                let v: Vec<model::Drive> = rows
                    .into_iter()
                    .map(|r| {
                        let mut d = r.drive;
                        d.id = self.id(car, r.tm_id);
                        d.car = car;
                        d.source = self.source;
                        d
                    })
                    .collect();
                if let (Some(a), Some(b)) = (v.iter().map(|d| d.start_ms).min(), v.iter().map(|d| d.end_ms).max()) {
                    self.widen(car, a, b);
                }
                db.upsert(car, &v).map_err(err)?;
            }
            Batch::Charges(rows) => {
                let mut v = Vec::with_capacity(rows.len());
                for r in rows {
                    let mut c = r.charge;
                    c.id = self.id(car, r.tm_id);
                    c.car = car;
                    c.source = self.source;
                    if let (Some(amount), Some(cur)) = (c.cost, &self.currency) {
                        let entry = records::CostEntry {
                            amount,
                            currency: cur.clone(),
                            source: self.source.site().map_or("teslamate", model::ImportSite::word).into(),
                            mode: None,
                            entered: None,
                            energy_basis: None,
                            place: None,
                            parts: Vec::new(),
                            session_fee: None,
                            gap_ms: 0,
                            at_ms: now_ms(),
                            cycle: None,
                            of: None,
                        };
                        records::put_cost(db, car, c.id, c.start_ms, Some(entry)).map_err(err)?;
                    }
                    self.widen(car, c.start_ms, c.end_ms);
                    v.push(c);
                }
                db.upsert(car, &v).map_err(err)?;
            }
            Batch::ChargePoints(rows) => {
                let mut pts: Vec<ChargePoint> = rows.into_iter().map(|r| r.point).collect();
                write_merged(db, car, self.source, &mut pts).map_err(err)?;
            }
            Batch::Positions(rows) => {
                let mut pts: Vec<Sample> = rows.into_iter().map(|r| r.sample).collect();
                if let (Some(a), Some(b)) = (pts.iter().map(|p| p.t_ms).min(), pts.iter().map(|p| p.t_ms).max()) {
                    self.widen(car, a, b);
                }
                write_merged(db, car, self.source, &mut pts).map_err(err)?;
            }
            Batch::States(rows) => {
                let v: Vec<StateRow> = rows
                    .into_iter()
                    .map(|r| {
                        let mut span = r.span;
                        span.car = car;
                        span.source = self.source;
                        StateRow { id: self.id(car, r.tm_id), span }
                    })
                    .collect();
                db.upsert(car, &v).map_err(err)?;
            }
            Batch::Updates(rows) => {
                let v: Vec<UpdateRow> = rows
                    .into_iter()
                    .map(|r| {
                        let mut update = r.update;
                        update.car = car;
                        update.source = self.source;
                        UpdateRow { id: self.id(car, r.tm_id), update }
                    })
                    .collect();
                db.upsert(car, &v).map_err(err)?;
            }
            Batch::Removed(rows) => {
                let ids = |t: import::Table| -> Vec<u64> {
                    rows.iter().filter(|(x, _)| *x == t).map(|(_, id)| self.id(car, *id)).collect()
                };
                let mut n = 0;
                n += db.delete::<model::Drive>(car, &ids(import::Table::Drives)).map_err(err)?;
                n += db.delete::<model::Charge>(car, &ids(import::Table::ChargingProcesses)).map_err(err)?;
                n += db.delete::<StateRow>(car, &ids(import::Table::States)).map_err(err)?;
                n += db.delete::<UpdateRow>(car, &ids(import::Table::Updates)).map_err(err)?;
                self.removed += n;
                self.touched.insert(car);
            }
            Batch::Cleared(spans) => {
                for (a, b) in spans {
                    let d = db.delete_imported_window(car, self.source, a, b.saturating_add(1)).map_err(err)?;
                    self.removed += d.drives + d.charges + d.states + d.updates;
                    self.widen(car, a, b);
                }
            }
        }
        Ok(())
    }

    fn time_zone(&mut self, car: CarId) -> Option<String> {
        if self.lots.contains_key(&car) {
            return None;
        }
        self.engine.inner.db.car(car).ok().flatten().map(|c| self.engine.inner.car_zone(&c).0)
    }

    fn places(&mut self, rows: Vec<PlaceRow>) -> Result<(), SinkError> {
        let db = &self.engine.inner.db;
        let have = db.places().map_err(err)?;
        for r in rows {
            let p = r.place;
            let same = have.iter().any(|h| {
                h.name == p.name && places::distance_m(h.lat, h.lon, p.lat, p.lon) < h.radius_m.max(p.radius_m)
            });
            if same {
                continue;
            }
            let mut p = p;
            p.radius_m = places::clamp_radius(p.radius_m);
            // TeslaMate keeps no currency: its prices are in the one the import was started with, and without one a
            // price could not be told apart from another currency's, so the place comes without it
            p.price = match (p.price.take(), &self.currency) {
                (Some(mut plan), Some(cur)) => {
                    if plan.currency.is_empty() {
                        plan.currency = cur.clone();
                    }
                    Some(plan)
                }
                (Some(plan), None) if !plan.currency.is_empty() => Some(plan),
                _ => None,
            };
            // a price per minute is kept as one typed here is: a plan of no price per kWh, the minutes beside it
            let mut price = None;
            let per_minute = match (r.per_minute_price, &self.currency) {
                (Some(m), Some(cur)) if p.price.is_none() => {
                    p.price = Some(model::PricePlan {
                        currency: cur.clone(),
                        per_kwh: 0.0,
                        rates: Vec::new(),
                        session_fee: r.session_fee,
                    });
                    price = Some(crate::query::places::Price {
                        currency: cur.clone(),
                        per_kwh: None,
                        per_minute: Some(m),
                        rates: Vec::new(),
                        session_fee: r.session_fee,
                        tiers: None,
                    });
                    Some(m)
                }
                _ => None,
            };
            let added = db.add_place(p).map_err(err)?;
            // every imported place says when it came, as one made here does
            let extra =
                crate::placing::PlaceExtra { per_minute, price, created_ms: Some(now_ms()), ..Default::default() };
            crate::placing::set_extra(db, added.id, Some(&extra)).map_err(err)?;
            self.places_added += 1;
        }
        self.engine.places_changed();
        Ok(())
    }

    fn addresses(&mut self, rows: Vec<AddressRow>) -> Result<(), SinkError> {
        self.addresses_seen += rows.len();
        Ok(())
    }

    fn car_done(&mut self, report: CarReport, state: Option<ImportState>) -> Result<(), SinkError> {
        if let (Some(car), Some(state)) = (report.car.filter(|c| !self.lots.contains_key(c)), state) {
            records::put(&self.engine.inner.db, NS, &format!("car/{car}/import"), &state).map_err(err)?;
        }
        self.reports.push(report);
        Ok(())
    }
}

/// Which cars of an import's file go to one car, for reading the file again ([`ReplaySink`]).
#[derive(Debug, Clone, Default)]
pub struct ReplayPlan {
    /// The car the rows are wanted for.
    pub car: CarId,
    /// The cars of the file (by their id in it) that were matched to it.
    pub mine: Vec<i64>,
    /// Cars of the file that go with another of the file (as [`DbSink::joins`]).
    pub joins: HashMap<i64, i64>,
}

/// The sink of an import's file read again to get back the drives of one car: nothing is stored, no car made and
/// no state kept (a fresh state reads every row), the drives are only collected, with the ids the import gave them.
pub struct ReplaySink<'a> {
    engine: &'a Engine,
    plan: ReplayPlan,
    source: Source,
    drives: std::collections::BTreeMap<u64, model::Drive>,
}

impl<'a> ReplaySink<'a> {
    pub fn new(engine: &'a Engine, plan: ReplayPlan, source: Source) -> ReplaySink<'a> {
        ReplaySink { engine, plan, source, drives: Default::default() }
    }

    /// The drives read, by start.
    pub fn into_drives(self) -> Vec<model::Drive> {
        let mut v: Vec<model::Drive> = self.drives.into_values().collect();
        v.sort_by_key(|d| (d.start_ms, d.id));
        v
    }

    fn root(&self, tm: i64) -> i64 {
        let mut t = tm;
        for _ in 0..64 {
            match self.plan.joins.get(&t) {
                Some(&j) if j != t => t = j,
                _ => break,
            }
        }
        t
    }
}

impl import::Sink for ReplaySink<'_> {
    fn car(&mut self, tm: &TmCar) -> Result<Option<Target>, SinkError> {
        let mine = self.plan.mine.contains(&self.root(tm.tm_id));
        Ok(mine.then(|| Target { car: self.plan.car, state: Default::default() }))
    }

    fn batch(&mut self, car: CarId, batch: Batch) -> Result<(), SinkError> {
        match batch {
            Batch::Drives(rows) => {
                for r in rows {
                    let mut d = r.drive;
                    d.id = imported_id_of(self.source, car, r.tm_id);
                    d.car = car;
                    d.source = self.source;
                    self.drives.insert(d.id, d);
                }
            }
            Batch::Removed(rows) => {
                for (t, id) in rows {
                    if t == import::Table::Drives {
                        self.drives.remove(&imported_id_of(self.source, car, id));
                    }
                }
            }
            Batch::Cleared(spans) => {
                for (a, b) in spans {
                    self.drives.retain(|_, d| d.start_ms < a || d.start_ms > b);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn time_zone(&mut self, car: CarId) -> Option<String> {
        self.engine.inner.db.car(car).ok().flatten().map(|c| self.engine.inner.car_zone(&c).0)
    }

    fn places(&mut self, _: Vec<PlaceRow>) -> Result<(), SinkError> {
        Ok(())
    }

    fn addresses(&mut self, _: Vec<AddressRow>) -> Result<(), SinkError> {
        Ok(())
    }

    fn car_done(&mut self, _: CarReport, _: Option<ImportState>) -> Result<(), SinkError> {
        Ok(())
    }
}

/// Writes imported points over the points already there (by time, in the source's own stream), sorted.
pub(crate) fn write_merged<P: db::Point>(db: &db::Db, car: CarId, source: Source, pts: &mut [P]) -> db::Result<()> {
    pts.sort_by_key(|p| p.t_ms());
    let (Some(a), Some(b)) = (pts.first().map(|p| p.t_ms()), pts.last().map(|p| p.t_ms() + 1)) else { return Ok(()) };
    let had = db.read_points::<P>(car, source, a, b)?;
    let merged = merge_points(had, pts);
    db.write_points(car, source, a, b, &merged)
}
