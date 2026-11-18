//! `/cars/{car}/drives`, a drive, its track, `/drive-stats`.

use super::system::Shown;
use super::{
    CarRef, Paging, Period, QResult, QueryError, Scoped, Where, car_of, paginate, parse_place, r, scoped, source_word,
    where_of,
};
use crate::Engine;
use crate::pipeline::all_rows;
use model::{Car, CarId, Drive, Sample, Source};
use serde::Serialize;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DriveFlags {
    pub in_progress: bool,
    pub gap_ms: u64,
    pub lost_frames: u64,
    pub reduced_range: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DriveView {
    pub id: String,
    pub car: CarRef,
    pub time_zone: String,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub duration_ms: u64,
    pub start: Where,
    pub end: Where,
    pub start_odometer_km: Option<f64>,
    pub end_odometer_km: Option<f64>,
    pub distance_km: Option<f64>,
    pub speed_avg_kph: Option<f64>,
    pub speed_max_kph: Option<f64>,
    pub power_max_kw: Option<f64>,
    pub power_min_kw: Option<f64>,
    pub energy_kwh: Option<f64>,
    /// `measured` (CAN: the pack's power), `estimated` (imported: from range).
    pub energy_basis: &'static str,
    pub regen_kwh: Option<f64>,
    pub wh_per_km: Option<f64>,
    pub efficiency_pct: Option<f64>,
    pub start_soc_pct: Option<f64>,
    pub end_soc_pct: Option<f64>,
    pub start_range_km: Option<f64>,
    pub end_range_km: Option<f64>,
    pub outside_avg_c: Option<f64>,
    pub cabin_avg_c: Option<f64>,
    pub climb_m: Option<f64>,
    pub descent_m: Option<f64>,
    pub hvac_kwh: Option<f64>,
    pub source: &'static str,
    pub flags: DriveFlags,
    pub purpose: Option<serde_json::Value>,
}

/// What a drive's row needs besides the drive.
pub(crate) struct Ctx {
    pub places: std::sync::Arc<crate::placing::Places>,
    /// The id of the drive going on now, if any.
    pub live: Option<u64>,
    pub rated_wh_per_km: Option<f64>,
}

pub(crate) fn ctx(e: &Engine, c: &Car) -> QResult<Ctx> {
    let live = e.db().entry(c.id)?.and_then(|x| x.summary.now).filter(|n| n.driving).and_then(|n| {
        e.db()
            .list::<Drive>(c.id, &db::Query { source: Some(Source::Can), ..db::Query::default().limit(1) })
            .ok()
            .and_then(|p| p.items.into_iter().next())
            .filter(|d| d.end_ms + 120_000 >= n.at_ms)
            .map(|d| d.id)
    });
    Ok(Ctx { places: e.places(), live, rated_wh_per_km: c.efficiency_wh_km })
}

pub(crate) fn view(c: &Car, d: &Drive, x: &Ctx) -> DriveView {
    let live = x.live == Some(d.id);
    let duration = d.end_ms.saturating_sub(d.start_ms);
    let whkm = d.efficiency_wh_km();
    let (sp, ep) = (d.start.as_ref(), d.end.as_ref());
    let tm = d.source == Source::Teslamate;
    DriveView {
        id: d.id.to_string(),
        car: CarRef(c.id),
        time_zone: c.time_zone.clone(),
        start_ms: d.start_ms,
        end_ms: (!live).then_some(d.end_ms),
        duration_ms: duration,
        start: where_of(&x.places, sp.map(|p| p.lat), sp.map(|p| p.lon), d.start_address.as_deref(), None),
        end: where_of(
            &x.places,
            ep.map(|p| p.lat),
            ep.map(|p| p.lon),
            d.end_address.as_deref(),
            (live || ep.is_none()).then_some(if live { "Driving now" } else { "Data stopped here" }),
        ),
        start_odometer_km: r(d.start_odometer_km, 3),
        end_odometer_km: r(d.end_odometer_km, 3),
        distance_km: r(d.distance_km, 3),
        speed_avg_kph: r(d.distance_km.filter(|_| duration > 0).map(|km| km / (duration as f64 / 3_600_000.0)), 1),
        speed_max_kph: r(d.speed_max_kmh, 1),
        power_max_kw: r(d.power_max_kw, 1),
        power_min_kw: r(d.power_min_kw, 1),
        energy_kwh: r(d.energy_used_kwh, 3),
        energy_basis: if tm { "estimated" } else { "measured" },
        regen_kwh: None,
        wh_per_km: r(whkm, 1),
        efficiency_pct: r(x.rated_wh_per_km.zip(whkm).filter(|(_, w)| *w > 0.0).map(|(rated, w)| rated / w * 100.0), 1),
        start_soc_pct: r(d.start_soc, 1),
        end_soc_pct: r(d.end_soc, 1),
        start_range_km: r(d.start_range_km, 1),
        end_range_km: r(d.end_range_km, 1),
        outside_avg_c: r(d.outside_temp_avg_c, 1),
        cabin_avg_c: r(d.inside_temp_avg_c, 1),
        climb_m: r(d.ascent_m, 0),
        descent_m: r(d.descent_m, 0),
        hvac_kwh: None,
        source: source_word(d.source),
        flags: DriveFlags { in_progress: live, ..DriveFlags::default() },
        purpose: None,
    }
}

/// `/cars/{car}/drives` filters.
#[derive(Debug, Clone, Default)]
pub struct DrivesQuery {
    pub period: Period,
    /// `places=p1,p2` and which end they are matched at: `start`, `end` or `any` (default).
    pub places: Option<String>,
    pub place_side: Option<String>,
    /// Text in a start or end label.
    pub q: Option<String>,
    pub min_distance_km: Option<f64>,
    pub min_avg_kph: Option<f64>,
    pub source: Option<String>,
    /// `gap`, `in_progress`, `reduced_range`: rows with any of them.
    pub flags: Option<String>,
    /// `plain` (default) or `hill_corrected`.
    pub efficiency: Option<String>,
    /// `start` (default), `distance`, `duration`, `energy`, `wh_per_km`, `speed_max`.
    pub sort: Option<String>,
    pub dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DrivesSummary {
    pub count: u64,
    pub distance_km: f64,
    pub duration_ms: u64,
    pub energy_kwh: f64,
    pub energy_measured_kwh: f64,
    pub energy_estimated_kwh: f64,
    pub wh_per_km: Option<f64>,
    pub without_energy: u64,
    pub efficiency: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DrivesPage {
    pub items: Vec<DriveView>,
    pub next: Option<String>,
    pub summary: DrivesSummary,
}

pub(crate) fn place_list(s: Option<&str>, places: &crate::placing::Places) -> QResult<Option<Vec<u32>>> {
    let Some(s) = s.filter(|s| !s.is_empty()) else { return Ok(None) };
    let mut out = Vec::new();
    for p in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let id = parse_place(p)
            .filter(|&id| places.get(id).is_some())
            .ok_or_else(|| QueryError::bad("places", "no such place"))?;
        out.push(id);
    }
    Ok(Some(out))
}

/// The drives of a car in a period (by start), as shown (overlaps with imported history settled).
pub(crate) fn rows(e: &Engine, car: CarId, period: &Period, source: Option<Source>) -> QResult<Vec<Drive>> {
    let (a, b) = period.bounds()?;
    let all: Vec<Drive> = all_rows(e.db(), car, a, b, source)?;
    let shown = Shown::new(e, car, source)?;
    Ok(all.into_iter().filter(|d| d.start_ms >= a && d.start_ms < b && shown.keep(d.source, d.start_ms)).collect())
}

/// `/cars/{car}/drives`.
pub fn drives(e: &Engine, car: CarId, q: &DrivesQuery, paging: &Paging) -> QResult<Scoped<DrivesPage>> {
    let c = car_of(e, car)?;
    let x = ctx(e, &c)?;
    let source = super::parse_source(q.source.as_deref())?;
    let side = q.place_side.as_deref().unwrap_or("any");
    if !["start", "end", "any"].contains(&side) {
        return Err(QueryError::bad("place_side", "start, end or any"));
    }
    let hill = match q.efficiency.as_deref() {
        None | Some("plain") => false,
        Some("hill_corrected") => true,
        Some(_) => return Err(QueryError::bad("efficiency", "plain or hill_corrected")),
    };
    let flags: Vec<&str> =
        q.flags.as_deref().map_or_else(Vec::new, |f| f.split(',').filter(|s| !s.is_empty()).collect());
    if let Some(bad) = flags.iter().find(|f| !["gap", "in_progress", "reduced_range"].contains(f)) {
        return Err(QueryError::bad("flags", &format!("{bad}: gap, in_progress or reduced_range")));
    }
    let want_places = place_list(q.places.as_deref(), &x.places)?;
    let needle = q.q.as_deref().map(str::to_lowercase).filter(|s| !s.is_empty());
    let mut items: Vec<DriveView> = Vec::new();
    for d in rows(e, car, &q.period, source)? {
        let mut v = view(&c, &d, &x);
        if hill {
            // the climb's potential energy taken off (a 2 t car, 85 % recovered on the way down is not assumed)
            let climb_kwh = d.ascent_m.map(|m| 2000.0 * 9.81 * m / 3.6e6);
            let w = d
                .energy_used_kwh
                .zip(d.distance_km)
                .filter(|(_, km)| *km > 0.1)
                .map(|(kwh, km)| (kwh - climb_kwh.unwrap_or(0.0)) * 1000.0 / km);
            v.wh_per_km = r(w.filter(|w| *w > 0.0), 1);
            v.efficiency_pct = r(x.rated_wh_per_km.zip(v.wh_per_km).map(|(rated, w)| rated / w * 100.0), 1);
        }
        if let Some(ps) = &want_places {
            let at = |w: &Where| w.place.as_ref().and_then(|p| parse_place(&p.id)).is_some_and(|id| ps.contains(&id));
            let ok = match side {
                "start" => at(&v.start),
                "end" => at(&v.end),
                _ => at(&v.start) || at(&v.end),
            };
            if !ok {
                continue;
            }
        }
        if let Some(n) = &needle {
            let hit = |w: &Where| w.label.as_deref().is_some_and(|l| l.to_lowercase().contains(n));
            if !hit(&v.start) && !hit(&v.end) {
                continue;
            }
        }
        if q.min_distance_km.is_some_and(|m| v.distance_km.is_none_or(|d| d < m)) {
            continue;
        }
        if q.min_avg_kph.is_some_and(|m| v.speed_avg_kph.is_none_or(|s| s < m)) {
            continue;
        }
        if !flags.is_empty() {
            let f = &v.flags;
            let any = flags.iter().any(|w| match *w {
                "gap" => f.gap_ms > 0,
                "in_progress" => f.in_progress,
                _ => f.reduced_range,
            });
            if !any {
                continue;
            }
        }
        items.push(v);
    }
    let summary = summarize(&items, hill);
    let sort = q.sort.as_deref().unwrap_or("start");
    let desc = super::parse_dir(q.dir.as_deref(), true)?;
    let key = |v: &DriveView| -> f64 {
        let n = |x: Option<f64>| x.unwrap_or(f64::NEG_INFINITY);
        match sort {
            "distance" => n(v.distance_km),
            "duration" => v.duration_ms as f64,
            "energy" => n(v.energy_kwh),
            "wh_per_km" => n(v.wh_per_km),
            "speed_max" => n(v.speed_max_kph),
            _ => v.start_ms as f64,
        }
    };
    if !["start", "distance", "duration", "energy", "wh_per_km", "speed_max"].contains(&sort) {
        return Err(QueryError::bad("sort", "start, distance, duration, energy, wh_per_km or speed_max"));
    }
    let sig = format!("drives|{car}|{}|{q:?}", q.period.sig());
    let page = paginate(items, |v| (key(v), v.id.clone()), desc, &sig, paging, false)?;
    Ok(scoped(&c, DrivesPage { items: page.items, next: page.next, summary }))
}

fn summarize(items: &[DriveView], hill: bool) -> DrivesSummary {
    let mut s = DrivesSummary {
        count: items.len() as u64,
        distance_km: 0.0,
        duration_ms: 0,
        energy_kwh: 0.0,
        energy_measured_kwh: 0.0,
        energy_estimated_kwh: 0.0,
        wh_per_km: None,
        without_energy: 0,
        efficiency: if hill { "hill_corrected" } else { "plain" },
    };
    let mut km_with_energy = 0.0;
    for v in items {
        s.distance_km += v.distance_km.unwrap_or(0.0);
        s.duration_ms += v.duration_ms;
        match v.energy_kwh {
            Some(kwh) => {
                s.energy_kwh += kwh;
                km_with_energy += v.distance_km.unwrap_or(0.0);
                if v.energy_basis == "measured" {
                    s.energy_measured_kwh += kwh;
                } else {
                    s.energy_estimated_kwh += kwh;
                }
            }
            None => s.without_energy += 1,
        }
    }
    s.wh_per_km = r((km_with_energy > 0.1).then(|| s.energy_kwh * 1000.0 / km_with_energy), 1);
    s.distance_km = r(Some(s.distance_km), 3).unwrap_or(0.0);
    s.energy_kwh = r(Some(s.energy_kwh), 3).unwrap_or(0.0);
    s.energy_measured_kwh = r(Some(s.energy_measured_kwh), 3).unwrap_or(0.0);
    s.energy_estimated_kwh = r(Some(s.energy_estimated_kwh), 3).unwrap_or(0.0);
    s
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SpeedBand {
    pub from_kph: f64,
    pub to_kph: f64,
    pub seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Gap {
    pub start_ms: u64,
    pub end_ms: u64,
    pub kind: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DriveDetail {
    #[serde(flatten)]
    pub drive: DriveView,
    pub prev: Option<String>,
    pub next: Option<String>,
    pub prev_charge: Option<String>,
    pub next_charge: Option<String>,
    pub speed_band_kph: f64,
    pub speed_bands: Vec<SpeedBand>,
    pub gaps: Vec<Gap>,
    pub rated_wh_per_km: Option<f64>,
    pub start_ideal_range_km: Option<f64>,
    pub end_ideal_range_km: Option<f64>,
    pub est_wh_per_km: Option<f64>,
    pub est_basis: &'static str,
}

/// The samples of a drive (its own source's), in time order.
pub(crate) fn samples(e: &Engine, car: CarId, d: &Drive) -> QResult<Vec<Sample>> {
    Ok(e.db().read_points::<Sample>(car, d.source, d.start_ms, d.end_ms.max(d.start_ms) + 1)?)
}

/// `/cars/{car}/drives/{drive}`; `speed_band_kph` is 1 to 100 (default 10).
pub fn drive(e: &Engine, car: CarId, id: u64, speed_band_kph: Option<f64>) -> QResult<DriveDetail> {
    let c = car_of(e, car)?;
    let band = speed_band_kph.unwrap_or(10.0);
    if !(1.0..=100.0).contains(&band) {
        return Err(QueryError::bad("speed_band_kph", "1 to 100"));
    }
    let d: Drive = e.db().get(car, id)?.ok_or(QueryError::NotFound("no_such_drive"))?;
    let x = ctx(e, &c)?;
    let all = rows(e, car, &Period::all(), None)?;
    let i = all.iter().position(|o| o.id == d.id);
    let prev = i.and_then(|i| i.checked_sub(1)).and_then(|j| all.get(j)).map(|o| o.id.to_string());
    let next = i.and_then(|i| all.get(i + 1)).map(|o| o.id.to_string());
    let charges: Vec<model::Charge> = super::charges::rows(e, car, &Period::all(), None)?;
    let prev_charge = charges.iter().rev().find(|ch| ch.start_ms < d.start_ms).map(|ch| ch.id.to_string());
    let next_charge = charges.iter().find(|ch| ch.start_ms > d.start_ms).map(|ch| ch.id.to_string());
    let s = samples(e, car, &d)?;
    let mut bands: std::collections::BTreeMap<u64, u64> = Default::default();
    let mut gaps = Vec::new();
    for w in s.windows(2) {
        let dt = w[1].t_ms.saturating_sub(w[0].t_ms);
        if dt > 10_000 {
            gaps.push(Gap { start_ms: w[0].t_ms + 1000, end_ms: w[1].t_ms, kind: "unknown" });
            continue;
        }
        if let Some(v) = w[0].speed_kmh.filter(|v| *v > 0.5) {
            *bands.entry((v / band).floor() as u64).or_default() += dt / 1000;
        }
    }
    let speed_bands = bands
        .into_iter()
        .map(|(k, seconds)| SpeedBand {
            from_kph: r(Some(k as f64 * band), 3).unwrap_or(0.0),
            to_kph: r(Some((k + 1) as f64 * band), 3).unwrap_or(0.0),
            seconds,
        })
        .collect();
    // the consumption of the 30 days of drives before this one
    let month: Vec<&Drive> =
        all.iter().filter(|o| o.start_ms < d.start_ms && o.start_ms + 30 * 86_400_000 >= d.start_ms).collect();
    let (kwh, km) = month.iter().fold((0.0, 0.0), |(k, m), o| match (o.energy_used_kwh, o.distance_km) {
        (Some(a), Some(b)) => (k + a, m + b),
        _ => (k, m),
    });
    let mut view = view(&c, &d, &x);
    view.flags.gap_ms = gaps.iter().map(|g| g.end_ms - g.start_ms).sum();
    Ok(DriveDetail {
        drive: view,
        prev,
        next,
        prev_charge,
        next_charge,
        speed_band_kph: band,
        speed_bands,
        gaps,
        rated_wh_per_km: c.efficiency_wh_km,
        start_ideal_range_km: r(s.iter().find_map(|x| x.ideal_range_km), 1),
        end_ideal_range_km: r(s.iter().rev().find_map(|x| x.ideal_range_km), 1),
        est_wh_per_km: r((km > 1.0).then(|| kwh * 1000.0 / km), 1),
        est_basis: "wh_per_km_30d",
    })
}

/// One point of a drive's track.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrackPoint {
    pub t_ms: u64,
    pub lat: f64,
    pub lon: f64,
    pub elevation_m: Option<f64>,
    pub accuracy_m: Option<f64>,
    pub speed_kph: Option<f64>,
    pub power_kw: Option<f64>,
    pub soc_pct: Option<f64>,
}

/// A drive's track: one point a second with a fix, cut into stretches where data is missing for over 10 s.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Track {
    pub car: CarRef,
    pub drive: String,
    pub name: String,
    pub start_ms: u64,
    pub time_zone: String,
    pub stretches: Vec<Vec<TrackPoint>>,
}

pub fn track(e: &Engine, car: CarId, id: u64) -> QResult<Track> {
    let c = car_of(e, car)?;
    let d: Drive = e.db().get(car, id)?.ok_or(QueryError::NotFound("no_such_drive"))?;
    let mut stretches: Vec<Vec<TrackPoint>> = Vec::new();
    let mut last_t: Option<u64> = None;
    for s in samples(e, car, &d)? {
        let (Some(lat), Some(lon)) = (s.lat, s.lon) else { continue };
        let p = TrackPoint {
            t_ms: s.t_ms,
            lat,
            lon,
            elevation_m: s.elevation_m,
            accuracy_m: s.gps_accuracy_m,
            speed_kph: s.speed_kmh,
            power_kw: s.battery_power_kw,
            soc_pct: s.soc,
        };
        if last_t.is_none_or(|t| s.t_ms.saturating_sub(t) > 10_000) || stretches.is_empty() {
            stretches.push(Vec::new());
        }
        if let Some(st) = stretches.last_mut() {
            st.push(p);
        }
        last_t = Some(s.t_ms);
    }
    Ok(Track {
        car: CarRef(car),
        drive: d.id.to_string(),
        name: c.name.clone(),
        start_ms: d.start_ms,
        time_zone: c.time_zone.clone(),
        stretches,
    })
}

/// Douglas-Peucker thinning to `tolerance_m`, keeping the ends.
pub fn simplify(points: &[TrackPoint], tolerance_m: f64) -> Vec<TrackPoint> {
    if points.len() < 3 || tolerance_m <= 0.0 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0usize, points.len() - 1)];
    // metres per degree near the track (a flat projection is fine at this scale)
    let lat0 = points[0].lat.to_radians();
    let (ky, kx) = (111_320.0, 111_320.0 * lat0.cos());
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (ax, ay) = (points[a].lon * kx, points[a].lat * ky);
        let (bx, by) = (points[b].lon * kx, points[b].lat * ky);
        let (dx, dy) = (bx - ax, by - ay);
        let len = (dx * dx + dy * dy).sqrt();
        let mut best = (0.0, a);
        for (i, p) in points.iter().enumerate().take(b).skip(a + 1) {
            let (px, py) = (p.lon * kx - ax, p.lat * ky - ay);
            let dist = if len > 0.0 { (px * dy - py * dx).abs() / len } else { (px * px + py * py).sqrt() };
            if dist > best.0 {
                best = (dist, i);
            }
        }
        if best.0 > tolerance_m {
            keep[best.1] = true;
            stack.push((a, best.1));
            stack.push((best.1, b));
        }
    }
    points.iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| p.clone()).collect()
}

