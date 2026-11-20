#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Reports through the real router (web::testkit): statistics, mileage, totals, gaps, compare, visits (with the
//! summary, the groups and the days), tracks, software updates (and naming one's version), the fleet's versions and a
//! car's events, over an engine that decoded two made-up cars' days of logs (synth: made-up VINs, a made-up town in
//! the open sea, public signing seeds), with a made-up home place and two made-up imported updates.

use engine::Engine;
use model::CarId;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use web::testkit::{Client, TestServer};

struct Seeded {
    s: TestServer,
    _engine: Engine,
    cars: Vec<String>,
    ids: Vec<CarId>,
    home: String,
    /// The imported updates of the first car, oldest first.
    updates: Vec<u64>,
    start_ms: u64,
    _dir: tempfile::TempDir,
}

struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

async fn put(store: &ingest::Store, judge: &ingest::Judge, name: &str, bytes: &[u8]) {
    let sha = hex::encode(Sha256::digest(bytes));
    let r = store.receive(name, bytes.to_vec().into(), 0, bytes.len() as u64, &sha).await.unwrap();
    assert!(r.complete, "{name}");
    judge.arrived(name).await.unwrap();
}

async fn seeded() -> Seeded {
    const START: u64 = synth::DEFAULT_START_MS + 5 * 86_400_000;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(
        db.clone(),
        store.clone(),
        engine::Options { workers: 2, new_keys: engine::NewKeys::NewCar, ..engine::Options::default() },
    );
    let judge = ingest::Judge::new(store.clone()).on_judged(engine.listener());
    engine.start().unwrap();
    for (n, trouble) in [(0, true), (1, false)] {
        let sc = synth::Scenario::new(17)
            .car(n)
            .start(START)
            .days(3)
            .rates(synth::Rates::Light)
            .supercharger_trip(n == 0)
            .with_trouble(trouble);
        for log in sc.logs() {
            put(&store, &judge, &log.name, &log.bytes).await;
            put(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
        }
    }
    engine.idle().await;
    let ids = engine::query::all_cars(&engine).unwrap();
    assert_eq!(ids.len(), 2, "{ids:?}");
    let home = db
        .add_place(model::Place {
            id: 0,
            name: "Made-up home".into(),
            kind: model::PlaceKind::Home,
            lat: synth::TOWN.lat,
            lon: synth::TOWN.lon,
            radius_m: 300.0,
            price: None,
        })
        .unwrap();
    engine.places_changed();
    // two made-up imported updates of the first car: one before its logs, one on its second night
    let updates = vec![9_000_000_000_000_001u64, 9_000_000_000_000_002];
    let rows = vec![
        db::UpdateRow {
            id: updates[0],
            update: model::Update {
                car: ids[0],
                start_ms: START - 20 * 86_400_000,
                end_ms: Some(START - 20 * 86_400_000 + 1_800_000),
                version: "2026.2.1".into(),
                source: model::Source::Teslamate,
                inputs: model::Inputs::NONE,
            },
        },
        db::UpdateRow {
            id: updates[1],
            update: model::Update {
                car: ids[0],
                start_ms: START + 86_400_000 + 3_600_000,
                end_ms: Some(START + 86_400_000 + 5_400_000),
                version: String::new(),
                source: model::Source::Teslamate,
                inputs: model::Inputs::NONE,
            },
        },
    ];
    db.upsert(ids[0], &rows).unwrap();
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| {
        b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2)).requests_per_minute(1_000_000)
    })
    .await;
    Seeded {
        s,
        _engine: engine,
        cars: ids.iter().map(|c| format!("c{c}")).collect(),
        ids,
        home: format!("p{}", home.id),
        updates,
        start_ms: START,
        _dir: dir,
    }
}

async fn ok(c: &mut Client, path: &str) -> Value {
    let r = c.get(path).await;
    assert_eq!(r.status, 200, "{path}: {}", r.text());
    r.json()
}

async fn code(c: &mut Client, path: &str) -> (u16, Option<String>) {
    let r = c.get(path).await;
    (r.status, r.code())
}

