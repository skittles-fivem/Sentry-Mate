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
    for n in 0..2 {
        let sc = synth::Scenario::new(5).car(n).days(1).rates(synth::Rates::Light);
        for log in sc.logs() {
            put(&store, &judge, &log.name, &log.bytes).await;
            put(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
        }
    }
    engine.idle().await;
    let ids = engine::query::all_cars(&engine).unwrap();
    assert_eq!(ids.len(), 2, "{ids:?}");
    let tls = data.join("tls");
    ingest::server::Identity::load_or_create(&tls, &["sentry-mate.test".to_string()], &v).unwrap();
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| {
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

#[tokio::test(flavor = "multi_thread")]
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
    assert_eq!(s["tiles"]["mode"], "online");
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

    // the tile origins of the packs' CSP follow the tile server; offline tiles: none
    let r = admin
        .patch(
            "/api/v1/settings",
            json!({ "version": v2, "tiles": { "online_url": "https://tiles.example.org:8443/{z}/{x}/{y}.png" }, "outbound": { "proxy": "socks5://proxy.lan:1080" }, "host_names": ["Car.Example", "car.example", "10.0.0.5"] }),
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
    // left out: kept; turning TLS off with it saved is refused
    let mut b = mqtt_body(v1 + 1);
    b.as_object_mut().unwrap().remove("password");
    b["pin_sha256"] = json!(vec!["AB"; 32].join(":"));
    let r = admin.put("/api/v1/settings/mqtt", b.clone()).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["password_set"], true);
    assert_eq!(r.json()["pin_sha256"], vec!["AB"; 32].join(":"));
    let mut off = b.clone();
    off["version"] = json!(v1 + 2);
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
    let r = admin.put("/api/v1/settings/mqtt", mqtt_body(v1 + 3)).await;
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
    let topic = |k: &str| format!("sentrymate/cars/{id}/{k}");
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
    assert!(st["last_error_ms"].is_u64());
    let a = ok(&mut admin, "/api/v1/audit").await;
    let rows: Vec<&Value> = a["items"].as_array().unwrap().iter().filter(|r| r["target"] == "settings:mqtt").collect();
    assert!(rows.iter().any(|r| r["action"] == "mqtt-tested") && rows.iter().any(|r| r["action"] == "mqtt-changed"));
}

#[tokio::test(flavor = "multi_thread")]
async fn network() {
    let t = seeded().await;
    let mut admin = t.s.admin().await;
    let mut watch = t.s.watch(&mut admin, &[t.ids[0]]).await;
    assert_eq!(watch.get("/api/v1/network").await.status, 403);
    let n = ok(&mut admin, "/api/v1/network").await;
    let out = n["outbound"].as_array().unwrap();
    let purposes: Vec<&str> = out.iter().map(|o| o["purpose"].as_str().unwrap()).collect();
    assert_eq!(purposes, ["geocoding", "tiles", "geonames_download", "mqtt", "release_check"]);
    let row = |p: &str| out.iter().find(|o| o["purpose"] == p).unwrap().clone();
    assert_eq!(row("geocoding")["enabled"], false, "address lookup is offline by default");
    assert_eq!(row("tiles")["host"], "tile.openstreetmap.org");
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
#[tokio::test(flavor = "multi_thread")]
async fn turning_mqtt_on_connects() {
    use tokio::io::AsyncReadExt;
    let t = seeded().await;
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
