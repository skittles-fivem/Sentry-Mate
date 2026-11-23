//! Places (docs/ui-api.md, "Places"): `/places` with their counts (matched live), one place, `/places/at`,
//! `/places/suggestions`, `/places/preview`, and the changes: create, change and delete (each answering its effect, the
//! same counts as the preview) and repricing past charges.
//!
//! Matching is live ([`crate::placing`]): every read matches drives, charges and stops against the places as they are
//! now, so a saved change renames every row at once. Costs stay put: before a change is saved, every ended charge it
//! moves (or whose place's price it changes) keeps the cost it shows now ([`costs::settle`]); a price reaches past
//! charges only through [`reprice`].
//!
//! Everything here streams the history a page of rows at a time ([`each`]): counts, the preview and repricing keep
//! only running totals and at most 8 examples, suggestions keep one group per spot.

use super::drives::each;
use super::system::Shown;
use super::{Address, CarRef, List, Money, Paging, QResult, QueryError, paginate, place_ref, r};
use crate::placing::{self, PlaceExtra, Places};
use crate::records;
use crate::{Engine, costs, now_ms, tz};
use model::{CarId, Charge, Drive, Place, PlaceId, PlaceKind, PricePlan, Source};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::broadcast;

/// The kinds a place may be.
pub const KINDS: &[&str] = &["home", "work", "charger", "service", "other"];
/// A new place's radius when none is given, m.
pub const NEW_RADIUS_M: f64 = 100.0;
/// At most this many examples in a preview, an effect or a reprice.
const EXAMPLES: usize = 8;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Counts {
    pub drives: u64,
    pub charges: u64,
    pub visits: u64,
}

impl Counts {
    fn used(&self) -> bool {
        self.drives + self.charges + self.visits > 0
    }
}

/// A time-of-use rate as the API gives and takes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rate {
    pub name: String,
    pub per_kwh: Option<f64>,
    pub per_minute: Option<f64>,
    /// `all`, `mon-fri`, `sat,sun` style.
    pub days: String,
    /// `HH:MM`; `to` may be `24:00` (the end of the day).
    pub from: String,
    pub to: String,
}

/// A place's price as the API gives and takes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub currency: String,
    pub per_kwh: Option<f64>,
    pub per_minute: Option<f64>,
    pub rates: Vec<Rate>,
    pub session_fee: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlaceView {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub lat: f64,
    pub lon: f64,
    pub radius_m: f64,
    pub time_zone: Option<String>,
    pub price: Option<Price>,
    pub counts: Counts,
    pub last_visit_ms: Option<u64>,
    pub address: Option<Address>,
    pub created_ms: Option<u64>,
    pub version: u64,
}

const DAY_NAMES: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// `0,1,2,3,4` → `mon-fri` style (days 0 = Monday).
fn days_word(days: &[u8]) -> String {
    let mut d: Vec<u8> = days.iter().copied().filter(|&x| x < 7).collect();
    d.sort_unstable();
    d.dedup();
    if d.len() == 7 || d.is_empty() {
        return "all".into();
    }
    if d.windows(2).all(|w| w[1] == w[0] + 1) && d.len() > 2 {
        return format!("{}-{}", DAY_NAMES[d[0] as usize], DAY_NAMES[d[d.len() - 1] as usize]);
    }
    d.iter().map(|&x| DAY_NAMES[x as usize]).collect::<Vec<_>>().join(",")
}

/// `mon-fri`, `sat,sun`, `fri-mon` (wrapping) or `all` → days 0 = Monday, sorted, each once.
pub fn parse_days(s: &str) -> Option<Vec<u8>> {
    let s = s.trim().to_ascii_lowercase();
    if s == "all" {
        return Some((0..7).collect());
    }
    let day = |w: &str| DAY_NAMES.iter().position(|d| *d == w.trim()).map(|i| i as u8);
    let mut out = Vec::new();
    for part in s.split(',') {
        let ws: Vec<&str> = part.split('-').collect();
        match ws.as_slice() {
            [one] => out.push(day(one)?),
            [a, b] => {
                let (mut x, y) = (day(a)?, day(b)?);
                loop {
                    out.push(x);
                    if x == y {
                        break;
                    }
                    x = (x + 1) % 7;
                }
            }
            _ => return None,
        }
    }
    out.sort_unstable();
    out.dedup();
    (!out.is_empty()).then_some(out)
}

/// The price a place shows: as the owner gave it, else the model's plan (an imported place).
fn price_of(p: &Place, extra: Option<&PlaceExtra>) -> Option<Price> {
    let plan = p.price.as_ref()?;
    if let Some(given) = extra.and_then(|x| x.price.clone()) {
        return Some(given);
    }
    Some(Price {
        currency: plan.currency.clone(),
        per_kwh: Some(plan.per_kwh),
        per_minute: extra.and_then(|x| x.per_minute),
        rates: plan
            .rates
            .iter()
            .map(|rt| Rate {
                name: rt.name.clone(),
                per_kwh: Some(rt.per_kwh),
                per_minute: None,
                days: days_word(&rt.days),
                from: rt.from.clone(),
                to: rt.to.clone(),
            })
            .collect(),
        session_fee: plan.session_fee,
    })
}

pub(crate) fn view(p: &Place, places: &Places, counts: Counts, last: Option<u64>) -> PlaceView {
    let extra = places.extra(p.id);
    PlaceView {
        id: place_ref(p.id),
        name: p.name.clone(),
        kind: places.kind_word(p),
        lat: p.lat,
        lon: p.lon,
        radius_m: p.radius_m,
        time_zone: extra.and_then(|x| x.time_zone.clone()),
        price: price_of(p, extra),
        counts,
        last_visit_ms: last,
        address: extra
            .and_then(|x| x.address.clone())
            .map(|short| Address { short: Some(short), ..Address::default() }),
        created_ms: extra.and_then(|x| x.created_ms),
        version: extra.map_or(1, |x| x.version.max(1)),
    }
}

/// A car's drives oldest first, each with the drive after it (`None` for the last), skipping nothing; the drive going
/// on now (`live`) is given too: callers leave out its end.
fn drives_with_next(
    e: &Engine,
    car: CarId,
    shown: &Shown,
    mut f: impl FnMut(&Drive, Option<&Drive>) -> QResult<()>,
) -> QResult<()> {
    let mut held: Option<Drive> = None;
    each::<Drive>(e, car, 0, u64::MAX, None, shown, false, |d| {
        if let Some(prev) = held.take() {
            f(&prev, Some(&d))?;
        }
        held = Some(d);
        Ok(true)
    })?;
    if let Some(last) = held {
        f(&last, None)?;
    }
    Ok(())
}

/// When the car was last heard of: its newest values, else the newest end of any of its rows (an imported car's
/// history stops there), else 0. A stop with no drive after it lasts until then, and never past now: the server does
/// not know where a car is after its history ends.
fn last_seen(e: &Engine, car: CarId) -> QResult<u64> {
    let now = now_ms();
    let s = e.db().entry(car)?.map(|x| x.summary);
    let at = s.and_then(|s| s.now.map(|n| n.at_ms).or(s.last_activity_ms)).unwrap_or(0);
    Ok(at.min(now))
}

