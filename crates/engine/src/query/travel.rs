//! `/visits` (one stop: from arriving to leaving), `/visits/summary`, `/visits/places`, `/tracks`.

use super::{
    CarRef, List, Money, Paging, Period, PlaceBrief, QResult, QueryError, car_of, money, paginate, place_ref, r,
    source_word,
};
use crate::Engine;
use model::{CarId, Drive};
use serde::Serialize;
use std::collections::BTreeMap;

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
    pub address: serde_json::Value,
    pub label: Option<String>,
    pub city_key: Option<String>,
    pub drive_in: String,
    pub drive_out: Option<String>,
    pub charges: Vec<String>,
    pub unknown_ms: u64,
    pub source: &'static str,
}

/// `/visits` filters.
#[derive(Debug, Clone, Default)]
pub struct VisitsQuery {
    pub period: Period,
    pub q: Option<String>,
    pub place: Option<String>,
    /// `arrived_desc` (default), `arrived_asc`, `dwell_desc`.
    pub sort: Option<String>,
}

/// Every stop of a car: from the end of a drive to the start of the next one.
fn stops(e: &Engine, car: CarId, period: &Period) -> QResult<Vec<Visit>> {
    let c = car_of(e, car)?;
    let (a, b) = period.bounds()?;
    let drives: Vec<Drive> = super::drives::rows(e, car, &Period::all(), None)?;
    let charges = super::charges::rows(e, car, &Period::all(), None)?;
    let places = e.places();
    let mut out = Vec::new();
    for (i, d) in drives.iter().enumerate() {
        let Some(end) = &d.end else { continue };
        if d.end_ms < a || d.end_ms >= b {
            continue;
        }
        let next = drives.get(i + 1);
        let left = next.map(|n| n.start_ms);
        let place = places.place_at(end.lat, end.lon);
        let label = place.map(|p| p.name.clone()).or_else(|| d.end_address.clone());
        out.push(Visit {
            id: format!("visit:{}:{}", car, d.end_ms),
            car: CarRef(car),
            time_zone: c.time_zone.clone(),
            arrived_ms: d.end_ms,
            left_ms: left,
            lat: r(Some(end.lat), 6).unwrap_or(end.lat),
            lon: r(Some(end.lon), 6).unwrap_or(end.lon),
            accuracy_m: None,
            place: place.map(|p| PlaceBrief { id: place_ref(p.id), name: p.name.clone() }),
            address: serde_json::json!({
                "short": d.end_address, "street": null, "neighbourhood": null, "locality": null, "region": null,
                "country_code": null, "country": null, "source": null,
            }),
            label,
            city_key: None,
            drive_in: d.id.to_string(),
            drive_out: next.map(|n| n.id.to_string()),
            charges: charges
                .iter()
                .filter(|ch| ch.start_ms >= d.end_ms && left.is_none_or(|l| ch.start_ms < l))
                .map(|ch| ch.id.to_string())
                .collect(),
            unknown_ms: 0,
            source: source_word(d.source),
        });
    }
    Ok(out)
}

fn filtered(e: &Engine, cars: &[CarId], q: &VisitsQuery) -> QResult<Vec<Visit>> {
    let place = match q.place.as_deref().filter(|p| !p.is_empty()) {
        Some(p) => Some(super::parse_place(p).ok_or_else(|| QueryError::bad("place", "no such place"))?),
        None => None,
    };
    let needle = q.q.as_deref().map(str::to_lowercase).filter(|s| !s.is_empty());
    let mut out = Vec::new();
    for &car in cars {
        for v in stops(e, car, &q.period)? {
            if place.is_some_and(|id| v.place.as_ref().and_then(|p| super::parse_place(&p.id)) != Some(id)) {
                continue;
            }
            if needle.as_ref().is_some_and(|n| !v.label.as_deref().is_some_and(|l| l.to_lowercase().contains(n))) {
                continue;
            }
            out.push(v);
        }
    }
    Ok(out)
}

