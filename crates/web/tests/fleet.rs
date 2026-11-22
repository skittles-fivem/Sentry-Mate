#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Fleet and cars through the real router (web::testkit), over an engine on a made-up fleet: three cars with
//! made-up VINs and newest values, of which a watch account sees one.

use engine::Engine;
use model::{Car, CarId, Now, Sample, State, VinInfo};
use serde_json::{Value, json};
use std::sync::Arc;
use web::testkit::{Client, TestServer};

struct Fleet {
    s: TestServer,
    engine: Engine,
    _dir: tempfile::TempDir,
}

fn made_up_car(name: &str, model: char, year: u16, serial: &str) -> Car {
    Car {
        id: 0,
        name: name.into(),
        vehicle_keys: Vec::new(),
        preferred_key: None,
        // a made-up VIN prefix (the first 11 characters, plant F; no serial)
        vin: Some(VinInfo {
            vin_prefix: format!("5YJ{model}{serial}F"),
            model: Some(model),
            year: Some(year),
            plant: Some('F'),
            via: 0x405,
        }),
        profile_setting: None,
        time_zone: "Europe/Amsterdam".into(),
        time_zone_typed: Some(true),
        efficiency_wh_km: None,
        created_ms: 1_790_000_000_000,
        inputs: Default::default(),
    }
}

fn now_of(car: CarId, at_ms: u64, state: State, driving: bool, soc: f64, charging: bool) -> Now {
    let sample = Sample {
        t_ms: at_ms,
        soc: Some(soc),
        range_km: Some(soc * 4.5),
        ideal_range_km: Some(soc * 4.9),
        charging: Some(charging),
        charge_power_kw: charging.then_some(11.0),
        tpms_bar: [Some(2.9), Some(2.9), Some(if soc < 10.0 { 2.0 } else { 2.9 }), Some(2.9)],
        ..Sample::default()
    };
    Now { range_estimated_km: None, healthy: None, car, at_ms, state: Some(state), driving, sample, place: None }
}

/// Cars c1 (driving, fresh), c2 (charging, fresh) and c3 (asleep for two days, a low tyre and a low battery).
async fn fleet() -> Fleet {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let now = (accounts::system_clock())();
    let a = db.create_car(made_up_car("Blue", 'Y', 2024, "GCEK0R")).unwrap();
    let b = db.create_car(made_up_car("Red", '3', 2022, "E1EA7N")).unwrap();
    let c = db.create_car(made_up_car("Grey", 'S', 2023, "A1E26P")).unwrap();
    db.set_now(now_of(a.id, now - 5_000, State::Online, true, 64.0, false)).unwrap();
    db.set_now(now_of(b.id, now - 60_000, State::Online, false, 41.0, true)).unwrap();
    db.set_now(now_of(c.id, now - 2 * 86_400_000, State::Asleep, false, 7.0, false)).unwrap();
    let store = ingest::Store::new(data.join("can"), v);
    let engine = Engine::new(db, store, engine::Options::default());
    let ids: std::collections::HashSet<CarId> = [a.id, b.id, c.id].into_iter().collect();
    let e2 = engine.clone();
    let s = TestServer::start_with(move |b| b.cars(Arc::new(web::FixedCars(ids))).service(Arc::new(e2))).await;
    Fleet { s, engine, _dir: dir }
}

fn ids(v: &Value) -> Vec<String> {
    v["items"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap().to_string()).collect()
}