/// The id of the drive going on now.
fn live_drive(e: &Engine, car: CarId) -> QResult<Option<u64>> {
    let c = super::car_of(e, car)?;
    Ok(super::drives::ctx(e, &c)?.live)
}

type ByPlace = BTreeMap<PlaceId, (Counts, Option<u64>)>;

/// One car's counts against one set of places: `open` is the place of the car's last stop (and when it got there),
/// whose last visit is "until the car's newest values", read when asked.
struct CarCounts {
    places: Arc<Places>,
    live: Option<u64>,
    by: ByPlace,
    open: Option<(PlaceId, u64)>,
}

/// The counts worked out, per engine and car, kept until the places change (another [`Places`]), the drive going on
/// changes, or the car's rows do (an `Event::Rows` or `Event::Fleet` for it; every car when events were missed).
struct CountsCache {
    engine: Weak<crate::Inner>,
    events: broadcast::Receiver<crate::Event>,
    cars: HashMap<CarId, Arc<CarCounts>>,
    /// Bumped when a car's counts are dropped, so counts worked out from rows read before are not kept.
    generation: HashMap<CarId, u64>,
}

static COUNTS: Mutex<Vec<CountsCache>> = Mutex::new(Vec::new());

/// The cached counts of `car` if still good (`Err`: the generation to keep newly worked-out ones under).
fn cached(e: &Engine, car: CarId, places: &Arc<Places>, live: Option<u64>) -> std::result::Result<Arc<CarCounts>, u64> {
    let mut all = crate::lock(&COUNTS);
    all.retain(|c| c.engine.strong_count() > 0);
    let at = match all.iter().position(|c| std::ptr::eq(c.engine.as_ptr(), Arc::as_ptr(&e.inner))) {
        Some(i) => i,
        None => {
            all.push(CountsCache {
                engine: Arc::downgrade(&e.inner),
                events: e.subscribe(),
                cars: HashMap::new(),
                generation: HashMap::new(),
            });
            all.len() - 1
        }
    };
    let c = &mut all[at];
    loop {
        match c.events.try_recv() {
            Ok(crate::Event::Rows { car, .. } | crate::Event::Fleet { car, .. }) => {
                c.cars.remove(&car);
                *c.generation.entry(car).or_default() += 1;
            }
            Ok(_) => {}
            Err(broadcast::error::TryRecvError::Lagged(_)) => {
                c.cars.clear();
                for g in c.generation.values_mut() {
                    *g += 1;
                }
            }
            Err(_) => break,
        }
    }
    let generation = c.generation.get(&car).copied().unwrap_or(0);
    match c.cars.get(&car) {
        Some(x) if Arc::ptr_eq(&x.places, places) && x.live == live => Ok(x.clone()),
        _ => Err(generation),
    }
}

fn keep_counts(e: &Engine, car: CarId, generation: u64, counts: Arc<CarCounts>) {
    let mut all = crate::lock(&COUNTS);
    if let Some(c) = all.iter_mut().find(|c| std::ptr::eq(c.engine.as_ptr(), Arc::as_ptr(&e.inner)))
        && c.generation.get(&car).copied().unwrap_or(0) == generation
    {
        c.cars.insert(car, counts);
    }
}

/// One car's counts, from the cache or worked out over its whole history (a page of rows at a time).
fn car_counts(e: &Engine, car: CarId, places: &Arc<Places>) -> QResult<Arc<CarCounts>> {
    let live = live_drive(e, car)?;
    let generation = match cached(e, car, places, live) {
        Ok(x) => return Ok(x),
        Err(g) => g,
    };
    let mut by = ByPlace::new();
    let mut open = None;
    let at = |p: Option<&model::Position>| p.and_then(|p| places.place_at(p.lat, p.lon)).map(|p| p.id);
    let shown = Shown::new(e, car, None)?;
    drives_with_next(e, car, &shown, |d, next| {
        let s = at(d.start.as_ref());
        let en = if live == Some(d.id) { None } else { at(d.end.as_ref()) };
        for id in [s, en.filter(|x| Some(*x) != s)].into_iter().flatten() {
            by.entry(id).or_default().0.drives += 1;
        }
        if let Some(id) = en {
            let x = by.entry(id).or_default();
            x.0.visits += 1;
            match next {
                Some(n) => x.1 = x.1.max(Some(n.start_ms.max(d.end_ms))),
                None => open = Some((id, d.end_ms)),
            }
        }
        Ok(())
    })?;
    each::<Charge>(e, car, 0, u64::MAX, None, &shown, false, |ch| {
        if let Some(id) = at(ch.position.as_ref()) {
            by.entry(id).or_default().0.charges += 1;
        }
        Ok(true)
    })?;
    let counts = Arc::new(CarCounts { places: places.clone(), live, by, open });
    keep_counts(e, car, generation, counts.clone());
    Ok(counts)
}

/// Each place's drives, charges and visits (by the live match), and its last visit (when the car left, or now while
/// it is still there), for the cars given. Kept per car until its rows or the places change.
pub fn counts(e: &Engine, cars: &[CarId]) -> QResult<ByPlace> {
    let places = e.places();
    let mut by = ByPlace::new();
    for &car in cars {
        let c = car_counts(e, car, &places)?;
        for (id, (n, last)) in &c.by {
            let x = by.entry(*id).or_default();
            x.0.drives += n.drives;
            x.0.charges += n.charges;
            x.0.visits += n.visits;
            x.1 = x.1.max(*last);
        }
        if let Some((id, arrived)) = c.open {
            let seen = last_seen(e, car)?;
            let x = by.entry(id).or_default();
            x.1 = x.1.max(Some(seen.max(arrived)));
        }
    }
    Ok(by)
}

/// `/places` filters.
#[derive(Debug, Clone, Default)]
pub struct PlacesQuery {
    pub search: Option<String>,
    pub kind: Option<String>,
    /// Counts for this car alone.
    pub car: Option<CarId>,
    /// `name` (default), `visits`, `drives`, `last_visit`, `created`.
    pub sort: Option<String>,
}

/// Who asks: the cars they see, and whether they see every place (an admin) or only those their cars have used.
#[derive(Debug, Clone)]
pub struct Asker {
    pub cars: Vec<CarId>,
    pub every_place: bool,
}

impl Asker {
    pub fn admin(cars: Vec<CarId>) -> Asker {
        Asker { cars, every_place: true }
    }
}

/// The places the asker sees, by id (`None`: all of them), from the counts over their cars.
fn seen(who: &Asker, counts: &BTreeMap<PlaceId, (Counts, Option<u64>)>) -> Option<Vec<PlaceId>> {
    (!who.every_place).then(|| counts.iter().filter(|(_, (c, _))| c.used()).map(|(id, _)| *id).collect())
}

