#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Battery, capacity, battery level, projected range, efficiency and temperatures through the real router
//! (web::testkit), over an engine that decoded a made-up car's days of logs (synth: a made-up VIN, made-up places, a
//! public signing seed), stored and judged the way an upload is.

use engine::Engine;
use model::CarId;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use web::testkit::{Client, TestServer};

struct Seeded {
    s: TestServer,
    _engine: Engine,
    id: CarId,
    car: String,
    from: u64,
    to: u64,
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

/// One made-up car, two days of light logs (drives, a home charge from low), decoded.
async fn seeded() -> Seeded {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(db.clone(), store.clone(), engine::Options { workers: 2, ..engine::Options::default() });
    let judge = ingest::Judge::new(store.clone()).on_judged(engine.listener());
    engine.start().unwrap();
    let sc = synth::Scenario::new(11).days(2).battery(35.0).odometer(1200.0).rates(synth::Rates::Light);
    let (from, to) = (sc.start_ms, sc.end_ms());
    for log in sc.logs() {
        put(&store, &judge, &log.name, &log.bytes).await;
        put(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
    }
    engine.idle().await;
    let cars = engine::query::all_cars(&engine).unwrap();
    assert_eq!(cars.len(), 1, "{cars:?}");
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    Seeded { s, _engine: engine, id: cars[0], car: format!("c{}", cars[0]), from, to, _dir: dir }
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

fn keys(v: &Value) -> Vec<&str> {
    let mut k: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    k.sort_unstable();
    k
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
async fn battery_capacity_level_range_efficiency_and_temperatures_of_decoded_days() {
    let k = seeded().await;
    let (car, from, to) = (k.car.clone(), k.from, k.to);
    let mut admin = k.s.admin().await;

    // battery: the pack's own reading (decoded from the logs), the band, the lifetime totals
    let b = ok(&mut admin, &format!("/api/v1/cars/{car}/battery")).await;
    assert_eq!(b["car"], json!(car));
    assert!(b["time_zone"].is_string());
    let cap = &b["capacity"];
    assert_eq!(
        keys(cap),
        vec![
            "basis",
            "buffer_kwh",
            "degradation_pct",
            "difference_kwh",
            "full_pack_kwh",
            "health_pct",
            "new_kwh",
            "new_source",
            "readings",
            "spread_kwh",
            "usable_basis",
            "usable_kwh",
            "warranty_pct"
        ]
    );
    // the made-up pack names no buffer, and the car has a charge that added more than 10 %: full and usable both
    // come from the charges, and with no buffer known the full pack is not put beside them
    assert_eq!(cap["basis"], json!("charges"), "{cap}");
    let usable = cap["usable_kwh"].as_f64().expect("the charges' usable capacity");
    assert!((40.0..120.0).contains(&usable), "{cap}");
    assert!(cap["full_pack_kwh"].is_null() && cap["buffer_kwh"].is_null(), "{cap}");
    assert!(cap["readings"].as_u64().unwrap() > 0, "{cap}");
    assert_eq!(cap["warranty_pct"], json!(70.0));
    // the history starts with the car about new (1200 km): the first 30 days' readings are the value when new
    assert_eq!(cap["new_source"], json!("early_bms"), "{b}");
    assert!(b["notes"].as_array().unwrap().contains(&json!("new_value_from_early_readings")), "{b}");
    for k2 in ["full_now_km", "full_now_from_energy_km", "full_new_km", "new_source", "lost_km"] {
        assert!(b["range"].get(k2).is_some(), "{k2}: {b}");
    }
    assert!(b["range"]["full_now_km"].as_f64().unwrap() > 100.0, "{b}");
    assert_eq!(b["band"]["low_pct"], json!(20));
    assert_eq!(b["band"]["high_pct"], json!(80));
    assert!(b["soc_pct"].is_number() && b["energy_now_kwh"].is_number(), "{b}");
    for k2 in
        ["charged_kwh", "discharged_kwh", "ac_kwh", "dc_kwh", "regen_kwh", "drive_kwh", "equivalent_cycles", "basis"]
    {
        assert!(b["lifetime"].get(k2).is_some(), "{k2}: {b}");
    }
    assert!(b["lifetime"]["charged_kwh"].as_f64().unwrap() > 0.0, "{b}");
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{car}/battery?format=csv")).await.0, 400);

    // the owner's value when new wins, and the warranty threshold follows the settings
    let st = ok(&mut admin, &format!("/api/v1/cars/{car}/settings")).await;
    let r = admin
        .patch(
            &format!("/api/v1/cars/{car}/settings"),
            json!({ "version": st["version"], "capacity_new_kwh": 80.0, "warranty_threshold_pct": 75 }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let b2 = ok(&mut admin, &format!("/api/v1/cars/{car}/battery")).await;
    assert_eq!(b2["capacity"]["new_source"], json!("owner"));
    assert_eq!(b2["capacity"]["new_kwh"], json!(80.0));
    assert_eq!(b2["capacity"]["warranty_pct"], json!(75.0));
    assert!(b2["notes"].as_array().unwrap().contains(&json!("new_value_typed")));

    // capacity: per charge (the default), per day (the pack's own reading), per month; paged
    let base = format!("/api/v1/cars/{car}/battery/capacity");
    let c = ok(&mut admin, &base).await;
    assert_eq!(c["group"], json!("charge"));
    assert!(c["trend"].get("slope_kwh_per_10000_km").is_some() && c["trend"].get("projection").is_some());
    for it in c["items"].as_array().unwrap() {
        assert_eq!(it["method"], json!("charge"), "{it}");
        assert!(it["charge"].is_string() && it["usable_kwh"].is_number(), "{it}");
        // the energy the charge stored (the rated range it added at the car's own stored energy per km): the made-up
        // car loses nothing charging, so it is the energy it added, within the rounding of the range
        assert!(["rated_range", "energy_added"].contains(&it["usable_basis"].as_str().unwrap()), "{it}");
        let (stored, added) = (it["stored_kwh"].as_f64().unwrap(), it["energy_added_kwh"].as_f64().unwrap());
        assert!((stored - added).abs() <= added * 0.02, "{it}");
    }
    let day = ok(&mut admin, &format!("{base}?group=day")).await;
    let days = day["items"].as_array().unwrap();
    assert!(!days.is_empty() && days.len() <= 3, "{day}");
    for it in days {
        assert_eq!(it["method"], json!("bms"), "{it}");
        assert!(it["full_pack_kwh"].is_number() && it["charge"].is_null(), "{it}");
        // no buffer named anywhere: usable is the full pack, and says so (the per-day line is not empty)
        assert_eq!(it["usable_basis"], json!("full_pack"), "{it}");
        assert_eq!(it["usable_kwh"], it["full_pack_kwh"], "{it}");
    }
    let p1 = ok(&mut admin, &format!("{base}?group=day&limit=1")).await;
    assert_eq!(p1["items"].as_array().unwrap().len(), 1);
    if days.len() > 1 {
        let next = p1["next"].as_str().expect("a next page");
        let p2 = ok(&mut admin, &format!("{base}?group=day&limit=1&after={next}")).await;
        assert_eq!(p2["items"][0], days[1], "the second day");
        assert_eq!(code(&mut admin, &format!("{base}?group=month&limit=1&after={next}")).await.0, 400);
    }
    let month = ok(&mut admin, &format!("{base}?group=month")).await;
    let m = &month["items"][0];
    assert!(m["readings"].as_u64().unwrap() >= 1 && m["full_pack_kwh"].is_number(), "{month}");
    assert!(m["usable_kwh"].is_number() && m["usable_basis"] == json!("full_pack"), "{month}");
    let odo = ok(&mut admin, &format!("{base}?group=day&x=odometer")).await;
    assert!(odo["items"].as_array().unwrap().iter().all(|i| i["odometer_km"].is_number()), "{odo}");
    assert_eq!(code(&mut admin, &format!("{base}?group=week")).await, (400, Some("bad_input".into())));
    assert_eq!(code(&mut admin, &format!("{base}?x=soc")).await.0, 400);
    let r = admin.get(&format!("{base}?group=day&format=csv")).await;
    assert!(r.header("content-type").is_some_and(|t| t.starts_with("text/csv")), "{}", r.text());

    // battery level: hours of the car's own day, moving lines, asleep and unknown spans
    let lv = ok(&mut admin, &format!("/api/v1/cars/{car}/battery-level?from_ms={from}&to_ms={to}")).await;
    assert_eq!((lv["bucket"].as_str(), lv["bucket_ms"].as_u64()), (Some("hour"), Some(3_600_000)));
    let bk = lv["buckets"].as_array().unwrap();
    assert!(bk.len() >= 48 && bk.len() <= 50, "{}", bk.len());
    assert!(bk.iter().any(|x| x["soc_pct"].is_number()));
    for line in ["mean", "median", "p7_5", "p92_5"] {
        assert_eq!(lv["moving"][line].as_array().unwrap().len(), bk.len(), "{line}");
    }
    for s in lv["spans"].as_array().unwrap() {
        assert!(["asleep", "unknown"].contains(&s["state"].as_str().unwrap()), "{s}");
        assert!(s["start_ms"].as_u64().unwrap() < s["end_ms"].as_u64().unwrap());
    }
    assert!(lv["stats"]["min"].as_f64().unwrap() <= lv["stats"]["max"].as_f64().unwrap());
    assert!(lv["share_above_80_pct"].is_number() && lv["band"]["high_pct"] == json!(80), "{lv}");
    assert_eq!(lv["spans_truncated"], json!(false));
    let d =
        ok(&mut admin, &format!("/api/v1/cars/{car}/battery-level?from_ms={from}&to_ms={to}&bucket=day&moving=false"))
            .await;
    assert_eq!(d["bucket_ms"], json!(86_400_000));
    let dt = d["buckets"].as_array().unwrap();
    assert!((2..=4).contains(&dt.len()), "{}", dt.len());
    assert!(d["moving"]["mean"].as_array().unwrap().is_empty());
    let def = ok(&mut admin, &format!("/api/v1/cars/{car}/battery-level?to_ms={to}")).await;
    assert_eq!(def["from_ms"].as_u64().unwrap(), to - 30 * 86_400_000, "30 days by default");
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{car}/battery-level?bucket=week")).await.0, 400);
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{car}/battery-level?window=9d")).await.0, 400);

    // projected range: columns like /series
    let pr =
        ok(&mut admin, &format!("/api/v1/cars/{car}/projected-range?from_ms={from}&to_ms={to}&max_points=100")).await;
    let t = pr["t_ms"].as_array().unwrap();
    assert!(!t.is_empty() && t.len() <= 100, "{}", t.len());
    for f in engine::query::battery::RANGE_FIELDS {
        assert_eq!(pr["values"][f].as_array().map(Vec::len), Some(t.len()), "{f}");
    }
    assert!(pr["values"]["full_range_km"].as_array().unwrap().iter().any(Value::is_number), "{pr}");
    assert_eq!(pr["state"].as_array().unwrap().len(), t.len());
    assert_eq!(pr["coverage_pct"].as_array().unwrap().len(), t.len());
    let pd = ok(&mut admin, &format!("/api/v1/cars/{car}/projected-range?from_ms={from}&to_ms={to}&bucket=day")).await;
    assert_eq!(pd["bucket_ms"], json!(86_400_000));
    // the same request answers the same (the factor behind the energy column holds still)
    let pd2 = ok(&mut admin, &format!("/api/v1/cars/{car}/projected-range?from_ms={from}&to_ms={to}&bucket=day")).await;
    assert_eq!(pd, pd2);
    let pdt = pd["t_ms"].as_array().unwrap();
    for (i, cov) in pd["coverage_pct"].as_array().unwrap().iter().enumerate() {
        assert!((0.0..=100.0).contains(&cov.as_f64().unwrap()), "{cov}");
        assert!(pd["state"][i].is_string(), "{pd}");
        let _ = &pdt[i];
    }
    let r = admin.get(&format!("/api/v1/cars/{car}/projected-range?from_ms={from}&to_ms={to}&format=csv")).await;
    assert!(r.header("content-type").is_some_and(|t| t.starts_with("text/csv")), "{}", r.text());
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{car}/projected-range?bucket=fortnight")).await.0, 400);
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{car}/projected-range?bucket=10")).await.0, 400);

    // efficiency: net and gross, groups by outside temperature, the factor
    let ef = ok(&mut admin, &format!("/api/v1/cars/{car}/efficiency?from_ms={from}&to_ms={to}")).await;
    assert!(ef["drives"].as_u64().unwrap() > 0, "{ef}");
    assert!(ef["net_wh_per_km"].as_f64().unwrap() > 50.0, "{ef}");
    assert!(ef["gross_wh_per_km"].as_f64().unwrap() >= ef["net_wh_per_km"].as_f64().unwrap() - 0.1, "{ef}");
    let groups = ef["by_temperature"].as_array().unwrap();
    let in_groups: u64 = groups.iter().map(|g| g["drives"].as_u64().unwrap()).sum();
    assert_eq!(in_groups + ef["no_temperature"]["drives"].as_u64().unwrap(), ef["drives"].as_u64().unwrap());
    for g in groups {
        assert!((g["temp_to_c"].as_f64().unwrap() - g["temp_c"].as_f64().unwrap() - 5.0).abs() < 1e-9, "{g}");
    }
    assert_eq!(ef["factor"]["source"], json!("energy_and_range"));
    assert!(ef["factor"]["rated_wh_per_km"].is_number(), "{ef}");
    // the factor comes from many readings and holds still between requests
    assert!(ef["factor"]["readings"].as_u64().unwrap() > 1, "{ef}");
    let ef2 = ok(&mut admin, &format!("/api/v1/cars/{car}/efficiency?from_ms={from}&to_ms={to}")).await;
    assert_eq!(ef2["factor"], ef["factor"]);
    assert!(ef["factor"]["ideal_wh_per_km"].is_number(), "{ef}");
    // drives without energy are counted, their distance is left out of the energy per km
    assert!(ef["without_energy"].is_u64(), "{ef}");
    let (dkm, dkwh) = (ef["distance_km"].as_f64().unwrap(), ef["energy_kwh"].as_f64().unwrap());
    if ef["without_energy"] == json!(0) {
        assert!((ef["net_wh_per_km"].as_f64().unwrap() - dkwh * 1000.0 / dkm).abs() < 0.2, "{ef}");
    }
    // a period after now is empty, not an error; parked energy is never -0
    for q in ["from_ms=4000000000000&to_ms=4100000000000", "from_ms=4000000000000", "to_ms=1"] {
        let fu = ok(&mut admin, &format!("/api/v1/cars/{car}/efficiency?{q}")).await;
        assert_eq!(fu["drives"], json!(0), "{q}: {fu}");
        assert!(fu["net_wh_per_km"].is_null(), "{q}: {fu}");
        assert_eq!(fu["parked_kwh"].to_string(), "0.0", "{q}: {fu}");
    }
    // the owner's factor: ideal follows the car's own range over ideal, and a typed value has no time
    let st = ok(&mut admin, &format!("/api/v1/cars/{car}/settings")).await;
    let r = admin
        .patch(
            &format!("/api/v1/cars/{car}/settings"),
            json!({ "version": st["version"], "efficiency_wh_per_km_override": 140.0 }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let ow = ok(&mut admin, &format!("/api/v1/cars/{car}/efficiency?from_ms={from}&to_ms={to}")).await;
    assert_eq!(ow["factor"]["source"], json!("owner"));
    assert_eq!(ow["factor"]["rated_wh_per_km"], json!(140.0));
    let ideal = ow["factor"]["ideal_wh_per_km"].as_f64().expect("ideal from the car's ratio");
    assert!(ideal > 100.0 && ideal < 140.0, "{ow}");
    assert!(ow["factor"]["as_of_ms"].is_null(), "{ow}");
    let st = ok(&mut admin, &format!("/api/v1/cars/{car}/settings")).await;
    let r = admin
        .patch(
            &format!("/api/v1/cars/{car}/settings"),
            json!({ "version": st["version"], "efficiency_wh_per_km_override": null }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let far =
        ok(&mut admin, &format!("/api/v1/cars/{car}/efficiency?from_ms={from}&to_ms={to}&min_distance_km=10000")).await;
    assert_eq!(far["drives"], json!(0));
    assert_eq!(far["all_drives"]["drives"], ef["all_drives"]["drives"], "gross keeps every drive");
    assert_eq!(code(&mut admin, &format!("/api/v1/cars/{car}/efficiency?temp_group_c=25")).await.0, 400);

    // temperatures: summaries, the car's own days, nothing asleep is unknown
    let tp = ok(&mut admin, &format!("/api/v1/cars/{car}/temperatures?from_ms={from}&to_ms={to}")).await;
    for k2 in ["outside", "cabin", "battery"] {
        let s = &tp[k2];
        assert!(s["min_c"].as_f64().unwrap() <= s["avg_c"].as_f64().unwrap(), "{k2}: {s}");
        assert!(s["avg_c"].as_f64().unwrap() <= s["max_c"].as_f64().unwrap(), "{k2}: {s}");
    }
    assert!(tp["missing_ms"]["asleep"].is_u64() && tp["missing_ms"]["unknown"].is_u64());
    let daily = tp["daily"].as_array().unwrap();
    assert!((2..=4).contains(&daily.len()), "{}", daily.len());
    for dd in daily {
        assert!(dd["date"].is_string() && dd["t_ms"].is_u64(), "{dd}");
        let cov = dd["coverage_pct"].as_f64().unwrap();
        assert!((0.0..=100.0).contains(&cov), "{dd}");
    }
    // lowest and highest are every reading's: never above /series' lowest point
    let se = ok(
        &mut admin,
        &format!("/api/v1/cars/{car}/series?fields=outside_c&from_ms={from}&to_ms={to}&agg=min&max_points=50"),
    )
    .await;
    let smin = se["values"]["outside_c"].as_array().unwrap().iter().filter_map(Value::as_f64).fold(f64::MAX, f64::min);
    assert!(tp["outside"]["min_c"].as_f64().unwrap() <= smin + 0.05, "{} vs {smin}", tp["outside"]["min_c"]);

    // a watch account that sees the car reads it all; one that does not gets the unknown car's answer
    let mut watch = k.s.watch(&mut admin, &[k.id]).await;
    // a second car, not on the watch account's list
    let other = k
        ._engine
        .db()
        .create_car(model::Car {
            id: 0,
            name: "Other".into(),
            vehicle_keys: Vec::new(),
            preferred_key: None,
            vin: None,
            profile_setting: None,
            time_zone: "UTC".into(),
            time_zone_typed: Some(true),
            efficiency_wh_km: None,
            created_ms: 0,
            inputs: Default::default(),
        })
        .unwrap()
        .id;
    let wb = ok(&mut watch, &format!("/api/v1/cars/{car}/battery")).await;
    assert_eq!(wb["capacity"], b2["capacity"]);
    for path in ["battery", "battery/capacity", "battery-level", "projected-range", "efficiency", "temperatures"] {
        let unknown = code(&mut admin, &format!("/api/v1/cars/c999/{path}")).await;
        assert_eq!(unknown, (404, Some("no_such_car".into())), "{path}");
        let hidden = code(&mut watch, &format!("/api/v1/cars/c{other}/{path}")).await;
        assert_eq!(hidden, unknown, "{path}: a hidden car answers as an unknown one");
        assert_eq!(code(&mut watch, &format!("/api/v1/cars/c999/{path}")).await, unknown, "{path}");
        let mut blind = k.s.client();
        assert_eq!(code(&mut blind, &format!("/api/v1/cars/{car}/{path}")).await.0, 401, "{path}: signed out");
        // reads only: a change is refused
        let r = watch.post(&format!("/api/v1/cars/{car}/{path}"), json!({})).await;
        assert!(r.status == 403 || r.status == 405, "{path}: {}", r.status);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_car_without_history_answers_the_same_keys_with_null() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(db.clone(), store, engine::Options { workers: 1, ..engine::Options::default() });
    engine.start().unwrap();
    let id = db
        .create_car(model::Car {
            id: 0,
            name: "Empty".into(),
            vehicle_keys: Vec::new(),
            preferred_key: None,
            vin: None,
            profile_setting: None,
            time_zone: "UTC".into(),
            time_zone_typed: Some(true),
            efficiency_wh_km: None,
            created_ms: 0,
            inputs: Default::default(),
        })
        .unwrap()
        .id;
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    let mut admin = s.admin().await;
    let car = format!("c{id}");
    let b = ok(&mut admin, &format!("/api/v1/cars/{car}/battery")).await;
    assert!(b["capacity"]["full_pack_kwh"].is_null() && b["capacity"]["new_kwh"].is_null(), "{b}");
    assert!(b["lifetime"]["charged_kwh"].is_null(), "{b}");
    assert_eq!(b["notes"], json!(["no_history"]));
    let c = ok(&mut admin, &format!("/api/v1/cars/{car}/battery/capacity")).await;
    assert_eq!(c["items"], json!([]));
    let lv = ok(&mut admin, &format!("/api/v1/cars/{car}/battery-level")).await;
    assert!(lv["buckets"].as_array().unwrap().iter().all(|x| x["soc_pct"].is_null()));
    assert!(lv["stats"]["mean"].is_null());
    let pr = ok(&mut admin, &format!("/api/v1/cars/{car}/projected-range")).await;
    assert_eq!(pr["t_ms"], json!([]));
    let ef = ok(&mut admin, &format!("/api/v1/cars/{car}/efficiency")).await;
    assert!(ef["net_wh_per_km"].is_null() && ef["drives"] == json!(0), "{ef}");
    let tp = ok(&mut admin, &format!("/api/v1/cars/{car}/temperatures")).await;
    assert!(tp["outside"]["min_c"].is_null() && tp["daily"] == json!([]), "{tp}");
}

/// The days' pack values worked out ahead give the answers the logs decoded again give: with them gone (as for data
/// of an older version), the battery answers have them worked out again and say the same.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
async fn the_pack_values_worked_out_ahead_answer_as_the_logs_decoded_again() {
    let k = seeded().await;
    let car = k.car.clone();
    let mut admin = k.s.admin().await;
    let paths = [
        format!("/api/v1/cars/{car}/battery"),
        format!("/api/v1/cars/{car}/battery/capacity?group=day"),
        format!("/api/v1/cars/{car}/battery/capacity?group=month"),
        format!("/api/v1/cars/{car}/efficiency"),
    ];
    let mut kept = Vec::new();
    for p in &paths {
        kept.push(ok(&mut admin, p).await);
    }
    assert!(kept[1]["items"].as_array().is_some_and(|v| !v.is_empty()), "{}", kept[1]);
    let kv = k._engine.db().kv();
    for ns in [engine::query::battery::PACK_NS, engine::query::pack_logs::NS] {
        let keys = kv.keys(ns).unwrap();
        assert!(!keys.is_empty(), "{ns}");
        for key in keys {
            kv.delete(ns, &key).unwrap();
        }
    }
    for (p, before) in paths.iter().zip(&kept) {
        assert_eq!(&ok(&mut admin, p).await, before, "{p}");
    }
    // and the logs' values are kept again
    assert!(!kv.keys(engine::query::pack_logs::NS).unwrap().is_empty());
}

/// More record days than a `group=day` page looks at (limit + 180): one short log a day of a made-up car over 240
/// days. Paging one reading at a time reaches the newest day, and holds every day the whole list holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
async fn day_readings_page_one_at_a_time_past_the_days_a_page_looks_at() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 2_000_000, |_| u64::MAX);
    let engine = Engine::new(db.clone(), store.clone(), engine::Options { workers: 4, ..engine::Options::default() });
    let judge = ingest::Judge::new(store.clone()).on_judged(engine.listener());
    engine.start().unwrap();
    // the first log of each of 240 days (a short run of the same made-up car started on each day, left unsigned:
    // the runs do not chain)
    let mut days = 0u64;
    for i in 0..240u64 {
        let sc = synth::Scenario::new(23)
            .start(synth::DEFAULT_START_MS + i * 86_400_000)
            .days(1)
            .battery(70.0)
            .odometer(1200.0 + i as f64 * 40.0)
            .rates(synth::Rates::Light);
        let Some(mut log) = sc.logs().next() else { continue };
        if let Some(o) = log.sidecar.as_object_mut() {
            o.remove("integrity");
        }
        days += 1;
        put(&store, &judge, &log.name, &log.bytes).await;
        put(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
    }
    assert!(days > 185, "{days} days");
    engine.idle().await;
    let cars = engine::query::all_cars(&engine).unwrap();
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    let mut admin = s.admin().await;
    let base = format!("/api/v1/cars/c{}/battery/capacity?group=day", cars[0]);
    let whole = ok(&mut admin, &format!("{base}&limit=500")).await;
    let all = whole["items"].as_array().unwrap().clone();
    assert!(all.len() > 182, "{} days with readings", all.len());
    assert!(whole["next"].is_null());
    let mut paged: Vec<Value> = Vec::new();
    let mut next: Option<String> = None;
    for _ in 0..1000 {
        let path = match &next {
            Some(n) => format!("{base}&limit=1&after={n}"),
            None => format!("{base}&limit=1"),
        };
        let p = ok(&mut admin, &path).await;
        let items = p["items"].as_array().unwrap();
        assert!(items.len() <= 1, "{p}");
        paged.extend(items.iter().cloned());
        match p["next"].as_str() {
            Some(n) => next = Some(n.to_string()),
            None => break,
        }
    }
    assert_eq!(paged.len(), all.len(), "every day, the newest too");
    assert_eq!(paged, all);
}

/// A made-up car with only its first few minutes of logs and no reference values: every battery endpoint the
/// battery page reads (and every query shape it sends) answers 200 with empty values, never a 500.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_car_with_only_a_few_minutes_of_data_and_no_references_answers_every_battery_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(db.clone(), store.clone(), engine::Options { workers: 2, ..engine::Options::default() });
    let judge = ingest::Judge::new(store.clone()).on_judged(engine.listener());
    engine.start().unwrap();
    let sc = synth::Scenario::new(12).days(1).battery(60.0).odometer(48_000.0).rates(synth::Rates::Light);
    for log in sc.logs().take(4) {
        put(&store, &judge, &log.name, &log.bytes).await;
        put(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
    }
    engine.idle().await;
    let cars = engine::query::all_cars(&engine).unwrap();
    assert_eq!(cars.len(), 1, "{cars:?}");
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    let mut admin = s.admin().await;
    let car = format!("c{}", cars[0]);
    // working values kept by an older version in another shape are worked out again, not a failed request
    let day = sc.start_ms / 86_400_000;
    db.kv().put(engine::query::battery::PACK_NS, &format!("car/{}/day/{day}", cars[0]), b"{\"old\":1}").unwrap();
    db.kv()
        .put(engine::query::battery::PACK_NS, &format!("car/{}/factor/{}", cars[0], day * 86_400_000), b"[1]")
        .unwrap();
    let now = sc.start_ms + 600_000;
    let mut paths = vec![
        format!("/api/v1/cars/{car}/battery"),
        format!("/api/v1/cars/{car}/efficiency"),
        format!("/api/v1/cars/{car}/temperatures"),
        format!("/api/v1/cars/{car}/projected-range"),
        format!("/api/v1/cars/{car}/battery-level"),
    ];
    for g in ["charge", "day", "month"] {
        for x in ["time", "odometer"] {
            paths.push(format!("/api/v1/cars/{car}/battery/capacity?group={g}&x={x}&limit=500"));
        }
    }
    for preset in ["all", "year", "month", "week", "day"] {
        paths.push(format!("/api/v1/cars/{car}/battery-level?preset={preset}"));
        paths.push(format!("/api/v1/cars/{car}/projected-range?preset={preset}"));
        paths.push(format!("/api/v1/cars/{car}/efficiency?preset={preset}"));
        paths.push(format!("/api/v1/cars/{car}/temperatures?preset={preset}"));
    }
    for (bucket, window) in [("hour", "7d"), ("day", "30d")] {
        for moving in ["true", "false"] {
            paths.push(format!(
                "/api/v1/cars/{car}/battery-level?from_ms={}&to_ms={now}&bucket={bucket}&moving={moving}&window={window}",
                sc.start_ms
            ));
        }
    }
    // an empty period: nothing in it
    paths.push(format!("/api/v1/cars/{car}/battery-level?from_ms={now}&to_ms={}", now + 1));
    paths.push(format!("/api/v1/cars/{car}/projected-range?from_ms={now}&to_ms={}", now + 1));
    for p in &paths {
        ok(&mut admin, p).await;
    }
    let b = ok(&mut admin, &format!("/api/v1/cars/{car}/battery")).await;
    assert!(b["capacity"]["degradation_pct"].is_null() || b["capacity"]["degradation_pct"].is_number(), "{b}");
    assert_eq!(b["pack"], json!({ "replaced_ms": null, "source": null }), "{b}");

    // the owner says the pack was replaced: the figures follow the new pack, its value when new is the owner's
    let st = ok(&mut admin, &format!("/api/v1/cars/{car}/settings")).await;
    let r = admin
        .patch(
            &format!("/api/v1/cars/{car}/settings"),
            json!({ "version": st["version"], "pack_replaced_date": "2026-01-05", "pack_new_kwh": 77.5 }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(
        (r.json()["pack_replaced_date"].clone(), r.json()["pack_new_kwh"].clone()),
        (json!("2026-01-05"), json!(77.5))
    );
    let b = ok(&mut admin, &format!("/api/v1/cars/{car}/battery")).await;
    assert_eq!(b["pack"]["source"], json!("owner"), "{b}");
    assert!(b["pack"]["replaced_ms"].is_u64(), "{b}");
    assert_eq!((b["capacity"]["new_kwh"].clone(), b["capacity"]["new_source"].clone()), (json!(77.5), json!("owner")));
    assert!(b["notes"].as_array().unwrap().contains(&json!("pack_replaced")), "{b}");
    let c = ok(&mut admin, &format!("/api/v1/cars/{car}/battery/capacity")).await;
    assert_eq!(c["pack"], b["pack"]);
    let r = admin
        .patch(
            &format!("/api/v1/cars/{car}/settings"),
            json!({ "version": st["version"].as_u64().unwrap() + 1, "pack_replaced_date": "05/01/2026" }),
        )
        .await;
    assert_eq!(r.status, 422, "{}", r.text());
}