/// The track as GeoJSON: one LineString per stretch, coordinates `[lon, lat, elevation_m]`, with parallel `t_ms` and
/// `accuracy_m` (and `speed_kph`, `power_kw`, `soc_pct` when asked in `extensions`).
pub fn geojson(t: &Track, simplify_m: Option<f64>, extensions: &[&str]) -> serde_json::Value {
    let features: Vec<serde_json::Value> = t
        .stretches
        .iter()
        .map(|st| {
            let pts = simplify_m.map_or_else(|| st.clone(), |m| simplify(st, m));
            let coords: Vec<serde_json::Value> = pts
                .iter()
                .map(|p| match p.elevation_m {
                    Some(el) => serde_json::json!([p.lon, p.lat, el]),
                    None => serde_json::json!([p.lon, p.lat]),
                })
                .collect();
            let mut props = serde_json::Map::new();
            props.insert("t_ms".into(), pts.iter().map(|p| p.t_ms).collect::<Vec<_>>().into());
            props.insert("accuracy_m".into(), serde_json::json!(pts.iter().map(|p| p.accuracy_m).collect::<Vec<_>>()));
            for ext in extensions {
                let col: Vec<Option<f64>> = match *ext {
                    "speed" => pts.iter().map(|p| p.speed_kph).collect(),
                    "power" => pts.iter().map(|p| p.power_kw).collect(),
                    "soc" => pts.iter().map(|p| p.soc_pct).collect(),
                    _ => continue,
                };
                let name = match *ext {
                    "speed" => "speed_kph",
                    "power" => "power_kw",
                    _ => "soc_pct",
                };
                props.insert(name.into(), serde_json::json!(col));
            }
            serde_json::json!({
                "type": "Feature",
                "geometry": { "type": "LineString", "coordinates": coords },
                "properties": props,
            })
        })
        .collect();
    serde_json::json!({ "type": "FeatureCollection", "features": features })
}