/// `/places`: every place the asker sees, with counts for their cars (or `q.car`'s alone).
pub fn places(e: &Engine, who: &Asker, q: &PlacesQuery, paging: &Paging) -> QResult<List<PlaceView>> {
    let pl = e.places();
    if q.car.is_some_and(|c| !who.cars.contains(&c)) {
        return Err(QueryError::NoSuchCar);
    }
    // an admin sees every place: the counts over every car are needed only for a watch account's places
    let all_counts = if who.every_place && q.car.is_some() { None } else { Some(counts(e, &who.cars)?) };
    let shown_ids = all_counts.as_ref().and_then(|c| seen(who, c));
    let counts = match (q.car, all_counts) {
        (Some(c), _) => counts(e, &[c])?,
        (None, Some(all)) => all,
        (None, None) => ByPlace::new(),
    };
    let needle = q.search.as_deref().map(str::trim).map(str::to_lowercase).filter(|s| !s.is_empty());
    let kind = q.kind.as_deref().filter(|k| !k.is_empty());
    if let Some(k) = kind
        && !KINDS.contains(&k)
    {
        return Err(QueryError::bad("kind", "home, work, charger, service or other"));
    }
    let items: Vec<PlaceView> = pl
        .all()
        .iter()
        .filter(|p| shown_ids.as_ref().is_none_or(|ids| ids.contains(&p.id)))
        .filter(|p| {
            needle.as_ref().is_none_or(|n| {
                let addr = pl.extra(p.id).and_then(|x| x.address.as_deref()).unwrap_or("");
                p.name.to_lowercase().contains(n) || addr.to_lowercase().contains(n)
            })
        })
        .filter(|p| kind.is_none_or(|k| pl.kind_word(p) == k))
        .map(|p| {
            let (c, last) = counts.get(&p.id).cloned().unwrap_or_default();
            view(p, &pl, c, last)
        })
        .collect();
    let sort = q.sort.as_deref().unwrap_or("name");
    let sig = format!("places|{:?}|{}|{:?}|{:?}|{:?}|{sort}", who.cars, who.every_place, q.car, q.search, q.kind);
    let key = |v: &PlaceView| -> f64 {
        match sort {
            "visits" => v.counts.visits as f64,
            "drives" => v.counts.drives as f64 * 1e9 + v.counts.visits as f64,
            "last_visit" => v.last_visit_ms.map_or(-1.0, |t| t as f64),
            "created" => v.created_ms.map_or(-1.0, |t| t as f64),
            _ => 0.0,
        }
    };
    match sort {
        "name" => by_name(items, &sig, paging),
        "visits" | "drives" | "last_visit" | "created" => {
            paginate(items, |v| (key(v), v.id.clone()), true, &sig, paging, true)
        }
        _ => Err(QueryError::bad("sort", "name, visits, drives, last_visit or created")),
    }
}

/// The places by name (any case), then by id, paged by the row's own name and id: a place added, renamed or removed
/// between pages moves no other row.
fn by_name(items: Vec<PlaceView>, sig: &str, paging: &Paging) -> QResult<List<PlaceView>> {
    let limit = paging.limit()?;
    let num = |id: &str| super::parse_place(id).unwrap_or(u32::MAX);
    let mut v: Vec<(String, u32, PlaceView)> =
        items.into_iter().map(|x| (x.name.to_lowercase(), num(&x.id), x)).collect();
    v.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    let total = v.len() as u64;
    let start = match super::read_after(paging, sig)? {
        None => 0,
        Some((id, name)) => {
            let id = id as u32;
            v.partition_point(|x| (x.0.as_str(), x.1) <= (name.as_str(), id))
        }
    };
    let mut rest: Vec<(String, u32, PlaceView)> = v.into_iter().skip(start).collect();
    let more = rest.len() > limit;
    rest.truncate(limit);
    let next = match rest.last() {
        Some(last) if more => Some(super::make_after(&(f64::from(last.1), last.0.clone()), sig)?),
        _ => None,
    };
    Ok(List { items: rest.into_iter().map(|x| x.2).collect(), next, total: Some(total) })
}

