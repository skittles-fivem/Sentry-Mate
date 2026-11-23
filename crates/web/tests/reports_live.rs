#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Reports of cars whose rows are laid down by hand (made-up cars, made-up places in the open sea), through the real
//! router: a live car whose newest state is a short unknown span (the gaps list still pages through every older gap),
//! a car whose data stopped weeks ago (no odometer distance after that, the projection over the recorded days, a stop
//! going on with its time since counted unknown), a stop over midnight with a charge (on its start day only), the
//! unknown city key as a filter, and the visit groups paged by name while a group is added.

use engine::Engine;
use model::{CarId, Charge, Drive, Position, Source, State, StateSpan};
use serde_json::{Value, json};
use std::sync::Arc;
use web::testkit::{Client, TestServer};

const MIN: u64 = 60_000;
const HOUR: u64 = 60 * MIN;
const DAY: u64 = 24 * HOUR;
const LAT: f64 = 47.0;
const LON: f64 = -31.0;

struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

fn car(name: &str) -> model::Car {
    model::Car {
        id: 0,
        name: name.into(),
        vehicle_keys: Vec::new(),
        preferred_key: None,
        vin: None,
        profile_setting: None,
        time_zone: "UTC".into(),
        time_zone_typed: Some(true),
        efficiency_wh_km: None,
        created_ms: 0,
        inputs: Default::default(),
    }
}

fn state(car: CarId, state: State, start_ms: u64, end_ms: u64) -> db::StateRow {
    db::StateRow {
        id: start_ms,
        span: StateSpan {
            car,
            state,
            start_ms,
            end_ms,
            buses: vec!["can0".into()],
            source: Source::Live,
            inputs: model::Inputs::NONE,
        },
    }
}

fn at(t_ms: u64, k: f64) -> Position {
    Position { t_ms, lat: LAT + k * 0.01, lon: LON + k * 0.01, ..Position::default() }
}

/// A drive from spot `from` to spot `to` (made-up spots 1 km or so apart), `km` long, at odometer `odo`.
fn drive(car: CarId, start_ms: u64, mins: u64, from: f64, to: f64, odo: f64, km: f64) -> Drive {
    let end_ms = start_ms + mins * MIN;
    Drive {
        id: start_ms,
        car,
        start_ms,
        end_ms,
        start: Some(at(start_ms, from)),
        end: Some(at(end_ms, to)),
        start_place: None,
        end_place: None,
        start_address: None,
        end_address: None,
        start_odometer_km: Some(odo),
        end_odometer_km: Some(odo + km),
        distance_km: Some(km),
        speed_max_kmh: Some(90.0),
        power_max_kw: Some(80.0),
        power_min_kw: Some(-30.0),
        energy_used_kwh: Some(km * 0.15),
        start_soc: Some(80.0),
        end_soc: Some(75.0),
        start_range_km: None,
        end_range_km: None,
        outside_temp_avg_c: Some(12.0),
        inside_temp_avg_c: Some(21.0),
        ascent_m: None,
        descent_m: None,
        source: Source::Live,
        inputs: model::Inputs::NONE,
        rules_version: 1,
    }
}

fn charge(car: CarId, start_ms: u64, end_ms: u64, k: f64) -> Charge {
    Charge {
        id: start_ms,
        car,
        start_ms,
        end_ms,
        position: Some(at(start_ms, k)),
        place: None,
        address: None,
        kind: Some(model::ChargeKind::Ac),
        energy_added_kwh: Some(20.0),
        energy_used_kwh: Some(22.0),
        energy_spans: None,
        start_soc: Some(40.0),
        end_soc: Some(70.0),
        start_range_km: None,
        end_range_km: None,
        power_max_kw: Some(11.0),
        outside_temp_avg_c: None,
        battery_temp_avg_c: None,
        cost: None,
        source: Source::Live,
        inputs: model::Inputs::NONE,
        rules_version: 1,
    }
}

