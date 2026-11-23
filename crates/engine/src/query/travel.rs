//! `/visits` (one stop: from arriving to leaving), `/visits/summary`, the stops grouped by place, city, region and
//! country, `/visits/days`, and `/tracks`.
//!
//! A stop is the time between two drives: it starts where a drive ended and ends when the next one starts (the stop
//! going on now has no end). Its place is matched live ([`crate::placing::Places`]), so a renamed or moved place
//! changes every stop at once. Stops are made from the drives as they are read, a db page at a time: a list stops
//! reading once its page is full (arrival order) or keeps only its page while it reads (longest stays); the summary
//! and the groups keep counters and one entry per group, never the stops themselves.
//!
//! Addresses: a stop has the short address its drive kept (imported history has them); street, town, region and
//! country are not known until the server has an address lookup, so they are null, and grouping by them puts
//! every stop under one unknown key (`-`).

use super::drives::{Best, each, page_of};
use super::reports::{LONGEST_DRIVE_MS, drive_from, overlapping, still_unknown};
use super::system::Shown;
use super::{
    CarRef, Money, Paging, Period, PlaceBrief, QResult, QueryError, car_of, cmp_key, money, place_ref, r, read_after,
};
use crate::Engine;
use crate::tz;
use db::StateRow;
use model::{Car, CarId, Charge, Drive, State};
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

const DAY: u64 = 86_400_000;

/// A stop's address as rows carry it (only `short` is known for now; see the module's notes).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct VisitAddress {
    pub short: Option<String>,
    pub street: Option<String>,
    pub neighbourhood: Option<String>,
    pub locality: Option<String>,
    pub region: Option<String>,
    pub country_code: Option<String>,
    pub country: Option<String>,
    /// `teslamate` (kept by imported history), else null.
    pub source: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Visit {
    pub id: String,
    pub car: CarRef,
    pub time_zone: String,
    pub arrived_ms: u64,
    /// Null for the stop going on now.
    pub left_ms: Option<u64>,
    pub lat: f64,
    pub lon: f64,
    pub accuracy_m: Option<f64>,
    pub place: Option<PlaceBrief>,
    /// The place's name, else the short address.
    pub label: Option<String>,
    pub address: VisitAddress,
    /// `<country code>/<region>/<town>`; null while the address parts are unknown.
    pub city_key: Option<String>,
    pub drive_in: Option<String>,
    pub drive_out: Option<String>,
    pub charges: Vec<String>,
    /// When each of `charges` started (to place it on its day).
    #[serde(skip)]
    pub(crate) charge_starts: Vec<u64>,
    /// Where a stop with no drive after it ends for counting: null while the car can still report (it lasts until
    /// now), else when its history stops (a car with only imported history: nothing is known after that).
    #[serde(skip)]
    pub(crate) open_until: Option<u64>,
    /// Time inside the stop with no data (up to now when the car's data stopped during it).
    pub unknown_ms: u64,
    #[serde(flatten)]
    pub from: super::Sources,
}

/// `/visits` filters (the groups and days take the same).
#[derive(Debug, Clone, Default)]
pub struct VisitsQuery {
    pub period: Period,
    pub q: Option<String>,
    pub place: Option<String>,
    pub country: Option<String>,
    pub region: Option<String>,
    pub city: Option<String>,
    /// `arrived_desc` (default), `arrived_asc`, `dwell_desc`.
    pub sort: Option<String>,
}

impl VisitsQuery {
    fn sig(&self) -> String {
        format!(
            "{}|{:?}|{:?}|{:?}|{:?}|{:?}",
            self.period.sig(),
            self.q,
            self.place,
            self.country,
            self.region,
            self.city
        )
    }
}

/// The checked filters.
struct Filter {
    a: u64,
    b: u64,
    place: Option<model::PlaceId>,
    needle: Option<String>,
    country: Option<String>,
    region: Option<String>,
    city: Option<String>,
}