/// One place with its counts over the asker's cars (`no_such_place` for one they don't see).
pub fn place(e: &Engine, who: &Asker, id: PlaceId) -> QResult<PlaceView> {
    let pl = e.places();
    let p = pl.get(id).ok_or(QueryError::NotFound("no_such_place"))?;
    let mut counts = counts(e, &who.cars)?;
    let (c, last) = counts.remove(&id).unwrap_or_default();
    if !who.every_place && !c.used() {
        return Err(QueryError::NotFound("no_such_place"));
    }
    Ok(view(p, &pl, c, last))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Candidate {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub distance_m: f64,
    pub radius_m: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct At {
    pub lat: f64,
    pub lon: f64,
    pub place: Option<PlaceView>,
    pub candidates: Vec<Candidate>,
    pub rule: &'static str,
    pub address: Option<Address>,
}

/// `/places/at`: the place a point is at by the nearest-centre rule (the lowest id on a tie), and every place whose
/// circle holds it, of those the asker sees.
pub fn at(e: &Engine, who: &Asker, lat: f64, lon: f64) -> QResult<At> {
    if !lat.is_finite() || !(-90.0..=90.0).contains(&lat) {
        return Err(QueryError::bad("lat", "-90 to 90"));
    }
    if !lon.is_finite() || !(-180.0..=180.0).contains(&lon) {
        return Err(QueryError::bad("lon", "-180 to 180"));
    }
    let pl = e.places();
    let circles = pl.places_at(lat, lon);
    // counts only where a circle holds the point (an admin's are needed only for the place it is at)
    let counts = if circles.is_empty() { ByPlace::new() } else { counts(e, &who.cars)? };
    let ids = seen(who, &counts);
    let near: Vec<(&Place, f64)> =
        circles.into_iter().filter(|(p, _)| ids.as_ref().is_none_or(|i| i.contains(&p.id))).collect();
    let candidates = near
        .iter()
        .map(|(p, d)| Candidate {
            id: place_ref(p.id),
            name: p.name.clone(),
            kind: pl.kind_word(p),
            distance_m: r(Some(*d), 1).unwrap_or(*d),
            radius_m: p.radius_m,
        })
        .collect();
    let place = near.first().map(|(p, _)| {
        let (c, last) = counts.get(&p.id).cloned().unwrap_or_default();
        view(p, &pl, c, last)
    });
    Ok(At { lat, lon, place, candidates, rule: "nearest_centre", address: None })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Suggestion {
    pub lat: f64,
    pub lon: f64,
    pub radius_m: f64,
    pub visits: u64,
    pub dwell_ms: u64,
    pub cars: Vec<CarRef>,
    pub label: Option<String>,
    pub address: Option<Address>,
    pub last_visit_ms: u64,
    pub kind: &'static str,
}

/// `/places/suggestions`: where the cars stop most that no saved place covers (two stops or more), most visited
/// first, with how long they stayed. Stops stream into the grouping one at a time.
pub fn suggestions(e: &Engine, cars: &[CarId], limit: Option<usize>) -> QResult<List<Suggestion>> {
    let max = limit.unwrap_or(10);
    if !(1..=100).contains(&max) {
        return Err(QueryError::bad("limit", "1 to 100"));
    }
    let pl = e.places();
    let index = places::PlaceIndex::new(pl.all().to_vec());
    let opts = places::SuggestOptions { max, ..places::SuggestOptions::default() };
    let mut sug = places::Suggester::new(&index, opts);
    for &car in cars {
        let shown = Shown::new(e, car, None)?;
        let live = live_drive(e, car)?;
        let seen = last_seen(e, car)?;
        drives_with_next(e, car, &shown, |d, next| {
            if live == Some(d.id) {
                return Ok(());
            }
            if let Some(end) = &d.end {
                let left = next.map_or(seen, |n| n.start_ms).max(d.end_ms);
                let spot = places::Spot { lat: end.lat, lon: end.lon, t_ms: left, car };
                sug.add_stay(&spot, left - d.end_ms, d.end_address.as_deref());
            }
            Ok(())
        })?;
    }
    let items = sug
        .finish()
        .into_iter()
        .map(|s| Suggestion {
            lat: s.lat,
            lon: s.lon,
            radius_m: NEW_RADIUS_M,
            visits: s.visits as u64,
            dwell_ms: s.dwell_ms,
            cars: s.cars.iter().map(|&c| CarRef(c)).collect(),
            address: s.label.clone().map(|short| Address { short: Some(short), ..Address::default() }),
            label: s.label,
            last_visit_ms: s.last_ms,
            kind: "other",
        })
        .collect();
    Ok(List { items, next: None, total: None })
}

// ---------------------------------------------------------------------------------------------------------------
// What a change does

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Taken {
    pub place: TakenPlace,
    pub drives: u64,
    pub charges: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TakenPlace {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Example {
    pub kind: &'static str,
    pub id: String,
    pub car: CarRef,
    pub start_ms: u64,
    pub change: &'static str,
}

/// What saving or deleting a place changes: the drives and charges that move between places.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Effect {
    pub drives_named: u64,
    pub drives_unnamed: u64,
    pub charges_named: u64,
    pub charges_unnamed: u64,
    pub drives_taken: u64,
    pub charges_taken: u64,
    pub taken_from: Vec<Taken>,
    pub charges_repriced: u64,
    pub examples: Vec<String>,
    pub example_rows: Vec<Example>,
}

impl Effect {
    fn note(&mut self, kind: &'static str, id: u64, car: CarId, start_ms: u64, change: &'static str) {
        let id = id.to_string();
        if self.example_rows.len() < EXAMPLES && !self.example_rows.iter().any(|x| x.id == id && x.kind == kind) {
            self.examples.push(id.clone());
            self.example_rows.push(Example { kind, id, car: CarRef(car), start_ms, change });
        }
    }
}

/// `/places/preview`: the effect, and which place it is about.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Preview {
    #[serde(flatten)]
    pub effect: Effect,
    pub place: Option<String>,
    pub delete: bool,
}

/// Whether a charge has ended long enough ago that its cost is kept.
fn ended(e: &Engine, ch: &Charge, now: u64) -> bool {
    ch.end_ms.saturating_add(e.options().params.charge_gap_ms) < now
}

/// What a charge costs now (kept, else at its place's price among `before`), and what it would cost at its place's
/// price among `after`.
fn cost_pair(
    e: &Engine,
    car: CarId,
    tz: &str,
    ch: &Charge,
    before: &Places,
    after: &Places,
) -> QResult<(Option<records::CostEntry>, Option<records::CostEntry>)> {
    let kept = records::cost(e.db(), car, ch.id, ch.start_ms)?;
    let tou = |p: &Places| {
        ch.position
            .as_ref()
            .and_then(|x| p.place_at(x.lat, x.lon))
            .and_then(|pl| pl.price.as_ref())
            .is_some_and(|pr| !pr.rates.is_empty())
    };
    let curve = if tou(before) || tou(after) { costs::curve_of(e, car, ch)? } else { Vec::new() };
    let now = match kept {
        Some(k) => Some(k),
        // it keeps having no cost
        None if costs::kept_none(e, car, ch.id)? => None,
        None => costs::price_in(before, ch, &curve, tz),
    };
    Ok((now, costs::price_in(after, ch, &curve, tz)))
}

/// The effect of going from `before` to `after` for the drives and charges of `cars`, where `focus` are the places
/// changed (the place edited, deleted or drafted). With `freeze`, every ended charge it moves (or whose place's price
/// changes) keeps the cost it shows now, before the change is saved.
fn effect(
    e: &Engine,
    cars: &[CarId],
    before: &Places,
    after: &Places,
    focus: &[PlaceId],
    freeze: bool,
) -> QResult<Effect> {
    let mut out = Effect::default();
    let mut taken: BTreeMap<PlaceId, (u64, u64)> = BTreeMap::new();
    let id_at = |p: &Places, pos: Option<&model::Position>| pos.and_then(|x| p.place_at(x.lat, x.lon)).map(|x| x.id);
    let in_focus = |id: Option<PlaceId>| id.is_some_and(|i| focus.contains(&i));
    let price_changed = |b: Option<PlaceId>, a: Option<PlaceId>| {
        let pb = b.and_then(|i| before.get(i)).map(|p| (&p.price, before.extra(p.id).and_then(|x| x.per_minute)));
        let pa = a.and_then(|i| after.get(i)).map(|p| (&p.price, after.extra(p.id).and_then(|x| x.per_minute)));
        pb != pa
    };
    let now = now_ms();
    for &car in cars {
        let shown = Shown::new(e, car, None)?;
        let live = live_drive(e, car)?;
        let tz = super::car_of(e, car)?.time_zone;
        each::<Drive>(e, car, 0, u64::MAX, None, &shown, false, |d| {
            let ends = [Some(d.start.as_ref()), (live != Some(d.id)).then_some(d.end.as_ref())];
            let (mut named, mut unnamed, mut took) = (false, false, false);
            for pos in ends.into_iter().flatten() {
                let (b, a) = (id_at(before, pos), id_at(after, pos));
                if b == a {
                    continue;
                }
                if in_focus(a) {
                    named = true;
                    if let Some(from) = b.filter(|x| !focus.contains(x)) {
                        took = true;
                        taken.entry(from).or_default().0 += 1;
                    }
                }
                if in_focus(b) {
                    unnamed = true;
                }
            }
            out.drives_named += u64::from(named);
            out.drives_unnamed += u64::from(unnamed);
            out.drives_taken += u64::from(took);
            if named || unnamed {
                out.note("drive", d.id, car, d.start_ms, if named { "named" } else { "unnamed" });
            }
            Ok(true)
        })?;
        each::<Charge>(e, car, 0, u64::MAX, None, &shown, false, |ch| {
            let (b, a) = (id_at(before, ch.position.as_ref()), id_at(after, ch.position.as_ref()));
            let moved = b != a;
            if moved && in_focus(a) {
                out.charges_named += 1;
                if let Some(from) = b.filter(|x| !focus.contains(x)) {
                    out.charges_taken += 1;
                    taken.entry(from).or_default().1 += 1;
                }
            }
            if moved && in_focus(b) {
                out.charges_unnamed += 1;
            }
            if moved && (in_focus(a) || in_focus(b)) {
                out.note("charge", ch.id, car, ch.start_ms, if in_focus(a) { "named" } else { "unnamed" });
            }
            let touched = in_focus(a) || in_focus(b);
            if !touched || ch.source != Source::Live || !ended(e, &ch, now) {
                return Ok(true);
            }
            let kept = records::cost(e.db(), car, ch.id, ch.start_ms)?;
            if kept.as_ref().is_some_and(|k| k.source == "manual") {
                return Ok(true);
            }
            if in_focus(a) {
                let (cur, new) = cost_pair(e, car, &tz, &ch, before, after)?;
                if cur.as_ref().map(|c| (c.amount, &c.currency)) != new.as_ref().map(|c| (c.amount, &c.currency)) {
                    out.charges_repriced += 1;
                    if !moved {
                        out.note("charge", ch.id, car, ch.start_ms, "repriced");
                    }
                }
            }
            if freeze && kept.is_none() && (moved || price_changed(b, a)) {
                let curve = costs::curve_of(e, car, &ch)?;
                // the cost it shows now, kept; one with none keeps that it has none (only a reprice prices it)
                if costs::settle(e, car, &ch, &curve, &tz, true)?.is_none() {
                    costs::put_kept_none(e, car, ch.id, true)?;
                }
            }
            Ok(true)
        })?;
    }
    let mut tf: Vec<(PlaceId, (u64, u64))> = taken.into_iter().collect();
    tf.sort_by(|a, b| (b.1.0 + b.1.1).cmp(&(a.1.0 + a.1.1)).then(a.0.cmp(&b.0)));
    out.taken_from = tf
        .into_iter()
        .map(|(id, (d, c))| Taken {
            place: TakenPlace { id: place_ref(id), name: before.get(id).map(|p| p.name.clone()).unwrap_or_default() },
            drives: d,
            charges: c,
        })
        .collect();
    Ok(out)
}

/// A place as the owner gives it (a new one, an edit merged onto the saved one, or a draft to preview).
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub name: String,
    pub kind: String,
    pub lat: f64,
    pub lon: f64,
    pub radius_m: f64,
    /// `None`: the nearest known place's zone (else the car's).
    pub time_zone: Option<String>,
    pub price: Option<Price>,
}

/// What may change in an edit (`None`: unchanged; `Some(None)` for time zone and price: none).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Patch {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub radius_m: Option<f64>,
    pub time_zone: Option<Option<String>>,
    pub price: Option<Option<Price>>,
}

impl Patch {
    pub fn apply(self, d: &mut Draft) {
        if let Some(v) = self.name {
            d.name = v;
        }
        if let Some(v) = self.kind {
            d.kind = v;
        }
        if let Some(v) = self.lat {
            d.lat = v;
        }
        if let Some(v) = self.lon {
            d.lon = v;
        }
        if let Some(v) = self.radius_m {
            d.radius_m = v;
        }
        if let Some(v) = self.time_zone {
            d.time_zone = v;
        }
        if let Some(v) = self.price {
            d.price = v;
        }
    }

    pub fn is_move(&self) -> bool {
        self.lat.is_some() || self.lon.is_some()
    }
}

/// The saved place as a draft.
fn draft_of(p: &Place, places: &Places) -> Draft {
    let extra = places.extra(p.id);
    Draft {
        name: p.name.clone(),
        kind: places.kind_word(p).to_string(),
        lat: p.lat,
        lon: p.lon,
        radius_m: p.radius_m,
        time_zone: extra.and_then(|x| x.time_zone.clone()),
        price: price_of(p, extra),
    }
}

fn price_rules(p: &Price, bad: &mut BTreeMap<String, String>) {
    let mut add = |k: &str, why: &str| {
        bad.insert(k.to_string(), why.to_string());
    };
    if p.currency.len() != 3 || !p.currency.bytes().all(|b| b.is_ascii_uppercase()) {
        add("price.currency", "A currency code such as USD, CAD or EUR.");
    }
    let per_kwh_ok =
        |v: Option<f64>| v.is_none_or(|x| x.is_finite() && (0.0..=places::price::MAX_PER_KWH).contains(&x));
    let per_min_ok = |v: Option<f64>| v.is_none_or(|x| x.is_finite() && (0.0..=100.0).contains(&x));
    if !per_kwh_ok(p.per_kwh) {
        add("price.per_kwh", "Must be between 0 and 10.");
    }
    if !per_min_ok(p.per_minute) {
        add("price.per_minute", "Must be between 0 and 100.");
    }
    if p.session_fee.is_some_and(|f| !f.is_finite() || !(0.0..=places::price::MAX_SESSION_FEE).contains(&f)) {
        add("price.session_fee", "Must be between 0 and 1000.");
    }
    if p.rates.len() > places::price::MAX_RATES {
        add("price.rates", "At most 12 rates.");
    }
    for (i, rt) in p.rates.iter().enumerate() {
        let k = |f: &str| format!("price.rates.{i}.{f}");
        let name = rt.name.trim();
        if name.is_empty()
            || name.chars().count() > 24
            || name.chars().any(|c| c.is_control() || "<>&\"'`\\".contains(c))
        {
            add(&k("name"), "1 to 24 characters, plain words.");
        }
        if !per_kwh_ok(rt.per_kwh) {
            add(&k("per_kwh"), "Must be between 0 and 10.");
        }
        if !per_min_ok(rt.per_minute) {
            add(&k("per_minute"), "Must be between 0 and 100.");
        }
        if parse_days(&rt.days).is_none() {
            add(&k("days"), "Days such as mon-fri, sat,sun or all.");
        }
        let (f, t) = (places::price::parse_hhmm(&rt.from), places::price::parse_end_hhmm(&rt.to));
        if f.is_none() {
            add(&k("from"), "A time as HH:MM.");
        }
        if t.is_none() {
            add(&k("to"), "A time as HH:MM (24:00 for the end of the day).");
        }
        if f.is_some() && f == t {
            add(&k("to"), "From and to are the same time.");
        }
        if rt.per_kwh.is_none() && rt.per_minute.is_none() {
            add(&k("per_kwh"), "Give the rate a price per kWh or per minute.");
        }
    }
    // the hours no rate covers are at the place's own price: without one they would be free, not unknown
    if p.per_kwh.is_none() && p.per_minute.is_none() && !p.rates.is_empty() && !rates_cover_week(&p.rates) {
        add("price.per_kwh", "The rates leave some hours of the week out: give a price per kWh for those hours.");
    }
    if p.per_kwh.is_none() && p.per_minute.is_none() && p.rates.is_empty() && p.session_fee.is_none() {
        add("price", "Give a price per kWh, per minute, a rate or a session fee, or send null for no price.");
    }
}

/// Whether the rates (each well formed, else left out) cover every minute of the week.
fn rates_cover_week(rates: &[Rate]) -> bool {
    const DAY: usize = 1440;
    let mut week = vec![false; 7 * DAY];
    for rt in rates {
        let (Some(days), Some(f), Some(t)) =
            (parse_days(&rt.days), places::price::parse_hhmm(&rt.from), places::price::parse_end_hhmm(&rt.to))
        else {
            continue;
        };
        let (f, t) = (usize::from(f).min(DAY), usize::from(t).min(DAY));
        for d in days {
            let d = usize::from(d) % 7;
            if f < t {
                week[d * DAY + f..d * DAY + t].iter_mut().for_each(|m| *m = true);
            } else {
                // past midnight: on to the next day
                week[d * DAY + f..(d + 1) * DAY].iter_mut().for_each(|m| *m = true);
                let n = (d + 1) % 7;
                week[n * DAY..n * DAY + t].iter_mut().for_each(|m| *m = true);
            }
        }
    }
    week.iter().all(|&m| m)
}

/// Checks a draft (`422` naming every field that breaks a rule).
pub fn check(d: &Draft) -> QResult<()> {
    let mut bad = BTreeMap::new();
    let name = d.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        bad.insert("name".into(), "1 to 64 characters.".into());
    } else if name.chars().any(char::is_control) {
        bad.insert("name".into(), "No control characters.".into());
    }
    if !KINDS.contains(&d.kind.as_str()) {
        bad.insert("kind".into(), "Must be home, work, charger, service or other.".into());
    }
    if !d.lat.is_finite() || !(-90.0..=90.0).contains(&d.lat) {
        bad.insert("lat".into(), "Must be between -90 and 90.".into());
    }
    if !d.lon.is_finite() || !(-180.0..=180.0).contains(&d.lon) {
        bad.insert("lon".into(), "Must be between -180 and 180.".into());
    }
    if !d.radius_m.is_finite() || !(model::MIN_RADIUS_M..=model::MAX_RADIUS_M).contains(&d.radius_m.round()) {
        bad.insert("radius_m".into(), "Must be between 10 and 5000 m.".into());
    }
    if d.time_zone.as_deref().is_some_and(|z| !tz::is_zone(z)) {
        bad.insert("time_zone".into(), "Not a known time zone (for example America/Chicago).".into());
    }
    if let Some(p) = &d.price {
        price_rules(p, &mut bad);
    }
    if bad.is_empty() { Ok(()) } else { Err(QueryError::Invalid { fields: bad }) }
}