async fn all_pages(c: &mut Client, path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let sep = if path.contains('?') { '&' } else { '?' };
        let url = match &after {
            Some(a) => format!("{path}{sep}limit=1&after={a}"),
            None => format!("{path}{sep}limit=1"),
        };
        let r = c.get(&url).await;
        assert_eq!(r.status, 200, "{url}: {}", r.text());
        out.extend(ids(&r.json()));
        match r.json()["next"].as_str() {
            Some(n) => after = Some(n.to_string()),
            None => return out,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cars_list_pages_sorts_filters_and_hides_what_an_account_may_not_see() {
    let f = fleet().await;
    let mut admin = f.s.admin().await;

    let r = admin.get("/api/v1/cars").await;
    assert_eq!(r.status, 200, "{}", r.text());
    let list = r.json();
    assert_eq!(ids(&list), ["c1", "c2", "c3"], "newest activity first: {list}");
    assert_eq!(list["total"], json!(3));
    let one = &list["items"][0];
    for k in [
        "id",
        "name",
        "model",
        "year",
        "vin_prefix",
        "trim",
        "profile",
        "time_zone",
        "last_activity_ms",
        "now",
        "attention",
    ] {
        assert!(one.get(k).is_some(), "{k} missing: {one}");
    }
    for k in [
        "state",
        "since_ms",
        "at_ms",
        "received_ms",
        "stale",
        "soc_pct",
        "range_km",
        "ideal_range_km",
        "place",
        "plugged",
        "charge_power_kw",
    ] {
        assert!(one["now"].get(k).is_some(), "now.{k} missing: {one}");
    }
    assert_eq!((one["model"].clone(), one["now"]["state"].clone()), (json!("Model Y"), json!("driving")));
    assert_eq!(list["items"][1]["now"]["state"], json!("charging"));
    assert_eq!(list["items"][2]["now"]["stale"], json!(true));
    assert!(list["items"][2]["attention"].as_array().unwrap().contains(&json!("low_battery")), "{list}");

    // paging by one keeps every car once, in the same order
    assert_eq!(all_pages(&mut admin, "/api/v1/cars").await, ["c1", "c2", "c3"]);
    assert_eq!(all_pages(&mut admin, "/api/v1/cars?sort=name").await, ["c1", "c3", "c2"]);
    assert_eq!(ids(&admin.get("/api/v1/cars?sort=soc").await.json()), ["c1", "c2", "c3"]);
    assert_eq!(ids(&admin.get("/api/v1/cars?sort=soc&dir=asc").await.json()), ["c3", "c2", "c1"]);
    // search (name, model, VIN prefix) and state; no total then
    let r = admin.get("/api/v1/cars?search=model%203").await.json();
    assert_eq!((ids(&r), r.get("total")), (vec!["c2".to_string()], None));
    assert_eq!(ids(&admin.get("/api/v1/cars?search=5yjs").await.json()), ["c3"]);
    assert_eq!(ids(&admin.get("/api/v1/cars?state=asleep").await.json()), ["c3"]);
    // bad values name the field; a cursor of another sort is refused
    assert!(admin.get("/api/v1/cars?state=flying").await.json()["error"]["fields"]["state"].is_string());
    assert_eq!(admin.get("/api/v1/cars?sort=colour").await.status, 400);
    let next = admin.get("/api/v1/cars?limit=1").await.json()["next"].as_str().unwrap().to_string();
    let r = admin.get(&format!("/api/v1/cars?sort=name&limit=1&after={next}")).await;
    assert_eq!((r.status, r.json()["error"]["fields"]["after"].is_string()), (400, true), "{}", r.text());
    // custom: the person's pins, then their order, then the rest by activity
    let r = admin.patch("/api/v1/me/preferences", json!({ "pinned_cars": ["c3"], "car_order": ["c2"] })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(all_pages(&mut admin, "/api/v1/cars?sort=custom").await, ["c3", "c2", "c1"]);
    // a cursor of the custom order is of that order: changed pins start the list again
    let next = admin.get("/api/v1/cars?sort=custom&limit=1").await.json()["next"].as_str().unwrap().to_string();
    assert_eq!(admin.get(&format!("/api/v1/cars?sort=custom&limit=1&after={next}")).await.status, 200);
    let r = admin.patch("/api/v1/me/preferences", json!({ "pinned_cars": ["c1"] })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = admin.get(&format!("/api/v1/cars?sort=custom&limit=1&after={next}")).await;
    assert_eq!((r.status, r.json()["error"]["fields"]["after"].is_string()), (400, true), "{}", r.text());
    assert_eq!(all_pages(&mut admin, "/api/v1/cars?sort=custom").await, ["c1", "c2", "c3"]);
    // CSV: every row, not one page
    let r = admin.get("/api/v1/cars?format=csv&limit=1").await;
    assert_eq!(r.status, 200);
    let text = r.text();
    assert!(text.lines().count() == 4 && text.contains("now.soc_pct"), "{text}");

    // a watch account sees its one car only; the others are unknown to it
    let mut watch = f.s.watch(&mut admin, &[2]).await;
    let r = watch.get("/api/v1/cars").await.json();
    assert_eq!((ids(&r), r["total"].clone()), (vec!["c2".to_string()], json!(1)));
    assert_eq!(watch.get("/api/v1/cars/c1").await.json()["error"]["code"], json!("no_such_car"));
    assert_eq!(watch.get("/api/v1/cars/c9").await.json()["error"]["code"], json!("no_such_car"));
    let s = watch.get("/api/v1/fleet/summary").await.json();
    assert_eq!(
        s,
        json!({ "cars": 1, "by_state": { "driving": 0, "charging": 1, "parked": 0, "asleep": 0, "unknown": 0 }, "stale": 0, "sold": 0 })
    );
    // (no log of c2 ever arrived: it is silent, as its health says; nothing of the other cars shows)
    let a = watch.get("/api/v1/fleet/attention").await.json();
    assert!(a["items"].as_array().unwrap().iter().all(|i| i["car"] == json!("c2")), "{a}");
    drop(f.engine);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_fleet_summary_and_attention_count_every_car() {
    let f = fleet().await;
    let mut admin = f.s.admin().await;
    let s = admin.get("/api/v1/fleet/summary").await.json();
    assert_eq!(
        s,
        json!({ "cars": 3, "by_state": { "driving": 1, "charging": 1, "parked": 0, "asleep": 1, "unknown": 0 }, "stale": 1, "sold": 0 })
    );
    let a = admin.get("/api/v1/fleet/attention").await;
    assert_eq!(a.status, 200, "{}", a.text());
    let items = a.json()["items"].as_array().cloned().unwrap();
    let of = |car: &str| -> Vec<&str> {
        items.iter().filter(|i| i["car"] == json!(car)).map(|i| i["code"].as_str().unwrap()).collect()
    };
    // no log of these made-up cars ever arrived: each is silent (the rule of its health), c3's battery is low; a
    // tyre is not said against a pressure the server picked (the car's recommended one isn't read)
    assert_eq!((of("c1"), of("c2")), (vec!["upload_silence"], vec!["upload_silence"]), "{items:?}");
    assert_eq!(of("c3"), ["upload_silence", "low_battery"], "{items:?}");
    let silent = items.iter().find(|i| i["code"] == json!("upload_silence")).unwrap();
    assert_eq!(
        (silent["since_ms"].clone(), silent["detail"].clone()),
        (json!(null), json!({ "last_upload_ms": null }))
    );
    // when the battery went low is not known: not "since just now"
    let low = items.iter().find(|i| i["code"] == json!("low_battery")).unwrap();
    assert_eq!((low["since_ms"].clone(), low["detail"].clone()), (json!(null), json!({ "soc_pct": 7.0 })));
    // pages of one hold each item once
    let mut seen = 0;
    let mut after = String::new();
    loop {
        let r = admin.get(&format!("/api/v1/fleet/attention?limit=1{after}")).await.json();
        seen += r["items"].as_array().unwrap().len();
        match r["next"].as_str() {
            Some(n) => after = format!("&after={n}"),
            None => break,
        }
    }
    assert_eq!(seen, items.len());
    assert_eq!(admin.get("/api/v1/fleet/attention?limit=0").await.status, 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_car_is_changed_with_its_version_by_an_admin_only() {
    let f = fleet().await;
    let mut admin = f.s.admin().await;
    let r = admin.get("/api/v1/cars/c1").await;
    assert_eq!(r.status, 200, "{}", r.text());
    let car = r.json();
    assert_eq!(car["version"], json!(1));
    // the CAN Bridge keys are an admin's to read, at /can-keys: not in the car
    assert!(car.get("keys").is_none() && car.get("preferred_key").is_none(), "{car}");
    assert_eq!(
        car["config"]["model"],
        json!({
            "value": "Model Y",
            "source": "vin",
            "sources": [{ "kind": "smo", "label": "Data from SMO" }],
        })
    );
    assert_eq!(car["config"]["paint"], json!({ "value": null, "source": null, "sources": [] }));
    for k in [
        "trim",
        "drivetrain",
        "performance_package",
        "pack_kwh_code",
        "wheel",
        "paint",
        "roof",
        "spoiler",
        "right_hand_drive",
        "map_region",
        "supercharging_access",
    ] {
        assert!(car["config"].get(k).is_some(), "config.{k}: {car}");
    }

    let r = admin
        .patch("/api/v1/cars/c1", json!({ "version": 1, "name": "  Blue Y  ", "time_zone": "America/Chicago", "config": { "paint": "Deep blue", "right_hand_drive": false } }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let c = r.json();
    assert_eq!(
        (c["name"].clone(), c["time_zone"].clone(), c["version"].clone()),
        (json!("Blue Y"), json!("America/Chicago"), json!(2))
    );
    assert_eq!(c["config"]["paint"], json!({ "value": "Deep blue", "source": "owner", "sources": [] }));
    assert_eq!(c["config"]["right_hand_drive"], json!({ "value": false, "source": "owner", "sources": [] }));
    assert_eq!(admin.get("/api/v1/cars").await.json()["items"][0]["name"], json!("Blue Y"));
    // a stale version changes nothing and says the current one
    let r = admin.patch("/api/v1/cars/c1", json!({ "version": 1, "name": "Other" })).await;
    assert_eq!(
        (r.status, r.code(), r.json()["error"]["version"].clone()),
        (409, Some("stale_version".into()), json!(2))
    );
    // forgetting a typed value
    let r = admin.patch("/api/v1/cars/c1", json!({ "version": 2, "config": { "paint": null } })).await;
    assert_eq!(r.json()["config"]["paint"], json!({ "value": null, "source": null, "sources": [] }), "{}", r.text());
    // wrong types are 400, broken rules 422, each named
    let r = admin.patch("/api/v1/cars/c1", json!({ "version": 3, "name": 5, "colour": "x" })).await;
    assert_eq!(r.status, 400, "{}", r.text());
    assert!(r.json()["error"]["fields"]["name"].is_string() && r.json()["error"]["fields"]["colour"].is_string());
    let r = admin.patch("/api/v1/cars/c1", json!({ "version": 3, "time_zone": "Mars/Base", "name": "" })).await;
    assert_eq!((r.status, r.code()), (422, Some("invalid".into())), "{}", r.text());
    assert!(r.json()["error"]["fields"]["time_zone"].is_string() && r.json()["error"]["fields"]["name"].is_string());
    let r = admin.patch("/api/v1/cars/c1", json!({ "version": 3, "config": { "right_hand_drive": "yes" } })).await;
    assert!(r.json()["error"]["fields"]["config.right_hand_drive"].is_string(), "{}", r.text());
    assert_eq!(admin.patch("/api/v1/cars/c1", json!({ "name": "No version" })).await.status, 400);
    // no CSRF header: refused; an unknown car: 404
    let r = admin
        .request(
            "PATCH",
            "/api/v1/cars/c1",
            Some(br#"{"version":3,"name":"x"}"#),
            &[("X-Sentry-Mate", "0"), ("Content-Type", "application/json")],
        )
        .await;
    assert_eq!(r.code(), Some("csrf".into()));
    assert_eq!(admin.patch("/api/v1/cars/c9", json!({ "version": 1 })).await.code(), Some("no_such_car".into()));
    // recorded in the audit log, with the car
    let audit = admin.get("/api/v1/audit").await.json();
    assert!(
        audit["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["action"] == json!("car-changed") && a["car"] == json!("c1")),
        "{audit}"
    );

    // a watch account may read its car but not change it, and a hidden car is unknown to it
    let mut watch = f.s.watch(&mut admin, &[1]).await;
    let mine = watch.get("/api/v1/cars/c1").await.json();
    assert_eq!(mine["version"], json!(3));
    assert!(mine.get("keys").is_none() && mine.get("preferred_key").is_none(), "{mine}");
    assert_eq!(watch.patch("/api/v1/cars/c1", json!({ "version": 3, "name": "Mine" })).await.status, 403);
    // (a change is refused for the role before any car is looked at: a hidden car and an unknown one alike)
    let hidden = watch.patch("/api/v1/cars/c2", json!({ "version": 1, "name": "Mine" })).await;
    let unknown = watch.patch("/api/v1/cars/c9", json!({ "version": 1, "name": "Mine" })).await;
    assert_eq!((hidden.status, hidden.json()), (unknown.status, unknown.json()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cars_settings_keep_their_rules_and_the_consumption_factor_reaches_the_car() {
    let f = fleet().await;
    let mut admin = f.s.admin().await;
    let r = admin.get("/api/v1/cars/c2/settings").await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(
        r.json(),
        json!({ "car": "c2", "time_zone": "Europe/Amsterdam", "version": 1,
                "battery_chemistry": { "setting": "auto", "detected": null, "evidence": null },
                "free_supercharging": [], "capacity_new_kwh": null, "full_range_new_km": null,
                "warranty_threshold_pct": 70.0, "efficiency_wh_per_km_override": null, "software_version_typed": null,
                "import_gap_fill": null, "import_gap_fill_on": true, "pack_replaced_date": null, "pack_new_kwh": null,
                // what the car's build (a US Model 3 RWD) gives when nothing is typed
                "capacity_new_kwh_default": 57.5, "full_range_new_km_default": 438.0,
                "efficiency_wh_per_km_default": 131.3, "defaults_source": "vehicle_info" })
    );
    let r = admin
        .patch(
            "/api/v1/cars/c2/settings",
            json!({ "version": 1, "battery_chemistry": { "setting": "lfp" }, "capacity_new_kwh": 60.5,
                    "efficiency_wh_per_km_override": 140, "software_version_typed": " 2026.38.4 ",
                    "free_supercharging": [{ "from_date": "2026-03-01", "to_date": null }, { "from_date": "2025-01-01", "to_date": "2025-06-30" }] }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let s = r.json();
    assert_eq!(s["version"], json!(2));
    assert_eq!(s["battery_chemistry"]["setting"], json!("lfp"));
    assert_eq!((s["capacity_new_kwh"].clone(), s["software_version_typed"].clone()), (json!(60.5), json!("2026.38.4")));
    assert_eq!(s["free_supercharging"][0], json!({ "from_date": "2025-01-01", "to_date": "2025-06-30" }));
    assert_eq!(f.engine.db().car(2).unwrap().unwrap().efficiency_wh_km, Some(140.0), "the car's range figures use it");
    let r =
        admin.patch("/api/v1/cars/c2/settings", json!({ "version": 2, "efficiency_wh_per_km_override": null })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(f.engine.db().car(2).unwrap().unwrap().efficiency_wh_km, None, "back to what it was");
    // stale, rules, types
    let r = admin.patch("/api/v1/cars/c2/settings", json!({ "version": 1, "capacity_new_kwh": 70 })).await;
    assert_eq!((r.status, r.json()["error"]["version"].clone()), (409, json!(3)));
    let r = admin
        .patch(
            "/api/v1/cars/c2/settings",
            json!({ "version": 3, "capacity_new_kwh": 5, "warranty_threshold_pct": 99, "battery_chemistry": { "setting": "lead" },
                    "software_version_typed": "v12", "free_supercharging": [{ "from_date": "2025-02-01", "to_date": "2025-01-01" }] }),
        )
        .await;
    assert_eq!((r.status, r.code()), (422, Some("invalid".into())), "{}", r.text());
    let fields = r.json()["error"]["fields"].clone();
    for k in [
        "capacity_new_kwh",
        "warranty_threshold_pct",
        "battery_chemistry.setting",
        "software_version_typed",
        "free_supercharging.0.to_date",
    ] {
        assert!(fields[k].is_string(), "{k}: {fields}");
    }
    let r = admin
        .patch("/api/v1/cars/c2/settings", json!({ "version": 3, "free_supercharging": [{ "from_date": "2025-01-01", "to_date": null }, { "from_date": "2026-01-01" }] }))
        .await;
    assert!(r.json()["error"]["fields"]["free_supercharging"].is_string(), "overlap: {}", r.text());
    let r = admin
        .patch(
            "/api/v1/cars/c2/settings",
            json!({ "version": 3, "warranty_threshold_pct": null, "capacity_new_kwh": "60" }),
        )
        .await;
    assert_eq!(r.status, 400, "{}", r.text());
    // an unknown part of the chemistry is refused, named, like any unknown field
    let r = admin
        .patch(
            "/api/v1/cars/c2/settings",
            json!({ "version": 3, "battery_chemistry": { "setting": "auto", "extra": 1 } }),
        )
        .await;
    assert_eq!(r.status, 400, "{}", r.text());
    assert!(r.json()["error"]["fields"]["battery_chemistry.extra"].is_string(), "{}", r.text());
    assert_eq!(admin.get("/api/v1/cars/c2/settings").await.json()["version"], json!(3), "nothing changed");

    let mut watch = f.s.watch(&mut admin, &[2]).await;
    assert_eq!(watch.get("/api/v1/cars/c2/settings").await.json()["battery_chemistry"]["setting"], json!("lfp"));
    assert_eq!(
        watch.patch("/api/v1/cars/c2/settings", json!({ "version": 3, "capacity_new_kwh": 70 })).await.status,
        403
    );
    assert_eq!(watch.get("/api/v1/cars/c1/settings").await.code(), Some("no_such_car".into()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notices_say_what_the_files_say() {
    let dir = tempfile::tempdir().unwrap();
    let rec = dir.path().join("RECOVERY-KEY.txt");
    std::fs::write(&rec, "made up").unwrap();
    let sources = web::resources::notices::Sources { recovery_file: Some(rec.clone()), ..Default::default() };
    let s = TestServer::start_with(move |b| b.service(Arc::new(sources))).await;
    let mut admin = s.admin().await;
    assert_eq!(s.client().get("/api/v1/notices").await.status, 401);
    let r = admin.get("/api/v1/notices").await;
    assert_eq!(r.status, 200, "{}", r.text());
    let n = &r.json()["items"][0];
    assert_eq!(
        (n["code"].clone(), n["level"].clone(), n["fix_link"].clone()),
        (json!("recovery_key_unsaved"), json!("warning"), json!("/settings/server"))
    );
    assert!(n["message"].is_string() && n["since_ms"].is_u64(), "{n}");
    std::fs::remove_file(&rec).unwrap();
    assert_eq!(admin.get("/api/v1/notices").await.json(), json!({ "items": [], "next": null }));
    // the test server's own certificate (made by the server: valid for a very long time) is read, and says nothing
    let pem = std::fs::read_to_string(s.dir.path().join("data/tls/cert.pem")).unwrap();
    let after = web::resources::notices::not_after_ms(&pem).unwrap();
    assert!(after > s.app.now() + 3650 * 86_400_000, "{after}");
    let soon = web::resources::notices::notices(
        &web::resources::notices::Sources {
            certificate: Some(s.dir.path().join("data/tls/cert.pem")),
            ..Default::default()
        },
        after - 3 * 86_400_000,
    );
    assert_eq!(
        (soon[0].code, soon[0].detail.clone()),
        ("certificate_expiring", Some(json!({ "expires_ms": after, "days": 3 })))
    );
}

struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

async fn put(store: &ingest::Store, judge: &ingest::Judge, name: &str, bytes: &[u8]) {
    use sha2::Digest;
    let sha = hex::encode(sha2::Sha256::digest(bytes));
    let r = store.receive(name, bytes.to_vec().into(), 0, bytes.len() as u64, &sha).await.unwrap();
    assert!(r.complete, "{name}");
    judge.arrived(name).await.unwrap();
}

/// A made-up car's day of logs (synth: a made-up VIN, a public signing seed), uploaded and judged as an upload is,
/// whose newest log is not the file that was signed: it fails its check.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
async fn the_fleet_follows_the_logs_rules_of_health_and_the_time_logs_arrived() {
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
    let sc = synth::Scenario::new(5).days(1).rates(synth::Rates::Light);
    let logs: Vec<_> = sc.logs().collect();
    let sent_from = (accounts::system_clock())();
    for (i, log) in logs.iter().enumerate() {
        let mut bytes = log.bytes.clone();
        if i + 1 == logs.len() {
            bytes.push(0);
        }
        put(&store, &judge, &log.name, &bytes).await;
        put(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
    }
    engine.idle().await;
    let sent_to = (accounts::system_clock())();
    assert_eq!(engine.stats().failures, 0, "{:?}", engine.stats().last_error);
    let id = engine::query::all_cars(&engine).unwrap()[0];
    let car = format!("c{id}");
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    let mut admin = s.admin().await;

    // the list's times: when the newest state began (as the car's Now says) and when its newest log arrived (not when
    // it was decoded, nor when its data was recorded)
    let list = admin.get("/api/v1/cars").await.json();
    let item = &list["items"][0];
    let now = admin.get(&format!("/api/v1/cars/{car}/now")).await.json();
    assert!(item["now"]["since_ms"].is_u64(), "{item}");
    assert_eq!(item["now"]["since_ms"], now["since_ms"], "{item} {now}");
    let received = item["now"]["received_ms"].as_u64().unwrap();
    assert!((sent_from..=sent_to).contains(&received), "{received} not in {sent_from}..{sent_to}");
    // the made-up day is long past: silent by its data, with the real last upload said; its newest log failed
    let codes: Vec<&str> = item["attention"].as_array().unwrap().iter().map(|c| c.as_str().unwrap()).collect();
    assert!(codes.contains(&"upload_silence") && codes.contains(&"trust_failed"), "{item}");
    let a = admin.get("/api/v1/fleet/attention").await.json();
    let items = a["items"].as_array().unwrap();
    let silent = items.iter().find(|i| i["code"] == json!("upload_silence")).unwrap();
    assert_eq!(silent["detail"]["last_upload_ms"].as_u64(), Some(received), "{a}");
    assert!(silent["since_ms"].as_u64().unwrap() < sent_from, "since its newest data: {a}");
    // the car's health says the same, of the same log
    let h = admin.get(&format!("/api/v1/cars/{car}/health")).await.json();
    let reasons = h["reasons"].as_array().unwrap();
    assert!(reasons.contains(&json!("upload_silence")) && reasons.contains(&json!("trust_failed")), "{h}");
    assert_eq!(h["last_log_trust"]["result"], json!("failed"), "{h}");
    let failed = items.iter().find(|i| i["code"] == json!("trust_failed")).unwrap();
    assert_eq!(failed["detail"]["log_start_ms"], h["last_log_trust"]["log_start_ms"], "{a}");
    assert!(failed["detail"]["log_start_ms"].is_u64() && failed["since_ms"] == failed["detail"]["log_start_ms"]);
    assert!(failed["detail"].get("reason").is_some(), "{a}");
    // the car itself carries the same times
    let one = admin.get(&format!("/api/v1/cars/{car}")).await.json();
    assert_eq!(
        (one["now"]["received_ms"].as_u64(), one["now"]["since_ms"].clone()),
        (Some(received), now["since_ms"].clone())
    );
}

/// The preferred key changed (`PATCH /can-keys`) while an admin renames the car: neither change is lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_change_and_a_rename_at_once_both_stay() {
    let f = fleet().await;
    let mut c = f.engine.db().car(1).unwrap().unwrap();
    c.vehicle_keys = vec!["made-up-key-a".into(), "made-up-key-b".into()];
    f.engine.db().update_car(c).unwrap();
    let mut admin = f.s.admin().await;
    let e = f.engine.clone();
    let flipper = std::thread::spawn(move || {
        for i in 0..400 {
            let key = if i % 2 == 0 { "made-up-key-a" } else { "made-up-key-b" };
            e.set_preferred(key, true).unwrap();
        }
    });
    let mut version = 1;
    for i in 0..30 {
        let r = admin.patch("/api/v1/cars/c1", json!({ "version": version, "name": format!("Blue {i}") })).await;
        assert_eq!(r.status, 200, "{}", r.text());
        version = r.json()["version"].as_u64().unwrap();
    }
    flipper.join().unwrap();
    let c = f.engine.db().car(1).unwrap().unwrap();
    assert_eq!(c.name, "Blue 29", "the rename stays");
    assert_eq!(c.preferred_key.as_deref(), Some("made-up-key-b"), "the key change stays");
    assert_eq!(c.vehicle_keys.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_typed_model_and_year_reach_the_list_and_the_car() {
    let f = fleet().await;
    let mut admin = f.s.admin().await;
    assert_eq!(admin.get("/api/v1/cars/c1").await.json()["year"], json!(2024));
    let r = admin
        .patch("/api/v1/cars/c1", json!({ "version": 1, "config": { "model_year": 2025, "model": "Model Y L" } }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let c = admin.get("/api/v1/cars/c1").await.json();
    assert_eq!((c["year"].clone(), c["model"].clone()), (json!(2025), json!("Model Y L")), "{c}");
    assert_eq!(c["config"]["year"]["value"], json!(2025));
    assert_eq!(c["vehicle"]["model_year"]["value"], json!(2025));
    let list = admin.get("/api/v1/cars").await.json();
    let one = list["items"].as_array().unwrap().iter().find(|i| i["id"] == json!("c1")).unwrap().clone();
    assert_eq!((one["year"].clone(), one["model"].clone()), (json!(2025), json!("Model Y L")), "{one}");
    // the search finds it by the typed model
    let found = admin.get("/api/v1/cars?search=y%20l").await.json();
    assert_eq!(ids(&found), vec!["c1".to_string()], "{found}");
    // forgotten: the VIN's again
    let r =
        admin.patch("/api/v1/cars/c1", json!({ "version": 2, "config": { "model_year": null, "model": null } })).await;
    assert_eq!((r.json()["year"].clone(), r.json()["model"].clone()), (json!(2024), json!("Model Y")), "{}", r.text());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sold_car_keeps_its_history_but_raises_nothing_and_leaves_the_fleet_totals() {
    let f = fleet().await;
    let mut admin = f.s.admin().await;
    let r = admin.patch("/api/v1/cars/c3", json!({ "version": 1, "sold": true, "sold_date": "2026-03-14" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!((r.json()["sold"].clone(), r.json()["sold_date"].clone()), (json!(true), json!("2026-03-14")));
    let s = admin.get("/api/v1/fleet/summary").await.json();
    assert_eq!((s["cars"].clone(), s["sold"].clone()), (json!(2), json!(1)), "{s}");
    let a = admin.get("/api/v1/fleet/attention").await.json();
    assert!(a["items"].as_array().unwrap().iter().all(|i| i["car"] != json!("c3")), "{a}");
    // still listed, marked sold
    let list = admin.get("/api/v1/cars").await.json();
    let one = list["items"].as_array().unwrap().iter().find(|i| i["id"] == json!("c3")).unwrap().clone();
    assert_eq!((one["sold"].clone(), one["attention"].clone()), (json!(true), json!([])), "{one}");
    assert_eq!(admin.patch("/api/v1/cars/c3", json!({ "version": 2, "sold_date": "14 March" })).await.status, 422);
    // undone: the date goes with it
    let r = admin.patch("/api/v1/cars/c3", json!({ "version": 2, "sold": false })).await;
    assert_eq!((r.json()["sold"].clone(), r.json()["sold_date"].clone()), (json!(false), json!(null)), "{}", r.text());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_recommended_tyre_pressure_is_typed_else_the_cars_else_the_placards_and_a_low_tyre_is_said() {
    let f = fleet().await;
    let mut admin = f.s.admin().await;
    let kpa = |v: &Value| v["value"].as_f64();
    // c3 (a Model S, no wheel size known, nothing sent): no recommended pressure, so no tyre is judged
    let n = admin.get("/api/v1/cars/c3/now").await.json();
    assert_eq!(
        (n["tyre_recommended_kpa"].clone(), n["tyre_recommended_source"].clone(), n["tyre_low"].clone()),
        (json!(null), json!(null), json!(null)),
        "{n}"
    );
    assert_eq!(n["tyres_slots_mapped"], json!(false), "a Now that does not say is not mapped");

    // typed (kPa): beats everything; the sensors' wheels are not known, so each tyre is judged against the lower
    let body = json!({ "version": 1, "config": { "tyre_pressure_front_kpa": 290, "tyre_pressure_rear_kpa": 300 } });
    let r = admin.patch("/api/v1/cars/c3", body).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let typed = r.json()["config"]["tyre_pressure_front_kpa"].clone();
    assert_eq!((kpa(&typed), typed["source"].clone()), (Some(290.0), json!("owner")), "{typed}");
    let n = admin.get("/api/v1/cars/c3/now").await.json();
    let rec = (kpa(&n["tyre_recommended_kpa"]), kpa(&n["tyre_recommended_rear_kpa"]));
    assert_eq!(rec, (Some(290.0), Some(300.0)), "{n}");
    assert_eq!(n["tyre_recommended_source"], json!("owner"));
    assert_eq!(
        (n["tyre_recommended_kpa"]["at_ms"].clone(), n["tyre_recommended_kpa"]["sources"].clone()),
        (json!(null), json!([]))
    );
    assert_eq!(n["tyre_low"]["value"], json!(true), "200 kPa is under 90 % of 290: {n}");
    let a = admin.get("/api/v1/fleet/attention").await.json();
    let items = a["items"].as_array().cloned().unwrap();
    let low = items.iter().find(|i| i["code"] == json!("tyre_low")).cloned().unwrap();
    assert_eq!(low["car"], json!("c3"));
    let d = &low["detail"];
    assert_eq!(
        (d["tyre"].clone(), d["source"].clone()),
        (json!(null), json!("owner")),
        "not mapped: which tyre is not known"
    );
    assert_eq!((d["pressure_kpa"].as_f64(), d["recommended_kpa"].as_f64()), (Some(200.0), Some(290.0)), "{low}");
    // out of range, refused
    let r = admin.patch("/api/v1/cars/c3", json!({ "version": 2, "config": { "tyre_pressure_rear_kpa": 50 } })).await;
    assert_eq!(r.status, 422, "{}", r.text());

    // c1: the car sends 2.9 front, 3.1 rear (CAN) and its sensors' wheels are known: the rear right at 2.7 bar is
    // more than 10 % under the rear's
    let mut c1 = now_of(1, (accounts::system_clock())() - 5_000, State::Online, true, 64.0, false);
    c1.sample.tpms_bar = [Some(2.9), Some(2.9), Some(3.1), Some(2.7)];
    c1.sample.extra.tire_recommended_front_bar = Some(2.9);
    c1.sample.extra.tire_recommended_rear_bar = Some(3.1);
    c1.sample.extra.tires_mapped = Some(true);
    f.engine.db().set_now(c1.clone()).unwrap();
    let n = admin.get("/api/v1/cars/c1/now").await.json();
    let rec = (kpa(&n["tyre_recommended_kpa"]), kpa(&n["tyre_recommended_rear_kpa"]));
    assert_eq!(rec, (Some(290.0), Some(310.0)), "{n}");
    assert_eq!((n["tyre_recommended_source"].clone(), n["tyres_slots_mapped"].clone()), (json!("can"), json!(true)));
    assert!(n["tyre_recommended_kpa"]["at_ms"].is_u64(), "a value read from the car: {n}");
    assert_eq!(n["tyre_low"]["value"], json!(true));
    let a = admin.get("/api/v1/fleet/attention").await.json();
    let items = a["items"].as_array().cloned().unwrap();
    let low = items.iter().find(|i| i["code"] == json!("tyre_low") && i["car"] == json!("c1")).cloned();
    assert_eq!(low.map(|l| l["detail"]["tyre"].clone()), Some(json!("rr")));
    // the same tyres not mapped: judged against the lower (2.9 bar), none is low (no false alarm)
    c1.sample.extra.tires_mapped = Some(false);
    f.engine.db().set_now(c1).unwrap();
    let n = admin.get("/api/v1/cars/c1/now").await.json();
    assert_eq!((n["tyre_low"]["value"].clone(), n["tyres_slots_mapped"].clone()), (json!(false), json!(false)), "{n}");

    // c2 (a Model 3 of 2022): nothing typed or sent; its wheel size typed: the door placard's for 18 in
    let version = admin.get("/api/v1/cars/c2").await.json()["version"].clone();
    let r = admin.patch("/api/v1/cars/c2", json!({ "version": version, "config": { "wheel_in": 18 } })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let n = admin.get("/api/v1/cars/c2/now").await.json();
    let rec = (kpa(&n["tyre_recommended_kpa"]), kpa(&n["tyre_recommended_rear_kpa"]));
    assert_eq!(rec, (Some(290.0), Some(290.0)), "{n}");
    assert_eq!(
        (n["tyre_recommended_source"].clone(), n["tyre_recommended_wheel_in"].clone()),
        (json!("placard"), json!(18))
    );
    assert_eq!(n["tyre_low"]["value"], json!(false));
}