/// `/visits` for the cars given.
pub fn visits(e: &Engine, cars: &[CarId], q: &VisitsQuery, paging: &Paging) -> QResult<List<Visit>> {
    let items = filtered(e, cars, q)?;
    let now = crate::now_ms();
    let sort = q.sort.as_deref().unwrap_or("arrived_desc");
    let (desc, dwell) = match sort {
        "arrived_desc" => (true, false),
        "arrived_asc" => (false, false),
        "dwell_desc" => (true, true),
        _ => return Err(QueryError::bad("sort", "arrived_desc, arrived_asc or dwell_desc")),
    };
    let sig = format!("visits|{cars:?}|{}|{:?}|{:?}|{sort}", q.period.sig(), q.q, q.place);
    paginate(
        items,
        |v| {
            let k =
                if dwell { v.left_ms.unwrap_or(now).saturating_sub(v.arrived_ms) as f64 } else { v.arrived_ms as f64 };
            (k, v.id.clone())
        },
        desc,
        &sig,
        paging,
        false,
    )
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VisitGroup {
    pub key: String,
    pub label: Option<String>,
    pub level: &'static str,
    pub place: Option<PlaceBrief>,
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

/// `/visits/places`: the stops grouped by saved place (stops at no place by their label).
pub fn visit_places(
    e: &Engine,
    cars: &[CarId],
    q: &VisitsQuery,
    sort: Option<&str>,
    paging: &Paging,
) -> QResult<List<VisitGroup>> {
    let now = crate::now_ms();
    let mut by: BTreeMap<String, (VisitGroup, std::collections::BTreeSet<String>)> = BTreeMap::new();
    for v in filtered(e, cars, q)? {
        let key = v.place.as_ref().map_or_else(|| format!("at:{:.3},{:.3}", v.lat, v.lon), |p| p.id.clone());
        let dwell = v.left_ms.unwrap_or(now).saturating_sub(v.arrived_ms);
        let day = crate::tz::date_str(v.arrived_ms, crate::tz::zone(&v.time_zone));
        let g = by.entry(key.clone()).or_insert_with(|| {
            (
                VisitGroup {
                    key,
                    label: v.label.clone(),
                    level: "place",
                    place: v.place.clone(),
                    lat: v.lat,
                    lon: v.lon,
                    visits: 0,
                    places: 1,
                    days: 0,
                    dwell_ms: 0,
                    first_ms: v.arrived_ms,
                    last_ms: v.arrived_ms,
                    first_car: v.car,
                    last_car: v.car,
                    cars: Vec::new(),
                },
                Default::default(),
            )
        });
        g.0.visits += 1;
        g.0.dwell_ms += dwell;
        if v.arrived_ms < g.0.first_ms {
            g.0.first_ms = v.arrived_ms;
            g.0.first_car = v.car;
        }
        let last = v.left_ms.unwrap_or(now);
        if last > g.0.last_ms {
            g.0.last_ms = last;
            g.0.last_car = v.car;
        }
        if !g.0.cars.contains(&v.car) {
            g.0.cars.push(v.car);
        }
        g.1.insert(format!("{}:{day}", v.car.0));
    }
    let items: Vec<VisitGroup> = by
        .into_values()
        .map(|(mut g, days)| {
            g.days = days.len() as u64;
            g
        })
        .collect();
    let s = sort.unwrap_or("visits");
    if !["visits", "dwell", "last", "name"].contains(&s) {
        return Err(QueryError::bad("sort", "visits, dwell, last or name"));
    }
    let sig = format!("visit-places|{cars:?}|{}|{:?}|{s}", q.period.sig(), q.q);
    let mut ranked = items;
    if s == "name" {
        ranked.sort_by(|a, b| a.label.cmp(&b.label));
        let pos: Vec<(VisitGroup, f64)> = ranked.into_iter().enumerate().map(|(i, g)| (g, i as f64)).collect();
        let page = paginate(pos, |(g, k)| (*k, g.key.clone()), false, &sig, paging, false)?;
        return Ok(List { items: page.items.into_iter().map(|(g, _)| g).collect(), next: page.next, total: None });
    }
    paginate(
        ranked,
        |g| {
            let k = match s {
                "dwell" => g.dwell_ms as f64,
                "last" => g.last_ms as f64,
                _ => g.visits as f64,
            };
            (k, g.key.clone())
        },
        true,
        &sig,
        paging,
        false,
    )
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VisitsSummary {
    pub visits: u64,
    pub places: u64,
    pub cities: Option<u64>,
    pub regions: Option<u64>,
    pub countries: Option<u64>,
    pub days: u64,
    pub first_ms: Option<u64>,
    pub last_ms: Option<u64>,
    pub first_car: Option<CarRef>,
    pub last_car: Option<CarRef>,
    pub cars: u64,
    pub unnamed_stops: u64,
    pub distance_km: f64,
    pub energy_added_kwh: f64,
    pub energy_grid_kwh: f64,
    pub cost_by_currency: Vec<Money>,
    pub charges_without_cost: u64,
}

/// `/visits/summary`.
pub fn visits_summary(e: &Engine, cars: &[CarId], q: &VisitsQuery) -> QResult<VisitsSummary> {
    let vs = filtered(e, cars, q)?;
    let mut places = std::collections::BTreeSet::new();
    let mut days = std::collections::BTreeSet::new();
    let mut car_set = std::collections::BTreeSet::new();
    let mut unnamed = 0;
    for v in &vs {
        match &v.place {
            Some(p) => {
                places.insert(p.id.clone());
            }
            None => unnamed += 1,
        }
        days.insert(format!("{}:{}", v.car.0, crate::tz::date_str(v.arrived_ms, crate::tz::zone(&v.time_zone))));
        car_set.insert(v.car.0);
    }
    let mut km = 0.0;
    let (mut added, mut grid) = (0.0, 0.0);
    let mut cost: BTreeMap<String, f64> = BTreeMap::new();
    let mut unpriced = 0;
    for &car in cars {
        km += super::drives::rows(e, car, &q.period, None)?.iter().filter_map(|d| d.distance_km).sum::<f64>();
        let c = car_of(e, car)?;
        let cx = super::charges::ctx(e, &c)?;
        for ch in super::charges::rows(e, car, &q.period, None)? {
            added += ch.energy_added_kwh.unwrap_or(0.0);
            grid += ch.energy_used_kwh.unwrap_or(0.0);
            match super::charges::view(e, &c, &ch, &cx)?.cost {
                Some(x) => *cost.entry(x.currency).or_default() += x.amount,
                None => unpriced += 1,
            }
        }
    }
    let first = vs.iter().min_by_key(|v| v.arrived_ms);
    let last = vs.iter().max_by_key(|v| v.left_ms.unwrap_or(u64::MAX));
    Ok(VisitsSummary {
        visits: vs.len() as u64,
        places: places.len() as u64,
        cities: None,
        regions: None,
        countries: None,
        days: days.len() as u64,
        first_ms: first.map(|v| v.arrived_ms),
        last_ms: last.map(|v| v.left_ms.unwrap_or_else(crate::now_ms)),
        first_car: first.map(|v| v.car),
        last_car: last.map(|v| v.car),
        cars: car_set.len() as u64,
        unnamed_stops: unnamed,
        distance_km: r(Some(km), 3).unwrap_or(0.0),
        energy_added_kwh: r(Some(added), 3).unwrap_or(0.0),
        energy_grid_kwh: r(Some(grid), 3).unwrap_or(0.0),
        cost_by_currency: money(cost),
        charges_without_cost: unpriced,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DriveTrack {
    pub car: CarRef,
    pub drive: String,
    pub source: &'static str,
    pub t_ms: Vec<u64>,
    pub lat: Vec<f64>,
    pub lon: Vec<f64>,
    pub accuracy_m: Vec<Option<f64>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tracks {
    pub drives: Vec<DriveTrack>,
    pub points: u64,
}

/// `/tracks`: every drive's track in the period, `max_points` (2 to 200000, default 10000) shared by duration, at
/// least two each.
pub fn tracks(e: &Engine, cars: &[CarId], period: &Period, max_points: Option<u64>) -> QResult<Tracks> {
    let max = max_points.unwrap_or(10_000);
    if !(2..=200_000).contains(&max) {
        return Err(QueryError::bad("max_points", "2 to 200000"));
    }
    let mut drives = Vec::new();
    for &car in cars {
        for d in super::drives::rows(e, car, period, None)? {
            drives.push((car, d));
        }
    }
    if drives.len() as u64 * 2 > max {
        return Err(QueryError::bad(
            "max_points",
            "too few for the drives of the period: ask for more, or a shorter period",
        ));
    }
    let total_ms: u64 = drives.iter().map(|(_, d)| d.end_ms.saturating_sub(d.start_ms).max(1)).sum();
    let mut out = Vec::new();
    let mut points = 0u64;
    for (car, d) in drives {
        let share =
            ((max as f64) * d.end_ms.saturating_sub(d.start_ms).max(1) as f64 / total_ms.max(1) as f64).floor() as u64;
        let want = share.max(2);
        let pts: Vec<super::drives::TrackPoint> =
            super::drives::track(e, car, d.id)?.stretches.into_iter().flatten().collect();
        let step = (pts.len() as u64).div_ceil(want).max(1) as usize;
        let mut picked: Vec<&super::drives::TrackPoint> = pts.iter().step_by(step).collect();
        if let Some(l) = pts.last()
            && picked.last().is_some_and(|p| p.t_ms != l.t_ms)
            && (picked.len() as u64) < want
        {
            picked.push(l);
        }
        points += picked.len() as u64;
        out.push(DriveTrack {
            car: CarRef(car),
            drive: d.id.to_string(),
            source: source_word(d.source),
            t_ms: picked.iter().map(|p| p.t_ms).collect(),
            lat: picked.iter().map(|p| p.lat).collect(),
            lon: picked.iter().map(|p| p.lon).collect(),
            accuracy_m: picked.iter().map(|p| p.accuracy_m).collect(),
        });
    }
    Ok(Tracks { drives: out, points })
}
