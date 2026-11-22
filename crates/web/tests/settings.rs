#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Server settings and integrations through the real router (web::testkit): `/settings` (its defaults, a PATCH of
//! only the parts that change, the frame's preference defaults and tile origins following it, field errors, stale
//! versions, the car order per account), `/settings/mqtt` (write-only password, the TLS rules, versions),
//! `/settings/mqtt/status`, `/preview` (the topics and Home Assistant entities a broker would get, in the server's
//! units), `/test`, `/network`, and the publisher really connecting once MQTT is on. Roles (a watch account reads
//! what it may and changes nothing, a hidden car answers like an unknown one) and CSRF. Over an engine that decoded
//! two made-up cars' day of logs (synth: made-up VINs, a made-up town in the open sea, public signing seeds).

use engine::Engine;
use model::CarId;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use web::resources::mqtt::{Network, Publisher};
use web::testkit::{Client, TestServer};

struct Seeded {
    s: TestServer,
    _engine: Engine,
    cars: Vec<String>,
    ids: Vec<CarId>,
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

/// Two made-up cars with a light day each; the MQTT publisher's side, the ports and a certificate made for
/// `sentry-mate.test`.
async fn seeded() -> Seeded {
    seeded_with(|b, _, _| b).await
}

/// The server with no logs at all (nothing to decode): for the tests that need no car, so they stay in the fast set.
async fn bare() -> Seeded {
    seeded_cars(0, |b, _, _| b).await
}

/// The same; `more` adds services (given the data folder and its vault).
async fn seeded_with(
    more: impl FnOnce(web::Builder, &std::path::Path, &Arc<vault::Vault>) -> web::Builder + Send + 'static,
) -> Seeded {
    seeded_cars(2, more).await
}

/// The same with `cars` made-up cars (0: no logs at all, nothing to decode).
async fn seeded_cars(
    cars: u32,
    more: impl FnOnce(web::Builder, &std::path::Path, &Arc<vault::Vault>) -> web::Builder + Send + 'static,
) -> Seeded {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v.clone(), 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(
        db.clone(),
        store.clone(),
        engine::Options { workers: 2, new_keys: engine::NewKeys::NewCar, ..engine::Options::default() },
    );
    let judge = ingest::Judge::new(store.clone()).on_judged(engine.listener());
    engine.start().unwrap();
    for n in 0..cars {
        let sc = synth::Scenario::new(5).car(n).days(1).rates(synth::Rates::Light);
        for log in sc.logs() {
            put(&store, &judge, &log.name, &log.bytes).await;
            put(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
        }
    }
    engine.idle().await;
    let ids = engine::query::all_cars(&engine).unwrap();
    assert_eq!(ids.len(), cars as usize, "{ids:?}");
    let tls = data.join("tls");
    ingest::server::Identity::load_or_create(&tls, &["sentry-mate.test".to_string()], &v).unwrap();
    let (e2, d2, data2, v2) = (engine.clone(), db.clone(), data.clone(), v.clone());
    let s = TestServer::start_with(move |b| {
        let b = more(b, &data2, &v2);
        b.cars(Arc::new(EngineCars(d2)))
            .service(Arc::new(e2))
            .service(Arc::new(Publisher::new()))
            .service(Arc::new(Network { web_port: 10101, upload_port: 10100 }))
            .service(Arc::new(web::resources::notices::Sources {
                certificate: Some(tls.join("cert.pem")),
                ..Default::default()
            }))
    })
    .await;
    Seeded { s, _engine: engine, cars: ids.iter().map(|c| format!("c{c}")).collect(), ids, _dir: dir }
}

async fn ok(c: &mut Client, path: &str) -> Value {
    let r = c.get(path).await;
    assert_eq!(r.status, 200, "{path}: {}", r.text());
    r.json()
}

fn fields(r: &web::testkit::Res) -> Value {
    r.json()["error"]["fields"].clone()
}

/// The fast set's settings test, with no logs to decode: the defaults, a change of only the parts sent, a stale
/// version and CSRF (`server_settings` below, in the --include-ignored set, does all of it over two decoded cars).
#[tokio::test(flavor = "multi_thread")]
async fn server_settings_read_and_change_without_logs() {
    let t = seeded_cars(0, |b, _, _| b).await;
    let mut admin = t.s.admin().await;
    let s = ok(&mut admin, "/api/v1/settings").await;
    assert_eq!(s["units"], json!({ "distance": "km", "temperature": "c", "pressure": "bar" }));
    assert_eq!(s["range"], "rated");
    assert_eq!(s["tiles"]["mode"], "offline");
    assert_eq!(s["host_names"], json!(["sentry-mate.test"]));
    let v1 = s["version"].as_u64().unwrap();
    let body = serde_json::to_vec(&json!({ "version": v1, "range": "ideal" })).unwrap();
    let r = admin
        .request(
            "PATCH",
            "/api/v1/settings",
            Some(&body),
            &[("Content-Type", "application/json"), ("X-Sentry-Mate", "0")],
        )
        .await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("csrf")));
    let r = admin.patch("/api/v1/settings", json!({ "version": v1, "units": { "pressure": "psi" } })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let s = r.json();
    assert_eq!(s["units"], json!({ "distance": "km", "temperature": "c", "pressure": "psi" }));
    assert_eq!((&s["range"], &s["version"]), (&json!("rated"), &json!(v1 + 1)));
    let r = admin.patch("/api/v1/settings", json!({ "version": v1, "range": "ideal" })).await;
    assert_eq!(r.status, 409, "{}", r.text());
    assert_eq!(ok(&mut admin, "/api/v1/settings").await["units"]["pressure"], "psi");
    assert_eq!(t.s.client().get("/api/v1/settings").await.status, 401);
}

/// The time zone new cars start in: the engine's (UTC here, the container's `TZ` in the real server) until one is
/// saved, an IANA name only, and the engine follows a change at once (cars already made keep theirs).
#[tokio::test(flavor = "multi_thread")]
async fn the_default_time_zone_for_new_cars() {
    let t = bare().await;
    let mut admin = t.s.admin().await;
    let s = ok(&mut admin, "/api/v1/settings").await;
    assert_eq!(s["default_time_zone"], "UTC", "{s}");
    // first-run setup's browser zone: kept only when it is a zone and none is saved yet
    let app = t.s.app.clone();
    tokio::task::spawn_blocking(move || {
        web::resources::settings::first_time_zone(&app, "Not/A_Zone").unwrap();
        assert_eq!(app.service::<Engine>().unwrap().default_time_zone(), "UTC");
        web::resources::settings::first_time_zone(&app, "Europe/Berlin").unwrap();
        web::resources::settings::first_time_zone(&app, "Asia/Tokyo").unwrap();
    })
    .await
    .unwrap();
    let s = ok(&mut admin, "/api/v1/settings").await;
    assert_eq!(s["default_time_zone"], "Europe/Berlin", "the first one stays: {s}");
    assert_eq!(t._engine.default_time_zone(), "Europe/Berlin");
    let v = s["version"].as_u64().unwrap();
    for bad in [json!("Mars/Olympus_Mons"), json!(""), json!("CST6CDT; x")] {
        let r = admin.patch("/api/v1/settings", json!({ "version": v, "default_time_zone": bad })).await;
        assert_eq!(r.status, 422, "{bad}: {}", r.text());
        assert!(fields(&r)["default_time_zone"].is_string(), "{}", r.text());
    }
    let r = admin.patch("/api/v1/settings", json!({ "version": v, "default_time_zone": 5 })).await;
    assert_eq!(r.status, 400, "{}", r.text());
    assert_eq!(t._engine.default_time_zone(), "Europe/Berlin");
    let r = admin.patch("/api/v1/settings", json!({ "version": v, "default_time_zone": " America/Chicago " })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["default_time_zone"], "America/Chicago");
    assert_eq!(t._engine.default_time_zone(), "America/Chicago", "new cars start in it from now on");
    assert_eq!(ok(&mut admin, "/api/v1/settings").await["default_time_zone"], "America/Chicago");
    let audit = ok(&mut admin, "/api/v1/audit").await.to_string();
    assert!(audit.contains("default time zone"), "{audit}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
async fn server_settings() {
    let t = seeded().await;
    let mut admin = t.s.admin().await;
    let mut watch = t.s.watch(&mut admin, &[t.ids[0]]).await;

    // the defaults: every part there, the certificate's names as host names
    let s = ok(&mut admin, "/api/v1/settings").await;
    assert_eq!(s["units"], json!({ "distance": "km", "temperature": "c", "pressure": "bar" }));
    assert_eq!(s["range"], "rated");
    assert_eq!(s["car_order"], json!([]));
    assert_eq!(s["geocode"]["mode"], "offline");
    assert_eq!(s["geocode"]["dataset"], "cities1000");
    assert_eq!(s["tiles"]["mode"], "offline", "no outside tile server until an admin picks one");
    assert!(s["tiles"]["online_url"].as_str().unwrap().contains("{z}"));
    assert_eq!(s["tiles"]["max_zoom"], 19);
    assert_eq!(s["release_check"], json!({ "enabled": false }));
    assert_eq!(s["outside_links"], json!({ "maps": false, "release_notes": false }));
    assert_eq!(s["outbound"], json!({ "proxy": null }));
    assert_eq!(s["host_names"], json!(["sentry-mate.test"]));
    let v1 = s["version"].as_u64().unwrap();

    // a watch account reads (its car order only) but doesn't change
    assert_eq!(ok(&mut watch, "/api/v1/settings").await["version"], v1);
    let r = watch.patch("/api/v1/settings", json!({ "version": v1, "range": "ideal" })).await;
    assert_eq!(r.status, 403, "{}", r.text());
    // CSRF
    let body = serde_json::to_vec(&json!({ "version": v1, "range": "ideal" })).unwrap();
    let r = admin
        .request(
            "PATCH",
            "/api/v1/settings",
            Some(&body),
            &[("Content-Type", "application/json"), ("X-Sentry-Mate", "0")],
        )
        .await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("csrf")));

    // only the parts sent change; the preference defaults follow
    let order = json!([t.cars[1], t.cars[0]]);
    let r = admin
        .patch("/api/v1/settings", json!({ "version": v1, "units": { "pressure": "psi" }, "car_order": order }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let s = r.json();
    assert_eq!(s["units"], json!({ "distance": "km", "temperature": "c", "pressure": "psi" }));
    assert_eq!(s["car_order"], order);
    assert_eq!(s["version"], v1 + 1);
    assert_eq!(t.s.app.defaults().units.pressure, web::units::Pressure::Psi);
    let p = ok(&mut watch, "/api/v1/me/preferences").await;
    assert_eq!(p["units"]["pressure"], "psi", "{p}");
    // the watch account sees its own car in the order only
    assert_eq!(ok(&mut watch, "/api/v1/settings").await["car_order"], json!([t.cars[0]]));

    // a stale version
    let r = admin.patch("/api/v1/settings", json!({ "version": v1, "range": "ideal" })).await;
    assert_eq!((r.status, r.code().as_deref()), (409, Some("stale_version")));
    assert_eq!(r.json()["error"]["version"], v1 + 1);
    let v2 = v1 + 1;

    // wrong types: 400 by field
    let r = admin
        .patch(
            "/api/v1/settings",
            json!({ "version": v2, "units": { "distance": 3, "speed": "x" }, "tiles": "online", "bogus": 1, "car_order": "c1" }),
        )
        .await;
    assert_eq!((r.status, r.code().as_deref()), (400, Some("bad_input")), "{}", r.text());
    let f = fields(&r);
    for k in ["units.distance", "units.speed", "tiles", "bogus", "car_order"] {
        assert!(f.get(k).is_some(), "{k}: {f}");
    }
    // broken rules: 422 by field
    let r = admin
        .patch(
            "/api/v1/settings",
            json!({
                "version": v2,
                "units": { "distance": "furlong" },
                "range": "far",
                "geocode": { "mode": "online", "rounding_m": 5000 },
                "tiles": { "online_url": "http://tiles.example/{z}/{x}/{y}.png", "max_zoom": 30 },
                "outbound": { "proxy": "http://user:pw@proxy.lan:3128" },
                "host_names": ["ok.local", "not a name"],
                "car_order": ["c999999"],
            }),
        )
        .await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")), "{}", r.text());
    let f = fields(&r);
    for k in [
        "units.distance",
        "range",
        "geocode.url",
        "geocode.rounding_m",
        "tiles.online_url",
        "tiles.max_zoom",
        "outbound.proxy",
        "host_names.1",
        "car_order",
    ] {
        assert!(f.get(k).is_some(), "{k}: {f}");
    }
    assert_eq!(ok(&mut admin, "/api/v1/settings").await["version"], v2, "nothing saved");

    // the tile origins of the packs' CSP follow the tile server once an admin turns it on; offline tiles: none
    assert!(t.s.app.tiles().is_empty(), "a fresh server lets the pages load no outside tiles");
    let r = admin
        .patch(
            "/api/v1/settings",
            json!({ "version": v2, "tiles": { "mode": "online", "online_url": "https://tiles.example.org:8443/{z}/{x}/{y}.png" }, "outbound": { "proxy": "socks5://proxy.lan:1080" }, "host_names": ["Car.Example", "car.example", "10.0.0.5"] }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let s = r.json();
    assert_eq!(s["host_names"], json!(["car.example", "10.0.0.5"]));
    assert_eq!(s["outbound"]["proxy"], "socks5://proxy.lan:1080");
    assert_eq!(t.s.app.tiles(), vec!["https://tiles.example.org:8443".to_string()]);
    let r = admin.patch("/api/v1/settings", json!({ "version": v2 + 1, "tiles": { "mode": "offline" } })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(t.s.app.tiles().is_empty());

    // a car order from the watch account's view keeps the cars it doesn't see (an admin changing with all cars)
    let r = admin.patch("/api/v1/settings", json!({ "version": v2 + 2, "car_order": [t.cars[0]] })).await;
    assert_eq!(r.json()["car_order"], json!([t.cars[0]]), "an admin sees every car: the order is what it sent");

    // the audit log names the change
    let a = ok(&mut admin, "/api/v1/audit").await;
    let rows: Vec<&Value> =
        a["items"].as_array().unwrap().iter().filter(|r| r["target"] == "settings:server").collect();
    assert!(rows.len() >= 3, "{a}");
    assert!(rows.iter().any(|r| r["summary"].as_str().unwrap().contains("units")), "{rows:?}");

    // not signed in
    assert_eq!(t.s.client().get("/api/v1/settings").await.status, 401);
}

fn mqtt_body(version: u64) -> Value {
    json!({
        "version": version,
        "enabled": false,
        "host": "broker.example",
        "port": 8883,
        "username": "ha",
        "password": "made-up-password",
        "tls": true,
        "pin_sha256": null,
        "namespace": "sentrymate",
        "publish_location": false,
        "ha_discovery": { "enabled": true, "prefix": "homeassistant", "url": "https://ha.example:8123" },
        "no_place_text": "away",
    })
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
async fn mqtt_settings_status_preview_and_test() {
    let t = seeded().await;
    let mut admin = t.s.admin().await;
    let mut watch = t.s.watch(&mut admin, &[t.ids[0]]).await;

    // the defaults: off, TLS on, no password
    let m = ok(&mut admin, "/api/v1/settings/mqtt").await;
    assert_eq!(m["enabled"], false);
    assert_eq!(m["tls"], true);
    assert_eq!((m["password"].clone(), m["password_set"].clone()), (Value::Null, json!(false)));
    assert_eq!(m["namespace"], "sentry-mate");
    assert_eq!(m["ha_discovery"], json!({ "enabled": false, "prefix": "homeassistant", "url": null }));
    let v1 = m["version"].as_u64().unwrap();
    // admins only, to read and to change
    assert_eq!(watch.get("/api/v1/settings/mqtt").await.status, 403);
    assert_eq!(watch.put("/api/v1/settings/mqtt", mqtt_body(v1)).await.status, 403);
    let body = serde_json::to_vec(&mqtt_body(v1)).unwrap();
    let r = admin
        .request(
            "PUT",
            "/api/v1/settings/mqtt",
            Some(&body),
            &[("Content-Type", "application/json"), ("X-Sentry-Mate", "0")],
        )
        .await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("csrf")));

    // PUT sends the whole setting
    let r = admin.put("/api/v1/settings/mqtt", json!({ "version": v1, "enabled": true })).await;
    assert_eq!((r.status, r.code().as_deref()), (400, Some("bad_input")));
    assert!(fields(&r).get("host").is_some() && fields(&r).get("ha_discovery").is_some(), "{}", r.text());
    // the rules: a password and a pin need TLS, topic prefixes, an https Home Assistant address, the port
    let mut b = mqtt_body(v1);
    b["tls"] = json!(false);
    b["pin_sha256"] = json!("ab".repeat(32));
    b["namespace"] = json!("a/+/b");
    b["ha_discovery"]["prefix"] = json!("$sys");
    b["ha_discovery"]["url"] = json!("http://ha.example");
    b["port"] = json!(70000);
    let r = admin.put("/api/v1/settings/mqtt", b).await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")), "{}", r.text());
    let f = fields(&r);
    for k in ["tls", "pin_sha256", "namespace", "ha_discovery.prefix", "ha_discovery.url", "port"] {
        assert!(f.get(k).is_some(), "{k}: {f}");
    }
    let mut b = mqtt_body(v1);
    b["enabled"] = json!(true);
    b["host"] = json!("");
    assert!(fields(&admin.put("/api/v1/settings/mqtt", b).await).get("host").is_some(), "on needs a host");

    // saved: the password is write-only
    let r = admin.put("/api/v1/settings/mqtt", mqtt_body(v1)).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let m = r.json();
    assert_eq!((m["password"].clone(), m["password_set"].clone()), (Value::Null, json!(true)));
    assert_eq!(
        (m["host"].as_str(), m["port"].as_u64(), m["version"].as_u64()),
        (Some("broker.example"), Some(8883), Some(v1 + 1))
    );
    assert!(!r.text().contains("made-up-password"));
    // left out: kept for the same broker; turning TLS off with it saved is refused
    let mut b = mqtt_body(v1 + 1);
    b.as_object_mut().unwrap().remove("password");
    b["namespace"] = json!("sentrymate2");
    b["host"] = json!("BROKER.example");
    let r = admin.put("/api/v1/settings/mqtt", b.clone()).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["password_set"], true);
    b["namespace"] = json!("sentrymate");
    b["version"] = json!(v1 + 2);
    b["pin_sha256"] = json!(vec!["AB"; 32].join(":"));
    b["password"] = json!("made-up-password");
    let r = admin.put("/api/v1/settings/mqtt", b.clone()).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["password_set"], true);
    assert_eq!(r.json()["pin_sha256"], vec!["AB"; 32].join(":"));
    b.as_object_mut().unwrap().remove("password");
    let mut off = b.clone();
    off["version"] = json!(v1 + 3);
    off["tls"] = json!(false);
    off["pin_sha256"] = Value::Null;
    let r = admin.put("/api/v1/settings/mqtt", off.clone()).await;
    assert!(fields(&r).get("tls").is_some(), "{}", r.text());
    // null removes it; then plain MQTT is allowed (the owner turned TLS off)
    off["password"] = Value::Null;
    let r = admin.put("/api/v1/settings/mqtt", off).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!((r.json()["password_set"].clone(), r.json()["tls"].clone()), (json!(false), json!(false)));
    // a stale version
    let r = admin.put("/api/v1/settings/mqtt", mqtt_body(v1)).await;
    assert_eq!((r.status, r.code().as_deref()), (409, Some("stale_version")));
    let r = admin.put("/api/v1/settings/mqtt", mqtt_body(v1 + 4)).await;
    assert_eq!(r.status, 200, "{}", r.text());

    // status: anyone signed in
    let st = ok(&mut watch, "/api/v1/settings/mqtt/status").await;
    assert_eq!(st["enabled"], false);
    assert_eq!(st["connected"], false);
    assert_eq!(st["broker"], "broker.example:8883");
    assert_eq!(st["tls"], true);
    for k in ["delivered", "waiting", "dropped", "last_error", "last_error_ms"] {
        assert!(st.get(k).is_some(), "{k}: {st}");
    }

    // preview: the first car the account sees by default, its topics, Home Assistant's entities
    let pv = ok(&mut watch, "/api/v1/settings/mqtt/preview").await;
    assert_eq!(pv["car"], t.cars[0]);
    let id = t.ids[0];
    let msgs = pv["messages"].as_array().unwrap();
    assert_eq!(msgs[0], json!({ "topic": "sentrymate/status", "payload": "online", "retained": true }));
    let topic = |k: &str| format!("sentrymate/cars/c{id}/{k}");
    let find = |k: &str| msgs.iter().find(|m| m["topic"] == topic(k)).cloned();
    assert!(find("state").is_some() && find("soc").is_some(), "{pv}");
    assert_eq!(find("latitude").unwrap()["payload"], "", "no positions while location publishing is off");
    assert_eq!(pv["units"], json!({ "distance": "km", "temperature": "c", "pressure": "bar" }));
    let ents = pv["ha_entities"].as_array().unwrap();
    let soc = ents.iter().find(|e| e["state_topic"] == topic("soc")).unwrap();
    assert_eq!(soc["unit"], "%");
    assert!(soc["config_topic"].as_str().unwrap().starts_with("homeassistant/sensor/"), "{soc}");
    assert!(pv["device"]["identifiers"].is_array(), "{pv}");
    // a hidden car answers like an unknown one
    for c in [t.cars[1].as_str(), "c999999"] {
        let r = watch.get(&format!("/api/v1/settings/mqtt/preview?car={c}")).await;
        assert_eq!((r.status, r.code().as_deref()), (404, Some("no_such_car")), "{c}");
    }
    // the admin's preview of the second car; values in the server's units after they change
    let s = ok(&mut admin, "/api/v1/settings").await;
    let r = admin.patch("/api/v1/settings", json!({ "version": s["version"], "units": { "distance": "mi" } })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let pv = ok(&mut admin, &format!("/api/v1/settings/mqtt/preview?car={}", t.cars[1])).await;
    assert_eq!(pv["car"], t.cars[1]);
    assert_eq!(pv["units"]["distance"], "mi");
    let range = pv["ha_entities"].as_array().unwrap().iter().find(|e| e["unit"] == "mi").cloned();
    assert!(range.is_some(), "{pv}");

    // the test: admins only, no body, the saved settings
    assert_eq!(watch.post("/api/v1/settings/mqtt/test", json!({})).await.status, 403);
    let r = admin.post("/api/v1/settings/mqtt/test", json!({ "host": "x" })).await;
    assert_eq!(r.status, 400, "{}", r.text());
    // a broker port nothing listens on (plain MQTT, no password: the owner turned TLS off)
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let m = ok(&mut admin, "/api/v1/settings/mqtt").await;
    let mut b = mqtt_body(m["version"].as_u64().unwrap());
    b["host"] = json!("127.0.0.1");
    b["port"] = json!(port);
    b["tls"] = json!(false);
    b["password"] = Value::Null;
    assert_eq!(admin.put("/api/v1/settings/mqtt", b).await.status, 200);
    let r = admin.post("/api/v1/settings/mqtt/test", json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let res = r.json();
    assert_eq!((res["ok"].clone(), res["error"].clone()), (json!(false), json!("connection_refused")), "{res}");
    assert!(res["at_ms"].is_u64() && res.get("latency_ms").is_some());
    let st = ok(&mut admin, "/api/v1/settings/mqtt/status").await;
    assert_eq!(st["last_error"], "connection_refused");
    assert!(st["last_error_message"].as_str().is_some_and(|m| m.contains("127.0.0.1")), "{st}");
    assert!(st["last_error_ms"].is_u64());
    let a = ok(&mut admin, "/api/v1/audit").await;
    let rows: Vec<&Value> = a["items"].as_array().unwrap().iter().filter(|r| r["target"] == "settings:mqtt").collect();
    assert!(rows.iter().any(|r| r["action"] == "mqtt-tested") && rows.iter().any(|r| r["action"] == "mqtt-changed"));
}

#[tokio::test(flavor = "multi_thread")]
async fn network() {
    let t = bare().await;
    let mut admin = t.s.admin().await;
    let mut watch = t.s.watch(&mut admin, &[]).await;
    assert_eq!(watch.get("/api/v1/network").await.status, 403);
    let n = ok(&mut admin, "/api/v1/network").await;
    let out = n["outbound"].as_array().unwrap();
    let purposes: Vec<&str> = out.iter().map(|o| o["purpose"].as_str().unwrap()).collect();
    assert_eq!(purposes, ["geocoding", "tiles", "geonames_download", "mqtt", "release_check"]);
    let row = |p: &str| out.iter().find(|o| o["purpose"] == p).unwrap().clone();
    assert_eq!(row("geocoding")["enabled"], false, "address lookup is offline by default");
    assert_eq!(
        row("release_check"),
        json!({ "purpose": "release_check", "host": "api.github.com", "enabled": false, "last_used_ms": null }),
        "the release check is off by default"
    );
    assert_eq!(
        row("tiles"),
        json!({ "purpose": "tiles", "host": null, "hosts": [], "preset": "satellite", "enabled": false, "last_used_ms": null }),
        "tiles are offline by default"
    );
    assert_eq!(row("mqtt"), json!({ "purpose": "mqtt", "host": null, "enabled": false, "last_used_ms": null }));
    assert_eq!(
        n["inbound"],
        json!([{ "purpose": "web", "port": 10101, "tls": true }, { "purpose": "upload", "port": 10100, "tls": true }])
    );
    let names = n["certificate"]["names"].as_array().unwrap();
    assert!(names.contains(&json!("sentry-mate.test")) && names.contains(&json!("localhost")), "{n}");
    // saving host names doesn't change the certificate's
    let s = ok(&mut admin, "/api/v1/settings").await;
    let r = admin.patch("/api/v1/settings", json!({ "version": s["version"], "host_names": ["other.example"] })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let n2 = ok(&mut admin, "/api/v1/network").await;
    assert_eq!(n2["certificate"], n["certificate"]);
}

/// With MQTT on, the publisher started by `serve` connects to the broker (here a listener that takes the
/// connection and reads its first packet: an MQTT CONNECT).
/// An admin session can't carry the saved broker password (write-only: no answer shows it) to a broker of its own:
/// a PUT that changes the broker's host, port, TLS or certificate pin must send the password again.
#[tokio::test(flavor = "multi_thread")]
async fn the_saved_mqtt_password_never_follows_a_new_broker() {
    let t = bare().await;
    let mut admin = t.s.admin().await;
    let v = ok(&mut admin, "/api/v1/settings/mqtt").await["version"].as_u64().unwrap();
    let r = admin.put("/api/v1/settings/mqtt", mqtt_body(v)).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let v = v + 1;
    let pin = "7d".repeat(32);
    let attacks = [
        json!({ "host": "127.0.0.1", "port": 18883, "pin_sha256": pin }),
        json!({ "host": "attacker.example" }),
        json!({ "port": 18883 }),
        json!({ "pin_sha256": pin }),
    ];
    for change in attacks {
        let mut b = mqtt_body(v);
        b.as_object_mut().unwrap().remove("password");
        for (k, x) in change.as_object().unwrap() {
            b[k] = x.clone();
        }
        let r = admin.put("/api/v1/settings/mqtt", b).await;
        assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")), "{change}: {}", r.text());
        assert!(fields(&r).get("password").is_some(), "{change}: {}", r.text());
    }
    // nothing was saved: the test still goes to the broker the password was typed for
    let m = ok(&mut admin, "/api/v1/settings/mqtt").await;
    assert_eq!(
        (m["host"].as_str(), m["port"].as_u64(), m["version"].as_u64()),
        (Some("broker.example"), Some(8883), Some(v))
    );
    assert_eq!(m["pin_sha256"], Value::Null);
    // sent again, the new broker is saved (or null: saved without one)
    let mut b = mqtt_body(v);
    b["host"] = json!("broker2.example");
    b["password"] = json!("another-made-up-password");
    assert_eq!(admin.put("/api/v1/settings/mqtt", b.clone()).await.status, 200);
    b["version"] = json!(v + 1);
    b["host"] = json!("broker3.example");
    b["password"] = Value::Null;
    let r = admin.put("/api/v1/settings/mqtt", b).await;
    assert_eq!((r.status, r.json()["password_set"].clone()), (200, json!(false)), "{}", r.text());
}

#[tokio::test(flavor = "multi_thread")]
async fn turning_mqtt_on_connects() {
    use tokio::io::AsyncReadExt;
    let t = bare().await;
    let task = web::resources::mqtt::spawn(t.s.app.clone());
    let mut admin = t.s.admin().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let m = ok(&mut admin, "/api/v1/settings/mqtt").await;
    let mut b = mqtt_body(m["version"].as_u64().unwrap());
    b["enabled"] = json!(true);
    b["host"] = json!("127.0.0.1");
    b["port"] = json!(port);
    b["tls"] = json!(false);
    b["password"] = Value::Null;
    let r = admin.put("/api/v1/settings/mqtt", b).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let (mut conn, _) =
        tokio::time::timeout(std::time::Duration::from_secs(20), listener.accept()).await.unwrap().unwrap();
    let mut first = [0u8; 1];
    conn.read_exact(&mut first).await.unwrap();
    assert_eq!(first[0], 0x10, "an MQTT CONNECT");
    let st = ok(&mut admin, "/api/v1/settings/mqtt/status").await;
    assert_eq!(st["enabled"], true);
    task.abort();
}

/// `host_names`: an empty list means the running certificate's names (so what GET answers can always be sent back),
/// IPv6 addresses are names too; the release check saves (and checks at once); and the frame follows the settings in
/// the order they were saved, however close together two changes come.
#[tokio::test(flavor = "multi_thread")]
async fn host_names_release_check_and_changes_close_together() {
    let t = bare().await;
    let mut admin = t.s.admin().await;
    let s = ok(&mut admin, "/api/v1/settings").await;
    let v = s["version"].as_u64().unwrap();
    let r = admin.patch("/api/v1/settings", json!({ "version": v, "host_names": [] })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["host_names"], json!(["sentry-mate.test"]), "the default names");
    let s = ok(&mut admin, "/api/v1/settings").await;
    let r = admin.patch("/api/v1/settings", json!({ "version": v + 1, "host_names": s["host_names"] })).await;
    assert_eq!(r.status, 200, "what GET answers goes back: {}", r.text());
    let r = admin
        .patch(
            "/api/v1/settings",
            json!({ "version": v + 2, "host_names": ["[::1]", "FD00:0::1", "car.example", "::1"] }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["host_names"], json!(["::1", "fd00::1", "car.example"]));
    let r = admin.patch("/api/v1/settings", json!({ "version": v + 3, "host_names": ["[::1"] })).await;
    assert_eq!((r.status, fields(&r).get("host_names.0").is_some()), (422, true), "{}", r.text());
    let many: Vec<String> = (0..11).map(|i| format!("h{i}.example")).collect();
    let r = admin.patch("/api/v1/settings", json!({ "version": v + 3, "host_names": many })).await;
    assert_eq!((r.status, fields(&r).get("host_names").is_some()), (422, true), "{}", r.text());

    // the release check: turning it on saves, /network lists it on (GitHub's API host), and it checks at once (this
    // test's build is a dev build: nothing to compare, no request)
    let r = admin.patch("/api/v1/settings", json!({ "version": v + 3, "release_check": { "enabled": true } })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["release_check"], json!({ "enabled": true }));
    assert_eq!(ok(&mut admin, "/api/v1/settings").await["release_check"], json!({ "enabled": true }));
    let n = ok(&mut admin, "/api/v1/network").await;
    let rc = n["outbound"].as_array().unwrap().iter().find(|o| o["purpose"] == "release_check").unwrap().clone();
    assert_eq!((rc["host"].clone(), rc["enabled"].clone()), (json!("api.github.com"), json!(true)));
    let mut rel = Value::Null;
    for _ in 0..100 {
        rel = ok(&mut admin, "/api/v1/release").await;
        if rel["status"] == "error" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!((rel["status"].clone(), rel["error"]["code"].clone()), (json!("error"), json!("dev_build")), "{rel}");
    let r = admin.patch("/api/v1/settings", json!({ "version": v + 4, "release_check": { "enabled": false } })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["release_check"], json!({ "enabled": false }));
    let rel = ok(&mut admin, "/api/v1/release").await;
    assert_eq!(
        (rel["status"].clone(), rel["error"].clone(), rel["next_ms"].clone()),
        (json!("off"), Value::Null, Value::Null)
    );
    let n = ok(&mut admin, "/api/v1/network").await;
    let rc = n["outbound"].as_array().unwrap().iter().find(|o| o["purpose"] == "release_check").unwrap().clone();
    assert_eq!(rc["enabled"], false);

    // two admins changing at once: whatever order they are saved in, the frame ends with the saved defaults
    let mut other = t.s.client();
    let r = other
        .post("/api/v1/session", json!({ "name": web::testkit::ADMIN, "password": web::testkit::ADMIN_PASSWORD }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    async fn change_to(c: &mut Client, pressure: &str) {
        for _ in 0..50 {
            let v = ok(c, "/api/v1/settings").await["version"].clone();
            let r = c.patch("/api/v1/settings", json!({ "version": v, "units": { "pressure": pressure } })).await;
            match r.status {
                200 => return,
                409 => continue,
                s => panic!("{s}: {}", r.text()),
            }
        }
        panic!("never saved");
    }
    for round in 0..6 {
        let (a, b) = if round % 2 == 0 { ("psi", "bar") } else { ("bar", "psi") };
        tokio::join!(change_to(&mut admin, a), change_to(&mut other, b));
        let saved = ok(&mut admin, "/api/v1/settings").await["units"]["pressure"].clone();
        let frame = match t.s.app.defaults().units.pressure {
            web::units::Pressure::Psi => "psi",
            web::units::Pressure::Bar => "bar",
        };
        assert_eq!(saved, json!(frame), "round {round}");
    }
}

// ---- a small MQTT broker: takes plain MQTT 3.1.1, acknowledges everything, keeps what it got

#[derive(Debug, Clone)]
struct Got {
    client_id: String,
    topic: String,
    payload: String,
    retain: bool,
}

#[derive(Default)]
struct BrokerLog {
    clients: Vec<String>,
    got: Vec<Got>,
}

struct Broker {
    port: u16,
    log: Arc<std::sync::Mutex<BrokerLog>>,
}

impl Broker {
    async fn start() -> Broker {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let log = Arc::new(std::sync::Mutex::new(BrokerLog::default()));
        let l2 = log.clone();
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else { return };
                tokio::spawn(serve_mqtt(tcp, l2.clone()));
            }
        });
        Broker { port, log }
    }

    fn got(&self) -> Vec<Got> {
        self.log.lock().unwrap().got.clone()
    }

    fn clients(&self) -> Vec<String> {
        self.log.lock().unwrap().clients.clone()
    }
}

async fn serve_mqtt(mut s: tokio::net::TcpStream, log: Arc<std::sync::Mutex<BrokerLog>>) {
    use rumqttc::mqttbytes::v4::{ConnAck, ConnectReturnCode, Packet, PubAck};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = bytes::BytesMut::new();
    let mut chunk = vec![0u8; 16 * 1024];
    let mut client = String::new();
    loop {
        loop {
            let packet = match Packet::read(&mut buf, 1 << 20) {
                Ok(p) => p,
                Err(rumqttc::mqttbytes::Error::InsufficientBytes(_)) => break,
                Err(_) => return,
            };
            let reply = match packet {
                Packet::Connect(c) => {
                    client = c.client_id.clone();
                    log.lock().unwrap().clients.push(c.client_id);
                    Some(Packet::ConnAck(ConnAck::new(ConnectReturnCode::Success, false)))
                }
                Packet::Publish(p) => {
                    log.lock().unwrap().got.push(Got {
                        client_id: client.clone(),
                        topic: p.topic.clone(),
                        payload: String::from_utf8(p.payload.to_vec()).unwrap(),
                        retain: p.retain,
                    });
                    (p.qos == rumqttc::mqttbytes::QoS::AtLeastOnce).then(|| Packet::PubAck(PubAck::new(p.pkid)))
                }
                Packet::PingReq => Some(Packet::PingResp),
                Packet::Disconnect => return,
                _ => None,
            };
            if let Some(r) = reply {
                let mut out = bytes::BytesMut::new();
                r.write(&mut out, 1 << 20).unwrap();
                if s.write_all(&out).await.is_err() {
                    return;
                }
            }
        }
        match s.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

async fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !f() {
        assert!(start.elapsed() < std::time::Duration::from_secs(60), "waited too long for {what}");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// The MQTT setting for the test broker: on, plain MQTT (the owner allowed it), no password.
fn on_body(version: u64, port: u16, namespace: &str, discovery: bool) -> Value {
    let mut b = mqtt_body(version);
    b["enabled"] = json!(true);
    b["host"] = json!("127.0.0.1");
    b["port"] = json!(port);
    b["tls"] = json!(false);
    b["password"] = Value::Null;
    b["namespace"] = json!(namespace);
    b["ha_discovery"]["enabled"] = json!(discovery);
    b
}

/// The publisher started by `serve` sends each drive and charge as the engine records it: on the event topic of the
/// car's API id, exactly as `/cars/{car}/events` lists it, once each (a restart sends none again). A save that turns
/// Home Assistant discovery off removes the old configs from the broker; a new topic prefix clears the old values.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
async fn the_publisher_sends_events_and_clears_old_layouts() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v.clone(), 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(
        db.clone(),
        store.clone(),
        engine::Options { workers: 2, new_keys: engine::NewKeys::NewCar, ..engine::Options::default() },
    );
    let judge = ingest::Judge::new(store.clone()).on_judged(engine.listener());
    engine.start().unwrap();
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| {
        b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2)).service(Arc::new(Publisher::new()))
    })
    .await;
    let task = web::resources::mqtt::spawn(s.app.clone());
    let mut admin = s.admin().await;
    let broker = Broker::start().await;
    let m = ok(&mut admin, "/api/v1/settings/mqtt").await;
    let r = admin.put("/api/v1/settings/mqtt", on_body(m["version"].as_u64().unwrap(), broker.port, "sm", true)).await;
    assert_eq!(r.status, 200, "{}", r.text());
    wait_until("the publisher's connection", || !broker.clients().is_empty()).await;

    // a made-up car's day that ended six hours ago, uploaded now
    let now = s.app.now();
    let start = (now - 30 * 3_600_000) / 60_000 * 60_000;
    let sc = synth::Scenario::new(5).car(0).days(1).start(start).rates(synth::Rates::Light);
    for log in sc.logs() {
        put(&store, &judge, &log.name, &log.bytes).await;
        put(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
    }
    engine.idle().await;
    let car = engine::query::all_cars(&engine).unwrap()[0];
    let c = format!("c{car}");

    // what the events list says a subscriber got
    let types = "drive_started,drive_ended,charge_started,charge_ended";
    let from = now - web::resources::mqtt::RECENT_MS;
    let listed =
        ok(&mut admin, &format!("/api/v1/cars/{c}/events?types={types}&from_ms={from}&to_ms={now}&limit=200")).await;
    // an end goes out once the data goes on past it (the day's data ends at start + 24 h)
    let data_end = start + 24 * 3_600_000;
    let mut want: Vec<(String, Value)> = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| {
            !r["type"].as_str().unwrap().ends_with("_ended") || r["at_ms"].as_u64().unwrap() + 120_000 <= data_end
        })
        .map(|r| (r["topic"].as_str().unwrap().to_string(), r["payload"].clone()))
        .collect();
    want.reverse();
    assert!(want.len() >= 2, "a day has drives: {listed}");
    let topic = format!("sm/cars/{c}/event");
    assert!(want.iter().all(|(t, _)| *t == topic), "{want:?}");
    let events = |b: &Broker| -> Vec<(String, Value)> {
        b.got()
            .into_iter()
            .filter(|g| g.topic.ends_with("/event") && !g.topic.contains("homeassistant"))
            .map(|g| {
                assert!(!g.retain, "events are not retained");
                (g.topic, serde_json::from_str(&g.payload).unwrap())
            })
            .collect()
    };
    wait_until("the events", || events(&broker).len() >= want.len()).await;
    let got = events(&broker);
    assert_eq!(got, want, "each event once, oldest first, as the list shows it");
    // the values went to the car's API id too, and Home Assistant's configs with it
    let all = broker.got();
    assert!(all.iter().any(|g| g.topic == format!("sm/cars/{c}/soc") && g.retain), "{all:?}");
    let config = format!("homeassistant/event/sm_{c}/event/config");
    let cfg = all.iter().find(|g| g.topic == config).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&cfg.payload).unwrap()["state_topic"], json!(topic));

    // discovery off: the old configs are removed (by a connection of its own), the events are not sent again
    let m = ok(&mut admin, "/api/v1/settings/mqtt").await;
    let r = admin.put("/api/v1/settings/mqtt", on_body(m["version"].as_u64().unwrap(), broker.port, "sm", false)).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let cleared = |b: &Broker, t: &str| b.got().iter().any(|g| g.topic == t && g.payload.is_empty() && g.retain);
    wait_until("the configs removed", || cleared(&broker, &config)).await;
    let by: Vec<String> = broker
        .got()
        .iter()
        .filter(|g| g.topic == config && g.payload.is_empty())
        .map(|g| g.client_id.clone())
        .collect();
    assert!(by.iter().all(|id| id.ends_with("-cleanup")), "{by:?}");
    assert!(!cleared(&broker, &format!("sm/cars/{c}/soc")), "the values stay: the prefix is the same");
    wait_until("the new publisher", || broker.clients().iter().filter(|id| !id.ends_with("-cleanup")).count() >= 2)
        .await;

    // a new topic prefix: the old values and status are cleared, the new ones published
    let m = ok(&mut admin, "/api/v1/settings/mqtt").await;
    let r =
        admin.put("/api/v1/settings/mqtt", on_body(m["version"].as_u64().unwrap(), broker.port, "sm2", false)).await;
    assert_eq!(r.status, 200, "{}", r.text());
    wait_until("the old values cleared", || {
        cleared(&broker, &format!("sm/cars/{c}/soc")) && cleared(&broker, "sm/status")
    })
    .await;
    wait_until("the new prefix", || broker.got().iter().any(|g| g.topic == format!("sm2/cars/{c}/soc"))).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(events(&broker).len(), want.len(), "no event sent twice after the restarts");
    task.abort();
    drop(engine);
}

/// A publisher that can't reach its broker says so as a code, the words beside it.
#[tokio::test(flavor = "multi_thread")]
async fn publisher_errors_are_codes() {
    let t = bare().await;
    let task = web::resources::mqtt::spawn(t.s.app.clone());
    let mut admin = t.s.admin().await;
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let m = ok(&mut admin, "/api/v1/settings/mqtt").await;
    let r = admin.put("/api/v1/settings/mqtt", on_body(m["version"].as_u64().unwrap(), port, "sm", false)).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let start = std::time::Instant::now();
    let st = loop {
        let st = ok(&mut admin, "/api/v1/settings/mqtt/status").await;
        if !st["last_error"].is_null() {
            break st;
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(30), "{st}");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    assert_eq!(st["last_error"], "connection_refused", "{st}");
    let words = st["last_error_message"].as_str().unwrap();
    assert!(!words.is_empty() && words != "connection_refused", "{st}");
    assert!(st["last_error_ms"].is_u64());
    task.abort();
}

/// The program's side of a new certificate, for the tests: the real one, or (`own`) a server that runs with its
/// owner's certificate.
struct Maker {
    live: Arc<ingest::server::LiveCert>,
    own: std::sync::atomic::AtomicBool,
}

impl web::resources::certificate::CertificateMaker for Maker {
    fn own_certificate(&self) -> bool {
        self.own.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn renew(&self, names: &[String]) -> Result<String, web::resources::certificate::Refused> {
        if self.own.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(web::resources::certificate::Refused::OwnCertificate);
        }
        self.live.renew(names).map_err(|e| web::resources::certificate::Refused::Failed(e.to_string()))
    }
}

/// `POST /network/certificate`: an admin with the password again; the saved host names go in, `/network` shows the
/// new SHA-256 at once; refused while a move code waits, and when the server runs with its owner's own
/// certificate; recorded in the audit log. (No logs: none are needed, and it stays in the fast set.)
#[tokio::test(flavor = "multi_thread")]
async fn a_new_certificate() {
    let maker = Arc::new(std::sync::Mutex::new(None::<Arc<Maker>>));
    let m2 = maker.clone();
    let t =
        seeded_cars(0, move |b, data, v| {
            let tls = data.join("tls");
            let id = ingest::server::Identity::load_or_create(&tls, &["sentry-mate.test".to_string()], v).unwrap();
            let live = ingest::server::LiveCert::new(&id, &tls, v.clone()).unwrap();
            let m = Arc::new(Maker { live, own: false.into() });
            *m2.lock().unwrap() = Some(m.clone());
            let keys = data.parent().unwrap().join("keys");
            b.service(Arc::new(web::resources::certificate::Certificates(m)))
                .service(Arc::new(web::moving::MoveSite::new(data.to_path_buf(), keys, "0.0.0")))
        })
        .await;
    let maker = maker.lock().unwrap().clone().unwrap();
    let mut admin = t.s.admin().await;
    let mut watch = t.s.watch(&mut admin, &[]).await;
    let pw = web::testkit::ADMIN_PASSWORD;
    let before = ok(&mut admin, "/api/v1/network").await;
    let old = before["certificate"]["sha256"].as_str().unwrap().to_string();
    assert_eq!(old.len(), 95, "{before}");
    assert_eq!(before["upload_address"], json!("https://sentry-mate.test:10100/api/can/logs"));
    let s = ok(&mut admin, "/api/v1/settings").await;
    let r = admin.patch("/api/v1/settings", json!({ "version": s["version"], "host_names": ["nas.example"] })).await;
    assert_eq!(r.status, 200, "{}", r.text());

    let path = "/api/v1/network/certificate";
    assert_eq!(watch.post(path, json!({ "password": web::testkit::WATCH_PASSWORD })).await.status, 403);
    assert_eq!(admin.post(path, json!({})).await.status, 400);
    let r = admin.post(path, json!({ "password": "not it" })).await;
    assert_eq!((r.status, r.code()), (403, Some("wrong_password".into())), "{}", r.text());
    let r = admin.post(path, json!({ "password": pw, "extra": 1 })).await;
    assert_eq!(r.status, 400, "{}", r.text());
    assert_eq!(ok(&mut admin, "/api/v1/network").await["certificate"]["sha256"], json!(old), "nothing changed");

    let r = admin.post(path, json!({ "password": pw })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let made = r.json();
    assert_eq!(made["names"], json!(["localhost", "nas.example"]));
    assert_eq!(made["upload_address"], json!("https://nas.example:10100/api/can/logs"));
    let new = made["sha256"].as_str().unwrap().to_string();
    assert_ne!(new, old);
    assert_eq!(new.replace(':', "").to_ascii_lowercase(), maker.live.fingerprint());
    let n = ok(&mut admin, "/api/v1/network").await;
    assert_eq!(n["certificate"], json!({ "names": ["localhost", "nas.example"], "sha256": new, "own": false }));
    let a = ok(&mut admin, "/api/v1/audit").await;
    let e = a["items"].as_array().unwrap().iter().find(|e| e["action"] == "certificate-made").unwrap().clone();
    assert_eq!(e["target"], "server:certificate", "{e}");

    // a move code waiting: the new server would pin this certificate
    let r = admin.post("/api/v1/system/move", json!({ "password": pw })).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let r = admin.post(path, json!({ "password": pw })).await;
    assert_eq!((r.status, r.code()), (409, Some("move_under_way".into())), "{}", r.text());
    let r = admin.delete("/api/v1/system/move", json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());

    // the owner's own certificate is not replaced: /network says so, and it is refused before the password is
    // checked (a two-step code is not used up on it)
    maker.own.store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(ok(&mut admin, "/api/v1/network").await["certificate"]["own"], json!(true));
    let r = admin.post(path, json!({ "password": "not it" })).await;
    assert_eq!((r.status, r.code()), (409, Some("own_certificate".into())), "{}", r.text());
    let r = admin.post(path, json!({ "password": pw })).await;
    assert_eq!((r.status, r.code()), (409, Some("own_certificate".into())), "{}", r.text());
    let words = r.json()["error"]["message"].as_str().unwrap().to_string();
    assert!(words.contains("key.sealed") && !words.contains("remove those two files"), "{words}");
    maker.own.store(false, std::sync::atomic::Ordering::Relaxed);
    let r = admin.post(path, json!({ "password": pw })).await;
    assert_eq!(r.status, 200, "after the move code is cancelled: {}", r.text());
}

// ---- the release check, against a stand-in for GitHub's API (never the network)

const THIS_BUILD: &str = "1111111aaaabbbbccccddddeeeeffff000011112";
const NEWEST_BUILD: &str = "abc1234aaaabbbbccccddddeeeeffff000011112";

/// `/release` and `/release/check`: off by default (and "Check now" refused while off), turning it on checks at once,
/// a newer build with its titles, the admins' `release_available` notice, `/about` `build`, `/network`'s row.
#[tokio::test(flavor = "multi_thread")]
async fn release_check_against_a_fake_github() {
    let fake = Arc::new(web::testkit::FakeGitHub::default());
    fake.answer(
        "/repos/skittles-fivem/Sentry-Mate/actions/workflows/image.yml/runs?branch=main&status=success&per_page=1",
        json!({ "total_count": 1, "workflow_runs": [
            { "head_sha": NEWEST_BUILD, "run_number": 57, "updated_at": "2026-11-24T10:00:00Z" } ] }),
    );
    fake.answer(
        &format!("/repos/skittles-fivem/Sentry-Mate/compare/{THIS_BUILD}...{NEWEST_BUILD}"),
        json!({ "status": "ahead", "ahead_by": 2, "behind_by": 0, "commits": [
            { "commit": { "message": "A made-up first change\n\nbody" } },
            { "commit": { "message": "A made-up second change" } } ] }),
    );
    let build = release::Build::from_parts("0.1.0", Some("12"), Some(THIS_BUILD), Some("2026-11-22T08:00:00Z"));
    let f2 = fake.clone();
    let t = seeded_cars(0, move |b, _, _| b.release_check(f2, build)).await;
    let mut admin = t.s.admin().await;
    let mut watch = t.s.watch(&mut admin, &[]).await;

    let r = ok(&mut watch, "/api/v1/release").await;
    assert_eq!(
        (r["enabled"].clone(), r["status"].clone(), r["newest"].clone()),
        (json!(false), json!("off"), Value::Null)
    );
    assert_eq!(
        r["current"],
        json!({ "commit": THIS_BUILD, "short": "1111111", "built_ms": 1_795_334_400_000u64, "number": 12 })
    );
    assert_eq!(r["pull"], "docker compose pull && docker compose up -d");
    assert_eq!(r["image"], "ghcr.io/skittles-fivem/sentry-mate:latest");
    let r = admin.post("/api/v1/release/check", json!({})).await;
    assert_eq!((r.status, r.code()), (409, Some("release_check_off".into())), "{}", r.text());
    assert_eq!(watch.post("/api/v1/release/check", json!({})).await.status, 403);
    assert!(fake.asked().is_empty(), "nothing asked while off");

    let a = ok(&mut watch, "/api/v1/about").await;
    assert_eq!(
        a["build"],
        json!({ "version": "0.1.0", "number": 12, "commit": THIS_BUILD, "short": "1111111",
                "built_ms": 1_795_334_400_000u64, "dev": false })
    );

    // on: a check at once
    let v = ok(&mut admin, "/api/v1/settings").await["version"].as_u64().unwrap();
    let r = admin.patch("/api/v1/settings", json!({ "version": v, "release_check": { "enabled": true } })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let mut rel = Value::Null;
    for _ in 0..200 {
        rel = ok(&mut watch, "/api/v1/release").await;
        if rel["status"] == "newer" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(rel["status"], "newer", "{rel}");
    let n = &rel["newest"];
    assert_eq!(
        (n["commit"].clone(), n["short"].clone(), n["build"].clone()),
        (json!(NEWEST_BUILD), json!("abc1234"), json!(57))
    );
    assert_eq!((n["changes"].clone(), n["more"].clone()), (json!(2), json!(0)));
    assert_eq!(n["titles"], json!(["A made-up first change", "A made-up second change"]));
    assert_eq!(
        n["compare_url"],
        format!("https://github.com/skittles-fivem/Sentry-Mate/compare/{THIS_BUILD}...{NEWEST_BUILD}")
    );
    let checked = rel["checked_ms"].as_u64().unwrap();
    assert_eq!(rel["next_ms"].as_u64(), Some(checked + 24 * 3_600_000), "the next one a day later");
    assert_eq!(fake.asked().len(), 2);

    // the notice: admins only
    let items = ok(&mut admin, "/api/v1/notices").await["items"].clone();
    let notice = items.as_array().unwrap().iter().find(|i| i["code"] == "release_available").cloned().unwrap();
    assert_eq!(notice["level"], "info");
    assert_eq!(notice["fix_link"], "/help#about");
    assert_eq!(notice["message"], "A newer build is ready: abc1234, Nov 24, 2 changes.");
    assert_eq!(notice["detail"]["commit"], NEWEST_BUILD);
    assert_eq!(notice["detail"]["version"], "abc1234");
    assert_eq!(notice["detail"]["build"], 57);
    assert_eq!(notice["detail"]["notes_url"], n["compare_url"]);
    let items = ok(&mut watch, "/api/v1/notices").await["items"].clone();
    assert!(!items.as_array().unwrap().iter().any(|i| i["code"] == "release_available"));

    // "Check now": asks again at once
    let r = admin.post("/api/v1/release/check", json!({})).await;
    assert_eq!((r.status, r.json()["status"].clone()), (200, json!("newer")), "{}", r.text());
    assert_eq!(fake.asked().len(), 4);
    let n = ok(&mut admin, "/api/v1/network").await;
    let rc = n["outbound"].as_array().unwrap().iter().find(|o| o["purpose"] == "release_check").unwrap().clone();
    assert_eq!((rc["host"].clone(), rc["enabled"].clone()), (json!("api.github.com"), json!(true)));
    assert!(rc["last_used_ms"].is_u64(), "{rc}");

    // GitHub unreachable: an error, the newest kept, no notice
    fake.answers.lock().unwrap().clear();
    let r = admin.post("/api/v1/release/check", json!({})).await;
    assert_eq!((r.json()["status"].clone(), r.json()["error"]["code"].clone()), (json!("error"), json!("unreachable")));
    let items = ok(&mut admin, "/api/v1/notices").await["items"].clone();
    assert!(!items.as_array().unwrap().iter().any(|i| i["code"] == "release_available"));

    // off: the result dropped
    let v = ok(&mut admin, "/api/v1/settings").await["version"].as_u64().unwrap();
    let r = admin.patch("/api/v1/settings", json!({ "version": v, "release_check": { "enabled": false } })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let rel = ok(&mut admin, "/api/v1/release").await;
    assert_eq!(
        (rel["status"].clone(), rel["newest"].clone(), rel["checked_ms"].clone()),
        (json!("off"), Value::Null, Value::Null)
    );
}