impl Filter {
    fn new(q: &VisitsQuery) -> QResult<Filter> {
        let (a, b) = q.period.bounds()?;
        let place = match q.place.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            Some(p) => Some(super::parse_place(p).ok_or_else(|| QueryError::bad("place", "a place id such as p3"))?),
            None => None,
        };
        let clean = |s: &Option<String>| s.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_lowercase);
        Ok(Filter {
            a,
            b,
            place,
            needle: clean(&q.q),
            country: clean(&q.country),
            region: clean(&q.region),
            city: q.city.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string),
        })
    }

    fn keeps(&self, v: &Visit) -> bool {
        if !(self.a..self.b).contains(&v.arrived_ms) {
            return false;
        }
        if self.place.is_some_and(|id| v.place.as_ref().is_none_or(|p| p.id != place_ref(id))) {
            return false;
        }
        let ad = &v.address;
        // an unknown part matches `-`, the key its group row hands out
        let part = |x: &Option<String>| x.as_deref().map_or_else(|| "-".to_string(), str::to_lowercase);
        if self.country.as_ref().is_some_and(|c| part(&ad.country_code) != *c) {
            return false;
        }
        if self.region.as_ref().is_some_and(|c| part(&ad.region) != *c) {
            return false;
        }
        if self.city.as_ref().is_some_and(|c| group_key(Level::City, v) != *c) {
            return false;
        }
        if let Some(n) = &self.needle {
            let text = [
                v.place.as_ref().map(|p| p.name.as_str()),
                ad.short.as_deref(),
                ad.street.as_deref(),
                ad.neighbourhood.as_deref(),
                ad.locality.as_deref(),
                ad.region.as_deref(),
                ad.country.as_deref(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
            if !text.contains(n.as_str()) {
                return false;
            }
        }
        true
    }
}

/// What making a car's stops needs.
struct Stops<'a> {
    e: &'a Engine,
    c: Car,
    shown: Shown,
    places: std::sync::Arc<crate::placing::Places>,
    live: Option<u64>,
    /// When the car's data stopped, if its newest state is unknown.
    still: Option<u64>,
    /// When the history of a car with no CAN history stops (see [`Visit::open_until`]).
    history_end: Option<u64>,
}

impl Visit {
    /// When the stop ends for counting: when the car left, else when its history stops, else `now`.
    fn end_or(&self, now: u64) -> u64 {
        self.left_ms.or(self.open_until).unwrap_or(now).min(now)
    }
}

/// When the history of a car with no CAN rows stops (its newest row's end); `None` for a car that has CAN history and
/// so may still report.
fn imported_history_end(e: &Engine, car: CarId) -> QResult<Option<u64>> {
    let q = db::Query { source: Some(model::Source::Live), ..db::Query::default().limit(1) };
    if !e.db().list::<StateRow>(car, &q)?.items.is_empty() || !e.db().list::<Drive>(car, &q)?.items.is_empty() {
        return Ok(None);
    }
    Ok(e.db().entry(car)?.and_then(|x| x.summary.last_activity_ms))
}

