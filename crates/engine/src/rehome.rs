//! Imported rows that went to the wrong car, and the rows that wait for one.
//!
//! An import tells its cars apart by the whole VIN where the source has it: a car keeps a keyed hash of each whole
//! VIN an import brought it (`car/<id>/vin_tags`, never the VIN), so two cars of the same model, year and plant (the
//! same VIN prefix) are never taken for one another. Rows of TeslaFi without a VIN go with a car only when their
//! odometer and time plainly carry on from it ([`import::teslafi::Group`]); otherwise they are held here until an
//! admin chooses their car (Settings: Import), a new car, or to leave them aside.
//!
//! Rows are moved, never deleted: what an owner entered on them (a charge's cost, a drive's purpose and note) goes
//! with them, under the ids they get on the other car. The held rows sit in the engine's sealed key/value store:
//!
//! ```text
//! engine  car/<id>/vin_tags        the keyed hashes of the whole VINs imports brought the car
//! engine  held/index               the held lots, by id
//! engine  held/<id>/meta           what a lot holds and where it came from
//! engine  held/<id>/part/<n>       its rows (a [`Bundle`] each)
//! ```

use crate::importer::imported_id_of;
use crate::query::logbook::{self, OwnPurpose};
use crate::records::{self, CostEntry, NS};
use crate::{Engine, Event, Result, now_ms};
use db::{Db, Item, Query, StateRow, UpdateRow};
use model::{Car, CarId, Charge, ChargePoint, Drive, Sample, Source};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// What the keyed hash of a whole VIN is made for (its key is the vault's own for this).
pub const VIN_TAG_PURPOSE: &[u8] = b"sentry-mate import vin tag v1";
/// Samples in one held part at most (a part is one sealed record).
const PART_SAMPLES: usize = 20_000;
/// Rows read at once from a table.
const PAGE: usize = db::MAX_PAGE;

/// The keyed hash of a whole VIN.
pub fn vin_tag(db: &Db, vin: &import::FullVin) -> String {
    db.keyed_tag(VIN_TAG_PURPOSE, vin.as_str())
}

fn tags_key(car: CarId) -> String {
    format!("car/{car}/vin_tags")
}

/// The keyed hashes of the whole VINs imports brought a car (none: its whole VIN is not known).
pub fn vin_tags(db: &Db, car: CarId) -> Result<BTreeSet<String>> {
    Ok(records::get(db, NS, &tags_key(car))?.unwrap_or_default())
}

/// Notes that a car's whole VIN has this tag.
pub fn add_vin_tag(db: &Db, car: CarId, tag: &str) -> Result<()> {
    if tag.is_empty() {
        return Ok(());
    }
    let mut t = vin_tags(db, car)?;
    if t.insert(tag.to_string()) {
        records::put(db, NS, &tags_key(car), &t)?;
    }
    Ok(())
}

/// Every car here, from the fleet list.
pub(crate) fn all_cars(db: &Db) -> Result<Vec<Car>> {
    let mut q = db::FleetQuery { search: None, limit: 500, after: None };
    let mut out = Vec::new();
    for _ in 0..100_000 {
        let page = db.fleet(&q)?;
        out.extend(page.cars.into_iter().map(|e| e.car));
        match page.next {
            Some(n) => q.after = Some(n),
            None => break,
        }
    }
    Ok(out)
}

/// Every car's id.
pub fn car_ids(e: &Engine) -> Result<Vec<CarId>> {
    Ok(all_cars(e.db())?.into_iter().map(|c| c.id).collect())
}

/// A new car for rows that waited for one, named as they were.
pub fn new_car(e: &Engine, name: &str) -> Result<CarId> {
    let c = e.db().create_car(Car {
        id: 0,
        name: name.to_string(),
        vehicle_keys: Vec::new(),
        preferred_key: None,
        vin: None,
        profile_setting: None,
        time_zone: e.inner.default_zone(),
        time_zone_typed: Some(false),
        efficiency_wh_km: None,
        created_ms: now_ms(),
        inputs: Default::default(),
    })?;
    e.emit(Event::Fleet { car: c.id, change: "added", name: Some(c.name.clone()) });
    Ok(c.id)
}

