#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The local TeslaMate input's routes through the real router (web::testkit): admin only (reads too), off until
//! added, the password asked again, the plain-text password rule (`422 plaintext_password`), the resend rule, audit
//! entries without a password, the `/network` rows and the `/system/health` part. The broker is a closed port on
//! this machine; every name and password is made up.

use engine::Engine;
use model::CarId;
use serde_json::{Value, json};
use std::sync::Arc;
use web::resources::teslamate_local::TeslamateLocalService;
use web::testkit::{ADMIN_PASSWORD, Client, TestServer};

const PASSWORD: &str = "made-up-broker-password";

struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

struct World {
    s: TestServer,
    svc: Arc<TeslamateLocalService>,
    car: CarId,
    _engine: Engine,
    _dir: tempfile::TempDir,
}

async fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v.clone(), 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(db.clone(), store, engine::Options { workers: 1, ..engine::Options::default() });
    let car = engine
        .api_new_car_for(
            &tesla::NewCar {
                vin_prefix: None,
                model: Some('3'),
                year: Some(2023),
                display_name: Some("Made-up Car".into()),
            },
            model::Input::TeslamateLocal,
        )
        .unwrap();
    let svc = Arc::new(web::resources::teslamate_local::service(&data, v, &engine));
    let (e2, d2, s2) = (engine.clone(), db.clone(), svc.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2)).service(s2)).await;
    World { s, svc, car, _engine: engine, _dir: dir }
}

fn closed_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

fn clean(r: &web::testkit::Res) {
    assert!(!r.text().contains(PASSWORD), "a password in an answer: {}", r.text());
}

async fn audit_actions(admin: &mut Client) -> Vec<String> {
    let r = admin.get("/api/v1/audit?limit=200").await;
    assert_eq!(r.status, 200, "{}", r.text());
    clean(&r);
    r.json()["items"].as_array().unwrap().iter().map(|e| e["action"].as_str().unwrap_or_default().to_string()).collect()
}

fn row(net: &Value, purpose: &str) -> Value {
    net["outbound"].as_array().unwrap().iter().find(|r| r["purpose"] == purpose).cloned().unwrap()
}

