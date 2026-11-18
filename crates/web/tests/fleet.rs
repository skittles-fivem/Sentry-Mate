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
        // "XP7" is no maker's code: a made-up VIN prefix
        vin: Some(VinInfo {
            vin_prefix: format!("XP7{model}{serial}"),
            model: Some(model),
            year: Some(year),
            plant: Some('F'),
            via: 0x405,
        }),
        profile_setting: None,
        time_zone: "Europe/Amsterdam".into(),
        efficiency_wh_km: None,
        created_ms: 1_790_000_000_000,
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
    Now { car, at_ms, state: Some(state), driving, sample, place: None }
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
    assert_eq!(ids(&admin.get("/api/v1/cars?search=xp7s").await.json()), ["c3"]);
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
        json!({ "cars": 1, "by_state": { "driving": 0, "charging": 1, "parked": 0, "asleep": 0, "unknown": 0 }, "stale": 0 })
    );
    assert_eq!(watch.get("/api/v1/fleet/attention").await.json()["items"], json!([]));
    drop(f.engine);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_fleet_summary_and_attention_count_every_car() {
    let f = fleet().await;
    let mut admin = f.s.admin().await;
    let s = admin.get("/api/v1/fleet/summary").await.json();
    assert_eq!(
        s,
        json!({ "cars": 3, "by_state": { "driving": 1, "charging": 1, "parked": 0, "asleep": 1, "unknown": 0 }, "stale": 1 })
    );
    let a = admin.get("/api/v1/fleet/attention").await;
    assert_eq!(a.status, 200, "{}", a.text());
    let items = a.json()["items"].as_array().cloned().unwrap();
    let codes: Vec<&str> = items.iter().map(|i| i["code"].as_str().unwrap()).collect();
    for want in ["upload_silence", "low_battery", "tyre_low"] {
        assert!(codes.contains(&want), "{want}: {items:?}");
    }
    assert!(items.iter().all(|i| i["car"] == json!("c3") && i["since_ms"].is_u64() && i["detail"].is_object()));
    let tyre = items.iter().find(|i| i["code"] == json!("tyre_low")).unwrap();
    assert_eq!((tyre["detail"]["tyre"].clone(), tyre["detail"]["pressure_kpa"].clone()), (json!("rl"), json!(200.0)));
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
    assert_eq!(car["config"]["model"], json!({ "value": "Model Y", "source": "vin" }));
    assert_eq!(car["config"]["paint"], json!({ "value": null, "source": null }));
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
    assert_eq!(c["config"]["paint"], json!({ "value": "Deep blue", "source": "owner" }));
    assert_eq!(c["config"]["right_hand_drive"], json!({ "value": false, "source": "owner" }));
    assert_eq!(admin.get("/api/v1/cars").await.json()["items"][0]["name"], json!("Blue Y"));
    // a stale version changes nothing and says the current one
    let r = admin.patch("/api/v1/cars/c1", json!({ "version": 1, "name": "Other" })).await;
    assert_eq!(
        (r.status, r.code(), r.json()["error"]["version"].clone()),
        (409, Some("stale_version".into()), json!(2))
    );
    // forgetting a typed value
    let r = admin.patch("/api/v1/cars/c1", json!({ "version": 2, "config": { "paint": null } })).await;
    assert_eq!(r.json()["config"]["paint"], json!({ "value": null, "source": null }), "{}", r.text());
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
    assert_eq!(watch.get("/api/v1/cars/c1").await.json()["version"], json!(3));
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
                "warranty_threshold_pct": 70.0, "efficiency_wh_per_km_override": null, "software_version_typed": null })
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