/// The cars here an imported car with this VIN prefix (and whole VIN's `tag`) may be: the car that has the tag; else
/// the cars with the prefix whose whole VIN is not known to be another. A prefix alone never matches a car whose
/// whole VIN differs.
pub fn cars_for_vin(db: &Db, prefix: &str, tag: Option<&str>) -> Result<Vec<CarId>> {
    if prefix.is_empty() {
        return Ok(Vec::new());
    }
    let mut same = Vec::new();
    for c in all_cars(db)? {
        if c.vin.as_ref().is_some_and(|v| v.vin_prefix == prefix) {
            same.push((c.id, vin_tags(db, c.id)?));
        }
    }
    if let Some(t) = tag
        && let Some((c, _)) = same.iter().find(|(_, tags)| tags.contains(t))
    {
        return Ok(vec![*c]);
    }
    Ok(same.into_iter().filter(|(_, tags)| tag.is_none() || tags.is_empty()).map(|(c, _)| c).collect())
}

/// Rows of one import source taken off a car (or held), with what the owner entered on them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bundle {
    pub source: Source,
    /// The stretches (inclusive) the rows were taken from.
    pub spans: Vec<(u64, u64)>,
    #[serde(default)]
    pub drives: Vec<Drive>,
    #[serde(default)]
    pub charges: Vec<Charge>,
    #[serde(default)]
    pub states: Vec<StateRow>,
    #[serde(default)]
    pub updates: Vec<UpdateRow>,
    #[serde(default)]
    pub samples: Vec<Sample>,
    #[serde(default)]
    pub points: Vec<ChargePoint>,
    /// Charge id, its start, the cost kept with it.
    #[serde(default)]
    pub costs: Vec<(u64, u64, CostEntry)>,
    /// Drive id, its start, the owner's purpose.
    #[serde(default)]
    pub purposes: Vec<(u64, u64, OwnPurpose)>,
    /// Charge id, the cost it kept before the owner typed one (comes back if the typed one is taken off).
    #[serde(default)]
    pub costs_before: Vec<(u64, CostEntry)>,
}

impl Bundle {
    pub fn new(source: Source) -> Bundle {
        Bundle {
            source,
            spans: Vec::new(),
            drives: Vec::new(),
            charges: Vec::new(),
            states: Vec::new(),
            updates: Vec::new(),
            samples: Vec::new(),
            points: Vec::new(),
            costs: Vec::new(),
            purposes: Vec::new(),
            costs_before: Vec::new(),
        }
    }

    /// Rows of every kind.
    pub fn rows(&self) -> u64 {
        (self.drives.len()
            + self.charges.len()
            + self.states.len()
            + self.updates.len()
            + self.samples.len()
            + self.points.len()) as u64
    }

    pub fn is_empty(&self) -> bool {
        self.rows() == 0
    }

    fn span(&self) -> Option<(u64, u64)> {
        let a = self.spans.iter().map(|s| s.0).min()?;
        let b = self.spans.iter().map(|s| s.1).max()?;
        Some((a, b))
    }

    /// The bundle in parts of at most [`PART_SAMPLES`] samples and points each.
    fn split(mut self) -> Vec<Bundle> {
        if self.samples.len() <= PART_SAMPLES && self.points.len() <= PART_SAMPLES {
            return vec![self];
        }
        let mut out = Vec::new();
        let samples = std::mem::take(&mut self.samples);
        let points = std::mem::take(&mut self.points);
        let spans = self.spans.clone();
        out.push(self);
        for c in samples.chunks(PART_SAMPLES) {
            let mut b = Bundle::new(out[0].source);
            b.spans = spans.clone();
            b.samples = c.to_vec();
            out.push(b);
        }
        for c in points.chunks(PART_SAMPLES) {
            let mut b = Bundle::new(out[0].source);
            b.spans = spans.clone();
            b.points = c.to_vec();
            out.push(b);
        }
        out
    }
}

/// Where [`crate::costs`] keeps the cost a charge had before one was typed.
fn before_key(car: CarId, charge: u64) -> String {
    format!("car/{car}/cost-before-typed/{charge}")
}

/// The import's own id of an imported row (what [`imported_id_of`] put in its low bits).
fn own_id(id: u64) -> i64 {
    i64::try_from(id & 0xFF_FFFF_FFFF).unwrap_or(0)
}