#[tokio::test]
async fn off_until_added_plaintext_refused_then_added_and_removed() {
    let w = world().await;
    let mut admin = w.s.admin().await;
    let t = "/api/v1/integrations/teslamate";
    let o = admin.get(t).await;
    assert_eq!(o.status, 200, "{}", o.text());
    let o = o.json();
    assert_eq!((o["enabled"].clone(), o["version"].clone(), o["hosts"].clone()), (json!(false), json!(0), json!([])));
    assert_eq!(o["status"]["mqtt"]["state"], "off");
    assert_eq!(o["mqtt"]["prefix"], "teslamate");
    let net = admin.get("/api/v1/network").await.json();
    assert_eq!(row(&net, "teslamate_mqtt")["enabled"], false);
    assert_eq!(row(&net, "teslamate_db")["host"], Value::Null);
    let h = admin.get("/api/v1/system/health").await.json();
    assert_eq!(
        (h["teslamate_local"]["enabled"].clone(), h["teslamate_local"]["mqtt_state"].clone()),
        (json!(false), json!("off"))
    );

    // a watch account reads nothing of it and changes nothing
    let mut watch = w.s.watch(&mut admin, &[w.car]).await;
    assert_eq!(watch.get(t).await.status, 403);
    assert_eq!(watch.get("/api/v1/integrations/teslamate/cars").await.status, 403);

    // a password over plain TCP without the tick: refused, nothing stored or connected
    let port = closed_port();
    let mqtt = json!({ "host": "127.0.0.1", "port": port, "username": "reader", "password": PASSWORD });
    let r = admin.put(t, json!({ "version": 0, "mqtt": mqtt, "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("plaintext_password")), "{}", r.text());
    assert!(r.json()["error"]["fields"]["mqtt.password"].is_string());
    clean(&r);
    let r = admin
        .post(
            "/api/v1/integrations/teslamate/test",
            json!({ "target": "mqtt", "mqtt": mqtt, "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("plaintext_password")));
    assert!(!w.svc.store().exists() && !w.svc.running());

    // other rules: a scheme in the host, a wildcard prefix, a pin without TLS
    let r = admin
        .put(t, json!({ "version": 0, "mqtt": { "host": "mqtt://192.0.2.10", "prefix": "a/#", "pin_sha256": "ab".repeat(32) }, "confirm_password": ADMIN_PASSWORD }))
        .await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")));
    let f = &r.json()["error"]["fields"];
    assert!(f["mqtt.host"].is_string() && f["mqtt.prefix"].is_string() && f["mqtt.pin_sha256"].is_string(), "{f}");

    // the password first
    let mut ok = mqtt.clone();
    ok["allow_unencrypted"] = json!(true);
    let r = admin.put(t, json!({ "version": 0, "mqtt": ok })).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("password_needed")));
    assert!(!audit_actions(&mut admin).await.iter().any(|a| a.starts_with("teslamate-local")));

    // allowed for the local network: added (201), the password sealed and never shown
    let r = admin.put(t, json!({ "version": 0, "mqtt": ok, "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 201, "{}", r.text());
    clean(&r);
    let o = r.json();
    assert_eq!((o["enabled"].clone(), o["version"].clone()), (json!(true), json!(1)));
    assert_eq!((o["mqtt"]["password_set"].clone(), o["mqtt"]["password"].clone()), (json!(true), Value::Null));
    assert_eq!(o["hosts"], json!([format!("127.0.0.1:{port}")]));
    let net = admin.get("/api/v1/network").await.json();
    assert_eq!(
        (row(&net, "teslamate_mqtt")["host"].clone(), row(&net, "teslamate_mqtt")["enabled"].clone()),
        (json!(format!("127.0.0.1:{port}")), json!(true))
    );
    assert_eq!(admin.get("/api/v1/system/health").await.json()["teslamate_local"]["enabled"], true);
    // a stale version
    let r = admin.put(t, json!({ "version": 0, "mqtt": ok, "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code().as_deref()), (409, Some("stale_version")));

    // a kept password goes only to the host it was typed for
    let moved = json!({ "host": "192.0.2.10", "port": port, "username": "reader", "allow_unencrypted": true });
    let r = admin.put(t, json!({ "version": 1, "mqtt": moved, "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("password_resend")), "{}", r.text());
    // the same host, the password left out: kept
    let kept = json!({ "host": "127.0.0.1", "port": port, "username": "reader", "allow_unencrypted": true });
    let r = admin.put(t, json!({ "version": 1, "mqtt": kept, "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!((r.json()["version"].clone(), r.json()["mqtt"]["password_set"].clone()), (json!(2), json!(true)));

    // pause and resume
    let r = admin.patch(t, json!({ "version": 2, "paused": true, "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(
        (r.json()["paused"].clone(), r.json()["status"]["mqtt"]["state"].clone()),
        (json!(true), json!("paused"))
    );
    assert!(!w.svc.running());

    // cars: none yet; an unknown one is refused
    let r = admin.get("/api/v1/integrations/teslamate/cars").await;
    assert_eq!((r.status, r.json()["items"].clone()), (200, json!([])));
    let r = admin
        .put(
            "/api/v1/integrations/teslamate/cars",
            json!({ "version": 3, "items": [{ "teslamate_car": "1", "match": format!("c{}", w.car) }], "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")));

    // removed: the file is gone, nothing runs
    let r = admin.delete(t, json!({ "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.json()["deleted"].clone()), (200, json!(true)), "{}", r.text());
    assert!(!w.svc.store().exists() && !w.svc.running());
    assert_eq!(admin.get(t).await.json()["enabled"], false);
    let actions = audit_actions(&mut admin).await;
    for a in ["teslamate-local-added", "teslamate-local-changed", "teslamate-local-paused", "teslamate-local-removed"] {
        assert!(actions.iter().any(|x| x == a), "{a} in {actions:?}");
    }
}