/// The model's place and the extra fields of a checked draft.
fn to_model(id: PlaceId, d: &Draft, extra: PlaceExtra) -> (Place, PlaceExtra) {
    let kind = match d.kind.as_str() {
        "home" => PlaceKind::Home,
        "work" => PlaceKind::Work,
        "charger" => PlaceKind::Charger,
        _ => PlaceKind::Other,
    };
    let round6 = |x: f64| (x * 1e6).round() / 1e6;
    let price = d.price.as_ref().map(|p| {
        let mut p = p.clone();
        for rt in &mut p.rates {
            rt.name = rt.name.trim().to_string();
        }
        p
    });
    let plan = price.as_ref().map(|p| PricePlan {
        currency: p.currency.clone(),
        per_kwh: p.per_kwh.unwrap_or(0.0),
        rates: p
            .rates
            .iter()
            .map(|rt| model::Rate {
                name: rt.name.clone(),
                per_kwh: rt.per_kwh.unwrap_or(0.0),
                days: parse_days(&rt.days).unwrap_or_else(|| (0..7).collect()),
                from: rt.from.clone(),
                to: rt.to.clone(),
            })
            .collect(),
        session_fee: p.session_fee,
    });
    let place = Place {
        id,
        name: d.name.trim().to_string(),
        kind,
        lat: round6(d.lat),
        lon: round6(d.lon),
        radius_m: d.radius_m.round(),
        price: plan,
    };
    let extra = PlaceExtra {
        time_zone: d.time_zone.clone(),
        kind: (d.kind == "service").then(|| "service".to_string()),
        per_minute: price.as_ref().and_then(|p| p.per_minute),
        price,
        ..extra
    };
    (place, extra)
}