/// Every row of `T` of `source` on `car` that starts in `[a, b]`.
fn rows_in<T: Item>(db: &Db, car: CarId, source: Source, a: u64, b: u64) -> Result<Vec<T>> {
    let mut out = Vec::new();
    let mut q = Query::between(a, b.saturating_add(1)).oldest_first().limit(PAGE);
    q.source = Some(source);
    loop {
        let page = db.list::<T>(car, &q)?;
        out.extend(page.items.into_iter().filter(|r| r.source() == source && r.start_ms() >= a && r.start_ms() <= b));
        match page.next {
            Some(n) => q.after = Some(n),
            None => break,
        }
    }
    Ok(out)
}

/// Reads the rows of `source` on `car` that start in the spans (inclusive), with their costs and purposes; nothing is
/// changed.
pub fn collect(db: &Db, car: CarId, source: Source, spans: &[(u64, u64)]) -> Result<Bundle> {
    let mut b = Bundle::new(source);
    let mut seen: BTreeSet<(u8, u64)> = BTreeSet::new();
    for &(a, z) in spans {
        b.spans.push((a, z));
        for d in rows_in::<Drive>(db, car, source, a, z)? {
            if seen.insert((0, d.id)) {
                b.drives.push(d);
            }
        }
        for c in rows_in::<Charge>(db, car, source, a, z)? {
            if seen.insert((1, c.id)) {
                if let Some(cost) = records::cost(db, car, c.id, c.start_ms)? {
                    b.costs.push((c.id, c.start_ms, cost));
                }
                if let Some(before) = records::get::<CostEntry>(db, NS, &before_key(car, c.id))? {
                    b.costs_before.push((c.id, before));
                }
                b.charges.push(c);
            }
        }
        for s in rows_in::<StateRow>(db, car, source, a, z)? {
            if seen.insert((2, s.id)) {
                b.states.push(s);
            }
        }
        for u in rows_in::<UpdateRow>(db, car, source, a, z)? {
            if seen.insert((3, u.id)) {
                b.updates.push(u);
            }
        }
        b.samples.extend(db.read_points::<Sample>(car, source, a, z.saturating_add(1))?);
        b.points.extend(db.read_points::<ChargePoint>(car, source, a, z.saturating_add(1))?);
    }
    let drives: Vec<(u64, u64)> = b.drives.iter().map(|d| (d.id, d.start_ms)).collect();
    b.purposes = logbook::read_purposes(db, car, &drives)?;
    b.samples.sort_by_key(|p| p.t_ms);
    b.samples.dedup_by_key(|p| p.t_ms);
    b.points.sort_by_key(|p| p.t_ms);
    b.points.dedup_by_key(|p| p.t_ms);
    Ok(b)
}

/// Takes what [`collect`] read off `car` (once it is stored elsewhere).
fn detach(db: &Db, car: CarId, b: &Bundle) -> Result<()> {
    db.delete::<Drive>(car, &b.drives.iter().map(|d| d.id).collect::<Vec<_>>())?;
    db.delete::<Charge>(car, &b.charges.iter().map(|c| c.id).collect::<Vec<_>>())?;
    db.delete::<StateRow>(car, &b.states.iter().map(|s| s.id).collect::<Vec<_>>())?;
    db.delete::<UpdateRow>(car, &b.updates.iter().map(|u| u.id).collect::<Vec<_>>())?;
    for &(a, z) in &b.spans {
        db.write_points::<Sample>(car, b.source, a, z.saturating_add(1), &[])?;
        db.write_points::<ChargePoint>(car, b.source, a, z.saturating_add(1), &[])?;
    }
    for (id, start, _) in &b.costs {
        records::put_cost(db, car, *id, *start, None)?;
    }
    for (id, _) in &b.costs_before {
        db.kv().delete(NS, &before_key(car, *id))?;
    }
    let drives: Vec<(u64, u64)> = b.purposes.iter().map(|(id, s, _)| (*id, *s)).collect();
    logbook::take_purposes(db, car, &drives)?;
    Ok(())
}