struct Laid {
    s: TestServer,
    engine: Engine,
    db: db::Db,
    live: CarId,
    stopped: CarId,
    /// The stopped car's first day (UTC midnight) and when its data stopped.
    day0: u64,
    stop_ms: u64,
    /// The live car's three old gaps, newest first, and its still-unknown newest state's start.
    gaps: Vec<u64>,
    still_ms: u64,
    /// The stop over midnight: its charge, and the charge's start.
    overnight_charge: u64,
    _dir: tempfile::TempDir,
}

async fn laid() -> Laid {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(db.clone(), store, engine::Options { workers: 1, ..engine::Options::default() });
    engine.start().unwrap();
    let now = now_ms();

    // the live car: online, three old gaps of 30, 20 and 15 minutes, online up to three minutes ago, then a short
    // still-unknown newest state (the time after its last log, before the next upload)
    let live = db.create_car(car("Live")).unwrap().id;
    let t0 = now - 10 * HOUR;
    let gaps = vec![t0 + 6 * HOUR, t0 + 4 * HOUR, t0 + 2 * HOUR];
    let still_ms = now - 3 * MIN;
    let lens = [15 * MIN, 20 * MIN, 30 * MIN];
    let mut rows = Vec::new();
    let mut x = t0;
    for (g, len) in gaps.iter().rev().zip(lens.iter().rev()) {
        rows.push(state(live, State::Online, x, *g));
        rows.push(state(live, State::Unknown, *g, g + len));
        x = g + len;
    }
    rows.push(state(live, State::Online, x, still_ms));
    rows.push(state(live, State::Unknown, still_ms, still_ms + 1));
    db.upsert(live, &rows).unwrap();
    db.upsert(live, &[drive(live, t0 + 30 * MIN, 20, 0.0, 1.0, 5_000.0, 12.0)]).unwrap();

    // the stopped car: eight days of data from 40 days ago, a drive a day, then nothing since
    let stopped = db.create_car(car("Stopped")).unwrap().id;
    let day0 = (now - 40 * DAY) / DAY * DAY;
    let stop_ms = day0 + 8 * DAY;
    db.upsert(
        stopped,
        &[state(stopped, State::Online, day0, stop_ms), state(stopped, State::Unknown, stop_ms, stop_ms + 1)],
    )
    .unwrap();
    let mut drives = Vec::new();
    let mut odo = 10_000.0;
    for d in 0..8u64 {
        // day 1 has no drive: the stop from day 0's evening to day 2's morning spans two midnights
        if d == 1 {
            continue;
        }
        let start = day0 + d * DAY + if d == 2 { 10 * HOUR } else { 18 * HOUR };
        let spot = (d % 4) as f64;
        drives.push(drive(stopped, start, 30, spot, (d % 4 + 1) as f64, odo, 40.0));
        odo += 40.0;
    }
    db.upsert(stopped, &drives).unwrap();
    // a charge in the overnight stop (day 0, 23:30 to day 1, 03:00), and one in the last, still going stop
    let overnight_charge = day0 + 23 * HOUR + 30 * MIN;
    db.upsert(
        stopped,
        &[
            charge(stopped, overnight_charge, day0 + DAY + 3 * HOUR, 1.0),
            charge(stopped, day0 + 7 * DAY + 20 * HOUR, day0 + 7 * DAY + 22 * HOUR, 4.0),
        ],
    )
    .unwrap();

    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| {
        b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2)).requests_per_minute(1_000_000)
    })
    .await;
    Laid { s, engine, db, live, stopped, day0, stop_ms, gaps, still_ms, overnight_charge, _dir: dir }
}

async fn ok(c: &mut Client, path: &str) -> Value {
    let r = c.get(path).await;
    assert_eq!(r.status, 200, "{path}: {}", r.text());
    r.json()
}

