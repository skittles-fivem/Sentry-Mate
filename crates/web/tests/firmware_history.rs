#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! `/cars/{car}/firmware/history` through the real router (web::testkit), over a made-up car whose fingerprints are
//! laid down by hand (made-up values, three logs days apart): set A, an update changing the battery's and the rear
//! drive unit's app CRCs, an update changing the battery's again and the front body controller's; and one imported
//! version (a made-up TeslaMate row). Rows per module from the update markers, the version names they came with,
//! the drive and log where they changed, paging, the module filter, and a watch account reading it.

use engine::Engine;
use model::{CarId, Drive, Source};
use serde_json::{Value, json};
use std::collections::BTreeSet;
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

fn drive(car: CarId, start_ms: u64) -> Drive {
    Drive {
        id: start_ms,
        car,
        start_ms,
        end_ms: start_ms + 20 * MIN,
        start: None,
        end: None,
        start_place: None,
        end_place: None,
        start_address: None,
        end_address: None,
        start_odometer_km: Some(1000.0),
        end_odometer_km: Some(1010.0),
        distance_km: Some(10.0),
        speed_max_kmh: Some(80.0),
        power_max_kw: Some(60.0),
        power_min_kw: Some(-20.0),
        energy_used_kwh: Some(1.5),
        start_soc: Some(80.0),
        end_soc: Some(78.0),
        start_range_km: None,
        end_range_km: None,
        outside_temp_avg_c: None,
        inside_temp_avg_c: None,
        ascent_m: None,
        descent_m: None,
        source: Source::Live,
        inputs: model::Inputs::SMO,
        rules_version: 1,
    }
}

async fn ok(c: &mut Client, path: &str) -> Value {
    let r = c.get(path).await;
    assert_eq!(r.status, 200, "{path}: {}", r.text());
    r.json()
}

fn items(v: &Value) -> Vec<Value> {
    v["items"].as_array().unwrap().clone()
}

/// The rows without the current version's duration (counted up to now).
fn steady(rows: &[Value]) -> Vec<Value> {
    rows.iter()
        .cloned()
        .map(|mut r| {
            if r["kind"] == "version" && r["current"] == true {
                r["duration_ms"] = Value::Null;
            }
            r
        })
        .collect()
}