/// Stores a bundle on `car`: each row under the id it has there, with its cost and purpose.
fn attach(db: &Db, car: CarId, b: &Bundle) -> Result<()> {
    let src = b.source;
    let id = |old: u64| imported_id_of(src, car, own_id(old));
    let drives: Vec<Drive> = b
        .drives
        .iter()
        .cloned()
        .map(|mut d| {
            d.id = id(d.id);
            d.car = car;
            d
        })
        .collect();
    db.upsert(car, &drives)?;
    let charges: Vec<Charge> = b
        .charges
        .iter()
        .cloned()
        .map(|mut c| {
            c.id = id(c.id);
            c.car = car;
            c
        })
        .collect();
    db.upsert(car, &charges)?;
    let states: Vec<StateRow> = b
        .states
        .iter()
        .cloned()
        .map(|mut s| {
            s.id = id(s.id);
            s.span.car = car;
            s
        })
        .collect();
    db.upsert(car, &states)?;
    let updates: Vec<UpdateRow> = b
        .updates
        .iter()
        .cloned()
        .map(|mut u| {
            u.id = id(u.id);
            u.update.car = car;
            u
        })
        .collect();
    db.upsert(car, &updates)?;
    if !b.samples.is_empty() {
        crate::importer::write_merged(db, car, src, &mut b.samples.clone())?;
    }
    if !b.points.is_empty() {
        crate::importer::write_merged(db, car, src, &mut b.points.clone())?;
    }
    for (cid, start, cost) in &b.costs {
        records::put_cost(db, car, id(*cid), *start, Some(cost.clone()))?;
    }
    for (cid, before) in &b.costs_before {
        records::put(db, NS, &before_key(car, id(*cid)), before)?;
    }
    let purposes: Vec<(u64, u64, OwnPurpose)> =
        b.purposes.iter().map(|(did, s, p)| (id(*did), *s, p.clone())).collect();
    logbook::put_purposes(db, car, &purposes)?;
    Ok(())
}

/// What a move took.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Moved {
    pub rows: u64,
    pub drives: u64,
    pub charges: u64,
    /// Purposes and costs the owner had entered that went with them.
    pub owner_entries: u64,
    pub first_ms: Option<u64>,
    pub last_ms: Option<u64>,
}

impl Moved {
    fn of(b: &Bundle) -> Moved {
        let span = b.span();
        Moved {
            rows: b.rows(),
            drives: b.drives.len() as u64,
            charges: b.charges.len() as u64,
            owner_entries: (b.costs.len() + b.purposes.len()) as u64,
            first_ms: span.map(|s| s.0),
            last_ms: span.map(|s| s.1),
        }
    }

    fn add(&mut self, o: &Moved) {
        self.rows += o.rows;
        self.drives += o.drives;
        self.charges += o.charges;
        self.owner_entries += o.owner_entries;
        self.first_ms = match (self.first_ms, o.first_ms) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.last_ms = self.last_ms.max(o.last_ms);
    }

    /// `N rows (D drives, C charges)`.
    pub fn words(&self) -> String {
        let mut s = format!("{} rows", self.rows);
        let mut parts = Vec::new();
        if self.drives > 0 {
            parts.push(format!("{} drive{}", self.drives, if self.drives == 1 { "" } else { "s" }));
        }
        if self.charges > 0 {
            parts.push(format!("{} charge{}", self.charges, if self.charges == 1 { "" } else { "s" }));
        }
        if !parts.is_empty() {
            s.push_str(&format!(" ({})", parts.join(", ")));
        }
        s
    }
}

/// A lot of rows that waits for a car.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Held {
    /// `h1`, `h2`...
    pub id: String,
    /// The import it came from (`/imports` id), if any.
    pub import: Option<String>,
    /// The import's own number for the car of the file it is ([`import::TmCar::tm_id`]), if any.
    pub tm_car: Option<i64>,
    pub source: Source,
    /// The name the rows carried.
    pub name: Option<String>,
    /// The car the rows were taken off, when they were moved here from one.
    pub from_car: Option<CarId>,
    /// `waiting`, `ignored` (left aside, still kept) or `assigned`.
    pub status: String,
    pub to_car: Option<CarId>,
    pub parts: u32,
    pub held: Moved,
    pub first_odo_km: Option<f64>,
    pub last_odo_km: Option<f64>,
    /// Why the rows wait (`no_vin`, `odometer`).
    pub why: String,
    pub at_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Index {
    next: u64,
    /// `import/tm` → id, for the lots an import holds.
    by_import: BTreeMap<String, String>,
    ids: BTreeSet<String>,
}

fn index(db: &Db) -> Result<Index> {
    Ok(records::get(db, NS, "held/index")?.unwrap_or_default())
}

fn meta_key(id: &str) -> String {
    format!("held/{id}/meta")
}

fn part_key(id: &str, n: u32) -> String {
    format!("held/{id}/part/{n}")
}

static HELD: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn holding() -> std::sync::MutexGuard<'static, ()> {
    HELD.lock().unwrap_or_else(|e| e.into_inner())
}