impl<'a> Stops<'a> {
    fn new(e: &'a Engine, car: CarId) -> QResult<Stops<'a>> {
        let c = car_of(e, car)?;
        let live = super::drives::ctx(e, &c)?.live;
        let still = still_unknown(e, car)?;
        let history_end = imported_history_end(e, car)?;
        Ok(Stops { e, shown: Shown::new(e, car, None)?, places: e.places(), live, still, history_end, c })
    }

    /// The stop after drive `d` (until `next` starts), without its charges and unknown time ([`Stops::fill`]).
    fn stop(&self, d: &Drive, next: Option<&Drive>) -> Option<Visit> {
        if self.live == Some(d.id) {
            return None;
        }
        let at = d.end.as_ref().or(next.and_then(|n| n.start.as_ref()))?;
        let place = self.places.place_at(at.lat, at.lon);
        let short = d.end_address.clone().or_else(|| next.and_then(|n| n.start_address.clone()));
        let car = self.c.id;
        Some(Visit {
            id: format!("c{car}-{}", d.end_ms),
            car: CarRef(car),
            time_zone: self.c.time_zone.clone(),
            arrived_ms: d.end_ms,
            left_ms: next.map(|n| n.start_ms),
            lat: r(Some(at.lat), 6).unwrap_or(at.lat),
            lon: r(Some(at.lon), 6).unwrap_or(at.lon),
            accuracy_m: None,
            place: place.map(|p| PlaceBrief { id: place_ref(p.id), name: p.name.clone() }),
            label: place.map(|p| p.name.clone()).or_else(|| short.clone()),
            address: VisitAddress { source: short.as_ref().map(|_| "teslamate"), short, ..VisitAddress::default() },
            city_key: None,
            drive_in: Some(d.id.to_string()),
            drive_out: next.map(|n| n.id.to_string()),
            charges: Vec::new(),
            charge_starts: Vec::new(),
            open_until: if next.is_none() { self.history_end.map(|t| t.max(d.end_ms)) } else { None },
            unknown_ms: 0,
            from: super::Sources::of(d.source, d.inputs),
        })
    }

    /// A stop's charges (those that started in it) and its time with no data: the unknown states inside it, and,
    /// when the car's data stopped during it, the time from then on ([`still_unknown`]).
    fn fill(&self, v: &mut Visit) -> QResult<()> {
        let (x, y) = (v.arrived_ms, v.end_or(crate::now_ms()).max(v.arrived_ms));
        let (mut charges, mut starts) = (Vec::new(), Vec::new());
        each::<Charge>(self.e, self.c.id, x, y.max(x + 1), None, &self.shown, false, |ch| {
            charges.push(ch.id.to_string());
            starts.push(ch.start_ms);
            Ok(charges.len() < 1000)
        })?;
        v.charges = charges;
        v.charge_starts = starts;
        let still = self.still;
        let mut unknown = 0;
        overlapping::<StateRow>(self.e, self.c.id, x, y, None, false, |s| {
            if s.span.state == State::Unknown && still != Some(s.span.start_ms) {
                unknown += s.span.end_ms.min(y).saturating_sub(s.span.start_ms.max(x));
            }
            Ok(true)
        })?;
        if let Some(t) = still {
            unknown += y.saturating_sub(t.max(x));
        }
        v.unknown_ms = unknown;
        Ok(())
    }

    /// Calls `f` with each stop that arrived in `[a, b)`, newest or oldest first, reading the drives a page at a
    /// time; `f` answers whether to go on.
    fn walk(&self, a: u64, b: u64, newest_first: bool, mut f: impl FnMut(Visit) -> QResult<bool>) -> QResult<()> {
        // a stop arrives when its drive ends: read the drives that started a little before the period too
        let from = a.saturating_sub(LONGEST_DRIVE_MS);
        let car = self.c.id;
        let mut held: Option<Drive> = None;
        let mut go = true;
        if newest_first {
            each::<Drive>(self.e, car, from, b, None, &self.shown, true, |d| {
                let next = match held.take() {
                    Some(n) => Some(n),
                    None => drive_from(self.e, car, d.start_ms.saturating_add(1), &self.shown)?,
                };
                if (a..b).contains(&d.end_ms)
                    && let Some(v) = self.stop(&d, next.as_ref())
                {
                    go = f(v)?;
                }
                let older = d.end_ms < a;
                held = Some(d);
                Ok(go && !older)
            })?;
        } else {
            each::<Drive>(self.e, car, from, b, None, &self.shown, false, |d| {
                if let Some(prev) = held.take()
                    && (a..b).contains(&prev.end_ms)
                    && let Some(v) = self.stop(&prev, Some(&d))
                {
                    go = f(v)?;
                }
                held = Some(d);
                Ok(go)
            })?;
            if go
                && let Some(prev) = held.take()
                && (a..b).contains(&prev.end_ms)
            {
                let next = drive_from(self.e, car, prev.start_ms.saturating_add(1), &self.shown)?;
                if let Some(v) = self.stop(&prev, next.as_ref()) {
                    f(v)?;
                }
            }
        }
        Ok(())
    }
}

/// The `now` a list's first page measured with, from the cursor's row key (`<key>@<now>`), else now.
fn cursor_now(after: Option<&(f64, String)>) -> u64 {
    after
        .and_then(|k| k.1.rsplit_once('@'))
        .and_then(|(_, n)| n.parse::<u64>().ok())
        .map_or_else(crate::now_ms, |n| n.min(crate::now_ms()))
}

/// How long a stop lasted within `[a, b)` and up to now.
fn dwell(v: &Visit, a: u64, b: u64, now: u64) -> u64 {
    let x = v.arrived_ms.max(a);
    let y = v.end_or(now).min(b);
    y.saturating_sub(x)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Visits {
    pub cars: Vec<CarRef>,
    pub items: Vec<Visit>,
    pub next: Option<String>,
}

/// `/visits` for the cars given.
pub fn visits(e: &Engine, cars: &[CarId], q: &VisitsQuery, paging: &Paging) -> QResult<Visits> {
    let f = Filter::new(q)?;
    let limit = paging.limit()?;
    let sort = q.sort.as_deref().unwrap_or("arrived_desc");
    let (desc, by_dwell) = match sort {
        "arrived_desc" => (true, false),
        "arrived_asc" => (false, false),
        "dwell_desc" => (true, true),
        _ => return Err(QueryError::bad("sort", "arrived_desc, arrived_asc or dwell_desc")),
    };
    let sig = format!("visits|{cars:?}|{}|{sort}", q.sig());
    let after = read_after(paging, &sig)?;
    // the stop going on now grows: by dwell, every page measures it up to the first page's now (kept in the cursor)
    let now = cursor_now(after.as_ref());
    let key = |v: &Visit| -> (f64, String) {
        if by_dwell {
            (dwell(v, f.a, f.b, now) as f64, format!("{}@{now}", v.id))
        } else {
            (v.arrived_ms as f64, v.id.clone())
        }
    };
    let past = |k: &(f64, String)| after.as_ref().is_none_or(|a| cmp_key(k, a, desc) == Ordering::Greater);
    let mut best: Best<(usize, Visit)> = Best::new(limit + 1, desc, after.clone());
    let mut makers = Vec::with_capacity(cars.len());
    for (i, &car) in cars.iter().enumerate() {
        let s = Stops::new(e, car)?;
        let mut n = 0;
        s.walk(f.a, f.b, desc || by_dwell, |v| {
            if !f.keeps(&v) {
                return Ok(true);
            }
            let k = key(&v);
            if !past(&k) {
                return Ok(true);
            }
            best.offer(k, (i, v));
            n += 1;
            Ok(by_dwell || n <= limit)
        })?;
        makers.push(s);
    }
    let (rows, next) = page_of(best, limit, &sig)?;
    let mut items = Vec::with_capacity(rows.len());
    for (i, mut v) in rows {
        makers[i].fill(&mut v)?;
        items.push(v);
    }
    Ok(Visits { cars: cars.iter().map(|&c| CarRef(c)).collect(), items, next })
}

// ---------------------------------------------------------------- groups and the summary

/// Which visits a group row gathers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Place,
    City,
    Region,
    Country,
}

impl Level {
    pub fn parse(s: &str) -> Option<Level> {
        match s {
            "places" | "place" => Some(Level::Place),
            "cities" | "city" => Some(Level::City),
            "regions" | "region" => Some(Level::Region),
            "countries" | "country" => Some(Level::Country),
            _ => None,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Level::Place => "place",
            Level::City => "city",
            Level::Region => "region",
            Level::Country => "country",
        }
    }
}

/// A stop at no saved place, keyed by where it was (about 150 m squares).
fn pos_key(lat: f64, lon: f64) -> String {
    let k = 1.0 / 0.0014;
    format!("pos:{}:{}", (lat * k).round(), (lon * k * lat.to_radians().cos()).round())
}

fn group_key(level: Level, v: &Visit) -> String {
    let a = &v.address;
    let part = |s: &Option<String>| s.clone().unwrap_or_else(|| "-".into());
    match level {
        Level::Place => v.place.as_ref().map_or_else(|| pos_key(v.lat, v.lon), |p| p.id.clone()),
        Level::City => v.city_key.clone().unwrap_or_else(|| "-/-/-".into()),
        Level::Region => format!("{}/{}", part(&a.country_code), part(&a.region)),
        Level::Country => part(&a.country_code),
    }
}

/// The local dates a stop covers (up to now), at most 400.
fn days_of(v: &Visit, now: u64) -> Vec<chrono::NaiveDate> {
    let z = tz::zone(&v.time_zone);
    let a = v.arrived_ms;
    let b = v.end_or(now).max(a + 1);
    let (d0, d1) = (tz::date(a, z), tz::date(b - 1, z));
    let mut out = Vec::new();
    let mut d = d0;
    while d <= d1 && out.len() < 400 {
        out.push(d);
        match d.succ_opt() {
            Some(n) => d = n,
            None => break,
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VisitGroup {
    pub key: String,
    pub label: Option<String>,
    pub level: &'static str,
    /// The saved place (`/visits/places` only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub place: Option<Option<PlaceBrief>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub street: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub neighbourhood: Option<Option<String>>,
    pub locality: Option<String>,
    pub region: Option<String>,
    pub country_code: Option<String>,
    pub country: Option<String>,
    pub lat: f64,
    pub lon: f64,
    pub visits: u64,
    pub places: u64,
    pub days: u64,
    pub dwell_ms: u64,
    pub first_ms: u64,
    pub last_ms: u64,
    pub first_car: CarRef,
    pub last_car: CarRef,
    pub cars: Vec<CarRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VisitGroups {
    pub cars: Vec<CarRef>,
    pub level: &'static str,
    pub items: Vec<VisitGroup>,
    pub next: Option<String>,
}

struct GroupAcc {
    g: VisitGroup,
    lat: f64,
    lon: f64,
    places: BTreeSet<String>,
    days: BTreeSet<(CarId, chrono::NaiveDate)>,
}

/// `/visits/places`, `/visits/cities`, `/visits/regions`, `/visits/countries`: the stops grouped, `sort=visits`
/// (default), `dwell`, `last` (most first) or `name`.
pub fn visit_groups(
    e: &Engine,
    cars: &[CarId],
    q: &VisitsQuery,
    level: Level,
    sort: Option<&str>,
    paging: &Paging,
) -> QResult<VisitGroups> {
    let f = Filter::new(q)?;
    let s = sort.unwrap_or("visits");
    if !["visits", "dwell", "last", "name"].contains(&s) {
        return Err(QueryError::bad("sort", "visits, dwell, last or name"));
    }
    // every page measures the stop going on now up to the first page's now (kept in the cursor), so the keys of the
    // `visits`, `dwell` and `last` orders do not move between pages
    let sig = format!("visit-groups|{cars:?}|{}|{}|{s}", q.sig(), level.word());
    let now = cursor_now(read_after(paging, &sig)?.as_ref());
    let mut by: BTreeMap<String, GroupAcc> = BTreeMap::new();
    for &car in cars {
        let st = Stops::new(e, car)?;
        st.walk(f.a, f.b, false, |v| {
            if !f.keeps(&v) {
                return Ok(true);
            }
            let key = group_key(level, &v);
            let ad = &v.address;
            let acc = by.entry(key.clone()).or_insert_with(|| {
                let label = match level {
                    Level::Place => v.label.clone(),
                    Level::City => ad.locality.clone().or_else(|| ad.short.clone()),
                    Level::Region => ad.region.clone().or_else(|| ad.short.clone()),
                    Level::Country => ad.country.clone().or_else(|| ad.country_code.clone()),
                };
                let place_level = level == Level::Place;
                GroupAcc {
                    g: VisitGroup {
                        key,
                        label,
                        level: level.word(),
                        place: place_level.then(|| v.place.clone()),
                        street: place_level.then(|| ad.street.clone()),
                        neighbourhood: place_level.then(|| ad.neighbourhood.clone()),
                        locality: if matches!(level, Level::Place | Level::City) { ad.locality.clone() } else { None },
                        region: if level == Level::Country { None } else { ad.region.clone() },
                        country_code: ad.country_code.clone(),
                        country: ad.country.clone(),
                        lat: 0.0,
                        lon: 0.0,
                        visits: 0,
                        places: 0,
                        days: 0,
                        dwell_ms: 0,
                        first_ms: v.arrived_ms,
                        last_ms: v.arrived_ms,
                        first_car: v.car,
                        last_car: v.car,
                        cars: Vec::new(),
                    },
                    lat: 0.0,
                    lon: 0.0,
                    places: BTreeSet::new(),
                    days: BTreeSet::new(),
                }
            });
            acc.g.visits += 1;
            acc.lat += v.lat;
            acc.lon += v.lon;
            acc.places.insert(v.place.as_ref().map_or_else(|| pos_key(v.lat, v.lon), |p| p.id.clone()));
            for d in days_of(&v, now) {
                acc.days.insert((v.car.0, d));
            }
            acc.g.dwell_ms += dwell(&v, f.a, f.b, now);
            if v.arrived_ms < acc.g.first_ms {
                acc.g.first_ms = v.arrived_ms;
                acc.g.first_car = v.car;
            }
            let left = v.end_or(now);
            if left > acc.g.last_ms {
                acc.g.last_ms = left;
                acc.g.last_car = v.car;
            }
            if !acc.g.cars.contains(&v.car) {
                acc.g.cars.push(v.car);
            }
            Ok(true)
        })?;
    }
    let places = e.places();
    let rows: Vec<VisitGroup> = by
        .into_values()
        .map(|mut a| {
            let n = a.g.visits.max(1) as f64;
            let centre = match a.g.place.as_ref().and_then(|p| p.as_ref()).and_then(|p| super::parse_place(&p.id)) {
                Some(id) => places.get(id).map(|p| (p.lat, p.lon)),
                None => None,
            };
            let (lat, lon) = centre.unwrap_or((a.lat / n, a.lon / n));
            a.g.lat = r(Some(lat), 6).unwrap_or(lat);
            a.g.lon = r(Some(lon), 6).unwrap_or(lon);
            a.g.places = a.places.len() as u64;
            a.g.days = a.days.len() as u64;
            a.g.cars.sort();
            a.g
        })
        .collect();
    let page = if s == "name" {
        // keyed by the lowercased label (no label first), then the group's key: a group added between pages moves
        // no other row
        let name_key = |g: &VisitGroup| {
            let label =
                g.label.as_deref().map_or_else(|| "\u{0}".to_string(), |l| format!("\u{1}{}", l.to_lowercase()));
            (0.0, format!("{label}\u{0}{}", g.key))
        };
        super::paginate(rows, name_key, false, &sig, paging, false)?
    } else {
        super::paginate(
            rows,
            |g| {
                let k = match s {
                    "dwell" => g.dwell_ms as f64,
                    "last" => g.last_ms as f64,
                    _ => g.visits as f64 + (g.dwell_ms as f64 / 1e14).min(0.999),
                };
                (k, format!("{}@{now}", g.key))
            },
            true,
            &sig,
            paging,
            false,
        )?
    };
    Ok(VisitGroups {
        cars: cars.iter().map(|&c| CarRef(c)).collect(),
        level: level.word(),
        items: page.items,
        next: page.next,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Efficiency {
    /// Energy into the pack over the energy from the grid, of the charges that know both, %.
    pub value: Option<f64>,
    pub charges_counted: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VisitsSummary {
    pub cars: Vec<CarRef>,
    pub visits: u64,
    pub places: u64,
    /// Null while no stop's address part is known.
    pub cities: Option<u64>,
    pub regions: Option<u64>,
    pub countries: Option<u64>,
    pub days: u64,
    pub first_ms: Option<u64>,
    pub last_ms: Option<u64>,
    pub first_car: Option<CarRef>,
    pub last_car: Option<CarRef>,
    pub unnamed_stops: u64,
    pub distance_km: f64,
    pub energy_added_kwh: f64,
    pub energy_grid_kwh: f64,
    pub efficiency: Efficiency,
    pub cost_by_currency: Vec<Money>,
    pub charges_without_cost: u64,
}

/// `/visits/summary`: the stops' counts with the same filters, and the period's driving and charging (as the
/// lists give them) for the same cars.
pub fn visits_summary(e: &Engine, cars: &[CarId], q: &VisitsQuery) -> QResult<VisitsSummary> {
    let f = Filter::new(q)?;
    let now = crate::now_ms();
    let (mut visits, mut unnamed) = (0u64, 0u64);
    let mut places = BTreeSet::new();
    let (mut cities, mut regions, mut countries) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    let mut days = BTreeSet::new();
    let mut first: Option<(u64, CarRef)> = None;
    let mut last: Option<(u64, CarRef)> = None;
    let (mut km, mut added, mut grid, mut unpriced) = (0.0, 0.0, 0.0, 0u64);
    let (mut added_g, mut grid_g, mut counted) = (0.0, 0.0, 0u64);
    let mut cost: BTreeMap<String, f64> = BTreeMap::new();
    for &car in cars {
        let st = Stops::new(e, car)?;
        st.walk(f.a, f.b, false, |v| {
            if !f.keeps(&v) {
                return Ok(true);
            }
            visits += 1;
            match &v.place {
                Some(p) => {
                    places.insert(p.id.clone());
                }
                None => {
                    unnamed += 1;
                    places.insert(pos_key(v.lat, v.lon));
                }
            }
            let ad = &v.address;
            if let Some(k) = &v.city_key {
                cities.insert(k.clone());
            }
            if let Some(rg) = &ad.region {
                regions.insert((ad.country_code.clone(), rg.clone()));
            }
            if let Some(cc) = &ad.country_code {
                countries.insert(cc.clone());
            }
            for d in days_of(&v, now) {
                days.insert((car, d));
            }
            if first.is_none_or(|x| v.arrived_ms < x.0) {
                first = Some((v.arrived_ms, v.car));
            }
            let left = v.end_or(now);
            if last.is_none_or(|x| left > x.0) {
                last = Some((left, v.car));
            }
            Ok(true)
        })?;
        each::<Drive>(e, car, f.a, f.b, None, &st.shown, false, |d| {
            km += d.distance_km.unwrap_or(0.0);
            Ok(true)
        })?;
        let cx = super::charges::ctx(e, &st.c)?;
        each::<Charge>(e, car, f.a, f.b, None, &st.shown, false, |ch| {
            let v = super::charges::view(e, &st.c, &ch, &cx)?;
            added += v.energy_added_kwh.unwrap_or(0.0);
            grid += v.energy_grid_kwh.unwrap_or(0.0);
            // the efficiency: over the charges whose two energies may be compared (as each charge's own)
            if let Some((a, g)) = super::charges::compared_energies(&ch) {
                added_g += a;
                grid_g += g;
                counted += 1;
            }
            match v.cost {
                Some(x) => *cost.entry(x.currency).or_default() += x.amount,
                None => unpriced += 1,
            }
            Ok(true)
        })?;
    }
    let known = |n: usize| if n == 0 && visits > 0 { None } else { Some(n as u64) };
    Ok(VisitsSummary {
        cars: cars.iter().map(|&c| CarRef(c)).collect(),
        visits,
        places: places.len() as u64,
        cities: known(cities.len()),
        regions: known(regions.len()),
        countries: known(countries.len()),
        days: days.len() as u64,
        first_ms: first.map(|x| x.0),
        last_ms: last.map(|x| x.0),
        first_car: first.map(|x| x.1),
        last_car: last.map(|x| x.1),
        unnamed_stops: unnamed,
        distance_km: r(Some(km), 3).unwrap_or(0.0),
        energy_added_kwh: r(Some(added), 3).unwrap_or(0.0),
        energy_grid_kwh: r(Some(grid), 3).unwrap_or(0.0),
        efficiency: Efficiency {
            value: r((grid_g > 0.0).then(|| added_g / grid_g * 100.0), 1),
            charges_counted: counted,
        },
        cost_by_currency: money(cost),
        charges_without_cost: unpriced,
    })
}

// ---------------------------------------------------------------- days

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VisitDay {
    pub date: String,
    pub car: CarRef,
    pub time_zone: String,
    pub first_ms: u64,
    pub last_ms: u64,
    pub drives: Vec<String>,
    pub charges: Vec<String>,
    pub places: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VisitDays {
    pub cars: Vec<CarRef>,
    pub city: Option<String>,
    pub items: Vec<VisitDay>,
    pub next: Option<String>,
}

fn day_key(d: chrono::NaiveDate, car: CarId) -> (f64, String) {
    use chrono::Datelike;
    (f64::from(d.num_days_from_ce()), format!("c{car}"))
}

/// `/visits/days`: each car's local days with stops that pass the filters (a city's days, mostly), newest first:
/// when it was there, its drives in and out that day, the charges that started that day, and the places. Each car's
/// stops are read newest first until the page is full.
pub fn visit_days(e: &Engine, cars: &[CarId], q: &VisitsQuery, paging: &Paging) -> QResult<VisitDays> {
    let f = Filter::new(q)?;
    let limit = paging.limit()?;
    let sig = format!("visit-days|{cars:?}|{}", q.sig());
    let after = read_after(paging, &sig)?;
    let now = crate::now_ms();
    let past = |k: &(f64, String)| after.as_ref().is_none_or(|a| cmp_key(k, a, true) == Ordering::Greater);
    let mut best: Best<VisitDay> = Best::new(limit + 1, true, after.clone());
    for &car in cars {
        let st = Stops::new(e, car)?;
        let z = tz::zone(&st.c.time_zone);
        let mut per: BTreeMap<chrono::NaiveDate, VisitDay> = BTreeMap::new();
        let mut done_days = 0usize;
        st.walk(f.a, f.b, true, |mut v| {
            if !f.keeps(&v) {
                return Ok(true);
            }
            st.fill(&mut v)?;
            let ds = days_of(&v, now);
            for d in &ds {
                if !past(&day_key(*d, car)) {
                    continue;
                }
                let a = tz::start_of(*d, z);
                let b = d.succ_opt().map_or(a + DAY, |n| tz::start_of(n, z));
                let (x, y) = (a.max(v.arrived_ms), b.min(v.end_or(now)));
                if y <= x {
                    continue;
                }
                let row = per.entry(*d).or_insert_with(|| VisitDay {
                    date: d.format("%Y-%m-%d").to_string(),
                    car: CarRef(car),
                    time_zone: st.c.time_zone.clone(),
                    first_ms: x,
                    last_ms: y,
                    drives: Vec::new(),
                    charges: Vec::new(),
                    places: Vec::new(),
                });
                row.first_ms = row.first_ms.min(x);
                row.last_ms = row.last_ms.max(y);
                // the drive in ended that day, the drive out started that day
                if let Some(din) = &v.drive_in
                    && (a..b).contains(&v.arrived_ms)
                    && !row.drives.contains(din)
                {
                    row.drives.push(din.clone());
                }
                if let (Some(dout), Some(l)) = (&v.drive_out, v.left_ms)
                    && (a..b).contains(&l)
                    && !row.drives.contains(dout)
                {
                    row.drives.push(dout.clone());
                }
                // the charges that started that day
                for (ch, at) in v.charges.iter().zip(&v.charge_starts) {
                    if (a..b).contains(at) && !row.charges.contains(ch) {
                        row.charges.push(ch.clone());
                    }
                }
                if let Some(l) = &v.label
                    && !row.places.contains(l)
                {
                    row.places.push(l.clone());
                }
            }
            // the days after this stop's first are done (stops of a car never overlap)
            if let Some(d0) = ds.first() {
                done_days = per.range(d0.succ_opt().unwrap_or(*d0)..).count();
            }
            Ok(done_days <= limit)
        })?;
        for (d, row) in per {
            best.offer(day_key(d, car), row);
        }
    }
    let (items, next) = page_of(best, limit, &sig)?;
    Ok(VisitDays {
        cars: cars.iter().map(|&c| CarRef(c)).collect(),
        city: q.city.clone(),
        items: items
            .into_iter()
            .map(|mut d| {
                d.drives.sort_by(|a, b| super::cmp_ids(a, b));
                d
            })
            .collect(),
        next,
    })
}

// ---------------------------------------------------------------- tracks

/// Days of points each of them keeps decoded (its drives follow one another, so their days are read again).
const TRACK_KEPT_DAYS: usize = 12;

/// The points of each drive's track (`(car, drive id, ..)`, in their order; none for a drive gone meanwhile): read
/// on all cores, each a run of drives that follow one another, keeping the days it read decoded.
fn drive_tracks(
    e: &Engine,
    drives: &[(CarId, u64, u64, u64, model::Source)],
) -> QResult<Vec<Vec<super::drives::TrackPoint>>> {
    let run = |part: &[(CarId, u64, u64, u64, model::Source)]| {
        db::keeping_days(TRACK_KEPT_DAYS, || {
            // each car's import gap filling, for all its tracks (it keeps the import's days it read)
            let mut gaps: Vec<(CarId, super::gapfill::Gaps<'_>)> = Vec::new();
            let mut out = Vec::with_capacity(part.len());
            for &(car, id, ..) in part {
                if !gaps.iter().any(|(c, _)| *c == car) {
                    gaps.push((car, super::gapfill::Gaps::new(e, car, None)?));
                }
                let g = gaps.iter().find(|(c, _)| *c == car).map(|(_, g)| g);
                out.push(match super::drives::track_with(e, car, id, g) {
                    Ok(t) => t.stretches.into_iter().flatten().collect(),
                    Err(QueryError::NotFound(_)) => Vec::new(),
                    Err(x) => return Err(x),
                });
            }
            Ok(out)
        })
    };
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    if threads < 2 || drives.len() < 2 * threads {
        return run(drives);
    }
    let size = drives.len().div_ceil(threads);
    let parts: Vec<QResult<Vec<Vec<super::drives::TrackPoint>>>> = std::thread::scope(|sc| {
        let handles: Vec<_> = drives.chunks(size).map(|part| sc.spawn(move || run(part))).collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err(QueryError::Engine(crate::Error::Store("a track failed".into())))))
            .collect()
    });
    let mut out = Vec::with_capacity(drives.len());
    for p in parts {
        out.extend(p?);
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DriveTrack {
    pub car: CarRef,
    pub drive: String,
    #[serde(flatten)]
    pub from: super::Sources,
    pub t_ms: Vec<u64>,
    pub lat: Vec<f64>,
    pub lon: Vec<f64>,
    pub accuracy_m: Vec<Option<f64>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tracks {
    pub cars: Vec<CarRef>,
    pub from_ms: u64,
    pub to_ms: u64,
    /// Points sent, never more than `max_points`.
    pub points: u64,
    pub drives: Vec<DriveTrack>,
}

/// `/tracks`: every drive's track in the period, oldest first, thinned on the server: `max_points` (2 to 200000,
/// default 10000) is shared by duration, at least two each (`400` on `max_points` when the drives need more than
/// that), and the answer never holds more. One drive's points are read at a time.
pub fn tracks(e: &Engine, cars: &[CarId], period: &Period, max_points: Option<u64>) -> QResult<Tracks> {
    let max = max_points.unwrap_or(10_000);
    if !(2..=200_000).contains(&max) {
        return Err(QueryError::bad("max_points", "2 to 200000"));
    }
    let (a, b) = period.bounds()?;
    let end = b.min(crate::now_ms());
    let too_many = || {
        QueryError::bad("max_points", "too few for the drives of the period: ask for more points, or a shorter period")
    };
    // (car, drive id, start, end, source): small, and at most max / 2 of them
    let mut drives: Vec<(CarId, u64, u64, u64, model::Source)> = Vec::new();
    for &car in cars {
        let c = car_of(e, car)?;
        let shown = Shown::new(e, car, None)?;
        let live = super::drives::ctx(e, &c)?.live;
        let mut over = false;
        each::<Drive>(e, car, a, end.max(a), None, &shown, false, |d| {
            let to = if live == Some(d.id) { crate::now_ms() } else { d.end_ms };
            drives.push((car, d.id, d.start_ms, to.max(d.start_ms), d.source));
            over = drives.len() as u64 * 2 > max;
            Ok(!over)
        })?;
        if over {
            return Err(too_many());
        }
    }
    let total_ms: u64 = drives.iter().map(|d| (d.3 - d.2).max(1000)).sum::<u64>().max(1);
    let spare = max - 2 * drives.len() as u64;
    let mut out = Vec::with_capacity(drives.len());
    let mut points = 0u64;
    let tracks = drive_tracks(e, &drives)?;
    for ((car, id, start, to, source), pts) in drives.into_iter().zip(tracks) {
        let dur = (to - start).max(1000);
        let share = (spare as f64 * dur as f64 / total_ms as f64).floor() as u64;
        let want = (2 + share).min(dur / 1000 + 1).max(2) as usize;
        let picked: Vec<&super::drives::TrackPoint> = if pts.len() <= want {
            pts.iter().collect()
        } else {
            // evenly by index, the first and the last kept
            (0..want).map(|k| &pts[k * (pts.len() - 1) / (want - 1)]).collect()
        };
        points += picked.len() as u64;
        out.push(DriveTrack {
            car: CarRef(car),
            drive: id.to_string(),
            from: super::Sources::family(source),
            t_ms: picked.iter().map(|p| p.t_ms).collect(),
            lat: picked.iter().map(|p| r(Some(p.lat), 6).unwrap_or(p.lat)).collect(),
            lon: picked.iter().map(|p| r(Some(p.lon), 6).unwrap_or(p.lon)).collect(),
            accuracy_m: picked.iter().map(|p| r(p.accuracy_m, 1)).collect(),
        });
    }
    Ok(Tracks { cars: cars.iter().map(|&c| CarRef(c)).collect(), from_ms: a, to_ms: end, points, drives: out })
}
