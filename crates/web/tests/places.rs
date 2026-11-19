#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Places through the real router (web::testkit): the list with its search, kinds, sorts, paging and one car's
//! counts, one place, create, change and delete with their effect, the preview, repricing, suggestions, the place at a
//! point, the maps and address answers; roles (a watch account sees only the places its cars used, may preview but not
//! change), CSRF and field errors. Over an engine that decoded two made-up cars' days of logs (synth: made-up VINs, a
//! made-up town in the open sea, public signing seeds), stored and judged the way an upload is.

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
    store: ingest::Store,
    judge: ingest::Judge,
    _dir: tempfile::TempDir,
}

const SATURDAY_MS: u64 = synth::DEFAULT_START_MS + 5 * 86_400_000;

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

/// Two made-up cars, two light days each (home charging at night; the first also drives to a supercharger), no
/// places yet.
async fn seeded() -> Seeded {
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
    for n in 0..2 {
        let sc = synth::Scenario::new(11)
            .car(n)
            .start(SATURDAY_MS)
            .days(2)
            .rates(synth::Rates::Light)
            .supercharger_trip(n == 0);
        for log in sc.logs() {
            put(&store, &judge, &log.name, &log.bytes).await;
            put(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
        }
    }
    engine.idle().await;
    let ids = engine::query::all_cars(&engine).unwrap();
    assert_eq!(ids.len(), 2, "{ids:?}");
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    Seeded { s, _engine: engine, cars: ids.iter().map(|c| format!("c{c}")).collect(), ids, store, judge, _dir: dir }
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

const PLACE_KEYS: &[&str] = &[
    "id",
    "name",
    "kind",
    "lat",
    "lon",
    "radius_m",
    "time_zone",
    "price",
    "counts",
    "last_visit_ms",
    "address",
    "created_ms",
    "version",
];

const EFFECT_KEYS: &[&str] = &[
    "drives_named",
    "drives_unnamed",
    "charges_named",
    "charges_unnamed",
    "drives_taken",
    "charges_taken",
    "taken_from",
    "charges_repriced",
    "examples",
    "example_rows",
];

fn has_keys(v: &Value, keys: &[&str]) {
    for k in keys {
        assert!(v.get(*k).is_some(), "{k}: {v}");
    }
}

/// Every charge of a car (one page is enough here).
async fn charges(c: &mut Client, car: &str) -> Vec<Value> {
    ok(c, &format!("/api/v1/cars/{car}/charges?limit=500")).await["items"].as_array().unwrap().clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn places_crud_preview_reprice_suggestions_and_at_over_decoded_days() {
    let k = seeded().await;
    let (a, b) = (k.cars[0].clone(), k.cars[1].clone());
    let mut admin = k.s.admin().await;

    // no place yet: an empty list, and suggestions where the cars park (home first: both cars sleep there)
    let l = ok(&mut admin, "/api/v1/places").await;
    assert_eq!((l["items"].as_array().unwrap().len(), l["total"].as_u64(), &l["next"]), (0, Some(0), &Value::Null));
    let s = ok(&mut admin, "/api/v1/places/suggestions?limit=5").await;
    let sug = s["items"].as_array().unwrap().clone();
    assert!(!sug.is_empty() && sug.len() <= 5, "{s}");
    assert_eq!(s["next"], Value::Null);
    for x in &sug {
        has_keys(
            x,
            &["lat", "lon", "radius_m", "visits", "dwell_ms", "cars", "label", "address", "last_visit_ms", "kind"],
        );
        assert!(x["visits"].as_u64().unwrap() >= 2, "{x}");
    }
    let visits: Vec<u64> = sug.iter().map(|x| x["visits"].as_u64().unwrap()).collect();
    assert!(visits.windows(2).all(|w| w[0] >= w[1]), "most visited first: {visits:?}");
    let home_spot = &sug[0];
    let cars_there: Vec<&str> = home_spot["cars"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(cars_there.contains(&a.as_str()) && cars_there.contains(&b.as_str()), "{home_spot}");
    assert!(home_spot["dwell_ms"].as_u64().unwrap() > 3_600_000, "a night at home: {home_spot}");
    assert_eq!(code(&mut admin, "/api/v1/places/suggestions?limit=0").await.0, 400);

    // before: charges at home have no place
    let before: Vec<Value> = charges(&mut admin, &a).await;
    assert!(!before.is_empty());
    assert!(before.iter().all(|c| c["place"].is_null()), "{before:?}");

    // the preview of a new home: what would carry its name; nothing changes
    let draft = json!({ "name": "Made-up home", "kind": "home", "lat": synth::TOWN.lat, "lon": synth::TOWN.lon,
                        "radius_m": 300, "price": { "currency": "EUR", "per_kwh": 0.25 } });
    let r = admin.post("/api/v1/places/preview", draft.clone()).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let pv = r.json();
    has_keys(&pv, EFFECT_KEYS);
    assert_eq!((&pv["place"], &pv["delete"]), (&Value::Null, &json!(false)));
    assert!(pv["drives_named"].as_u64().unwrap() > 0 && pv["charges_named"].as_u64().unwrap() > 0, "{pv}");
    assert_eq!((pv["drives_taken"].as_u64(), pv["drives_unnamed"].as_u64()), (Some(0), Some(0)));
    assert!(pv["example_rows"].as_array().unwrap().len() <= 8);
    for x in pv["example_rows"].as_array().unwrap() {
        has_keys(x, &["kind", "id", "car", "start_ms", "change"]);
        assert_eq!(x["change"], "named");
    }
    assert_eq!(ok(&mut admin, "/api/v1/places").await["total"], 0, "a preview changes nothing");

    // field errors: types are 400, rules 422, each by field
    let r = admin.post("/api/v1/places", json!({ "name": 5, "lat": "north", "colour": "red" })).await;
    assert_eq!((r.status, r.code().as_deref()), (400, Some("bad_input")), "{}", r.text());
    let f = &r.json()["error"]["fields"];
    for key in ["name", "lat", "colour", "lon"] {
        assert!(f.get(key).is_some(), "{key}: {f}");
    }
    let r = admin
        .post(
            "/api/v1/places",
            json!({ "name": "x", "lat": 1.0, "lon": 1.0, "radius_m": 5, "kind": "castle", "time_zone": "Mars/Base",
                    "price": { "currency": "EUR" } }),
        )
        .await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")), "{}", r.text());
    let f = &r.json()["error"]["fields"];
    for key in ["radius_m", "kind", "time_zone", "price"] {
        assert!(f.get(key).is_some(), "{key}: {f}");
    }
    // CSRF on a change
    let body = serde_json::to_vec(&draft).unwrap();
    let r = admin
        .request("POST", "/api/v1/places", Some(&body), &[("Content-Type", "application/json"), ("X-Sentry-Mate", "0")])
        .await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("csrf")));

    // create: 201, the place with its counts and the same effect the preview gave
    let r = admin.post("/api/v1/places", draft.clone()).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let home = r.json();
    has_keys(&home, PLACE_KEYS);
    has_keys(&home["effect"], EFFECT_KEYS);
    assert_eq!(home["effect"]["drives_named"], pv["drives_named"]);
    assert_eq!(home["effect"]["charges_named"], pv["charges_named"]);
    let hid = home["id"].as_str().unwrap().to_string();
    assert!(hid.starts_with('p'));
    assert_eq!(
        (home["kind"].as_str(), home["radius_m"].as_f64(), home["version"].as_u64()),
        (Some("home"), Some(300.0), Some(1))
    );
    assert_eq!(home["price"]["currency"], "EUR");
    assert_eq!(home["price"]["per_kwh"], 0.25);
    assert_eq!(home["price"]["rates"], json!([]));
    assert!(
        home["counts"]["charges"].as_u64().unwrap() > 0 && home["counts"]["visits"].as_u64().unwrap() > 0,
        "{home}"
    );
    assert!(home["last_visit_ms"].as_u64().is_some() && home["created_ms"].as_u64().is_some());

    // matching is live: the charges at home carry its name at once
    let after: Vec<Value> = charges(&mut admin, &a).await;
    let at_home: Vec<&Value> = after.iter().filter(|c| c["place"]["id"] == json!(hid)).collect();
    assert!(!at_home.is_empty(), "{after:?}");
    assert!(at_home.iter().all(|c| c["place"]["name"] == "Made-up home"));
    // ...but costs stay put: the ended charges had no cost (no place) and keep none until a reprice
    let ended_home: Vec<&&Value> = at_home.iter().filter(|c| !c["end_ms"].is_null()).collect();
    assert!(!ended_home.is_empty());
    assert!(ended_home.iter().all(|c| c["cost"].is_null()), "{ended_home:?}");
    let r = admin.post(&format!("/api/v1/places/{hid}/reprice"), json!({ "only_without_cost": true })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.json()["repriced"].as_u64().unwrap() >= ended_home.len() as u64, "{}", r.text());
    assert!(
        charges(&mut admin, &a)
            .await
            .iter()
            .filter(|c| c["place"]["id"] == json!(hid))
            .all(|c| c["cost"]["currency"] == "EUR")
    );
    // ...and the place at a point says so, by the nearest-centre rule
    let at = ok(&mut admin, &format!("/api/v1/places/at?lat={}&lon={}", synth::TOWN.lat, synth::TOWN.lon)).await;
    assert_eq!((&at["place"]["id"], &at["rule"]), (&json!(hid), &json!("nearest_centre")));
    assert_eq!(at["candidates"][0]["id"], json!(hid));
    has_keys(&at, &["lat", "lon", "place", "candidates", "rule", "address"]);
    let far = ok(&mut admin, "/api/v1/places/at?lat=-45&lon=100").await;
    assert_eq!((&far["place"], far["candidates"].as_array().unwrap().len()), (&Value::Null, 0));
    assert_eq!(code(&mut admin, "/api/v1/places/at?lat=91&lon=0").await.0, 400);
    assert_eq!(code(&mut admin, "/api/v1/places/at?lat=1").await.0, 400);
    // no suggestion at a saved place any more
    let s2 = ok(&mut admin, "/api/v1/places/suggestions").await;
    for x in s2["items"].as_array().unwrap() {
        let d = ((x["lat"].as_f64().unwrap() - synth::TOWN.lat).powi(2)
            + (x["lon"].as_f64().unwrap() - synth::TOWN.lon).powi(2))
        .sqrt();
        assert!(d > 0.002, "home is saved: {x}");
    }

    // a second place from the next suggestion, for the list's sorts and paging
    let spot = &s2["items"][0];
    let r = admin
        .post("/api/v1/places", json!({ "name": "a shop", "lat": spot["lat"], "lon": spot["lon"], "kind": "service" }))
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let shop = r.json();
    assert_eq!((shop["kind"].as_str(), shop["radius_m"].as_f64()), (Some("service"), Some(100.0)));
    let sid = shop["id"].as_str().unwrap().to_string();
    let l = ok(&mut admin, "/api/v1/places").await;
    let names: Vec<&str> = l["items"].as_array().unwrap().iter().filter_map(|p| p["name"].as_str()).collect();
    assert_eq!((names, l["total"].as_u64()), (vec!["a shop", "Made-up home"], Some(2)), "by name, any case");
    let p1 = ok(&mut admin, "/api/v1/places?limit=1&sort=visits").await;
    assert_eq!(p1["items"].as_array().unwrap().len(), 1);
    let next = p1["next"].as_str().unwrap().to_string();
    let p2 = ok(&mut admin, &format!("/api/v1/places?limit=1&sort=visits&after={next}")).await;
    assert_ne!(p1["items"][0]["id"], p2["items"][0]["id"]);
    assert_eq!(p2["next"], Value::Null);
    assert!(p1["items"][0]["counts"]["visits"].as_u64() >= p2["items"][0]["counts"]["visits"].as_u64());
    assert_eq!(code(&mut admin, &format!("/api/v1/places?limit=1&sort=name&after={next}")).await.0, 400);
    for sort in ["name", "visits", "drives", "last_visit", "created"] {
        assert_eq!(ok(&mut admin, &format!("/api/v1/places?sort={sort}")).await["total"], 2, "{sort}");
    }
    assert_eq!(code(&mut admin, "/api/v1/places?sort=colour").await.0, 400);
    let found = ok(&mut admin, "/api/v1/places?search=HOME").await;
    assert_eq!(found["items"].as_array().unwrap().len(), 1);
    let kinds = ok(&mut admin, "/api/v1/places?kind=service").await;
    assert_eq!(kinds["items"][0]["id"], json!(sid));
    assert_eq!(code(&mut admin, "/api/v1/places?kind=castle").await.0, 400);
    // one car's counts; every place still listed; a car that isn't is 404
    let ca = ok(&mut admin, &format!("/api/v1/places?car={a}&sort=drives")).await;
    assert_eq!(ca["total"], 2);
    let all_home = ok(&mut admin, &format!("/api/v1/places/{hid}")).await;
    let a_home = ca["items"].as_array().unwrap().iter().find(|p| p["id"] == json!(hid)).unwrap().clone();
    assert!(a_home["counts"]["visits"].as_u64() < all_home["counts"]["visits"].as_u64(), "{a_home} {all_home}");
    assert_eq!(code(&mut admin, "/api/v1/places?car=c999").await, (404, Some("no_such_car".into())));
    // CSV
    let r = admin.get("/api/v1/places?format=csv").await;
    assert_eq!(r.status, 200);
    assert!(r.text().lines().count() >= 3, "{}", r.text());

    // change: needs the version; a stale one is 409 with the version it has
    let r = admin.patch(&format!("/api/v1/places/{hid}"), json!({ "name": "Home" })).await;
    assert_eq!(r.status, 400, "{}", r.text());
    let r = admin.patch(&format!("/api/v1/places/{hid}"), json!({ "name": "Home", "version": 7 })).await;
    assert_eq!((r.status, r.code().as_deref()), (409, Some("stale_version")));
    assert_eq!(r.json()["error"]["version"], 1);
    // a rename moves nothing, and every row shows the new name
    let r = admin.patch(&format!("/api/v1/places/{hid}"), json!({ "name": "Home", "version": 1 })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let renamed = r.json();
    assert_eq!((renamed["name"].as_str(), renamed["version"].as_u64()), (Some("Home"), Some(2)));
    assert_eq!(
        (renamed["effect"]["drives_named"].as_u64(), renamed["effect"]["charges_named"].as_u64()),
        (Some(0), Some(0))
    );
    assert!(
        charges(&mut admin, &a)
            .await
            .iter()
            .filter(|c| c["place"]["id"] == json!(hid))
            .all(|c| c["place"]["name"] == "Home")
    );

    // costs stay put: a new price changes no ended charge, the preview counts what a reprice would change
    let costs_before: Vec<Value> = charges(&mut admin, &a)
        .await
        .iter()
        .filter(|c| c["place"]["id"] == json!(hid))
        .map(|c| c["cost"].clone())
        .collect();
    assert!(costs_before.iter().any(|c| c["currency"] == "EUR"), "{costs_before:?}");
    let new_price = json!({ "currency": "EUR", "per_kwh": 0.5, "rates": [
        { "name": "Night", "per_kwh": 0.1, "days": "all", "from": "00:00", "to": "06:00" }], "session_fee": 1.0 });
    let pv = admin.post("/api/v1/places/preview", json!({ "id": hid, "price": new_price })).await.json();
    assert_eq!(pv["place"], json!(hid));
    let would = pv["charges_repriced"].as_u64().unwrap();
    assert!(would > 0, "{pv}");
    let r = admin.patch(&format!("/api/v1/places/{hid}"), json!({ "price": new_price, "version": 2 })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["price"]["rates"][0]["to"], "06:00");
    let costs_now: Vec<Value> = charges(&mut admin, &a)
        .await
        .iter()
        .filter(|c| c["place"]["id"] == json!(hid))
        .map(|c| c["cost"].clone())
        .collect();
    assert_eq!(
        costs_now.iter().map(|c| &c["amount"]).collect::<Vec<_>>(),
        costs_before.iter().map(|c| &c["amount"]).collect::<Vec<_>>()
    );
    // reprice: a dry run changes nothing; the real one applies today's price
    let dry = admin.post(&format!("/api/v1/places/{hid}/reprice"), json!({ "dry_run": true })).await;
    assert_eq!(dry.status, 200, "{}", dry.text());
    let dry = dry.json();
    has_keys(
        &dry,
        &[
            "place",
            "from_ms",
            "to_ms",
            "only_without_cost",
            "dry_run",
            "charges",
            "repriced",
            "unchanged",
            "examples",
            "example_rows",
            "cost_before",
            "cost_after",
        ],
    );
    assert_eq!(dry["dry_run"], true);
    let repriced = dry["repriced"].as_u64().unwrap();
    assert!(repriced > 0, "{dry}");
    assert_eq!(dry["charges"].as_u64(), Some(repriced + dry["unchanged"].as_u64().unwrap()));
    for x in dry["example_rows"].as_array().unwrap() {
        assert_eq!((&x["kind"], &x["change"]), (&json!("charge"), &json!("repriced")));
    }
    let costs_dry: Vec<Value> = charges(&mut admin, &a)
        .await
        .iter()
        .filter(|c| c["place"]["id"] == json!(hid))
        .map(|c| c["cost"]["amount"].clone())
        .collect();
    assert_eq!(costs_dry, costs_before.iter().map(|c| c["amount"].clone()).collect::<Vec<_>>());
    let real = admin.post(&format!("/api/v1/places/{hid}/reprice"), json!({})).await.json();
    assert_eq!(real["repriced"].as_u64(), Some(repriced));
    let again = admin.post(&format!("/api/v1/places/{hid}/reprice"), json!({ "dry_run": true })).await.json();
    assert_eq!(again["repriced"], 0, "{again}");
    let only = admin
        .post(&format!("/api/v1/places/{hid}/reprice"), json!({ "only_without_cost": true, "dry_run": true }))
        .await
        .json();
    assert_eq!(only["charges"], 0, "every charge there has a cost: {only}");
    let r = admin.post(&format!("/api/v1/places/{hid}/reprice"), json!({ "from_ms": 5, "to_ms": 5 })).await;
    assert_eq!(r.status, 422);
    let r = admin.post(&format!("/api/v1/places/{hid}/reprice"), json!({ "dry_run": "yes" })).await;
    assert_eq!(r.status, 400);
    let r = admin.post("/api/v1/places/p999/reprice", json!({})).await;
    assert_eq!((r.status, r.code().as_deref()), (404, Some("no_such_place")));

    // a watch account of car b: sees only the places b used, may preview them, changes nothing
    let mut watch = k.s.watch(&mut admin, &[k.ids[1]]).await;
    let wl = ok(&mut watch, "/api/v1/places").await;
    let seen: Vec<&str> = wl["items"].as_array().unwrap().iter().filter_map(|p| p["id"].as_str()).collect();
    assert!(seen.contains(&hid.as_str()), "{wl}");
    let w_home = wl["items"].as_array().unwrap().iter().find(|p| p["id"] == json!(hid)).unwrap();
    let b_home = ok(&mut admin, &format!("/api/v1/places?car={b}")).await["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == json!(hid))
        .unwrap()
        .clone();
    assert_eq!(w_home["counts"], b_home["counts"], "counts over its own cars");
    assert_eq!(code(&mut watch, &format!("/api/v1/places?car={a}")).await, (404, Some("no_such_car".into())));
    let used_by_b = seen.contains(&sid.as_str());
    if !used_by_b {
        assert_eq!(code(&mut watch, &format!("/api/v1/places/{sid}")).await, (404, Some("no_such_place".into())));
        let r = watch.post("/api/v1/places/preview", json!({ "place": sid, "delete": true })).await;
        assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")));
        assert!(r.json()["error"]["fields"].get("place").is_some());
    }
    let r = watch.post("/api/v1/places/preview", json!({ "place": hid, "delete": true })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["delete"], true);
    // a preview changes nothing, so it writes nothing to the audit log: a watch account sending it again and again
    // can't flood the admins' log
    let before = ok(&mut admin, "/api/v1/audit?limit=500").await["items"].as_array().unwrap().len();
    for _ in 0..5 {
        let r = watch.post("/api/v1/places/preview", json!({ "place": hid, "delete": true })).await;
        assert_eq!(r.status, 200, "{}", r.text());
    }
    let audit = ok(&mut admin, "/api/v1/audit?limit=500").await;
    let items = audit["items"].as_array().unwrap();
    assert_eq!(items.len(), before, "{audit}");
    assert!(!items.iter().any(|x| x["summary"].as_str().is_some_and(|s| s.contains("/places/preview"))), "{audit}");
    for (m, path) in [
        ("POST", "/api/v1/places".to_string()),
        ("PATCH", format!("/api/v1/places/{hid}")),
        ("DELETE", format!("/api/v1/places/{hid}")),
        ("POST", format!("/api/v1/places/{hid}/reprice")),
    ] {
        let r = watch.request(m, &path, Some(b"{}"), &[("Content-Type", "application/json")]).await;
        assert_eq!(r.status, 403, "{m} {path}: {}", r.text());
    }
    let wat = ok(&mut watch, &format!("/api/v1/places/at?lat={}&lon={}", synth::TOWN.lat, synth::TOWN.lon)).await;
    assert_eq!(wat["place"]["id"], json!(hid));

    // delete: the effect, then the charges show no place again; a stale version is 409
    let r = admin.request("DELETE", &format!("/api/v1/places/{hid}?version=1"), None, &[]).await;
    assert_eq!(r.status, 409, "{}", r.text());
    let pv = admin.post("/api/v1/places/preview", json!({ "place": hid, "delete": true })).await.json();
    let r = admin.request("DELETE", &format!("/api/v1/places/{hid}?version=3"), None, &[]).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let d = r.json();
    assert_eq!((&d["deleted"], &d["id"]), (&json!(true), &json!(hid)));
    assert!(d.get("name").is_none());
    assert_eq!(d["effect"]["drives_unnamed"], pv["drives_unnamed"]);
    assert!(d["effect"]["charges_unnamed"].as_u64().unwrap() > 0);
    assert!(charges(&mut admin, &a).await.iter().all(|c| c["place"]["id"] != json!(hid)));
    assert_eq!(code(&mut admin, &format!("/api/v1/places/{hid}")).await, (404, Some("no_such_place".into())));
    assert_eq!(code(&mut admin, "/api/v1/places/nonsense").await, (404, Some("no_such_place".into())));
    let r = admin.post("/api/v1/places/preview", json!({ "place": hid, "delete": true })).await;
    assert_eq!(r.status, 422);
    // the costs a charge had stay with it after its place is gone
    let kept: Vec<Value> = charges(&mut admin, &a)
        .await
        .iter()
        .filter(|c| c["cost"]["currency"] == "EUR")
        .map(|c| c["id"].clone())
        .collect();
    assert!(!kept.is_empty());

    // the audit log has the changes
    let audit = ok(&mut admin, "/api/v1/audit?limit=100").await;
    let actions: Vec<&str> = audit["items"].as_array().unwrap().iter().filter_map(|x| x["action"].as_str()).collect();
    for want in ["place-created", "place-changed", "place-repriced", "place-deleted"] {
        assert!(actions.contains(&want), "{want}: {actions:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn maps_and_addresses_say_what_the_server_has() {
    let s = TestServer::start(&[]).await;
    let mut admin = s.admin().await;
    // without the engine, the places routes are not ready; maps and addresses answer the default settings
    assert_eq!(admin.get("/api/v1/places").await.status, 503);
    let m = ok(&mut admin, "/api/v1/maps/config").await;
    // a fresh server sends no browser to an outside tile server: offline until an admin turns online tiles on
    assert_eq!(m["tiles"]["mode"], "offline", "{m}");
    assert_eq!(m["tiles"]["online_template"], Value::Null, "{m}");
    has_keys(&m["tiles"], &["mode", "offline_url", "online_template", "attribution", "max_zoom"]);
    has_keys(&m["geocoding"], &["mode", "credit", "offline_data_present"]);
    let g = ok(&mut admin, "/api/v1/geocode/status").await;
    has_keys(&g, &["mode", "offline_data", "cache_size", "online"]);
    assert_eq!(g["offline_data"]["present"], false);
    let mut anon = s.client();
    assert_eq!(anon.get("/api/v1/maps/config").await.status, 401);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn maps_and_addresses_follow_the_settings_at_once() {
    let k = seeded().await;
    let mut admin = k.s.admin().await;
    let m = ok(&mut admin, "/api/v1/maps/config").await;
    assert_eq!((m["tiles"]["mode"].as_str(), m["tiles"]["max_zoom"].as_u64()), (Some("offline"), Some(19)), "{m}");
    assert_eq!(m["tiles"]["online_template"], Value::Null, "{m}");
    // the pages' policy names no outside host while tiles are offline
    let csp = |r: &web::testkit::Res| r.header("content-security-policy").unwrap().to_string();
    let before = csp(&admin.get("/").await);
    assert!(!before.contains("https://"), "{before}");
    assert_eq!((m["geocoding"]["mode"].as_str(), m["geocoding"]["language"].as_str()), (Some("offline"), Some("en")));
    let v = ok(&mut admin, "/api/v1/settings").await["version"].as_u64().unwrap();
    let tiles = json!({ "mode": "online", "online_url": "https://tiles.example.org/{z}/{x}/{y}.png",
                        "attribution": "Example tiles", "max_zoom": 17 });
    let r = admin.patch("/api/v1/settings", json!({ "version": v, "tiles": tiles })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let m = ok(&mut admin, "/api/v1/maps/config").await;
    assert_eq!(
        (&m["tiles"]["mode"], &m["tiles"]["online_template"], &m["tiles"]["attribution"], &m["tiles"]["max_zoom"]),
        (&json!("online"), &json!("https://tiles.example.org/{z}/{x}/{y}.png"), &json!("Example tiles"), &json!(17)),
        "{m}"
    );
    let on = csp(&admin.get("/").await);
    assert!(on.contains("https://tiles.example.org") && !on.contains("openstreetmap"), "{on}");
    // offline tiles: none served yet, so no address for them, and nothing online
    let r = admin.patch("/api/v1/settings", json!({ "version": v + 1, "tiles": { "mode": "offline" } })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let m = ok(&mut admin, "/api/v1/maps/config").await;
    assert_eq!(
        (&m["tiles"]["mode"], &m["tiles"]["offline_url"], &m["tiles"]["online_template"]),
        (&json!("offline"), &Value::Null, &Value::Null)
    );
    let off = csp(&admin.get("/").await);
    assert!(!off.contains("https://"), "{off}");

    // addresses: the mode, service, language and rounding saved
    let g = ok(&mut admin, "/api/v1/geocode/status").await;
    assert_eq!(
        (&g["mode"], &g["online"]["enabled"], &g["online"]["url"]),
        (&json!("offline"), &json!(false), &Value::Null)
    );
    let geo = json!({ "mode": "online", "url": "https://geo.example.org/reverse", "language": "de", "rounding_m": 50 });
    let r = admin.patch("/api/v1/settings", json!({ "version": v + 2, "geocode": geo })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let g = ok(&mut admin, "/api/v1/geocode/status").await;
    assert_eq!(
        (&g["mode"], &g["online"]["enabled"], &g["online"]["url"], &g["rounding_m"]),
        (&json!("online"), &json!(true), &json!("https://geo.example.org/reverse"), &json!(50)),
        "{g}"
    );
    assert_eq!((&g["cache_size"], &g["online"]["last_error"]), (&json!(0), &Value::Null));
    let m = ok(&mut admin, "/api/v1/maps/config").await;
    assert_eq!((&m["geocoding"]["mode"], &m["geocoding"]["language"]), (&json!("online"), &json!("de")));
    let h = ok(&mut admin, "/api/v1/system/health").await;
    assert_eq!(h["geocoder"]["mode"], g["mode"], "the health page's two sources agree: {h}");
    // a watch account reads them too
    let mut watch = k.s.watch(&mut admin, &[k.ids[0]]).await;
    assert_eq!(ok(&mut watch, "/api/v1/geocode/status").await["mode"], "online");
}

/// A place over the home: its ended charges had no cost when it was saved.
fn home_draft(price: Value) -> Value {
    json!({ "name": "Made-up home", "kind": "home", "lat": synth::TOWN.lat, "lon": synth::TOWN.lon, "radius_m": 300,
            "price": price })
}

/// The ended charges of a car at a place: `(id, cost)`.
async fn ended_at(c: &mut Client, car: &str, place: &str) -> Vec<(String, Value)> {
    charges(c, car)
        .await
        .iter()
        .filter(|x| x["place"]["id"] == json!(place) && !x["end_ms"].is_null())
        .map(|x| (x["id"].as_str().unwrap().to_string(), x["cost"].clone()))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_past_charge_without_a_cost_keeps_none_until_a_reprice() {
    let k = seeded().await;
    let a = k.cars[0].clone();
    let mut admin = k.s.admin().await;

    // the preview counts the past charges a reprice after saving would price
    let price = json!({ "currency": "USD", "per_kwh": 0.5 });
    let pv = admin.post("/api/v1/places/preview", home_draft(price.clone())).await.json();
    assert!(pv["charges_repriced"].as_u64().unwrap() > 0, "{pv}");

    // a new priced place over charges that ended without a cost: they keep none, in the list and the detail
    let r = admin.post("/api/v1/places", home_draft(price)).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let hid = r.json()["id"].as_str().unwrap().to_string();
    let ended = ended_at(&mut admin, &a, &hid).await;
    assert!(!ended.is_empty());
    assert!(ended.iter().all(|(_, c)| c.is_null()), "{ended:?}");
    let (one, _) = &ended[0];
    let d = ok(&mut admin, &format!("/api/v1/cars/{a}/charges/{one}")).await;
    assert!(d["cost"].is_null(), "{d}");

    // a price change on the place does not price them either
    let r = admin
        .patch(
            &format!("/api/v1/places/{hid}"),
            json!({ "version": 1, "price": { "currency": "USD", "per_kwh": 0.7 } }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(ended_at(&mut admin, &a, &hid).await.iter().all(|(_, c)| c.is_null()));

    // only a reprice gives them a cost, at the price of today; a dry run first changes nothing
    let dry = admin.post(&format!("/api/v1/places/{hid}/reprice"), json!({ "dry_run": true })).await.json();
    assert!(dry["repriced"].as_u64().unwrap() >= ended.len() as u64, "{dry}");
    assert!(ended_at(&mut admin, &a, &hid).await.iter().all(|(_, c)| c.is_null()));
    let r = admin.post(&format!("/api/v1/places/{hid}/reprice"), json!({ "only_without_cost": true })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let priced = ended_at(&mut admin, &a, &hid).await;
    assert!(priced.iter().all(|(_, c)| c["currency"] == "USD" && c["source"] == "tariff"), "{priced:?}");
    let d = ok(&mut admin, &format!("/api/v1/cars/{a}/charges/{one}")).await;
    assert_eq!(d["cost"]["currency"], "USD", "{d}");

    // a place without a price saved over them: they keep the cost they have (no "no cost" overwrites it)
    let r = admin.request("DELETE", &format!("/api/v1/places/{hid}?version=2"), None, &[]).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let still: Vec<Value> =
        charges(&mut admin, &a).await.into_iter().filter(|c| c["cost"]["currency"] == "USD").collect();
    assert_eq!(still.len(), priced.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paging_by_name_keys_on_the_name_not_the_position() {
    let k = seeded().await;
    let mut admin = k.s.admin().await;
    let mut ids = Vec::new();
    for (i, name) in ["Bravo", "charlie", "Delta", "echo"].iter().enumerate() {
        let r = admin
            .post("/api/v1/places", json!({ "name": name, "lat": -40.0 - i as f64, "lon": 120.0 + i as f64 }))
            .await;
        assert_eq!(r.status, 201, "{}", r.text());
        ids.push(r.json()["id"].as_str().unwrap().to_string());
    }
    let names = |v: &Value| -> Vec<String> {
        v["items"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap().to_string()).collect()
    };
    let p1 = ok(&mut admin, "/api/v1/places?limit=2").await;
    assert_eq!(names(&p1), vec!["Bravo", "charlie"]);
    let next = p1["next"].as_str().unwrap().to_string();
    // a place added before the cursor, one removed before it and one renamed past it: no row repeats or is skipped
    let r = admin.post("/api/v1/places", json!({ "name": "!!first", "lat": -50.0, "lon": 130.0 })).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let r = admin.request("DELETE", &format!("/api/v1/places/{}", ids[0]), None, &[]).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let p2 = ok(&mut admin, &format!("/api/v1/places?limit=2&after={next}")).await;
    assert_eq!(names(&p2), vec!["Delta", "echo"], "{p2}");
    assert_eq!((p2["next"].clone(), p2["total"].as_u64()), (Value::Null, Some(4)));
    // two places of one name: by id, across pages
    let r = admin.post("/api/v1/places", json!({ "name": "DELTA", "lat": -51.0, "lon": 131.0 })).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let path = match &after {
            Some(c) => format!("/api/v1/places?limit=1&after={c}"),
            None => "/api/v1/places?limit=1".to_string(),
        };
        let p = ok(&mut admin, &path).await;
        seen.extend(p["items"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap().to_string()));
        match p["next"].as_str() {
            Some(c) => after = Some(c.to_string()),
            None => break,
        }
    }
    let mut unique = seen.clone();
    unique.dedup();
    assert_eq!((seen.len(), unique.len()), (5, 5), "{seen:?}");
    assert_eq!(code(&mut admin, "/api/v1/places?limit=1&after=bm9wZQ").await.0, 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prices_that_leave_energy_unpriced_and_wrong_preview_versions_are_refused() {
    let k = seeded().await;
    let mut admin = k.s.admin().await;
    let rates = json!([
        { "name": "Night", "days": "mon-fri", "from": "22:00", "to": "06:00", "per_kwh": 0.1 },
        { "name": "Day", "days": "all", "from": "06:00", "to": "22:00", "per_kwh": 0.3 }]);
    let r = admin.post("/api/v1/places", home_draft(json!({ "currency": "EUR", "rates": rates }))).await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")), "{}", r.text());
    assert!(r.json()["error"]["fields"].get("price.per_kwh").is_some(), "{}", r.text());
    let r = admin.post("/api/v1/places/preview", home_draft(json!({ "currency": "EUR", "rates": rates }))).await;
    assert_eq!(r.status, 422, "the preview says the same: {}", r.text());
    let no_price = json!([{ "name": "Free", "days": "all", "from": "00:00", "to": "06:00" }]);
    let r =
        admin.post("/api/v1/places", home_draft(json!({ "currency": "EUR", "per_kwh": 0.2, "rates": no_price }))).await;
    assert_eq!(r.status, 422, "{}", r.text());
    assert!(r.json()["error"]["fields"].get("price.rates.0.per_kwh").is_some(), "{}", r.text());
    // with a price for the hours left out, or a price per minute, it is saved
    let r =
        admin.post("/api/v1/places", home_draft(json!({ "currency": "EUR", "per_kwh": 0.2, "rates": rates }))).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let hid = r.json()["id"].as_str().unwrap().to_string();

    // a preview's version must be a whole number when it is sent
    let r = admin.post("/api/v1/places/preview", json!({ "id": hid, "version": "abc" })).await;
    assert_eq!((r.status, r.code().as_deref()), (400, Some("bad_input")), "{}", r.text());
    assert!(r.json()["error"]["fields"].get("version").is_some(), "{}", r.text());
    let r = admin.post("/api/v1/places/preview", json!({ "id": hid, "version": 1, "name": "Home" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn counts_follow_new_rows_and_every_way_of_asking_agrees() {
    let k = seeded().await;
    let (a, b) = (k.cars[0].clone(), k.cars[1].clone());
    let mut admin = k.s.admin().await;
    let r = admin.post("/api/v1/places", home_draft(Value::Null)).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let hid = r.json()["id"].as_str().unwrap().to_string();
    let counts_of =
        |l: &Value| l["items"].as_array().unwrap().iter().find(|p| p["id"] == json!(hid)).unwrap()["counts"].clone();
    let all = ok(&mut admin, &format!("/api/v1/places/{hid}")).await["counts"].clone();
    let ca = counts_of(&ok(&mut admin, &format!("/api/v1/places?car={a}")).await);
    let cb = counts_of(&ok(&mut admin, &format!("/api/v1/places?car={b}")).await);
    for f in ["drives", "charges", "visits"] {
        assert_eq!(all[f].as_u64().unwrap(), ca[f].as_u64().unwrap() + cb[f].as_u64().unwrap(), "{f}");
    }
    let at = ok(&mut admin, &format!("/api/v1/places/at?lat={}&lon={}", synth::TOWN.lat, synth::TOWN.lon)).await;
    assert_eq!(at["place"]["counts"], all);
    assert_eq!(counts_of(&ok(&mut admin, "/api/v1/places").await), all, "asked again: the same");
    let mut watch = k.s.watch(&mut admin, &[k.ids[1]]).await;
    assert_eq!(counts_of(&ok(&mut watch, "/api/v1/places").await), cb);
    let wat = ok(&mut watch, &format!("/api/v1/places/at?lat={}&lon={}", synth::TOWN.lat, synth::TOWN.lon)).await;
    assert_eq!(wat["place"]["counts"], cb);

    // a later day of car a arrives: its counts grow, car b's stay
    let more = synth::Scenario::new(11).car(0).start(SATURDAY_MS + 2 * 86_400_000).days(1).rates(synth::Rates::Light);
    for log in more.logs() {
        put(&k.store, &k.judge, &log.name, &log.bytes).await;
        put(&k.store, &k.judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
    }
    k._engine.idle().await;
    assert_eq!(engine::query::all_cars(&k._engine).unwrap().len(), 2, "the same car");
    let ca2 = counts_of(&ok(&mut admin, &format!("/api/v1/places?car={a}")).await);
    assert!(ca2["visits"].as_u64() > ca["visits"].as_u64(), "{ca} {ca2}");
    assert_eq!(counts_of(&ok(&mut admin, &format!("/api/v1/places?car={b}")).await), cb);
    let all2 = ok(&mut admin, &format!("/api/v1/places/{hid}")).await["counts"].clone();
    assert_eq!(all2["visits"].as_u64().unwrap(), ca2["visits"].as_u64().unwrap() + cb["visits"].as_u64().unwrap());
}