/// Every row of a list, page by page: no row twice, each page at most `limit`.
async fn walk(c: &mut Client, path: &str, limit: usize, key: impl Fn(&Value) -> String) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..500 {
        let sep = if path.contains('?') { '&' } else { '?' };
        let url = match &after {
            Some(a) => format!("{path}{sep}limit={limit}&after={a}"),
            None => format!("{path}{sep}limit={limit}"),
        };
        let v = ok(c, &url).await;
        let items = v["items"].as_array().unwrap();
        assert!(items.len() <= limit, "{url}");
        for it in items {
            assert!(!out.iter().any(|o| key(o) == key(it)), "{url}: {it} twice");
        }
        out.extend(items.iter().cloned());
        match v["next"].as_str() {
            Some(n) => after = Some(n.to_string()),
            None => return out,
        }
    }
    panic!("{path}: no end of pages");
}

/// The UTC date of `ms`, `YYYY-MM-DD` (the civil calendar from days since 1970).
fn date_of(ms: u64) -> String {
    let z = (ms / DAY) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn starts(rows: &[Value]) -> Vec<u64> {
    rows.iter().map(|r| r["start_ms"].as_u64().unwrap()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_cars_short_newest_unknown_takes_no_place_on_the_gaps_page() {
    let k = laid().await;
    let mut admin = k.s.admin().await;
    let car = format!("c{}", k.live);
    // one at a time: every old gap, newest first, and the short still-unknown span is no gap
    let p1 = ok(&mut admin, &format!("/api/v1/cars/{car}/gaps?limit=1")).await;
    assert_eq!(starts(p1["items"].as_array().unwrap()), vec![k.gaps[0]], "{p1}");
    assert!(p1["next"].is_string(), "older gaps are there: {p1}");
    let all = walk(&mut admin, &format!("/api/v1/cars/{car}/gaps"), 1, |r| r["id"].to_string()).await;
    assert_eq!(starts(&all), k.gaps, "{all:?}");
    assert!(all.iter().all(|g| g["end_ms"].is_u64()), "none of them is still going");
    let two = walk(&mut admin, &format!("/api/v1/cars/{car}/gaps"), 2, |r| r["id"].to_string()).await;
    assert_eq!(starts(&two), k.gaps);
    // the same with the period's end inside the short unknown span
    let to = k.still_ms + 1;
    let cut = walk(&mut admin, &format!("/api/v1/cars/{car}/gaps?to_ms={to}"), 1, |r| r["id"].to_string()).await;
    assert_eq!(starts(&cut), k.gaps);
    // and the CSV holds them all
    let csv = admin.get(&format!("/api/v1/cars/{car}/gaps?format=csv")).await;
    assert_eq!(csv.status, 200, "{}", csv.text());
    assert_eq!(csv.text().trim_end().lines().count(), 1 + k.gaps.len(), "{}", csv.text());

    // the stopped car's still-unknown time is one gap still going, its length up to now
    let st = format!("c{}", k.stopped);
    let g = ok(&mut admin, &format!("/api/v1/cars/{st}/gaps")).await;
    let items = g["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{g}");
    assert_eq!((items[0]["start_ms"].as_u64(), items[0]["end_ms"].clone()), (Some(k.stop_ms), Value::Null));
    assert!(items[0]["duration_ms"].as_u64().unwrap() >= now_ms() - k.stop_ms - 60_000);

    // a watch account that does not see the car gets the unknown car's answer
    let mut watch = k.s.watch(&mut admin, &[k.stopped]).await;
    let hidden = watch.get(&format!("/api/v1/cars/{car}/gaps")).await;
    let unknown = watch.get("/api/v1/cars/c999/gaps").await;
    assert_eq!((hidden.status, hidden.code()), (unknown.status, unknown.code()));
    assert_eq!(hidden.status, 404);
    let seen = ok(&mut watch, &format!("/api/v1/cars/{st}/gaps")).await;
    assert_eq!(starts(seen["items"].as_array().unwrap()), vec![k.stop_ms], "{seen}");
    drop(k.engine);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_odometer_distance_once_the_data_stopped_and_the_projection_over_the_recorded_days() {
    let k = laid().await;
    let mut admin = k.s.admin().await;
    let car = format!("c{}", k.stopped);
    let from = k.day0;
    let s = ok(&mut admin, &format!("/api/v1/cars/{car}/statistics?period=day&limit=500&from_ms={from}")).await;
    let days = s["items"].as_array().unwrap();
    assert!(days.len() >= 39, "{}", days.len());
    for d in days {
        let start = d["period_start_ms"].as_u64().unwrap();
        if start >= k.stop_ms {
            assert!(d["distance_odometer_km"].is_null(), "no data on this day: {d}");
            assert_eq!(d["completeness"]["recorded_pct"].as_f64(), Some(0.0), "{d}");
        } else {
            assert!(d["distance_odometer_km"].is_f64(), "{d}");
        }
    }
    let total: f64 = days.iter().filter_map(|d| d["distance_odometer_km"].as_f64()).sum();
    assert!((total - 7.0 * 40.0).abs() < 0.01, "{total}");
    // weeks: the ones after the data stopped are null too, the one it stopped in counts what came
    let w = ok(&mut admin, &format!("/api/v1/cars/{car}/statistics?period=week&limit=500&from_ms={from}")).await;
    let newest = &w["items"][0];
    assert!(newest["distance_odometer_km"].is_null(), "{newest}");
    let wsum: f64 = w["items"].as_array().unwrap().iter().filter_map(|d| d["distance_odometer_km"].as_f64()).sum();
    assert!((wsum - 280.0).abs() < 0.01, "{wsum}");

    let m = ok(&mut admin, &format!("/api/v1/cars/{car}/mileage?group=day&limit=500&from_ms={from}")).await;
    for it in m["items"].as_array().unwrap() {
        if it["period_start_ms"].as_u64().unwrap() >= k.stop_ms {
            for f in ["distance_km", "not_logged_km", "odometer_start_km", "odometer_end_km"] {
                assert!(it[f].is_null(), "{f}: {it}");
            }
        } else {
            assert!(it["distance_km"].is_f64(), "{it}");
        }
    }
    // the projection: over the eight recorded days, not the forty since
    let p = &m["projection"];
    assert!((p["basis_days"].as_f64().unwrap() - 8.0).abs() < 0.11, "{p}");
    let want = 280.0 / 8.0 * 365.25;
    assert!((p["km_per_year"].as_f64().unwrap() - want).abs() < 1.0, "{p}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stops_count_the_time_since_the_data_stopped_and_charges_land_on_their_start_day() {
    let k = laid().await;
    let mut admin = k.s.admin().await;
    let car = format!("c{}", k.stopped);
    let from = k.day0;
    // the stop going on: arrived on day 7, the data stopped on day 8, unknown ever since
    let v = ok(&mut admin, &format!("/api/v1/visits?cars={car}&from_ms={from}&limit=1")).await;
    let open = &v["items"][0];
    assert!(open["left_ms"].is_null(), "{open}");
    let unknown = open["unknown_ms"].as_u64().unwrap();
    let since = now_ms() - k.stop_ms;
    assert!(unknown + 60_000 >= since && unknown <= since + 60_000, "{unknown} vs {since}");

    // the stop over two midnights: its charge (started day 0, 23:30) is on day 0 only
    let days =
        walk(&mut admin, &format!("/api/v1/visits/days?cars={car}&from_ms={from}"), 500, |r| r["date"].to_string())
            .await;
    let id = k.overnight_charge.to_string();
    let with: Vec<&str> = days
        .iter()
        .filter(|d| d["charges"].as_array().unwrap().iter().any(|c| c.as_str() == Some(id.as_str())))
        .map(|d| d["date"].as_str().unwrap())
        .collect();
    let day0 = date_of(k.day0);
    assert_eq!(with, vec![day0.as_str()], "{days:?}");
    // the days of the overnight stop are there, the later ones without its charge
    let day1 = date_of(k.day0 + DAY);
    let d1 = days.iter().find(|d| d["date"] == json!(day1)).unwrap();
    assert_eq!(d1["charges"], json!([]), "{d1}");

    // the unknown city's key filters as it is handed out
    let cities = ok(&mut admin, &format!("/api/v1/visits/cities?cars={car}&from_ms={from}")).await;
    let key = cities["items"][0]["key"].as_str().unwrap().to_string();
    assert_eq!(key, "-/-/-");
    let all =
        walk(&mut admin, &format!("/api/v1/visits?cars={car}&from_ms={from}"), 100, |r| r["id"].to_string()).await;
    let by_city =
        walk(&mut admin, &format!("/api/v1/visits?cars={car}&from_ms={from}&city={key}"), 100, |r| r["id"].to_string())
            .await;
    assert!(!all.is_empty());
    assert_eq!(by_city.len(), all.len());
    let city_days = walk(&mut admin, &format!("/api/v1/visits/days?cars={car}&from_ms={from}&city={key}"), 500, |r| {
        r["date"].to_string()
    })
    .await;
    assert_eq!(city_days.len(), days.len());
    let by_country =
        walk(&mut admin, &format!("/api/v1/visits?cars={car}&from_ms={from}&country=-"), 100, |r| r["id"].to_string())
            .await;
    assert_eq!(by_country.len(), all.len());
    let none = ok(&mut admin, &format!("/api/v1/visits?cars={car}&from_ms={from}&city=US/California/Nowhere")).await;
    assert_eq!(none["items"], json!([]));
    // by dwell, page by page: every stop once
    let dw = walk(&mut admin, &format!("/api/v1/visits?cars={car}&from_ms={from}&sort=dwell_desc"), 1, |r| {
        r["id"].to_string()
    })
    .await;
    assert_eq!(dw.len(), all.len());
    for sort in ["visits", "dwell", "last"] {
        let full = ok(&mut admin, &format!("/api/v1/visits/places?cars={car}&from_ms={from}&sort={sort}")).await;
        let paged = walk(&mut admin, &format!("/api/v1/visits/places?cars={car}&from_ms={from}&sort={sort}"), 1, |r| {
            r["key"].to_string()
        })
        .await;
        assert_eq!(paged.len(), full["items"].as_array().unwrap().len(), "{sort}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn groups_by_name_page_on_by_name_when_a_group_is_added_between_pages() {
    let k = laid().await;
    let mut admin = k.s.admin().await;
    let car = format!("c{}", k.stopped);
    let from = k.day0;
    // made-up places at three of the four spots the car stops at
    for (name, spot) in [("Bravo", 1.0), ("Charlie", 2.0), ("Delta", 3.0)] {
        k.db.add_place(model::Place {
            id: 0,
            name: name.into(),
            kind: model::PlaceKind::Other,
            lat: LAT + spot * 0.01,
            lon: LON + spot * 0.01,
            radius_m: 100.0,
            price: None,
        })
        .unwrap();
    }
    k.engine.places_changed();
    let path = format!("/api/v1/visits/places?cars={car}&from_ms={from}&sort=name");
    let p1 = ok(&mut admin, &format!("{path}&limit=2")).await;
    let first: Vec<Value> = p1["items"].as_array().unwrap().clone();
    let labels: Vec<Value> = first.iter().map(|r| r["label"].clone()).collect();
    // the unnamed spot first, then the names
    assert_eq!(labels, vec![Value::Null, json!("Bravo")], "{p1}");
    // a place named to sort first is saved at the unnamed spot: the next page goes on after Bravo
    k.db.add_place(model::Place {
        id: 0,
        name: "Alpha".into(),
        kind: model::PlaceKind::Other,
        lat: LAT,
        lon: LON,
        radius_m: 100.0,
        price: None,
    })
    .unwrap();
    k.engine.places_changed();
    let cur = p1["next"].as_str().unwrap();
    let p2 = ok(&mut admin, &format!("{path}&limit=2&after={cur}")).await;
    let labels: Vec<Value> = p2["items"].as_array().unwrap().iter().map(|r| r["label"].clone()).collect();
    assert_eq!(labels, vec![json!("Charlie"), json!("Delta")], "{p2}");
    assert!(p2["next"].is_null(), "{p2}");
}