/// The zone of the nearest saved place that has one.
fn nearest_zone(places: &Places, lat: f64, lon: f64) -> Option<String> {
    places
        .all()
        .iter()
        .filter_map(|p| places.extra(p.id).and_then(|x| x.time_zone.clone()).map(|z| (p, z)))
        .min_by(|a, b| {
            places::distance_m(lat, lon, a.0.lat, a.0.lon).total_cmp(&places::distance_m(lat, lon, b.0.lat, b.0.lon))
        })
        .map(|(_, z)| z)
}

/// The places with `id` replaced by `p` (or left out when `None`); a new id is listed last, so it loses a tie to every
/// saved place, as it will once saved.
fn with(now: &Places, id: PlaceId, p: Option<(Place, PlaceExtra)>) -> Places {
    let mut list: Vec<Place> = now.all().iter().filter(|x| x.id != id).cloned().collect();
    let mut extras: HashMap<PlaceId, PlaceExtra> =
        now.all().iter().filter_map(|x| now.extra(x.id).map(|e| (x.id, e.clone()))).collect();
    extras.remove(&id);
    if let Some((place, extra)) = p {
        extras.insert(id, extra);
        let at = list.partition_point(|x| x.id < id);
        list.insert(at, place);
    }
    Places::new(list, extras)
}

/// What to preview: a draft (an edit of `id`, else a new place), or deleting a place.
#[derive(Debug, Clone)]
pub enum PreviewOf {
    /// A new place.
    New(Box<Draft>),
    /// An edit of a saved place.
    Edit(PlaceId, Box<Patch>),
    Delete(PlaceId),
}

/// `/places/preview`: what saving a draft or deleting a place would change, over the asker's cars. Nothing is
/// changed. A place the asker doesn't see is `422` on `place` (delete) or `id` (edit), as if there were none.
pub fn preview(e: &Engine, who: &Asker, what: PreviewOf) -> QResult<Preview> {
    let now = e.places();
    let sees = |id: PlaceId| -> QResult<bool> {
        if now.get(id).is_none() {
            return Ok(false);
        }
        if who.every_place {
            return Ok(true);
        }
        Ok(counts(e, &who.cars)?.get(&id).is_some_and(|(c, _)| c.used()))
    };
    match what {
        PreviewOf::Delete(id) => {
            if !sees(id)? {
                return Err(QueryError::invalid("place", "No such place."));
            }
            let after = with(&now, id, None);
            let effect = effect(e, &who.cars, &now, &after, &[id], false)?;
            Ok(Preview { effect, place: Some(place_ref(id)), delete: true })
        }
        PreviewOf::Edit(id, patch) => {
            if !sees(id)? {
                return Err(QueryError::invalid("id", "No such place."));
            }
            let Some(cur) = now.get(id) else { return Err(QueryError::invalid("id", "No such place.")) };
            let mut d = draft_of(cur, &now);
            patch.apply(&mut d);
            check(&d)?;
            let extra = now.extra(id).cloned().unwrap_or_default();
            let after = with(&now, id, Some(to_model(id, &d, extra)));
            let effect = effect(e, &who.cars, &now, &after, &[id], false)?;
            Ok(Preview { effect, place: Some(place_ref(id)), delete: false })
        }
        PreviewOf::New(d) => {
            let mut d = *d;
            if d.time_zone.is_none() {
                d.time_zone = nearest_zone(&now, d.lat, d.lon);
            }
            check(&d)?;
            let draft_id = PlaceId::MAX;
            let after = with(&now, draft_id, Some(to_model(draft_id, &d, PlaceExtra::default())));
            let effect = effect(e, &who.cars, &now, &after, &[draft_id], false)?;
            Ok(Preview { effect, place: None, delete: false })
        }
    }
}

