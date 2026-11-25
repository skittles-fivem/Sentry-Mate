#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The software version from the live inputs, through the real router (web::testkit), over a made-up car whose
//! fingerprints are laid down by hand (made-up values, three logs days apart: two fingerprint updates), one imported
//! version (a made-up TeslaMate row) and made-up versions read by local TeslaMate: the current version is named
//! from the input, each fingerprint update from the new version the input reported within a day after it, a typed
//! name wins, and imported updates count in the period. A period without updates still names the newest update.

use engine::Engine;
use model::{CarId, Input, Source};
use serde_json::{Value, json};
use std::sync::Arc;
use web::testkit::{Client, TestServer};

const MIN: u64 = 60_000;
const HOUR: u64 = 60 * MIN;
const DAY: u64 = 24 * HOUR;
/// 2026-03-01T00:00:00Z.
const MAR1: u64 = 1_772_323_200_000;

struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
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

async fn ok(c: &mut Client, path: &str) -> Value {
    let r = c.get(path).await;
    assert_eq!(r.status, 200, "{path}: {}", r.text());
    r.json()
}

fn fingerprint_updates(v: &Value) -> Vec<Value> {
    let mut fp: Vec<Value> =
        v["items"].as_array().unwrap().iter().filter(|u| u["detected_by"] == "fingerprint").cloned().collect();
    fp.sort_by_key(|u| u["start_ms"].as_u64().unwrap());
    fp
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn versions_from_a_live_input_name_the_car_and_its_fingerprint_updates() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(db.clone(), store, engine::Options { workers: 1, ..engine::Options::default() });
    engine.start().unwrap();
    let id = db.create_car(car("Made-up")).unwrap().id;

    // three logs, each 08:00 to 08:30 UTC: 1 March (set A), 4 March (B), 7 March (C)
    let logs = [
        ("mk1_20260301T080000Z_1.log.xz", MAR1 + 8 * HOUR, 0x2222_0001u64),
        ("mk1_20260304T080000Z_2.log.xz", MAR1 + 3 * DAY + 8 * HOUR, 0x2222_0002),
        ("mk1_20260307T080000Z_3.log.xz", MAR1 + 6 * DAY + 8 * HOUR, 0x2222_0003),
    ];
    for (name, t, bms) in logs {
        let vals = [("VCFRONT_infoAppCrc", 0x1111_0001, t, t + 30 * MIN), ("BMS_appCrc", bms, t, t + 30 * MIN)];
        engine.fw_note_values_for_tests(id, name, &vals).unwrap();
    }
    // an imported version a month before
    let imported = db::UpdateRow {
        id: (1 << 63) | 7,
        update: model::Update {
            car: id,
            start_ms: MAR1 - 30 * DAY,
            end_ms: Some(MAR1 - 30 * DAY + HOUR),
            version: "2026.2.9".into(),
            source: Source::Teslamate,
            inputs: model::Inputs::NONE,
        },
    };
    db.upsert(id, &[imported]).unwrap();
    // local TeslaMate's readings: the old version before the first update; a new one 4 hours after it; a new one
    // 2 hours after the second update, read again the next day
    let read = |at_ms: u64, v: &str| {
        engine.off_can(
            Input::TeslamateLocal,
            id,
            at_ms,
            model::OffCan { version: Some(v.into()), ..model::OffCan::default() },
        )
    };
    read(MAR1 + 9 * HOUR, "2026.2.9");
    read(MAR1 + 3 * DAY + 12 * HOUR, "2026.8.3");
    read(MAR1 + 6 * DAY + 10 * HOUR, "2026.12.1");
    read(MAR1 + 7 * DAY + 10 * HOUR, "2026.12.1");
    engine.idle().await;

    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    let mut admin = s.admin().await;
    let car = format!("c{id}");
    let base = format!("/api/v1/cars/{car}/updates");

    // every update: the imported one and the two found from fingerprints, both named from local TeslaMate
    let all = ok(&mut admin, &base).await;
    assert_eq!(all["summary"]["count"], json!(3), "{all}");
    let fp = fingerprint_updates(&all);
    assert_eq!(fp.len(), 2, "{all}");
    for (u, want) in fp.iter().zip(["2026.8.3", "2026.12.1"]) {
        assert_eq!(u["version"], json!(want), "{u}");
        assert_eq!(u["version_source"], json!("teslamate_local"), "{u}");
        assert_eq!(u["named_by"], json!("auto"), "{u}");
    }
    let imp = all["items"].as_array().unwrap().iter().find(|u| u["detected_by"] == "teslamate").unwrap().clone();
    assert_eq!((imp["version"].clone(), imp["version_source"].clone()), (json!("2026.2.9"), json!("teslamate")));
    // the current version: named from the input, since the second update
    let sm = &all["summary"];
    assert_eq!(sm["current_version"], json!("2026.12.1"), "{sm}");
    assert_eq!(sm["current_version_source"], json!("teslamate_local"));
    assert_eq!(sm["current_version_since_ms"], fp[1]["end_ms"]);
    assert!(sm["build_changed_since"].is_null(), "{sm}");
    assert_eq!(sm["last_update"]["id"], fp[1]["id"]);
    assert_eq!(sm["last_update"]["version"], json!("2026.12.1"));

    // the imported update counts in its period
    let feb = ok(&mut admin, &format!("{base}?from_ms={}&to_ms={}", MAR1 - 40 * DAY, MAR1)).await;
    assert_eq!(feb["summary"]["count"], json!(1), "{feb}");
    assert_eq!(feb["items"][0]["version"], json!("2026.2.9"));

    // a narrow period without updates: none listed, the version now and the newest update still named
    let narrow = ok(&mut admin, &format!("{base}?from_ms={}&to_ms={}", MAR1 + 20 * DAY, MAR1 + 21 * DAY)).await;
    assert_eq!(narrow["summary"]["count"], json!(0), "{narrow}");
    assert!(narrow["items"].as_array().unwrap().is_empty());
    assert_eq!(narrow["summary"]["current_version"], json!("2026.12.1"));
    assert_eq!(narrow["summary"]["last_update"]["start_ms"], fp[1]["start_ms"]);

    // the module history: each fingerprint came with its update's name (from the input), in the table and the bars;
    // the car versions are the imported one (until the first update) and the input's, the newest current
    let h = ok(&mut admin, &format!("/api/v1/cars/{car}/firmware/history")).await;
    let items = h["items"].as_array().unwrap();
    let mut bms: Vec<&Value> = items.iter().filter(|r| r["module"] == "bms").collect();
    bms.sort_by_key(|r| r["first_seen_ms"].as_u64().unwrap());
    let came: Vec<Value> = bms.iter().map(|r| r["version"].clone()).collect();
    // (the first one is named by local TeslaMate's reading of 2026.2.9 while it ran, though that is no update)
    assert_eq!(came, vec![json!("2026.2.9"), json!("2026.8.3"), json!("2026.12.1")], "{h}");
    assert_eq!(bms[0]["version_source"], json!("teslamate_local"));
    assert_eq!(bms[2]["update"]["version_source"], json!("teslamate_local"));
    let bars: Vec<Value> =
        h["spans"].as_array().unwrap().iter().filter(|s| s["module"] == "bms").map(|s| s["version"].clone()).collect();
    assert_eq!(bars, came);
    let mut versions: Vec<&Value> = items.iter().filter(|r| r["kind"] == "version").collect();
    versions.sort_by_key(|r| r["first_seen_ms"].as_u64().unwrap());
    let vv: Vec<(Value, Value, bool)> =
        versions.iter().map(|r| (r["version"].clone(), r["source"].clone(), r["current"].as_bool().unwrap())).collect();
    assert_eq!(
        vv,
        vec![
            (json!("2026.2.9"), json!("teslamate"), false),
            (json!("2026.8.3"), json!("teslamate_local"), false),
            (json!("2026.12.1"), json!("teslamate_local"), true),
        ],
        "{h}"
    );
    assert_eq!(versions[0]["until_ms"], fp[0]["end_ms"], "the imported version ends with the first update");

    // a typed name wins, on the update and as the current version
    let second = fp[1]["id"].as_str().unwrap().to_string();
    let r = admin.patch(&format!("{base}/{second}"), json!({ "version": "2026.12.2" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!((r.json()["version"].clone(), r.json()["version_source"].clone()), (json!("2026.12.2"), json!("owner")));
    let all = ok(&mut admin, &base).await;
    let fp = fingerprint_updates(&all);
    assert_eq!((fp[1]["version"].clone(), fp[1]["named_by"].clone()), (json!("2026.12.2"), json!("owner")));
    assert_eq!(fp[0]["version_source"], json!("teslamate_local"), "the other keeps its input's name");
    assert_eq!(all["summary"]["current_version"], json!("2026.12.2"));
    assert_eq!(all["summary"]["current_version_source"], json!("owner"));
    // and the fingerprints it brought come with it at once
    let h = ok(&mut admin, &format!("/api/v1/cars/{car}/firmware/history?module=bms")).await;
    let newest = h["items"].as_array().unwrap().iter().find(|r| r["current"] == true).unwrap().clone();
    assert_eq!(
        (newest["version"].clone(), newest["update"]["version_source"].clone()),
        (json!("2026.12.2"), json!("owner"))
    );

    // a cleared name stays cleared: the input does not name it again
    let r = admin.patch(&format!("{base}/{second}"), json!({ "version": null })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.json()["version"].is_null(), "{}", r.text());
    // the input still reads the version now
    let all = ok(&mut admin, &base).await;
    assert_eq!(all["summary"]["current_version"], json!("2026.12.1"));
    assert_eq!(all["summary"]["current_version_source"], json!("teslamate_local"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_version_reported_more_than_a_day_after_an_update_does_not_name_it() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(db.clone(), store, engine::Options { workers: 1, ..engine::Options::default() });
    engine.start().unwrap();
    let id = db.create_car(car("Made-up")).unwrap().id;
    let logs = [
        ("mk1_20260301T080000Z_1.log.xz", MAR1 + 8 * HOUR, 0x2222_0001u64),
        ("mk1_20260304T080000Z_2.log.xz", MAR1 + 3 * DAY + 8 * HOUR, 0x2222_0002),
    ];
    for (name, t, bms) in logs {
        let vals = [("BMS_appCrc", bms, t, t + 30 * MIN)];
        engine.fw_note_values_for_tests(id, name, &vals).unwrap();
    }
    // TeslaFi reads the old version before the update and a new one two days after it
    for (at, v) in [(MAR1 + 9 * HOUR, "2026.2.9"), (MAR1 + 5 * DAY + 9 * HOUR, "2026.8.3")] {
        engine.off_can(
            Input::TeslafiApi,
            id,
            at,
            model::OffCan { version: Some(v.into()), ..model::OffCan::default() },
        );
    }
    engine.idle().await;
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    let mut admin = s.admin().await;
    let all = ok(&mut admin, &format!("/api/v1/cars/c{id}/updates")).await;
    let fp = fingerprint_updates(&all);
    assert_eq!(fp.len(), 1, "{all}");
    assert!(fp[0]["version"].is_null(), "{}", fp[0]);
    // the version now is still the input's
    assert_eq!(all["summary"]["current_version"], json!("2026.8.3"));
    assert_eq!(all["summary"]["current_version_source"], json!("teslafi_api"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_update_across_days_without_readings_is_marked_and_named_from_what_the_input_said_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(db.clone(), store, engine::Options { workers: 1, ..engine::Options::default() });
    engine.start().unwrap();
    let id = db.create_car(car("Made-up")).unwrap().id;
    // the old set read daily up to 8 March (last at 22:53), then no readings at all until 23 March 00:19 (new set)
    let mut logs: Vec<(String, u64, u64, u64)> = Vec::new();
    for d in 0..8u64 {
        let t = MAR1 + d * DAY + 22 * HOUR + 23 * MIN;
        logs.push((format!("mk1_202603{:02}T222300Z_{d}.log.xz", d + 1), t, 0x1111_0001, 0x2222_0001));
    }
    let y = MAR1 + 22 * DAY + 19 * MIN;
    logs.push(("mk1_20260323T001900Z_9.log.xz".into(), y, 0x1111_0002, 0x2222_0002));
    for (name, t, vc, bms) in &logs {
        let vals = [
            ("VCFRONT_infoAppCrc", *vc, *t, t + 30 * MIN),
            ("BMS_appCrc", *bms, *t, t + 30 * MIN),
            ("BMS_bootCrc", 0x3333_0001, *t, t + 30 * MIN),
        ];
        engine.fw_note_values_for_tests(id, name, &vals).unwrap();
    }
    let x = MAR1 + 7 * DAY + 22 * HOUR + 53 * MIN;
    // local TeslaMate reported the new version from before the old set was last read, and still after the new set
    for at in [MAR1 + 6 * DAY, MAR1 + 15 * DAY, y + 2 * HOUR] {
        engine.off_can(
            Input::TeslamateLocal,
            id,
            at,
            model::OffCan { version: Some("2026.44.1".into()), ..model::OffCan::default() },
        );
    }
    engine.idle().await;
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    let mut admin = s.admin().await;
    let all = ok(&mut admin, &format!("/api/v1/cars/c{id}/updates")).await;
    let fp = fingerprint_updates(&all);
    assert_eq!(fp.len(), 1, "{all}");
    let u = &fp[0];
    assert_eq!(u["between_ms"], json!([x, y]), "{u}");
    assert_eq!(u["readings_gap"], json!(true), "{u}");
    // the row does not stretch over the two weeks nobody read: where the new set was first seen, no install time
    assert_eq!((u["start_ms"].clone(), u["end_ms"].clone()), (json!(y), json!(y)), "{u}");
    assert!(u["duration_ms"].is_null(), "{u}");
    let fields: Vec<&str> = u["changes"].as_array().unwrap().iter().map(|c| c["field"].as_str().unwrap()).collect();
    assert_eq!(fields, ["VCFRONT_infoAppCrc", "BMS_appCrc"]);
    // named from what the input reported right after the new set appeared
    assert_eq!((u["version"].clone(), u["version_source"].clone()), (json!("2026.44.1"), json!("teslamate_local")));
    let sm = &all["summary"];
    assert!(sm["build_changed_since"].is_null(), "{sm}");
    assert_eq!(sm["current_version"], json!("2026.44.1"), "{sm}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_named_version_runs_until_the_build_change_never_from_it() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(db.clone(), store, engine::Options { workers: 1, ..engine::Options::default() });
    engine.start().unwrap();
    let id = db.create_car(car("Made-up")).unwrap().id;
    // two updates on following days, no live input: the first named by hand, the second not
    let logs = [
        ("mk1_20260301T080000Z_1.log.xz", MAR1 + 8 * HOUR, 0x2222_0001u64),
        ("mk1_20260302T080000Z_2.log.xz", MAR1 + DAY + 8 * HOUR, 0x2222_0002),
        ("mk1_20260303T080000Z_3.log.xz", MAR1 + 2 * DAY + 8 * HOUR, 0x2222_0003),
    ];
    for (name, t, bms) in logs {
        engine.fw_note_values_for_tests(id, name, &[("BMS_appCrc", bms, t, t + 30 * MIN)]).unwrap();
    }
    engine.idle().await;
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    let mut admin = s.admin().await;
    let base = format!("/api/v1/cars/c{id}/updates");
    let fp = fingerprint_updates(&ok(&mut admin, &base).await);
    assert_eq!(fp.len(), 2);
    assert!(fp.iter().all(|u| u["readings_gap"] == json!(false)), "{fp:?}");
    let first = fp[0]["id"].as_str().unwrap().to_string();
    let r = admin.patch(&format!("{base}/{first}"), json!({ "version": "2026.8.3" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let all = ok(&mut admin, &base).await;
    let sm = &all["summary"];
    assert_eq!(sm["build_changed_since"], fp[1]["end_ms"], "{sm}");
    // it ran from the end of the update it names to the change: not a zero-length stretch at the change
    assert_eq!(sm["current_version_since_ms"], fp[0]["end_ms"], "{sm}");
}