/// A held lot.
pub fn held(db: &Db, id: &str) -> Result<Option<Held>> {
    records::get(db, NS, &meta_key(id))
}

/// Every held lot, oldest first.
pub fn held_all(db: &Db) -> Result<Vec<Held>> {
    let ix = index(db)?;
    let mut out = Vec::new();
    for id in &ix.ids {
        if let Some(h) = held(db, id)? {
            out.push(h);
        }
    }
    out.sort_by_key(|h| (h.at_ms, h.id.trim_start_matches('h').parse::<u64>().unwrap_or(0)));
    Ok(out)
}

/// The drives held lots keep, as (source, start, end, lot): rows that left a car and wait for one.
pub fn held_drive_spans(db: &Db) -> Result<Vec<(Source, u64, u64, String)>> {
    let mut out = Vec::new();
    for h in held_all(db)?.into_iter().filter(|h| h.status != "assigned") {
        for n in 0..h.parts {
            if let Some(b) = records::get::<Bundle>(db, NS, &part_key(&h.id, n))? {
                out.extend(b.drives.iter().map(|d| (b.source, d.start_ms, d.end_ms, h.id.clone())));
            }
        }
    }
    Ok(out)
}

/// The lots an import holds.
pub fn held_of_import(db: &Db, import: &str) -> Result<Vec<Held>> {
    Ok(held_all(db)?.into_iter().filter(|h| h.import.as_deref() == Some(import)).collect())
}

/// What a new lot is.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HoldAs {
    pub import: Option<String>,
    pub tm_car: Option<i64>,
    pub name: Option<String>,
    pub from_car: Option<CarId>,
    pub first_odo_km: Option<f64>,
    pub last_odo_km: Option<f64>,
    pub why: String,
}

/// Opens a lot for an import's car of the file, or the one it already has (emptied first when `fresh`: the import
/// is read again and brings its rows again).
pub fn open_lot(db: &Db, source: Source, how: &HoldAs, fresh: bool) -> Result<String> {
    let _h = holding();
    let mut ix = index(db)?;
    let key = match (&how.import, how.tm_car) {
        (Some(i), Some(t)) => Some(format!("{i}/{t}")),
        _ => None,
    };
    if let Some(id) = key.as_ref().and_then(|k| ix.by_import.get(k)).cloned()
        && let Some(mut h) = held(db, &id)?
        && h.status != "assigned"
    {
        if fresh {
            for n in 0..h.parts {
                db.kv().delete(NS, &part_key(&id, n))?;
            }
            h.parts = 0;
            h.held = Moved::default();
            h.name = how.name.clone().or(h.name);
            h.first_odo_km = how.first_odo_km.or(h.first_odo_km);
            h.last_odo_km = how.last_odo_km.or(h.last_odo_km);
            records::put(db, NS, &meta_key(&id), &h)?;
        }
        return Ok(id);
    }
    ix.next += 1;
    let id = format!("h{}", ix.next);
    let h = Held {
        id: id.clone(),
        import: how.import.clone(),
        tm_car: how.tm_car,
        source,
        name: how.name.clone(),
        from_car: how.from_car,
        status: "waiting".into(),
        to_car: None,
        parts: 0,
        held: Moved::default(),
        first_odo_km: how.first_odo_km,
        last_odo_km: how.last_odo_km,
        why: how.why.clone(),
        at_ms: now_ms(),
    };
    records::put(db, NS, &meta_key(&id), &h)?;
    ix.ids.insert(id.clone());
    if let Some(k) = key {
        ix.by_import.insert(k, id.clone());
    }
    records::put(db, NS, "held/index", &ix)?;
    Ok(id)
}

/// Adds rows to a lot.
pub fn hold(db: &Db, id: &str, b: Bundle) -> Result<()> {
    if b.is_empty() && b.costs.is_empty() && b.purposes.is_empty() {
        return Ok(());
    }
    let _h = holding();
    let Some(mut h) = held(db, id)? else { return Err(crate::Error::Damaged(format!("no held lot {id}"))) };
    for part in b.split() {
        h.held.add(&Moved::of(&part));
        records::put(db, NS, &part_key(id, h.parts), &part)?;
        h.parts += 1;
    }
    records::put(db, NS, &meta_key(id), &h)
}

