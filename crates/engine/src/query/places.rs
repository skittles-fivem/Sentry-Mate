//! `/places` with their counts (matched live), `/places/at`, `/places/suggestions`, `/places/preview`.

use super::{CarRef, List, Paging, QResult, QueryError, paginate, place_ref, r};
use crate::Engine;
use crate::placing::Places;
use model::{CarId, Place, PlaceId};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Counts {
    pub drives: u64,
    pub charges: u64,
    pub visits: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Rate {
    pub name: String,
    pub per_kwh: f64,
    pub per_minute: Option<f64>,
    pub days: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Price {
    pub currency: String,
    pub per_kwh: f64,
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
    pub address: Option<String>,
    pub created_ms: Option<u64>,
    pub version: String,
}

/// `0,1,2,3,4` → `mon-fri` style (days 0 = Monday).
fn days_word(days: &[u8]) -> String {
    const N: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    let mut d: Vec<u8> = days.iter().copied().filter(|&x| x < 7).collect();
    d.sort_unstable();
    d.dedup();
    if d.len() == 7 || d.is_empty() {
        return "all".into();
    }
    if d.windows(2).all(|w| w[1] == w[0] + 1) && d.len() > 2 {
        return format!("{}-{}", N[d[0] as usize], N[d[d.len() - 1] as usize]);
    }
    d.iter().map(|&x| N[x as usize]).collect::<Vec<_>>().join(",")
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
        price: p.price.as_ref().map(|pr| Price {
            currency: pr.currency.clone(),
            per_kwh: pr.per_kwh,
            per_minute: extra.and_then(|x| x.per_minute),
            rates: pr
                .rates
                .iter()
                .map(|rt| Rate {
                    name: rt.name.clone(),
                    per_kwh: rt.per_kwh,
                    per_minute: None,
                    days: days_word(&rt.days),
                    from: rt.from.clone(),
                    to: rt.to.clone(),
                })
                .collect(),
            session_fee: pr.session_fee,
        }),
        counts,
        last_visit_ms: last,
        address: extra.and_then(|x| x.address.clone()),
        created_ms: extra.and_then(|x| x.created_ms),
        version: format!("{}", extra.map_or(0, |x| x.version)),
    }
}

/// Each place's drives, charges and visits (by the live match), and its last visit, for the cars given.
pub fn counts(e: &Engine, cars: &[CarId]) -> QResult<BTreeMap<PlaceId, (Counts, Option<u64>)>> {
    let places = e.places();
    let mut by: BTreeMap<PlaceId, (Counts, Option<u64>)> = BTreeMap::new();
    let at = |p: Option<&model::Position>| p.and_then(|p| places.place_at(p.lat, p.lon)).map(|p| p.id);
    for &car in cars {
        for d in super::drives::rows(e, car, &super::Period::all(), None)? {
            let (s, en) = (at(d.start.as_ref()), at(d.end.as_ref()));
            for id in [s, en].into_iter().flatten().collect::<std::collections::BTreeSet<_>>() {
                by.entry(id).or_default().0.drives += 1;
            }
            if let Some(id) = en {
                let x = by.entry(id).or_default();
                x.0.visits += 1;
                x.1 = x.1.max(Some(d.end_ms));
            }
        }
        for ch in super::charges::rows(e, car, &super::Period::all(), None)? {
            if let Some(id) = at(ch.position.as_ref()) {
                by.entry(id).or_default().0.charges += 1;
            }
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

/// `/places`: every place, with counts for the cars given (or `q.car`).
pub fn places(e: &Engine, cars: &[CarId], q: &PlacesQuery, paging: &Paging) -> QResult<List<PlaceView>> {
    let pl = e.places();
    let for_cars: Vec<CarId> = match q.car {
        Some(c) if cars.contains(&c) => vec![c],
        Some(_) => return Err(QueryError::NoSuchCar),
        None => cars.to_vec(),
    };
    let counts = counts(e, &for_cars)?;
    let needle = q.search.as_deref().map(str::to_lowercase).filter(|s| !s.is_empty());
    let items: Vec<PlaceView> = pl
        .all()
        .iter()
        .filter(|p| needle.as_ref().is_none_or(|n| p.name.to_lowercase().contains(n)))
        .filter(|p| q.kind.as_deref().filter(|k| !k.is_empty()).is_none_or(|k| pl.kind_word(p) == k))
        .map(|p| {
            let (c, last) = counts.get(&p.id).cloned().unwrap_or_default();
            view(p, &pl, c, last)
        })
        .collect();
    let sort = q.sort.as_deref().unwrap_or("name");
    let sig = format!("places|{for_cars:?}|{:?}|{:?}|{sort}", q.search, q.kind);
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
        "name" => {
            let mut v = items;
            v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
            let pos: Vec<(PlaceView, f64)> = v.into_iter().enumerate().map(|(i, x)| (x, i as f64)).collect();
            let page = paginate(pos, |(x, k)| (*k, x.id.clone()), false, &sig, paging, true)?;
            Ok(List { items: page.items.into_iter().map(|(x, _)| x).collect(), next: page.next, total: page.total })
        }
        "visits" | "drives" | "last_visit" | "created" => {
            paginate(items, |v| (key(v), v.id.clone()), true, &sig, paging, true)
        }
        _ => Err(QueryError::bad("sort", "name, visits, drives, last_visit or created")),
    }
}

/// One place with its counts.
pub fn place(e: &Engine, cars: &[CarId], id: PlaceId) -> QResult<PlaceView> {
    let pl = e.places();
    let p = pl.get(id).ok_or(QueryError::NotFound("no_such_place"))?;
    let (c, last) = counts(e, cars)?.remove(&id).unwrap_or_default();
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
    pub address: Option<String>,
}

/// `/places/at`: the place a point is at by the nearest-centre rule, and every place whose circle holds it.
pub fn at(e: &Engine, lat: f64, lon: f64) -> QResult<At> {
    if !(-90.0..=90.0).contains(&lat) || !lat.is_finite() {
        return Err(QueryError::bad("lat", "-90 to 90"));
    }
    if !(-180.0..=180.0).contains(&lon) || !lon.is_finite() {
        return Err(QueryError::bad("lon", "-180 to 180"));
    }
    let pl = e.places();
    let candidates: Vec<Candidate> = pl
        .places_at(lat, lon)
        .into_iter()
        .map(|(p, d)| Candidate {
            id: place_ref(p.id),
            name: p.name.clone(),
            kind: pl.kind_word(p),
            distance_m: r(Some(d), 1).unwrap_or(d),
            radius_m: p.radius_m,
        })
        .collect();
    let place = pl.place_at(lat, lon).map(|p| view(p, &pl, Counts::default(), None));
    Ok(At { lat, lon, place, candidates, rule: "nearest_centre", address: None })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Suggestion {
    pub lat: f64,
    pub lon: f64,
    pub radius_m: f64,
    pub visits: u64,
    pub dwell_ms: Option<u64>,
    pub cars: Vec<CarRef>,
    pub label: Option<String>,
    pub address: Option<String>,
    pub last_visit_ms: u64,
    pub kind: Option<&'static str>,
}

/// `/places/suggestions`: where the cars park most that no saved place covers, most visited first.
pub fn suggestions(e: &Engine, cars: &[CarId], limit: Option<usize>) -> QResult<List<Suggestion>> {
    let max = limit.unwrap_or(8);
    if !(1..=super::MAX_LIMIT).contains(&max) {
        return Err(QueryError::bad("limit", "1 to 500"));
    }
    let pl = e.places();
    let index = places::PlaceIndex::new(pl.all().to_vec());
    let opts = places::SuggestOptions { max, ..places::SuggestOptions::default() };
    let mut sug = places::Suggester::new(&index, opts);
    for &car in cars {
        for d in super::drives::rows(e, car, &super::Period::all(), None)? {
            if let Some(end) = &d.end {
                sug.add(&places::Spot { lat: end.lat, lon: end.lon, t_ms: d.end_ms, car });
            }
        }
    }
    let items = sug
        .finish()
        .into_iter()
        .map(|s| Suggestion {
            lat: s.lat,
            lon: s.lon,
            radius_m: model::DEFAULT_RADIUS_M,
            visits: s.visits as u64,
            dwell_ms: None,
            cars: s.cars.iter().map(|&c| CarRef(c)).collect(),
            label: None,
            address: None,
            last_visit_ms: s.last_ms,
            kind: None,
        })
        .collect();
    Ok(List { items, next: None, total: None })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Preview {
    pub drives_named: u64,
    pub drives_unnamed: u64,
    pub charges_named: u64,
    pub charges_unnamed: u64,
    pub drives_taken: u64,
    pub charges_taken: u64,
    pub taken_from: Vec<serde_json::Value>,
    pub charges_repriced: u64,
    pub examples: Vec<String>,
    pub example_rows: Vec<serde_json::Value>,
}

/// `/places/preview`: what saving `draft` (a new place when its id is not saved, else an edit) or deleting a place
/// would change: the drives and charges that move between places. Nothing is changed.
pub fn preview(e: &Engine, cars: &[CarId], draft: &Place, delete: bool) -> QResult<Preview> {
    let now = e.places();
    let mut list: Vec<Place> = now.all().iter().filter(|p| p.id != draft.id).cloned().collect();
    let new_id = if now.get(draft.id).is_some() { draft.id } else { u32::MAX };
    if !delete {
        let mut d = draft.clone();
        d.id = new_id;
        list.push(d);
    }
    let extras = now.all().iter().filter_map(|p| now.extra(p.id).map(|x| (p.id, x.clone()))).collect();
    let after = Places::new(list, extras);
    let target = if delete { draft.id } else { new_id };
    let mut out = Preview {
        drives_named: 0,
        drives_unnamed: 0,
        charges_named: 0,
        charges_unnamed: 0,
        drives_taken: 0,
        charges_taken: 0,
        taken_from: Vec::new(),
        charges_repriced: 0,
        examples: Vec::new(),
        example_rows: Vec::new(),
    };
    let mut taken: BTreeMap<PlaceId, (u64, u64)> = BTreeMap::new();
    let id_at = |p: &Places, pos: Option<&model::Position>| pos.and_then(|x| p.place_at(x.lat, x.lon)).map(|x| x.id);
    let note = |out: &mut Preview, kind: &str, id: u64, car: CarId, start: u64, change: &str| {
        if out.example_rows.len() < 8 {
            out.examples.push(id.to_string());
            out.example_rows.push(serde_json::json!({ "kind": kind, "id": id.to_string(), "car": CarRef(car), "start_ms": start, "change": change }));
        }
    };
    for &car in cars {
        for d in super::drives::rows(e, car, &super::Period::all(), None)? {
            for pos in [d.start.as_ref(), d.end.as_ref()] {
                let (b, a) = (id_at(&now, pos), id_at(&after, pos));
                if b == a {
                    continue;
                }
                if a == Some(target) {
                    out.drives_named += 1;
                    if let Some(from) = b {
                        out.drives_taken += 1;
                        taken.entry(from).or_default().0 += 1;
                    }
                    note(&mut out, "drive", d.id, car, d.start_ms, "named");
                } else if b == Some(target) || delete {
                    out.drives_unnamed += 1;
                    note(&mut out, "drive", d.id, car, d.start_ms, "unnamed");
                }
            }
        }
        for ch in super::charges::rows(e, car, &super::Period::all(), None)? {
            let (b, a) = (id_at(&now, ch.position.as_ref()), id_at(&after, ch.position.as_ref()));
            if b == a {
                continue;
            }
            if a == Some(target) {
                out.charges_named += 1;
                if let Some(from) = b {
                    out.charges_taken += 1;
                    taken.entry(from).or_default().1 += 1;
                }
                if draft.price.is_some() {
                    out.charges_repriced += 1;
                }
                note(&mut out, "charge", ch.id, car, ch.start_ms, "named");
            } else if b == Some(target) || delete {
                out.charges_unnamed += 1;
                note(&mut out, "charge", ch.id, car, ch.start_ms, "unnamed");
            }
        }
    }
    let mut tf: Vec<(PlaceId, (u64, u64))> = taken.into_iter().collect();
    tf.sort_by_key(|a| std::cmp::Reverse(a.1.0 + a.1.1));
    out.taken_from = tf
        .into_iter()
        .map(|(id, (d, c))| {
            let name = now.get(id).map(|p| p.name.clone());
            serde_json::json!({ "place": { "id": place_ref(id), "name": name }, "drives": d, "charges": c })
        })
        .collect();
    Ok(out)
}