fn hexes(rows: &[Value]) -> Vec<String> {
    rows.iter().map(|r| r["fingerprint"].as_str().unwrap().to_string()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn module_history_from_the_update_markers_paged_and_filtered() {
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
    let empty = db.create_car(car("Empty")).unwrap().id;

    // three logs, each 08:00 to 08:30 UTC: 1 March (set A), 4 March (bms and dir changed), 7 March (bms and vcfront)
    let logs = [
        ("mk1_20260301T080000Z_1.log.xz", MAR1 + 8 * HOUR),
        ("mk1_20260304T080000Z_2.log.xz", MAR1 + 3 * DAY + 8 * HOUR),
        ("mk1_20260307T080000Z_3.log.xz", MAR1 + 6 * DAY + 8 * HOUR),
    ];
    let sets: [[(&str, u64); 5]; 3] = [
        [
            ("VCFRONT_infoAppCrc", 0x1111_0001),
            ("BMS_appCrc", 0x2222_0001),
            ("BMS_bootCrc", 0x2222_00b0),
            ("DIR_applicationCrc", 0x3333_0001),
            ("DIF_applicationCrc", 0x4444_0001),
        ],
        [
            ("VCFRONT_infoAppCrc", 0x1111_0001),
            ("BMS_appCrc", 0x2222_0002),
            ("BMS_bootCrc", 0x2222_00b0),
            ("DIR_applicationCrc", 0x3333_0002),
            ("DIF_applicationCrc", 0x4444_0001),
        ],
        [
            ("VCFRONT_infoAppCrc", 0x1111_0002),
            ("BMS_appCrc", 0x2222_0003),
            ("BMS_bootCrc", 0x2222_00b0),
            ("DIR_applicationCrc", 0x3333_0002),
            ("DIF_applicationCrc", 0x4444_0001),
        ],
    ];
    for ((name, t), set) in logs.iter().zip(sets.iter()) {
        let vals: Vec<(&str, u64, u64, u64)> = set.iter().map(|&(f, v)| (f, v, *t, t + 30 * MIN)).collect();
        engine.fw_note_values_for_tests(id, name, &vals).unwrap();
    }
    // a drive after the second update (on the third log's day), one before it; and an imported version
    db.upsert(id, &[drive(id, MAR1 + 6 * DAY + 8 * HOUR + 5 * MIN), drive(id, MAR1 + 5 * DAY)]).unwrap();
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

    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    let mut admin = s.admin().await;
    let car = format!("c{id}");
    let base = format!("/api/v1/cars/{car}/firmware/history");

    // name the first fingerprint update (the one that changed bms and dir)
    let ups = ok(&mut admin, &format!("/api/v1/cars/{car}/updates")).await;
    let fp: Vec<&Value> =
        ups["items"].as_array().unwrap().iter().filter(|u| u["detected_by"] == "fingerprint").collect();
    assert_eq!(fp.len(), 2, "{ups}");
    let first_up = fp.iter().min_by_key(|u| u["start_ms"].as_u64().unwrap()).unwrap();
    let first_id = first_up["id"].as_str().unwrap().to_string();
    let r = admin.patch(&format!("/api/v1/cars/{car}/updates/{first_id}"), json!({ "version": "2026.8.3" })).await;
    assert_eq!(r.status, 200, "{}", r.text());

    let all = ok(&mut admin, &base).await;
    assert_eq!(all["car"], json!(car));
    let rows = items(&all);
    // 3 bms + 2 dir + 2 vcfront + 1 dif fingerprints, and the imported version
    assert_eq!(rows.len(), 9, "{all}");
    assert!(all["next"].is_null());
    let firsts: Vec<u64> = rows.iter().map(|r| r["first_seen_ms"].as_u64().unwrap()).collect();
    assert!(firsts.windows(2).all(|w| w[0] >= w[1]), "newest first: {firsts:?}");
    let ids: Vec<&str> = all["modules"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["vcfront", "bms", "dir", "dif"]);
    let counts: Vec<u64> =
        all["modules"].as_array().unwrap().iter().map(|m| m["fingerprints"].as_u64().unwrap()).collect();
    assert_eq!(counts, [2, 3, 2, 1]);
    assert_eq!(all["modules"][1]["current"], "22220003");
    assert_eq!(all["spans"].as_array().unwrap().len(), 8);
    assert_eq!(all["range"]["from_ms"], json!(MAR1 + 8 * HOUR));
    assert_eq!(all["range"]["to_ms"], json!(MAR1 + 6 * DAY + 8 * HOUR + 30 * MIN));
    let ver: Vec<&Value> = rows.iter().filter(|r| r["kind"] == "version").collect();
    assert_eq!(ver.len(), 1);
    assert_eq!(ver[0]["version"], "2026.2.9");
    assert_eq!(ver[0]["source"], "teslamate");
    assert!(ver[0]["module"].is_null() && ver[0]["fingerprint"].is_null());

    // one module: its fingerprints newest first, each with the one before, the update and version it came with
    let bms = items(&ok(&mut admin, &format!("{base}?module=bms")).await);
    assert_eq!(hexes(&bms), ["22220003", "22220002", "22220001"]);
    assert_eq!(bms.iter().map(|r| r["current"].as_bool().unwrap()).collect::<Vec<_>>(), [true, false, false]);
    assert_eq!(bms[0]["previous"], "22220002");
    assert!(bms[2]["previous"].is_null() && bms[2]["update"].is_null() && bms[2]["changed_in"].is_null());
    assert_eq!(bms[1]["update"]["id"], json!(first_id));
    assert_eq!(bms[1]["version"], "2026.8.3");
    assert_eq!(bms[1]["update"]["version_source"], "owner");
    assert!(bms[0]["version"].is_null());
    assert_eq!(bms[0]["source"], "can");
    assert_eq!(
        bms[1]["fields"],
        json!([{ "field": "BMS_appCrc", "hex": "22220002" }, { "field": "BMS_bootCrc", "hex": "222200b0" }])
    );
    // first and last seen, until the next, how long it ran
    assert_eq!(bms[2]["first_seen_ms"], json!(MAR1 + 8 * HOUR));
    assert_eq!(bms[2]["last_seen_ms"], json!(MAR1 + 8 * HOUR + 30 * MIN));
    assert_eq!(bms[2]["until_ms"], json!(MAR1 + 3 * DAY + 8 * HOUR));
    assert_eq!(bms[2]["duration_ms"], json!(30 * MIN));
    assert_eq!(bms[1]["first_seen_ms"], json!(MAR1 + 3 * DAY + 8 * HOUR));
    assert!(bms[0]["until_ms"].is_null());
    // where it changed: the log the new value came in, the first drive after the old one was last seen
    assert_eq!(bms[0]["changed_in"]["log"], logs[2].0);
    assert_eq!(bms[1]["changed_in"]["log"], logs[1].0);
    assert_eq!(bms[1]["changed_in"]["drive_id"], json!((MAR1 + 5 * DAY).to_string()));
    assert_eq!(bms[0]["changed_in"]["drive_id"], json!((MAR1 + 6 * DAY + 8 * HOUR + 5 * MIN).to_string()));
    // the front drive unit never changed: one current row; the rear's came with the named update
    let dif = items(&ok(&mut admin, &format!("{base}?module=dif")).await);
    assert_eq!(hexes(&dif), ["44440001"]);
    assert!(dif[0]["current"].as_bool().unwrap());
    let dir_rows = items(&ok(&mut admin, &format!("{base}?module=dir")).await);
    assert_eq!(hexes(&dir_rows), ["33330002", "33330001"]);
    assert_eq!(dir_rows[0]["version"], "2026.8.3");
    // the filter leaves version rows out, and keeps the summary and the strip whole
    let only = ok(&mut admin, &format!("{base}?module=vcfront")).await;
    assert_eq!(hexes(&items(&only)), ["11110002", "11110001"]);
    assert_eq!(only["spans"], all["spans"]);
    assert_eq!(only["modules"], all["modules"]);

    // paging: pages of 4 give every row once, in the same order (rows of one time included)
    let mut paged: Vec<Value> = Vec::new();
    let mut next: Option<String> = None;
    let mut pages = 0;
    loop {
        let path = match &next {
            Some(n) => format!("{base}?limit=4&after={n}"),
            None => format!("{base}?limit=4"),
        };
        let p = ok(&mut admin, &path).await;
        paged.extend(items(&p));
        pages += 1;
        match p["next"].as_str() {
            Some(n) => next = Some(n.to_string()),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(steady(&paged), steady(&rows));
    let uniq: BTreeSet<&str> = paged.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(uniq.len(), 9);
    // a cursor of another filter is refused; an unknown module and a bad limit too
    let first_page = ok(&mut admin, &format!("{base}?limit=4")).await;
    let c = first_page["next"].as_str().unwrap();
    assert_eq!(admin.get(&format!("{base}?module=bms&after={c}")).await.status, 400);
    assert_eq!(admin.get(&format!("{base}?module=gateway")).await.status, 400);
    assert_eq!(admin.get(&format!("{base}?limit=0")).await.status, 400);

    // a car without fingerprints: no rows, the four modules empty
    let none = ok(&mut admin, &format!("/api/v1/cars/c{empty}/firmware/history")).await;
    assert_eq!(items(&none).len(), 0);
    assert!(none["range"]["from_ms"].is_null());
    assert!(none["modules"].as_array().unwrap().iter().all(|m| m["fingerprints"] == 0 && m["current"].is_null()));

    // a watch account reads it for its car, and not for another
    let mut watch = s.watch(&mut admin, &[id]).await;
    assert_eq!(steady(&items(&ok(&mut watch, &base).await)), steady(&rows));
    assert_eq!(watch.get(&format!("/api/v1/cars/c{empty}/firmware/history")).await.status, 404);
}