/// Fills in a lot's name and odometer once its import is done.
pub fn describe_lot(db: &Db, id: &str, name: Option<String>, odo: (Option<f64>, Option<f64>)) -> Result<()> {
    let _h = holding();
    let Some(mut h) = held(db, id)? else { return Ok(()) };
    h.name = name.or(h.name);
    h.first_odo_km = odo.0.or(h.first_odo_km);
    h.last_odo_km = odo.1.or(h.last_odo_km);
    records::put(db, NS, &meta_key(id), &h)
}

/// Where rows go.
#[derive(Debug, Clone, PartialEq)]
pub enum Dest {
    Car(CarId),
    /// Held until a car is chosen.
    Hold(HoldAs),
}

/// One move of rows, as `/imports` lists it and the audit log tells it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Move {
    pub from_car: Option<CarId>,
    /// The car they went to, or `None` (held: `held` names the lot).
    pub to_car: Option<CarId>,
    pub held: Option<String>,
    pub moved: Moved,
    pub at_ms: u64,
}

/// Moves the rows of `source` that start in `spans` (inclusive) from `from` to `to`, what the owner entered on them
/// with them. The rows are stored at their new place before they are taken off the old one.
pub fn move_rows(e: &Engine, from: CarId, to: &Dest, source: Source, spans: &[(u64, u64)]) -> Result<Option<Move>> {
    let db = e.db();
    let b = collect(db, from, source, spans)?;
    if b.is_empty() {
        return Ok(None);
    }
    let moved = Moved::of(&b);
    let (to_car, lot) = match to {
        Dest::Car(c) => {
            attach(db, *c, &b)?;
            (Some(*c), None)
        }
        Dest::Hold(how) => {
            let how = HoldAs { from_car: Some(from), ..how.clone() };
            let id = open_lot(db, source, &how, false)?;
            hold(db, &id, b.clone())?;
            (None, Some(id))
        }
    };
    detach(db, from, &b)?;
    let (a, z) = b.span().unwrap_or((0, u64::MAX));
    for c in std::iter::once(from).chain(to_car) {
        e.emit(Event::Rows {
            car: c,
            kinds: vec!["drives", "charges", "updates", "visits", "states"],
            from_ms: a,
            to_ms: z.saturating_add(1),
        });
    }
    Ok(Some(Move { from_car: Some(from), to_car, held: lot, moved, at_ms: now_ms() }))
}

/// Puts a held lot's rows on `car` (the lot is then `assigned`, its parts gone to the car).
pub fn assign(e: &Engine, id: &str, car: CarId) -> Result<Move> {
    let db = e.db();
    if db.car(car)?.is_none() {
        return Err(crate::Error::Damaged(format!("no car {car}")));
    }
    let _h = holding();
    let Some(mut h) = held(db, id)? else { return Err(crate::Error::Damaged(format!("no held lot {id}"))) };
    if h.status == "assigned" {
        return Err(crate::Error::Damaged(format!("held lot {id} went to a car already")));
    }
    let mut moved = Moved::default();
    let mut all = Bundle::new(h.source);
    for n in 0..h.parts {
        let Some(part) = records::get::<Bundle>(db, NS, &part_key(id, n))? else { continue };
        attach(db, car, &part)?;
        moved.add(&Moved::of(&part));
        all.spans.extend(part.spans.iter().copied());
    }
    for n in 0..h.parts {
        db.kv().delete(NS, &part_key(id, n))?;
    }
    h.status = "assigned".into();
    h.to_car = Some(car);
    records::put(db, NS, &meta_key(id), &h)?;
    let (a, z) = all.span().unwrap_or((0, u64::MAX));
    e.emit(Event::Rows {
        car,
        kinds: vec!["drives", "charges", "updates", "visits", "states"],
        from_ms: a,
        to_ms: z.saturating_add(1),
    });
    e.warm(Some(car));
    Ok(Move { from_car: None, to_car: Some(car), held: Some(id.to_string()), moved, at_ms: now_ms() })
}

/// Leaves a held lot aside (`ignore`), or takes it back to wait for a car. Its rows stay kept either way.
pub fn set_ignored(db: &Db, id: &str, ignored: bool) -> Result<Held> {
    let _h = holding();
    let Some(mut h) = held(db, id)? else { return Err(crate::Error::Damaged(format!("no held lot {id}"))) };
    if h.status != "assigned" {
        h.status = if ignored { "ignored" } else { "waiting" }.into();
        records::put(db, NS, &meta_key(id), &h)?;
    }
    Ok(h)
}