fn iso(ms: u64) -> String {
    let t = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(i64::try_from(ms).unwrap_or(0)).unwrap_or_default();
    t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The track as GPX 1.1: one `trkseg` per stretch, `ele` and `time` on every point.
pub fn gpx(t: &Track, simplify_m: Option<f64>, extensions: &[&str]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gpx version=\"1.1\" creator=\"Sentry Mate\" \
         xmlns=\"http://www.topografix.com/GPX/1/1\" xmlns:sm=\"https://sentry-mate.invalid/gpx/1\">\n",
    );
    out.push_str(&format!("<trk><name>{} drive {}</name>\n", xml(&t.name), iso(t.start_ms)));
    for st in &t.stretches {
        out.push_str("<trkseg>\n");
        let pts = simplify_m.map_or_else(|| st.clone(), |m| simplify(st, m));
        for p in pts {
            out.push_str(&format!("<trkpt lat=\"{:.6}\" lon=\"{:.6}\">", p.lat, p.lon));
            if let Some(el) = p.elevation_m {
                out.push_str(&format!("<ele>{el:.1}</ele>"));
            }
            out.push_str(&format!("<time>{}</time>", iso(p.t_ms)));
            let mut ext = String::new();
            for x in extensions {
                let (tag, v) = match *x {
                    "speed" => ("speed_kph", p.speed_kph),
                    "power" => ("power_kw", p.power_kw),
                    "soc" => ("soc_pct", p.soc_pct),
                    "accuracy" => ("accuracy_m", p.accuracy_m),
                    _ => continue,
                };
                if let Some(v) = v {
                    ext.push_str(&format!("<sm:{tag}>{v}</sm:{tag}>"));
                }
            }
            if !ext.is_empty() {
                out.push_str(&format!("<extensions>{ext}</extensions>"));
            }
            out.push_str("</trkpt>\n");
        }
        out.push_str("</trkseg>\n");
    }
    out.push_str("</trk>\n</gpx>\n");
    out
}