/// Every row of a list, page by page: no row twice, each page at most `limit`. `key` names a row.
async fn walk(c: &mut Client, path: &str, limit: usize, key: impl Fn(&Value) -> String) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..2000 {
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

fn has_keys(v: &Value, keys: &[&str]) {
    for k in keys {
        assert!(v.get(*k).is_some(), "{k}: {v}");
    }
}

const STAT_KEYS: &[&str] = &[
    "car",
    "time_zone",
    "period_start_ms",
    "period_end_ms",
    "label",
    "time_driven_ms",
    "drives",
    "distance_odometer_km",
    "distance_logged_km",
    "outside_avg_c",
    "driving_efficiency_pct",
    "energy_used_kwh",
    "energy_standing_kwh",
    "energy_added_kwh",
    "energy_grid_kwh",
    "charges",
    "energy_per_charge_kwh",
    "cost_by_currency",
    "price_per_kwh",
    "cost_per_100km",
    "net_wh_per_km",
    "gross_wh_per_km",
    "overhead_pct",
    "completeness",
];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn statistics_mileage_totals_gaps_and_compare() {
    let k = seeded().await;
    let (a, b) = (k.cars[0].clone(), k.cars[1].clone());
    let mut admin = k.s.admin().await;

    // one car, by day: newest first, each a local day, adding up to the drives list
    let s = ok(&mut admin, &format!("/api/v1/cars/{a}/statistics?period=day&limit=500")).await;
    assert_eq!((s["car"].clone(), s["period"].clone()), (json!(a), json!("day")));
    assert!(s["time_zone"].is_string() && s.get("cars").is_none(), "{s}");
    let days = s["items"].as_array().unwrap().clone();
    assert!(days.len() >= 3, "{s}");
    for d in &days {
        has_keys(d, STAT_KEYS);
        has_keys(&d["completeness"], &["recorded_pct", "unknown_ms", "dropped_stretches", "open_drive", "open_charge"]);
        assert_eq!(d["car"], json!(a));
        let label = d["label"].as_str().unwrap();
        assert!(label.len() == 10 && label.as_bytes()[4] == b'-', "{label}");
    }
    let starts: Vec<u64> = days.iter().map(|d| d["period_start_ms"].as_u64().unwrap()).collect();
    assert!(starts.windows(2).all(|w| w[0] > w[1]), "{starts:?}");
    let drives: u64 = days.iter().map(|d| d["drives"].as_u64().unwrap()).sum();
    let list = walk(&mut admin, &format!("/api/v1/cars/{a}/drives"), 500, |r| r["id"].to_string()).await;
    assert_eq!(drives, list.len() as u64, "the days add up to the drives list");
    assert!(drives > 0);
    // the whole history as one row, and months and ISO weeks labelled
    let all = ok(&mut admin, &format!("/api/v1/cars/{a}/statistics?period=all")).await;
    let row = &all["items"][0];
    assert_eq!((row["label"].as_str(), row["drives"].as_u64()), (Some("All"), Some(drives)));
    // the logs that lost frames, counted a day's records at a time: the whole history's row adds up the days'
    let dropped: u64 = days.iter().map(|d| d["completeness"]["dropped_stretches"].as_u64().unwrap()).sum();
    assert_eq!(row["completeness"]["dropped_stretches"].as_u64(), Some(dropped), "{row}");
    let wk = ok(&mut admin, &format!("/api/v1/cars/{a}/statistics?period=week")).await;
    assert!(wk["items"][0]["label"].as_str().unwrap().contains("-W"), "{wk}");
    // paging goes through every day once; a cursor of other filters is refused
    let rows =
        walk(&mut admin, &format!("/api/v1/cars/{a}/statistics?period=day"), 1, |r| r["label"].to_string()).await;
    assert_eq!(rows.len(), days.len());
    let p1 = ok(&mut admin, &format!("/api/v1/cars/{a}/statistics?period=day&limit=1")).await;
    let cur = p1["next"].as_str().unwrap().to_string();
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{a}/statistics?period=week&limit=1&after={cur}")).await.0, 400);
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{a}/statistics?period=fortnight")).await.0, 400);
    // a period: rows clipped to it
    let (f, t) = (starts[1], starts[0]);
    let one = ok(&mut admin, &format!("/api/v1/cars/{a}/statistics?period=day&from_ms={f}&to_ms={t}")).await;
    assert_eq!(one["items"].as_array().unwrap().len(), 1, "{one}");
    // several cars: rows of both, each with its car; CSV of every row
    // (the days run from the first data to now: a week of them here)
    let span = format!("&from_ms={}&to_ms={}", k.start_ms - 86_400_000, k.start_ms + 6 * 86_400_000);
    let both = ok(&mut admin, &format!("/api/v1/statistics?period=day&limit=500{span}")).await;
    assert_eq!(both["cars"], json!([a, b]));
    let cars: std::collections::BTreeSet<String> =
        both["items"].as_array().unwrap().iter().map(|r| r["car"].as_str().unwrap().to_string()).collect();
    assert_eq!(cars.len(), 2);
    let rows =
        walk(&mut admin, &format!("/api/v1/statistics?period=day{span}"), 2, |r| format!("{}{}", r["car"], r["label"]))
            .await;
    assert_eq!(rows.len(), both["items"].as_array().unwrap().len());
    let csv = admin.get(&format!("/api/v1/statistics?period=day&format=csv{span}")).await;
    assert_eq!(csv.status, 200);
    assert!(csv.text().contains("completeness.recorded_pct"), "{}", csv.text());
    assert_eq!(csv.text().trim_end().lines().count(), rows.len() + 1);

    // mileage: oldest first, by odometer and as logged, a projection
    let m = ok(&mut admin, &format!("/api/v1/cars/{a}/mileage?group=day&limit=500")).await;
    assert_eq!(m["car"], json!(a));
    let items = m["items"].as_array().unwrap();
    assert!(!items.is_empty());
    for it in items {
        has_keys(
            it,
            &[
                "period_start_ms",
                "period_end_ms",
                "label",
                "distance_km",
                "odometer_start_km",
                "odometer_end_km",
                "logged_km",
                "not_logged_km",
            ],
        );
    }
    let ms: Vec<u64> = items.iter().map(|d| d["period_start_ms"].as_u64().unwrap()).collect();
    assert!(ms.windows(2).all(|w| w[0] < w[1]), "{ms:?}");
    has_keys(&m["projection"], &["km_per_year", "basis_days"]);
    let mp = walk(&mut admin, &format!("/api/v1/cars/{a}/mileage?group=day"), 1, |r| r["label"].to_string()).await;
    assert_eq!(mp.len(), items.len());
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{a}/mileage?group=all")).await.0, 400);

    // totals of the whole history
    let t = ok(&mut admin, &format!("/api/v1/cars/{a}/totals")).await;
    has_keys(
        &t,
        &[
            "drives",
            "distance_km",
            "odometer_first_km",
            "odometer_last_km",
            "odometer_last_ms",
            "not_logged_km",
            "charges",
            "energy_added_kwh",
            "energy_grid_kwh",
            "grid_coverage_pct",
            "charging_efficiency_pct",
            "ac",
            "dc",
            "updates",
        ],
    );
    assert_eq!((t["drives"].as_u64(), t["updates"].as_u64()), (Some(drives), Some(2)));
    assert!(t["dc"]["charges"].as_u64().unwrap() >= 1, "{t}");
    let tm = ok(&mut admin, &format!("/api/v1/cars/{a}/totals?source=teslamate")).await;
    assert_eq!((tm["drives"].as_u64(), tm["updates"].as_u64()), (Some(0), Some(2)));
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{a}/totals?source=x")).await.0, 400);
    assert_eq!(admin.get(&format!("/api/v1/cars/{a}/totals?format=csv")).await.status, 400);

    // gaps: the first car had the bridge's trouble
    let g = ok(&mut admin, &format!("/api/v1/cars/{a}/gaps?limit=500")).await;
    assert_eq!(g["car"], json!(a));
    for it in g["items"].as_array().unwrap() {
        has_keys(
            it,
            &[
                "id",
                "start_ms",
                "end_ms",
                "duration_ms",
                "odometer_jump_km",
                "from_place",
                "to_place",
                "cause",
                "gap_reason",
                "source",
                "inside",
            ],
        );
        assert!(["no_log", "not_recorded", "chain_gap", "unknown"].contains(&it["cause"].as_str().unwrap()), "{it}");
        assert!(it["duration_ms"].as_u64().unwrap() >= 600_000, "{it}");
    }
    let n = g["items"].as_array().unwrap().len();
    if n > 1 {
        let gp = walk(&mut admin, &format!("/api/v1/cars/{a}/gaps"), 1, |r| r["id"].to_string()).await;
        assert_eq!(gp.len(), n);
    }

    // compare: one series per car, the metric's unit
    for (metric, unit) in [("capacity", "kWh"), ("full_range", "km"), ("wh_per_km", "Wh/km"), ("odometer", "km")] {
        let c = ok(&mut admin, &format!("/api/v1/compare?cars={a},{b}&metric={metric}")).await;
        assert_eq!((c["metric"].as_str(), c["unit"].as_str()), (Some(metric), Some(unit)), "{c}");
        let series = c["series"].as_array().unwrap();
        assert_eq!(series.len(), 2);
        for s in series {
            has_keys(s, &["car", "name", "time_zone", "t_ms", "values"]);
            assert_eq!(s["t_ms"].as_array().unwrap().len(), s["values"].as_array().unwrap().len());
        }
    }
    let odo = ok(&mut admin, &format!("/api/v1/compare?cars={a}&metric=odometer")).await;
    assert!(!odo["series"][0]["values"].as_array().unwrap().is_empty(), "{odo}");
    let whk = ok(&mut admin, &format!("/api/v1/compare?cars={a}&metric=wh_per_km&min_distance_km=100000")).await;
    assert!(whk["series"][0]["values"].as_array().unwrap().is_empty(), "{whk}");
    assert_eq!(code(&mut admin, "/api/v1/compare?metric=speed").await.0, 400);
    assert_eq!(admin.get("/api/v1/compare?format=csv").await.status, 400);

    // a watch account that sees the second car only: the first is an unknown car everywhere
    let mut watch = k.s.watch(&mut admin, &[k.ids[1]]).await;
    for path in [
        format!("/api/v1/cars/{a}/statistics"),
        format!("/api/v1/cars/{a}/mileage"),
        format!("/api/v1/cars/{a}/totals"),
        format!("/api/v1/cars/{a}/gaps"),
        format!("/api/v1/statistics?cars={a}"),
        format!("/api/v1/compare?cars={a},{b}"),
        "/api/v1/cars/c999/statistics".to_string(),
    ] {
        assert_eq!(code(&mut watch, &path).await, (404, Some("no_such_car".into())), "{path}");
    }
    let mine = ok(&mut watch, "/api/v1/statistics?period=all").await;
    assert_eq!(mine["cars"], json!([b]));
    assert!(mine["items"].as_array().unwrap().iter().all(|r| r["car"] == json!(b)));
    let cmp = ok(&mut watch, "/api/v1/compare?metric=odometer").await;
    assert_eq!(cmp["cars"], json!([b]));
    // signed out: nothing
    assert_eq!(k.s.client().get(&format!("/api/v1/cars/{b}/statistics")).await.status, 401);
}

const VISIT_KEYS: &[&str] = &[
    "id",
    "car",
    "time_zone",
    "arrived_ms",
    "left_ms",
    "lat",
    "lon",
    "accuracy_m",
    "place",
    "label",
    "address",
    "city_key",
    "drive_in",
    "drive_out",
    "charges",
    "unknown_ms",
    "source",
];

const GROUP_KEYS: &[&str] = &[
    "key",
    "label",
    "level",
    "lat",
    "lon",
    "visits",
    "places",
    "days",
    "dwell_ms",
    "first_ms",
    "last_ms",
    "first_car",
    "last_car",
    "cars",
];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn visits_their_groups_days_and_tracks() {
    let k = seeded().await;
    let (a, b) = (k.cars[0].clone(), k.cars[1].clone());
    let mut admin = k.s.admin().await;

    let v = ok(&mut admin, &format!("/api/v1/visits?cars={a}&limit=500")).await;
    assert_eq!(v["cars"], json!([a]));
    let items = v["items"].as_array().unwrap().clone();
    assert!(items.len() >= 2, "{v}");
    for it in &items {
        has_keys(it, VISIT_KEYS);
        has_keys(
            &it["address"],
            &["short", "street", "neighbourhood", "locality", "region", "country_code", "country"],
        );
        assert_eq!(it["car"], json!(a));
        assert!(it["drive_in"].is_string(), "{it}");
    }
    let arr: Vec<u64> = items.iter().map(|x| x["arrived_ms"].as_u64().unwrap()).collect();
    assert!(arr.windows(2).all(|w| w[0] >= w[1]), "newest first: {arr:?}");
    // a stop at home: the place matched; the charges that started in it listed
    let home: Vec<&Value> = items.iter().filter(|x| x["place"]["id"] == json!(k.home)).collect();
    assert!(!home.is_empty(), "{items:?}");
    assert_eq!(home[0]["label"], "Made-up home");
    assert!(items.iter().any(|x| !x["charges"].as_array().unwrap().is_empty()), "{items:?}");
    let only_home = ok(&mut admin, &format!("/api/v1/visits?cars={a}&place={}&limit=500", k.home)).await;
    assert_eq!(only_home["items"].as_array().unwrap().len(), home.len());
    let q = ok(&mut admin, &format!("/api/v1/visits?cars={a}&q=made-up%20HOME&limit=500")).await;
    assert_eq!(q["items"].as_array().unwrap().len(), home.len());
    // every sort pages through every stop once
    for sort in ["arrived_desc", "arrived_asc", "dwell_desc"] {
        let rows = walk(&mut admin, &format!("/api/v1/visits?cars={a}&sort={sort}"), 1, |r| r["id"].to_string()).await;
        assert_eq!(rows.len(), items.len(), "{sort}");
    }
    let asc = walk(&mut admin, &format!("/api/v1/visits?cars={a}&sort=arrived_asc"), 2, |r| r["id"].to_string()).await;
    let t: Vec<u64> = asc.iter().map(|x| x["arrived_ms"].as_u64().unwrap()).collect();
    assert!(t.windows(2).all(|w| w[0] <= w[1]), "{t:?}");
    assert_eq!(code(&mut admin, "/api/v1/visits?sort=name").await.0, 400);
    assert_eq!(code(&mut admin, "/api/v1/visits?place=home").await.0, 400);
    let csv = admin.get(&format!("/api/v1/visits?cars={a}&format=csv")).await;
    assert_eq!(csv.status, 200);
    assert_eq!(csv.text().trim_end().lines().count(), items.len() + 1);
    // both cars: rows of each
    let all = ok(&mut admin, "/api/v1/visits?limit=500").await;
    assert_eq!(all["cars"], json!([a, b]));
    assert!(all["items"].as_array().unwrap().iter().any(|x| x["car"] == json!(b)));

    // the summary: the same stops counted
    let s = ok(&mut admin, &format!("/api/v1/visits/summary?cars={a}")).await;
    has_keys(
        &s,
        &[
            "places",
            "cities",
            "regions",
            "countries",
            "days",
            "first_ms",
            "last_ms",
            "unnamed_stops",
            "distance_km",
            "energy_added_kwh",
            "energy_grid_kwh",
            "efficiency",
            "cost_by_currency",
            "charges_without_cost",
            "visits",
            "cars",
            "first_car",
            "last_car",
        ],
    );
    assert_eq!(s["visits"].as_u64(), Some(items.len() as u64));
    assert!(s["distance_km"].as_f64().unwrap() > 0.0 && s["days"].as_u64().unwrap() >= 3, "{s}");
    has_keys(&s["efficiency"], &["value", "charges_counted"]);
    assert_eq!(admin.get("/api/v1/visits/summary?format=csv").await.status, 400);

    // the groups: the stops at home are one place row; every level answers
    let pl = ok(&mut admin, &format!("/api/v1/visits/places?cars={a}&limit=500")).await;
    assert_eq!(pl["level"], "place");
    let groups = pl["items"].as_array().unwrap();
    for g in groups {
        has_keys(g, GROUP_KEYS);
        has_keys(g, &["place", "street", "neighbourhood", "locality", "region", "country_code", "country"]);
    }
    let total: u64 = groups.iter().map(|g| g["visits"].as_u64().unwrap()).sum();
    assert_eq!(total, items.len() as u64);
    let hg = groups.iter().find(|g| g["key"] == json!(k.home)).unwrap();
    assert_eq!((hg["visits"].as_u64(), hg["label"].as_str()), (Some(home.len() as u64), Some("Made-up home")));
    let vis: Vec<u64> = groups.iter().map(|g| g["visits"].as_u64().unwrap()).collect();
    assert!(vis.windows(2).all(|w| w[0] >= w[1]), "{vis:?}");
    for sort in ["visits", "dwell", "last", "name"] {
        let rows =
            walk(&mut admin, &format!("/api/v1/visits/places?cars={a}&sort={sort}"), 1, |r| r["key"].to_string()).await;
        assert_eq!(rows.len(), groups.len(), "{sort}");
    }
    assert_eq!(code(&mut admin, "/api/v1/visits/places?sort=arrived_desc").await.0, 400);
    for (path, level) in [("cities", "city"), ("regions", "region"), ("countries", "country")] {
        let g = ok(&mut admin, &format!("/api/v1/visits/{path}?cars={a}")).await;
        assert_eq!(g["level"], level);
        for row in g["items"].as_array().unwrap() {
            has_keys(row, GROUP_KEYS);
            assert!(row.get("place").is_none(), "{row}");
        }
    }

    // the days: newest first, each a local date of the car, with its drives
    let d = ok(&mut admin, &format!("/api/v1/visits/days?cars={a}&limit=500")).await;
    let ds = d["items"].as_array().unwrap();
    assert!(ds.len() >= 3, "{d}");
    for day in ds {
        has_keys(day, &["date", "car", "time_zone", "first_ms", "last_ms", "drives", "charges", "places"]);
    }
    let dates: Vec<&str> = ds.iter().map(|x| x["date"].as_str().unwrap()).collect();
    assert!(dates.windows(2).all(|w| w[0] > w[1]), "{dates:?}");
    assert!(ds.iter().any(|x| !x["drives"].as_array().unwrap().is_empty()));
    let dp = walk(&mut admin, &format!("/api/v1/visits/days?cars={a}"), 1, |r| r["date"].to_string()).await;
    assert_eq!(dp.len(), ds.len());

    // tracks: never more points than asked, at least two a drive, too few points is 400
    let tr = ok(&mut admin, &format!("/api/v1/tracks?cars={a}&max_points=400")).await;
    let drives = tr["drives"].as_array().unwrap();
    assert!(!drives.is_empty(), "{tr}");
    let pts: usize = drives.iter().map(|x| x["t_ms"].as_array().unwrap().len()).sum();
    assert!(pts <= 400 && tr["points"].as_u64() == Some(pts as u64), "{pts}");
    for x in drives {
        has_keys(x, &["car", "drive", "source", "t_ms", "lat", "lon", "accuracy_m"]);
        let n = x["t_ms"].as_array().unwrap().len();
        assert!(n >= 2 && x["lat"].as_array().unwrap().len() == n && x["accuracy_m"].as_array().unwrap().len() == n);
    }
    assert_eq!(code(&mut admin, &format!("/api/v1/tracks?cars={a}&max_points=2")).await.0, 400);
    assert_eq!(code(&mut admin, "/api/v1/tracks?max_points=1").await.0, 400);
    assert_eq!(admin.get("/api/v1/tracks?format=csv").await.status, 400);

    // a watch account that sees the second car only
    let mut watch = k.s.watch(&mut admin, &[k.ids[1]]).await;
    for path in [
        format!("/api/v1/visits?cars={a}"),
        format!("/api/v1/visits/summary?cars={a}"),
        format!("/api/v1/visits/places?cars={a}"),
        format!("/api/v1/visits/days?cars={a}"),
        format!("/api/v1/tracks?cars={a}"),
    ] {
        assert_eq!(code(&mut watch, &path).await, (404, Some("no_such_car".into())), "{path}");
    }
    let mine = ok(&mut watch, "/api/v1/visits?limit=500").await;
    assert_eq!(mine["cars"], json!([b]));
    assert!(mine["items"].as_array().unwrap().iter().all(|x| x["car"] == json!(b)));
    let tr = ok(&mut watch, "/api/v1/tracks").await;
    assert!(tr["drives"].as_array().unwrap().iter().all(|x| x["car"] == json!(b)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn updates_versions_and_events() {
    let k = seeded().await;
    let (a, b) = (k.cars[0].clone(), k.cars[1].clone());
    let mut admin = k.s.admin().await;

    // updates: newest first, the imported version, the stretch after each, the summary
    let u = ok(&mut admin, &format!("/api/v1/cars/{a}/updates")).await;
    assert_eq!(u["car"], json!(a));
    let items = u["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{u}");
    for it in items {
        has_keys(
            it,
            &[
                "id",
                "car",
                "start_ms",
                "end_ms",
                "duration_ms",
                "since_previous_ms",
                "until_ms",
                "version",
                "version_source",
                "detected_by",
                "build_id",
                "changed_modules",
                "charges",
                "full_range_km",
                "full_pack_kwh",
                "source",
            ],
        );
        assert_eq!((it["detected_by"].as_str(), it["source"].as_str()), (Some("teslamate"), Some("teslamate")));
    }
    let (newest, oldest) = (&items[0], &items[1]);
    assert_eq!(newest["id"], json!(k.updates[1].to_string()));
    assert_eq!((newest["version"].clone(), newest["until_ms"].clone()), (Value::Null, Value::Null));
    assert_eq!((oldest["version"].as_str(), oldest["version_source"].as_str()), (Some("2026.2.1"), Some("teslamate")));
    assert_eq!(oldest["until_ms"], newest["start_ms"]);
    assert!(oldest["charges"].as_u64().unwrap() >= 1, "{oldest}");
    assert!(newest["since_previous_ms"].as_u64().unwrap() > 0);
    let sm = &u["summary"];
    assert_eq!((sm["count"].as_u64(), sm["current_version"].clone()), (Some(2), Value::Null));
    has_keys(sm, &["median_gap_ms", "current_version_source", "current_version_since_ms", "build_changed_since"]);
    let pages = walk(&mut admin, &format!("/api/v1/cars/{a}/updates"), 1, |r| r["id"].to_string()).await;
    assert_eq!(pages.len(), 2);
    let late = ok(&mut admin, &format!("/api/v1/cars/{a}/updates?from_ms={}", k.start_ms)).await;
    assert_eq!((late["items"].as_array().unwrap().len(), late["summary"]["count"].as_u64()), (1, Some(1)));
    let none = ok(&mut admin, &format!("/api/v1/cars/{b}/updates")).await;
    assert!(none["items"].as_array().unwrap().is_empty() && none["summary"]["count"] == 0, "{none}");

    // naming the newest update's version: admins only, with the CSRF header, a version such as 2026.38.4
    let path = format!("/api/v1/cars/{a}/updates/{}", k.updates[1]);
    let r = admin
        .request(
            "PATCH",
            &path,
            Some(br#"{"version":"2026.8.3"}"#),
            &[("X-Sentry-Mate", "0"), ("Content-Type", "application/json")],
        )
        .await;
    assert_eq!((r.status, r.code()), (403, Some("csrf".into())));
    let r = admin.patch(&path, json!({ "version": "eight" })).await;
    assert_eq!((r.status, r.code()), (422, Some("invalid".into())));
    let r = admin.patch(&path, json!({ "version": 8 })).await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())));
    let r = admin.patch(&path, json!({ "version": "2026.8.3", "colour": "red" })).await;
    assert_eq!(r.status, 400);
    assert_eq!(admin.patch(&path, json!({})).await.status, 400);
    let r = admin.patch(&format!("/api/v1/cars/{a}/updates/12345"), json!({ "version": "2026.8.3" })).await;
    assert_eq!((r.status, r.code()), (404, Some("no_such_update".into())));
    let r = admin.patch(&format!("/api/v1/cars/{b}/updates/{}", k.updates[1]), json!({ "version": "2026.8.3" })).await;
    assert_eq!((r.status, r.code()), (404, Some("no_such_update".into())));
    let r = admin.patch(&path, json!({ "version": " 2026.8.3 " })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let one = r.json();
    assert_eq!((one["version"].as_str(), one["version_source"].as_str()), (Some("2026.8.3"), Some("owner")));
    assert!(one["time_zone"].is_string() && one["car"] == json!(a), "{one}");
    let u = ok(&mut admin, &format!("/api/v1/cars/{a}/updates")).await;
    assert_eq!(u["summary"]["current_version"], "2026.8.3");
    assert_eq!(u["summary"]["current_version_source"], "owner");
    let audit = ok(&mut admin, "/api/v1/audit?limit=50").await;
    assert!(audit.to_string().contains("update-version"), "{audit}");
    // forgetting it: the update has no version again
    let r = admin.patch(&path, json!({ "version": null })).await;
    assert_eq!((r.status, r.json()["version"].clone()), (200, Value::Null));

    // the fleet's versions: every car the account sees, in id order
    let r = admin.patch(&path, json!({ "version": "2026.8.3" })).await;
    assert_eq!(r.status, 200);
    let fv = ok(&mut admin, "/api/v1/fleet/versions").await;
    let rows = fv["items"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{fv}");
    for row in rows {
        has_keys(row, &["car", "version", "version_source", "build_changed_since", "last_update_ms", "time_zone"]);
    }
    assert_eq!((rows[0]["car"].clone(), rows[0]["version"].as_str()), (json!(a), Some("2026.8.3")));
    assert_eq!((rows[1]["car"].clone(), rows[1]["version"].clone()), (json!(b), Value::Null));
    let found = ok(&mut admin, "/api/v1/fleet/versions?search=2026.8").await;
    assert_eq!(found["items"].as_array().unwrap().len(), 1);
    let pages = walk(&mut admin, "/api/v1/fleet/versions", 1, |r| r["car"].to_string()).await;
    assert_eq!(pages.len(), 2);

    // events as MQTT publishes them, newest first
    let ev = ok(&mut admin, &format!("/api/v1/cars/{a}/events?limit=500")).await;
    assert_eq!(ev["car"], json!(a));
    let evs = ev["items"].as_array().unwrap();
    assert!(evs.len() >= 4, "{ev}");
    for e in evs {
        has_keys(e, &["id", "type", "at_ms", "topic", "retained", "payload"]);
        assert_eq!(e["payload"]["event_type"], e["type"], "{e}");
        assert!(e["topic"].as_str().unwrap().ends_with("/event"), "{e}");
        assert_eq!(e["retained"], false);
    }
    let at: Vec<u64> = evs.iter().map(|e| e["at_ms"].as_u64().unwrap()).collect();
    assert!(at.windows(2).all(|w| w[0] >= w[1]), "{at:?}");
    let types: std::collections::BTreeSet<&str> = evs.iter().map(|e| e["type"].as_str().unwrap()).collect();
    for t in ["drive_started", "drive_ended", "charge_started", "charge_ended", "software_changed"] {
        assert!(types.contains(t), "{t}: {types:?}");
    }
    let ended = evs.iter().find(|e| e["type"] == "drive_ended").unwrap();
    has_keys(&ended["payload"], &["drive", "start", "end", "distance", "units"]);
    assert!(ended["payload"].get("start_latitude").is_none(), "no positions unless MQTT publishes them");
    let sw = evs.iter().find(|e| e["type"] == "software_changed").unwrap();
    assert_eq!(sw["payload"]["version"], "2026.8.3");
    let rows = walk(&mut admin, &format!("/api/v1/cars/{a}/events"), 3, |r| r["id"].to_string()).await;
    assert_eq!(rows.len(), evs.len());
    let only = ok(&mut admin, &format!("/api/v1/cars/{a}/events?types=charge_ended&limit=500")).await;
    assert!(only["items"].as_array().unwrap().iter().all(|e| e["type"] == "charge_ended"));
    assert!(!only["items"].as_array().unwrap().is_empty());
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{a}/events?types=door_opened")).await.0, 400);
    let p1 = ok(&mut admin, &format!("/api/v1/cars/{a}/events?limit=1")).await;
    let cur = p1["next"].as_str().unwrap().to_string();
    let r = code(&mut admin, &format!("/api/v1/cars/{a}/events?types=drive_ended&limit=1&after={cur}")).await;
    assert_eq!(r.0, 400);

    // a watch account that sees the second car: the first is unknown, it may not name versions
    let mut watch = k.s.watch(&mut admin, &[k.ids[1]]).await;
    for p in [format!("/api/v1/cars/{a}/updates"), format!("/api/v1/cars/{a}/events")] {
        assert_eq!(code(&mut watch, &p).await, (404, Some("no_such_car".into())), "{p}");
    }
    // (a change is refused for the role before any car is looked at, so a hidden car tells nothing either)
    let r = watch.patch(&path, json!({ "version": "2026.8.4" })).await;
    assert_eq!((r.status, r.code()), (403, Some("not_allowed".into())));
    let r = watch.patch(&format!("/api/v1/cars/{b}/updates/1"), json!({ "version": "2026.8.4" })).await;
    assert_eq!((r.status, r.code()), (403, Some("not_allowed".into())));
    let fv = ok(&mut watch, "/api/v1/fleet/versions").await;
    assert_eq!(fv["items"].as_array().unwrap().len(), 1);
    assert_eq!(fv["items"][0]["car"], json!(b));
    assert!(ok(&mut watch, &format!("/api/v1/cars/{b}/events")).await["items"].is_array());
}