/// A car of TeslaFi's files that the rows put nowhere for sure, as a re-run of the matching sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Rehome {
    /// Its number in the re-run.
    pub tm_car: i64,
    pub name: Option<String>,
    /// Its spells that meet no car with a VIN of the files (rows elsewhere in time are mixed with that car's, and stay).
    pub spans: Vec<(u64, u64)>,
    pub first_odo_km: Option<f64>,
    pub last_odo_km: Option<f64>,
    /// The car here it plainly is (its name, odometer and time), else none: held.
    pub to: Option<CarId>,
}

/// What a re-run of TeslaFi's matching over an import's files says should move: the groups that need a car (each
/// lead with the groups that follow it), with the spells of theirs that no car with a VIN of the files has rows in.
pub fn rehome_plan(e: &Engine, groups: &[import::teslafi::Group]) -> Result<Vec<Rehome>> {
    let with_vin: Vec<&import::teslafi::Group> = groups.iter().filter(|g| g.vin).collect();
    let mut out: Vec<Rehome> = Vec::new();
    for lead in groups.iter().filter(|g| g.needs_match && g.joins.is_none()) {
        let members: Vec<&import::teslafi::Group> = groups
            .iter()
            .filter(|g| g.tm_car == lead.tm_car || (g.needs_match && g.joins == Some(lead.tm_car)))
            .collect();
        let mut spans = Vec::new();
        let mut merged = import::teslafi::Group { needs_match: true, ..lead.clone() };
        merged.spells.clear();
        for m in &members {
            for sp in &m.spells {
                merged.spells.push(*sp);
                let meets = with_vin
                    .iter()
                    .any(|v| v.spells.iter().any(|o| o.first_ms <= sp.last_ms && o.last_ms >= sp.first_ms));
                if !meets {
                    spans.push((sp.first_ms, sp.last_ms));
                }
            }
        }
        merged.spells.sort_by_key(|s| s.first_ms);
        let first_odo = merged.spells.iter().find_map(|s| s.first_odo_km);
        let last_odo = merged.spells.iter().rev().find_map(|s| s.last_odo_km);
        let to = by_name(e, lead.name.as_deref(), &merged)?;
        out.push(Rehome {
            tm_car: lead.tm_car,
            name: lead.name.clone(),
            spans,
            first_odo_km: first_odo,
            last_odo_km: last_odo,
            to,
        });
    }
    Ok(out)
}

/// The one car here named as the rows are whose own history the rows' odometer and time fit into (no drive of its
/// own in their time; its odometer before them no higher, after them no lower).
pub fn by_name(e: &Engine, name: Option<&str>, g: &import::teslafi::Group) -> Result<Option<CarId>> {
    let Some(name) = name.map(|n| n.trim().to_lowercase()).filter(|n| !n.is_empty()) else { return Ok(None) };
    let (Some(a), Some(z)) = (g.spells.iter().map(|s| s.first_ms).min(), g.spells.iter().map(|s| s.last_ms).max())
    else {
        return Ok(None);
    };
    let db = e.db();
    let mut fits = Vec::new();
    for c in all_cars(db)?.into_iter().filter(|c| c.name.trim().to_lowercase() == name) {
        let within = db.list::<Drive>(c.id, &Query::between(a, z.saturating_add(1)).limit(1))?;
        if !within.items.is_empty() {
            continue;
        }
        let before = db.list::<Drive>(c.id, &Query::between(0, a).limit(1))?.items.into_iter().next();
        let after = db
            .list::<Drive>(c.id, &Query::between(z.saturating_add(1), u64::MAX).oldest_first().limit(1))?
            .items
            .into_iter()
            .next();
        let first = g.spells.iter().find_map(|s| s.first_odo_km);
        let last = g.spells.iter().rev().find_map(|s| s.last_odo_km);
        let back = import::teslafi::ODO_BACK_KM;
        let ok_before = match (before.and_then(|d| d.end_odometer_km), first) {
            (Some(b), Some(f)) => f >= b - back,
            _ => true,
        };
        let ok_after = match (after.and_then(|d| d.start_odometer_km), last) {
            (Some(n), Some(l)) => n >= l - back,
            _ => true,
        };
        if ok_before && ok_after {
            fits.push(c.id);
        }
    }
    Ok(if fits.len() == 1 { fits.first().copied() } else { None })
}