/// A change's answer, or the version the place has when the one sent is stale.
#[derive(Debug, Clone, PartialEq)]
pub enum Change<T> {
    Done(T),
    Stale(u64),
}

/// A saved place and what saving it did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Saved {
    #[serde(flatten)]
    pub place: PlaceView,
    pub effect: Effect,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Deleted {
    pub deleted: bool,
    pub id: String,
    pub name: String,
    pub effect: Effect,
}

/// Changes are made one at a time (the effect is worked out against the places as they are).
static WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// After a change: the next read matches against the places as they are now, and every page reloads its rows.
fn changed(e: &Engine, cars: &[CarId]) {
    e.places_changed();
    let now = now_ms();
    for &car in cars {
        e.emit(crate::Event::Rows { car, kinds: vec!["drives", "charges", "visits"], from_ms: 0, to_ms: now });
    }
}

/// Every car (the effect of a change is on every car's rows, whoever makes it).
fn every_car(e: &Engine) -> QResult<Vec<CarId>> {
    super::all_cars(e)
}

fn saved_view(e: &Engine, who: &Asker, id: PlaceId) -> QResult<PlaceView> {
    place(e, &Asker { cars: who.cars.clone(), every_place: true }, id)
}

/// Adds a place (checked). Its time zone defaults to the nearest known place's. The effect counts over the asker's
/// cars; the costs of every car's charges that move stay put.
pub fn create(e: &Engine, who: &Asker, mut d: Draft) -> QResult<Saved> {
    let _w = crate::lock(&WRITES);
    let now = e.places();
    if d.time_zone.is_none() {
        d.time_zone = nearest_zone(&now, d.lat, d.lon);
    }
    check(&d)?;
    let extra = PlaceExtra { created_ms: Some(now_ms()), version: 1, ..PlaceExtra::default() };
    let draft_id = PlaceId::MAX;
    let after = with(&now, draft_id, Some(to_model(draft_id, &d, extra.clone())));
    let effect = freeze_and_effect(e, who, &now, &after, &[draft_id])?;
    let (place, extra) = to_model(0, &d, extra);
    let added = e.db().add_place(place)?;
    placing::set_extra(e.db(), added.id, Some(&extra))?;
    changed(e, &every_car(e)?);
    Ok(Saved { place: saved_view(e, who, added.id)?, effect })
}

/// Works out the effect over every car, keeping the costs of the charges it moves, and answers it over the asker's
/// cars (worked out again only when they don't see every car).
fn freeze_and_effect(e: &Engine, who: &Asker, before: &Places, after: &Places, focus: &[PlaceId]) -> QResult<Effect> {
    let all = every_car(e)?;
    let whole = effect(e, &all, before, after, focus, true)?;
    if all.iter().all(|c| who.cars.contains(c)) {
        return Ok(whole);
    }
    let mine: Vec<CarId> = all.iter().copied().filter(|c| who.cars.contains(c)).collect();
    effect(e, &mine, before, after, focus, false)
}

/// Changes a place: `patch` merged onto it, when `version` is the one it has.
pub fn update(e: &Engine, who: &Asker, id: PlaceId, version: u64, patch: Patch) -> QResult<Change<Saved>> {
    let _w = crate::lock(&WRITES);
    let now = e.places();
    let cur = now.get(id).ok_or(QueryError::NotFound("no_such_place"))?;
    let mut extra = now.extra(id).cloned().unwrap_or_default();
    let have = extra.version.max(1);
    if version != have {
        return Ok(Change::Stale(have));
    }
    let moved = patch.is_move();
    let mut d = draft_of(cur, &now);
    patch.apply(&mut d);
    check(&d)?;
    extra.version = have + 1;
    if moved {
        // the address was of the old centre
        extra.address = None;
    }
    let (place, extra) = to_model(id, &d, extra);
    let after = with(&now, id, Some((place.clone(), extra.clone())));
    let effect = freeze_and_effect(e, who, &now, &after, &[id])?;
    e.db().update_place(place)?;
    placing::set_extra(e.db(), id, Some(&extra))?;
    changed(e, &every_car(e)?);
    Ok(Change::Done(Saved { place: saved_view(e, who, id)?, effect }))
}

/// Deletes a place (when `version`, if given, is the one it has).
pub fn delete(e: &Engine, who: &Asker, id: PlaceId, version: Option<u64>) -> QResult<Change<Deleted>> {
    let _w = crate::lock(&WRITES);
    let now = e.places();
    let cur = now.get(id).ok_or(QueryError::NotFound("no_such_place"))?;
    let have = now.extra(id).map_or(1, |x| x.version.max(1));
    if version.is_some_and(|v| v != have) {
        return Ok(Change::Stale(have));
    }
    let name = cur.name.clone();
    let after = with(&now, id, None);
    let effect = freeze_and_effect(e, who, &now, &after, &[id])?;
    e.db().remove_place(id)?;
    placing::set_extra(e.db(), id, None)?;
    changed(e, &every_car(e)?);
    Ok(Change::Done(Deleted { deleted: true, id: place_ref(id), name, effect }))
}

/// What [`reprice`] did (or would do, `dry_run`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Repriced {
    pub place: String,
    pub from_ms: u64,
    pub to_ms: u64,
    pub only_without_cost: bool,
    pub dry_run: bool,
    pub charges: u64,
    pub repriced: u64,
    pub unchanged: u64,
    pub examples: Vec<String>,
    pub example_rows: Vec<Example>,
    pub cost_before: Vec<Money>,
    pub cost_after: Vec<Money>,
}