/// `<car>-drive-<YYYY-MM-DD-HHMM>.<ext>` in the car's zone (anything but letters, digits, `.`, `_`, `-` → `-`).
pub fn track_file_name(t: &Track, ext: &str) -> String {
    let tz = crate::tz::zone(&t.time_zone);
    let local = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(i64::try_from(t.start_ms).unwrap_or(0))
        .unwrap_or_default()
        .with_timezone(&tz);
    let name: String =
        t.name.chars().map(|ch| if ch.is_ascii_alphanumeric() || ".-_".contains(ch) { ch } else { '-' }).collect();
    format!("{name}-drive-{}.{ext}", local.format("%Y-%m-%d-%H%M"))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DayCount {
    pub date: String,
    pub count: u64,
    pub distance_km: f64,
    pub energy_kwh: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MaxSpeed {
    pub kph: Option<f64>,
    pub drive: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DriveStats {
    pub count: u64,
    pub distance_km: f64,
    pub energy_kwh: f64,
    pub median_distance_km: Option<f64>,
    pub per_day: Vec<DayCount>,
    pub days: u64,
    pub max_speed: serde_json::Value,
    pub projection: serde_json::Value,
    pub top_destinations: Vec<serde_json::Value>,
}

/// `/cars/{car}/drive-stats`: the period's drives (leaving out those that start or end at `exclude_places`).
pub fn drive_stats(
    e: &Engine,
    car: CarId,
    period: &Period,
    exclude_places: Option<&str>,
    now_ms: u64,
) -> QResult<Scoped<DriveStats>> {
    let c = car_of(e, car)?;
    let x = ctx(e, &c)?;
    let tz = crate::tz::zone(&c.time_zone);
    let exclude = place_list(exclude_places, &x.places)?.unwrap_or_default();
    let all = rows(e, car, &Period::all(), None)?;
    let (a, b) = period.bounds()?;
    let end = b.min(now_ms);
    let views: Vec<DriveView> = all
        .iter()
        .filter(|d| d.start_ms >= a && d.start_ms < b)
        .map(|d| view(&c, d, &x))
        .filter(|v| {
            let at =
                |w: &Where| w.place.as_ref().and_then(|p| parse_place(&p.id)).is_some_and(|id| exclude.contains(&id));
            !at(&v.start) && !at(&v.end)
        })
        .collect();
    let mut dist: Vec<f64> = views.iter().filter_map(|v| v.distance_km).collect();
    dist.sort_by(f64::total_cmp);
    let median = dist.get(dist.len() / 2).copied().filter(|_| !dist.is_empty());
    let first = all.first().map_or(a, |d| d.start_ms).max(a);
    let mut per_day = Vec::new();
    let mut day = crate::tz::date(first, tz);
    let last = crate::tz::date(end.max(first), tz);
    while day <= last && per_day.len() < 20_000 {
        let label = day.format("%Y-%m-%d").to_string();
        let on: Vec<&DriveView> = views.iter().filter(|v| crate::tz::date_str(v.start_ms, tz) == label).collect();
        per_day.push(DayCount {
            date: label,
            count: on.len() as u64,
            distance_km: r(Some(on.iter().filter_map(|v| v.distance_km).sum()), 3).unwrap_or(0.0),
            energy_kwh: r(Some(on.iter().filter_map(|v| v.energy_kwh).sum()), 3).unwrap_or(0.0),
        });
        let Some(n) = day.succ_opt() else { break };
        day = n;
    }
    let fastest = |from: u64| -> MaxSpeed {
        let best = all
            .iter()
            .filter(|d| d.start_ms >= from && d.start_ms < end)
            .filter_map(|d| d.speed_max_kmh.map(|s| (s, d.id)))
            .max_by(|p, q| p.0.total_cmp(&q.0));
        MaxSpeed { kph: r(best.map(|b| b.0), 1), drive: best.map(|b| b.1.to_string()) }
    };
    let period_best =
        views.iter().filter_map(|v| v.speed_max_kph.map(|s| (s, v.id.clone()))).max_by(|p, q| p.0.total_cmp(&q.0));
    let total_km: f64 = views.iter().filter_map(|v| v.distance_km).sum();
    let days = per_day.len() as u64;
    let mut dest: std::collections::BTreeMap<String, (String, u64)> = Default::default();
    for v in views.iter().filter(|v| !v.flags.in_progress) {
        if let Some(p) = &v.end.place {
            dest.entry(p.id.clone()).or_insert((p.name.clone(), 0)).1 += 1;
        }
    }
    let mut top: Vec<(String, (String, u64))> = dest.into_iter().collect();
    top.sort_by_key(|p| std::cmp::Reverse(p.1.1));
    Ok(scoped(
        &c,
        DriveStats {
            count: views.len() as u64,
            distance_km: r(Some(total_km), 3).unwrap_or(0.0),
            energy_kwh: r(Some(views.iter().filter_map(|v| v.energy_kwh).sum()), 3).unwrap_or(0.0),
            median_distance_km: r(median, 3),
            per_day,
            days,
            max_speed: serde_json::json!({
                "period": { "kph": period_best.as_ref().map(|b| b.0), "drive": period_best.map(|b| b.1) },
                "last_30d": fastest(end.saturating_sub(30 * 86_400_000)),
                "last_7d": fastest(end.saturating_sub(7 * 86_400_000)),
            }),
            projection: serde_json::json!({
                "per_month_km": r((days > 0).then(|| total_km / days as f64 * 30.436_875), 1),
                "per_year_km": r((days > 0).then(|| total_km / days as f64 * 365.25), 1),
                "basis_days": days,
            }),
            top_destinations: top
                .into_iter()
                .take(10)
                .map(|(id, (label, count))| serde_json::json!({ "place": id, "label": label, "count": count }))
                .collect(),
        },
    ))
}