/// Drives in a row that the car's own odometer shows can't be its own, and the stretch they cover.
#[derive(Debug, Clone, PartialEq)]
pub struct Stray {
    pub source: Source,
    pub span: (u64, u64),
    pub drives: usize,
    pub first_odo_km: Option<f64>,
    pub last_odo_km: Option<f64>,
}

/// Drives in a row this short are never taken for another car's (one odd reading is not another car).
const STRAY_MIN_DRIVES: usize = 3;

/// Without the import's files: the car's imported drives whose odometer reads higher than the car's own later
/// drives (an odometer never runs back), [`STRAY_MIN_DRIVES`] or more in a row, of one source. Each run's stretch
/// reaches to the drives before and after it, never over them.
pub fn strays(e: &Engine, car: CarId) -> Result<Vec<Stray>> {
    let db = e.db();
    let mut drives: Vec<Drive> = Vec::new();
    let mut q = Query::between(0, u64::MAX).oldest_first().limit(PAGE);
    loop {
        let page = db.list::<Drive>(car, &q)?;
        drives.extend(page.items);
        match page.next {
            Some(n) => q.after = Some(n),
            None => break,
        }
    }
    let back = import::teslafi::ODO_BACK_KM;
    // the lowest odometer of the drives after each one, from the newest back: a drive whose odometer reads higher
    // than a later drive's (by more than a rounding) is not this car's; one odd low reading must not mark every drive
    // before it, so the low is taken over the next drives' median of three
    let odo = |d: &Drive| d.start_odometer_km.or(d.end_odometer_km);
    let mut low_after: Vec<Option<f64>> = vec![None; drives.len()];
    let mut low: Option<f64> = None;
    for i in (0..drives.len()).rev() {
        low_after[i] = low;
        let mut next: Vec<f64> = drives[i..drives.len().min(i + 3)].iter().filter_map(odo).collect();
        next.sort_by(f64::total_cmp);
        if let Some(m) = next.get(next.len() / 2) {
            low = Some(low.map_or(*m, |l: f64| l.min(*m)));
        }
    }
    let bad: Vec<bool> = drives
        .iter()
        .zip(&low_after)
        .map(|(d, l)| d.source.is_imported() && matches!((odo(d), l), (Some(o), Some(l)) if o > l + back))
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < drives.len() {
        if !bad[i] {
            i += 1;
            continue;
        }
        let src = drives[i].source;
        let mut j = i;
        while j + 1 < drives.len() && bad[j + 1] && drives[j + 1].source == src {
            j += 1;
        }
        if j + 1 - i >= STRAY_MIN_DRIVES {
            let lo = if i > 0 { drives[i - 1].end_ms.saturating_add(1) } else { 0 };
            let hi = drives.get(j + 1).map_or(u64::MAX - 1, |d| d.start_ms.saturating_sub(1));
            let span = (
                lo.max(drives[i].start_ms.saturating_sub(6 * 3_600_000)),
                hi.min(drives[j].end_ms.saturating_add(6 * 3_600_000)),
            );
            out.push(Stray {
                source: src,
                span,
                drives: j + 1 - i,
                first_odo_km: odo(&drives[i]),
                last_odo_km: drives[j].end_odometer_km.or(odo(&drives[j])),
            });
        }
        i = j + 1;
    }
    Ok(out)
}

/// Without the import's files: moves each car's [`strays`] to a lot of their own, to wait for a car.
pub fn rehome_strays(e: &Engine, car: CarId) -> Result<Vec<Move>> {
    let mut out = Vec::new();
    for st in strays(e, car)? {
        let how = HoldAs {
            from_car: Some(car),
            first_odo_km: st.first_odo_km,
            last_odo_km: st.last_odo_km,
            why: "odometer".into(),
            ..Default::default()
        };
        if let Some(m) = move_rows(e, car, &Dest::Hold(how), st.source, &[st.span])? {
            out.push(m);
        }
    }
    Ok(out)
}

/// The key/value record of what was re-homed once (so a restart does not do it again).
pub fn done_once(db: &Db, what: &str) -> Result<bool> {
    Ok(records::get::<u64>(db, NS, &format!("rehome/{what}"))?.is_some())
}

pub fn mark_done(db: &Db, what: &str) -> Result<()> {
    records::put(db, NS, &format!("rehome/{what}"), &now_ms())
}