/// `POST /places/{place}/reprice`: the place's price as it is now applied to its past charges that started in
/// `[from_ms, to_ms)` (CAN charges that ended, of the cars given). A typed cost is never changed and not counted;
/// `only_without_cost` takes only charges without a cost. Streams the charges a page at a time.
pub fn reprice(
    e: &Engine,
    cars: &[CarId],
    place: PlaceId,
    from_ms: Option<u64>,
    to_ms: Option<u64>,
    only_without_cost: bool,
    dry_run: bool,
) -> QResult<Repriced> {
    let _w = (!dry_run).then(|| crate::lock(&WRITES));
    let places = e.places();
    if places.get(place).is_none() {
        return Err(QueryError::NotFound("no_such_place"));
    }
    let now = now_ms();
    let (a, b) = (from_ms.unwrap_or(0), to_ms.unwrap_or(now + 1));
    if a >= b {
        return Err(QueryError::invalid("to_ms", "Must be after from_ms."));
    }
    let mut out =
        Repriced { place: place_ref(place), from_ms: a, to_ms: b, only_without_cost, dry_run, ..Default::default() };
    let (mut before, mut after): (BTreeMap<String, f64>, BTreeMap<String, f64>) = Default::default();
    for &car in cars {
        let tz = super::car_of(e, car)?.time_zone;
        let shown = Shown::new(e, car, Some(super::SourceFilter::Live))?;
        let mut changed_here = false;
        each::<Charge>(e, car, a, b, Some(super::SourceFilter::Live), &shown, false, |ch| {
            let here = ch.position.as_ref().and_then(|p| places.place_at(p.lat, p.lon)).map(|p| p.id);
            if here != Some(place) || !ended(e, &ch, now) {
                return Ok(true);
            }
            let kept = records::cost(e.db(), car, ch.id, ch.start_ms)?;
            if kept.as_ref().is_some_and(|k| k.source == "manual") {
                return Ok(true);
            }
            let curve = costs::curve_of(e, car, &ch)?;
            let old = match &kept {
                Some(k) => Some(k.clone()),
                None if costs::kept_none(e, car, ch.id)? => None,
                None => costs::price_in(&places, &ch, &curve, &tz),
            };
            if only_without_cost && old.is_some() {
                return Ok(true);
            }
            out.charges += 1;
            let new = costs::price_in(&places, &ch, &curve, &tz);
            for (box_, c) in [(&mut before, &old), (&mut after, &new)] {
                if let Some(c) = c {
                    *box_.entry(c.currency.clone()).or_default() += c.amount;
                }
            }
            if old.as_ref().map(|c| (c.amount, &c.currency)) == new.as_ref().map(|c| (c.amount, &c.currency)) {
                out.unchanged += 1;
                if !dry_run && kept.is_none() && new.is_some() {
                    costs::keep(e, car, &ch, new)?;
                }
                return Ok(true);
            }
            out.repriced += 1;
            if out.example_rows.len() < EXAMPLES {
                out.examples.push(ch.id.to_string());
                out.example_rows.push(Example {
                    kind: "charge",
                    id: ch.id.to_string(),
                    car: CarRef(car),
                    start_ms: ch.start_ms,
                    change: "repriced",
                });
            }
            if !dry_run {
                costs::keep(e, car, &ch, new)?;
                changed_here = true;
            }
            Ok(true)
        })?;
        if changed_here {
            e.emit(crate::Event::Rows { car, kinds: vec!["charges"], from_ms: a, to_ms: b });
        }
    }
    out.cost_before = super::money(before);
    out.cost_after = super::money(after);
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn days_both_ways() {
        assert_eq!(parse_days("mon-fri").unwrap(), vec![0, 1, 2, 3, 4]);
        assert_eq!(parse_days("sat,sun").unwrap(), vec![5, 6]);
        assert_eq!(parse_days("fri-mon").unwrap(), vec![0, 4, 5, 6]);
        assert_eq!(parse_days("all").unwrap().len(), 7);
        assert!(parse_days("someday").is_none());
        assert!(parse_days("").is_none());
        assert_eq!(days_word(&[0, 1, 2, 3, 4]), "mon-fri");
        assert_eq!(days_word(&[5, 6]), "sat,sun");
        assert_eq!(days_word(&[0, 1, 2, 3, 4, 5, 6]), "all");
    }

    #[test]
    fn drafts_are_checked_field_by_field() {
        let ok = Draft {
            name: "Home".into(),
            kind: "home".into(),
            lat: 1.0,
            lon: 2.0,
            radius_m: 100.0,
            time_zone: Some("Europe/Berlin".into()),
            price: Some(Price {
                currency: "EUR".into(),
                per_kwh: None,
                per_minute: None,
                rates: vec![Rate {
                    name: "Night".into(),
                    per_kwh: Some(0.1),
                    per_minute: None,
                    days: "all".into(),
                    from: "00:00".into(),
                    to: "24:00".into(),
                }],
                session_fee: None,
            }),
        };
        check(&ok).unwrap();
        let bad = Draft {
            name: " ".into(),
            kind: "castle".into(),
            radius_m: 9.0,
            time_zone: Some("Mars/Base".into()),
            price: Some(Price {
                currency: "eur".into(),
                per_kwh: None,
                per_minute: None,
                rates: Vec::new(),
                session_fee: None,
            }),
            ..ok.clone()
        };
        let QueryError::Invalid { fields } = check(&bad).unwrap_err() else { unreachable!() };
        for k in ["name", "kind", "radius_m", "time_zone", "price.currency", "price"] {
            assert!(fields.contains_key(k), "{k}: {fields:?}");
        }
        let (place, extra) = to_model(3, &ok, PlaceExtra::default());
        let plan = place.price.unwrap();
        assert_eq!((plan.per_kwh, plan.rates[0].days.len(), plan.rates[0].to.as_str()), (0.0, 7, "24:00"));
        assert_eq!(extra.price.unwrap().per_kwh, None, "given back as given");
        let t = places::Tariff::new(&PricePlan { ..plan_of_one_rate() }, "UTC").unwrap();
        assert_eq!(t.price_at(23 * 3_600_000 + 59 * 60_000), 0.1, "24:00 covers the day's last minute");
    }

    #[test]
    fn rates_that_leave_hours_out_need_a_price_of_their_own() {
        let rate = |name: &str, days: &str, from: &str, to: &str, per_kwh: Option<f64>| Rate {
            name: name.into(),
            per_kwh,
            per_minute: None,
            days: days.into(),
            from: from.into(),
            to: to.into(),
        };
        let draft = |per_kwh: Option<f64>, rates: Vec<Rate>| Draft {
            name: "Home".into(),
            kind: "home".into(),
            lat: 1.0,
            lon: 2.0,
            radius_m: 100.0,
            time_zone: None,
            price: Some(Price { currency: "EUR".into(), per_kwh, per_minute: None, rates, session_fee: None }),
        };
        let weekdays = vec![
            rate("Night", "mon-fri", "22:00", "06:00", Some(0.1)),
            rate("Day", "all", "06:00", "22:00", Some(0.3)),
        ];
        let QueryError::Invalid { fields } = check(&draft(None, weekdays.clone())).unwrap_err() else { unreachable!() };
        assert!(fields.contains_key("price.per_kwh"), "weekend nights are left out: {fields:?}");
        check(&draft(Some(0.2), weekdays)).unwrap();
        // every night (past midnight on to the next day) and every day: the whole week
        let whole =
            vec![rate("Night", "all", "22:00", "06:00", Some(0.1)), rate("Day", "all", "06:00", "22:00", Some(0.3))];
        check(&draft(None, whole)).unwrap();
        // a rate with no price at all
        let QueryError::Invalid { fields } =
            check(&draft(Some(0.2), vec![rate("Free", "all", "00:00", "06:00", None)])).unwrap_err()
        else {
            unreachable!()
        };
        assert!(fields.contains_key("price.rates.0.per_kwh"), "{fields:?}");
    }

    fn plan_of_one_rate() -> PricePlan {
        PricePlan {
            currency: "EUR".into(),
            per_kwh: 0.3,
            rates: vec![model::Rate {
                name: "Night".into(),
                per_kwh: 0.1,
                days: (0..7).collect(),
                from: "00:00".into(),
                to: "24:00".into(),
            }],
            session_fee: None,
        }
    }
}
